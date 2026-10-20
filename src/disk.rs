use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{self, Sender, SyncSender};

use chrono::Utc;

use crate::sessions::Msg;
use crate::snapshot;

/// Gitignore-respecting listing of one checkout: root-relative path → is_dir.
pub type Listing = HashMap<PathBuf, bool>;

#[derive(Default)]
pub struct DiskSnapshot {
    /// Per checkout; `None` until that checkout has been walked.
    pub listings: Vec<Option<Arc<Listing>>>,
    /// Merged-view FILES disk trie, available once every checkout has been walked.
    pub merged: Option<crate::tree::Trie>,
}

impl DiskSnapshot {
    pub fn new(listings: Vec<Option<Arc<Listing>>>) -> Self {
        let merged = (listings.len() > 1 && listings.iter().all(Option::is_some))
            .then(|| crate::tree::merged_disk_trie(&listings));
        Self { listings, merged }
    }

    /// Some(listed?) once checkout `root` was walked; None before.
    pub fn listed(&self, root: usize, rel: &Path) -> Option<bool> {
        self.listings.get(root)?.as_ref().map(|listing| listing.contains_key(rel))
    }
}

fn walk(root: &Path, nested: Vec<PathBuf>, seed: bool) -> (Listing, Vec<(PathBuf, String)>) {
    let mut listing = Listing::new();
    let mut files = Vec::new();
    for entry in snapshot::project_walker(root, nested).flatten() {
        let path = entry.path();
        if path == root {
            continue;
        }
        let Ok(rel) = path.strip_prefix(root) else { continue };
        if rel.as_os_str().is_empty() {
            continue;
        }
        let is_dir = entry.file_type().is_some_and(|kind| kind.is_dir());
        listing.insert(rel.to_path_buf(), is_dir);
        if seed
            && !is_dir
            && let Some(content) = snapshot::read_text(path)
        {
            files.push((rel.to_path_buf(), content));
        }
    }
    (listing, files)
}

/// List checkouts off-thread, then refresh requested checkouts in coalesced batches.
/// Only checkout zero (the selected project) receives content baselines.
pub fn spawn_lister(roots: Vec<(PathBuf, Vec<PathBuf>)>, seed: bool, tx: SyncSender<Msg>) -> Sender<usize> {
    let (request_tx, request_rx) = mpsc::channel();
    std::thread::spawn(move || {
        let at = Utc::now();
        let count = roots.len();
        let next = AtomicUsize::new(0);
        let workers = std::thread::available_parallelism().map_or(1, |n| n.get()).min(count);
        let mut listings = vec![None; count];
        let mut files = Vec::new();
        std::thread::scope(|scope| {
            let handles: Vec<_> = (0..workers)
                .map(|_| {
                    let roots = &roots;
                    let next = &next;
                    scope.spawn(move || {
                        let mut walked = Vec::new();
                        loop {
                            let i = next.fetch_add(1, Ordering::Relaxed);
                            let Some((root, nested)) = roots.get(i) else { break };
                            let (listing, files) = walk(root, nested.clone(), seed && i == 0);
                            walked.push((i, listing, files));
                        }
                        walked
                    })
                })
                .collect();
            for handle in handles {
                for (i, listing, seeded) in handle.join().expect("disk listing worker panicked") {
                    listings[i] = Some(Arc::new(listing));
                    if i == 0 {
                        files = seeded;
                    }
                }
            }
        });
        if seed && tx.send(Msg::Seed { at, files }).is_err() {
            return;
        }
        let snapshot = Box::new(DiskSnapshot::new(listings.clone()));
        if tx.send(Msg::Disk { snapshot, walked: (0..count).collect() }).is_err() {
            return;
        }
        while let Ok(first) = request_rx.recv() {
            let batch: BTreeSet<usize> = std::iter::once(first).chain(request_rx.try_iter()).collect();
            for &i in &batch {
                let (root, nested) = &roots[i];
                let (listing, _) = walk(root, nested.clone(), false);
                listings[i] = Some(Arc::new(listing));
            }
            let snapshot = Box::new(DiskSnapshot::new(listings.clone()));
            if tx.send(Msg::Disk { snapshot, walked: batch.into_iter().collect() }).is_err() {
                return;
            }
        }
    });
    request_tx
}

/// Populate cached disk listings synchronously for tests that construct a model directly.
#[cfg(test)]
pub fn list_now(model: &mut crate::model::Model) {
    let listings = model
        .roots
        .iter()
        .enumerate()
        .map(|(i, root)| Some(Arc::new(walk(&root.path, model.nested_roots(i), false).0)))
        .collect();
    model.disk = DiskSnapshot::new(listings);
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    #[test]
    fn walk_excludes_nested_checkout_and_seeds_text() {
        let base = std::env::temp_dir().join(format!("antty-disk-test-nested-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        let outer = base.join("outer");
        let nested = outer.join("nested");
        fs::create_dir_all(outer.join("src")).unwrap();
        fs::create_dir_all(nested.join("src")).unwrap();
        fs::write(outer.join("src/a.rs"), "outer").unwrap();
        fs::write(nested.join("src/a.rs"), "inner").unwrap();
        fs::write(outer.join("binary"), [0xff]).unwrap();

        let (listing, files) = walk(&outer, vec![nested], true);
        assert_eq!(listing.get(Path::new("src")), Some(&true));
        assert_eq!(listing.get(Path::new("src/a.rs")), Some(&false));
        assert!(!listing.keys().any(|path| path.starts_with("nested")));
        assert_eq!(files, vec![(PathBuf::from("src/a.rs"), "outer".to_owned())]);
        assert!(walk(&outer, Vec::new(), false).1.is_empty());

        fs::remove_dir_all(base).unwrap();
    }
}
