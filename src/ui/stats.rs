use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::widgets::{Block, Borders, Clear};

use antty::model::Ts;
use antty::stats::{Stats, Subject, compute, subject_for};
use antty::tui::{StatsView, render_stats_column, render_stats_panel};

use super::AppRef;

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

pub(super) fn render_panel(f: &mut Frame, area: Rect, comparison: &Comparison) {
    let view = StatsView {
        label: &comparison.label,
        selected: comparison.selected(),
        all: &comparison.all,
        range: comparison.range,
        share: false,
    };
    render_stats_panel(f, area, &view);
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
    render_stats_column(f, left, &comparison.label, comparison.selected());
    render_stats_column(f, right, "all", &comparison.all);
}
