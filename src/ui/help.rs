use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};

const KEYS: &[(&str, &str)] = &[
    ("[count]", "repeat: 5j 10l 2H 3n 2} 3Ctrl-e …"),
    ("j/k, ↓/↑", "row down/up"),
    ("gg/G", "first/last row; [count]: row N"),
    ("{/}", "prev/next section header"),
    ("Ctrl-d/Ctrl-u", "half page down/up; [count]: N rows"),
    ("Ctrl-f/Ctrl-b", "page down/up"),
    ("Ctrl-e/Ctrl-y", "scroll rows down/up by one"),
    ("zz/zt/zb", "selected row to middle/top/bottom"),
    ("h/l, ←/→", "time cursor ±1 column"),
    ("0/$", "time cursor to view start / now"),
    ("H/L", "pan by half the pane width"),
    ("+/-", "zoom in/out"),
    ("t", "cursor to now, pinned near the right edge"),
    ("f", "fit all activity, latest near the right edge"),
    ("/", "search row names (smartcase); Enter jumps to the next match"),
    ("n/N", "next/prev search match; no search: next/prev activity on the row"),
    ("Space/za", "toggle dir/session; on a file: collapse its dir"),
    ("zo/zc", "open/close; zc on a file or closed dir: its parent"),
    ("zO/zC", "open/close recursively"),
    ("zM/zR", "collapse/expand all dirs"),
    ("a", "auto-expand the focused session's files on/off"),
    ("Enter", "open Detail for the selected row"),
    ("Enter (Detail)", "full-screen diff of selected event"),
    ("T", "touched-only"),
    ("s", "session picker"),
    ("?", "this help"),
    (":q⏎", "quit"),
    ("Ctrl-C", "hint: quit with :q⏎"),
    (":/ line", "Ctrl-U clear · Ctrl-W delete word · ↑/↓ history"),
    ("q/Esc", "close overlay; Esc also cancels a count/prefix and clears the search"),
];

pub fn render(f: &mut Frame, area: Rect) {
    f.render_widget(Clear, area);
    let block = Block::default().borders(Borders::ALL).title("Help").style(Style::default().bg(Color::Black));
    let inner = block.inner(area);
    f.render_widget(block, area);
    let lines: Vec<String> = KEYS.iter().map(|(k, d)| format!("{k:<16} {d}")).collect();
    f.render_widget(Paragraph::new(lines.join("\n")).style(Style::default().fg(Color::White)), inner);
}
