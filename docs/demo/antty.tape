# antty README demo. Re-record from the repo root (needs the MesloLGS Nerd Font Mono font):
#   cargo build --release --locked && DEMO_EXCLUDE='<regex>' vhs docs/demo/antty.tape
# Replays this repo's 2026-11-22 omp sessions, minus groups matching DEMO_EXCLUDE (see copy-sessions.sh), from /tmp/antty-demo.
Output docs/demo/antty.gif

Set Shell "bash"
Set FontFamily "MesloLGS Nerd Font Mono"
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
Type@1ms "D=/tmp/antty-demo; rm -rf $D && mkdir -p $D/claude $D/codex $D/state && docs/demo/copy-sessions.sh $D/omp/p $HOME/.omp/agent/sessions/-projects-tools-antty '2026-11-22T*' && clear" Enter
Type@1ms "./target/release/antty --omp-dir $D/omp --claude-dir $D/claude --codex-dir $D/codex --state-dir $D/state" Enter
Wait+Screen@20s /Gantt Chart Timeline/
Sleep 1.5s
Show

# 1. One evening of overlapping sessions
Type "f"
Sleep 2s
Type "j"
Sleep 300ms
Type "j"
Sleep 300ms
Type "j"
Sleep 300ms
Sleep 500ms

# 2. Expand a session into its subagent lanes
Type "/Formi bundle"
Sleep 400ms
Enter
Sleep 700ms
Escape
Sleep 300ms
Space
Sleep 1.8s

# 3. Stats panel: the selected row vs all activity
Type "i"
Sleep 1.5s
Type "j"
Sleep 900ms
Type "j"
Sleep 1.2s

# 4. A file merged across Git worktrees: its events and full diff
Type "/app.rs"
Sleep 400ms
Enter
Sleep 600ms
Escape
Sleep 1.2s
Enter
Sleep 2.2s
Enter
Sleep 3.2s
Type "q"
Sleep 400ms
Type "q"
Sleep 600ms
Type "i"
Sleep 400ms

# 5. Action filter: only eval activity
Type "F"
Sleep 1s
Type "n"
Sleep 500ms
Space
Sleep 1.2s
Enter
Sleep 3s
Type "F"
Sleep 2.2s
Type "a"
Sleep 400ms
Enter
Sleep 800ms

# 6. Vim-style key help
Type "?"
Sleep 2.8s
Type "q"
Sleep 600ms

Hide
Type ":q"
Enter
Sleep 500ms
Type@1ms "rm -rf $D" Enter
Sleep 300ms
