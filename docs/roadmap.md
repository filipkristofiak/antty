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

## 2. Open in editor

- [ ] A key on file rows and on Detail/Diff event rows opens the file (at the
  first changed line when known).
- [ ] Inside Neovim's `:terminal` (`$NVIM` set):
  `nvim --server "$NVIM" --remote` opens it in the parent Neovim.
- [ ] Otherwise `$EDITOR +<line> <file>`, suspending the TUI until it exits.

## 3. External diff tools

- [ ] Hand the diff to `$PAGER` / delta / difftastic instead of the built-in
  renderer.
- [ ] Open `nvim -d` on the before/after snapshots.

## 4. Keys

- [ ] `ZZ` / `ZQ` quit. `Ctrl-C` keeps flashing
  `Type :q and press <Enter> to exit`.
- [ ] `Ctrl-z` suspends to the shell and restores the TUI on resume.

## 5. Colours and themes

- [ ] Help, picker and status bars use the terminal's default background, not
  `Color::Black` (`src/ui/help.rs`, `src/ui/picker.rs`, `src/ui/status.rs`).
- [ ] The selection (`Rgb(50,70,130)`) and cursor-column (`Rgb(40,40,60)`)
  colours stay readable on light themes (`tree_pane.rs`, `gantt_pane.rs`,
  `detail.rs`, `picker.rs`).
- [ ] Drop `Color::White` from the participant lane palette (`src/ui/mod.rs`);
  it vanishes on light backgrounds.
- [ ] Honor `NO_COLOR`.

## 6. Packaging

- [ ] Publish to crates.io.
- [ ] CI: build and test on Linux and macOS.
- [ ] Release binaries.
- [ ] Shell completions (`clap_complete`) and a man page.
- [ ] AUR package and Nix flake.

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
