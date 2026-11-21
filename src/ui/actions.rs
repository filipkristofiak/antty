use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState};

use antty::filter::{ActionKey, SCRIPT_EXTS, label};

use super::{ActionRow, AppRef, HIGHLIGHT_FG, SELECTED_BG};

pub fn render(f: &mut Frame, area: Rect, app: &AppRef) {
    f.render_widget(Clear, area);
    let block = Block::default()
        .borders(Borders::ALL)
        .title("Actions  (Space toggle · a all · n none · Enter apply · q/Esc cancel)");
    let inner = block.inner(area);
    f.render_widget(block, area);

    let items: Vec<_> = app
        .ui
        .actions_rows
        .iter()
        .enumerate()
        .map(|(i, row)| {
            let checked = if app.ui.actions_checked.contains(&i) { 'x' } else { ' ' };
            let (key, count) = match row {
                ActionRow::Fixed(action, count) => (ActionKey::Fixed(*action), count),
                ActionRow::FsDuring(tool, count) => (ActionKey::FsDuring(tool), count),
                ActionRow::ScriptsOnly => {
                    let checked = if app.ui.actions_scripts { 'x' } else { ' ' };
                    return ListItem::new(format!("[{checked}] scripts only  ({})", SCRIPT_EXTS.join(", ")));
                }
            };
            ListItem::new(format!("[{checked}] {}  ({count})", label(key)))
        })
        .collect();
    let mut state = ListState::default().with_selected(Some(app.ui.actions_cursor));
    let list = List::new(items)
        .highlight_style(Style::default().bg(SELECTED_BG).fg(HIGHLIGHT_FG).add_modifier(Modifier::BOLD));
    f.render_stateful_widget(list, inner, &mut state);
}
