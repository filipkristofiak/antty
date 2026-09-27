use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::Line;
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState};

use crate::model::{Model, ParticipantKind, Session, Ts};

use super::{AppRef, HIGHLIGHT_FG, SELECTED_BG};

const LIVE_THRESHOLD_SECS: i64 = 120;

fn local(t: Ts) -> chrono::DateTime<chrono::Local> {
    t.with_timezone(&chrono::Local)
}

/// Session indices ordered by start time ascending; the picker's cursor is a position in this
/// order, not a raw session index, so navigation matches what's on screen.
pub fn sorted_indices(model: &Model) -> Vec<usize> {
    let mut order: Vec<usize> = (0..model.sessions.len()).collect();
    order.sort_by_key(|&i| model.sessions[i].start);
    order
}

/// A session shows an explicit `end` if it has one. Otherwise, `end` being unset only means no
/// clean `session_exit` was recorded, not that the session is live "right now": show `live` only
/// while its most recent entry (from any participant) is within `LIVE_THRESHOLD_SECS`, else fall
/// back to that last-seen time as its effective end.
fn end_label(s: &Session, now: Ts) -> String {
    if let Some(e) = s.end {
        return local(e).format("%m-%d %H:%M").to_string();
    }
    if (now - s.last_seen).num_seconds() < LIVE_THRESHOLD_SECS {
        "live".to_string()
    } else {
        local(s.last_seen).format("%m-%d %H:%M").to_string()
    }
}

pub fn render(f: &mut Frame, area: Rect, app: &AppRef) {
    f.render_widget(Clear, area);
    let block = Block::default()
        .borders(Borders::ALL)
        .title("Sessions  (Space toggle · a all · Enter apply · q/Esc cancel)");
    let inner = block.inner(area);
    f.render_widget(block, area);

    let now = chrono::Utc::now();
    let items: Vec<ListItem> = sorted_indices(app.model)
        .into_iter()
        .map(|i| {
            let s = &app.model.sessions[i];
            let checked = if app.ui.picker_selected.contains(&i) { "x" } else { " " };
            let end = end_label(s, now);
            let n_sub = app
                .model
                .participants
                .iter()
                .filter(|p| p.session == Some(i) && p.kind == ParticipantKind::Subagent)
                .count();
            let text = format!("[{checked}] {}  {}–{}  {n_sub} subagents", s.title, local(s.start).format("%m-%d %H:%M"), end);
            ListItem::new(Line::from(text))
        })
        .collect();

    let mut state = ListState::default();
    state.select(Some(app.ui.picker_cursor));
    let list = List::new(items).highlight_style(Style::default().bg(SELECTED_BG).fg(HIGHLIGHT_FG));
    f.render_stateful_widget(list, inner, &mut state);
}
