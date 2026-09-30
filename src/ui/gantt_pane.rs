use std::fmt::Write;

use chrono::Timelike;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::widgets::{Block, Borders};

use crate::model::{Model, ParticipantId, ParticipantKind, TouchKind, Ts};
use crate::timeline::Column;
use crate::tree::Row;

use super::{AppRef, CURSOR_BG, HIGHLIGHT_FG, SELECTED_BG, color_for, palette_color};

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
    if col_secs < 3600 { a.hour() != b.hour() } else { a.date_naive() != b.date_naive() }
}

fn tick_char(local: chrono::DateTime<chrono::Local>, secs: i64, day_boundary: bool) -> char {
    if secs >= 86400 || day_boundary {
        local.format("%a").to_string().chars().next().unwrap_or(' ')
    } else if secs > 3600 {
        ' '
    } else if secs >= 1800 {
        local.format("%H").to_string().chars().last().unwrap_or(' ')
    } else if matches!(secs, 300 | 600 | 900) {
        local.format("%M").to_string().chars().next().unwrap_or(' ')
    } else {
        local.format("%M").to_string().chars().last().unwrap_or(' ')
    }
}

fn fmt_span(d: chrono::Duration) -> String {
    let mut seconds = d.num_seconds().max(0);
    let mut parts = String::new();
    let mut written = 0;
    for (unit, suffix) in [(86400, "d"), (3600, "h"), (60, "m"), (1, "s")] {
        let count = seconds / unit;
        if count > 0 {
            write!(parts, "{count}{suffix}").expect("writing to a String cannot fail");
            seconds %= unit;
            written += 1;
            if written == 2 {
                break;
            }
        }
    }
    if written == 0 { "0s".to_string() } else { parts }
}

/// Omit a duration entirely rather than showing a clipped or overwritten number.
fn break_label(duration: chrono::Duration, col: i64, next_break: Option<i64>, width: i64) -> String {
    let label = format!("~{}", fmt_span(duration));
    let end = col + label.len() as i64;
    if end <= width && next_break.is_none_or(|next| end < next) { label } else { "~".to_string() }
}

fn render_header(f: &mut Frame, area: Rect, app: &AppRef) {
    let width = area.width as i64;
    let secs = app.view.col_secs();
    let cursor_col = app.view.col_for(app.view.cursor);
    let cols: Vec<Column> = (0..width).map(|c| app.view.column(c)).collect();
    let buf = f.buffer_mut();

    // At col_secs < 3600 (every such level divides 3600), `origin` (a multiple of col_secs) is
    // also guaranteed a multiple of 3600 divided evenly by col_secs, so every hour mark lands
    // exactly on a bucket's `start`: label that instant directly. At coarser zooms (2h and up) a
    // local day boundary is not guaranteed to land on a bucket start (the local UTC offset need
    // not divide col_secs), so `is_boundary` finds the crossing anywhere in the bucket and we
    // label its tail instead.
    let label_instant = |start: Ts, end: Ts| -> chrono::DateTime<chrono::Local> {
        if secs < 3600 {
            start.with_timezone(&chrono::Local)
        } else {
            (end - chrono::Duration::seconds(1)).with_timezone(&chrono::Local)
        }
    };

    let breaks: Vec<_> = cols
        .iter()
        .enumerate()
        .filter_map(|(col, column)| match column {
            Column::Break { start, end } => Some((col as i64, *start, *end)),
            Column::Time { .. } | Column::GapPad { .. } => None,
        })
        .collect();
    let mut reserved = Vec::with_capacity(breaks.len());
    for (i, &(col, start, end)) in breaks.iter().enumerate() {
        let next_break = breaks.get(i + 1).map(|b| b.0);
        let shown = break_label(end - start, col, next_break, width);
        buf.set_string(area.x + col as u16, area.y, &shown, Style::default().fg(Color::DarkGray));
        // Protect both blank flanks and a space after the label so it cannot read as a time.
        reserved.push((col - 1, col + shown.len() as i64));
    }

    let mut next_free = 0i64;
    let mut force = true;
    let mut break_start: Option<Ts> = None;
    for (col, &column) in cols.iter().enumerate() {
        let (start, end) = match column {
            Column::Time { start, end } => (start, end),
            Column::Break { start, .. } => {
                force = true;
                break_start = Some(start);
                continue;
            }
            Column::GapPad { .. } => continue,
        };
        let col = col as i64;
        let local = label_instant(start, end);
        if (force || is_boundary(start, end, secs)) && col >= next_free {
            let is_month_start = local.format("%d").to_string() == "01";
            let label = if secs < 3600 {
                if force
                    && break_start.is_some_and(|b| b.with_timezone(&chrono::Local).date_naive() != local.date_naive())
                {
                    local.format("%a %H:%M").to_string()
                } else {
                    local.format("%H:%M").to_string()
                }
            } else if force || is_month_start {
                local.format("%b %d").to_string()
            } else {
                local.format("%d").to_string()
            };
            let len = label.chars().count() as i64;
            if reserved.iter().any(|&(rs, re)| col <= re && rs <= col + len) {
                continue;
            }
            let x = area.x + col as u16;
            let max = (area.x + area.width - x) as usize;
            buf.set_string(x, area.y, label.chars().take(max).collect::<String>(), Style::default().fg(Color::Gray));
            next_free = col + len + 1;
            force = false;
        }
    }

    for (col, &column) in cols.iter().enumerate() {
        let ch = match column {
            Column::Break { .. } => '~',
            Column::GapPad { .. } => ' ',
            Column::Time { start, end } => {
                let local = label_instant(start, end);
                // At day zoom, show the weekday letter. At minute zoom, show the last minute digit;
                // at 30m/1h zoom, show the last hour digit. Between 1h and 1d only mark local day
                // boundaries. At 5m/10m/15m the minute's last digit only alternates 0/5 or stays 0,
                // so show the tens digit instead (5m: 001122…, 15m: 0134).
                tick_char(local, secs, secs > 3600 && secs < 86400 && is_boundary(start, end, secs))
            }
        };
        let mut style = Style::default().fg(Color::DarkGray);
        if col as i64 == cursor_col {
            style = style.bg(CURSOR_BG).fg(HIGHLIGHT_FG);
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
        let has_prompt =
            app.model.prompts.iter().any(|p| p.at >= bs && p.at < be && app.model.session_visible(p.session));
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
        && !app.ui.expand.expanded_sessions.contains(&sidx)
    {
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
    let cols: Vec<Column> = (0..width).map(|c| app.view.column(c)).collect();
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
        for (col, &column) in cols.iter().enumerate() {
            let col = col as i64;
            let (ch, color) = match column {
                Column::Break { .. } => ('~', Some(Color::DarkGray)),
                Column::GapPad { .. } => (' ', None),
                Column::Time { start, end } => glyph_for_row(app, row, start, end),
            };
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
            let out = if matches!(column, Column::Time { .. })
                && ch == ' '
                && col == now_col
                && !is_selected
                && col != cursor_col
            {
                style = style.fg(Color::LightBlue);
                '│'
            } else {
                ch
            };
            buf.set_string(x, y, out.to_string(), style);
        }
    }
}

#[cfg(test)]
mod tests {
    use chrono::{Duration, Local, TimeZone};

    use super::{break_label, fmt_span, tick_char};

    #[test]
    fn minute_ticks_show_changing_digits() {
        let at = |minute| Local.with_ymd_and_hms(2026, 1, 5, 10, minute, 0).unwrap();
        for (minute, expected) in [(0, '0'), (15, '1'), (30, '3'), (45, '4')] {
            assert_eq!(tick_char(at(minute), 900, false), expected);
        }
        assert_eq!(tick_char(at(5), 300, false), '0');
        assert_eq!(tick_char(at(55), 300, false), '5');
        assert_eq!(tick_char(at(7), 60, false), '7');
    }
    #[test]
    fn fmt_span_uses_two_largest_units() {
        for (secs, label) in [(100_800, "1d4h"), (12_000, "3h20m"), (7_200, "2h"), (2_700, "45m"), (30, "30s")] {
            assert_eq!(fmt_span(Duration::seconds(secs)), label);
        }
        assert_eq!(fmt_span(Duration::seconds(0)), "0s");
    }
    #[test]
    fn break_labels_never_show_partial_durations() {
        let duration = Duration::hours(14) + Duration::minutes(50);
        assert_eq!(break_label(duration, 10, Some(20), 40), "~14h50m");
        assert_eq!(break_label(duration, 10, Some(17), 40), "~");
        assert_eq!(break_label(duration, 10, None, 16), "~");
        assert_eq!(break_label(duration, 10, None, 17), "~14h50m");
    }
}
