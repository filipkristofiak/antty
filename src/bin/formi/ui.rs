use antty::sessions::Harness;
use antty::timeline::Column;
use antty::tui::{
    CURSOR_BG, HIGHLIGHT_FG, SELECTED_BG, STATS_MIN_WIDTH, STATS_PANEL_WIDTH, StatsView, bar_lines, compact, fmt_span,
    fold_glyph, render_stats_panel, truncate,
};
use ratatui::Frame;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Line;
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph};

use crate::app::{ActionRow, App, Order, Row, Summary, overlaps};
use crate::discover::Kind;

use ratatui::layout::{Constraint, Direction, Layout as Split, Rect};

pub const HELP: antty::tui::HelpSections = &[
    (
        "Rows",
        &[
            ("[count]", "repeat: 5j 10l 3n …"),
            ("j/k, ↓/↑", "row down/up"),
            ("gg/G", "first/last row; [count]: row N"),
            ("Ctrl-d/Ctrl-u", "half page down/up; [count]: N rows"),
            ("Wheel", "scroll the rows or help under the pointer by 3 rows; keep selection"),
        ],
    ),
    (
        "Timeline",
        &[
            ("h/l, ←/→", "time cursor ±1 column"),
            ("H/L", "pan by half the pane width"),
            ("+/-", "zoom in/out"),
            ("t", "cursor to now, pinned near the right edge"),
            ("f", "fit all activity"),
            ("c", "collapse idle stretches into ' ~ ' breaks on/off"),
        ],
    ),
    (
        "Projects",
        &[
            ("Enter", "open the project or session in antty; on a fold row: show what it hides"),
            ("Space/za", "open/close a project or directory"),
            ("zo/zc", "open/close"),
            ("zM/zR", "close/open all"),
            ("o", "order: recent activity / path tree"),
            ("p", "fold older sessions and projects on/off"),
        ],
    ),
    ("Search", &[("/", "search project paths and session titles (smartcase)"), ("n/N", "next/prev match")]),
    (
        "Stats",
        &[
            ("i", "stats panel on/off: selected session, project or directory vs all indexed sessions"),
            ("I", "dashboard: selection vs all, projects ranked by calls, last 7 days"),
            ("In I", "v visible timeline range only · j/k move the selection · Enter open · q/Esc close"),
            ("F", "action filter: list only sessions that used the chosen actions; bars show those actions"),
            ("In F", "Space toggle · a all · n none · Enter apply · q/Esc cancel; scripts only narrows file actions"),
        ],
    ),
    (
        "General",
        &[
            ("?", "this help. In help: j/k, Ctrl-d/Ctrl-u, g/G scroll"),
            (":q⏎, ZZ/ZQ", "quit"),
            ("Ctrl-C", "hint: quit with :q⏎"),
            ("Ctrl-z", "suspend to the shell; fg resumes"),
            (":/ line", "Ctrl-U clear · Ctrl-W delete word · ↑/↓ history"),
            ("q/Esc", "close help; Esc also clears the search"),
        ],
    ),
];

pub struct Layout {
    pub tree: Rect,
    pub gantt: Rect,
    /// The stats panel, when it is on and the terminal is wide enough.
    pub stats: Option<Rect>,
    pub status: Rect,
    pub help: Rect,
    pub body_height: u16,
    pub gantt_width: u16,
}

pub fn layout(root: Rect, stats_panel: bool) -> Layout {
    let outer = Split::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(0), Constraint::Length(1)])
        .split(root);
    let tw = (root.width / 3).clamp(20, 40);
    let columns = [Constraint::Length(tw), Constraint::Min(10), Constraint::Length(STATS_PANEL_WIDTH)];
    let panel = stats_panel && root.width >= STATS_MIN_WIDTH;
    let panes = Split::horizontal(&columns[..if panel { 3 } else { 2 }]).split(outer[0]);
    Layout {
        tree: panes[0],
        gantt: panes[1],
        stats: panes.get(2).copied(),
        status: outer[1],
        help: antty::tui::centered_rect(60, 80, root),
        body_height: panes[1].height.saturating_sub(4),
        gantt_width: panes[1].width.saturating_sub(2),
    }
}

pub fn draw(f: &mut Frame, app: &App) {
    tree(f, app);
    gantt(f, app);
    status(f, app);
    if let Some(area) = app.layout.stats {
        let (label, selection, all) = app.stats();
        let view = StatsView {
            label: &label,
            selected: selection.as_ref().unwrap_or(&all),
            all: &all,
            range: app.stats_window,
            // Against itself the share is always 100%; only a real selection gets the row.
            share: selection.is_some(),
        };
        render_stats_panel(f, area, &view);
    }
    if app.dashboard {
        dashboard(f, app);
    }
    if app.actions.is_some() {
        actions(f, app);
    }
    if let Some(scroll) = app.help {
        antty::tui::render_help(f, app.layout.help, scroll, HELP);
    }
    if app.no_color {
        antty::tui::strip_colors(f.buffer_mut());
    }
}

fn harness_color(h: Harness) -> Color {
    match h {
        Harness::Omp => Color::Cyan,
        Harness::Claude => Color::Magenta,
        Harness::Codex => Color::Green,
    }
}

/// A dashboard KPI row: its name and how to format it for one side.
type KpiRow = (&'static str, fn(&Summary) -> String);

/// The last two components of a project path: `tools/antty`.
fn short_label(path: &std::path::Path) -> String {
    match (path.parent().and_then(|p| p.file_name()), path.file_name()) {
        (Some(parent), Some(name)) => format!("{}/{}", parent.to_string_lossy(), name.to_string_lossy()),
        _ => path.display().to_string(),
    }
}

fn actions(f: &mut Frame, app: &App) {
    let Some(overlay) = &app.actions else { return };
    let area = antty::tui::centered_rect(60, 60, f.area());
    f.render_widget(Clear, area);
    let block = Block::default()
        .borders(Borders::ALL)
        .title("Actions  (Space toggle · a all · n none · Enter apply · q/Esc cancel)");
    let inner = block.inner(area);
    f.render_widget(block, area);
    let items: Vec<_> = overlay
        .rows
        .iter()
        .enumerate()
        .map(|(i, row)| match row {
            ActionRow::Action { action, sessions, events } => {
                let checked = if overlay.checked.contains(&i) { 'x' } else { ' ' };
                let label = antty::filter::label(antty::filter::ActionKey::Fixed(*action));
                ListItem::new(format!("[{checked}] {label:<9} {sessions:>5} sessions · {events} events"))
            }
            ActionRow::ScriptsOnly => {
                let checked = if overlay.scripts { 'x' } else { ' ' };
                ListItem::new(format!("[{checked}] scripts only  ({})", antty::filter::SCRIPT_EXTS.join(", ")))
            }
        })
        .collect();
    let mut state = ListState::default().with_selected(Some(overlay.cursor));
    let list = List::new(items)
        .highlight_style(Style::default().bg(SELECTED_BG).fg(HIGHLIGHT_FG).add_modifier(Modifier::BOLD));
    f.render_stateful_widget(list, inner, &mut state);
}

fn dashboard(f: &mut Frame, app: &App) {
    use ratatui::style::Stylize;
    let d = app.dashboard();
    let area = antty::tui::centered_rect(80, 80, f.area());
    f.render_widget(Clear, area);
    let range = if app.stats_window { " · range" } else { "" };
    let title = format!("Dashboard  (v range · j/k move selection · Enter open · q/Esc close){range}");
    let block = Block::default().borders(Borders::ALL).title(title);
    let inner = block.inner(area);
    f.render_widget(block, area);
    let [left, _, right] =
        Split::horizontal([Constraint::Fill(1), Constraint::Length(2), Constraint::Fill(1)]).areas(inner);

    let sel = d.selection.as_ref().unwrap_or(&d.all);
    let span = if app.stats_window { "visible range" } else { "all time" };
    let mut lines = vec![
        Line::from(truncate(&format!("{} · {span}", d.label), left.width.into())).bold(),
        Line::from(""),
        Line::from(format!("{:<10}{:>9}{:>9}", "", "sel", "all")),
    ];
    let head: [KpiRow; 2] = [("projects", |s| compact(s.projects)), ("sessions", |s| compact(s.sessions.iter().sum()))];
    for (name, value) in head {
        lines.push(Line::from(format!("{name:<10}{:>9}{:>9}", value(sel), value(&d.all))));
    }
    for (i, harness) in Harness::ALL.iter().enumerate() {
        lines.push(Line::from(format!(
            "  {:<8}{:>9}{:>9}",
            harness.label(),
            compact(sel.sessions[i]),
            compact(d.all.sessions[i])
        )));
    }
    let tail: [KpiRow; 5] = [
        ("calls", |s| compact(s.stats.calls)),
        ("read", |s| compact(s.stats.files_read)),
        ("written", |s| compact(s.stats.files_written)),
        ("agent", |s| s.stats.agent_secs.map_or_else(|| "–".into(), |x| fmt_span(chrono::Duration::seconds(x)))),
        ("prompts", |s| s.stats.prompts.map_or_else(|| "–".into(), compact)),
    ];
    for (name, value) in tail {
        lines.push(Line::from(format!("{name:<10}{:>9}{:>9}", value(sel), value(&d.all))));
    }
    if d.selection.is_some() {
        let share = match d.all.stats.calls {
            0 => "–".into(),
            all => format!("{}%", (sel.stats.calls * 100).div_ceil(all).min(100)),
        };
        lines.push(Line::from(format!("{:<10}{share:>9}", "share")));
    }
    lines.push(Line::from("─".repeat(left.width.into())));
    lines.push(Line::from("tool mix").bold());
    let remaining = usize::from(left.height).saturating_sub(lines.len());
    lines.extend(bar_lines(&sel.stats.tools, left.width, remaining, 12));
    f.render_widget(Paragraph::new(lines), left);

    let label = |&(p, calls): &(usize, usize)| {
        let mark = if d.current == Some(p) { "▶" } else { " " };
        (format!("{mark}{}", short_label(&app.projects[p].key.path)), calls)
    };
    let ranked: Vec<_> = d.ranked.iter().map(label).collect();
    let week: Vec<_> = d.week.iter().map(label).collect();
    // Header and blank line above each block, plus a blank line between the blocks.
    let ranked_rows = usize::from(right.height).saturating_sub(5 + week.len().clamp(1, 8));
    let mut lines = vec![Line::from("projects by calls").bold(), Line::from("")];
    if ranked.len() > ranked_rows && ranked_rows > 0 {
        let shown = ranked_rows - 1;
        lines.extend(bar_lines(&ranked[..shown], right.width, shown, 22));
        let rest: usize = ranked[shown..].iter().map(|&(_, calls)| calls).sum();
        lines.push(Line::from(format!("⋯ {} more · Σ {}", ranked.len() - shown, compact(rest))).fg(Color::DarkGray));
    } else {
        lines.extend(bar_lines(&ranked, right.width, ranked_rows, 22));
    }
    lines.extend([Line::from(""), Line::from("last 7 days").bold(), Line::from("")]);
    let remaining = usize::from(right.height).saturating_sub(lines.len());
    lines.extend(bar_lines(&week, right.width, remaining, 22));
    f.render_widget(Paragraph::new(lines), right);
}

fn tree(f: &mut Frame, app: &App) {
    let block = Block::default().borders(Borders::ALL).title("projects");
    let inner = block.inner(app.layout.tree);
    f.render_widget(block, app.layout.tree);
    if inner.height < 3 || inner.width == 0 {
        return;
    }
    let width = inner.width as usize;
    let buf = f.buffer_mut();
    for (r, row) in app.rows.iter().skip(app.scroll).take(app.layout.body_height as usize).enumerate() {
        let (text, mut style) = match *row {
            Row::Node { node, depth } => {
                let n = &app.forest.nodes[node];
                let marker = if app.expanded.contains(&n.key) { "▾ " } else { "▸ " };
                let text = format!("{}{marker}{} ({})", "  ".repeat(depth), n.label, n.sessions);
                let missing = n.project.is_some_and(|p| app.projects[p].kind == Kind::Missing);
                (text, if missing { Style::default().fg(Color::DarkGray) } else { Style::default() })
            }
            Row::Session { project, session, depth } => {
                let s = &app.projects[project].sessions[session];
                let mut text = format!("{}{}", "  ".repeat(depth), s.title);
                if let Some((start, _)) = s.span {
                    text.push_str(&format!(" · {}", start.with_timezone(&chrono::Local).format("%m-%d %H:%M")));
                }
                (text, Style::default().fg(harness_color(s.harness)))
            }
            Row::SessionFold { project, depth } => {
                let folded = app.projects[project].sessions.len() - app.sessions_shown(project, app.live_since);
                (format!("{}⋯ {folded} older", "  ".repeat(depth)), Style::default().fg(Color::DarkGray))
            }
            Row::ProjectFold => {
                let folded = app.forest.roots.len() - app.roots_shown(app.live_since);
                (format!("⋯ {folded} older projects"), Style::default().fg(Color::DarkGray))
            }
        };
        let y = inner.y + 2 + r as u16;
        if app.scroll + r == app.selected {
            if matches!(style.fg, None | Some(Color::DarkGray)) {
                style = style.fg(HIGHLIGHT_FG);
            }
            style = style.bg(SELECTED_BG).add_modifier(Modifier::BOLD);
            buf.set_string(inner.x, y, " ".repeat(width), style);
        }
        if let Some(query) = &app.search
            && app.search_label(*row).is_some_and(|label| antty::search::is_match(label, query))
        {
            style = style.add_modifier(Modifier::UNDERLINED);
        }
        buf.set_string(inner.x, y, truncate(&text, width), style);
    }
}

fn gantt(f: &mut Frame, app: &App) {
    let block = Block::default().borders(Borders::ALL).title("timeline");
    let inner = block.inner(app.layout.gantt);
    f.render_widget(block, app.layout.gantt);
    if inner.height < 3 || inner.width == 0 {
        return;
    }
    antty::tui::render_header(f, Rect { height: 2, ..inner }, &app.view);
    let cols: Vec<_> = (0..inner.width).map(|c| app.view.column(c as i64)).collect();
    let cursor_col = app.view.col_for(app.view.cursor);
    let now_col = app.view.col_for(chrono::Utc::now());
    let buf = f.buffer_mut();
    for (r, row) in app.rows.iter().skip(app.scroll).take(app.layout.body_height as usize).enumerate() {
        let selected = app.scroll + r == app.selected;
        let y = inner.y + 2 + r as u16;
        let folded = if let Row::SessionFold { project, .. } = *row {
            &app.projects[project].sessions[app.sessions_shown(project, app.live_since)..]
        } else {
            &[]
        };
        let folded_roots =
            if *row == Row::ProjectFold { &app.forest.roots[app.roots_shown(app.live_since)..] } else { &[] };
        for (col, &column) in cols.iter().enumerate() {
            let (ch, color) = match column {
                Column::Break { .. } => ('~', Some(Color::DarkGray)),
                Column::GapPad { .. } => (' ', None),
                Column::Time { start, end } => match *row {
                    Row::Node { node, .. } if overlaps(&app.forest.nodes[node].spans, start, end) => {
                        ('▓', Some(Color::DarkGray))
                    }
                    Row::Session { project, session, .. } => {
                        let s = &app.projects[project].sessions[session];
                        if overlaps(s.bars(), start, end) {
                            (if s.activity.is_some() { '█' } else { '▒' }, Some(harness_color(s.harness)))
                        } else {
                            (' ', None)
                        }
                    }
                    Row::SessionFold { .. } => {
                        let mut hits = folded.iter().filter(|s| overlaps(s.bars(), start, end)).take(3);
                        let first = hits.next();
                        fold_glyph(usize::from(first.is_some()) + hits.count(), first.map(|s| harness_color(s.harness)))
                    }
                    Row::ProjectFold => {
                        let count = folded_roots
                            .iter()
                            .filter(|&&root| overlaps(&app.forest.nodes[root].spans, start, end))
                            .take(3)
                            .count();
                        fold_glyph(count, Some(Color::DarkGray))
                    }
                    _ => (' ', None),
                },
            };
            let col = col as i64;
            let mut style = Style::default();
            if let Some(color) = color {
                style = style.fg(color);
            }
            if selected {
                style = style.bg(SELECTED_BG);
            } else if col == cursor_col {
                style = style.bg(CURSOR_BG);
            }
            let out = if matches!(column, Column::Time { .. })
                && ch == ' '
                && col == now_col
                && !selected
                && col != cursor_col
            {
                style = style.fg(Color::LightBlue);
                '│'
            } else {
                ch
            };
            buf.set_string(inner.x + col as u16, y, out.to_string(), style);
        }
    }
}

fn status(f: &mut Frame, app: &App) {
    let area = app.layout.status;
    let width = area.width as usize;
    if width == 0 || area.height == 0 {
        return;
    }
    if let Some(line) = &app.cmdline {
        f.render_widget(Paragraph::new(truncate(&format!("{}{}", line.kind.prompt(), line.text), width)), area);
        f.set_cursor_position((area.x + (1 + line.text.chars().count()).min(width - 1) as u16, area.y));
        return;
    }
    if let Some(flash) = &app.flash {
        f.render_widget(Paragraph::new(truncate(flash, width)).style(Style::default().fg(Color::Red)), area);
        return;
    }
    let left = "Nav (j/k/h/l) | Zoom (+/-) | Now (t) | Fit (f) | Open (⏎) | Fold (␣) | Order (o) | Stats (i/I) | Actions (F) | Help (?) | Quit (:q)";
    // Filter and stats state stay readable even when the rest of the status line is clipped.
    let mut flags = String::new();
    if app.stats_panel && app.layout.stats.is_none() {
        flags.push_str(" · stats:narrow");
    }
    if app.stats_window {
        flags.push_str(" · stats:range");
    }
    if let Some(filter) = &app.action_filter {
        let mut actions: Vec<_> = filter.actions.iter().copied().collect();
        actions.sort_unstable();
        let mut names: Vec<_> =
            actions.into_iter().map(|a| antty::filter::label(antty::filter::ActionKey::Fixed(a))).collect();
        if filter.scripts_only {
            names.push("scripts".into());
        }
        flags.push_str(&format!(" · actions:{}", if names.is_empty() { "none".into() } else { names.join(",") }));
    }
    let right = format!(
        "{} · {} projects · {} sessions · order:{} · gaps:{} · fold:{}",
        app.view.zoom_label(),
        app.projects.len(),
        app.projects.iter().map(|p| p.sessions.len()).sum::<usize>(),
        if app.order == Order::Recent { "recent" } else { "tree" },
        if app.view.collapse_gaps() { "on" } else { "off" },
        if app.fold { "on" } else { "off" },
    );
    let right = if let Some(s) = &app.index_status { format!("{s} · {right}") } else { right };
    // Clip the key hints first; the state on the right goes only when nothing else is left.
    let width = width.saturating_sub(flags.chars().count());
    let left_len = left.chars().count();
    let right_len = right.chars().count();
    let text = if left_len + right_len + 2 <= width {
        format!("{left}{}{right}{flags}", " ".repeat(width - left_len - right_len))
    } else if right_len + 3 <= width {
        format!("{}  {right}{flags}", truncate(left, width - right_len - 2))
    } else {
        format!("{}{flags}", truncate(&right, width))
    };
    f.render_widget(Paragraph::new(text), area);
}
