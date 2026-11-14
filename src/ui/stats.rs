use chrono::Duration;
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};

use antty::model::Ts;
use antty::stats::{Stats, Subject, compute, subject_for};

use super::AppRef;
use antty::tui::{fmt_span, truncate};

/// One comparison per frame, shared when the panel is visible behind the popup.
pub(super) struct Comparison {
    label: String,
    selection: Option<Stats>,
    all: Stats,
    range: bool,
}

impl Comparison {
    pub(super) fn new(app: &AppRef, gantt_width: u16) -> Self {
        let window = window(app, gantt_width);
        let (label, subject) = subject_for(app.model, app.tree, app.rows.get(app.ui.selected).copied());
        let selection = (!matches!(subject, Subject::All)).then(|| compute(app.model, &subject, window));
        Self { label, selection, all: compute(app.model, &Subject::All, window), range: window.is_some() }
    }

    fn selected(&self) -> &Stats {
        self.selection.as_ref().unwrap_or(&self.all)
    }
}

fn window(app: &AppRef, gantt_width: u16) -> Option<(Ts, Ts)> {
    app.ui
        .stats_window
        .then(|| (app.view.column(0).range().0, app.view.column(i64::from(gantt_width.max(1) - 1)).range().1))
}

fn compact(n: usize) -> String {
    match n {
        0..10_000 => n.to_string(),
        10_000..1_000_000 => format!("{:.1}k", n as f64 / 1_000.0),
        _ => format!("{:.1}M", n as f64 / 1_000_000.0),
    }
}

fn kpis(s: &Stats) -> [(&'static str, String); 5] {
    [
        ("calls", compact(s.calls)),
        ("read", compact(s.files_read)),
        ("written", compact(s.files_written)),
        ("agent", s.agent_secs.map_or_else(|| "–".into(), |secs| fmt_span(Duration::seconds(secs)))),
        ("prompts", s.prompts.map_or_else(|| "–".into(), compact)),
    ]
}

fn bar_lines(tools: &[(String, usize)], width: u16, max_rows: usize) -> Vec<Line<'static>> {
    if max_rows == 0 {
        return Vec::new();
    }
    let Some((_, max_count)) = tools.first() else {
        return vec![Line::from("no tool calls").fg(Color::DarkGray)];
    };
    let label_w = tools.iter().map(|(tool, _)| tool.chars().count()).max().unwrap_or(0).min(10);
    let count_w = max_count.ilog10() as usize + 1;
    let bar_w = usize::from(width).saturating_sub(label_w + count_w + 2);
    let shown = if tools.len() > max_rows { max_rows - 1 } else { tools.len() };
    let mut lines = Vec::with_capacity(tools.len().min(max_rows));
    for (tool, count) in &tools[..shown] {
        let label = truncate(tool, label_w);
        let bar_len = (*count as u128 * bar_w as u128).div_ceil(*max_count as u128) as usize;
        lines.push(Line::from(vec![
            Span::raw(format!("{label:<label_w$} ")),
            Span::raw("█".repeat(bar_len.max(1))).fg(Color::Cyan),
            Span::raw(format!(" {count:>count_w$}")),
        ]));
    }
    if shown < tools.len() {
        lines.push(Line::from(format!("+{} more", tools.len() - shown)).fg(Color::DarkGray));
    }
    lines
}

fn append_chart(lines: &mut Vec<Line<'static>>, area: Rect, stats: &Stats) {
    lines.push(Line::from("─".repeat(area.width.into())));
    let remaining = usize::from(area.height).saturating_sub(lines.len());
    lines.extend(bar_lines(&stats.tools, area.width, remaining));
}

pub(super) fn render_panel(f: &mut Frame, area: Rect, comparison: &Comparison) {
    let title = if comparison.range { "Stats · range" } else { "Stats" };
    let block = Block::default().borders(Borders::ALL).title(title);
    let inner = block.inner(area);
    f.render_widget(block, area);
    let selected = comparison.selected();
    let mut lines = vec![
        Line::from(truncate(&comparison.label, inner.width.into())).bold(),
        Line::from(format!("{:<8}{:>11}{:>11}", "", "sel", "all")),
    ];
    for ((name, sel), (_, all)) in kpis(selected).into_iter().zip(kpis(&comparison.all)) {
        lines.push(Line::from(format!("{name:<8}{sel:>11}{all:>11}")));
    }
    append_chart(&mut lines, inner, selected);
    f.render_widget(Paragraph::new(lines), inner);
}

fn render_column(f: &mut Frame, area: Rect, label: &str, stats: &Stats) {
    let mut lines = vec![Line::from(truncate(label, area.width.into())).bold()];
    for (name, value) in kpis(stats) {
        lines.push(Line::from(format!("{name:<8}{value:>11}")));
    }
    append_chart(&mut lines, area, stats);
    f.render_widget(Paragraph::new(lines), area);
}

pub(super) fn render_popup(f: &mut Frame, area: Rect, comparison: &Comparison) {
    let title = if comparison.range {
        "Stats  (v time range · j/k move · q/Esc close) · range"
    } else {
        "Stats  (v time range · j/k move · q/Esc close)"
    };
    f.render_widget(Clear, area);
    let block = Block::default().borders(Borders::ALL).title(title);
    let inner = block.inner(area);
    f.render_widget(block, area);
    let [left, _, right] =
        Layout::horizontal([Constraint::Fill(1), Constraint::Length(2), Constraint::Fill(1)]).areas(inner);
    render_column(f, left, &comparison.label, comparison.selected());
    render_column(f, right, "all", &comparison.all);
}
