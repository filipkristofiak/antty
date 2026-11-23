//! Apparent size of session logs and their adjacent artifact directories.

use std::fs;
use std::path::{Component, Path, PathBuf};

/// `<stem>/` beside `log`: the log path without its `.jsonl` extension.
pub fn artifact_dir(log: &Path) -> PathBuf {
    log.with_extension("")
}

/// Apparent size of `log` plus everything under its artifact directory; missing paths count 0.
pub fn of_log(log: &Path) -> u64 {
    let artifacts = artifact_dir(log);
    let log_bytes = fs::symlink_metadata(log).ok().filter(|m| m.is_file()).map_or(0, |m| m.len());
    let artifact_bytes =
        if fs::symlink_metadata(&artifacts).is_ok_and(|m| m.is_dir()) { dir_size(&artifacts) } else { 0 };
    log_bytes + artifact_bytes
}

/// Recursive apparent size of regular files under `dir`. Symlinks are not followed or counted;
/// unreadable entries count 0.
fn dir_size(dir: &Path) -> u64 {
    let Ok(entries) = fs::read_dir(dir) else {
        return 0;
    };
    entries
        .flatten()
        .map(|entry| match entry.file_type() {
            Ok(t) if t.is_dir() => dir_size(&entry.path()),
            Ok(t) if t.is_file() => entry.metadata().map_or(0, |m| m.len()),
            _ => 0,
        })
        .sum()
}

/// Whether `path` starts with `log`'s artifact directory, without allocating the directory path.
pub(crate) fn in_artifact_dir(log: &Path, path: &Path) -> bool {
    let (Some(parent), Some(stem)) = (log.parent(), log.file_stem()) else { return false };
    path.strip_prefix(parent).ok().and_then(|p| p.components().next()) == Some(Component::Normal(stem))
}

/// Footprint of one session from its logs: logs inside another listed log's artifact
/// directory (omp and Claude Code subagents) are already counted by that log.
pub fn session(logs: &[&Path], footprint: impl Fn(&Path) -> u64) -> u64 {
    logs.iter()
        .filter(|log| !logs.iter().any(|other| other != *log && in_artifact_dir(other, log)))
        .map(|log| footprint(log))
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn measures_artifacts_without_double_counting_nested_logs() {
        let dir = std::env::temp_dir().join(format!("antty-footprint-test-{}", std::process::id()));
        fs::create_dir_all(dir.join("s/deep")).unwrap();
        let main = dir.join("s.jsonl");
        let sub = dir.join("s/sub.jsonl");
        let sibling = dir.join("r.jsonl");
        fs::write(&main, [0; 10]).unwrap();
        fs::write(&sub, [0; 5]).unwrap();
        fs::write(dir.join("s/a.md"), [0; 7]).unwrap();
        fs::write(dir.join("s/deep/x.png"), [0; 3]).unwrap();
        fs::write(dir.join("outside"), [0; 100]).unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(dir.join("outside"), dir.join("s/link")).unwrap();
        fs::write(&sibling, [0; 4]).unwrap();

        assert_eq!(of_log(&main), 25);
        assert_eq!(of_log(&sub), 5);
        assert_eq!(session(&[&main, &sub, &sibling], of_log), 29);
        assert_eq!(of_log(&dir.join("missing.jsonl")), 0);
        fs::remove_dir_all(dir).unwrap();
    }
}
