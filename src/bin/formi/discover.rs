use std::collections::BTreeMap;
use std::fs::File;
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use antty::model::{Model, ParticipantId, Session, Ts};
use antty::sessions::{Harness, SessionFile};
use chrono::{DateTime, Utc};
use serde_json::Value;

pub const IDLE_GAP: i64 = 30;
const HEADER_LINES: usize = 50;
const TAIL_BYTES: u64 = 64 * 1024;
const MAX_TAIL_BYTES: u64 = 8 * 1024 * 1024;

/// Machine a project lives on; only the local machine for now.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Origin {
    Local,
}

/// Identity of a project or a tree directory: a path on one origin.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ProjectKey {
    pub origin: Origin,
    pub path: PathBuf,
}

/// What Enter opens, decided at discovery.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Drill {
    Local(PathBuf),
    None,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionInfo {
    pub harness: Harness,
    pub cwd: PathBuf,
    /// First log of the session's file group; the main log when listed.
    pub file: PathBuf,
    pub drill: Drill,
    pub title: String,
    /// (start, end) of the bar; None when no line read had a top-level timestamp.
    pub span: Option<(Ts, Ts)>,
    /// Merged, sorted real activity, or `None` before indexing.
    pub activity: Option<Vec<(Ts, Ts)>>,
    /// Bytes on disk for the session's logs and artifact directories; `None` before indexing.
    pub disk: Option<u64>,
}

impl SessionInfo {
    /// What the timeline draws: real activity once indexed, else the start→last-seen span.
    pub fn bars(&self) -> &[(Ts, Ts)] {
        self.activity.as_deref().unwrap_or(self.span.as_slice())
    }
}

/// The logs of one session, with `files[0].path` as its key.
pub struct Source {
    pub harness: Harness,
    pub cwd: PathBuf,
    pub files: Vec<SessionFile>,
}

pub fn collect(roots: &[(Harness, PathBuf); 3]) -> (BTreeMap<PathBuf, Vec<SessionInfo>>, Vec<Source>) {
    let mut pairs = Vec::new();
    for (harness, root) in roots {
        let index = harness.index_root(root);
        for cwd in index.cwds() {
            pairs.extend(collect_cwd(*harness, cwd, &index.files(cwd)));
        }
    }
    pairs.sort_by(|(a, _), (b, _)| {
        b.span.map(|(_, e)| e).cmp(&a.span.map(|(_, e)| e)).then_with(|| a.file.cmp(&b.file))
    });
    let mut sessions: BTreeMap<PathBuf, Vec<SessionInfo>> = BTreeMap::new();
    let mut sources = Vec::with_capacity(pairs.len());
    for (info, source) in pairs {
        sessions.entry(info.cwd.clone()).or_default().push(info);
        sources.push(source);
    }
    (sessions, sources)
}

/// Register every file in index order; group i belongs to `model.sessions[i]`.
/// Keep that order except for moving the session's main file first, when present.
pub fn sessions_of(harness: Harness, cwd: &Path, files: &[SessionFile]) -> (Model, Vec<Vec<SessionFile>>) {
    let mut model = Model::new(vec![cwd.to_path_buf()]);
    let mut groups: Vec<Vec<SessionFile>> = Vec::new();
    for f in files {
        let who = harness.ensure_participant(&mut model, &f.path, &f.project_dir);
        groups.resize_with(model.sessions.len(), Vec::new);
        if let Some(s) = model.participants[who.0].session {
            groups[s].push(f.clone());
        }
    }
    for (s, group) in model.sessions.iter().zip(&mut groups) {
        if let Some(i) = group.iter().position(|f| f.path == s.file) {
            group[..=i].rotate_right(1);
        }
    }
    (model, groups)
}

pub fn session_span(s: &Session) -> Option<(Ts, Ts)> {
    (s.last_seen != DateTime::<Utc>::MIN_UTC).then(|| {
        let end = s.end.unwrap_or(s.last_seen);
        (s.start.min(end), end)
    })
}

pub fn drill_for(cwd: &Path) -> Drill {
    if cwd.exists() { Drill::Local(cwd.to_path_buf()) } else { Drill::None }
}

fn collect_cwd(harness: Harness, cwd: &Path, files: &[SessionFile]) -> Vec<(SessionInfo, Source)> {
    let (mut model, groups) = sessions_of(harness, cwd, files);
    let drill = drill_for(cwd);
    groups
        .into_iter()
        .enumerate()
        .filter_map(|(i, files)| {
            let file = files.first()?;
            let s = &model.sessions[i];
            if file.path == s.file {
                let who = s.main;
                read_meta(harness, &mut model, who, file);
            }
            let s = &model.sessions[i];
            let info = SessionInfo {
                harness,
                cwd: cwd.to_path_buf(),
                file: file.path.clone(),
                drill: drill.clone(),
                title: s.title.clone(),
                span: session_span(s),
                activity: None,
                disk: None,
            };
            Some((info, Source { harness, cwd: cwd.to_path_buf(), files }))
        })
        .collect()
}

fn read_meta(harness: Harness, model: &mut Model, who: ParticipantId, f: &SessionFile) {
    let Ok(file) = File::open(&f.path) else { return };
    // Finish all I/O before ingesting: a failed read must not leave half a span.
    let read = || -> std::io::Result<Vec<Value>> {
        let mut reader = BufReader::new(file);
        let mut values = Vec::new();
        let mut line = Vec::new();
        let mut header_end = 0;
        for _ in 0..HEADER_LINES {
            line.clear();
            reader.read_until(b'\n', &mut line)?;
            if !line.ends_with(b"\n") {
                break;
            }
            header_end += line.len() as u64;
            if let Ok(v) = serde_json::from_slice::<Value>(&line) {
                let timestamped = v.get("timestamp").is_some();
                values.push(v);
                if timestamped {
                    break;
                }
            }
        }
        let len = reader.get_ref().metadata()?.len();
        let mut window = TAIL_BYTES;
        let mut bytes = Vec::new();
        loop {
            let tail_start = header_end.max(len.saturating_sub(window));
            reader.seek(SeekFrom::Start(tail_start))?;
            bytes.clear();
            reader.by_ref().take(len.saturating_sub(tail_start)).read_to_end(&mut bytes)?;
            let tail = if tail_start > header_end {
                bytes.iter().position(|&b| b == b'\n').map_or(&[][..], |i| &bytes[i + 1..])
            } else {
                &bytes
            };
            let mut pieces = tail.split(|&b| b == b'\n');
            pieces.next_back(); // Never ingest the final, potentially partial record.
            let parsed: Vec<Value> = pieces.filter_map(|p| serde_json::from_slice(p).ok()).collect();
            if parsed.iter().any(|v| v.get("timestamp").is_some())
                || tail_start == header_end
                || window >= MAX_TAIL_BYTES
            {
                values.extend(parsed);
                return Ok(values);
            }
            window *= 2;
        }
    };
    if let Ok(values) = read() {
        for v in values {
            harness.ingest(model, who, &v, &f.project_dir, IDLE_GAP);
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Git,
    NoGit,
    Missing,
}

#[derive(Debug, PartialEq, Eq)]
pub struct Project<T> {
    pub key: ProjectKey,
    pub kind: Kind,
    pub cwds: Vec<(PathBuf, T)>,
}

fn project_key(cwd: &Path) -> (PathBuf, Kind, PathBuf) {
    if !cwd.exists() {
        return (cwd.to_path_buf(), Kind::Missing, cwd.to_path_buf());
    }
    if let Some(common) = antty::worktree::git_common_dir(cwd) {
        let label = if common.file_name().is_some_and(|name| name == ".git") {
            common.parent().unwrap_or(&common).to_path_buf()
        } else {
            common.clone()
        };
        return (common, Kind::Git, label);
    }
    (cwd.to_path_buf(), Kind::NoGit, cwd.to_path_buf())
}

pub fn project_of(cwd: &Path) -> (ProjectKey, Kind) {
    let (_, kind, label) = project_key(cwd);
    (ProjectKey { origin: Origin::Local, path: label }, kind)
}

pub fn group<T>(by_cwd: BTreeMap<PathBuf, T>) -> Vec<Project<T>> {
    let mut projects: BTreeMap<PathBuf, Project<T>> = BTreeMap::new();
    for (cwd, counts) in by_cwd {
        let (key, kind, label) = project_key(&cwd);
        projects
            .entry(key)
            .or_insert_with(|| Project {
                key: ProjectKey { origin: Origin::Local, path: label },
                kind,
                cwds: Vec::new(),
            })
            .cwds
            .push((cwd, counts));
    }
    let mut projects: Vec<_> = projects.into_values().collect();
    projects.sort_by(|a, b| a.key.cmp(&b.key));
    projects
}
#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    #[test]
    fn read_meta_takes_header_tail_and_skips_partial_lines() {
        let base = std::env::temp_dir().join(format!("antty-formi-test-meta-{}", std::process::id()));
        let dir = base.join("sessions/proj");
        fs::create_dir_all(&dir).unwrap();
        let file = dir.join("s1.jsonl");
        let mut data = format!(
            "{{\"type\":\"title\",\"v\":1,\"title\":\"Fix bug\"}}\n{}\n",
            serde_json::json!({"type":"session","id":"s1","cwd":base,"timestamp":"2026-01-01T00:00:00.000Z"})
        );
        data.push_str(
            &"{\"type\":\"custom\",\"customType\":\"noop\",\"timestamp\":\"2026-01-01T00:01:00.000Z\"}\n".repeat(2000),
        );
        data.push_str(&format!(
            "{}\n",
            serde_json::json!({
                "type":"custom", "customType":"session_exit", "timestamp":"2026-01-01T01:00:00.000Z",
                "data":{"recordedAt":"2026-01-01T01:00:00.000Z"}, "pad":"x".repeat(100_000)
            })
        ));
        data.push_str("{\"type\":\"custom\",\"customType\":\"noop\",\"timestamp\":\"2026-01-01T02:00:00.000Z\"}");
        fs::write(&file, data).unwrap();
        let sessions: Vec<_> =
            collect_cwd(Harness::Omp, &base, &[SessionFile { project_dir: dir, path: file.clone() }])
                .into_iter()
                .map(|(info, _)| info)
                .collect();
        let at = |s: &str| s.parse::<Ts>().unwrap();
        assert_eq!(
            sessions,
            vec![SessionInfo {
                harness: Harness::Omp,
                cwd: base.clone(),
                file,
                drill: Drill::Local(base.clone()),
                title: "Fix bug".into(),
                span: Some((at("2026-01-01T00:00:00Z"), at("2026-01-01T01:00:00Z"))),
                activity: None,
                disk: None,
            }]
        );
        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn collect_cwd_records_no_drill_for_a_missing_directory() {
        let base = std::env::temp_dir().join(format!("antty-formi-test-gone-{}", std::process::id()));
        let dir = base.join("sessions/proj");
        fs::create_dir_all(&dir).unwrap();
        let file = dir.join("s1.jsonl");
        let gone = base.join("gone");
        let header = serde_json::json!({"type":"session","id":"s1","cwd":gone,"timestamp":"2026-01-01T00:00:00.000Z"});
        fs::write(&file, format!("{header}\n")).unwrap();
        let sessions: Vec<_> = collect_cwd(Harness::Omp, &gone, &[SessionFile { project_dir: dir, path: file }])
            .into_iter()
            .map(|(info, _)| info)
            .collect();
        fs::remove_dir_all(base).unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].drill, Drill::None);
    }

    #[test]
    fn group_folds_worktrees_and_keeps_plain_missing_and_subdir_cwds() {
        let base = std::env::temp_dir().join(format!("antty-formi-test-group-{}", std::process::id()));
        fs::create_dir_all(base.join("repo/.git/worktrees/feat")).unwrap();
        fs::create_dir_all(base.join("repo/sub")).unwrap();
        fs::create_dir_all(base.join("feat")).unwrap();
        fs::create_dir_all(base.join("plain")).unwrap();
        let base = base.canonicalize().unwrap();
        let repo = base.join("repo");
        let feat = base.join("feat");
        let sub = repo.join("sub");
        let plain = base.join("plain");
        let gone = base.join("gone");
        let admin = repo.join(".git/worktrees/feat");
        fs::write(admin.join("gitdir"), format!("{}\n", feat.join(".git").display())).unwrap();
        fs::write(admin.join("commondir"), "../..\n").unwrap();
        fs::write(feat.join(".git"), format!("gitdir: {}\n", admin.display())).unwrap();

        let counts = BTreeMap::from([
            (repo.clone(), [1, 0, 0]),
            (feat.clone(), [0, 2, 0]),
            (sub.clone(), [1, 0, 0]),
            (plain.clone(), [0, 0, 3]),
            (gone.clone(), [1, 0, 0]),
        ]);
        let local = |path: &Path| ProjectKey { origin: Origin::Local, path: path.to_path_buf() };
        assert_eq!(
            group(counts),
            vec![
                Project { key: local(&gone), kind: Kind::Missing, cwds: vec![(gone, [1, 0, 0])] },
                Project { key: local(&plain), kind: Kind::NoGit, cwds: vec![(plain, [0, 0, 3])] },
                Project { key: local(&repo), kind: Kind::Git, cwds: vec![(feat, [0, 2, 0]), (repo, [1, 0, 0])] },
                Project { key: local(&sub), kind: Kind::NoGit, cwds: vec![(sub, [1, 0, 0])] },
            ]
        );
        fs::remove_dir_all(base).unwrap();
    }
}
