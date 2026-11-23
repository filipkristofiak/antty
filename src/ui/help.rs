pub const SECTIONS: antty::tui::HelpSections = &[
    (
        "Rows",
        &[
            ("[count]", "repeat: 5j 10l 2H 3n 2} 3Ctrl-e …"),
            ("j/k, ↓/↑", "row down/up"),
            ("gg/G", "first/last row; [count]: row N"),
            ("{/}", "prev/next section header"),
            ("Ctrl-d/Ctrl-u", "half page down/up; [count]: N rows"),
            ("Ctrl-f/Ctrl-b", "page down/up"),
            ("Ctrl-e/Ctrl-y", "scroll rows down/up by one"),
            ("Wheel", "scroll hovered viewport by 3 rows; keep selection"),
            ("zz/zt/zb", "selected row to middle/top/bottom"),
        ],
    ),
    (
        "Timeline",
        &[
            ("h/l, ←/→", "time cursor ±1 column"),
            ("0/$", "time cursor to view start / now"),
            ("H/L", "pan by half the pane width"),
            ("+/-", "zoom in/out"),
            ("t", "cursor to now, pinned near the right edge"),
            ("f", "fit all activity, latest near the right edge"),
            ("c", "collapse idle stretches into ' ~ ' breaks on/off"),
        ],
    ),
    (
        "Search",
        &[
            ("/", "search row names (smartcase); Enter jumps to the next match"),
            ("n/N", "next/prev search match; no search: next/prev activity on the row"),
        ],
    ),
    (
        "Tree",
        &[
            ("Space/za", "toggle dir/session; on a file: collapse its dir"),
            ("zo/zc", "open/close; zc on a file or closed dir: its parent"),
            ("zO/zC", "open/close recursively"),
            ("zM/zR", "collapse/expand all dirs"),
            ("a", "auto-expand the focused session's files on/off"),
            ("T", "touched-only"),
            ("p", "fold older sessions into one row on/off; Enter on the fold row shows all"),
            ("w", "worktrees: merged/separate FILES view"),
        ],
    ),
    (
        "Detail & diffs",
        &[
            ("Enter", "open Detail for the selected row"),
            ("Enter (Detail)", "full-screen diff of selected event"),
            ("e", "open file in $EDITOR, or the parent Neovim when $NVIM is set (file rows, Detail, Diff)"),
            ("D", "diff of selected event in delta (Detail, Diff); needs delta on PATH"),
        ],
    ),
    (
        "Pickers & filters",
        &[("s", "session picker"), ("F", "action filter: show only chosen actions; script-files toggle")],
    ),
    (
        "Stats",
        &[
            ("i", "stats panel on/off: tool calls and totals, selection vs all"),
            ("I", "stats popup; j/k move the selection underneath"),
            ("v (Stats)", "count only the Gantt's visible time range on/off"),
        ],
    ),
    (
        "General",
        &[
            ("?", "this help, from any view; closing returns there. In help: j/k, Ctrl-d/Ctrl-u, g/G scroll"),
            (":q⏎, ZZ/ZQ", "quit"),
            ("Ctrl-C", "hint: quit with :q⏎"),
            ("Ctrl-z", "suspend to the shell; fg resumes"),
            (":/ line", "Ctrl-U clear · Ctrl-W delete word · ↑/↓ history"),
            ("q/Esc", "close overlay; Esc also cancels a count/prefix and clears the search"),
        ],
    ),
];
