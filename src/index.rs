use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::model::{Model, Scope};
use crate::sessions::{Harness, TailSource};

const FORMAT: u32 = 1;
const TAIL_BYTES: u64 = 256;

#[derive(Clone)]
pub struct Spec {
    pub path: PathBuf,
    pub harness_roots: Vec<PathBuf>,
    pub idle_gap: i64,
    pub roots: Vec<PathBuf>,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct CachedTail {
    pub harness: Harness,
    pub project_root: PathBuf,
    pub project_dir: PathBuf,
    pub files: Vec<CachedFile>,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct CachedFile {
    pub path: PathBuf,
    pub offset: u64,
    pub tail: Vec<u8>,
}

#[derive(Serialize, Deserialize)]
struct Header {
    format: u32,
    version: String,
    harness_roots: Vec<PathBuf>,
    idle_gap: i64,
}

#[derive(Serialize)]
struct BodyRef<'a> {
    roots: &'a [PathBuf],
    tails: &'a [CachedTail],
    model: &'a Model,
}

#[derive(Deserialize)]
struct Body {
    roots: Vec<PathBuf>,
    tails: Vec<CachedTail>,
    model: Model,
}

pub fn path_for(state_dir: &Path, project: &Path) -> PathBuf {
    state_dir.join("index").join(format!("{}.bin", crate::attrib::state_file_stem(project)))
}

pub fn tails(sources: &[TailSource]) -> Result<Vec<CachedTail>, String> {
    sources
        .iter()
        .filter_map(|src| src.project_dir.as_ref().map(|dir| (src, dir)))
        .map(|(src, dir)| {
            let files = src
                .states
                .iter()
                .map(|(path, state)| {
                    let offset = state.offset - state.partial.len() as u64;
                    let tail = read_tail(path, offset, offset.min(TAIL_BYTES) as usize)
                        .map_err(|e| format!("{}: {e}", path.display()))?;
                    Ok(CachedFile { path: path.clone(), offset, tail })
                })
                .collect::<Result<_, String>>()?;
            Ok(CachedTail {
                harness: src.harness,
                project_root: src.project_root.clone(),
                project_dir: dir.clone(),
                files,
            })
        })
        .collect()
}

fn read_tail(path: &Path, offset: u64, len: usize) -> std::io::Result<Vec<u8>> {
    let mut file = File::open(path)?;
    file.seek(SeekFrom::Start(offset - len as u64))?;
    let mut tail = vec![0; len];
    file.read_exact(&mut tail)?;
    Ok(tail)
}

pub fn encode(spec: &Spec, model: &Model, tails: &[CachedTail]) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    let header = Header {
        format: FORMAT,
        version: env!("CARGO_PKG_VERSION").to_owned(),
        harness_roots: spec.harness_roots.clone(),
        idle_gap: spec.idle_gap,
    };
    rmp_serde::encode::write(&mut bytes, &header).map_err(|e| e.to_string())?;
    rmp_serde::encode::write(&mut bytes, &BodyRef { roots: &spec.roots, tails, model }).map_err(|e| e.to_string())?;
    Ok(bytes)
}

pub fn write(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut tmp = path.as_os_str().to_os_string();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    fs::write(&tmp, bytes)?;
    fs::rename(tmp, path)
}

pub fn load(spec: &Spec) -> Result<Option<(Model, Vec<CachedTail>)>, String> {
    let bytes = match fs::read(&spec.path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.to_string()),
    };
    let mut de = rmp_serde::Deserializer::new(&bytes[..]);
    let header = Header::deserialize(&mut de).map_err(|e| e.to_string())?;
    if header.format != FORMAT
        || header.version != env!("CARGO_PKG_VERSION")
        || header.harness_roots != spec.harness_roots
        || header.idle_gap != spec.idle_gap
    {
        return Ok(None);
    }
    let body = Body::deserialize(&mut de).map_err(|e| e.to_string())?;
    if !valid(&body.tails) {
        return Ok(None);
    }
    Ok(remap(body.model, &body.roots, &spec.roots).map(|model| (model, body.tails)))
}

fn valid(tails: &[CachedTail]) -> bool {
    tails.iter().flat_map(|tail| &tail.files).all(|file| {
        file.tail.len() as u64 <= file.offset
            && fs::metadata(&file.path).is_ok_and(|meta| meta.len() >= file.offset)
            && read_tail(&file.path, file.offset, file.tail.len()).is_ok_and(|tail| tail == file.tail)
    })
}

fn remap(mut model: Model, cached: &[PathBuf], current: &[PathBuf]) -> Option<Model> {
    if cached == current {
        return Some(model);
    }
    let map = cached.iter().map(|root| current.iter().position(|r| r == root)).collect::<Option<Vec<_>>>()?;
    if current
        .iter()
        .filter(|root| !cached.contains(root))
        .any(|new| cached.iter().any(|old| new.starts_with(old) || old.starts_with(new)))
    {
        return None;
    }
    let mut roots = Model::new(current.to_vec()).roots;
    for (old, &new) in map.iter().enumerate() {
        roots[new].aliases = std::mem::take(&mut model.roots[old].aliases);
    }
    model.roots = roots;
    for event in &mut model.events {
        if let Scope::Project(root) = &mut event.scope {
            *root = map[*root];
        }
    }
    let mut snapshots: Vec<_> = (0..current.len()).map(|_| Default::default()).collect();
    for (old, contents) in std::mem::take(&mut model.snapshots).into_iter().enumerate() {
        snapshots[map[old]] = contents;
    }
    model.snapshots = snapshots;
    Some(model)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Call, EventDetail, FileEvent, Pending, TouchKind, TouchSource};
    use crate::sessions::TailState;
    use std::collections::HashMap;

    fn event() -> FileEvent {
        let at = chrono::Utc::now();
        FileEvent {
            who: Model::YOU,
            rel: "a.rs".into(),
            scope: Scope::Project(0),
            kind: TouchKind::Write,
            source: TouchSource::Tool("edit".into()),
            start: at,
            end: at,
            tool_call_id: Some("c1".into()),
            detail: EventDetail::Diff("-a\n+b".into()),
        }
    }

    #[test]
    fn round_trip_resumes_only_unchanged_logs() {
        let base = std::env::temp_dir().join(format!("antty-index-test-roundtrip-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(&base).unwrap();
        let log = base.join("s.jsonl");
        let bytes = b"{\"type\":\"session\"}\n";
        fs::write(&log, bytes).unwrap();
        let project = base.join("project");
        let spec = Spec {
            path: path_for(&base, &project),
            harness_roots: vec![base.clone()],
            idle_gap: 30,
            roots: vec![project.clone()],
        };
        let mut model = Model::new(spec.roots.clone());
        model.events.push(event());
        model.calls.insert((0, "c1".into()), Call { tool: "edit".into(), input: "a.rs".into() });
        model.pending_tools.insert(
            (0, "c2".into()),
            Pending { tool: "write".into(), args: serde_json::json!({"path":"b.rs"}), start: model.events[0].start },
        );
        model.session_filter.insert(42);
        let sources = vec![TailSource {
            harness: Harness::Omp,
            root: base.clone(),
            project_root: project,
            project_dir: Some(base.clone()),
            states: HashMap::from([(log.clone(), TailState { offset: bytes.len() as u64, partial: Vec::new() })]),
        }];
        let tails = tails(&sources).unwrap();
        let encoded = encode(&spec, &model, &tails).unwrap();
        write(&spec.path, &encoded).unwrap();
        let (loaded, resumed) = load(&spec).unwrap().unwrap();
        assert_eq!(loaded.events[0].rel, Path::new("a.rs"));
        assert_eq!(loaded.events[0].scope, Scope::Project(0));
        assert_eq!(loaded.calls[&(0, "c1".into())].input, "a.rs");
        assert_eq!(loaded.pending_tools[&(0, "c2".into())].args["path"], "b.rs");
        assert!(loaded.session_filter.is_empty());
        assert_eq!(resumed[0].files[0].offset, bytes.len() as u64);
        // Appends do not invalidate a complete-line prefix.
        fs::write(&log, [bytes.as_slice(), b"new partial"].concat()).unwrap();
        assert!(load(&spec).unwrap().is_some());
        let mut changed = spec.clone();
        changed.idle_gap += 1;
        assert!(load(&changed).unwrap().is_none());
        changed = spec.clone();
        changed.harness_roots.push(base.join("other"));
        assert!(load(&changed).unwrap().is_none());
        let header = Header {
            format: FORMAT,
            version: "old".into(),
            harness_roots: spec.harness_roots.clone(),
            idle_gap: spec.idle_gap,
        };
        let mut old_version = Vec::new();
        rmp_serde::encode::write(&mut old_version, &header).unwrap();
        write(&spec.path, &old_version).unwrap();
        assert!(load(&spec).unwrap().is_none());
        write(&spec.path, &encoded).unwrap();
        let mut rewritten = bytes.to_vec();
        *rewritten.last_mut().unwrap() = b'x';
        fs::write(&log, rewritten).unwrap();
        assert!(load(&spec).unwrap().is_none());
        fs::write(&log, b"short").unwrap();
        assert!(load(&spec).unwrap().is_none());
        fs::remove_file(&log).unwrap();
        assert!(load(&spec).unwrap().is_none());
        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn tails_stop_before_partial_records_and_require_all_files() {
        let base = std::env::temp_dir().join(format!("antty-index-test-partial-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(&base).unwrap();
        let log = base.join("s.jsonl");
        fs::write(&log, b"complete\npartial").unwrap();
        let sources = vec![TailSource {
            harness: Harness::Omp,
            root: base.clone(),
            project_root: base.clone(),
            project_dir: Some(base.clone()),
            states: HashMap::from([(log.clone(), TailState { offset: 16, partial: b"partial".to_vec() })]),
        }];
        let cached = tails(&sources).unwrap();
        assert_eq!(cached[0].files[0].offset, 9);
        assert_eq!(cached[0].files[0].tail, b"complete\n");
        fs::remove_file(log).unwrap();
        assert!(tails(&sources).is_err());
        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn remap_moves_project_scopes_and_rejects_removed_or_nesting_roots() {
        let roots = vec![PathBuf::from("/a"), PathBuf::from("/b")];
        let make = || {
            let mut model = Model::new(roots.clone());
            model.roots[0].aliases.push("/alias-a".into());
            model.events.push(event());
            model.record_snapshot(Scope::Project(0), Path::new("a.rs"), model.events[0].start, "a".into());
            model
        };
        let current = vec![roots[1].clone(), roots[0].clone(), PathBuf::from("/c")];
        let model = remap(make(), &roots, &current).unwrap();
        assert_eq!(model.events[0].scope, Scope::Project(1));
        assert_eq!(model.snapshots[1][Path::new("a.rs")][0].1, "a");
        assert!(model.snapshots[0].is_empty());
        assert_eq!(model.roots[1].aliases, vec![PathBuf::from("/alias-a")]);
        assert!(remap(make(), &roots, &roots[..1]).is_none());
        assert!(remap(make(), &roots, &[roots[0].clone(), roots[1].clone(), PathBuf::from("/a/nested")]).is_none());
        assert!(remap(make(), &roots, &[roots[0].clone(), roots[1].clone(), PathBuf::from("/")]).is_none());
    }
}
