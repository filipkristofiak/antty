use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::widgets::Paragraph;

use antty::filter::{Action, ActionKey, label};

use super::AppRef;
use antty::tui::truncate;

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
        "{} · {} sessions · watch:{} · follow:{} · gaps:{}",
        app.view.zoom_label(),
        app.model.sessions.len(),
        if app.ui.watch_on { "on" } else { "off" },
        if app.ui.expand.auto_follow { "on" } else { "off" },
        if app.view.collapse_gaps() { "on" } else { "off" }
    );
    if let Some((done, total)) = app.ui.loading {
        right = format!("indexing {done}/{total} · {right}");
    }
    if app.model.roots.len() > 1 {
        right.push_str(match app.ui.files_view {
            antty::tree::FilesView::Merged => " · worktrees:merged",
            antty::tree::FilesView::Separate => " · worktrees:separate",
        });
    }
    if let Some(filter) = &app.model.action_filter {
        right.push_str(" · filter:");
        let mut tools: Vec<_> = filter.fs_during.iter().map(String::as_str).collect();
        tools.sort_unstable();
        let keys = Action::ALL
            .into_iter()
            .filter(|a| filter.actions.contains(a))
            .map(ActionKey::Fixed)
            .chain(tools.into_iter().map(ActionKey::FsDuring));
        let label_start = right.len();
        for key in keys {
            if right.len() != label_start {
                right.push(',');
            }
            right.push_str(&label(key));
        }
        if right.len() == label_start {
            right.push_str("none");
        }
        if filter.scripts_only {
            right.push_str(",+scripts");
        }
    }
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
    // Keep the stats state readable even when the narrow layout clips the other status text.
    let stats = match (app.ui.stats_window, app.ui.stats_panel && area.width < super::STATS_MIN_WIDTH) {
        (true, true) => " · stats:range · stats:narrow",
        (true, false) => " · stats:range",
        (false, true) => " · stats:narrow",
        (false, false) => "",
    };
    let width = width.saturating_sub(stats.chars().count());
    let left = truncate(hints, width);
    let left_len = left.chars().count();
    let right_len = right.chars().count();
    let mut text = if left_len + right_len + 2 <= width {
        let pad = width - left_len - right_len;
        format!("{left}{}{right}", " ".repeat(pad))
    } else {
        truncate(&format!("{left}  {right}"), width)
    };
    text.push_str(stats);
    f.render_widget(Paragraph::new(text), area);
}
