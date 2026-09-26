use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::widgets::{Block, Borders, Paragraph};

use super::AppRef;
use super::detail;
use crate::ui::DetailItem;

/// Full-screen scrollable rendering of the currently selected Detail item (its diff, for
/// `Diff`/`Written`/`Fs` events; whatever `detail::detail_lines` produces otherwise).
pub fn render(f: &mut Frame, area: Rect, app: &AppRef) {
    let Some(item) = detail::selected_item(app) else { return };
    let title = match &item {
        DetailItem::Event(idx) => {
            let e = &app.model.events[*idx];
            format!(
                " {} · {} · {} · {}  (j/k scroll · Ctrl-d/u page · g/G top/bottom · Esc back) ",
                detail::display_target(app.model, e),
                app.model.participants[e.who.0].label,
                detail::event_kind_label(e),
                e.start.with_timezone(&chrono::Local).format("%Y-%m-%d %H:%M:%S")
            )
        }
        _ => " detail ".to_string(),
    };
    let block = Block::default().borders(Borders::ALL).title(title);
    let inner = block.inner(area);
    f.render_widget(block, area);
    if inner.width == 0 || inner.height == 0 {
        return;
    }
    let text = detail::detail_lines(app, &item);
    let para = Paragraph::new(text).scroll((app.ui.diff_scroll, 0));
    f.render_widget(para, inner);
}
