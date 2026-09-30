# antty README demo. Re-record from the repo root after `cargo build --release`:
#   vhs docs/demo.tape
# Replays this repo's own 2026-09-26 omp sessions from a throwaway copy in /tmp/antty-demo.
Output docs/demo.gif

Set Shell "bash"
Set FontFamily "Menlo"
Set FontSize 14
Set Width 1280
Set Height 760
Set Padding 12
Set Theme "Catppuccin Mocha"
Set TypingSpeed 90ms
Set Framerate 24
Set CursorBlink false
Env TZ "Europe/Prague"
Env NO_COLOR ""

Hide
Type@1ms "S=$HOME/.omp/agent/sessions/-projects-tools-antty D=/tmp/antty-demo; rm -rf $D && mkdir -p $D/omp/p $D/claude $D/state && cp -R $S/2026-09-26T* $D/omp/p/ && clear" Enter
Type@1ms "./target/release/antty --omp-dir $D/omp --claude-dir $D/claude --state-dir $D/state" Enter
Wait+Screen@20s /Gantt Chart Timeline/
Sleep 1s
Show

# 1. Overview: overlapping session lanes, fitted to the day
Sleep 2.5s
Type "j"
Sleep 250ms
Type "j"
Sleep 250ms
Type "j"
Sleep 250ms
Type "j"
Sleep 600ms

# Widen briefly to show the Sep 26 day header, then fit to the activity.
Type "-"
Sleep 350ms
Type "-"
Sleep 350ms
Type "-"
Sleep 1.4s
Type "f"
Sleep 500ms

# 2. Search a session, expand it into subagent lanes
Type "gg"
Sleep 400ms
Type "/Gantt dashboard"
Sleep 400ms
Enter
Sleep 900ms
Escape
Sleep 300ms
Space
Sleep 2s

# 3. Zoom into concurrent subagent work, then fit back
Type "j"
Sleep 400ms
Type "N"
Sleep 700ms
Type "+"
Sleep 700ms
Type "+"
Sleep 700ms
Type "+"
Sleep 1.8s
Type "f"
Sleep 900ms
Type "k"
Sleep 300ms
Space
Sleep 700ms

# 4. Jump to a file row: per-file activity across sessions
Type "/main.rs"
Sleep 400ms
Enter
Sleep 600ms
Escape
Sleep 1.2s

# 5. Detail list with inline diff preview, then the full-screen diff
Enter
Sleep 1.2s
Type "j"
Sleep 400ms
Type "j"
Sleep 400ms
Type "j"
Sleep 400ms
Sleep 1.8s
Enter
Sleep 3s
Type "q"
Sleep 400ms
Type "q"
Sleep 700ms

# 6. Touched-only filter hides files no session touched
Type "T"
Sleep 2s

# 7. Session picker: hide two sessions from the timeline
Type "gg"
Sleep 700ms
Type "s"
Sleep 1.3s
Space
Sleep 400ms
Type "j"
Sleep 300ms
Space
Sleep 1.5s
Enter
Sleep 2s

# 8. Vim-style key help
Type "?"
Sleep 2.2s
Type "q"
Sleep 800ms

Hide
Type ":q"
Enter
Sleep 500ms
Type@1ms "rm -rf $D" Enter
Sleep 300ms
