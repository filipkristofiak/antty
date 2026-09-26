use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::widgets::Paragraph;

use super::{AppRef, truncate};

pub fn render(f: &mut Frame, area: Rect, app: &AppRef) {
    let hints = "Nav (j/k/h/l) | Zoom (+/-) | Now (t) | Fit (f) | Detail (⏎) | Sessions (s) | Help (?)";
    let mut right = format!(
        "{} · {} sessions · watch:{}",
        app.view.zoom_label(),
        app.model.sessions.len(),
        if app.ui.watch_on { "on" } else { "off" }
    );
    if let Some(err) = &app.ui.status_extra {
        right = format!("{right} · {err}");
    }
    let width = area.width as usize;
    let left = truncate(hints, width);
    let text = if left.len() + right.len() + 2 <= width {
        let pad = width - left.len() - right.len();
        format!("{left}{}{right}", " ".repeat(pad))
    } else {
        truncate(&format!("{left}  {right}"), width)
    };
    f.render_widget(Paragraph::new(text).style(Style::default().bg(Color::Black).fg(Color::Gray)), area);
}
