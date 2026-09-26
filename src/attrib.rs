use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use crate::model::{EventDetail, FileEvent, FsChange, Model, ParticipantId, Scope, Ts, TouchKind, TouchSource};
use crate::watch::{FsKind, RawFs};

const CLASSIFY_DELAY_SECS: i64 = 5;
const DEDUP_TOOL_BEFORE_SECS: i64 = 10;
const DEDUP_TOOL_AFTER_SECS: i64 = 1;
const WINDOW_START_SLACK_SECS: i64 = 1;
const WINDOW_END_SLACK_SECS: i64 = 8;

fn fs_kind_to_change(k: FsKind) -> FsChange {
    match k {
        FsKind::Created => FsChange::Created,
        FsKind::Modified => FsChange::Modified,
        FsKind::Removed => FsChange::Removed,
    }
}

fn kind_strength(k: FsKind) -> u8 {
    match k {
        FsKind::Removed => 2,
        FsKind::Created => 1,
        FsKind::Modified => 0,
    }
}

fn persist_path_for(state_dir: &Path, root: &Path) -> PathBuf {
    let name = root.to_string_lossy().replace('/', "_");
    state_dir.join(format!("{name}.jsonl"))
}

struct Pending {
    raw: RawFs,
    first_kind: FsKind,
}

pub struct Attributor {
    pending: Vec<Pending>,
    persist_file: Option<File>,
    pub persist_error: Option<String>,
    delay_secs: i64,
}

impl Attributor {
    pub fn new(state_dir: &Path, root: &Path) -> Self {
        let path = persist_path_for(state_dir, root);
        let mut persist_error = None;
        let persist_file = match fs::create_dir_all(state_dir) {
            Ok(()) => match OpenOptions::new().create(true).append(true).open(&path) {
                Ok(f) => Some(f),
                Err(e) => {
                    persist_error = Some(format!("watch events not persisted: {e}"));
                    None
                }
            },
            Err(e) => {
                persist_error = Some(format!("watch events not persisted: {e}"));
                None
            }
        };
        Attributor { pending: Vec::new(), persist_file, persist_error, delay_secs: CLASSIFY_DELAY_SECS }
    }

    #[cfg(test)]
    fn disabled() -> Self {
        Attributor { pending: Vec::new(), persist_file: None, persist_error: None, delay_secs: CLASSIFY_DELAY_SECS }
    }

    /// Coalesce a raw fs event by path: keep the latest timestamp and the strongest kind
    /// (Removed > Created > Modified), while remembering the first kind ever seen for the path
    /// so a create-then-remove scratch file (e.g. `sed -i`'s temp file) can be dropped outright.
    pub fn push(&mut self, raw: RawFs) {
        if let Some(existing) = self.pending.iter_mut().find(|p| p.raw.path == raw.path) {
            existing.raw.at = raw.at;
            if kind_strength(raw.kind) > kind_strength(existing.raw.kind) {
                existing.raw.kind = raw.kind;
            }
        } else {
            self.pending.push(Pending { first_kind: raw.kind, raw });
        }
    }

    /// Classify every pending item older than the delay threshold and apply the resulting
    /// `FileEvent`s to `model`, persisting each to the state-dir log. Returns how many events
    /// were actually pushed, so the caller knows whether cached tree/row state needs a rebuild.
    pub fn classify_and_apply(&mut self, model: &mut Model, now: Ts) -> usize {
        let delay = chrono::Duration::seconds(self.delay_secs);
        let mut ready = Vec::new();
        self.pending.retain(|p| {
            if now - p.raw.at >= delay {
                ready.push((p.raw.clone(), p.first_kind));
                false
            } else {
                true
            }
        });
        let mut pushed = 0;
        for (raw, first_kind) in ready {
            if first_kind == FsKind::Created && raw.kind == FsKind::Removed {
                continue; // created then removed within the debounce window: a scratch file, net no-op
            }
            if self.classify_one(model, raw, now) {
                pushed += 1;
            }
        }
        pushed
    }

    fn classify_one(&mut self, model: &mut Model, raw: RawFs, now: Ts) -> bool {
        if matches!(raw.kind, FsKind::Created | FsKind::Modified) && !raw.path.exists() {
            return false;
        }
        let Some(rel) = raw.path.strip_prefix(&model.root).ok().map(|p| p.to_path_buf()) else {
            return false;
        };

        // Diff against the last known content, from session-log-derived snapshots or an
        // earlier watcher read. Recorded regardless of whether this event is later dropped as
        // a tool-write echo below, so the content baseline stays current either way.
        let current = if raw.kind == FsKind::Removed {
            Some(String::new())
        } else {
            crate::snapshot::read_text(&raw.path)
        };
        let prev = model.snapshot_before(&rel, raw.at).map(str::to_string);
        let diff = if let (Some(p), Some(c)) = (&prev, &current) {
            Some(crate::snapshot::unified(p, c))
        } else if raw.kind == FsKind::Created && prev.is_none() {
            current.as_deref().map(|c| crate::snapshot::unified("", c))
        } else {
            None
        }
        .filter(|d| !d.is_empty());
        if let Some(c) = current {
            model.record_snapshot(&rel, raw.at, c);
        }

        // FSEvents only reports *late*, never early: a tool's own write can finish up to
        // ~10s before the watcher notices, but never notably after. The window is asymmetric
        // for the same reason.
        let already = model.events.iter().any(|e| {
            e.kind == TouchKind::Write
                && e.rel == rel
                && matches!(e.source, TouchSource::Tool(_))
                && e.end >= raw.at - chrono::Duration::seconds(DEDUP_TOOL_BEFORE_SECS)
                && e.end <= raw.at + chrono::Duration::seconds(DEDUP_TOOL_AFTER_SECS)
        });
        if already {
            return false;
        }

        let mut best: Option<(ParticipantId, String, Ts)> = None;
        for w in &model.tool_windows {
            let start_ok = raw.at >= w.start - chrono::Duration::seconds(WINDOW_START_SLACK_SECS);
            let end_ok = raw.at <= w.end.unwrap_or(now) + chrono::Duration::seconds(WINDOW_END_SLACK_SECS);
            if start_ok && end_ok && best.as_ref().map(|(_, _, s)| w.start > *s).unwrap_or(true) {
                best = Some((w.who, w.tool_call_id.clone(), w.start));
            }
        }

        let (who, source, tool_call_id) = match best {
            Some((who, tcid, _)) => (who, TouchSource::Bash, Some(tcid)),
            None => (Model::YOU, TouchSource::Watcher, None),
        };

        model.events.push(FileEvent {
            who,
            rel: rel.clone(),
            scope: Scope::Project,
            kind: TouchKind::Write,
            source,
            start: raw.at,
            end: raw.at,
            tool_call_id: tool_call_id.clone(),
            detail: EventDetail::Fs { change: fs_kind_to_change(raw.kind), diff: diff.clone() },
        });
        self.persist(model, &rel, raw.kind, who, tool_call_id, raw.at, diff.as_deref());
        true
    }

    #[allow(clippy::too_many_arguments)]
    fn persist(
        &mut self,
        model: &Model,
        rel: &Path,
        kind: FsKind,
        who: ParticipantId,
        tool_call_id: Option<String>,
        at: Ts,
        diff: Option<&str>,
    ) {
        let Some(file) = self.persist_file.as_mut() else { return };
        let change = match kind {
            FsKind::Created => "created",
            FsKind::Modified => "modified",
            FsKind::Removed => "removed",
        };
        let who_json = if who == Model::YOU {
            json!("you")
        } else {
            let file_path = model.participants[who.0].file.clone().unwrap_or_default();
            json!({"file": file_path.to_string_lossy(), "toolCallId": tool_call_id.unwrap_or_default()})
        };
        let mut line = json!({
            "t": at.to_rfc3339(),
            "path": rel.to_string_lossy(),
            "change": change,
            "who": who_json,
        });
        if let Some(d) = diff {
            line["diff"] = json!(d);
        }
        let _ = writeln!(file, "{line}");
    }
}

/// Replay a previously persisted watch-event log into `model`. Called once at startup, after
/// the initial historical session load so `who.file` participants are resolvable. Lines whose
/// participant can't be resolved, or that fail to parse, are skipped. Returns an error string
/// (for the status bar) only when the log exists but can't be read at all.
pub fn replay(model: &mut Model, state_dir: &Path, root: &Path) -> Option<String> {
    let path = persist_path_for(state_dir, root);
    let content = match fs::read_to_string(&path) {
        Ok(c) => c,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return None,
        Err(e) => return Some(format!("failed reading persisted watch events: {e}")),
    };
    for line in content.lines() {
        let Ok(v) = serde_json::from_str::<Value>(line) else { continue };
        let Some(path_str) = v.get("path").and_then(|x| x.as_str()) else { continue };
        let Some(change) = v.get("change").and_then(|x| x.as_str()) else { continue };
        let Some(t) = v
            .get("t")
            .and_then(|x| x.as_str())
            .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
            .map(|d| d.with_timezone(&chrono::Utc))
        else {
            continue;
        };
        let (who, tool_call_id) = match v.get("who").cloned().unwrap_or(Value::Null) {
            Value::String(s) if s == "you" => (Model::YOU, None),
            Value::Object(o) => {
                let Some(file_str) = o.get("file").and_then(|x| x.as_str()) else {
                    continue;
                };
                let Some(&pid) = model.file_participant.get(Path::new(file_str)) else {
                    continue;
                };
                let tcid = o
                    .get("toolCallId")
                    .and_then(|x| x.as_str())
                    .filter(|s| !s.is_empty())
                    .map(|s| s.to_string());
                (pid, tcid)
            }
            _ => continue,
        };
        let fs_change = match change {
            "created" => FsChange::Created,
            "modified" => FsChange::Modified,
            "removed" => FsChange::Removed,
            _ => continue,
        };
        let diff = v.get("diff").and_then(|x| x.as_str()).map(str::to_string);
        model.events.push(FileEvent {
            who,
            rel: PathBuf::from(path_str),
            scope: Scope::Project,
            kind: TouchKind::Write,
            source: if tool_call_id.is_some() { TouchSource::Bash } else { TouchSource::Watcher },
            start: t,
            end: t,
            tool_call_id,
            detail: EventDetail::Fs { change: fs_change, diff },
        });
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{ParticipantKind, ToolWindow};
    use chrono::Duration;

    fn temp_root(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("antty-attrib-test-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn change_inside_bash_window_attributes_to_participant() {
        let root = temp_root("bashwin");
        let file = root.join("a.txt");
        fs::write(&file, "x").unwrap();
        let mut model = Model::new(root.clone());
        let sess_file = root.join("session.jsonl");
        let who = model.get_or_create_participant(&sess_file, ParticipantKind::Main, "main".into(), None, None);
        let t0 = chrono::Utc::now() - Duration::seconds(20);
        model.tool_windows.push(ToolWindow {
            who,
            tool_call_id: "tc1".into(),
            tool: "bash".into(),
            start: t0,
            end: Some(t0 + Duration::seconds(5)),
        });
        let mut attributor = Attributor::disabled();
        attributor.push(RawFs { path: file, kind: FsKind::Modified, at: t0 + Duration::seconds(2) });
        let now = t0 + Duration::seconds(30);
        attributor.classify_and_apply(&mut model, now);
        assert_eq!(model.events.len(), 1);
        assert_eq!(model.events[0].who, who);
        assert_eq!(model.events[0].source, TouchSource::Bash);
    }

    #[test]
    fn change_shortly_after_tool_write_is_dropped() {
        let root = temp_root("dedup");
        let file = root.join("a.txt");
        fs::write(&file, "x").unwrap();
        let rel = PathBuf::from("a.txt");
        let mut model = Model::new(root.clone());
        let sess_file = root.join("session.jsonl");
        let who = model.get_or_create_participant(&sess_file, ParticipantKind::Main, "main".into(), None, None);
        let edit_end = chrono::Utc::now() - Duration::seconds(20);
        model.events.push(FileEvent {
            who,
            rel: rel.clone(),
            scope: Scope::Project,
            kind: TouchKind::Write,
            source: TouchSource::Tool("edit".into()),
            start: edit_end - Duration::seconds(1),
            end: edit_end,
            tool_call_id: Some("tc-edit".into()),
            detail: EventDetail::None,
        });
        let mut attributor = Attributor::disabled();
        attributor.push(RawFs { path: file, kind: FsKind::Modified, at: edit_end + Duration::seconds(1) });
        let now = edit_end + Duration::seconds(30);
        attributor.classify_and_apply(&mut model, now);
        // still just the one original tool-authored event; the fs echo was dropped
        assert_eq!(model.events.len(), 1);
    }

    #[test]
    fn change_reported_late_by_fsevents_still_dedups() {
        // FSEvents can deliver several seconds after the tool's own write finished; the dedup
        // window must tolerate that lag (asymmetrically: only late, never early).
        let root = temp_root("dedup-late");
        let file = root.join("a.txt");
        fs::write(&file, "x").unwrap();
        let rel = PathBuf::from("a.txt");
        let mut model = Model::new(root.clone());
        let sess_file = root.join("session.jsonl");
        let who = model.get_or_create_participant(&sess_file, ParticipantKind::Main, "main".into(), None, None);
        let edit_end = chrono::Utc::now() - Duration::seconds(20);
        model.events.push(FileEvent {
            who,
            rel: rel.clone(),
            scope: Scope::Project,
            kind: TouchKind::Write,
            source: TouchSource::Tool("edit".into()),
            start: edit_end - Duration::seconds(1),
            end: edit_end,
            tool_call_id: Some("tc-edit".into()),
            detail: EventDetail::None,
        });
        let mut attributor = Attributor::disabled();
        attributor.push(RawFs { path: file, kind: FsKind::Modified, at: edit_end + Duration::seconds(6) });
        let now = edit_end + Duration::seconds(30);
        attributor.classify_and_apply(&mut model, now);
        assert_eq!(model.events.len(), 1, "the 6s-late fs echo should still be deduped against the tool write");
    }

    #[test]
    fn persisted_timestamp_is_the_watcher_observation_time_not_classification_time() {
        let state_dir = temp_root("persist-ts-state");
        let root = temp_root("persist-ts-root");
        let file = root.join("a.txt");
        fs::write(&file, "x").unwrap();
        let mut model = Model::new(root.clone());
        let mut attributor = Attributor::new(&state_dir, &root);
        let at = chrono::Utc::now() - Duration::seconds(20);
        attributor.push(RawFs { path: file, kind: FsKind::Modified, at });
        let now = at + Duration::seconds(30); // classification happens well after `at`
        attributor.classify_and_apply(&mut model, now);
        drop(attributor);
        let persisted = fs::read_to_string(persist_path_for(&state_dir, &root)).unwrap();
        let line: Value = serde_json::from_str(persisted.lines().next().unwrap()).unwrap();
        let logged_t = line.get("t").and_then(|v| v.as_str()).unwrap();
        let logged_ts = chrono::DateTime::parse_from_rfc3339(logged_t).unwrap();
        assert_eq!(logged_ts.timestamp(), at.timestamp(), "persisted t must be raw.at, not the classification wall clock");
    }

    #[test]
    fn change_with_no_window_attributes_to_you() {
        let root = temp_root("nowindow");
        let file = root.join("a.txt");
        fs::write(&file, "x").unwrap();
        let mut model = Model::new(root.clone());
        let mut attributor = Attributor::disabled();
        let at = chrono::Utc::now() - Duration::seconds(20);
        attributor.push(RawFs { path: file, kind: FsKind::Modified, at });
        let now = at + Duration::seconds(30);
        attributor.classify_and_apply(&mut model, now);
        assert_eq!(model.events.len(), 1);
        assert_eq!(model.events[0].who, Model::YOU);
        assert_eq!(model.events[0].source, TouchSource::Watcher);
    }

    #[test]
    fn created_path_that_no_longer_exists_is_dropped() {
        let root = temp_root("gone");
        let file = root.join("ghost.txt"); // never created on disk
        let mut model = Model::new(root.clone());
        let mut attributor = Attributor::disabled();
        let at = chrono::Utc::now() - Duration::seconds(20);
        attributor.push(RawFs { path: file, kind: FsKind::Created, at });
        let now = at + Duration::seconds(30);
        attributor.classify_and_apply(&mut model, now);
        assert_eq!(model.events.len(), 0);
    }

    #[test]
    fn scratch_file_created_then_removed_is_dropped_entirely() {
        let root = temp_root("scratch");
        let file = root.join(".tmpABC123"); // e.g. `sed -i`'s temp file, gone by classify time
        let mut model = Model::new(root.clone());
        let mut attributor = Attributor::disabled();
        let at = chrono::Utc::now() - Duration::seconds(20);
        attributor.push(RawFs { path: file.clone(), kind: FsKind::Created, at });
        attributor.push(RawFs { path: file, kind: FsKind::Removed, at: at + Duration::seconds(1) });
        let now = at + Duration::seconds(30);
        let pushed = attributor.classify_and_apply(&mut model, now);
        assert_eq!(pushed, 0);
        assert_eq!(model.events.len(), 0);
    }

    #[test]
    fn classify_and_apply_returns_count_of_pushed_events() {
        let root = temp_root("count");
        let file = root.join("a.txt");
        fs::write(&file, "x").unwrap();
        let mut model = Model::new(root.clone());
        let mut attributor = Attributor::disabled();
        let at = chrono::Utc::now() - Duration::seconds(20);
        attributor.push(RawFs { path: file, kind: FsKind::Modified, at });
        let now = at + Duration::seconds(30);
        assert_eq!(attributor.classify_and_apply(&mut model, now), 1);
        // nothing pending now, so a second call pushes nothing further.
        assert_eq!(attributor.classify_and_apply(&mut model, now), 0);
    }

    #[test]
    fn diff_computed_against_known_snapshot() {
        let root = temp_root("diff-known");
        let file = root.join("a.txt");
        fs::write(&file, "b\n").unwrap();
        let mut model = Model::new(root.clone());
        let rel = PathBuf::from("a.txt");
        let t0 = chrono::Utc::now() - Duration::seconds(30);
        model.record_snapshot(&rel, t0, "a\n".to_string());
        let mut attributor = Attributor::disabled();
        attributor.push(RawFs { path: file, kind: FsKind::Modified, at: t0 + Duration::seconds(2) });
        let now = t0 + Duration::seconds(60);
        attributor.classify_and_apply(&mut model, now);
        assert_eq!(model.events.len(), 1);
        match &model.events[0].detail {
            EventDetail::Fs { diff: Some(d), .. } => {
                assert!(d.contains("-a"));
                assert!(d.contains("+b"));
            }
            other => panic!("expected Fs {{ diff: Some(_), .. }}, got {other:?}"),
        }
    }

    #[test]
    fn persisted_diff_field_survives_replay() {
        let state_dir = temp_root("diff-persist-state");
        let root = temp_root("diff-persist-root");
        let file = root.join("a.txt");
        fs::write(&file, "b\n").unwrap();
        let mut model = Model::new(root.clone());
        let rel = PathBuf::from("a.txt");
        let t0 = chrono::Utc::now() - Duration::seconds(30);
        model.record_snapshot(&rel, t0, "a\n".to_string());
        let mut attributor = Attributor::new(&state_dir, &root);
        attributor.push(RawFs { path: file, kind: FsKind::Modified, at: t0 + Duration::seconds(2) });
        let now = t0 + Duration::seconds(60);
        attributor.classify_and_apply(&mut model, now);
        drop(attributor);

        let persisted = fs::read_to_string(persist_path_for(&state_dir, &root)).unwrap();
        let line: Value = serde_json::from_str(persisted.lines().next().unwrap()).unwrap();
        assert!(line.get("diff").and_then(|v| v.as_str()).is_some(), "persisted line must include a diff field");

        let mut fresh_model = Model::new(root.clone());
        replay(&mut fresh_model, &state_dir, &root);
        assert_eq!(fresh_model.events.len(), 1);
        assert!(matches!(&fresh_model.events[0].detail, EventDetail::Fs { diff: Some(_), .. }));
    }
}
