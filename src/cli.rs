use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use clap::Parser;

use antty::dirs::{default_claude_dir, default_codex_dir, default_omp_dir, default_state_dir, expand_tilde};

#[derive(Parser, Debug)]
#[command(name = "antty", about = "Gantt timeline TUI for coding-agent sessions")]
pub struct RawArgs {
    /// Project root to show sessions for. Defaults to the current directory.
    #[arg(long)]
    pub project: Option<PathBuf>,

    /// Root directory of omp's session jsonl files (default: ~/.omp/agent/sessions).
    #[arg(long)]
    pub omp_dir: Option<PathBuf>,

    /// Claude Code projects directory holding its session jsonl files (default: $CLAUDE_CONFIG_DIR/projects, else ~/.claude/projects).
    #[arg(long)]
    pub claude_dir: Option<PathBuf>,

    /// Codex sessions directory holding its rollout jsonl files (default: $CODEX_HOME/sessions, else ~/.codex/sessions).
    #[arg(long)]
    pub codex_dir: Option<PathBuf>,

    /// Directory used to persist watcher-observed filesystem changes and the binary session index (default: $XDG_STATE_HOME/antty, else ~/.local/state/antty).
    #[arg(long)]
    pub state_dir: Option<PathBuf>,

    /// Disable the live filesystem watcher; FILES is listed once at startup.
    #[arg(long)]
    pub no_watch: bool,
    /// Show only --project itself, not the other Git worktrees of its repository.
    #[arg(long)]
    pub no_worktrees: bool,
    /// Start with each Git worktree in its own FILES group instead of one merged tree (toggle with w).
    #[arg(long)]
    pub separate_worktrees: bool,

    /// Keep the timeline linear instead of collapsing idle stretches into ~ breaks.
    #[arg(long)]
    pub no_collapse_gaps: bool,

    /// Idle gap (seconds) used to merge raw activity intervals into spans.
    #[arg(long, default_value_t = 30)]
    pub idle_gap: i64,
}

#[derive(Debug, Clone)]
pub struct Args {
    pub project: PathBuf,
    pub omp_dir: PathBuf,
    pub claude_dir: PathBuf,
    pub codex_dir: PathBuf,
    pub state_dir: PathBuf,
    pub no_watch: bool,
    pub no_worktrees: bool,
    pub separate_worktrees: bool,
    pub no_collapse_gaps: bool,
    pub idle_gap: i64,
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

        let omp_dir = raw.omp_dir.map(|p| expand_tilde(&p)).unwrap_or_else(|| default_omp_dir(&home));
        let claude_dir = raw
            .claude_dir
            .map(|p| expand_tilde(&p))
            .unwrap_or_else(|| default_claude_dir(std::env::var_os("CLAUDE_CONFIG_DIR"), &home));
        let codex_dir = raw
            .codex_dir
            .map(|p| expand_tilde(&p))
            .unwrap_or_else(|| default_codex_dir(std::env::var_os("CODEX_HOME"), &home));

        let state_dir = match raw.state_dir {
            Some(p) => expand_tilde(&p),
            None => default_state_dir(std::env::var_os("XDG_STATE_HOME"), std::env::var_os("HOME"))?,
        };
        Ok(Args {
            project,
            omp_dir,
            claude_dir,
            codex_dir,
            state_dir,
            no_watch: raw.no_watch,
            no_worktrees: raw.no_worktrees,
            separate_worktrees: raw.separate_worktrees,
            no_collapse_gaps: raw.no_collapse_gaps,
            idle_gap: raw.idle_gap,
        })
    }
}
