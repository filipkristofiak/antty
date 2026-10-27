use std::fs;
use std::path::{Path, PathBuf};

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

/// The shared gitignore-respecting project walk. Nested checkout roots are excluded so each
/// file belongs only to its own checkout's disk tree and snapshot map.
pub fn project_walker(root: &Path, nested: Vec<PathBuf>) -> ignore::Walk {
    ignore::WalkBuilder::new(root)
        .hidden(false)
        .git_ignore(true)
        .filter_entry(move |e| e.file_name() != ".git" && !nested.iter().any(|path| e.path() == path))
        .build()
}
