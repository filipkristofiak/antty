use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::widgets::Paragraph;

use super::{AppRef, truncate};

pub fn render(f: &mut Frame, area: Rect, app: &AppRef) {
    if let Some(line) = &app.ui.cmdline {
        let text = format!("{}{}", line.kind.prompt(), line.text);
        f.render_widget(Paragraph::new(text), area);
        f.set_cursor_position((area.x + 1 + line.text.chars().count() as u16, area.y));
        return;
    }
    let width = area.width as usize;
    if let Some(msg) = &app.ui.flash {
        // A flash (e.g. an unknown `:` command) replaces the whole bar, like vim's error line,
        // so it's never at risk of being truncated away by the hints/right-side text.
        let text = truncate(msg, width);
        f.render_widget(Paragraph::new(text).style(Style::default().fg(Color::Red)), area);
        return;
    }
    let hints = "Nav (j/k/h/l) | Zoom (+/-) | Now (t) | Fit (f) | Detail (⏎) | Sessions (s) | Help (?) | Quit (:q)";
    let mut right = format!(
        "{} · {} sessions · watch:{} · follow:{}",
        app.view.zoom_label(),
        app.model.sessions.len(),
        if app.ui.watch_on { "on" } else { "off" },
        if app.ui.expand.auto_follow { "on" } else { "off" }
    );
    if let Some(err) = &app.ui.status_extra {
        right = format!("{right} · {err}");
    }
    if let Some(query) = &app.ui.search {
        right = format!("/{query} · {right}");
    }
    let mut showcmd = app.ui.count.map_or_else(String::new, |n| n.to_string());
    if let Some(prefix) = app.ui.pending {
        showcmd.push(prefix);
    }
    if !showcmd.is_empty() {
        right = format!("{showcmd} · {right}");
    }
    let left = truncate(hints, width);
    let left_len = left.chars().count();
    let right_len = right.chars().count();
    let text = if left_len + right_len + 2 <= width {
        let pad = width - left_len - right_len;
        format!("{left}{}{right}", " ".repeat(pad))
    } else {
        truncate(&format!("{left}  {right}"), width)
    };
    f.render_widget(Paragraph::new(text), area);
}
