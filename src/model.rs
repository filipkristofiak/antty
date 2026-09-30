use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

pub type Ts = chrono::DateTime<chrono::Utc>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ParticipantId(pub usize);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParticipantKind {
    You,
    Main,
    Subagent,
    Advisor,
}

#[derive(Debug, Clone)]
pub struct Participant {
    pub kind: ParticipantKind,
    pub label: String,
    /// index into Model.sessions
    pub session: Option<usize>,
    /// hierarchy link (subagent/advisor -> spawning participant); not yet surfaced in the UI.
    #[allow(dead_code)]
    pub parent: Option<ParticipantId>,
    pub color_idx: usize,
    /// absolute path to the jsonl file this participant is tailed from; None for You.
    pub file: Option<PathBuf>,
}

#[derive(Debug, Clone)]
pub struct Session {
    pub file: PathBuf,
    pub id: String,
    pub title: String,
    pub cwd: PathBuf,
    pub start: Ts,
    /// set by a `session_exit` entry, and cleared again if any later entry (from any
    /// participant in the session) shows the session resumed. `None` combined with a stale
    /// `last_seen` means "ended without a clean exit", not "still live".
    pub end: Option<Ts>,
    /// timestamp of the most recent entry seen from any participant in this session; used to
    /// tell a genuinely live session from one that ended without a `session_exit` record.
    pub last_seen: Ts,
    pub main: ParticipantId,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TouchKind {
    Read,
    Write,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TouchSource {
    Tool(String),
    Bash,
    Watcher,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FsChange {
    Created,
    Modified,
    Removed,
}

#[derive(Debug, Clone)]
pub enum EventDetail {
    None,
    Diff(String),
    Written { content: String },
    Moved { to: PathBuf },
    Removed,
    Fs { change: FsChange, diff: Option<String> },
    Search { sources: Vec<(String, String)> },
}

/// Which tree section a touched target belongs to, and how `FileEvent.rel` is to be read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Scope {
    /// `rel` is project-root-relative (FILES).
    Project,
    /// `rel` is an absolute local path outside the project and not a session's own file (MOUNTS).
    External,
    /// `rel` is `ssh://<host>/<path>` verbatim (MOUNTS).
    Remote,
    /// `rel` is absolute: inside session `.0`'s own dir, or a temp-dir file that session touched
    /// (nested under the session row).
    Session(usize),
    /// `rel` is the search query (WEB).
    WebSearch,
    /// `rel` is the fetched URL (WEB).
    WebFetch,
}

#[derive(Debug, Clone)]
pub struct FileEvent {
    pub who: ParticipantId,
    pub rel: PathBuf,
    pub scope: Scope,
    pub kind: TouchKind,
    pub source: TouchSource,
    pub start: Ts,
    pub end: Ts,
    /// correlates this event back to the originating tool call; not yet surfaced in the UI.
    #[allow(dead_code)]
    pub tool_call_id: Option<String>,
    pub detail: EventDetail,
}

#[derive(Debug, Clone)]
pub struct Span {
    pub who: ParticipantId,
    pub start: Ts,
    pub end: Ts,
}

#[derive(Debug, Clone)]
pub struct Prompt {
    pub at: Ts,
    pub session: usize,
}

#[derive(Debug, Clone)]
pub struct ToolWindow {
    pub who: ParticipantId,
    pub tool_call_id: String,
    /// "bash" or "eval"; not yet surfaced in the UI (both attribute the same way).
    #[allow(dead_code)]
    pub tool: String,
    pub start: Ts,
    pub end: Option<Ts>,
}

/// A tool call seen via `tool_execution_start` or an assistant `toolCall` block,
/// awaiting its matching `toolResult`.
#[derive(Debug, Clone)]
pub struct Pending {
    pub tool: String,
    pub args: serde_json::Value,
    pub start: Ts,
}

pub struct Model {
    pub root: PathBuf,
    /// non-canonical spellings of `root` seen in session headers (e.g. `/tmp/x` when `root` is
    /// the canonicalized `/private/tmp/x`). `parse::normalize` tries each of these too, since a
    /// session's own `cwd` field is not canonicalized by omp.
    pub root_aliases: Vec<PathBuf>,
    pub participants: Vec<Participant>,
    pub sessions: Vec<Session>,
    pub events: Vec<FileEvent>,
    pub spans: Vec<Span>,
    pub prompts: Vec<Prompt>,
    pub tool_windows: Vec<ToolWindow>,
    /// empty = all sessions visible
    pub session_filter: HashSet<usize>,

    /// glue: resolve a jsonl file path to its participant.
    pub file_participant: HashMap<PathBuf, ParticipantId>,
    /// glue: pending tool calls per participant, keyed by toolCallId.
    pub pending_tools: HashMap<(usize, String), Pending>,
    /// glue: raw (unmerged) activity intervals per participant, for span recomputation.
    pub raw_intervals: HashMap<usize, Vec<(Ts, Ts)>>,
    /// glue: participant indices whose spans need recomputing before the next render.
    pub dirty_spans: HashSet<usize>,

    /// Known full contents of project files over time, from session logs (write content,
    /// edit oldText/newText) and live watcher reads. Keyed by project-relative path; each Vec
    /// sorted by timestamp.
    pub snapshots: HashMap<PathBuf, Vec<(Ts, String)>>,
    /// Session roots for formats with session-specific directories; other layouts are
    /// excluded because `session_dir_split` cannot associate their paths with a session.
    pub session_roots: Vec<PathBuf>,
    /// Latest entry in a participant's activity sequence; anchors the next interval.
    pub last_entry_ts: HashMap<usize, Ts>,
}

impl Model {
    pub const YOU: ParticipantId = ParticipantId(0);

    pub fn new(root: PathBuf) -> Self {
        let you = Participant {
            kind: ParticipantKind::You,
            label: "you".to_string(),
            session: None,
            parent: None,
            color_idx: 0,
            file: None,
        };
        Model {
            root,
            root_aliases: Vec::new(),
            participants: vec![you],
            sessions: Vec::new(),
            events: Vec::new(),
            spans: Vec::new(),
            prompts: Vec::new(),
            tool_windows: Vec::new(),
            session_filter: HashSet::new(),
            file_participant: HashMap::new(),
            pending_tools: HashMap::new(),
            raw_intervals: HashMap::new(),
            dirty_spans: HashSet::new(),
            snapshots: HashMap::new(),
            session_roots: Vec::new(),
            last_entry_ts: HashMap::new(),
        }
    }

    /// For `abs` = `<session root>/<any project dir>/<session stem>/<rest>`, the session whose
    /// jsonl file stem is `<session stem>`, and `<rest>`. Roots are tried in order; the first
    /// match wins. Matching the stem (not the whole dir) keeps this correct for sessions
    /// `/move`d between project dirs. None if `abs` is outside every root or no known session
    /// has that stem.
    pub fn session_dir_split(&self, abs: &Path) -> Option<(usize, PathBuf)> {
        self.session_roots.iter().find_map(|root| {
            if root.as_os_str().is_empty() {
                return None;
            }
            let rest = abs.strip_prefix(root).ok()?;
            let mut comps = rest.components();
            comps.next()?; // project dir component
            let stem = comps.next()?.as_os_str();
            let idx = self.sessions.iter().position(|s| s.file.file_stem() == Some(stem))?;
            Some((idx, comps.as_path().to_path_buf()))
        })
    }

    /// Record `cwd` as an alias of `root` if it isn't already `root` but resolves to it, so
    /// later `strip_prefix` calls against this session's raw (non-canonical) cwd still succeed.
    pub fn note_root_alias(&mut self, cwd: &Path) {
        if cwd == self.root || self.root_aliases.iter().any(|a| a == cwd) {
            return;
        }
        if cwd.canonicalize().map(|c| c == self.root).unwrap_or(false) {
            self.root_aliases.push(cwd.to_path_buf());
        }
    }

    pub fn visible(&self, who: ParticipantId) -> bool {
        if who == Self::YOU {
            return true;
        }
        if self.session_filter.is_empty() {
            return true;
        }
        match self.participants[who.0].session {
            Some(s) => self.session_filter.contains(&s),
            None => true,
        }
    }

    /// A session is visible under the current filter iff its Main participant is.
    pub fn session_visible(&self, session_idx: usize) -> bool {
        self.session_filter.is_empty() || self.session_filter.contains(&session_idx)
    }

    fn next_color_idx(&self) -> usize {
        let n = self.participants.len() - 1; // non-you participants registered so far
        1 + (n % 7)
    }

    /// Register a participant for `file` if it isn't already known. Idempotent.
    pub fn get_or_create_participant(
        &mut self,
        file: &Path,
        kind: ParticipantKind,
        label: String,
        session: Option<usize>,
        parent: Option<ParticipantId>,
    ) -> ParticipantId {
        if let Some(&id) = self.file_participant.get(file) {
            return id;
        }
        let color_idx = self.next_color_idx();
        let id = ParticipantId(self.participants.len());
        self.participants.push(Participant { kind, label, session, parent, color_idx, file: Some(file.to_path_buf()) });
        self.file_participant.insert(file.to_path_buf(), id);
        id
    }

    /// Drop every *tool-authored* event/span/prompt/tool-window owned by `who`, e.g. before a
    /// Reset re-ingest of its session file from offset 0. Watcher- and bash-attributed
    /// `FileEvent`s are deliberately kept: they come from live fs observation, not from the
    /// session file, so re-ingesting the file can never reconstruct them. Wiping them on every
    /// truncation-triggered Reset would permanently erase real history.
    pub fn clear_participant_data(&mut self, who: ParticipantId) {
        self.events.retain(|e| !(e.who == who && matches!(e.source, TouchSource::Tool(_))));
        self.spans.retain(|s| s.who != who);
        self.tool_windows.retain(|w| w.who != who);
        self.raw_intervals.remove(&who.0);
        self.last_entry_ts.remove(&who.0);
        let p = &self.participants[who.0];
        if p.kind == ParticipantKind::Main
            && let Some(session) = p.session
        {
            self.prompts.retain(|pr| pr.session != session);
        }
        self.pending_tools.retain(|(idx, _), _| *idx != who.0);
    }

    /// Record `content` as the known state of `rel` at `at`. Entries are kept sorted by
    /// timestamp; inserting content identical to the immediately preceding entry is a no-op
    /// (dedupes Reset re-ingest and tool-write echoes).
    pub fn record_snapshot(&mut self, rel: &Path, at: Ts, content: String) {
        let v = self.snapshots.entry(rel.to_path_buf()).or_default();
        let idx = v.partition_point(|(t, _)| *t <= at);
        if idx > 0 && v[idx - 1].1 == content {
            return;
        }
        v.insert(idx, (at, content));
    }

    /// The latest known snapshot strictly earlier than `at`, with its timestamp.
    pub fn snapshot_before_with_ts(&self, rel: &Path, at: Ts) -> Option<(Ts, &str)> {
        let v = self.snapshots.get(rel)?;
        let idx = v.partition_point(|(t, _)| *t < at);
        if idx == 0 {
            None
        } else {
            let (t, s) = &v[idx - 1];
            Some((*t, s.as_str()))
        }
    }

    /// The latest known snapshot strictly earlier than `at`.
    pub fn snapshot_before(&self, rel: &Path, at: Ts) -> Option<&str> {
        self.snapshot_before_with_ts(rel, at).map(|(_, s)| s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reset_clears_tool_events_but_keeps_watcher_and_bash_events() {
        let root = PathBuf::from("/tmp/x");
        let mut model = Model::new(root.clone());
        let file = root.join("session.jsonl");
        let who = model.get_or_create_participant(&file, ParticipantKind::Main, "main".into(), None, None);
        let now = chrono::Utc::now();
        let rel = PathBuf::from("a.txt");
        model.events.push(FileEvent {
            who,
            rel: rel.clone(),
            scope: Scope::Project,
            kind: TouchKind::Write,
            source: TouchSource::Tool("edit".into()),
            start: now,
            end: now,
            tool_call_id: Some("tc1".into()),
            detail: EventDetail::None,
        });
        model.events.push(FileEvent {
            who,
            rel: rel.clone(),
            scope: Scope::Project,
            kind: TouchKind::Write,
            source: TouchSource::Bash,
            start: now,
            end: now,
            tool_call_id: Some("tc2".into()),
            detail: EventDetail::None,
        });
        model.events.push(FileEvent {
            who,
            rel,
            scope: Scope::Project,
            kind: TouchKind::Write,
            source: TouchSource::Watcher,
            start: now,
            end: now,
            tool_call_id: None,
            detail: EventDetail::None,
        });

        model.clear_participant_data(who);

        assert_eq!(model.events.len(), 2, "watcher- and bash-attributed events must survive a Reset");
        assert!(model.events.iter().all(|e| !matches!(e.source, TouchSource::Tool(_))));
    }

    #[test]
    fn record_snapshot_keeps_order_and_snapshot_before_returns_latest_strictly_earlier() {
        let root = PathBuf::from("/tmp/x");
        let mut model = Model::new(root);
        let rel = PathBuf::from("a.txt");
        let t0 = chrono::Utc::now();
        let t1 = t0 + chrono::Duration::seconds(10);
        let t2 = t0 + chrono::Duration::seconds(20);
        model.record_snapshot(&rel, t1, "a\n".to_string());
        model.record_snapshot(&rel, t0, "before\n".to_string());
        model.record_snapshot(&rel, t2, "b\n".to_string());
        assert_eq!(model.snapshots.get(&rel).unwrap().iter().map(|(t, _)| *t).collect::<Vec<_>>(), vec![t0, t1, t2]);
        assert_eq!(model.snapshot_before(&rel, t1), Some("before\n"));
        assert_eq!(model.snapshot_before(&rel, t2), Some("a\n"));
        assert_eq!(model.snapshot_before(&rel, t0), None);
    }

    #[test]
    fn record_snapshot_identical_content_immediately_after_is_a_no_op() {
        let root = PathBuf::from("/tmp/x");
        let mut model = Model::new(root);
        let rel = PathBuf::from("a.txt");
        let t0 = chrono::Utc::now();
        let t1 = t0 + chrono::Duration::seconds(10);
        model.record_snapshot(&rel, t0, "same\n".to_string());
        model.record_snapshot(&rel, t1, "same\n".to_string());
        assert_eq!(model.snapshots.get(&rel).unwrap().len(), 1, "identical content right after itself is a no-op");
    }
}
