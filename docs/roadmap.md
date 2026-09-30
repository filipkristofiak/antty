# Roadmap

antty borrows vim/Neovim muscle memory: a vim user's reflexes should either do
the expected thing or nothing — never something different. It should also fit
the tools those users already run: their agent harness, their editor, their
terminal theme and their package manager.

For keys: a key that does the *wrong* thing (false friend) is worse than a key
that does nothing (dead key), which is worse than a missing nicety.

## 1. More harnesses

omp and Claude Code logs are read today. Many Neovim users run Codex CLI,
aider, avante.nvim or codecompanion.nvim, so antty shows them nothing.

- [x] Read Claude Code session logs (`$CLAUDE_CONFIG_DIR/projects`, else
  `~/.claude/projects`) alongside omp's: sessions, subagents, titles, prompts,
  file reads/writes/edits, web fetch/search, bash windows.
- [x] `--sessions-dir` becomes `--omp-dir`; add `--claude-dir`.
- [ ] Codex CLI, aider, avante.nvim, codecompanion.nvim readers.

## 2. Collapsible timeline gaps

- [ ] Collapse runs of 6 or more columns idle across every row into a clearly
  marked break (for example `~`) showing the skipped duration. Decide whether
  this is opt-in or the default. The current linear time-to-column mapping
  needs a piecewise replacement: cursor movement across the break, `t`/`f`
  jumps, panning, zoom and time labels must stay accurate and not imply
  adjacent events were simultaneous.

## 3. Public release polish

Each item is one commit.

- [ ] Docs: add a "Why / design principles" section to the README: the
  problem (seeing what agents touched, and when), the false-friend rule
  above, reading session logs without talking to the agent, and a watcher
  that guesses from timing and says so. Note that `docs/demo.tape` replays
  the author's own omp sessions, so it can't be re-recorded as-is. Add
  `src/editor.rs` to the `AGENTS.md` module map.
- [ ] Formatting, lints and CI: run `cargo fmt` once (add a `rustfmt.toml`
  first if longer lines are preferred) and fix the two clippy warnings
  (`let...else` → `?`, `sort_by` → `sort_by_key`). Then add a GitHub Actions
  workflow on Linux and macOS running build, test, `cargo fmt --check` and
  `cargo clippy --all-targets -- -D warnings`, so both stay clean.
- [ ] Move key handling out of `src/main.rs` (1,207 lines) into its own
  module, so the vim grammar (counts, prefixes, modifier guard) reads in one
  place. Do it after the formatting commit to keep the diff reviewable.

## 4. Packaging

- [ ] Publish to crates.io, after filling in `Cargo.toml`: `description`
  (required), `repository`, `readme`, `keywords`, `categories` and
  `rust-version = "1.88"`.
- [ ] Release binaries.
- [ ] Shell completions (`clap_complete`) and a man page.
- [ ] AUR package and Nix flake.

## 5. Git branches and file moves (major)

FILES already lists more than the files on disk: it is a disk walk plus
every project file an event touched, and files no longer on disk show as
deleted. What's missing:

- **Moves aren't linked.** A renamed or moved file shows as two unrelated
  rows (old path, deleted; new path), each holding part of its history.
- **No branch awareness.** Events from another git branch mix into the
  current tree and show as deleted files. Content snapshots are keyed only by
  path, so diffs can compare contents from different branches.
- **No "as of time T" view.** The tree always has today's shape; you can't
  see the tree as it was at the cursor time.

How to show past files is not decided yet. Whatever we choose must be opt-in
and leave the default view unchanged.

- [ ] Choose a strategy. Open options:
  - follow renames (`EventDetail::Moved`, watcher rename pairs, git rename
    detection) so a file keeps one row on its current path;
  - record the active branch/HEAD per session and event, then filter, group
    or key snapshots by branch;
  - rebuild the tree as of the cursor time from events, snapshots or git;
  - handle a checkout that changes the tree under a live session.

## 6. More mouse support (consideration)

- [ ] Consider horizontal timeline scrolling with a carefully chosen modifier,
  plus context-sensitive right-click/back-button actions for Enter, q, and
  Space toggles. Decide the exact gesture mappings only after checking for
  terminal conflicts; no new mouse bindings are committed yet.

## Done

- Modifier guard: plain-letter bindings ignore Ctrl chords; `Ctrl-f`/`Ctrl-b`/
  `Ctrl-e`/`Ctrl-y` scroll; unbound Ctrl chords are no-ops.
- Consistent overlays: picker closes on `q`; `g`/`G` and `Ctrl-d`/`Ctrl-u` in
  the picker, `g`/`G` in Detail.
- Vim grammar: counts, `zo`/`zc`/`zO`/`zC`/`za`/`zM`/`zR`, `0`/`$`, `{`/`}`,
  `zz`/`zt`/`zb`, `gg`.
- Search: `/` over every tree row, revealing collapsed matches; `n`/`N` cycle.
- Feedback: pending prefix in the status bar, `Ctrl-C` hint, `:` line
  `Ctrl-U`/`Ctrl-W`/history.
- Mapping clashes (`-`, `H`/`L`) documented in `docs/usage.md`.
- 15-minute zoom tick digits.
- Linux runtime: watcher errors surfaced, only non-ignored dirs watched,
  `$XDG_STATE_HOME` honored, no idle redraws.
- Open in editor: `e` opens file, Detail and Diff event rows at the first
  changed line when known; uses the parent Neovim via `$NVIM` or suspends for
  `$EDITOR` and restores the TUI afterwards.
- Colours and themes: help, picker and status use the terminal's default
  background; highlights remain readable on light themes; the participant
  palette avoids white; `NO_COLOR` is honored.
- External diffs: `D` in Detail/Diff pipes the selected event's diff to
  `delta --paging always`; delta uses its configured pager.
- Keys: `ZZ`/`ZQ` quit; `Ctrl-z` suspends and `fg` restores the TUI; `?` opens help from any view and returns there.
