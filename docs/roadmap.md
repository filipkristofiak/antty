# Roadmap

antty borrows vim/Neovim muscle memory: a vim user's reflexes should either do
the expected thing or nothing — never something different. It should also fit
the tools those users already run: their agent harness, their editor, their
terminal theme and their package manager.

For keys: a key that does the *wrong* thing (false friend) is worse than a key
that does nothing (dead key), which is worse than a missing nicety.

## 1. More harnesses

omp, Claude Code and Codex CLI logs are read today. Readers for aider,
avante.nvim and codecompanion.nvim remain.

- [x] Read Claude Code session logs (`$CLAUDE_CONFIG_DIR/projects`, else
  `~/.claude/projects`) alongside omp's: sessions, subagents, titles, prompts,
  file reads/writes/edits, web fetch/search, bash windows.
- [x] `--sessions-dir` becomes `--omp-dir`; add `--claude-dir`.
- [x] Read Codex CLI rollouts (`$CODEX_HOME/sessions`, else `~/.codex/sessions`)
  alongside omp and Claude Code; add `--codex-dir`.
- [ ] aider, avante.nvim, codecompanion.nvim readers.

## 2. Packaging

- [ ] Publish to crates.io, after filling in `Cargo.toml`: `description`
  (required), `repository`, `readme`, `keywords`, `categories` and
  `rust-version = "1.88"`.
- [ ] Release binaries.
- [ ] Shell completions (`clap_complete`) and a man page.
- [ ] AUR package and Nix flake.

## 3. Git branches and file moves (major)

FILES combines current disk listings with every project path an event touched.
Worktrees are already discovered and shown as merged paths or separate checkout
groups, but a checkout's branch and tree shape can change during a session.
Current gaps:

- **Moves aren't linked.** Old and new paths hold separate histories.
- **No branch/state awareness.** Snapshots are isolated per checkout, but keyed
  by path and time within it. Switching branches can mix content baselines and
  make another branch's paths look deleted.
- **Merged paths lose structural differences.** Presence in several worktrees
  does not mean equal content or type: a file in one can be a directory in another.
- **No historical shape.** The tree includes today's disk shape rather than
  the shape an agent actually worked with.

Direction chosen: **context-safe snapshots → context-scoped tree → drift
comparison → historical tree**. These share one recorded checkout-state model,
not separate Git integrations. New views are opt-in and leave the default
presentation unchanged; snapshot and attribution correctness apply in every view.
Implementation remains pending.

### 3.1 Record context and protect snapshot boundaries

- [ ] Record checkout-state intervals: checkout identity, observed branch/ref,
  HEAD commit, observation times and evidence/coverage. Events reference their
  context; a session can span several contexts. Branch names are labels, not
  identities: refs move, branches are renamed and detached HEAD exists.
- [ ] Scope mutable snapshots by checkout, context interval, path and time;
  share immutable Git content by object ID. After a transition, re-establish
  the baseline or explicitly carry a known one, never reuse same-path content
  from an unrelated state.
- [ ] Observe worktree-specific Git metadata and shared refs; reconcile HEAD,
  index and working-tree changes off-thread. Represent Git transitions
  separately from editing activity. Do not coalesce across context boundaries
  or blanket-drop nearby writes: concurrent edits and delayed watcher delivery
  require uncertain attribution to remain visible.
- [ ] Keep older events in an unknown context unless historical evidence
  supports reconstruction. Never assign today's branch retroactively.
  Reflogs cannot recover unobserved dirty files or guarantee complete history.

### 3.2 Add an opt-in context-scoped tree

- [ ] Select checkout/branch context independently of `w`, which keeps its
  merged/separate-worktree meaning. Distinguish tree source (working tree or
  pinned committed tree) from activity scope (where events were observed).
- [ ] Apply activity scope consistently to the tree, Gantt, Detail and stats.
  Branch scope means recorded event context, not commit reachability: merging
  a branch must not relabel where its activity happened.
- [ ] Keep excluded and unknown-context activity reachable, with a visible
  scope/reset control. A path absent from the selected context is not evidence
  of deletion.

### 3.3 Explain drift with a pinned comparison and move links

- [ ] Add a comparison overlay with two explicit endpoints: refs resolved to
  pinned commit IDs, or an explicitly captured working-tree snapshot. Inspect
  other branches without checking them out.
- [ ] Show only-A/only-B paths, content changes, type changes and likely renames,
  with links to recorded participant activity. Preserve typed path variants
  instead of flattening file/directory differences into one node.
- [ ] Link moves with provenance (`EventDetail::Moved`, watcher rename pairs,
  Git similarity detection). Keep original paths and offer follow-move history;
  do not force divergent renames, copies or splits into one global current path.
  Git rename detection is a heuristic, not permanent file identity.
- [ ] If comparing both sides against a merge-base, label shared changes as
  overlap, not proof of a merge conflict.

### 3.4 Project a coverage-aware tree at cursor time

- [ ] Add an explicit historical mode using the checkout's recorded HEAD at T,
  not the latest commit dated before T. Reconstruct its committed tree plus
  observed uncommitted changes, and limit file activity to T.
- [ ] Show coverage separately: committed tree exact when available;
  uncommitted overlay partial or unknown where observations are missing.
  Never silently substitute today's disk for missing historical content.
- [ ] Keep historical content read-only; opening its live counterpart for
  editing is a separate action. Preserve selection through known move links
  and show checkout transitions on a context lane.
- [ ] Leave ordinary `h/l` navigation stable: tree shape follows the cursor
  only inside historical mode, not whenever the normal timeline moves.

Use Git tree queries without mutating the checkout; cache listings by tree
object ID and load blobs/rename comparisons on demand. No Git subprocesses in
rendering. Branch management is out of scope.

## 4. More mouse support (consideration)

- [ ] Consider horizontal timeline scrolling with a carefully chosen modifier,
  plus context-sensitive right-click/back-button actions for Enter, q, and
  Space toggles. Decide the exact gesture mappings only after checking for
  terminal conflicts; no new mouse bindings are committed yet.

## 5. How files were touched

Every event is tied to a harness read/write/edit or to a watcher change
inside a shell window. Most of the "how" lives in what the agent ran.

- [x] Command provenance: watcher writes attributed to a tool call show that
  call's command/code in Detail and full-screen Diff; the text is persisted
  (capped at 4 KiB) in the state log so it survives transcript loss.
- [x] Attribution windows for every file-mutating tool, not only bash/eval:
  omp `xd://` device writes (`ast_edit`/`resolve`/`lsp`) and MCP tools in all
  three harnesses. Watcher writes in those windows are labelled with the tool
  instead of falling back to "you".
- [x] `MCP` section: MCP tool calls grouped server → tool, with arguments in
  Detail.
- [x] Inferred remote targets in MOUNTS from ssh/scp/rsync/docker exec/kubectl
  exec shell commands, labelled "inferred".
- [x] omp `eval` cells as session rows (`eval/HH:MM:SS <title>.<py|js|txt>`),
  with their code in Detail, full-screen Diff and delta.
- [ ] Interpreter heredocs as session rows (low priority): bodies of
  `python3 - <<'EOF'`-style bash commands, named from the call's `i` and start
  time, with the extension from the interpreter. Covers omp and Claude Code Bash.
  Measured at 80 heredocs against 2996 eval cells (Aug–Oct 2026), falling from 39%
  to 2% of eval volume. Promote if that share stops falling. `-c`/`-e` one-liners
  and `cat > file <<EOF` stay out: mostly pipe filters, and 3 cases respectively.
- [ ] Inferred web fetches: URLs from `curl`/`wget`/`gh api`/HTTP clients in
  shell commands, shown in WEB as inferred.
- [ ] Exploration marks: grep/glob/find/ls (and Codex `parsed_cmd`
  search/list_files) as a "scanned" mark on directory rows, not a section.
- [ ] Script-run linking: a temp script the session wrote → the command that ran
  it → the files that run changed.
- [ ] Watch temp roots too, so shell writes outside the project (`/tmp`,
  `$TMPDIR`) show under the session row.

Limits: remote side effects are never observed (only the command sent);
interactive stdin streams (Codex `write_stdin`, pty shells) and indirection
(`bash -c` beyond one level, Makefiles, subprocess calls) are parsed from
intent, not effect.

## 6. Big projects (consideration)

PARTICIPANTS folds older sessions into one row (see
[Condensing PARTICIPANTS](participants-defrag.md)). Possible next steps, kept in
that doc with mockups:

- [ ] Sessions tab in the side panel: browse sessions next to Stats, with the
  fold row as the fallback below 110 columns.
- [ ] One lens for the session picker, action filter and stats range, applied
  to the tree, Gantt and stats together.
- [ ] Show folded sessions that touched the selected file as rows.

## Done

- PARTICIPANTS fold: up to 12 recent sessions (live ones always) stay as
  rows; older ones fold into one `⋯ N older` row with a `▄▆█` activity strip.
  `p` toggles.
- Timeline gaps: runs of 6 or more columns idle across visible activity
  collapse into a three-column ` ~ ` break by default, with a duration label
  when it fits. `c` toggles collapsing; `--no-collapse-gaps` starts linear.
  Cursor movement, jumps, zoom, panning and time labels use the piecewise axis.
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
- Formatting, lints and CI: `rustfmt.toml` sets the style; code is formatted,
  clippy warnings are fixed, and Linux/macOS CI runs fmt, clippy, build and tests.
- Key handling lives in `src/keys.rs`, with per-mode dispatch and the Normal-mode
  vim grammar (counts, prefixes and modifier guard) together.
