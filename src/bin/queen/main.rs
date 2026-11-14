//! queen: overview of every project with coding-agent sessions.

mod app;
mod discover;
mod ui;

use std::path::PathBuf;

use antty::dirs::{default_claude_dir, default_codex_dir, default_omp_dir, expand_tilde};
use antty::sessions::Harness;
use clap::Parser;
use discover::{Kind, Project, SessionInfo, collect, group};

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
    /// Print the project list with per-harness session counts instead of opening the TUI.
    #[arg(long)]
    list: bool,
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

fn print(projects: &[Project<Vec<SessionInfo>>]) {
    let mut cwd_totals = [0; 3];
    let mut norepo_totals = [0; 3];
    let mut session_totals = [0; 3];
    for project in projects {
        let mut totals = [0; 3];
        for (_, sessions) in &project.cwds {
            for (h, count) in counts(sessions).into_iter().enumerate() {
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
        for (cwd, sessions) in &project.cwds {
            let counts = counts(sessions);
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

fn counts(sessions: &[SessionInfo]) -> [usize; 3] {
    [Harness::Omp, Harness::Claude, Harness::Codex].map(|h| sessions.iter().filter(|s| s.harness == h).count())
}

fn main() -> anyhow::Result<()> {
    let raw = RawArgs::parse();
    let list = raw.list;
    let projects = group(collect(&harness_roots(raw)));
    if list {
        print(&projects);
        return Ok(());
    }
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
    let mut app = app::App::new(app::project_views(projects), home);
    let mut terminal = ratatui::init();
    let result = run_loop(&mut terminal, &mut app);
    ratatui::restore();
    result
}

fn run_loop(terminal: &mut ratatui::DefaultTerminal, app: &mut app::App) -> anyhow::Result<()> {
    use crossterm::event::{self, Event, KeyEventKind};
    use std::time::Duration;

    let size = terminal.size()?;
    app.layout = ui::layout(ratatui::layout::Rect::new(0, 0, size.width, size.height));
    app.fit();
    loop {
        let size = terminal.size()?;
        app.layout = ui::layout(ratatui::layout::Rect::new(0, 0, size.width, size.height));
        app.clamp_scroll();
        app.view.clamp_to_view(app.layout.gantt_width as usize);
        terminal.draw(|f| ui::draw(f, app))?;
        if event::poll(Duration::from_secs(1))?
            && let Event::Key(key) = event::read()?
            && key.kind == KeyEventKind::Press
        {
            if app.handle_key(key) {
                break;
            }
            if let Some(inv) = app.launch.take() {
                app.flash = antty::tui::run(terminal, &inv)?;
            }
            if std::mem::take(&mut app.suspend) {
                app.flash = antty::tui::suspend(terminal)?;
            }
        }
    }
    Ok(())
}
