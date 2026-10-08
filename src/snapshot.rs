use std::fs;
use std::path::{Path, PathBuf};

use crate::model::{Model, Scope, Ts};

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

/// Walk the project tree (same filter as `tree::Tree::build`) and record every readable regular
/// file's current content as a snapshot at `at`, so the first diff view after startup has a
/// baseline even for files never touched by a logged write/edit.
pub fn seed_from_disk(model: &mut Model, at: Ts) {
    for i in 0..model.roots.len() {
        let root = model.roots[i].path.clone();
        for entry in project_walker(&root, model.nested_roots(i)).flatten() {
            let path = entry.path();
            if path == root || entry.file_type().is_some_and(|t| t.is_dir()) {
                continue;
            }
            let Ok(rel) = path.strip_prefix(&root) else { continue };
            if let Some(content) = read_text(path) {
                model.record_snapshot(Scope::Project(i), rel, at, content);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seed_uses_separate_snapshot_maps_and_skips_nested_worktree() {
        let base = std::env::temp_dir().join(format!("antty-snapshot-test-nested-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        let outer = base.join("outer");
        let inner = outer.join("nested");
        fs::create_dir_all(outer.join("src")).unwrap();
        fs::create_dir_all(inner.join("src")).unwrap();
        fs::write(outer.join("src/a.rs"), "outer").unwrap();
        fs::write(inner.join("src/a.rs"), "inner").unwrap();

        let mut model = Model::new(vec![outer.canonicalize().unwrap(), inner.canonicalize().unwrap()]);
        seed_from_disk(&mut model, chrono::Utc::now());
        let rel = Path::new("src/a.rs");
        assert_eq!(model.snapshots[0].get(rel).unwrap()[0].1, "outer");
        assert_eq!(model.snapshots[1].get(rel).unwrap()[0].1, "inner");
        assert!(!model.snapshots[0].contains_key(Path::new("nested/src/a.rs")));

        fs::remove_dir_all(base).unwrap();
    }
}
