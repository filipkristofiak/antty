use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span as TSpan};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph};

use crate::model::{EventDetail, FileEvent, ParticipantKind, Ts, TouchSource};
use crate::tree::Row;

use super::{AppRef, DetailItem};

fn event_kind_label(e: &FileEvent) -> &'static str {
    match &e.source {
        TouchSource::Tool(t) if t == "read" => "read",
        TouchSource::Tool(_) => "write",
        TouchSource::Bash => "bash",
        TouchSource::Watcher => "fs",
    }
}

/// The selected row's events/spans/prompts, newest first.
pub fn resolve_items(app: &AppRef) -> Vec<DetailItem> {
    if app.ui.selected >= app.rows.len() {
        return Vec::new();
    }
    let mut items: Vec<(Ts, DetailItem)> = match app.rows[app.ui.selected] {
        Row::Node(idx) => app.tree.nodes[idx]
            .events
            .iter()
            .filter(|&&i| app.model.visible(app.model.events[i].who))
            .map(|&i| (app.model.events[i].end, DetailItem::Event(i)))
            .collect(),
        Row::Participant(pid, _) => {
            let mut v: Vec<(Ts, DetailItem)> = app
                .model
                .spans
                .iter()
                .filter(|s| s.who == pid)
                .map(|s| (s.end, DetailItem::Span { who: pid, start: s.start, end: s.end }))
                .collect();
            let p = &app.model.participants[pid.0];
            if p.kind == ParticipantKind::Main
                && let Some(sidx) = p.session {
                    for pr in &app.model.prompts {
                        if pr.session == sidx {
                            v.push((pr.at, DetailItem::Prompt { at: pr.at }));
                        }
                    }
                }
            v
        }
        Row::Section(_) => Vec::new(),
    };
    items.sort_by_key(|a| std::cmp::Reverse(a.0));
    items.into_iter().map(|(_, it)| it).collect()
}

pub fn item_time(item: &DetailItem, app: &AppRef) -> Ts {
    match item {
        DetailItem::Event(idx) => app.model.events[*idx].end,
        DetailItem::Span { end, .. } => *end,
        DetailItem::Prompt { at, .. } => *at,
    }
}

/// Index of the item whose time is closest to `cursor`, for the initial Detail-mode selection.
pub fn closest_to(app: &AppRef, items: &[DetailItem], cursor: Ts) -> usize {
    items
        .iter()
        .enumerate()
        .min_by_key(|(_, it)| (item_time(it, app) - cursor).num_seconds().abs())
        .map(|(i, _)| i)
        .unwrap_or(0)
}

fn local(t: Ts) -> chrono::DateTime<chrono::Local> {
    t.with_timezone(&chrono::Local)
}

fn item_line(app: &AppRef, item: &DetailItem) -> String {
    match item {
        DetailItem::Event(idx) => {
            let e = &app.model.events[*idx];
            format!(
                "{}  {}  {}  {}",
                local(e.start).format("%H:%M:%S"),
                app.model.participants[e.who.0].label,
                event_kind_label(e),
                e.rel.display()
            )
        }
        DetailItem::Span { who, start, .. } => {
            format!("{}  {}  active", local(*start).format("%H:%M:%S"), app.model.participants[who.0].label)
        }
        DetailItem::Prompt { at, .. } => format!("{}  you  prompt", local(*at).format("%H:%M:%S")),
    }
}

fn detail_lines(app: &AppRef, item: &DetailItem) -> Vec<Line<'static>> {
    match item {
        DetailItem::Event(idx) => {
            let e = &app.model.events[*idx];
            let mut lines = vec![Line::from(format!(
                "{}  {}  {:?}  {}",
                local(e.start).format("%Y-%m-%d %H:%M:%S"),
                app.model.participants[e.who.0].label,
                e.kind,
                e.rel.display()
            ))];
            match &e.detail {
                EventDetail::Diff(d) => {
                    for line in d.lines() {
                        let style = if line.starts_with('+') {
                            Style::default().fg(Color::Green)
                        } else if line.starts_with('-') {
                            Style::default().fg(Color::Red)
                        } else {
                            Style::default()
                        };
                        lines.push(Line::from(vec![TSpan::styled(line.to_string(), style)]));
                    }
                }
                EventDetail::Written { bytes } => lines.push(Line::from(format!("wrote {bytes} bytes"))),
                EventDetail::Fs(change) => lines.push(Line::from(format!("fs {change:?}").to_lowercase())),
                EventDetail::Moved { to } => lines.push(Line::from(format!("moved to {}", to.display()))),
                EventDetail::Removed => lines.push(Line::from("removed".to_string())),
                EventDetail::None => lines.push(Line::from("(no additional detail)")),
            }
            lines
        }
        DetailItem::Span { who, start, end } => vec![Line::from(format!(
            "{} active {} – {}",
            app.model.participants[who.0].label,
            local(*start).format("%H:%M:%S"),
            local(*end).format("%H:%M:%S")
        ))],
        DetailItem::Prompt { at, .. } => {
            vec![Line::from(format!("prompt sent at {}", local(*at).format("%Y-%m-%d %H:%M:%S")))]
        }
    }
}

pub fn render(f: &mut Frame, area: Rect, app: &AppRef) {
    let block = Block::default().borders(Borders::ALL).title("Detail");
    let inner = block.inner(area);
    f.render_widget(block, area);
    if inner.width == 0 || inner.height == 0 {
        return;
    }
    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(45), Constraint::Percentage(55)])
        .split(inner);

    let items = resolve_items(app);
    if items.is_empty() {
        f.render_widget(Paragraph::new("(no events for this row)"), inner);
        return;
    }
    let selected = app.ui.detail_selected.min(items.len() - 1);
    let list_items: Vec<ListItem> = items.iter().map(|it| ListItem::new(item_line(app, it))).collect();
    let mut state = ListState::default();
    state.select(Some(selected));
    let list = List::new(list_items).highlight_style(Style::default().bg(Color::Rgb(50, 70, 130)).fg(Color::White));
    f.render_stateful_widget(list, cols[0], &mut state);

    let text = detail_lines(app, &items[selected]);
    let para = Paragraph::new(text).scroll((app.ui.detail_scroll, 0));
    f.render_widget(para, cols[1]);
}
