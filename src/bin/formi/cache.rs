use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use antty::index::{self, CachedFile};
use antty::model::Model;
use antty::sessions::{Harness, SessionFile};
use serde::{Deserialize, Serialize};

use crate::discover::IDLE_GAP;

const FORMAT: u32 = 1;

#[derive(Serialize, Deserialize)]
struct Header {
    format: u32,
    idle_gap: i64,
}

#[derive(Serialize, Deserialize)]
pub struct CachedSessionFile {
    pub project_dir: PathBuf,
    pub file: CachedFile,
}

#[derive(Serialize, Deserialize)]
pub struct Entry {
    pub harness: Harness,
    pub cwd: PathBuf,
    pub files: Vec<CachedSessionFile>,
    pub model: std::sync::Arc<Model>,
}

pub fn load(path: &Path) -> HashMap<PathBuf, Entry> {
    let Ok(bytes) = fs::read(path) else { return HashMap::new() };
    let mut de = rmp_serde::Deserializer::new(&bytes[..]);
    let Ok(header) = Header::deserialize(&mut de) else { return HashMap::new() };
    if header.format != FORMAT || header.idle_gap != IDLE_GAP {
        return HashMap::new();
    }
    let Ok(entries) = Vec::<Entry>::deserialize(&mut de) else { return HashMap::new() };
    entries.into_iter().filter_map(|e| Some((e.files.first()?.file.path.clone(), e))).collect()
}

pub fn save<'a>(path: &Path, entries: impl Iterator<Item = &'a Entry>) -> Result<(), String> {
    let mut bytes = Vec::new();
    rmp_serde::encode::write(&mut bytes, &Header { format: FORMAT, idle_gap: IDLE_GAP }).map_err(|e| e.to_string())?;
    rmp_serde::encode::write(&mut bytes, &entries.collect::<Vec<_>>()).map_err(|e| e.to_string())?;
    index::write(path, &bytes).map_err(|e| e.to_string())
}

pub fn valid(entry: &Entry, harness: Harness, cwd: &Path, files: &[SessionFile]) -> bool {
    entry.harness == harness
        && entry.cwd == cwd
        && entry.files.len() == files.len()
        && entry.files.iter().zip(files).all(|(cached, current)| {
            let f = &cached.file;
            f.path == current.path
                && cached.project_dir == current.project_dir
                && fs::metadata(&f.path).is_ok_and(|meta| meta.len() == f.offset)
                && index::tail_matches(f)
        })
}
