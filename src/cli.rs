use std::ffi::OsString;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use clap::Parser;

#[derive(Parser, Debug)]
#[command(name = "antty", about = "Gantt timeline TUI for coding-agent sessions")]
pub struct RawArgs {
    /// Project root to show sessions for. Defaults to the current directory.
    #[arg(long)]
    pub project: Option<PathBuf>,

    /// Root directory holding the agent session jsonl files (default: omp's ~/.omp/agent/sessions).
    #[arg(long)]
    pub sessions_dir: Option<PathBuf>,

    /// Directory used to persist watcher-observed filesystem changes (default: $XDG_STATE_HOME/antty, else ~/.local/state/antty).
    #[arg(long)]
    pub state_dir: Option<PathBuf>,

    /// Disable the live filesystem watcher.
    #[arg(long)]
    pub no_watch: bool,

    /// Idle gap (seconds) used to merge raw activity intervals into spans.
    #[arg(long, default_value_t = 30)]
    pub idle_gap: i64,
}

#[derive(Debug, Clone)]
pub struct Args {
    pub project: PathBuf,
    pub sessions_dir: PathBuf,
    pub state_dir: PathBuf,
    pub no_watch: bool,
    pub idle_gap: i64,
}

fn expand_tilde(p: &Path) -> PathBuf {
    if let Ok(s) = p.strip_prefix("~")
        && let Ok(home) = std::env::var("HOME") {
            return Path::new(&home).join(s);
        }
    p.to_path_buf()
}

fn default_state_dir(xdg_state_home: Option<OsString>, home: Option<OsString>) -> Result<PathBuf> {
    if let Some(dir) = xdg_state_home.filter(|p| !p.is_empty() && Path::new(p).is_absolute()) {
        return Ok(PathBuf::from(dir).join("antty"));
    }
    if let Some(dir) = home.filter(|p| !p.is_empty() && Path::new(p).is_absolute()) {
        return Ok(PathBuf::from(dir).join(".local/state/antty"));
    }
    bail!("cannot choose a default --state-dir: neither $XDG_STATE_HOME nor $HOME is an absolute path; pass --state-dir")
}

impl Args {
    pub fn parse() -> Result<Self> {
        let raw = RawArgs::parse();
        let home = std::env::var("HOME").unwrap_or_default();

        let project_raw = raw
            .project
            .map(|p| expand_tilde(&p))
            .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
        if !project_raw.exists() {
            bail!("--project path does not exist: {}", project_raw.display());
        }
        let project = project_raw
            .canonicalize()
            .with_context(|| format!("failed to canonicalize --project {}", project_raw.display()))?;

        let sessions_dir = raw
            .sessions_dir
            .map(|p| expand_tilde(&p))
            .unwrap_or_else(|| PathBuf::from(&home).join(".omp/agent/sessions"));

        let state_dir = match raw.state_dir {
            Some(p) => expand_tilde(&p),
            None => default_state_dir(std::env::var_os("XDG_STATE_HOME"), std::env::var_os("HOME"))?,
        };
        Ok(Args {
            project,
            sessions_dir,
            state_dir,
            no_watch: raw.no_watch,
            idle_gap: raw.idle_gap,
        })
    }
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;
    use std::path::PathBuf;

    use super::default_state_dir;

    #[test]
    fn default_state_dir_uses_absolute_xdg_or_home() {
        let home = Some(OsString::from("/home/tester"));
        assert_eq!(
            default_state_dir(Some(OsString::from("/var/state")), home.clone()).unwrap(),
            PathBuf::from("/var/state/antty")
        );
        for xdg in [OsString::from("rel"), OsString::new()] {
            assert_eq!(
                default_state_dir(Some(xdg), home.clone()).unwrap(),
                PathBuf::from("/home/tester/.local/state/antty")
            );
        }
        assert!(default_state_dir(None, None).is_err());
        assert!(default_state_dir(None, Some(OsString::new())).is_err());
    }
}
