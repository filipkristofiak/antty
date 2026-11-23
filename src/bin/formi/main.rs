//! formi: overview of every project with coding-agent sessions.

mod activity;
mod app;
mod cache;
mod discover;
mod ui;

use std::path::PathBuf;
use std::sync::mpsc;

use antty::dirs::{default_claude_dir, default_codex_dir, default_omp_dir, default_state_dir, expand_tilde};
use antty::sessions::Harness;
use clap::Parser;
use discover::{Kind, Project, SessionInfo, collect, group};

#[derive(Parser, Debug)]
#[command(name = "formi", about = "Overview of coding-agent sessions across all projects")]
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
    /// Disable mouse capture so the terminal handles wheel input and text selection.
    #[arg(long)]
    no_mouse: bool,
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
        println!(
            "{}  [{}]  omp={} claude={} codex={}",
            project.key.path.display(),
            kind,
            totals[0],
            totals[1],
            totals[2]
        );
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
    let no_mouse = raw.no_mouse;
    let roots = harness_roots(raw);
    let (by_cwd, sources) = collect(&roots);
    let projects = group(by_cwd);
    if list {
        print(&projects);
        return Ok(());
    }
    let home = std::env::var_os("HOME");
    let cache_path =
        default_state_dir(std::env::var_os("XDG_STATE_HOME"), home.clone()).ok().map(|d| d.join("formi/activity.bin"));
    let (tx, rx) = mpsc::sync_channel(256);
    let mut app =
        app::App::new(app::project_views(projects), home.map(PathBuf::from).unwrap_or_default(), roots.clone());
    app.no_mouse = no_mouse;
    activity::spawn(roots, sources, cache_path, tx);
    let mut terminal = ratatui::init();
    let mut mouse = antty::tui::MouseCapture::default();
    let result = mouse
        .set(!no_mouse)
        .map_err(anyhow::Error::from)
        .and_then(|()| run_loop(&mut terminal, &mut app, rx, &mut mouse));
    drop(mouse);
    ratatui::restore();
    result
}

fn run_loop(
    terminal: &mut ratatui::DefaultTerminal,
    app: &mut app::App,
    rx: mpsc::Receiver<activity::Msg>,
    mouse: &mut antty::tui::MouseCapture,
) -> anyhow::Result<()> {
    use crossterm::event::{self, Event, KeyEventKind};
    use std::time::{Duration, Instant};

    use activity::Msg;

    const INPUT_BUDGET: Duration = Duration::from_millis(8);

    let size = terminal.size()?;
    app.layout = ui::layout(ratatui::layout::Rect::new(0, 0, size.width, size.height));
    app.fit();
    let mut dirty = true;
    let mut last_draw = Instant::now();
    loop {
        let started = Instant::now();
        let mut updates = Vec::new();
        while started.elapsed() < Duration::from_millis(30) {
            let Ok(msg) = rx.try_recv() else { break };
            dirty = true;
            match msg {
                Msg::Session { info, fresh_title } => updates.push((info, fresh_title)),
                Msg::Progress { done, total } => app.index_status = Some(format!("indexing {done}/{total}")),
                Msg::Indexed { sessions, scanned, elapsed } => {
                    app.index_status =
                        Some(format!("indexed {sessions} · {scanned} scanned · {:.2}s", elapsed.as_secs_f64()));
                }
                Msg::Notice(s) => app.flash = Some(s),
            }
        }
        let backlog = started.elapsed() >= Duration::from_millis(30);
        if !updates.is_empty() {
            app.apply(updates);
        }
        let size = terminal.size()?;
        app.layout = ui::layout(ratatui::layout::Rect::new(0, 0, size.width, size.height));
        app.clamp_scroll();
        app.view.clamp_to_view(app.layout.gantt_width as usize);
        if dirty || last_draw.elapsed() >= Duration::from_secs(1) {
            terminal.draw(|f| ui::draw(f, app))?;
            last_draw = Instant::now();
            dirty = false;
        }
        if event::poll(Duration::from_millis(if backlog { 0 } else { 200 }))? {
            let started = Instant::now();
            // Same-direction wheel reports for one pane, applied once before the next other event.
            let mut wheel = None;
            loop {
                match event::read()? {
                    Event::Key(key) if key.kind == KeyEventKind::Press => {
                        app.apply_wheel(wheel.take());
                        dirty = true;
                        if app.handle_key(key) {
                            return Ok(());
                        }
                        if let Some(inv) = app.launch.take() {
                            app.flash = if inv.suspend {
                                mouse.hand_off(|| antty::tui::run(terminal, &inv))?
                            } else {
                                antty::tui::run(terminal, &inv)?
                            };
                        }
                        if std::mem::take(&mut app.suspend) {
                            app.flash = mouse.hand_off(|| antty::tui::suspend(terminal))?;
                        }
                    }
                    Event::Mouse(event) if mouse.enabled() => {
                        if let Some((pane, delta)) = app.wheel_action(event) {
                            // Do not cancel opposite directions at a scroll boundary.
                            if let Some((target, amount)) = &mut wheel
                                && *target == pane
                                && amount.signum() == delta.signum()
                            {
                                *amount += delta;
                            } else {
                                dirty |= app.apply_wheel(wheel.replace((pane, delta)));
                            }
                        }
                    }
                    Event::Resize(width, height) => {
                        app.apply_wheel(wheel.take());
                        app.layout = ui::layout(ratatui::layout::Rect::new(0, 0, width, height));
                        app.clamp_scroll();
                        dirty = true;
                    }
                    _ => {}
                }
                if started.elapsed() >= INPUT_BUDGET || !event::poll(Duration::ZERO)? {
                    break;
                }
            }
            dirty |= app.apply_wheel(wheel);
        }
    }
}
