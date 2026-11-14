use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::widgets::{Block, Borders};

use antty::model::{Model, ParticipantId, ParticipantKind, TouchKind, Ts};
use antty::timeline::Column;
use antty::tree::Row;

use super::{AppRef, TOUCHED_BG, color_for, palette_color};
use antty::tui::{CURSOR_BG, SELECTED_BG};

type FoldLane = (ParticipantId, Vec<(Ts, Ts)>);

pub fn render(f: &mut Frame, area: Rect, app: &AppRef, touched: &[bool], folded: &[usize]) {
    let block = Block::default().borders(Borders::ALL).title("Gantt Chart Timeline");
    let inner = block.inner(area);
    f.render_widget(block, area);
    if inner.height < 3 || inner.width == 0 {
        return;
    }
    antty::tui::render_header(f, Rect { height: 2, ..inner }, app.view);
    let body = Rect { y: inner.y + 2, height: inner.height.saturating_sub(2), ..inner };
    render_body(f, body, app, touched, folded);
}

/// (glyph, color) for one row's cell at time bucket `[bs, be)`.
fn glyph_for_row(
    app: &AppRef,
    row: Row,
    bs: Ts,
    be: Ts,
    lanes: Option<&[Vec<(Ts, Ts)>]>,
    fold: &[FoldLane],
) -> (char, Option<Color>) {
    match row {
        Row::Section(_) => (' ', None),
        Row::Fold => {
            let mut hits = fold.iter().filter(|(_, lane)| lane.iter().any(|&(s, e)| s < be && e >= bs));
            let Some((main, _)) = hits.next() else { return fold_glyph(0, None) };
            if hits.next().is_none() {
                fold_glyph(1, Some(color_for(app.model, *main)))
            } else {
                fold_glyph(2 + usize::from(hits.next().is_some()), None)
            }
        }
        Row::Participant(pid, _) => glyph_for_participant(app, pid, bs, be, lanes),
        Row::Node(idx) => glyph_for_node(app, idx, bs, be),
    }
}

/// Fold-strip cell: bar height is the number of folded sessions active (▄ 1, ▆ 2, █ 3+);
/// a lone session keeps its colour, 2+ are DarkGray. Shape carries the count under NO_COLOR.
fn fold_glyph(hits: usize, lone: Option<Color>) -> (char, Option<Color>) {
    match hits {
        0 => (' ', None),
        1 => ('▄', lone),
        2 => ('▆', Some(Color::DarkGray)),
        _ => ('█', Some(Color::DarkGray)),
    }
}

fn glyph_for_participant(
    app: &AppRef,
    pid: ParticipantId,
    bs: Ts,
    be: Ts,
    lanes: Option<&[Vec<(Ts, Ts)>]>,
) -> (char, Option<Color>) {
    if let Some(lanes) = lanes {
        let hit = |lane: &[(Ts, Ts)]| lane.iter().any(|&(s, e)| s < be && e >= bs);
        if hit(&lanes[pid.0]) {
            let color = if pid == Model::YOU { palette_color(0) } else { color_for(app.model, pid) };
            return ('█', Some(color));
        }
        let p = &app.model.participants[pid.0];
        if p.kind == ParticipantKind::Main
            && let Some(sidx) = p.session
            && !app.ui.expand.expanded_sessions.contains(&sidx)
            && app
                .model
                .participants
                .iter()
                .enumerate()
                .any(|(i, q)| i != pid.0 && q.session == Some(sidx) && hit(&lanes[i]))
        {
            return ('▓', Some(Color::DarkGray));
        }
        return (' ', None);
    }
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
        if !app.model.event_visible(e) {
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

fn fold_lanes(model: &Model, folded: &[usize], lanes: Option<&[Vec<(Ts, Ts)>]>) -> Vec<FoldLane> {
    if folded.is_empty() {
        return Vec::new();
    }
    // Group once by session rather than scanning all spans separately for each folded session.
    let mut index = vec![None; model.sessions.len()];
    let mut fold = Vec::with_capacity(folded.len());
    for &s in folded {
        index[s] = Some(fold.len());
        fold.push((model.sessions[s].main, Vec::new()));
    }
    if let Some(lanes) = lanes {
        for (p, lane) in model.participants.iter().zip(lanes) {
            if let Some(i) = p.session.and_then(|s| index[s]) {
                fold[i].1.extend_from_slice(lane);
            }
        }
    } else {
        for s in &model.spans {
            if let Some(i) = model.participants[s.who.0].session.and_then(|s| index[s]) {
                fold[i].1.push((s.start, s.end));
            }
        }
    }
    fold
}

fn render_body(f: &mut Frame, area: Rect, app: &AppRef, touched: &[bool], folded: &[usize]) {
    let width = area.width as i64;
    let cols: Vec<Column> = (0..width).map(|c| app.view.column(c)).collect();
    let cursor_col = app.view.col_for(app.view.cursor);
    let now = chrono::Utc::now();
    let now_col = app.view.col_for(now);
    let buf = f.buffer_mut();
    let lanes = app.model.action_filter.as_ref().map(|_| {
        let mut lanes = vec![Vec::new(); app.model.participants.len()];
        for e in app.model.events.iter().filter(|e| app.model.event_visible(e)) {
            lanes[e.who.0].push((e.start, e.end));
        }
        lanes
    });
    let fold = fold_lanes(app.model, folded, lanes.as_deref());

    for r in 0..area.height {
        let row_idx = app.ui.scroll + r as usize;
        if row_idx >= app.rows.len() {
            break;
        }
        let row = app.rows[row_idx];
        let is_selected = row_idx == app.ui.selected;
        let touched_row = !app.ui.no_color && matches!(row, Row::Participant(pid, _) if touched[pid.0]);
        let y = area.y + r;
        for (col, &column) in cols.iter().enumerate() {
            let col = col as i64;
            let (ch, color) = match column {
                Column::Break { .. } => ('~', Some(Color::DarkGray)),
                Column::GapPad { .. } => (' ', None),
                Column::Time { start, end } => glyph_for_row(app, row, start, end, lanes.as_deref(), &fold),
            };
            let mut style = Style::default();
            if let Some(c) = color {
                style = style.fg(c);
            }
            if is_selected {
                style = style.bg(SELECTED_BG);
            } else if col == cursor_col {
                style = style.bg(CURSOR_BG);
            } else if touched_row {
                style = style.bg(TOUCHED_BG);
            }
            let x = area.x + col as u16;
            let out = if matches!(column, Column::Time { .. })
                && ch == ' '
                && row != Row::Fold
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
    use super::{Color, fold_glyph};

    #[test]
    fn fold_glyph_encodes_session_count() {
        assert_eq!(fold_glyph(0, None), (' ', None));
        assert_eq!(fold_glyph(1, Some(Color::Red)), ('▄', Some(Color::Red)));
        assert_eq!(fold_glyph(2, Some(Color::Red)), ('▆', Some(Color::DarkGray)));
        assert_eq!(fold_glyph(5, None), ('█', Some(Color::DarkGray)));
    }
}
