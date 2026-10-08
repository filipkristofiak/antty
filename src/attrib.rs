use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use crate::model::{EventDetail, FileEvent, FsChange, Model, ParticipantId, Scope, TouchKind, TouchSource, Ts};
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
    persist_files: Vec<Option<File>>,
    pub persist_error: Option<String>,
    delay_secs: i64,
}

impl Attributor {
    pub fn new(state_dir: &Path, roots: &[PathBuf]) -> Self {
        let mut persist_error = None;
        let persist_files = match fs::create_dir_all(state_dir) {
            Ok(()) => roots
                .iter()
                .map(|root| {
                    match OpenOptions::new().create(true).append(true).open(persist_path_for(state_dir, root)) {
                        Ok(file) => Some(file),
                        Err(e) => {
                            if persist_error.is_none() {
                                persist_error = Some(format!("watch events not persisted: {e}"));
                            }
                            None
                        }
                    }
                })
                .collect(),
            Err(e) => {
                persist_error = Some(format!("watch events not persisted: {e}"));
                (0..roots.len()).map(|_| None).collect()
            }
        };
        Attributor { pending: Vec::new(), persist_files, persist_error, delay_secs: CLASSIFY_DELAY_SECS }
    }

    #[cfg(test)]
    fn disabled() -> Self {
        Attributor {
            pending: Vec::new(),
            persist_files: Vec::new(),
            persist_error: None,
            delay_secs: CLASSIFY_DELAY_SECS,
        }
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
        let Some((root, rel)) = model.project_rel(&raw.path) else {
            return false;
        };

        // Diff against the last known content, from session-log-derived snapshots or an
        // earlier watcher read. Recorded regardless of whether this event is later dropped as
        // a tool-write echo below, so the content baseline stays current either way.
        let current =
            if raw.kind == FsKind::Removed { Some(String::new()) } else { crate::snapshot::read_text(&raw.path) };
        let prev = model.snapshot_before(Scope::Project(root), &rel, raw.at).map(str::to_string);
        let diff = if let (Some(p), Some(c)) = (&prev, &current) {
            Some(crate::snapshot::unified(p, c))
        } else if raw.kind == FsKind::Created && prev.is_none() {
            current.as_deref().map(|c| crate::snapshot::unified("", c))
        } else {
            None
        }
        .filter(|d| !d.is_empty());
        if let Some(c) = current {
            model.record_snapshot(Scope::Project(root), &rel, raw.at, c);
        }

        // FSEvents only reports *late*, never early: a tool's own write can finish up to
        // ~10s before the watcher notices, but never notably after. The window is asymmetric
        // for the same reason.
        let already = model.events.iter().any(|e| {
            e.kind == TouchKind::Write
                && e.scope == Scope::Project(root)
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
            // A shell running in a different checkout cannot have caused this file change.
            if model.participants[w.who.0]
                .session
                .and_then(|i| model.sessions.get(i))
                .filter(|session| !session.cwd.as_os_str().is_empty())
                .and_then(|session| model.project_rel(&session.cwd))
                .is_some_and(|(window_root, _)| window_root != root)
            {
                continue;
            }
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
            scope: Scope::Project(root),
            kind: TouchKind::Write,
            source,
            start: raw.at,
            end: raw.at,
            tool_call_id: tool_call_id.clone(),
            detail: EventDetail::Fs { change: fs_kind_to_change(raw.kind), diff: diff.clone() },
        });
        self.persist(root, model, &rel, raw.kind, who, tool_call_id, raw.at, diff.as_deref());
        true
    }

    #[allow(clippy::too_many_arguments)]
    fn persist(
        &mut self,
        root: usize,
        model: &Model,
        rel: &Path,
        kind: FsKind,
        who: ParticipantId,
        tool_call_id: Option<String>,
        at: Ts,
        diff: Option<&str>,
    ) {
        let Some(file) = self.persist_files.get_mut(root).and_then(Option::as_mut) else { return };
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

/// Replay each checkout's persisted watch-event log into `model`, after session loading so
/// `who.file` participants are resolvable. Malformed lines and unresolved participants are
/// skipped. Report the first unreadable log without preventing the others from loading.
pub fn replay(model: &mut Model, state_dir: &Path) -> Option<String> {
    let roots: Vec<_> = model.roots.iter().map(|root| root.path.clone()).collect();
    let mut first_error = None;
    for (i, root) in roots.iter().enumerate() {
        let path = persist_path_for(state_dir, root);
        let content = match fs::read_to_string(&path) {
            Ok(c) => c,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => {
                if first_error.is_none() {
                    first_error = Some(format!("failed reading persisted watch events: {e}"));
                }
                continue;
            }
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
                    let tcid =
                        o.get("toolCallId").and_then(|x| x.as_str()).filter(|s| !s.is_empty()).map(|s| s.to_string());
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
            let (event_root, rel) =
                model.project_rel(&root.join(path_str)).unwrap_or_else(|| (i, PathBuf::from(path_str)));
            model.events.push(FileEvent {
                who,
                rel,
                scope: Scope::Project(event_root),
                kind: TouchKind::Write,
                source: if tool_call_id.is_some() { TouchSource::Bash } else { TouchSource::Watcher },
                start: t,
                end: t,
                tool_call_id,
                detail: EventDetail::Fs { change: fs_change, diff },
            });
        }
    }
    first_error
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{ParticipantKind, Session, ToolWindow};
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
        let mut model = Model::new(vec![root.clone()]);
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
        let mut model = Model::new(vec![root.clone()]);
        let sess_file = root.join("session.jsonl");
        let who = model.get_or_create_participant(&sess_file, ParticipantKind::Main, "main".into(), None, None);
        let edit_end = chrono::Utc::now() - Duration::seconds(20);
        model.events.push(FileEvent {
            who,
            rel: rel.clone(),
            scope: Scope::Project(0),
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
        let mut model = Model::new(vec![root.clone()]);
        let sess_file = root.join("session.jsonl");
        let who = model.get_or_create_participant(&sess_file, ParticipantKind::Main, "main".into(), None, None);
        let edit_end = chrono::Utc::now() - Duration::seconds(20);
        model.events.push(FileEvent {
            who,
            rel: rel.clone(),
            scope: Scope::Project(0),
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
        let mut model = Model::new(vec![root.clone()]);
        let mut attributor = Attributor::new(&state_dir, std::slice::from_ref(&root));
        let at = chrono::Utc::now() - Duration::seconds(20);
        attributor.push(RawFs { path: file, kind: FsKind::Modified, at });
        let now = at + Duration::seconds(30); // classification happens well after `at`
        attributor.classify_and_apply(&mut model, now);
        drop(attributor);
        let persisted = fs::read_to_string(persist_path_for(&state_dir, &root)).unwrap();
        let line: Value = serde_json::from_str(persisted.lines().next().unwrap()).unwrap();
        let logged_t = line.get("t").and_then(|v| v.as_str()).unwrap();
        let logged_ts = chrono::DateTime::parse_from_rfc3339(logged_t).unwrap();
        assert_eq!(
            logged_ts.timestamp(),
            at.timestamp(),
            "persisted t must be raw.at, not the classification wall clock"
        );
    }

    #[test]
    fn change_with_no_window_attributes_to_you() {
        let root = temp_root("nowindow");
        let file = root.join("a.txt");
        fs::write(&file, "x").unwrap();
        let mut model = Model::new(vec![root.clone()]);
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
        let mut model = Model::new(vec![root.clone()]);
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
        let mut model = Model::new(vec![root.clone()]);
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
        let mut model = Model::new(vec![root.clone()]);
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
        let mut model = Model::new(vec![root.clone()]);
        let rel = PathBuf::from("a.txt");
        let t0 = chrono::Utc::now() - Duration::seconds(30);
        model.record_snapshot(Scope::Project(0), &rel, t0, "a\n".to_string());
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
        let mut model = Model::new(vec![root.clone()]);
        let rel = PathBuf::from("a.txt");
        let t0 = chrono::Utc::now() - Duration::seconds(30);
        model.record_snapshot(Scope::Project(0), &rel, t0, "a\n".to_string());
        let mut attributor = Attributor::new(&state_dir, std::slice::from_ref(&root));
        attributor.push(RawFs { path: file, kind: FsKind::Modified, at: t0 + Duration::seconds(2) });
        let now = t0 + Duration::seconds(60);
        attributor.classify_and_apply(&mut model, now);
        drop(attributor);

        let persisted = fs::read_to_string(persist_path_for(&state_dir, &root)).unwrap();
        let line: Value = serde_json::from_str(persisted.lines().next().unwrap()).unwrap();
        assert!(line.get("diff").and_then(|v| v.as_str()).is_some(), "persisted line must include a diff field");

        let mut fresh_model = Model::new(vec![root.clone()]);
        replay(&mut fresh_model, &state_dir);
        assert_eq!(fresh_model.events.len(), 1);
        assert!(matches!(&fresh_model.events[0].detail, EventDetail::Fs { diff: Some(_), .. }));
    }
    #[test]
    fn same_relative_file_in_two_roots_persists_and_replays_independently() {
        let base = temp_root("two-roots");
        let state_dir = base.join("state");
        let a = base.join("a");
        let b = base.join("b");
        fs::create_dir_all(a.join("src")).unwrap();
        fs::create_dir_all(b.join("src")).unwrap();
        fs::write(a.join("src/a.rs"), "a\n").unwrap();
        fs::write(b.join("src/a.rs"), "b\n").unwrap();
        let roots = vec![a.clone(), b.clone()];
        let mut model = Model::new(roots.clone());
        let mut attributor = Attributor::new(&state_dir, &roots);
        assert!(attributor.persist_error.is_none());
        let at = chrono::Utc::now() - Duration::seconds(20);
        let rel = Path::new("src/a.rs");
        model.record_snapshot(Scope::Project(0), rel, at - Duration::seconds(1), "old-a\n".into());
        model.record_snapshot(Scope::Project(1), rel, at - Duration::seconds(1), "old-b\n".into());
        attributor.push(RawFs { path: a.join("src/a.rs"), kind: FsKind::Modified, at });
        attributor.push(RawFs { path: b.join("src/a.rs"), kind: FsKind::Modified, at });
        assert_eq!(attributor.classify_and_apply(&mut model, at + Duration::seconds(30)), 2);
        assert_eq!(model.events[0].scope, Scope::Project(0));
        assert_eq!(model.events[1].scope, Scope::Project(1));
        assert_eq!(model.events[0].rel, Path::new("src/a.rs"));
        assert_eq!(model.events[1].rel, Path::new("src/a.rs"));
        let diffs: Vec<_> = model
            .events
            .iter()
            .map(|e| match &e.detail {
                EventDetail::Fs { diff: Some(diff), .. } => diff.as_str(),
                other => panic!("expected watcher diff, got {other:?}"),
            })
            .collect();
        assert!(diffs[0].contains("-old-a") && !diffs[0].contains("-old-b"));
        assert!(diffs[1].contains("-old-b") && !diffs[1].contains("-old-a"));
        drop(attributor);

        for root in &roots {
            let lines = fs::read_to_string(persist_path_for(&state_dir, root)).unwrap();
            let entries: Vec<Value> = lines.lines().map(|line| serde_json::from_str(line).unwrap()).collect();
            assert_eq!(entries.len(), 1);
            assert_eq!(entries[0]["path"], "src/a.rs");
        }
        let mut restored = Model::new(roots);
        assert_eq!(replay(&mut restored, &state_dir), None);
        assert_eq!(restored.events.len(), 2);
        assert_eq!(restored.events[0].scope, Scope::Project(0));
        assert_eq!(restored.events[1].scope, Scope::Project(1));
        assert_eq!(restored.events[0].rel, restored.events[1].rel);
        assert!(restored.events.iter().all(|e| e.who == Model::YOU && e.source == TouchSource::Watcher));
    }

    #[test]
    fn bash_window_in_another_root_cannot_claim_change() {
        let base = temp_root("cross-root-window");
        let a = base.join("a");
        let b = base.join("b");
        fs::create_dir_all(&a).unwrap();
        fs::create_dir_all(&b).unwrap();
        fs::write(a.join("a.txt"), "a").unwrap();
        fs::write(b.join("b.txt"), "b").unwrap();
        let mut model = Model::new(vec![a.clone(), b.clone()]);
        let session_file = a.join("session.jsonl");
        let who = model.get_or_create_participant(&session_file, ParticipantKind::Main, "main".into(), None, None);
        let t0 = chrono::Utc::now() - Duration::seconds(30);
        model.sessions.push(Session {
            file: session_file,
            id: "session".into(),
            title: "session".into(),
            cwd: a.clone(),
            start: t0,
            end: None,
            last_seen: t0,
            main: who,
        });
        model.participants[who.0].session = Some(0);
        model.tool_windows.push(ToolWindow {
            who,
            tool_call_id: "bash1".into(),
            tool: "bash".into(),
            start: t0,
            end: Some(t0 + Duration::seconds(5)),
        });
        let mut attributor = Attributor::disabled();
        let at = t0 + Duration::seconds(2);
        attributor.push(RawFs { path: b.join("b.txt"), kind: FsKind::Modified, at });
        attributor.push(RawFs { path: a.join("a.txt"), kind: FsKind::Modified, at });
        assert_eq!(attributor.classify_and_apply(&mut model, at + Duration::seconds(30)), 2);
        assert_eq!(model.events[0].scope, Scope::Project(1));
        assert_eq!(model.events[0].who, Model::YOU);
        assert_eq!(model.events[0].source, TouchSource::Watcher);
        assert_eq!(model.events[1].scope, Scope::Project(0));
        assert_eq!(model.events[1].who, who);
        assert_eq!(model.events[1].source, TouchSource::Bash);
    }

    #[test]
    fn tool_write_in_other_root_does_not_dedup_watcher_change() {
        let base = temp_root("cross-root-dedup");
        let a = base.join("a");
        let b = base.join("b");
        fs::create_dir_all(&a).unwrap();
        fs::create_dir_all(&b).unwrap();
        fs::write(b.join("same.txt"), "changed").unwrap();
        let mut model = Model::new(vec![a, b.clone()]);
        let at = chrono::Utc::now() - Duration::seconds(20);
        model.events.push(FileEvent {
            who: Model::YOU,
            rel: PathBuf::from("same.txt"),
            scope: Scope::Project(0),
            kind: TouchKind::Write,
            source: TouchSource::Tool("write".into()),
            start: at,
            end: at,
            tool_call_id: None,
            detail: EventDetail::None,
        });
        let mut attributor = Attributor::disabled();
        attributor.push(RawFs { path: b.join("same.txt"), kind: FsKind::Modified, at });
        assert_eq!(attributor.classify_and_apply(&mut model, at + Duration::seconds(30)), 1);
        assert_eq!(model.events.len(), 2);
        assert_eq!(model.events[1].scope, Scope::Project(1));
        assert_eq!(model.events[1].source, TouchSource::Watcher);
    }

    #[test]
    fn replay_rehomes_old_nested_root_paths() {
        let base = temp_root("nested-history");
        let state_dir = base.join("state");
        fs::create_dir_all(&state_dir).unwrap();
        let inner = base.join("inner");
        fs::create_dir_all(&inner).unwrap();
        let roots = vec![base.clone(), inner];
        let t = chrono::Utc::now().to_rfc3339();
        fs::write(
            persist_path_for(&state_dir, &base),
            format!("{}\n", json!({"t": t, "path": "inner/src/a.rs", "change": "modified", "who": "you"})),
        )
        .unwrap();
        let mut model = Model::new(roots);
        assert_eq!(replay(&mut model, &state_dir), None);
        assert_eq!(model.events.len(), 1);
        assert_eq!(model.events[0].scope, Scope::Project(1));
        assert_eq!(model.events[0].rel, Path::new("src/a.rs"));
    }

    #[test]
    fn unreadable_root_log_does_not_block_other_roots() {
        let base = temp_root("replay-read-error");
        let state_dir = base.join("state");
        fs::create_dir_all(&state_dir).unwrap();
        let a = base.join("a");
        let b = base.join("b");
        fs::create_dir_all(&a).unwrap();
        fs::create_dir_all(&b).unwrap();
        fs::create_dir_all(persist_path_for(&state_dir, &a)).unwrap();
        fs::write(
            persist_path_for(&state_dir, &b),
            format!(
                "{}\n",
                json!({
                    "t": chrono::Utc::now().to_rfc3339(),
                    "path": "src/a.rs",
                    "change": "modified",
                    "who": "you"
                })
            ),
        )
        .unwrap();
        let mut model = Model::new(vec![a, b]);
        assert!(replay(&mut model, &state_dir).is_some());
        assert_eq!(model.events.len(), 1);
        assert_eq!(model.events[0].scope, Scope::Project(1));
    }
}
