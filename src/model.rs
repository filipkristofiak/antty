use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

pub type Ts = chrono::DateTime<chrono::Utc>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ParticipantId(pub usize);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ParticipantKind {
    You,
    Main,
    Subagent,
    Advisor,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
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

#[derive(Debug, Clone, Serialize, Deserialize)]
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TouchKind {
    Read,
    Write,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum TouchSource {
    Tool(String),
    /// Watcher change attributed to a tool-call window.
    Window(String),
    /// Target parsed out of a shell command; never observed.
    Inferred,
    Watcher,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FsChange {
    Created,
    Modified,
    Removed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Scope {
    /// `rel` is relative to `Model.roots[.0]` (FILES).
    Project(usize),
    /// `rel` is an absolute local path outside the project and not a session's own file (MOUNTS).
    External,
    /// `rel` is `<scheme>://<host>[/<path>]` — ssh paths from file tools or scp/rsync,
    /// or host-only ssh/docker/kubectl targets inferred from shell commands (MOUNTS).
    Remote,
    /// `rel` is absolute: inside session `.0`'s own dir, or a temp-dir file that session touched
    /// (nested under the session row).
    Session(usize),
    /// `rel` is the search query (WEB).
    WebSearch,
    /// `rel` is the fetched URL (WEB).
    WebFetch,
    /// `rel` is `<server>/<tool>` (MCP).
    Mcp,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileEvent {
    pub who: ParticipantId,
    pub rel: PathBuf,
    pub scope: Scope,
    pub kind: TouchKind,
    pub source: TouchSource,
    pub start: Ts,
    pub end: Ts,
    /// Correlates this event back to the originating tool call.
    pub tool_call_id: Option<String>,
    pub detail: EventDetail,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Span {
    pub who: ParticipantId,
    pub start: Ts,
    pub end: Ts,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Prompt {
    pub at: Ts,
    pub session: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Call {
    /// Display label: bash, eval, an omp device name, or mcp:<server>/<tool>.
    pub tool: String,
    /// Command, code, or compact JSON arguments, shown verbatim in Detail.
    pub input: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolWindow {
    pub who: ParticipantId,
    pub tool_call_id: String,
    pub start: Ts,
    pub end: Option<Ts>,
}

/// A tool call seen via `tool_execution_start` or an assistant `toolCall` block,
/// awaiting its matching `toolResult`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Pending {
    pub tool: String,
    pub args: serde_json::Value,
    pub start: Ts,
}

#[derive(Serialize, Deserialize)]
pub struct ProjectRoot {
    pub path: PathBuf,
    /// Non-canonical spellings seen in session headers.
    pub aliases: Vec<PathBuf>,
    pub label: String,
}

/// Find the most specific checkout containing `abs`, including known aliases.
pub fn project_rel(roots: &[ProjectRoot], abs: &Path) -> Option<(usize, PathBuf)> {
    roots
        .iter()
        .enumerate()
        .flat_map(|(i, root)| {
            std::iter::once(&root.path).chain(root.aliases.iter()).filter_map(move |prefix| {
                abs.strip_prefix(prefix).ok().map(|rel| (i, prefix.components().count(), rel.to_path_buf()))
            })
        })
        .max_by_key(|(_, len, _)| *len)
        .map(|(i, _, rel)| (i, rel))
}

#[derive(Serialize, Deserialize)]
pub struct Model {
    /// Canonical checkout paths, with the selected --project at index zero.
    pub roots: Vec<ProjectRoot>,
    #[serde(skip)]
    pub disk: crate::disk::DiskSnapshot,
    pub participants: Vec<Participant>,
    pub sessions: Vec<Session>,
    pub events: Vec<FileEvent>,
    pub spans: Vec<Span>,
    pub prompts: Vec<Prompt>,
    pub tool_windows: Vec<ToolWindow>,
    pub window_keys: HashSet<(usize, String)>,
    /// Tool calls keyed by participant index + call id; never cleared on reset,
    /// so retained watcher events keep their command.
    pub calls: HashMap<(usize, String), Call>,
    /// empty = all sessions visible
    #[serde(skip)]
    pub session_filter: HashSet<usize>,

    /// glue: resolve a jsonl file path to its participant.
    pub file_participant: HashMap<PathBuf, ParticipantId>,
    /// glue: pending tool calls per participant, keyed by toolCallId.
    pub pending_tools: HashMap<(usize, String), Pending>,
    /// glue: raw (unmerged) activity intervals per participant, for span recomputation.
    pub raw_intervals: HashMap<usize, Vec<(Ts, Ts)>>,
    /// glue: participant indices whose spans need recomputing before the next render.
    pub dirty_spans: HashSet<usize>,

    /// Known contents of root-relative project files over time, one map per checkout.
    pub snapshots: Vec<HashMap<PathBuf, Vec<(Ts, String)>>>,
    /// Known contents of files outside FILES, keyed by their absolute path or URL.
    pub other_snapshots: HashMap<PathBuf, Vec<(Ts, String)>>,
    /// Session roots for formats with session-specific directories; other layouts are
    /// excluded because `session_dir_split` cannot associate their paths with a session.
    pub session_roots: Vec<PathBuf>,
    /// Latest entry in a participant's activity sequence; anchors the next interval.
    pub last_entry_ts: HashMap<usize, Ts>,
}

impl Model {
    pub const YOU: ParticipantId = ParticipantId(0);

    pub fn new(roots: Vec<PathBuf>) -> Self {
        assert!(!roots.is_empty(), "a project root is required");
        let project_roots: Vec<_> = roots
            .iter()
            .map(|path| {
                let name = path.file_name().map(|n| n.to_string_lossy().into_owned());
                let collides = name.as_ref().is_some_and(|name| {
                    roots.iter().filter(|other| other.file_name().is_some_and(|n| n.to_string_lossy() == *name)).count()
                        > 1
                });
                ProjectRoot {
                    path: path.clone(),
                    aliases: Vec::new(),
                    label: if collides { None } else { name }.unwrap_or_else(|| path.display().to_string()),
                }
            })
            .collect();
        let you = Participant {
            kind: ParticipantKind::You,
            label: "you".to_string(),
            session: None,
            parent: None,
            color_idx: 0,
            file: None,
        };
        Model {
            roots: project_roots,
            disk: crate::disk::DiskSnapshot::default(),
            participants: vec![you],
            sessions: Vec::new(),
            events: Vec::new(),
            spans: Vec::new(),
            prompts: Vec::new(),
            tool_windows: Vec::new(),
            window_keys: HashSet::new(),
            calls: HashMap::new(),
            session_filter: HashSet::new(),
            file_participant: HashMap::new(),
            pending_tools: HashMap::new(),
            raw_intervals: HashMap::new(),
            dirty_spans: HashSet::new(),
            snapshots: (0..roots.len()).map(|_| HashMap::new()).collect(),
            other_snapshots: HashMap::new(),
            session_roots: Vec::new(),
            last_entry_ts: HashMap::new(),
        }
    }

    pub fn record_call(&mut self, who: ParticipantId, id: &str, tool: String, input: String) {
        self.calls.insert((who.0, id.to_owned()), Call { tool, input });
    }

    pub fn open_window(&mut self, who: ParticipantId, id: &str, tool: String, start: Ts, input: String) {
        if self.window_keys.insert((who.0, id.to_owned())) {
            self.tool_windows.push(ToolWindow { who, tool_call_id: id.to_owned(), start, end: None });
        }
        self.record_call(who, id, tool, input);
    }

    pub fn close_window(&mut self, who: ParticipantId, id: &str, end: Ts) {
        if let Some(w) =
            self.tool_windows.iter_mut().rev().find(|w| w.who == who && w.tool_call_id == id && w.end.is_none())
        {
            w.end = Some(end);
        }
    }

    pub fn call(&self, who: ParticipantId, id: &str) -> Option<&Call> {
        self.calls.get(&(who.0, id.to_owned()))
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

    pub fn project_rel(&self, abs: &Path) -> Option<(usize, PathBuf)> {
        project_rel(&self.roots, abs)
    }

    pub fn nested_roots(&self, i: usize) -> Vec<PathBuf> {
        self.roots
            .iter()
            .enumerate()
            .filter(|(j, root)| *j != i && root.path.starts_with(&self.roots[i].path))
            .map(|(_, root)| root.path.clone())
            .collect()
    }

    pub fn project_display(&self, root: usize, rel: &Path) -> String {
        if self.roots.len() > 1 {
            format!("{}/{}", self.roots[root].label, rel.display())
        } else {
            rel.display().to_string()
        }
    }

    /// Retain raw cwd spellings so paths in non-canonical session logs still resolve.
    pub fn note_root_alias(&mut self, cwd: &Path) {
        if self.roots.iter().any(|root| root.path == cwd || root.aliases.iter().any(|a| a == cwd)) {
            return;
        }
        if let Ok(canonical) = cwd.canonicalize()
            && let Some(root) = self.roots.iter_mut().find(|root| root.path == canonical)
        {
            root.aliases.push(cwd.to_path_buf());
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
    /// Reset re-ingest of its session file from offset 0. Watcher- and window-attributed
    /// `FileEvent`s are deliberately kept: they come from live fs observation, not from the
    /// session file, so re-ingesting the file can never reconstruct them. Wiping them on every
    /// truncation-triggered Reset would permanently erase real history.
    pub fn clear_participant_data(&mut self, who: ParticipantId) {
        self.events.retain(|e| !(e.who == who && matches!(e.source, TouchSource::Tool(_) | TouchSource::Inferred)));
        self.spans.retain(|s| s.who != who);
        self.tool_windows.retain(|w| w.who != who);
        self.window_keys.retain(|(w, _)| *w != who.0);
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

    fn snapshot_map(&self, scope: Scope) -> &HashMap<PathBuf, Vec<(Ts, String)>> {
        match scope {
            Scope::Project(i) => &self.snapshots[i],
            _ => &self.other_snapshots,
        }
    }

    fn snapshot_map_mut(&mut self, scope: Scope) -> &mut HashMap<PathBuf, Vec<(Ts, String)>> {
        match scope {
            Scope::Project(i) => &mut self.snapshots[i],
            _ => &mut self.other_snapshots,
        }
    }

    /// Record known content, deduplicating consecutive identical states.
    pub fn record_snapshot(&mut self, scope: Scope, rel: &Path, at: Ts, content: String) {
        let v = self.snapshot_map_mut(scope).entry(rel.to_path_buf()).or_default();
        let idx = v.partition_point(|(t, _)| *t <= at);
        if idx > 0 && v[idx - 1].1 == content {
            return;
        }
        v.insert(idx, (at, content));
    }

    /// Latest known snapshot strictly earlier than `at`, with its timestamp.
    pub fn snapshot_before_with_ts(&self, scope: Scope, rel: &Path, at: Ts) -> Option<(Ts, &str)> {
        let v = self.snapshot_map(scope).get(rel)?;
        let idx = v.partition_point(|(t, _)| *t < at);
        if idx == 0 {
            None
        } else {
            let (t, s) = &v[idx - 1];
            Some((*t, s.as_str()))
        }
    }

    /// Latest known snapshot strictly earlier than `at`.
    pub fn snapshot_before(&self, scope: Scope, rel: &Path, at: Ts) -> Option<&str> {
        self.snapshot_before_with_ts(scope, rel, at).map(|(_, s)| s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reset_clears_transcript_events_but_keeps_watcher_events_and_calls() {
        let root = PathBuf::from("/tmp/x");
        let mut model = Model::new(vec![root.clone()]);
        let file = root.join("session.jsonl");
        let who = model.get_or_create_participant(&file, ParticipantKind::Main, "main".into(), None, None);
        let now = chrono::Utc::now();
        let rel = PathBuf::from("a.txt");
        model.events.push(FileEvent {
            who,
            rel: rel.clone(),
            scope: Scope::Project(0),
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
            scope: Scope::Project(0),
            kind: TouchKind::Write,
            source: TouchSource::Window("bash".into()),
            start: now,
            end: now,
            tool_call_id: Some("tc2".into()),
            detail: EventDetail::None,
        });
        model.events.push(FileEvent {
            who,
            rel,
            scope: Scope::Project(0),
            kind: TouchKind::Write,
            source: TouchSource::Watcher,
            start: now,
            end: now,
            tool_call_id: None,
            detail: EventDetail::None,
        });

        model.events.push(FileEvent {
            who,
            rel: PathBuf::from("ssh://h"),
            scope: Scope::Remote,
            kind: TouchKind::Read,
            source: TouchSource::Inferred,
            start: now,
            end: now,
            tool_call_id: Some("tc2".into()),
            detail: EventDetail::None,
        });
        model.record_call(who, "tc2", "bash".into(), "ssh h".into());

        model.clear_participant_data(who);

        assert_eq!(model.events.len(), 2, "observed watcher events must survive a Reset");
        assert!(model.events.iter().all(|e| !matches!(e.source, TouchSource::Tool(_) | TouchSource::Inferred)));
        assert_eq!(model.call(who, "tc2").unwrap().input, "ssh h");
    }

    #[test]
    fn record_snapshot_keeps_order_and_snapshot_before_returns_latest_strictly_earlier() {
        let root = PathBuf::from("/tmp/x");
        let mut model = Model::new(vec![root]);
        let rel = PathBuf::from("a.txt");
        let t0 = chrono::Utc::now();
        let t1 = t0 + chrono::Duration::seconds(10);
        let t2 = t0 + chrono::Duration::seconds(20);
        model.record_snapshot(Scope::Project(0), &rel, t1, "a\n".to_string());
        model.record_snapshot(Scope::Project(0), &rel, t0, "before\n".to_string());
        model.record_snapshot(Scope::Project(0), &rel, t2, "b\n".to_string());
        assert_eq!(model.snapshots[0].get(&rel).unwrap().iter().map(|(t, _)| *t).collect::<Vec<_>>(), vec![t0, t1, t2]);
        assert_eq!(model.snapshot_before(Scope::Project(0), &rel, t1), Some("before\n"));
        assert_eq!(model.snapshot_before(Scope::Project(0), &rel, t2), Some("a\n"));
        assert_eq!(model.snapshot_before(Scope::Project(0), &rel, t0), None);
    }

    #[test]
    fn record_snapshot_identical_content_immediately_after_is_a_no_op() {
        let root = PathBuf::from("/tmp/x");
        let mut model = Model::new(vec![root]);
        let rel = PathBuf::from("a.txt");
        let t0 = chrono::Utc::now();
        let t1 = t0 + chrono::Duration::seconds(10);
        model.record_snapshot(Scope::Project(0), &rel, t0, "same\n".to_string());
        model.record_snapshot(Scope::Project(0), &rel, t1, "same\n".to_string());
        assert_eq!(model.snapshots[0].get(&rel).unwrap().len(), 1, "identical content right after itself is a no-op");
    }

    #[test]
    fn nested_root_takes_precedence_and_snapshots_stay_isolated() {
        let mut model = Model::new(vec![PathBuf::from("/repo"), PathBuf::from("/repo/.claude/worktrees/feat")]);
        assert_eq!(
            model.project_rel(Path::new("/repo/.claude/worktrees/feat/src/a.rs")),
            Some((1, PathBuf::from("src/a.rs")))
        );
        let rel = Path::new("src/a.rs");
        let now = chrono::Utc::now();
        model.record_snapshot(Scope::Project(0), rel, now, "main".into());
        model.record_snapshot(Scope::Project(1), rel, now, "feature".into());
        let later = now + chrono::Duration::seconds(1);
        assert_eq!(model.snapshot_before(Scope::Project(0), rel, later), Some("main"));
        assert_eq!(model.snapshot_before(Scope::Project(1), rel, later), Some("feature"));
        assert_eq!(model.nested_roots(0), vec![PathBuf::from("/repo/.claude/worktrees/feat")]);
    }
}
