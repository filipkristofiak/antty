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
    /// redundant with `Participant.file` for the session's Main participant; kept for parity
    /// with the documented model shape and for future direct session->file lookups.
    #[allow(dead_code)]
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
    Written { bytes: usize },
    Moved { to: PathBuf },
    Removed,
    Fs(FsChange),
}

#[derive(Debug, Clone)]
pub struct FileEvent {
    pub who: ParticipantId,
    pub rel: PathBuf,
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
        }
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
        self.participants.push(Participant {
            kind,
            label,
            session,
            parent,
            color_idx,
            file: Some(file.to_path_buf()),
        });
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
        let p = &self.participants[who.0];
        if p.kind == ParticipantKind::Main
            && let Some(session) = p.session
        {
            self.prompts.retain(|pr| pr.session != session);
        }
        self.pending_tools.retain(|(idx, _), _| *idx != who.0);
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
}
