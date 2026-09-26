use std::fs;
use std::path::Path;

use crate::model::{Model, Ts};

/// Files larger than this are never snapshotted (avoid pulling huge blobs into memory just to
/// diff them).
pub const MAX_SNAPSHOT_BYTES: u64 = 1 << 20;

/// Read a file's full UTF-8 text content. `None` for a missing file, one over
/// `MAX_SNAPSHOT_BYTES`, or non-UTF-8 (binary) content.
pub fn read_text(path: &Path) -> Option<String> {
    let meta = fs::metadata(path).ok()?;
    if meta.len() > MAX_SNAPSHOT_BYTES {
        return None;
    }
    let bytes = fs::read(path).ok()?;
    String::from_utf8(bytes).ok()
}

/// A unified diff (3 lines of context) between two full-file contents.
pub fn unified(old: &str, new: &str) -> String {
    similar::TextDiff::from_lines(old, new).unified_diff().context_radius(3).to_string()
}

/// Walk the project tree (same filter as `tree::Tree::build`) and record every readable regular
/// file's current content as a snapshot at `at`, so the first diff view after startup has a
/// baseline even for files never touched by a logged write/edit.
pub fn seed_from_disk(model: &mut Model, at: Ts) {
    let root = model.root.clone();
    let walker = ignore::WalkBuilder::new(&root)
        .hidden(false)
        .git_ignore(true)
        .filter_entry(|e| e.file_name() != ".git")
        .build();
    for entry in walker.flatten() {
        let path = entry.path();
        if path == root {
            continue;
        }
        let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
        if is_dir {
            continue;
        }
        let Ok(rel) = path.strip_prefix(&root) else { continue };
        if let Some(content) = read_text(path) {
            model.record_snapshot(rel, at, content);
        }
    }
}
