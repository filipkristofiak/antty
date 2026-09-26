use std::fs;
use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;

use chrono::Utc;
use ignore::Match;
use ignore::gitignore::{Gitignore, GitignoreBuilder};
use notify::event::{EventKind, ModifyKind, RenameMode};
use notify::{Event, RecommendedWatcher, RecursiveMode, Watcher};

use crate::model::Ts;
use crate::sessions::Msg;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FsKind {
    Created,
    Modified,
    Removed,
}

#[derive(Debug, Clone)]
pub struct RawFs {
    pub path: PathBuf,
    pub kind: FsKind,
    pub at: Ts,
}

/// One `.gitignore`'s rules, rooted at the directory that contains it. A project can have many
/// of these (one per directory with its own `.gitignore`); the watcher needs to honor all of
/// them, not just the project root's, or a nested `target/`/`node_modules/` rule is invisible to
/// it even though the static tree walk (which uses `ignore::WalkBuilder` directly) already
/// respects it.
struct IgnoreLayer {
    base: PathBuf,
    matcher: Gitignore,
}

pub struct GitignoreSet {
    root: PathBuf,
    /// Deepest base first, so a more specific `.gitignore` is consulted (and can override)
    /// before a shallower one, approximating git's own precedence.
    layers: Vec<IgnoreLayer>,
}

impl GitignoreSet {
    pub fn build(root: &Path) -> Self {
        let mut layers = Vec::new();
        Self::collect(root, root, &mut layers);
        layers.sort_by_key(|l| std::cmp::Reverse(l.base.components().count()));
        GitignoreSet { root: root.to_path_buf(), layers }
    }

    fn collect(dir: &Path, root: &Path, layers: &mut Vec<IgnoreLayer>) {
        let mut builder = GitignoreBuilder::new(dir);
        let mut any = false;
        let gi = dir.join(".gitignore");
        if gi.is_file() {
            let _ = builder.add(&gi);
            any = true;
        }
        if dir == root {
            let exclude = dir.join(".git/info/exclude");
            if exclude.is_file() {
                let _ = builder.add(&exclude);
                any = true;
            }
        }
        if any
            && let Ok(matcher) = builder.build()
        {
            layers.push(IgnoreLayer { base: dir.to_path_buf(), matcher });
        }

        let Ok(rd) = fs::read_dir(dir) else { return };
        for entry in rd.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            if path.file_name().and_then(|n| n.to_str()) == Some(".git") {
                continue;
            }
            // Don't bother discovering .gitignore files inside a directory that's already
            // ignored by the rules collected so far (an ancestor chain, since this is a
            // pre-order walk): its own rules can never make anything under it visible again in
            // a way that matters here, and skipping avoids descending into huge ignored trees
            // (e.g. node_modules) just to look for .gitignore files.
            if Self::is_ignored_with(root, layers, &path, true) {
                continue;
            }
            Self::collect(&path, root, layers);
        }
    }

    fn is_ignored_with(root: &Path, layers: &[IgnoreLayer], path: &Path, is_dir: bool) -> bool {
        if path.components().any(|c| c.as_os_str() == ".git") {
            return true;
        }
        if !path.starts_with(root) {
            // Defensive: `matched_path_or_any_parents` asserts the path is under the matcher's
            // base; this should never happen since we only ever watch under `root`.
            return true;
        }
        for layer in layers {
            if !path.starts_with(&layer.base) {
                continue;
            }
            match layer.matcher.matched_path_or_any_parents(path, is_dir) {
                Match::Ignore(_) => return true,
                Match::Whitelist(_) => return false,
                Match::None => {}
            }
        }
        false
    }

    pub fn is_ignored(&self, path: &Path, is_dir: bool) -> bool {
        Self::is_ignored_with(&self.root, &self.layers, path, is_dir)
    }
}

fn send(tx: &Sender<Msg>, path: PathBuf, kind: FsKind, at: Ts) {
    let _ = tx.send(Msg::Fs(RawFs { path, kind, at }));
}

fn handle_event(gi: &GitignoreSet, event: Event, tx: &Sender<Msg>) {
    let at = Utc::now();
    match event.kind {
        EventKind::Create(_) => {
            for path in event.paths {
                if path.is_dir() || gi.is_ignored(&path, false) {
                    continue;
                }
                send(tx, path, FsKind::Created, at);
            }
        }
        EventKind::Modify(ModifyKind::Name(mode)) => {
            let paths = event.paths;
            match mode {
                RenameMode::Both if paths.len() == 2 => {
                    let from = paths[0].clone();
                    let to = paths[1].clone();
                    if !gi.is_ignored(&from, false) {
                        send(tx, from, FsKind::Removed, at);
                    }
                    if to.is_file() && !gi.is_ignored(&to, false) {
                        send(tx, to, FsKind::Created, at);
                    }
                }
                RenameMode::From => {
                    for path in paths {
                        if gi.is_ignored(&path, false) {
                            continue;
                        }
                        send(tx, path, FsKind::Removed, at);
                    }
                }
                RenameMode::To => {
                    for path in paths {
                        if path.is_dir() || gi.is_ignored(&path, false) {
                            continue;
                        }
                        send(tx, path, FsKind::Created, at);
                    }
                }
                _ => {
                    for path in paths {
                        if gi.is_ignored(&path, false) {
                            continue;
                        }
                        if path.exists() {
                            if !path.is_dir() {
                                send(tx, path, FsKind::Created, at);
                            }
                        } else {
                            send(tx, path, FsKind::Removed, at);
                        }
                    }
                }
            }
        }
        EventKind::Modify(_) => {
            for path in event.paths {
                if path.is_dir() || gi.is_ignored(&path, false) {
                    continue;
                }
                send(tx, path, FsKind::Modified, at);
            }
        }
        EventKind::Remove(_) => {
            for path in event.paths {
                if gi.is_ignored(&path, false) {
                    continue;
                }
                send(tx, path, FsKind::Removed, at);
            }
        }
        _ => {}
    }
}

/// Spawn the recursive fs watcher on `root`. The returned watcher must be kept alive
/// (bound to a named variable) for the duration of the program; dropping it stops watching.
pub fn spawn_watcher(root: PathBuf, tx: Sender<Msg>) -> Option<RecommendedWatcher> {
    let gitignore = GitignoreSet::build(&root);
    let mut watcher = notify::recommended_watcher(move |res: notify::Result<Event>| {
        if let Ok(event) = res {
            handle_event(&gitignore, event, &tx);
        }
    })
    .ok()?;
    watcher.watch(&root, RecursiveMode::Recursive).ok()?;
    Some(watcher)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nested_gitignore_is_honored_even_without_a_root_one() {
        // Matches the real omp-extensions layout: no root .gitignore, but a nested crate's own
        // .gitignore lists a build dir that must still be filtered by the root-level watcher.
        let root = std::env::temp_dir().join(format!("omp-gantt-watch-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let sub = root.join("crate");
        fs::create_dir_all(sub.join("target/debug/deps")).unwrap();
        fs::write(sub.join("target/debug/deps/foo.o"), "obj").unwrap();
        fs::write(sub.join(".gitignore"), "target/\n").unwrap();
        fs::write(root.join("a.txt"), "a").unwrap();

        let set = GitignoreSet::build(&root);

        assert!(
            set.is_ignored(&sub.join("target/debug/deps/foo.o"), false),
            "a nested .gitignore's rules must be honored even though the root has none"
        );
        assert!(!set.is_ignored(&root.join("a.txt"), false), "an untouched-by-any-rule file must not be ignored");
    }
}
