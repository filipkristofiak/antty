# Usage

```
antty [OPTIONS]
```

## Flags

| Flag | Doc |
| --- | --- |
| `--project <PROJECT>` | Project root to show sessions for. Defaults to the current directory. |
| `--sessions-dir <SESSIONS_DIR>` | Root directory holding the agent session jsonl files (default: omp's `~/.omp/agent/sessions`). |
| `--state-dir <STATE_DIR>` | Directory used to persist watcher-observed filesystem changes (default: `$XDG_STATE_HOME/antty`, else `~/.local/state/antty`). |
| `--no-watch` | Disable the live filesystem watcher. |
| `--idle-gap <IDLE_GAP>` | Idle gap (seconds) used to merge raw activity intervals into spans. Default: 30. |

## Screen layout

Left pane, top to bottom:

- `PARTICIPANTS` — you, plus each session (and its subagents)
- `FILES` — the project's file tree
- `MOUNTS` — external/remote paths touched outside the project
- `WEB` — search queries and fetched URLs

Right pane: the Gantt timeline, one lane per participant.

Bottom: a status bar with key hints and current zoom/session/watch state.

## Overlays

- **Detail** (`Enter`) — event/span list for the selected row
- **Diff** — full-screen scrollable diff or detail of the selected event (`Enter` again from Detail)
- **Picker** (`s`) — session list, sorted by start time
- **Help** (`?`) — the key table below

## Keys

| Key | Action |
| --- | --- |
| `[count]` | repeat: 5j 10l 2H 3n 2} 3Ctrl-e … |
| `j/k, ↓/↑` | row down/up |
| `gg/G` | first/last row; [count]: row N |
| `{/}` | prev/next section header |
| `Ctrl-d/Ctrl-u` | half page down/up; [count]: N rows |
| `Ctrl-f/Ctrl-b` | page down/up |
| `Ctrl-e/Ctrl-y` | scroll rows down/up by one |
| `zz/zt/zb` | selected row to middle/top/bottom |
| `h/l, ←/→` | time cursor ±1 column |
| `0/$` | time cursor to view start / now |
| `H/L` | pan by half the pane width |
| `+/-` | zoom in/out |
| `t` | cursor to now, pinned near the right edge |
| `f` | fit all activity, latest near the right edge |
| `n/N` | next/prev bucket with activity on the selected row |
| `Space/za` | toggle dir/session; on a file: collapse its dir |
| `zo/zc` | open/close; zc on a file or closed dir: its parent |
| `zO/zC` | open/close recursively |
| `zM/zR` | collapse/expand all dirs |
| `a` | auto-expand the focused session's files on/off |
| `Enter` | open Detail for the selected row |
| `Enter (Detail)` | full-screen diff of selected event |
| `T` | touched-only |
| `s` | session picker |
| `?` | this help |
| `:q⏎` | quit |
| `q/Esc` | close overlay; Esc also cancels a count/prefix |

In Normal mode, letter keys act only when typed without Ctrl/Alt; unlisted Ctrl chords do nothing.
