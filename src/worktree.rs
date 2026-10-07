use std::fs;
use std::path::{Path, PathBuf};

fn first_line(path: &Path) -> Option<String> {
    Some(fs::read_to_string(path).ok()?.lines().next()?.trim().to_owned())
}

fn resolve(base: &Path, value: &str) -> PathBuf {
    base.join(value)
}

pub fn git_common_dir(worktree: &Path) -> Option<PathBuf> {
    let dot_git = worktree.join(".git");
    if dot_git.is_dir() {
        return dot_git.canonicalize().ok();
    }
    if !dot_git.is_file() {
        return None;
    }
    let line = first_line(&dot_git)?;
    let git_dir = resolve(worktree, line.strip_prefix("gitdir: ")?.trim()).canonicalize().ok()?;
    let common = git_dir.join("commondir");
    if common.exists() { resolve(&git_dir, &first_line(&common)?).canonicalize().ok() } else { Some(git_dir) }
}

pub fn discover(project: &Path) -> Vec<PathBuf> {
    let mut roots = vec![project.to_path_buf()];
    let Some(common) = git_common_dir(project) else {
        return roots;
    };
    if common.file_name().is_some_and(|name| name == ".git")
        && let Some(parent) = common.parent()
        && parent.join(".git").is_dir()
        && !roots.contains(&parent.to_path_buf())
    {
        roots.push(parent.to_path_buf());
    }
    if let Ok(entries) = fs::read_dir(common.join("worktrees")) {
        let mut entries: Vec<_> = entries.filter_map(Result::ok).collect();
        entries.sort_by_key(|entry| entry.path());
        for entry in entries {
            let admin = entry.path();
            let Some(gitdir) = first_line(&admin.join("gitdir")) else {
                continue;
            };
            let dot_git = resolve(&admin, &gitdir);
            if !dot_git.is_file() {
                continue;
            }
            let Some(candidate) = dot_git.parent().and_then(|p| p.canonicalize().ok()) else {
                continue;
            };
            if !roots.contains(&candidate) {
                roots.push(candidate);
            }
        }
    }
    roots
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discovers_main_linked_and_stale_worktrees() {
        let base = std::env::temp_dir().join(format!("antty-worktree-test-discovery-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        let repo = base.join("repo");
        let feat = base.join("feat");
        let admin = repo.join(".git/worktrees/feat");
        fs::create_dir_all(&admin).unwrap();
        fs::create_dir_all(&feat).unwrap();
        fs::create_dir_all(repo.join(".git/worktrees/gone")).unwrap();
        fs::create_dir_all(base.join("plain")).unwrap();
        let base = base.canonicalize().unwrap();
        let repo = base.join("repo");
        let feat = base.join("feat");
        let admin = repo.join(".git/worktrees/feat");
        fs::write(admin.join("gitdir"), format!("{}\n", feat.join(".git").display())).unwrap();
        fs::write(admin.join("commondir"), "../..\n").unwrap();
        fs::write(feat.join(".git"), format!("gitdir: {}\n", admin.display())).unwrap();
        fs::write(repo.join(".git/worktrees/gone/gitdir"), "/missing/.git\n").unwrap();

        assert_eq!(git_common_dir(&feat), Some(repo.join(".git")));
        assert_eq!(discover(&repo), vec![repo.clone(), feat.clone()]);
        assert_eq!(discover(&feat), vec![feat.clone(), repo.clone()]);
        assert_eq!(discover(&base.join("plain")), vec![base.join("plain")]);

        fs::write(feat.join(".git"), "gitdir: ../repo/.git/worktrees/feat\n").unwrap();
        assert_eq!(git_common_dir(&feat), Some(repo.join(".git")));
        fs::remove_dir_all(base).unwrap();
    }
}
