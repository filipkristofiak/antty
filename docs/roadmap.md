# Roadmap

antty borrows vim/Neovim muscle memory; it does not integrate with the Neovim
ecosystem (no editor hand-off, plugin, or remote API). The goal is that a
vim user's reflexes either do the expected thing or nothing — never something
different.

Priority order: a key that does the *wrong* thing (false friend) is worse than
a key that does nothing (dead key), which is worse than a missing nicety.

## 1. False friends: modifier guard in Normal mode

`handle_key_normal` (`src/main.rs`) only checks modifiers on `Ctrl-d`/`Ctrl-u`;
every other Ctrl chord runs the bare letter's action.

| Reflex | Vim meaning | antty today |
| --- | --- | --- |
| `Ctrl-f` | page down | fit — discards the current zoom |
| `Ctrl-z` | suspend | arms the `z` prefix, swallowing the next key |
| `Ctrl-n` | line down | next activity (`Ctrl-p` is dead) |
| `Ctrl-a` | increment | toggles auto-follow |
| `Ctrl-l` | redraw | time cursor right |
| `Ctrl-g` | file info | first row |
| `Ctrl-t` / `Ctrl-s` | tag pop / — | cursor to now / session picker |

- [x] Plain-letter bindings match only with no modifier other than Shift.
- [x] `Ctrl-f`/`Ctrl-b`: page down/up; `Ctrl-e`/`Ctrl-y`: scroll one row.
- [x] Unbound Ctrl chords are no-ops.

## 2. Consistent overlays

- [x] Picker closes on `q` (today only `Esc`, `handle_key_picker`), matching
  Detail/Diff/Help.
- [x] Picker: `g`/`G`, `Ctrl-d`/`Ctrl-u`.
- [x] Detail list: `g`/`G`.

## 3. Vim grammar

- [x] Counts: `5j`, `10l`, `3n`, etc.
- [x] Folds: `zo`/`zc`/`zO`/`zC` alongside `za`/`zM`/`zR`.
- [x] Time-axis line motions: `0` to view start, `$` to now.
- [x] Section motions: `{`/`}` between `PARTICIPANTS`/`FILES`/`MOUNTS`/`WEB`.
- [x] `zz`/`zt`/`zb`: scroll the selected row to middle/top/bottom.
- [x] `gg` as first row; frees a single `g` as a prefix.

## 4. Search

- [x] `/` searches every tree row by name and reveals collapsed matches; `n`/`N`
  cycle matches while a search is active. `Esc` clears it; otherwise `n`/`N`
  jump to activity on the selected row.

## 5. Feedback

- [x] Show a pending prefix (`z`, `g`, counts) in the status bar, like
  `showcmd`.
- [x] `Ctrl-C` in Normal mode flashes `Type :q and press <Enter> to exit`,
  mirroring Neovim. `q` stays a no-op at top level.
- [x] `:` line: `Ctrl-U` clears, `Ctrl-W` deletes a word; `↑`/`↓` history.

## 6. Minor mapping clashes

Keep, but document in `docs/usage.md`:

- `-` zooms out; oil.nvim/vim-vinegar users expect "parent directory".
- `H`/`L` pan the timeline; vim uses them for screen top/bottom.

## 7. Bugs

- [x] 15-minute zoom tick row reads `5050…`: `render_header`
  (`src/ui/gantt_pane.rs`) prints the minute's last digit, which only alternates
  0/5 at 15m columns. Same class as the already-handled 10m case.

## 8. Linux runtime

- [x] Watcher failure is silent and the status bar still reports `watch:on`:
  `spawn_watcher` (`src/watch.rs`) drops errors via `.ok()?`, and `main`
  sets watch state from `!args.no_watch`, not from the watcher result. Surface
  the error; report the real state.
- [x] inotify watch budget: `RecursiveMode::Recursive` adds a watch per
  directory, including gitignored `target/`/`node_modules/`; `.gitignore` is
  applied only after events arrive. Large repos can exhaust
  `fs.inotify.max_user_watches`. Watch non-ignored directories only.
- [x] Default `--state-dir` honors `$XDG_STATE_HOME` before
  `~/.local/state/antty` (`src/cli.rs`); don't fall back to a relative path
  when `HOME` is unset.
- [x] `run_loop` redraws every 200 ms even when idle; draw only on input,
  messages, resize, or a clock-column change.
