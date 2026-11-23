# formi README demo. Re-record from the repo root (needs the MesloLGS Nerd Font Mono font):
#   cargo build --release --locked && DEMO_EXCLUDE='<regex>' vhs docs/demo/formi.tape
# Replays this repo's and hlit's omp sessions, minus groups matching DEMO_EXCLUDE (see copy-sessions.sh), from /tmp/formi-demo; Enter opens the project in antty.
Output docs/demo/formi.gif

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
Type@1ms "D=/tmp/formi-demo; rm -rf $D && mkdir -p $D/claude $D/codex $D/state && for p in antty antty-queen hlit; do docs/demo/copy-sessions.sh $D/omp/$p $HOME/.omp/agent/sessions/-projects-tools-$p; done && clear" Enter
Type@1ms "XDG_STATE_HOME=$D/state ./target/release/formi --omp-dir $D/omp --claude-dir $D/claude --codex-dir $D/codex" Enter
Wait+Screen@30s /indexed \d+/
Sleep 1s
Show

# 1. Every project with agent sessions, newest first
Type "f"
Sleep 3.5s

# 2. Expand a project: recent sessions, older ones folded
Space
Sleep 2.5s
Type "j"
Sleep 300ms
Type "j"
Sleep 300ms
Type "j"
Sleep 300ms
Sleep 1.25s

# 3. Stats panel for a session, then the whole project
Type "i"
Sleep 3s
Type "gg"
Sleep 2.5s

# 4. Dashboard: sessions per harness, tool mix, projects by calls
Type "I"
Sleep 4.5s
Type "q"
Sleep 600ms
Type "i"
Sleep 400ms

# 5. Enter opens the project in antty; :q returns to formi
Enter
Wait+Screen@30s /Gantt Chart Timeline/
Sleep 4.5s
Type ":q"
Sleep 300ms
Enter
Sleep 2.8s

Hide
Type ":q"
Enter
Sleep 500ms
Type@1ms "rm -rf $D" Enter
Sleep 300ms
