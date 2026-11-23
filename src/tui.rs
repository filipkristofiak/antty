//! Terminal UI pieces shared by the antty and formi binaries: highlight colours,
//! the timeline ruler and handing the terminal to a child process.

use std::ffi::OsString;
use std::fmt::Write;

use chrono::Timelike;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Line;
use ratatui::widgets::{Block, Borders, Clear, Paragraph};

use crate::model::Ts;
use crate::timeline::{Column, View};
pub const SELECTED_BG: Color = Color::Rgb(50, 70, 130);
pub const CURSOR_BG: Color = Color::Rgb(40, 40, 60);
/// Text drawn on SELECTED_BG/CURSOR_BG. Explicit RGB so it stays light when the terminal's
/// default foreground is dark (light themes).
pub const HIGHLIGHT_FG: Color = Color::Rgb(235, 235, 235);

/// Sessions (and formi projects) active this recently are never folded.
pub const LIVE_HOURS: i64 = 2;

/// Fold-strip cell: bar height is the number of folded rows active (▄ 1, ▆ 2, █ 3+);
/// a lone hit keeps its colour, 2+ are DarkGray. Shape carries the count under NO_COLOR.
pub fn fold_glyph(hits: usize, lone: Option<Color>) -> (char, Option<Color>) {
    match hits {
        0 => (' ', None),
        1 => ('▄', lone),
        2 => ('▆', Some(Color::DarkGray)),
        _ => ('█', Some(Color::DarkGray)),
    }
}

pub fn centered_rect(percent_x: u16, percent_y: u16, area: Rect) -> Rect {
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(area);
    let horizontal = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(vertical[1]);
    horizontal[1]
}

/// Help overlay content: (section title, [(keys, description)]), in display order.
pub type HelpSections = &'static [(&'static str, &'static [(&'static str, &'static str)])];

pub fn help_line_count(sections: HelpSections) -> usize {
    let separators = sections.len().saturating_sub(1);
    separators + sections.iter().map(|(_, keys)| 1 + keys.len()).sum::<usize>()
}

fn help_lines(sections: HelpSections) -> Vec<Line<'static>> {
    let mut lines = Vec::with_capacity(help_line_count(sections));
    let heading = Style::default().add_modifier(Modifier::BOLD);
    for (title, keys) in sections {
        if !lines.is_empty() {
            lines.push(Line::default());
        }
        lines.push(Line::styled(*title, heading));
        lines.extend(keys.iter().map(|(k, d)| Line::from(format!("{k:<16} {d}"))));
    }
    lines
}

pub fn render_help(f: &mut Frame, area: Rect, scroll: u16, sections: HelpSections) {
    f.render_widget(Clear, area);
    let block = Block::default().borders(Borders::ALL);
    let inner = block.inner(area);
    let lines = help_lines(sections);
    let total = lines.len();
    let visible = usize::from(inner.height);
    let top = usize::from(scroll).min(total.saturating_sub(visible));
    let block = if visible > 0 && total > visible {
        block.title(format!("Help · {}-{}/{}", top + 1, top + visible, total))
    } else {
        block.title("Help")
    };
    f.render_widget(block, area);
    f.render_widget(Paragraph::new(lines).scroll((top as u16, 0)), inner);
}

/// Applies one key to an open help overlay; returns true when the key closes it.
pub fn help_key(key: KeyEvent, scroll: &mut u16, page: u16, sections: HelpSections) -> bool {
    let max = (help_line_count(sections) as u16).saturating_sub(page);
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    // A taller terminal may already be drawing at a lower offset.
    *scroll = (*scroll).min(max);
    match key.code {
        KeyCode::Char('q' | '?') | KeyCode::Esc => return true,
        KeyCode::Char('d') if ctrl => *scroll = scroll.saturating_add(page / 2).min(max),
        KeyCode::Char('u') if ctrl => *scroll = scroll.saturating_sub(page / 2),
        KeyCode::Char('j') | KeyCode::Down => *scroll = scroll.saturating_add(1).min(max),
        KeyCode::Char('k') | KeyCode::Up => *scroll = scroll.saturating_sub(1),
        KeyCode::Char('g') => *scroll = 0,
        KeyCode::Char('G') => *scroll = max,
        _ => {}
    }
    false
}

pub fn truncate(s: &str, width: usize) -> String {
    if s.chars().count() <= width {
        s.to_string()
    } else {
        s.chars().take(width.saturating_sub(1)).collect::<String>() + "…"
    }
}
/// NO_COLOR: drop every fg/bg colour. Background highlights become reverse video,
/// and DarkGray text (deleted files, advisors, collapsed-session bars) becomes dim.
pub fn strip_colors(buf: &mut ratatui::buffer::Buffer) {
    for cell in &mut buf.content {
        if cell.bg != Color::Reset {
            cell.modifier.insert(Modifier::REVERSED);
        }
        if cell.fg == Color::DarkGray {
            cell.modifier.insert(Modifier::DIM);
        }
        cell.fg = Color::Reset;
        cell.bg = Color::Reset;
    }
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

pub fn fmt_span(d: chrono::Duration) -> String {
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

pub fn render_header(f: &mut Frame, area: Rect, view: &View) {
    let width = area.width as i64;
    let secs = view.col_secs();
    let cursor_col = view.col_for(view.cursor);
    let cols: Vec<Column> = (0..width).map(|c| view.column(c)).collect();
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
pub struct Invocation {
    pub program: OsString,
    pub args: Vec<OsString>,
    pub suspend: bool,
    /// Bytes written to the child's stdin; honored only when `suspend` is true.
    pub stdin: Option<String>,
}
/// Run `inv`; returns a flash message on failure. Suspending invocations hand the terminal to
/// the child and restore the TUI afterwards.
pub fn run(terminal: &mut ratatui::DefaultTerminal, inv: &Invocation) -> std::io::Result<Option<String>> {
    use crossterm::event::{self, Event, KeyEventKind};
    use std::io::{Write, stdout};
    use std::process::{Command, Stdio};

    let program = inv.program.to_string_lossy();
    if !inv.suspend {
        let result = Command::new(&inv.program).args(&inv.args).stdin(Stdio::null()).output();
        return Ok(match result {
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Some(format!("{program}: not found on PATH")),
            Err(err) => Some(format!("{program}: {err}")),
            Ok(out) if !out.status.success() => Some(
                String::from_utf8_lossy(&out.stderr)
                    .lines()
                    .find(|line| !line.trim().is_empty())
                    .map(str::to_owned)
                    .unwrap_or_else(|| format!("{program} exited with {}", out.status)),
            ),
            Ok(_) => None,
        });
    }

    leave_tui()?;
    let result = match &inv.stdin {
        None => Command::new(&inv.program).args(&inv.args).status(),
        Some(input) => {
            Command::new(&inv.program).args(&inv.args).stdin(Stdio::piped()).spawn().and_then(|mut child| {
                if let Some(mut pipe) = child.stdin.take() {
                    let _ = pipe.write_all(input.as_bytes());
                }
                child.wait()
            })
        }
    };
    crossterm::terminal::enable_raw_mode()?;
    // A pager may exit without waiting (less -F, cat, etc.). Keep its output on the
    // primary screen until the user has had a chance to read it.
    let pause = if inv.stdin.is_some() && result.is_ok() {
        crossterm::execute!(stdout(), crossterm::style::Print("\r\n-- press any key to return to antty --"))?;
        loop {
            match event::read() {
                Ok(Event::Key(key)) if key.kind == KeyEventKind::Press => break Ok(()),
                Ok(_) => {}
                Err(err) => break Err(err),
            }
        }
    } else {
        Ok(())
    };
    enter_tui(terminal)?;
    pause?;
    Ok(match result {
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Some(format!("{program}: not found on PATH")),
        Err(err) => Some(format!("{program}: {err}")),
        Ok(status) if !status.success() => Some(format!("{program} exited with {status}")),
        Ok(_) => None,
    })
}

fn leave_tui() -> std::io::Result<()> {
    ratatui::restore();
    crossterm::execute!(std::io::stdout(), crossterm::cursor::Show)
}

fn enter_tui(terminal: &mut ratatui::DefaultTerminal) -> std::io::Result<()> {
    crossterm::execute!(std::io::stdout(), crossterm::terminal::EnterAlternateScreen)?;
    terminal.clear()?;
    terminal.hide_cursor()
}

/// Terminal mouse reporting that is switched off while a child process owns the terminal
/// and when dropped.
#[derive(Default)]
pub struct MouseCapture(bool);

impl MouseCapture {
    pub fn enabled(&self) -> bool {
        self.0
    }

    pub fn set(&mut self, enabled: bool) -> std::io::Result<()> {
        use crossterm::event::{DisableMouseCapture, EnableMouseCapture};
        if enabled && !self.0 {
            // Also attempt cleanup if enabling fails after writing only part of the sequence.
            self.0 = true;
            crossterm::execute!(std::io::stdout(), EnableMouseCapture)?;
        } else if !enabled && self.0 {
            crossterm::execute!(std::io::stdout(), DisableMouseCapture)?;
            self.0 = false;
        }
        Ok(())
    }

    pub fn hand_off<T>(&mut self, run: impl FnOnce() -> std::io::Result<T>) -> std::io::Result<T> {
        let enabled = self.0;
        self.set(false)?;
        let result = run()?;
        self.set(enabled)?;
        Ok(result)
    }
}

impl Drop for MouseCapture {
    fn drop(&mut self) {
        let _ = self.set(false);
    }
}

/// Stop antty like vim's `Ctrl-z`: hand the terminal back, SIGTSTP ourselves, and restore
/// the TUI once the shell resumes us (`fg`). Returns a flash message on failure.
#[cfg(unix)]
pub fn suspend(terminal: &mut ratatui::DefaultTerminal) -> std::io::Result<Option<String>> {
    leave_tui()?;
    // SAFETY: raise(3) only delivers a signal to the calling process.
    let failed = unsafe { libc::raise(libc::SIGTSTP) } != 0;
    let err = failed.then(std::io::Error::last_os_error);
    crossterm::terminal::enable_raw_mode()?;
    enter_tui(terminal)?;
    Ok(err.map(|e| format!("suspend: {e}")))
}

#[cfg(not(unix))]
pub fn suspend(_terminal: &mut ratatui::DefaultTerminal) -> std::io::Result<Option<String>> {
    Ok(Some("Ctrl-z: suspend is not supported on this platform".into()))
}

pub const STATS_PANEL_WIDTH: u16 = 34;
/// Narrower terminals leave no room for the panel beside the tree and timeline.
pub const STATS_MIN_WIDTH: u16 = 110;

/// Selection-vs-all comparison drawn by the stats panel.
pub struct StatsView<'a> {
    pub label: &'a str,
    pub selected: &'a crate::stats::Stats,
    pub all: &'a crate::stats::Stats,
    /// Limited to the visible timeline range.
    pub range: bool,
    /// Add a row with the selection's share of all tool calls.
    pub share: bool,
}

/// `n` in at most five characters: exact below 10k, then `12.3k`/`4.5M`.
pub fn compact(n: usize) -> String {
    match n {
        0..10_000 => n.to_string(),
        10_000..1_000_000 => format!("{:.1}k", n as f64 / 1_000.0),
        _ => format!("{:.1}M", n as f64 / 1_000_000.0),
    }
}

/// `n` bytes in decimal units: `512 B`, `9.2 MB`, `664.0 MB`.
pub fn bytes(n: u64) -> String {
    if n < 1000 {
        return format!("{n} B");
    }
    let mut value = n as f64 / 1000.0;
    for unit in ["kB", "MB", "GB", "TB", "PB"] {
        if value < 999.95 {
            return format!("{value:.1} {unit}");
        }
        value /= 1000.0;
    }
    format!("{value:.1} EB")
}

fn kpis(s: &crate::stats::Stats) -> [(&'static str, String); 6] {
    [
        ("calls", compact(s.calls)),
        ("read", compact(s.files_read)),
        ("written", compact(s.files_written)),
        ("agent", s.agent_secs.map_or_else(|| "–".into(), |secs| fmt_span(chrono::Duration::seconds(secs)))),
        ("prompts", s.prompts.map_or_else(|| "–".into(), compact)),
        ("disk", s.disk.map_or_else(|| "–".into(), bytes)),
    ]
}

/// One labelled bar per `(label, count)` item, sorted by descending count; zero counts are
/// dropped. Labels are cut to `label_max` characters. Rows past `max_rows` fold into `+N more`.
pub fn bar_lines(items: &[(String, usize)], width: u16, max_rows: usize, label_max: usize) -> Vec<Line<'static>> {
    use ratatui::style::Stylize;
    use ratatui::text::Span;
    if max_rows == 0 {
        return Vec::new();
    }
    let items = &items[..items.partition_point(|&(_, count)| count > 0)];
    let Some((_, max_count)) = items.first() else {
        return vec![Line::from("no tool calls").fg(Color::DarkGray)];
    };
    let label_w = items.iter().map(|(label, _)| label.chars().count()).max().unwrap_or(0).min(label_max);
    let count_w = max_count.ilog10() as usize + 1;
    let bar_w = usize::from(width).saturating_sub(label_w + count_w + 2);
    let shown = if items.len() > max_rows { max_rows - 1 } else { items.len() };
    let mut lines = Vec::with_capacity(items.len().min(max_rows));
    for (tool, count) in &items[..shown] {
        let label = truncate(tool, label_w);
        let bar_len = (*count as u128 * bar_w as u128).div_ceil(*max_count as u128) as usize;
        lines.push(Line::from(vec![
            Span::raw(format!("{label:<label_w$} ")),
            Span::raw("█".repeat(bar_len.max(1))).fg(Color::Cyan),
            Span::raw(format!(" {count:>count_w$}")),
        ]));
    }
    if shown < items.len() {
        lines.push(Line::from(format!("+{} more", items.len() - shown)).fg(Color::DarkGray));
    }
    lines
}

fn append_chart(lines: &mut Vec<Line<'static>>, area: Rect, stats: &crate::stats::Stats) {
    lines.push(Line::from("─".repeat(area.width.into())));
    let remaining = usize::from(area.height).saturating_sub(lines.len());
    lines.extend(bar_lines(&stats.tools, area.width, remaining, 10));
}

pub fn render_stats_panel(f: &mut Frame, area: Rect, view: &StatsView) {
    use ratatui::style::Stylize;
    let title = if view.range { "Stats · range" } else { "Stats" };
    let block = Block::default().borders(Borders::ALL).title(title);
    let inner = block.inner(area);
    f.render_widget(block, area);
    let mut lines = vec![
        Line::from(truncate(view.label, inner.width.into())).bold(),
        Line::from(format!("{:<8}{:>11}{:>11}", "", "sel", "all")),
    ];
    for ((name, sel), (_, all)) in kpis(view.selected).into_iter().zip(kpis(view.all)) {
        lines.push(Line::from(format!("{name:<8}{sel:>11}{all:>11}")));
    }
    if view.share {
        let share = match view.all.calls {
            0 => "–".into(),
            all => format!("{}%", (view.selected.calls * 100).div_ceil(all).min(100)),
        };
        lines.push(Line::from(format!("{:<8}{share:>11}", "share")));
    }
    append_chart(&mut lines, inner, view.selected);
    f.render_widget(Paragraph::new(lines), inner);
}

pub fn render_stats_column(f: &mut Frame, area: Rect, label: &str, stats: &crate::stats::Stats) {
    use ratatui::style::Stylize;
    let mut lines = vec![Line::from(truncate(label, area.width.into())).bold()];
    for (name, value) in kpis(stats) {
        lines.push(Line::from(format!("{name:<8}{value:>11}")));
    }
    append_chart(&mut lines, area, stats);
    f.render_widget(Paragraph::new(lines), area);
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Duration, Local, TimeZone};
    use ratatui::buffer::Buffer;

    #[test]
    fn bytes_rounds_and_promotes_decimal_units() {
        for (n, label) in [
            (0, "0 B"),
            (999, "999 B"),
            (1000, "1.0 kB"),
            (999_949, "999.9 kB"),
            (999_950, "1.0 MB"),
            (9_208_436, "9.2 MB"),
        ] {
            assert_eq!(bytes(n), label);
        }
    }

    #[test]
    fn fold_glyph_encodes_session_count() {
        assert_eq!(fold_glyph(0, None), (' ', None));
        assert_eq!(fold_glyph(1, Some(Color::Red)), ('▄', Some(Color::Red)));
        assert_eq!(fold_glyph(2, Some(Color::Red)), ('▆', Some(Color::DarkGray)));
        assert_eq!(fold_glyph(5, None), ('█', Some(Color::DarkGray)));
    }

    #[test]
    fn strip_colors_turns_highlights_into_modifiers() {
        let mut buf = Buffer::empty(Rect::new(0, 0, 3, 1));
        buf[(0, 0)].set_bg(SELECTED_BG).set_fg(Color::Cyan);
        buf[(1, 0)].set_fg(Color::DarkGray);
        strip_colors(&mut buf);
        for cell in &buf.content {
            assert_eq!(cell.fg, Color::Reset);
            assert_eq!(cell.bg, Color::Reset);
        }
        assert!(buf[(0, 0)].modifier.contains(Modifier::REVERSED));
        assert!(buf[(1, 0)].modifier.contains(Modifier::DIM));
        assert!(buf[(2, 0)].modifier.is_empty());
    }
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
