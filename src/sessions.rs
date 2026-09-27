use std::collections::HashMap;
use std::fs;
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;
use std::thread;
use std::time::Duration;

use crate::model::{Model, ParticipantId, ParticipantKind, Session};
use crate::watch::RawFs;

/// Which agent harness wrote a session log, and so which on-disk format to read it with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Harness {
    Omp,
    Claude,
}

impl Harness {
    fn discover_project_dir(self, root: &Path, project_root: &Path) -> Option<PathBuf> {
        match self {
            Harness::Omp => discover_omp_project_dir(root, project_root),
            Harness::Claude => crate::claude::discover_project_dir(root, project_root),
        }
    }

    fn scan_files(self, project_dir: &Path) -> Vec<PathBuf> {
        match self {
            Harness::Omp => scan_jsonl_files(project_dir),
            Harness::Claude => crate::claude::scan_files(project_dir),
        }
    }

    /// Register `file`'s participant (and its ancestors) in `model`; idempotent.
    pub fn ensure_participant(self, model: &mut Model, file: &Path, project_dir: &Path) -> ParticipantId {
        match self {
            Harness::Omp => ensure_omp_participant(model, file, project_dir),
            Harness::Claude => crate::claude::ensure_participant(model, file, project_dir),
        }
    }

    /// Ingest one parsed jsonl line from `who`'s session file. `idle_gap` is omp-only.
    pub fn ingest(self, model: &mut Model, who: ParticipantId, v: &serde_json::Value, idle_gap: i64) {
        match self {
            Harness::Omp => crate::parse::ingest(model, who, v, idle_gap),
            Harness::Claude => crate::claude::ingest(model, who, v),
        }
    }
}

/// Messages sent from background threads (tailer, fs watcher) to the main event loop.
pub enum Msg {
    Lines {
        harness: Harness,
        project_dir: PathBuf,
        participant_file: PathBuf,
        lines: Vec<String>,
    },
    Reset(PathBuf),
    Fs(RawFs),
    Tick,
    WatchError(String),
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
        FileRole {
            kind: ParticipantKind::Advisor,
            label: "advisor".to_string(),
            parent_file,
        }
    } else if parent_file.is_none() {
        FileRole {
            kind: ParticipantKind::Main,
            label: "main".to_string(),
            parent_file: None,
        }
    } else {
        let stem = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or(file_name)
            .to_string();
        FileRole {
            kind: ParticipantKind::Subagent,
            label: stem,
            parent_file,
        }
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
    path.file_name()
        .and_then(|n| n.to_str())
        .map(|n| n.starts_with('.'))
        .unwrap_or(true)
}

/// Find the direct child of omp's `sessions_root` whose most-recent top-level session file has
/// `cwd == project_root`. Header matching is authoritative; we never reimplement omp's
/// cwd-encoding rule.
fn discover_omp_project_dir(sessions_root: &Path, project_root: &Path) -> Option<PathBuf> {
    let rd = fs::read_dir(sessions_root).ok()?;
    for entry in rd.flatten() {
        let dir = entry.path();
        if !dir.is_dir() {
            continue;
        }
        let inner = match fs::read_dir(&dir) {
            Ok(r) => r,
            Err(_) => continue,
        };
        let mut top: Vec<PathBuf> = inner
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.is_file() && is_jsonl(p) && !is_dotfile(p))
            .collect();
        top.sort();
        let Some(last) = top.last() else { continue };
        let Some(cwd) = read_header_cwd(last) else { continue };
        let canon = cwd.canonicalize().unwrap_or(cwd);
        if canon == project_root {
            return Some(dir);
        }
    }
    None
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

/// Collect every non-dotfile `*.jsonl` under omp's `project_dir`, recursively.
fn scan_jsonl_files(project_dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    scan_jsonl_files_rec(project_dir, &mut out);
    out
}

/// Split previously-buffered partial bytes plus newly read bytes into complete lines and a new
/// trailing partial remainder. Shared by `load_initial` and the tailer loop so a session file
/// that's mid-write at either point is handled identically: the incomplete final line is carried
/// forward rather than dropped or reprocessed.
fn split_lines(mut partial: Vec<u8>, new_bytes: &[u8]) -> (Vec<String>, Vec<u8>) {
    partial.extend_from_slice(new_bytes);
    let mut lines = Vec::new();
    let mut start = 0usize;
    for (i, b) in partial.iter().enumerate() {
        if *b == b'\n' {
            if let Ok(s) = std::str::from_utf8(&partial[start..i])
                && !s.is_empty() {
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

/// One harness's session root being tailed for the current project.
pub struct TailSource {
    pub harness: Harness,
    pub root: PathBuf,
    /// the project's dir under `root`, once discovered.
    pub project_dir: Option<PathBuf>,
    pub states: HashMap<PathBuf, TailState>,
}

/// Synchronously read every jsonl file for the project's session dir under `root` once,
/// ingesting all historical lines into `model`. Returns the discovered project dir (if any) and
/// each file's resulting tail state (offset just past the last complete newline, plus any
/// dangling partial bytes), so a subsequent tailer can resume exactly from there without losing
/// or duplicating a line that was mid-write at startup.
pub fn load_initial(
    model: &mut Model,
    harness: Harness,
    root: &Path,
    project_root: &Path,
    idle_gap: i64,
) -> TailSource {
    let project_dir = harness.discover_project_dir(root, project_root);
    let mut states = HashMap::new();
    if let Some(dir) = &project_dir {
        for f in harness.scan_files(dir) {
            let Ok(bytes) = fs::read(&f) else { continue };
            let (lines, partial) = split_lines(Vec::new(), &bytes);
            // `offset` stops right before any dangling (incomplete) trailing line; we do NOT
            // also carry `partial` forward in the returned TailState. If we did, the tailer's
            // next read would start at `offset` (which still points at those same bytes on
            // disk) and re-read them, then `split_lines` would prepend the in-memory `partial`
            // on top, duplicating the fragment and breaking its JSON. Leaving the tail state's
            // partial empty lets the next read pick the dangling bytes up fresh, exactly once.
            let consumed = (bytes.len() - partial.len()) as u64;
            let who = harness.ensure_participant(model, &f, dir);
            for line in &lines {
                if let Ok(v) = serde_json::from_str::<serde_json::Value>(line) {
                    harness.ingest(model, who, &v, idle_gap);
                }
            }
            states.insert(f, TailState { offset: consumed, partial: Vec::new() });
        }
    }
    crate::parse::flush_dirty_spans(model, idle_gap);
    TailSource { harness, root: root.to_path_buf(), project_dir, states }
}

/// Background thread: for every source, rediscovers the project's session dir and tails every
/// jsonl file in it once per second, forwarding complete lines to the main thread. Each source's
/// `states` seeds file offsets from `load_initial`; `partial` starts empty in every seeded entry,
/// so any line still incomplete at startup is read fresh on the first tick.
pub fn spawn_tailer(project_root: PathBuf, mut sources: Vec<TailSource>, tx: Sender<Msg>) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        loop {
            for src in sources.iter_mut() {
                if src.project_dir.is_none() {
                    src.project_dir = src.harness.discover_project_dir(&src.root, &project_root);
                }
                let Some(dir) = &src.project_dir else { continue };
                for f in src.harness.scan_files(dir) {
                    let size = match fs::metadata(&f) {
                        Ok(m) => m.len(),
                        Err(_) => continue,
                    };
                    let state = src.states.entry(f.clone()).or_insert(TailState { offset: 0, partial: Vec::new() });
                    if size < state.offset {
                        state.offset = 0;
                        state.partial.clear();
                        let _ = tx.send(Msg::Reset(f.clone()));
                    }
                    if size > state.offset
                        && let Ok(mut file) = fs::File::open(&f)
                            && file.seek(SeekFrom::Start(state.offset)).is_ok() {
                                let mut buf = Vec::new();
                                if let Ok(n) = file.read_to_end(&mut buf) {
                                    let old_partial = std::mem::take(&mut state.partial);
                                    let (lines, new_partial) = split_lines(old_partial, &buf);
                                    // Advance by bytes actually read, not the pre-read `size`
                                    // stat: the file may have grown further between the two.
                                    // Using `size` here would under-count on a live file and
                                    // cause the next tick to re-read (and re-ingest) the tail.
                                    state.offset += n as u64;
                                    state.partial = new_partial;
                                    if !lines.is_empty() {
                                        let _ = tx.send(Msg::Lines {
                                            harness: src.harness,
                                            project_dir: dir.clone(),
                                            participant_file: f.clone(),
                                            lines,
                                        });
                                    }
                                }
                            }
                }
            }
            let _ = tx.send(Msg::Tick);
            thread::sleep(Duration::from_millis(1000));
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn load_initial_then_tailer_reconstructs_a_line_that_was_mid_write_at_startup() {
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
        // A line still being written when `load_initial` runs: no trailing newline yet.
        let half_written = "{\"type\":\"title_change\"".to_string();
        fs::write(&file, format!("{title_line}{header_line}{half_written}")).unwrap();

        let mut model = Model::new(project_root.clone());
        let src = load_initial(&mut model, Harness::Omp, &sessions_root, &project_root, 30);
        assert_eq!(src.project_dir, Some(project_dir));
        let state = src.states.get(&file).expect("tail state seeded for the file");
        assert!(state.partial.is_empty(), "load_initial must not carry the dangling line forward in-memory");

        // The writer finishes the line and appends more, exactly like a live tailer tick would see.
        let rest = ",\"title\":\"Real Title\"}\n";
        let mut f = fs::OpenOptions::new().append(true).open(&file).unwrap();
        f.write_all(rest.as_bytes()).unwrap();
        drop(f);

        let mut fh = fs::File::open(&file).unwrap();
        fh.seek(SeekFrom::Start(state.offset)).unwrap();
        let mut buf = Vec::new();
        fh.read_to_end(&mut buf).unwrap();
        let (lines, new_partial) = split_lines(Vec::new(), &buf);

        assert_eq!(lines.len(), 1, "the completed line must appear exactly once, not duplicated");
        assert!(new_partial.is_empty());
        let v: serde_json::Value = serde_json::from_str(&lines[0]).expect("reconstructed line must be valid JSON");
        assert_eq!(v["title"], "Real Title");
    }
}
