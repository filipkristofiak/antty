use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use clap::Parser;

#[derive(Parser, Debug)]
#[command(name = "omp-gantt", about = "Session Gantt TUI for omp projects")]
pub struct RawArgs {
    /// Project root to show sessions for. Defaults to the current directory.
    #[arg(long)]
    pub project: Option<PathBuf>,

    /// Root directory holding omp's session jsonl files.
    #[arg(long)]
    pub sessions_dir: Option<PathBuf>,

    /// Directory used to persist watcher-observed filesystem changes.
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

        let state_dir = raw
            .state_dir
            .map(|p| expand_tilde(&p))
            .unwrap_or_else(|| PathBuf::from(&home).join(".local/state/omp-gantt"));

        Ok(Args {
            project,
            sessions_dir,
            state_dir,
            no_watch: raw.no_watch,
            idle_gap: raw.idle_gap,
        })
    }
}
