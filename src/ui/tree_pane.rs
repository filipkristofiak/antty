use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::widgets::{Block, Borders};

use crate::model::{Model, ParticipantKind};
use crate::tree::Row;

use super::{AppRef, color_for, truncate};

const SELECTED_BG: Color = Color::Rgb(50, 70, 130);

fn row_text_and_style(app: &AppRef, row: Row) -> (String, Style) {
    match row {
        Row::Section(name) => (name.to_uppercase(), Style::default().add_modifier(Modifier::BOLD)),
        Row::Participant(pid, depth) => {
            let indent = "  ".repeat(depth);
            let p = &app.model.participants[pid.0];
            let label = if pid == Model::YOU {
                "you".to_string()
            } else if p.kind == ParticipantKind::Main {
                match p.session {
                    Some(sidx) => {
                        let s = &app.model.sessions[sidx];
                        format!("{} · {}", s.title, s.start.with_timezone(&chrono::Local).format("%m-%d %H:%M"))
                    }
                    None => p.label.clone(),
                }
            } else {
                p.label.clone()
            };
            let marker = if p.kind == ParticipantKind::Main {
                match p.session {
                    Some(sidx) if app.ui.expand.expanded_sessions.contains(&sidx) => "▾ ",
                    Some(_) => "▸ ",
                    None => "",
                }
            } else {
                ""
            };
            (format!("{indent}{marker}{label}"), Style::default().fg(color_for(app.model, pid)))
        }
        Row::Node(idx) => {
            let node = &app.tree.nodes[idx];
            let indent = "  ".repeat(node.depth);
            let arrow = if node.is_dir {
                if app.ui.expand.is_dir_expanded(node) { "▾ " } else { "▸ " }
            } else {
                "  "
            };
            let mut style = Style::default();
            if node.deleted {
                style = style.fg(Color::DarkGray);
            }
            (format!("{indent}{arrow}{}", node.name), style)
        }
    }
}

pub fn render(f: &mut Frame, area: Rect, app: &AppRef) {
    let block = Block::default().borders(Borders::ALL).title("files");
    let inner = block.inner(area);
    f.render_widget(block, area);
    if inner.height < 3 || inner.width == 0 {
        return;
    }
    // 2-line spacer to align rows with the gantt pane's date/tick header.
    let body_y = inner.y + 2;
    let body_height = inner.height.saturating_sub(2);
    let width = inner.width as usize;
    let buf = f.buffer_mut();
    for r in 0..body_height {
        let row_idx = app.ui.scroll + r as usize;
        if row_idx >= app.rows.len() {
            break;
        }
        let row = app.rows[row_idx];
        let (text, mut style) = row_text_and_style(app, row);
        let is_selected = row_idx == app.ui.selected;
        if is_selected {
            style = style.bg(SELECTED_BG);
            buf.set_string(inner.x, body_y + r, " ".repeat(width), style);
        }
        buf.set_string(inner.x, body_y + r, truncate(&text, width), style);
    }
}
