use antty::sessions::Harness;
use antty::timeline::Column;
use antty::tui::{CURSOR_BG, HIGHLIGHT_FG, SELECTED_BG, fold_glyph, truncate};
use ratatui::Frame;
use ratatui::style::{Color, Modifier, Style};
use ratatui::widgets::{Block, Borders, Paragraph};

use crate::app::{App, Order, Row, live_since, overlaps};
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
    pub status: Rect,
    pub help: Rect,
    pub body_height: u16,
    pub gantt_width: u16,
}

pub fn layout(root: Rect) -> Layout {
    let outer = Split::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(0), Constraint::Length(1)])
        .split(root);
    let tw = (root.width / 3).clamp(20, 40);
    let panes = Split::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Length(tw), Constraint::Min(10)])
        .split(outer[0]);
    Layout {
        tree: panes[0],
        gantt: panes[1],
        status: outer[1],
        help: antty::tui::centered_rect(60, 80, root),
        body_height: panes[1].height.saturating_sub(4),
        gantt_width: panes[1].width.saturating_sub(2),
    }
}

pub fn draw(f: &mut Frame, app: &App) {
    let live_since = live_since();
    tree(f, app, live_since);
    gantt(f, app, live_since);
    status(f, app);
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

fn tree(f: &mut Frame, app: &App, live_since: antty::model::Ts) {
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
                let folded = app.projects[project].sessions.len() - app.sessions_shown(project, live_since);
                (format!("{}⋯ {folded} older", "  ".repeat(depth)), Style::default().fg(Color::DarkGray))
            }
            Row::ProjectFold => {
                let folded = app.forest.roots.len() - app.roots_shown(live_since);
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
            && app.row_label(*row).is_some_and(|label| antty::search::is_match(label, query))
        {
            style = style.add_modifier(Modifier::UNDERLINED);
        }
        buf.set_string(inner.x, y, truncate(&text, width), style);
    }
}

fn gantt(f: &mut Frame, app: &App, live_since: antty::model::Ts) {
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
            &app.projects[project].sessions[app.sessions_shown(project, live_since)..]
        } else {
            &[]
        };
        let folded_roots =
            if *row == Row::ProjectFold { &app.forest.roots[app.roots_shown(live_since)..] } else { &[] };
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
                        if s.span.is_some_and(|(a, b)| a < end && b >= start) {
                            ('▒', Some(harness_color(s.harness)))
                        } else {
                            (' ', None)
                        }
                    }
                    Row::SessionFold { .. } => {
                        let mut hits =
                            folded.iter().filter(|s| s.span.is_some_and(|(a, b)| a < end && b >= start)).take(3);
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
    let left =
        "Nav (j/k/h/l) | Zoom (+/-) | Now (t) | Fit (f) | Open (⏎) | Fold (␣) | Order (o) | Help (?) | Quit (:q)";
    let right = format!(
        "{} · {} projects · {} sessions · order:{} · gaps:{} · fold:{}",
        app.view.zoom_label(),
        app.projects.len(),
        app.projects.iter().map(|p| p.sessions.len()).sum::<usize>(),
        if app.order == Order::Recent { "recent" } else { "tree" },
        if app.view.collapse_gaps() { "on" } else { "off" },
        if app.fold { "on" } else { "off" }
    );
    let left_len = left.chars().count();
    let right_len = right.chars().count();
    let text = if left_len + right_len + 2 <= width {
        format!("{left}{}{right}", " ".repeat(width - left_len - right_len))
    } else {
        truncate(&format!("{left}  {right}"), width)
    };
    f.render_widget(Paragraph::new(text), area);
}
