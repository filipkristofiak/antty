# Usage

```
antty [OPTIONS]
```

## Flags

| Flag | Doc |
| --- | --- |
| `--project <PROJECT>` | Project root to show sessions for. Defaults to the current directory. |
| `--no-worktrees` | Show only `--project` itself, not the other Git worktrees of its repository. |
| `--separate-worktrees` | Start with a FILES group per Git worktree instead of the merged tree; toggle with `w`. |
| `--omp-dir <OMP_DIR>` | Root directory of omp's session jsonl files (default: `~/.omp/agent/sessions`). |
| `--claude-dir <CLAUDE_DIR>` | Claude Code projects directory holding its session jsonl files (default: `$CLAUDE_CONFIG_DIR/projects`, else `~/.claude/projects`). |
| `--codex-dir <CODEX_DIR>` | Codex sessions directory holding rollout jsonl files (default: `$CODEX_HOME/sessions`, else `~/.codex/sessions`). |
| `--state-dir <STATE_DIR>` | Directory used to persist watcher-observed filesystem changes and the binary session index (default: `$XDG_STATE_HOME/antty`, else `~/.local/state/antty`). |
| `--no-watch` | Disable the live filesystem watcher; FILES is listed once at startup. |
| `--no-mouse` | Disable mouse capture so the terminal handles wheel input and text selection. |
| `--no-collapse-gaps` | Keep the timeline linear instead of collapsing idle stretches into `~` breaks. |
| `--idle-gap <IDLE_GAP>` | Idle gap (seconds) used to merge raw activity intervals into spans. Default: 30. |

By default, a Git checkout includes its main and linked worktrees: sessions, file changes, and
history from each appear together. FILES merges them by repository-relative path, with `⎇`
and checkout labels on paths present in only some worktrees (not for content differences).
Press `w` to switch between the merged tree and separate checkout groups; use
`--separate-worktrees` to start in separate mode. Worktrees are discovered at startup; restart
antty after adding one. `--no-worktrees` restores the single-checkout view. Non-Git directories
retain the single-root view.

The first frame appears while sessions are indexed in the background; the status bar shows
`indexing N/M`, and keys remain available. The binary index under `<state-dir>/index/`
lets subsequent launches resume at complete-line log offsets. Changed settings, removed or
overlapping new worktrees, and deleted, truncated or rewritten log tails trigger a cold load.
Content baselines for watcher diffs are seeded only for `--project`; in other worktrees,
the first observed change has no diff unless a tool already recorded the file's contents.

## Screen layout

Left pane, top to bottom:

- `PARTICIPANTS` — you, plus each session (and its subagents), most recently active first.
  Up to 12 sessions show as rows (fewer on short terminals to leave room for 10 FILES rows).
  Sessions active in the last 2 hours always show. Older ones fold into one `⋯ N older` row
  whose timeline bars count active folded sessions (`▄` 1, `▆` 2, `█` 3+).
  Press `p`, or Enter on the fold row, to show every session.
- `FILES` — the project's file tree, merged across worktrees by default, or grouped by checkout with `w`
- `MOUNTS` — external/remote paths touched outside the project, plus inferred ssh/scp/rsync/docker/kubectl targets
- `WEB` — search queries and fetched URLs
- `MCP` — tool calls grouped by server, then tool

Right pane: the Gantt timeline, aligned with the left-pane rows.
`i` adds a stats panel on the far right (hidden below 110 columns).
Runs of six or more columns idle across all visible sessions collapse into three display columns (` ~ `) by default, with the marker centered between two blank columns. The header shows `~<duration>` when it fits; closely spaced breaks retain a bare `~` rather than a truncated duration. Press `c` to toggle collapsing without moving the time cursor.

Bottom: a status bar with key hints and current zoom/session/watch/gap-collapse state, plus the worktree view when multiple roots are shown.

The selected row is bold in the tree, Detail event list, and session picker.
Selecting a file or directory also bolds the visible participants that touched it
and their session rows, even when collapsed, with a soft background tint across the
tree row and Gantt lane. Selection and the time-cursor column take precedence over
the tint. Directories include descendant events; sessions excluded by the session
filter do not contribute. Selecting a participant does not highlight file rows.
With `NO_COLOR`, touched rows stay bold without the tint.

## Overlays

- **Detail** (`Enter`) — event/span list for the selected row
- **Diff** — full-screen scrollable diff or detail of the selected event (`Enter` again from Detail)
- **Picker** (`s`) — session list, sorted by start time
- **Actions** (`F`) — choose which action types to show
- **Stats** (`I`) — tool-call bars and totals for the selected row next to all visible activity
- **Help** (`?` from any view) — the key table below; closing it returns to the view it was opened from

Stats cover all time by default. Press `v` in the Stats popup to count only the time shown in
the Gantt timeline; the side panel uses the same setting. The panel and time-range setting
reset on launch.

In Actions, `Space` toggles the highlighted checkbox, `a` checks every action, and `n` unchecks
every action. `Enter` applies; `q` or `Esc` cancels. The separate **scripts only** checkbox narrows
file-path actions to the listed extensions without hiding evals, web searches, fetches, or MCP
calls that you checked. To see only evals, press `F`, `n`, `Space` on the eval row, then `Enter`.

An active filter hides file and directory rows without matching events. The Gantt timeline,
Detail, `n`/`N` activity jumps, idle gaps, and `f` fit use only matching events. The status bar
lists the selection. To clear it, check all actions with `a`, turn off **scripts only** if checked,
and apply. The filter is not saved between launches.

Detail and full-screen Diff show the command/code behind watcher events attributed to
a tool-call window, and JSON arguments for MCP calls. The watcher state log retains
up to 4 KiB of command text (plus a truncation marker); live transcripts take precedence.

Inferred MOUNTS events are labelled `inferred` and show the shell command that named
the target. A host-only target has a `$` leaf. Remote effects are not observed:
these entries record intent, even if the command failed. Copy sources are reads and
destinations writes; ssh/docker/kubectl exec targets are read marks with unknown effects.
Quoted text and heredoc bodies are not treated as commands. Shell `-c` wrappers are
parsed only to a bounded depth; interactive stdin and subprocess/script indirection
are not traced.

## Keys

### Rows

| Key | Action |
| --- | --- |
| `[count]` | repeat: 5j 10l 2H 3n 2} 3Ctrl-e … |
| `j/k, ↓/↑` | row down/up |
| `gg/G` | first/last row; [count]: row N |
| `{/}` | prev/next section header |
| `Ctrl-d/Ctrl-u` | half page down/up; [count]: N rows |
| `Ctrl-f/Ctrl-b` | page down/up |
| `Ctrl-e/Ctrl-y` | scroll rows down/up by one |
| Wheel | scroll the viewport under the pointer by three rows per wheel event; keep selection |
| `zz/zt/zb` | selected row to middle/top/bottom |

Mouse capture is on by default. The tree and Gantt share a vertical viewport, so
scrolling either moves both. The selected row can go offscreen; a row-navigation
key brings its destination into view. The wheel also scrolls the Detail text,
full-screen diff, and Help when the pointer is over them.
In Picker, Actions, and the Stats popup, each wheel event acts like one
Up/Down keypress instead, moving the cursor or the underlying row selection.

For terminal text selection, use your terminal's bypass modifier (typically
Shift or Option while dragging), or start `antty --no-mouse`. With capture off,
wheel behavior depends on your terminal or multiplexer; arrows sent by the
terminal still move the selection.

### Timeline

| Key | Action |
| --- | --- |
| `h/l, ←/→` | time cursor ±1 column |
| `0/$` | time cursor to view start / now |
| `H/L` | pan by half the pane width |
| `+/-` | zoom in/out |
| `t` | cursor to now, pinned near the right edge |
| `f` | fit all activity, latest near the right edge |
| `c` | collapse idle stretches into ` ~ ` breaks on/off |

### Search

| Key | Action |
| --- | --- |
| `/` | search row names (smartcase); Enter jumps to the next match |
| `n/N` | next/prev search match; no search: next/prev activity on the row |

### Tree

| Key | Action |
| --- | --- |
| `Space/za` | toggle dir/session; on a file: collapse its dir |
| `zo/zc` | open/close; zc on a file or closed dir: its parent |
| `zO/zC` | open/close recursively |
| `zM/zR` | collapse/expand all dirs |
| `a` | auto-expand the focused session's files on/off |
| `T` | touched-only |
| `p` | fold older sessions into one row on/off; Enter on the fold row shows all |
| `w` | worktrees: merged/separate FILES view |

### Detail & diffs

| Key | Action |
| --- | --- |
| `Enter` | open Detail for the selected row |
| `Enter (Detail)` | full-screen diff of selected event |
| `e` | open file in `$EDITOR`, or the parent Neovim when `$NVIM` is set (file rows, Detail, Diff) |
| `D` | diff of selected event in delta (Detail, Diff); needs delta on PATH |

### Pickers & filters

| Key | Action |
| --- | --- |
| `s` | session picker |
| `F` | action filter: show only chosen actions; script-files toggle |

### Stats

| Key | Action |
| --- | --- |
| `i` | stats panel on/off: tool calls and totals, selection vs all |
| `I` | stats popup; j/k move the selection underneath |
| `v (Stats)` | count only the Gantt's visible time range on/off |

### General

| Key | Action |
| --- | --- |
| `?` | this help, from any view; closing returns there. In help: j/k, Ctrl-d/Ctrl-u, g/G scroll |
| `:q⏎, ZZ/ZQ` | quit |
| `Ctrl-C` | hint: quit with :q⏎ |
| `Ctrl-z` | suspend to the shell; fg resumes |
| `:/ line` | Ctrl-U clear · Ctrl-W delete word · ↑/↓ history |
| `q/Esc` | close overlay; Esc also cancels a count/prefix and clears the search |

In Normal mode, letter keys act only when typed without Ctrl/Alt; unlisted Ctrl chords do nothing.

With an action filter (`F`) active, PARTICIPANTS keeps `you` and sessions with at least one matching action from the main agent, a subagent, or an advisor. Expanded sessions hide subagents and advisors with no matching actions.

`$EDITOR` is split on whitespace; antty adds `+<line>` when known and suspends the TUI until it exits. When `$NVIM` is non-empty, antty asks that Neovim instance to open the file instead. Unset `$EDITOR` without `$NVIM` shows an error. `NO_COLOR` (non-empty) disables foreground/background colours, retaining selection as reverse video.

`D` pipes the selected event's diff to `delta --paging always` and suspends antty until delta's pager exits. delta chooses its pager from `DELTA_PAGER`, then `PAGER`, then `less`. After the pager exits, press any key to return to antty; this keeps short diffs visible even when the pager quits automatically. delta is optional; if it is not on PATH, `D` shows an error. omp edit diffs are converted from their recorded numbered lines, including when full before/after file contents are unavailable. Events without a unified diff show an error.

## Differences from vim

- `-` zooms out (paired with `+`); it does not open the parent directory as in oil.nvim/vim-vinegar. On a file, `zc` closes and selects its parent dir.
- `H`/`L` pan the timeline by half the pane width; they do not jump to the top/bottom visible row. Use `zt`/`zb` to place the selected row instead.
- `c` toggles gap collapsing; it is not vim's change operator.

## Queen

`queen [--omp-dir <PATH>] [--claude-dir <PATH>] [--codex-dir <PATH>] [--list]`
shows all discovered projects. Harness-root flags and defaults match antty;
`--list` prints per-project, per-cwd and total session counts instead of the TUI.

Projects start collapsed, newest activity first. `o` toggles an alphabetical
path tree; expansion keys are shared between both orders. `▓` bars are merged
project/directory spans; `▒` bars are not-yet-indexed session spans. Session
colours identify the harness. Missing projects are dimmed. `NO_COLOR` works
as in antty.

An expanded project shows the five most recent sessions and any others active
in the last two hours. The `⋯ N older` row summarizes the rest; its bars count
active folded sessions (`▄` 1, `▆` 2, `█` 3+).
Recent order shows the twelve most recently active projects and any others
active in the last two hours, with older projects summarized in a
`⋯ N older projects` row. Tree order shows all projects.
Fold counts and their timeline strips use the same two-hour cutoff as the
displayed rows, refreshed when those rows are rebuilt.

| Key | Queen action |
| --- | --- |
| `[count]j/k`, `↓/↑`, `gg/G`, `Ctrl-d/u` | Navigate rows; counts select/repeat as in antty |
| `[count]h/l`, `←/→`, `H/L` | Move the time cursor / half-pane pan |
| `[count]+/-`, `=`, `t`, `f`, `c` | Zoom, now, fit, toggle collapsed gaps |
| `o` | Toggle recent/path-tree order |
| `p` | Toggle folding older sessions and projects |
| `?` | Open help; scroll with `j/k`, `Ctrl-d/u`, or `g/G`; close with `q`, `?`, or `Esc` |
| `Space/za` | Toggle node; on a session, close and select its project; on a fold row, reveal hidden rows |
| `zo/zc`, `zR/zM` | Open/close node; expand/collapse all |
| `Enter` | Open session cwd, or project's most recently active existing cwd, in antty; directory-only nodes toggle |
| `Enter`, `zo` on a fold row | Reveal hidden rows and select the first one |
| `/pattern⏎` | Find full project paths in either order and session titles; uppercase makes the search case-sensitive. Opens any folds hiding the match. |
| `[count]n/N`, `Esc` | Next/previous match (wraps); clear the active search |
| `:q⏎`, `:q!`, `:qa`, `:qa!`, `:quit`, `ZZ/ZQ` | Quit (colon commands require Enter) |
| `Ctrl-c`, `Ctrl-z` | Quit hint / suspend to shell |
| `Esc`, `Ctrl-c`, `Backspace` in `:` or `/` | Cancel the line / erase a character (cancel if already empty) |
| `Ctrl-U`, `Ctrl-W`, `↑/↓` in `:` or `/` | Clear the line, delete a word, or browse the separate histories |

Returning from antty restores queen's selection and view. A cwd missing at
discovery shows an error instead of launching. The companion antty binary is
resolved beside queen first, then on PATH. Session discovery and header/tail
metadata are a startup snapshot; no background indexing, activity cache or live
updates ship yet.
