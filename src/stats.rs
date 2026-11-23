use std::collections::{HashMap, HashSet};
use std::path::Path;

use crate::footprint;
use crate::model::{FileEvent, Model, ParticipantId, ParticipantKind, Scope, TouchKind, Ts};
use crate::tree::{self, Row, Tree};

pub enum Subject {
    /// Everything visible (no row, or the PARTICIPANTS header).
    All,
    /// A session's main agent, subagents and advisors.
    Session(usize),
    /// An individual participant, including You.
    Participant(ParticipantId),
    /// A tree node or forest header's sorted, deduplicated indices into `Model.events`.
    Events(Vec<usize>),
}

#[derive(Debug, Default, PartialEq)]
pub struct Stats {
    /// (Tool, count), sorted by descending count, then by name.
    pub tools: Vec<(String, usize)>,
    pub calls: usize,
    /// Distinct file targets, excluding web and MCP events.
    pub files_read: usize,
    pub files_written: usize,
    /// Summed participant span seconds (concurrent agents add up); absent for event subjects.
    pub agent_secs: Option<i64>,
    /// Absent for event subjects and individual agents.
    pub prompts: Option<usize>,
    /// Bytes on disk for session logs and their artifacts; absent for event subjects and `you`.
    pub disk: Option<u64>,
}

pub fn subject_for(model: &Model, tree: &Tree, row: Option<Row>) -> (String, Subject) {
    let Some(row) = row else { return ("all".into(), Subject::All) };
    let mut events = match row {
        Row::Fold => return ("all".into(), Subject::All),
        Row::Section(name) => {
            let roots = match name {
                "files" => &tree.files,
                "mounts" => &tree.mounts,
                "web" => &tree.web,
                "mcp" => &tree.mcp,
                _ => return ("all".into(), Subject::All),
            };
            roots.iter().flat_map(|&idx| tree.nodes[idx].events.iter().copied()).collect()
        }
        Row::Participant(who, _) => {
            let participant = &model.participants[who.0];
            let subject = match (participant.kind, participant.session) {
                (ParticipantKind::Main, Some(session)) => Subject::Session(session),
                _ => Subject::Participant(who),
            };
            return (tree::row_label(model, tree, row), subject);
        }
        Row::Node(idx) => tree.nodes[idx].events.clone(),
    };
    events.sort_unstable();
    events.dedup();
    (tree::row_label(model, tree, row), Subject::Events(events))
}

pub fn compute(model: &Model, subject: &Subject, window: Option<(Ts, Ts)>) -> Stats {
    let mut tally = Tally::default();
    tally.add(model, subject, window, None);
    if let Some(bytes) = disk(model, subject, window) {
        tally.add_disk(bytes);
    }
    tally.finish()
}

pub fn disk(model: &Model, subject: &Subject, window: Option<(Ts, Ts)>) -> Option<u64> {
    let counted = |idx| {
        model.session_visible(idx)
            && window.is_none_or(|(ws, we)| {
                let s = &model.sessions[idx];
                s.start < we && s.end.unwrap_or(s.last_seen) >= ws
            })
    };
    let bytes = |p: &Path| model.footprints.get(p).copied().unwrap_or(0);
    match subject {
        Subject::All => {
            let mut logs = vec![Vec::new(); model.sessions.len()];
            for p in &model.participants {
                if let (Some(idx), Some(file)) = (p.session, p.file.as_deref())
                    && counted(idx)
                {
                    logs[idx].push(file);
                }
            }
            Some(logs.iter().map(|logs| footprint::session(logs, bytes)).sum())
        }
        Subject::Session(idx) => {
            if !counted(*idx) {
                return Some(0);
            }
            let logs: Vec<_> = model
                .participants
                .iter()
                .filter(|p| p.session == Some(*idx))
                .filter_map(|p| p.file.as_deref())
                .collect();
            Some(footprint::session(&logs, bytes))
        }
        Subject::Participant(who) => {
            let p = &model.participants[who.0];
            p.file.as_deref().map(|file| if p.session.is_none_or(counted) { bytes(file) } else { 0 })
        }
        Subject::Events(_) => None,
    }
}

/// One distinct file target: the project that owns a project-relative path (when merging
/// models from several projects), the scope, and the path.
type FileKey<'a> = (Option<&'a Path>, Scope, &'a Path);

/// Stats summed over several models. Tool and call counts, agent time and prompts add up;
/// files read and written are counted once across all models.
#[derive(Default)]
pub struct Tally<'a> {
    tools: HashMap<&'a str, usize>,
    read: HashSet<FileKey<'a>>,
    written: HashSet<FileKey<'a>>,
    agent_secs: Option<i64>,
    prompts: Option<usize>,
    disk: Option<u64>,
}

impl<'a> Tally<'a> {
    /// Add `model`'s stats. `project` identifies the project owning the model's single root,
    /// so models of the same project (its worktrees) share project-relative files and models
    /// of different projects do not; `None` keeps the model's own root index.
    pub fn add(&mut self, model: &'a Model, subject: &Subject, window: Option<(Ts, Ts)>, project: Option<&'a Path>) {
        let matches_who = |who: ParticipantId| match subject {
            Subject::All | Subject::Events(_) => true,
            Subject::Session(session) => model.participants[who.0].session == Some(*session),
            Subject::Participant(participant) => who == *participant,
        };
        let contains = |at| window.is_none_or(|(ws, we)| ws <= at && at < we);
        let event_visible =
            |e: &FileEvent| window.is_none_or(|(ws, we)| e.start < we && e.end >= ws) && model.event_visible(e);

        let (tools, read, written) = (&mut self.tools, &mut self.read, &mut self.written);
        let mut count_file = |e: &'a FileEvent| {
            if !matches!(e.scope, Scope::WebSearch | Scope::WebFetch | Scope::Mcp) {
                let files = match e.kind {
                    TouchKind::Read => &mut *read,
                    TouchKind::Write => &mut *written,
                };
                let key = match e.scope {
                    Scope::Project(_) if project.is_some() => (project, Scope::Project(0), e.rel.as_path()),
                    // Session-scoped paths are absolute, so the session index is not part of identity.
                    Scope::Session(_) => (None, Scope::Session(0), e.rel.as_path()),
                    scope => (None, scope, e.rel.as_path()),
                };
                files.insert(key);
            }
        };
        if let Subject::Events(indices) = subject {
            let mut seen = HashSet::new();
            // Reuse one owned lookup key instead of allocating a String per distinct call ID.
            let mut key = (0, String::new());
            for e in indices.iter().map(|&idx| &model.events[idx]).filter(|e| event_visible(e)) {
                count_file(e);
                if let Some(id) = e.tool_call_id.as_deref()
                    && seen.insert((e.who.0, id))
                {
                    key.0 = e.who.0;
                    key.1.clear();
                    key.1.push_str(id);
                    if let Some(call) = model.tool_calls.get(&key) {
                        *tools.entry(&call.tool).or_default() += 1;
                    }
                }
            }
        } else {
            for e in model.events.iter().filter(|e| matches_who(e.who) && event_visible(e)) {
                count_file(e);
            }
            for (&(who, _), call) in &model.tool_calls {
                let who = ParticipantId(who);
                if contains(call.start) && matches_who(who) && model.visible(who) {
                    *tools.entry(&call.tool).or_default() += 1;
                }
            }
        }

        if !matches!(subject, Subject::Events(_)) {
            let secs: i64 = model
                .spans
                .iter()
                .filter(|s| {
                    matches_who(s.who)
                        && model.visible(s.who)
                        && (!matches!(subject, Subject::All) || s.who != Model::YOU)
                })
                .map(|s| {
                    let (start, end) = window.map_or((s.start, s.end), |(ws, we)| (s.start.max(ws), s.end.min(we)));
                    (end - start).num_seconds().max(0)
                })
                .sum();
            *self.agent_secs.get_or_insert(0) += secs;
        }
        if let Subject::All | Subject::Participant(Model::YOU) | Subject::Session(_) = subject {
            let prompts = model
                .prompts
                .iter()
                .filter(|p| {
                    contains(p.at)
                        && model.session_visible(p.session)
                        && !matches!(subject, Subject::Session(s) if *s != p.session)
                })
                .count();
            *self.prompts.get_or_insert(0) += prompts;
        }
    }

    pub fn add_disk(&mut self, bytes: u64) {
        *self.disk.get_or_insert(0) += bytes;
    }

    pub fn finish(self) -> Stats {
        let calls = self.tools.values().sum();
        let mut tools: Vec<_> = self.tools.into_iter().map(|(tool, count)| (tool.to_owned(), count)).collect();
        tools.sort_unstable_by(|(a, x), (b, y)| y.cmp(x).then_with(|| a.cmp(b)));
        Stats {
            tools,
            calls,
            files_read: self.read.len(),
            files_written: self.written.len(),
            agent_secs: self.agent_secs,
            prompts: self.prompts,
            disk: self.disk,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::filter::{Action, ActionFilter};
    use crate::model::{EventDetail, Prompt, Session, Span, TouchSource};
    use crate::tree::FilesView;
    use std::path::{Path, PathBuf};

    #[test]
    fn stats_respect_subject_window_and_visibility() {
        let time = |sec| Ts::from_timestamp(sec, 0).unwrap();
        let root = PathBuf::from("/project");
        let mut model = Model::new(vec![root.clone()]);
        let file = root.join("main.jsonl");
        let main = model.get_or_create_participant(&file, ParticipantKind::Main, "main".into(), Some(0), None);
        let sub = model.get_or_create_participant(
            Path::new("/project/sub.jsonl"),
            ParticipantKind::Subagent,
            "sub".into(),
            Some(0),
            Some(main),
        );
        model.footprints = HashMap::from([("/project/main.jsonl".into(), 100), ("/project/sub.jsonl".into(), 30)]);
        model.sessions.push(Session {
            file,
            id: "session".into(),
            title: "Session title".into(),
            cwd: root,
            start: time(0),
            end: None,
            last_seen: time(20),
            main,
        });
        for (who, id, tool, at) in
            [(main, "b1", "bash", 0), (main, "b2", "bash", 1), (main, "r1", "read", 2), (sub, "g1", "grep", 10)]
        {
            model.record_tool_call(who, id, tool, time(at));
        }
        let read = FileEvent {
            who: main,
            rel: "a.rs".into(),
            scope: Scope::Project(0),
            kind: TouchKind::Read,
            source: TouchSource::Tool("read".into()),
            start: time(2),
            end: time(5),
            tool_call_id: Some("r1".into()),
            detail: EventDetail::None,
        };
        let write = FileEvent {
            who: sub,
            rel: "b.rs".into(),
            kind: TouchKind::Write,
            source: TouchSource::Tool("write".into()),
            start: time(10),
            end: time(12),
            tool_call_id: None,
            ..read.clone()
        };
        model.events.extend([read.clone(), write, read]);
        model.spans.extend([
            Span { who: main, start: time(0), end: time(8) },
            Span { who: sub, start: time(6), end: time(12) },
            Span { who: Model::YOU, start: time(0), end: time(20) },
        ]);
        model.prompts.extend([Prompt { at: time(5), session: 0 }, Prompt { at: time(20), session: 0 }]);

        let tree = Tree::build(&model, FilesView::Merged);
        let (label, session) = subject_for(&model, &tree, Some(Row::Participant(main, 0)));
        assert!(label.starts_with("Session title"));
        let total = Stats {
            tools: vec![("bash".into(), 2), ("grep".into(), 1), ("read".into(), 1)],
            calls: 4,
            files_read: 1,
            files_written: 1,
            agent_secs: Some(14),
            prompts: Some(2),
            disk: Some(130),
        };
        assert_eq!(compute(&model, &session, None), total);
        assert_eq!(compute(&model, &Subject::All, None), total);
        let agent = compute(&model, &Subject::Participant(sub), None);
        assert_eq!(agent.tools, vec![("grep".into(), 1)]);
        assert_eq!(agent.prompts, None);
        assert_eq!(agent.disk, Some(30));
        assert_eq!(compute(&model, &Subject::Participant(Model::YOU), None).disk, None);

        let file = Subject::Events(vec![0, 2]);
        let selected = compute(&model, &file, None);
        assert_eq!(selected.tools, vec![("read".into(), 1)]);
        assert_eq!(selected.agent_secs, None);
        assert_eq!(selected.disk, None);
        let window = Some((time(5), time(20)));
        assert_eq!(compute(&model, &file, window), selected);
        let ranged = compute(&model, &session, window);
        assert_eq!(ranged.tools, vec![("grep".into(), 1)]);
        assert_eq!(ranged.agent_secs, Some(9));
        assert_eq!(ranged.prompts, Some(1));
        assert_eq!(ranged.disk, Some(130));
        assert_eq!(compute(&model, &session, Some((time(30), time(40)))).disk, Some(0));

        model.action_filter = Some(ActionFilter { actions: HashSet::from([Action::Write]), ..Default::default() });
        assert_eq!(compute(&model, &file, None), Stats::default());
        assert_eq!(compute(&model, &session, None), Stats { files_read: 0, ..total });
        model.session_filter.insert(1);
        assert_eq!(
            compute(&model, &session, None),
            Stats { agent_secs: Some(0), prompts: Some(0), disk: Some(0), ..Default::default() }
        );
    }

    #[test]
    fn tally_dedupes_project_files_only_within_one_project() {
        let model = |root: &str, id: &str| {
            let root = PathBuf::from(root);
            let mut model = Model::new(vec![root.clone()]);
            let who = model.get_or_create_participant(
                &root.join("s.jsonl"),
                ParticipantKind::Main,
                "main".into(),
                None,
                None,
            );
            model.record_tool_call(who, id, "read", Ts::UNIX_EPOCH);
            model.events.push(FileEvent {
                who,
                rel: "src/main.rs".into(),
                scope: Scope::Project(0),
                kind: TouchKind::Read,
                source: TouchSource::Tool("read".into()),
                start: Ts::UNIX_EPOCH,
                end: Ts::UNIX_EPOCH,
                tool_call_id: Some(id.into()),
                detail: EventDetail::None,
            });
            model
        };
        let (repo, worktree, other) = (model("/repo", "a"), model("/repo-wt", "b"), model("/other", "c"));
        let mut tally = Tally::default();
        tally.add(&repo, &Subject::All, None, Some(Path::new("/repo")));
        tally.add(&worktree, &Subject::All, None, Some(Path::new("/repo")));
        tally.add(&other, &Subject::All, None, Some(Path::new("/other")));
        let stats = tally.finish();
        assert_eq!((stats.calls, stats.files_read), (3, 2));
    }
}
