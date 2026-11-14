//! queen: overview of every project with coding-agent sessions.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use antty::dirs::{default_claude_dir, default_codex_dir, default_omp_dir, expand_tilde};
use antty::model::Model;
use antty::sessions::Harness;
use clap::Parser;

#[derive(Parser, Debug)]
#[command(name = "queen", about = "Overview of coding-agent sessions across all projects")]
struct RawArgs {
    /// Root directory of omp's session jsonl files (default: ~/.omp/agent/sessions).
    #[arg(long)]
    omp_dir: Option<PathBuf>,
    /// Claude Code projects directory holding its session jsonl files (default: $CLAUDE_CONFIG_DIR/projects, else ~/.claude/projects).
    #[arg(long)]
    claude_dir: Option<PathBuf>,
    /// Codex sessions directory holding its rollout jsonl files (default: $CODEX_HOME/sessions, else ~/.codex/sessions).
    #[arg(long)]
    codex_dir: Option<PathBuf>,
}

fn harness_roots(raw: RawArgs) -> [(Harness, PathBuf); 3] {
    let home = std::env::var("HOME").unwrap_or_default();
    let omp_dir = raw.omp_dir.map(|p| expand_tilde(&p)).unwrap_or_else(|| default_omp_dir(&home));
    let claude_dir = raw
        .claude_dir
        .map(|p| expand_tilde(&p))
        .unwrap_or_else(|| default_claude_dir(std::env::var_os("CLAUDE_CONFIG_DIR"), &home));
    let codex_dir = raw
        .codex_dir
        .map(|p| expand_tilde(&p))
        .unwrap_or_else(|| default_codex_dir(std::env::var_os("CODEX_HOME"), &home));
    [(Harness::Omp, omp_dir), (Harness::Claude, claude_dir), (Harness::Codex, codex_dir)]
}

fn collect(roots: &[(Harness, PathBuf); 3]) -> BTreeMap<PathBuf, [usize; 3]> {
    let mut counts: BTreeMap<PathBuf, [usize; 3]> = BTreeMap::new();
    for (h, (harness, root)) in roots.iter().enumerate() {
        let index = harness.index_root(root);
        for cwd in index.cwds() {
            let mut model = Model::new(vec![cwd.to_path_buf()]);
            for f in index.files(cwd) {
                harness.ensure_participant(&mut model, &f.path, &f.project_dir);
            }
            counts.entry(cwd.to_path_buf()).or_default()[h] += model.sessions.len();
        }
    }
    counts
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Git,
    NoGit,
    Missing,
}

#[derive(Debug, PartialEq, Eq)]
struct Project {
    label: PathBuf,
    kind: Kind,
    cwds: Vec<(PathBuf, [usize; 3])>,
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

fn group(counts: BTreeMap<PathBuf, [usize; 3]>) -> Vec<Project> {
    let mut projects: BTreeMap<PathBuf, Project> = BTreeMap::new();
    for (cwd, counts) in counts {
        let (key, kind, label) = project_key(&cwd);
        projects.entry(key).or_insert_with(|| Project { label, kind, cwds: Vec::new() }).cwds.push((cwd, counts));
    }
    let mut projects: Vec<_> = projects.into_values().collect();
    projects.sort_by(|a, b| a.label.cmp(&b.label));
    projects
}

fn print(projects: &[Project]) {
    let mut cwd_totals = [0; 3];
    let mut norepo_totals = [0; 3];
    let mut session_totals = [0; 3];
    for project in projects {
        let mut totals = [0; 3];
        for (_, counts) in &project.cwds {
            for (h, &count) in counts.iter().enumerate() {
                totals[h] += count;
                if count > 0 {
                    cwd_totals[h] += 1;
                    if project.kind != Kind::Git {
                        norepo_totals[h] += 1;
                    }
                }
            }
        }
        for (total, count) in session_totals.iter_mut().zip(totals) {
            *total += count;
        }
        let kind = match project.kind {
            Kind::Git => "git",
            Kind::NoGit => "no-git",
            Kind::Missing => "missing",
        };
        println!("{}  [{}]  omp={} claude={} codex={}", project.label.display(), kind, totals[0], totals[1], totals[2]);
        for (cwd, counts) in &project.cwds {
            println!("  {}  omp={} claude={} codex={}", cwd.display(), counts[0], counts[1], counts[2]);
        }
    }
    if !projects.is_empty() {
        println!();
    }
    for (h, name) in ["omp", "claude", "codex"].iter().enumerate() {
        println!("{name}: {} cwds, {} without a repo, {} sessions", cwd_totals[h], norepo_totals[h], session_totals[h]);
    }
    println!("{} projects", projects.len());
}

fn main() -> anyhow::Result<()> {
    let roots = harness_roots(RawArgs::parse());
    let projects = group(collect(&roots));
    print(&projects);
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    #[test]
    fn group_folds_worktrees_and_keeps_plain_missing_and_subdir_cwds() {
        let base = std::env::temp_dir().join(format!("antty-queen-test-group-{}", std::process::id()));
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
        assert_eq!(
            group(counts),
            vec![
                Project { label: gone.clone(), kind: Kind::Missing, cwds: vec![(gone, [1, 0, 0])] },
                Project { label: plain.clone(), kind: Kind::NoGit, cwds: vec![(plain, [0, 0, 3])] },
                Project { label: repo.clone(), kind: Kind::Git, cwds: vec![(feat, [0, 2, 0]), (repo, [1, 0, 0])] },
                Project { label: sub.clone(), kind: Kind::NoGit, cwds: vec![(sub, [1, 0, 0])] },
            ]
        );
        fs::remove_dir_all(base).unwrap();
    }
}
