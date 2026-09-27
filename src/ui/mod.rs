pub mod detail;
pub mod diff_view;
pub mod gantt_pane;
pub mod help;
pub mod picker;
pub mod status;
pub mod tree_pane;

use std::collections::HashSet;

use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::Color;
use ratatui::widgets::Clear;

use crate::model::{Model, ParticipantId, ParticipantKind};
use crate::timeline::View;
use crate::tree::{ExpandState, Row, Tree};

const PALETTE: [Color; 8] = [
    Color::Cyan,
    Color::White,
    Color::Magenta,
    Color::Yellow,
    Color::Green,
    Color::Blue,
    Color::LightRed,
    Color::LightMagenta,
];

pub fn palette_color(idx: usize) -> Color {
    PALETTE[idx % PALETTE.len()]
}

pub fn color_for(model: &Model, who: ParticipantId) -> Color {
    let p = &model.participants[who.0];
    if p.kind == ParticipantKind::Advisor {
        return Color::DarkGray;
    }
    palette_color(p.color_idx)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Normal,
    Detail,
    Diff,
    Picker,
    Help,
}

/// One entry in the Detail pane's event/span list, resolved from the selected row.
#[derive(Clone)]
pub enum DetailItem {
    Event(usize),
    Span { who: ParticipantId, start: crate::model::Ts, end: crate::model::Ts },
    Prompt { at: crate::model::Ts },
}

pub struct UiState {
    pub mode: Mode,
    pub selected: usize,
    pub scroll: usize,
    pub expand: ExpandState,
    pub watch_on: bool,
    pub status_extra: Option<String>,
    /// Some while the `:` command line is open (Normal mode only); the text typed after `:`.
    pub command: Option<String>,
    /// One-shot status message (e.g. unknown command); cleared on the next key press.
    pub flash: Option<String>,
    /// `z` pressed in Normal mode; the next key completes a fold command (`za`/`zM`/`zR`).
    pub pending_z: bool,

    // Detail mode
    pub detail_selected: usize,
    pub detail_scroll: u16,

    // Diff mode (full-screen)
    pub diff_scroll: u16,

    // Picker mode: working selection, applied to model.session_filter on Enter.
    pub picker_selected: HashSet<usize>,
    pub picker_cursor: usize,
}

impl UiState {
    pub fn new(watch_on: bool) -> Self {
        UiState {
            mode: Mode::Normal,
            selected: 0,
            scroll: 0,
            expand: ExpandState::default(),
            watch_on,
            status_extra: None,
            command: None,
            flash: None,
            pending_z: false,
            detail_selected: 0,
            detail_scroll: 0,
            diff_scroll: 0,
            picker_selected: HashSet::new(),
            picker_cursor: 0,
        }
    }
}

/// Borrowed bundle of everything a frame needs to render; rebuilt fresh each draw.
pub struct AppRef<'a> {
    pub model: &'a Model,
    pub tree: &'a Tree,
    pub rows: &'a [Row],
    pub view: &'a View,
    pub ui: &'a UiState,
}

pub fn truncate(s: &str, width: usize) -> String {
    if s.chars().count() <= width {
        s.to_string()
    } else {
        s.chars().take(width.saturating_sub(1)).collect::<String>() + "…"
    }
}

fn centered_rect(percent_x: u16, percent_y: u16, area: Rect) -> Rect {
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(area);
    let horizontal = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(vertical[1]);
    horizontal[1]
}

pub struct LayoutInfo {
    pub tree_area: Rect,
    pub gantt_area: Rect,
    pub detail_area: Option<Rect>,
    pub status_area: Rect,
    /// visible row-list height shared by the tree and gantt panes.
    pub body_height: u16,
    /// number of timeline columns in the gantt pane.
    pub gantt_width: u16,
    /// centered session-picker overlay; its inner height sizes picker half-page moves.
    pub picker_area: Rect,
}

/// Shared layout math so main.rs can size scroll/pan behavior identically to what `draw` renders.
pub fn compute_layout(root: Rect, mode: Mode) -> LayoutInfo {
    let outer = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(0), Constraint::Length(1)])
        .split(root);

    let tree_width = (root.width / 3).clamp(20, 40);
    let (tree_area, gantt_area, detail_area) = if mode == Mode::Detail {
        let detail_height = (outer[0].height * 2 / 5).max(6);
        let split = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(0), Constraint::Length(detail_height)])
            .split(outer[0]);
        let top = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Length(tree_width), Constraint::Min(10)])
            .split(split[0]);
        (top[0], top[1], Some(split[1]))
    } else {
        let main = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Length(tree_width), Constraint::Min(10)])
            .split(outer[0]);
        (main[0], main[1], None)
    };

    // 2 border rows + 2 header rows in the gantt pane; tree pane matches with its own spacer.
    let body_height = gantt_area.height.saturating_sub(4);
    let gantt_width = gantt_area.width.saturating_sub(2);
    LayoutInfo { tree_area, gantt_area, detail_area, status_area: outer[1], body_height, gantt_width, picker_area: centered_rect(60, 60, root) }
}

/// Top-level draw: main split (tree | gantt) + status bar, with mode-specific overlays.
pub fn draw(f: &mut Frame, app: &AppRef) {
    let root = f.area();
    let layout = compute_layout(root, app.ui.mode);

    if let Some(area) = layout.detail_area {
        detail::render(f, area, app);
    }
    tree_pane::render(f, layout.tree_area, app);
    gantt_pane::render(f, layout.gantt_area, app);
    status::render(f, layout.status_area, app);

    match app.ui.mode {
        Mode::Picker => picker::render(f, layout.picker_area, app),
        Mode::Help => help::render(f, centered_rect(50, 60, root)),
        Mode::Diff => {
            let area = Rect { height: root.height.saturating_sub(1), ..root };
            f.render_widget(Clear, area);
            diff_view::render(f, area, app);
        }
        _ => {}
    }
}
