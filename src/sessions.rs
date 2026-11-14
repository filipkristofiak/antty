use std::collections::HashMap;
use std::fs;
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::mpsc::SyncSender;
use std::sync::{LazyLock, Mutex, PoisonError};
use std::thread;
use std::time::Duration;

use crate::model::{Model, ParticipantId, ParticipantKind, Session};
use crate::watch::RawFs;

/// Which agent harness wrote a session log, and so which on-disk format to read it with.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum Harness {
    Omp,
    Claude,
    Codex,
}

impl Harness {
    pub fn index_root(self, root: &Path) -> RootIndex {
        let sessions = match self {
            Harness::Omp => omp_sessions_by_cwd(root),
            Harness::Claude => crate::claude::sessions_by_cwd(root),
            Harness::Codex => crate::codex::rollouts_by_cwd(root)
                .into_iter()
                .map(|(cwd, paths)| {
                    let files =
                        paths.into_iter().map(|path| SessionFile { project_dir: root.to_path_buf(), path }).collect();
                    (cwd, files)
                })
                .collect(),
        };
        RootIndex { harness: self, sessions }
    }

    /// Register `file`'s participant (and its ancestors) in `model`; idempotent.
    pub fn ensure_participant(self, model: &mut Model, file: &Path, project_dir: &Path) -> ParticipantId {
        match self {
            Harness::Omp => ensure_omp_participant(model, file, project_dir),
            Harness::Claude => crate::claude::ensure_participant(model, file, project_dir),
            Harness::Codex => crate::codex::ensure_participant(model, file, project_dir),
        }
    }

    /// Ingest one parsed jsonl line from `who`'s session file.
    /// `project_dir` is Codex-only (title index lookup); `idle_gap` is omp-only.
    pub fn ingest(
        self,
        model: &mut Model,
        who: ParticipantId,
        v: &serde_json::Value,
        project_dir: &Path,
        idle_gap: i64,
    ) {
        match self {
            Harness::Omp => crate::parse::ingest(model, who, v, idle_gap),
            Harness::Claude => crate::claude::ingest(model, who, v),
            Harness::Codex => crate::codex::ingest(model, who, v, project_dir),
        }
    }
}

/// A session log to tail and the harness project dir its participant resolves against
/// (Codex: the sessions root, used for the title index).
#[derive(Clone)]
pub struct SessionFile {
    pub project_dir: PathBuf,
    pub path: PathBuf,
}

/// One shared pass over a harness session root: canonical launch cwd → that checkout's
/// top-level session logs (Codex: every rollout, parents first).
pub struct RootIndex {
    harness: Harness,
    sessions: HashMap<PathBuf, Vec<SessionFile>>,
}

impl RootIndex {
    /// Every canonical launch cwd with at least one indexed session, in no particular order.
    pub fn cwds(&self) -> impl Iterator<Item = &Path> + '_ {
        self.sessions.keys().map(PathBuf::as_path)
    }

    /// Every log to tail for `project_root`: each indexed session followed by its subagent logs.
    pub fn files(&self, project_root: &Path) -> Vec<SessionFile> {
        let Some(sessions) = self.sessions.get(project_root) else { return Vec::new() };
        if self.harness == Harness::Codex {
            return sessions.clone();
        }
        let mut files = Vec::new();
        for session in sessions {
            files.push(session.clone());
            let subagents = match self.harness {
                Harness::Omp => scan_jsonl_files(&session.path.with_extension("")),
                Harness::Claude => crate::claude::subagent_files(&session.path),
                Harness::Codex => unreachable!(),
            };
            files.extend(
                subagents.into_iter().map(|path| SessionFile { project_dir: session.project_dir.clone(), path }),
            );
        }
        files
    }
}

/// Messages sent from background threads (tailer, fs watcher) to the main event loop.
pub enum Msg {
    Records { harness: Harness, project_dir: PathBuf, participant_file: PathBuf, records: Vec<serde_json::Value> },
    Reset(PathBuf),
    Fs(RawFs),
    Tick,
    WatchError(String),
    Disk { snapshot: Box<crate::disk::DiskSnapshot>, walked: Vec<usize> },
    Seed { at: crate::model::Ts, files: Vec<(PathBuf, String)> },
    Progress { done: usize, total: usize },
    Loaded { found: bool, tails: Result<Vec<crate::index::CachedTail>, String> },
    Cached(Box<Model>),
    Notice(String),
}

pub struct FileRole {
    pub kind: ParticipantKind,
    pub label: String,
    pub parent_file: Option<PathBuf>,
}

/// `D/<X1>/.../<Xk>/<name>.jsonl` has parent file `D/<X1>/.../<Xk>.jsonl`.
/// A file directly inside the project dir (k == 0) has no parent (it is a Main session).
fn parent_file_of(path: &Path, project_dir: &Path) -> Option<PathBuf> {
    let containing_dir = path.parent()?;
    if containing_dir == project_dir {
        return None;
    }
    let mut s = containing_dir.as_os_str().to_os_string();
    s.push(".jsonl");
    Some(PathBuf::from(s))
}

fn classify_file(path: &Path, project_dir: &Path) -> FileRole {
    let file_name = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
    let parent_file = parent_file_of(path, project_dir);
    if file_name == "__advisor.jsonl" {
        FileRole { kind: ParticipantKind::Advisor, label: "advisor".to_string(), parent_file }
    } else if parent_file.is_none() {
        FileRole { kind: ParticipantKind::Main, label: "main".to_string(), parent_file: None }
    } else {
        let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or(file_name).to_string();
        FileRole { kind: ParticipantKind::Subagent, label: stem, parent_file }
    }
}

/// Recursively ensure `file`'s participant (and every ancestor's) is registered in `model`.
/// Idempotent: returns the existing id if already known.
fn ensure_omp_participant(model: &mut Model, file: &Path, project_dir: &Path) -> ParticipantId {
    if let Some(&id) = model.file_participant.get(file) {
        return id;
    }
    let role = classify_file(file, project_dir);
    match role.kind {
        ParticipantKind::Main => {
            let id = model.get_or_create_participant(file, ParticipantKind::Main, "main".to_string(), None, None);
            let idx = model.sessions.len();
            model.sessions.push(Session {
                file: file.to_path_buf(),
                id: String::new(),
                title: "(untitled)".to_string(),
                cwd: project_dir.to_path_buf(),
                start: chrono::Utc::now(),
                end: None,
                last_seen: chrono::DateTime::<chrono::Utc>::MIN_UTC,
                main: id,
            });
            model.participants[id.0].session = Some(idx);
            id
        }
        ParticipantKind::Advisor | ParticipantKind::Subagent => {
            let (parent_id, session) = match &role.parent_file {
                Some(pf) => {
                    let pid = ensure_omp_participant(model, pf, project_dir);
                    (Some(pid), model.participants[pid.0].session)
                }
                None => (None, None),
            };
            model.get_or_create_participant(file, role.kind, role.label.clone(), session, parent_id)
        }
        ParticipantKind::You => unreachable!("You is never file-backed"),
    }
}

/// Read the session header (physical line 2) of a jsonl file without loading the whole file.
fn read_header_cwd(path: &Path) -> Option<PathBuf> {
    let f = fs::File::open(path).ok()?;
    let mut reader = BufReader::new(f);
    let mut line = String::new();
    reader.read_line(&mut line).ok()?; // physical line 1: padded title slot, discard
    line.clear();
    let n = reader.read_line(&mut line).ok()?;
    if n == 0 {
        return None;
    }
    let v: serde_json::Value = serde_json::from_str(line.trim_end()).ok()?;
    v.get("cwd").and_then(|c| c.as_str()).map(PathBuf::from)
}

pub(crate) fn is_jsonl(path: &Path) -> bool {
    path.extension().map(|e| e == "jsonl").unwrap_or(false)
}

pub(crate) fn is_dotfile(path: &Path) -> bool {
    path.file_name().and_then(|n| n.to_str()).map(|n| n.starts_with('.')).unwrap_or(true)
}

static OMP_HEADER_CWDS: LazyLock<Mutex<HashMap<PathBuf, PathBuf>>> = LazyLock::new(|| Mutex::new(HashMap::new()));

/// Map canonical launch cwd to every top-level session whose header names it.
fn omp_sessions_by_cwd(sessions_root: &Path) -> HashMap<PathBuf, Vec<SessionFile>> {
    let mut sessions: HashMap<PathBuf, Vec<SessionFile>> = HashMap::new();
    let Ok(rd) = fs::read_dir(sessions_root) else { return sessions };
    for entry in rd.flatten() {
        let dir = entry.path();
        if !dir.is_dir() {
            continue;
        }
        let inner = match fs::read_dir(&dir) {
            Ok(r) => r,
            Err(_) => continue,
        };
        let mut top: Vec<PathBuf> =
            inner.flatten().map(|e| e.path()).filter(|p| p.is_file() && is_jsonl(p) && !is_dotfile(p)).collect();
        top.sort();
        let mut cache = OMP_HEADER_CWDS.lock().unwrap_or_else(PoisonError::into_inner);
        for path in top {
            let cwd = if let Some(cwd) = cache.get(&path) {
                cwd.clone()
            } else {
                let Some(cwd) = read_header_cwd(&path) else { continue };
                let cwd = cwd.canonicalize().unwrap_or(cwd);
                cache.insert(path.clone(), cwd.clone());
                cwd
            };
            sessions.entry(cwd).or_default().push(SessionFile { project_dir: dir.clone(), path });
        }
    }
    sessions
}

fn scan_jsonl_files_rec(dir: &Path, out: &mut Vec<PathBuf>) {
    let rd = match fs::read_dir(dir) {
        Ok(r) => r,
        Err(_) => return,
    };
    for entry in rd.flatten() {
        let path = entry.path();
        if is_dotfile(&path) {
            continue;
        }
        if path.is_dir() {
            scan_jsonl_files_rec(&path, out);
        } else if is_jsonl(&path) {
            out.push(path);
        }
    }
}

/// Collect every non-dotfile `*.jsonl` under a harness root, recursively.
pub(crate) fn scan_jsonl_files(project_dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    scan_jsonl_files_rec(project_dir, &mut out);
    out
}

/// Split buffered partial bytes and newly read bytes into complete lines and a trailing
/// remainder, carried forward until the writer finishes that record.
fn split_lines(mut partial: Vec<u8>, new_bytes: &[u8]) -> (Vec<String>, Vec<u8>) {
    partial.extend_from_slice(new_bytes);
    let mut lines = Vec::new();
    let mut start = 0usize;
    for (i, b) in partial.iter().enumerate() {
        if *b == b'\n' {
            if let Ok(s) = std::str::from_utf8(&partial[start..i])
                && !s.is_empty()
            {
                lines.push(s.to_string());
            }
            start = i + 1;
        }
    }
    let remainder = partial[start..].to_vec();
    (lines, remainder)
}

pub struct TailState {
    pub offset: u64,
    pub partial: Vec<u8>,
}

/// One harness's session root being tailed for one checkout.
pub struct TailSource {
    pub harness: Harness,
    pub root: PathBuf,
    /// Canonical checkout whose sessions this source tails.
    pub project_root: PathBuf,
    pub states: HashMap<PathBuf, TailState>,
}

pub fn apply_records(
    model: &mut Model,
    harness: Harness,
    project_dir: &Path,
    file: &Path,
    records: &[serde_json::Value],
    idle_gap: i64,
) {
    let who = harness.ensure_participant(model, file, project_dir);
    for record in records {
        harness.ingest(model, who, record, project_dir, idle_gap);
    }
    crate::parse::flush_dirty_spans(model, idle_gap);
}

const RECORDS_PER_MSG: usize = 2048;

fn poll_source(src: &mut TailSource, files: Vec<SessionFile>, tx: &SyncSender<Msg>) -> bool {
    for file in files {
        let Ok(meta) = fs::metadata(&file.path) else { continue };
        let state = src.states.entry(file.path.clone()).or_insert(TailState { offset: 0, partial: Vec::new() });
        if meta.len() < state.offset {
            state.offset = 0;
            state.partial.clear();
            if tx.send(Msg::Reset(file.path.clone())).is_err() {
                return false;
            }
        }
        if meta.len() > state.offset
            && let Ok(mut log) = fs::File::open(&file.path)
            && log.seek(SeekFrom::Start(state.offset)).is_ok()
        {
            let mut buf = Vec::new();
            if let Ok(n) = log.read_to_end(&mut buf) {
                let (lines, partial) = split_lines(std::mem::take(&mut state.partial), &buf);
                // Count actual bytes read: a live file may grow after the stat.
                state.offset += n as u64;
                state.partial = partial;
                let mut records = lines.into_iter().filter_map(|line| serde_json::from_str(&line).ok()).peekable();
                while records.peek().is_some() {
                    let chunk = records.by_ref().take(RECORDS_PER_MSG).collect();
                    if tx
                        .send(Msg::Records {
                            harness: src.harness,
                            project_dir: file.project_dir.clone(),
                            participant_file: file.path.clone(),
                            records: chunk,
                        })
                        .is_err()
                    {
                        return false;
                    }
                }
            }
        }
    }
    true
}

/// Discover once per harness root each tick and parse sources on bounded background workers.
pub fn spawn_tailer(
    mut sources: Vec<TailSource>,
    cache: Option<crate::index::Spec>,
    tx: SyncSender<Msg>,
) -> thread::JoinHandle<()> {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    thread::spawn(move || {
        if let Some(spec) = &cache {
            match crate::index::load(spec) {
                Ok(Some((model, tails))) => {
                    if tx.send(Msg::Cached(Box::new(model))).is_err() {
                        return;
                    }
                    for tail in tails {
                        if let Some(src) = sources
                            .iter_mut()
                            .find(|s| s.harness == tail.harness && s.project_root == tail.project_root)
                        {
                            src.states = tail
                                .files
                                .into_iter()
                                .map(|f| (f.path, TailState { offset: f.offset, partial: Vec::new() }))
                                .collect();
                        }
                    }
                }
                Ok(None) => {}
                Err(e) => {
                    if tx.send(Msg::Notice(format!("index rebuilt: {e}"))).is_err() {
                        return;
                    }
                }
            }
        }
        let mut first = true;
        loop {
            let mut indexes = HashMap::new();
            for src in &sources {
                indexes.entry((src.harness, src.root.clone())).or_insert_with(|| src.harness.index_root(&src.root));
            }
            let total = sources.len();
            let done = AtomicUsize::new(0);
            let stopped = AtomicBool::new(false);
            let workers = thread::available_parallelism().map_or(1, |n| n.get()).min(total).max(1);
            {
                let queue = Mutex::new(sources.iter_mut().rev().collect::<Vec<_>>());
                thread::scope(|scope| {
                    for _ in 0..workers {
                        let (queue, indexes, done, stopped, tx) = (&queue, &indexes, &done, &stopped, &tx);
                        scope.spawn(move || {
                            while !stopped.load(Ordering::Relaxed) {
                                let Some(src) = queue.lock().unwrap_or_else(PoisonError::into_inner).pop() else {
                                    break;
                                };
                                let files = indexes
                                    .get(&(src.harness, src.root.clone()))
                                    .map_or_else(Vec::new, |i| i.files(&src.project_root));
                                if !poll_source(src, files, tx)
                                    || (first
                                        && tx
                                            .send(Msg::Progress {
                                                done: done.fetch_add(1, Ordering::Relaxed) + 1,
                                                total,
                                            })
                                            .is_err())
                                {
                                    stopped.store(true, Ordering::Relaxed);
                                    break;
                                }
                            }
                        });
                    }
                });
            }
            if stopped.load(Ordering::Relaxed) {
                return;
            }
            if first {
                if tx
                    .send(Msg::Loaded {
                        found: sources.iter().any(|s| !s.states.is_empty()),
                        tails: crate::index::tails(&sources),
                    })
                    .is_err()
                {
                    return;
                }
                first = false;
            }
            if tx.send(Msg::Tick).is_err() {
                return;
            }
            thread::sleep(Duration::from_secs(1));
        }
    })
}

#[cfg(test)]
pub fn load_blocking(model: &mut Model, harness: Harness, root: &Path, project_root: &Path, idle_gap: i64) {
    let index = harness.index_root(root);
    let files = index.files(project_root);
    let mut src = TailSource {
        harness,
        root: root.to_path_buf(),
        project_root: project_root.to_path_buf(),
        states: HashMap::new(),
    };
    let (tx, rx) = std::sync::mpsc::sync_channel(4096);
    thread::scope(|scope| {
        scope.spawn(move || {
            poll_source(&mut src, files, &tx);
        });
        for msg in rx {
            if let Msg::Records { harness, project_dir, participant_file, records } = msg {
                apply_records(model, harness, &project_dir, &participant_file, &records, idle_gap);
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn poll_source_completes_a_line_left_mid_write_by_the_previous_pass() {
        let base = std::env::temp_dir().join(format!("antty-sessions-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        let project_root = base.join("proj");
        fs::create_dir_all(&project_root).unwrap();
        // Production always canonicalizes --project (cli.rs); mirror that so discovery's own
        // canonicalize-and-compare against the header cwd matches, since e.g. macOS temp dirs
        // resolve through a symlink (/var -> /private/var).
        let project_root = project_root.canonicalize().unwrap();
        let sessions_root = base.join("sessions");
        let project_dir = sessions_root.join("-proj-");
        fs::create_dir_all(&project_dir).unwrap();
        let file = project_dir.join("2026-01-01T00-00-00-000Z_x.jsonl");

        let title_line = "{\"type\":\"title\",\"title\":\"t\"}\n".to_string();
        let header_line = format!(
            "{{\"type\":\"session\",\"version\":3,\"id\":\"x\",\"timestamp\":\"2026-01-01T00:00:00.000Z\",\"cwd\":{:?}}}\n",
            project_root.to_string_lossy()
        );
        // A line still being written during the first poll.
        let half_written = "{\"type\":\"title_change\"".to_string();
        fs::write(&file, format!("{title_line}{header_line}{half_written}")).unwrap();

        let mut src = TailSource { harness: Harness::Omp, root: sessions_root, project_root, states: HashMap::new() };
        let (tx, rx) = std::sync::mpsc::sync_channel(16);
        let files = vec![SessionFile { project_dir, path: file.clone() }];
        assert!(poll_source(&mut src, files.clone(), &tx));
        for _ in rx.try_iter() {}

        // The writer finishes the line and appends more, exactly like a live tailer tick would see.
        let rest = ",\"title\":\"Real Title\"}\n";
        let mut f = fs::OpenOptions::new().append(true).open(&file).unwrap();
        f.write_all(rest.as_bytes()).unwrap();
        drop(f);

        assert!(poll_source(&mut src, files, &tx));
        let records: Vec<_> = rx
            .try_iter()
            .flat_map(|msg| match msg {
                Msg::Records { records, .. } => records,
                _ => Vec::new(),
            })
            .collect();
        assert_eq!(records.len(), 1, "the completed record must appear exactly once");
        assert_eq!(records[0]["title"], "Real Title");
    }

    #[test]
    fn omp_sessions_load_from_every_dir_sharing_a_canonical_cwd() {
        let base = std::env::temp_dir().join(format!("antty-sessions-test-dirs-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(base.join("proj")).unwrap();
        fs::create_dir_all(base.join("other")).unwrap();
        let project = base.join("proj").canonicalize().unwrap();
        let sessions_root = base.join("sessions");
        let dir_a = sessions_root.join("-a-");
        let dir_b = sessions_root.join("-b-");
        fs::create_dir_all(&dir_a).unwrap();
        fs::create_dir_all(dir_b.join("B")).unwrap();
        let write_session = |path: &Path, cwd: &Path| {
            let title = "{\"type\":\"title\",\"title\":\"t\"}\n";
            let header = format!(
                "{{\"type\":\"session\",\"version\":3,\"id\":\"x\",\"timestamp\":\"2026-01-01T00:00:00.000Z\",\"cwd\":{:?}}}\n",
                cwd.to_string_lossy()
            );
            fs::write(path, format!("{title}{header}")).unwrap();
        };
        let a = dir_a.join("A.jsonl");
        let b = dir_b.join("B.jsonl");
        let sub = dir_b.join("B/sub.jsonl");
        let c = dir_a.join("C.jsonl");
        let d = dir_a.join("D.jsonl");
        write_session(&a, &project);
        write_session(&b, &project.join("..").join("proj"));
        fs::write(&sub, "{}\n").unwrap();
        write_session(&c, &base.join("other"));
        fs::write(&d, "{\"type\":\"title\",\"title\":\"t\"}\n").unwrap();

        let mut model = Model::new(vec![project.clone()]);
        load_blocking(&mut model, Harness::Omp, &sessions_root, &project, 30);
        assert_eq!(model.sessions.len(), 2);
        for file in [&a, &b, &sub] {
            assert!(model.file_participant.contains_key(file));
        }
        for file in [&c, &d] {
            assert!(!model.file_participant.contains_key(file));
        }
        let participant = &model.participants[model.file_participant[&sub].0];
        assert_eq!(participant.kind, ParticipantKind::Subagent);
        assert_eq!(participant.parent, Some(model.file_participant[&b]));
        fs::remove_dir_all(base).unwrap();
    }
}
