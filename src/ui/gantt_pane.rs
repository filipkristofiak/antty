use chrono::Timelike;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::widgets::{Block, Borders};

use crate::model::{Model, ParticipantId, ParticipantKind, Ts, TouchKind};
use crate::tree::Row;

use super::{AppRef, color_for, palette_color};

const SELECTED_BG: Color = Color::Rgb(50, 70, 130);
const CURSOR_BG: Color = Color::Rgb(40, 40, 60);

pub fn render(f: &mut Frame, area: Rect, app: &AppRef) {
    let block = Block::default().borders(Borders::ALL).title("Gantt Chart Timeline");
    let inner = block.inner(area);
    f.render_widget(block, area);
    if inner.height < 3 || inner.width == 0 {
        return;
    }
    render_header(f, Rect { height: 2, ..inner }, app);
    let body = Rect { y: inner.y + 2, height: inner.height.saturating_sub(2), ..inner };
    render_body(f, body, app);
}

fn is_boundary(start: Ts, end: Ts, col_secs: i64) -> bool {
    // Buckets are aligned to UTC seconds-since-epoch, so in a non-UTC timezone a boundary can
    // fall anywhere inside a bucket, not just at its start. Compare the local hour/day just
    // before each end of `[start, end)`: if they differ, a crossing happened somewhere within.
    let a = (start - chrono::Duration::seconds(1)).with_timezone(&chrono::Local);
    let b = (end - chrono::Duration::seconds(1)).with_timezone(&chrono::Local);
    if col_secs <= 900 { a.hour() != b.hour() } else { a.date_naive() != b.date_naive() }
}

fn render_header(f: &mut Frame, area: Rect, app: &AppRef) {
    let width = area.width as i64;
    let secs = app.view.col_secs();
    let cursor_col = app.view.col_for(app.view.cursor);
    let buf = f.buffer_mut();

    // At col_secs <= 900, `origin` (a multiple of col_secs) is also guaranteed a multiple of
    // 3600 divided evenly by col_secs, so every hour mark lands exactly on a bucket's `start`:
    // label that instant directly. At coarser zooms (6h/1d columns) a local day boundary is not
    // guaranteed to land on a bucket start (the local UTC offset need not divide col_secs), so
    // `is_boundary` finds the crossing anywhere in the bucket and we label its tail instead.
    let label_instant = |start: Ts, end: Ts| -> chrono::DateTime<chrono::Local> {
        if secs <= 900 {
            start.with_timezone(&chrono::Local)
        } else {
            (end - chrono::Duration::seconds(1)).with_timezone(&chrono::Local)
        }
    };

    let mut next_free = 0i64;
    for col in 0..width {
        let (start, end) = app.view.bucket(col);
        let local = label_instant(start, end);
        let boundary = col == 0 || is_boundary(start, end, secs);
        if boundary && col >= next_free {
            let is_month_start = local.format("%d").to_string() == "01";
            let label = if secs <= 900 {
                local.format("%H:%M").to_string()
            } else if col == 0 || is_month_start {
                local.format("%b %d").to_string()
            } else {
                local.format("%d").to_string()
            };
            let x = area.x + col as u16;
            if x < area.x + area.width {
                let max = (area.x + area.width - x) as usize;
                buf.set_string(x, area.y, label.chars().take(max).collect::<String>(), Style::default().fg(Color::Gray));
                next_free = col + label.chars().count() as i64 + 1;
            }
        }
    }

    for col in 0..width {
        let (start, end) = app.view.bucket(col);
        let local = label_instant(start, end);
        // At day zoom, every column is a day: show its weekday letter. At minute/hour zoom,
        // show the last digit of the minute/hour. In between (e.g. 6h columns), a per-column
        // hour digit cycles through unrelated values and reads as noise, so only columns where
        // a local day actually begins get a mark; the rest stay blank except for the cursor.
        let ch = if secs >= 86400 {
            local.format("%a").to_string().chars().next().unwrap_or(' ')
        } else if secs > 3600 {
            if is_boundary(start, end, secs) {
                local.format("%a").to_string().chars().next().unwrap_or(' ')
            } else {
                ' '
            }
        } else if secs >= 3600 {
            local.format("%H").to_string().chars().last().unwrap_or(' ')
        } else {
            local.format("%M").to_string().chars().last().unwrap_or(' ')
        };
        let mut style = Style::default().fg(Color::DarkGray);
        if col == cursor_col {
            style = style.bg(CURSOR_BG).fg(Color::White);
        }
        let x = area.x + col as u16;
        buf.set_string(x, area.y + 1, ch.to_string(), style);
    }
}

/// (glyph, color) for one row's cell at time bucket `[bs, be)`.
fn glyph_for_row(app: &AppRef, row: Row, bs: Ts, be: Ts) -> (char, Option<Color>) {
    match row {
        Row::Section(_) => (' ', None),
        Row::Participant(pid, _) => glyph_for_participant(app, pid, bs, be),
        Row::Node(idx) => glyph_for_node(app, idx, bs, be),
    }
}

fn glyph_for_participant(app: &AppRef, pid: ParticipantId, bs: Ts, be: Ts) -> (char, Option<Color>) {
    if pid == Model::YOU {
        let has_prompt = app
            .model
            .prompts
            .iter()
            .any(|p| p.at >= bs && p.at < be && app.model.session_visible(p.session));
        if has_prompt {
            return ('●', Some(palette_color(0)));
        }
        let has_event = app.model.events.iter().any(|e| e.who == Model::YOU && e.start < be && e.end >= bs);
        if has_event {
            return ('█', Some(palette_color(0)));
        }
        return (' ', None);
    }
    let own = app.model.spans.iter().any(|s| s.who == pid && s.start < be && s.end >= bs);
    if own {
        return ('█', Some(color_for(app.model, pid)));
    }
    let p = &app.model.participants[pid.0];
    if p.kind == ParticipantKind::Main
        && let Some(sidx) = p.session
            && !app.ui.expand.expanded_sessions.contains(&sidx) {
                let child_hit = app.model.spans.iter().any(|s| {
                    s.who != pid && s.start < be && s.end >= bs && app.model.participants[s.who.0].session == Some(sidx)
                });
                if child_hit {
                    return ('▓', Some(Color::DarkGray));
                }
            }
    (' ', None)
}

fn glyph_for_node(app: &AppRef, idx: usize, bs: Ts, be: Ts) -> (char, Option<Color>) {
    let node = &app.tree.nodes[idx];
    let mut last_write: Option<(Ts, ParticipantId)> = None;
    let mut last_read: Option<(Ts, ParticipantId)> = None;
    for &ei in &node.events {
        let e = &app.model.events[ei];
        if !app.model.visible(e.who) {
            continue;
        }
        if e.start < be && e.end >= bs {
            match e.kind {
                TouchKind::Write => {
                    if last_write.map(|(t, _)| e.end > t).unwrap_or(true) {
                        last_write = Some((e.end, e.who));
                    }
                }
                TouchKind::Read => {
                    if last_read.map(|(t, _)| e.end > t).unwrap_or(true) {
                        last_read = Some((e.end, e.who));
                    }
                }
            }
        }
    }
    if let Some((_, who)) = last_write {
        return ('█', Some(color_for(app.model, who)));
    }
    if let Some((_, who)) = last_read {
        return ('░', Some(color_for(app.model, who)));
    }
    (' ', None)
}

fn render_body(f: &mut Frame, area: Rect, app: &AppRef) {
    let width = area.width as i64;
    let cursor_col = app.view.col_for(app.view.cursor);
    let now = chrono::Utc::now();
    let now_col = app.view.col_for(now);
    let buf = f.buffer_mut();

    for r in 0..area.height {
        let row_idx = app.ui.scroll + r as usize;
        if row_idx >= app.rows.len() {
            break;
        }
        let row = app.rows[row_idx];
        let is_selected = row_idx == app.ui.selected;
        let y = area.y + r;
        for col in 0..width {
            let (bs, be) = app.view.bucket(col);
            let (ch, color) = glyph_for_row(app, row, bs, be);
            let mut style = Style::default();
            if let Some(c) = color {
                style = style.fg(c);
            }
            if is_selected {
                style = style.bg(SELECTED_BG);
            } else if col == cursor_col {
                style = style.bg(CURSOR_BG);
            }
            let x = area.x + col as u16;
            let out = if ch == ' ' && col == now_col && !is_selected && col != cursor_col {
                style = style.fg(Color::LightBlue);
                '│'
            } else {
                ch
            };
            buf.set_string(x, y, out.to_string(), style);
        }
    }
}
