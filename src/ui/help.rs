use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};

const KEYS: &[(&str, &str)] = &[
    ("j/k, ↓/↑", "row down/up"),
    ("g/G", "first/last row"),
    ("Ctrl-d/Ctrl-u", "half page down/up"),
    ("h/l, ←/→", "time cursor ±1 column"),
    ("H/L", "pan by half the pane width"),
    ("+/-", "zoom in/out"),
    ("t", "cursor to now"),
    ("f", "fit"),
    ("n/N", "next/prev bucket with activity on the selected row"),
    ("Space/za", "toggle dir/session; on a file: collapse its dir"),
    ("zM/zR", "collapse/expand all dirs"),
    ("a", "auto-expand the focused session's files on/off"),
    ("Enter", "open Detail for the selected row"),
    ("Enter (Detail)", "full-screen diff of selected event"),
    ("T", "touched-only"),
    ("s", "session picker"),
    ("?", "this help"),
    (":q⏎", "quit"),
    ("q/Esc", "close overlay"),
];

pub fn render(f: &mut Frame, area: Rect) {
    f.render_widget(Clear, area);
    let block = Block::default().borders(Borders::ALL).title("Help").style(Style::default().bg(Color::Black));
    let inner = block.inner(area);
    f.render_widget(block, area);
    let lines: Vec<String> = KEYS.iter().map(|(k, d)| format!("{k:<16} {d}")).collect();
    f.render_widget(Paragraph::new(lines.join("\n")).style(Style::default().fg(Color::White)), inner);
}
