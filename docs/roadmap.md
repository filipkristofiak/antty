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

## 2. External diff tools

- [x] Hand the diff to delta (`D` in Detail/Diff).
- [ ] Hand the diff to `$PAGER` / difftastic instead of the built-in renderer.
- [ ] Open `nvim -d` on the before/after snapshots.

## 3. Keys

- [ ] `ZZ` / `ZQ` quit. `Ctrl-C` keeps flashing
  `Type :q and press <Enter> to exit`.
- [ ] `Ctrl-z` suspends to the shell and restores the TUI on resume.

## 4. Packaging

- [ ] Publish to crates.io.
- [ ] CI: build and test on Linux and macOS.
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
