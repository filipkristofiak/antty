# AGENTS.md

antty is a Rust 2024 ratatui/crossterm TUI: a Gantt timeline of coding-agent
sessions and the files they touched.

## Commands

```
rustup run stable cargo build
rustup run stable cargo test
rustup run stable cargo run
```

Plain `cargo` works wherever it's already on `PATH`.

## Module map

- `src/lib.rs` — library root; declares the modules shared with other binaries.
- `src/editor.rs` — `e` key: on-disk path and first changed line of an event, and the `$EDITOR`/`nvim` invocation that opens it.
- `src/tui.rs` — terminal pieces shared by antty and formi: highlight colours, timeline ruler, help overlay, stats panel rendering, mouse capture, handing the terminal to a child process.
- `src/main.rs` — event loop, `App` state and actions, startup wiring.
- `src/keys.rs` — per-mode key dispatch and Normal-mode vim grammar.
- `src/cli.rs` — `RawArgs`/`Args`: flag parsing and resolution.
- `src/filter.rs` — `Action` categories and `ActionFilter` matching for file events.
- `src/dirs.rs` — default harness session roots, state dir and `~` expansion.
- `src/cmdline.rs` — `:`/`/` line editing and in-memory history.
- `src/delta.rs` — unified patch for an event and the optional `delta` invocation.
- `src/model.rs` — core data model: participants, sessions, file events, spans, snapshots.
- `src/sessions.rs` — shared harness discovery indexes, parallel initial parsing and live tailing with progress.
- `src/parse.rs` — ingests omp jsonl lines into model events; path normalization and scoping.
- `src/claude.rs` — Claude Code reader: project-dir discovery (first-`cwd` matching), transcript scan, participants, ingest.
- `src/codex.rs` — Codex CLI reader: rollout discovery (header-`cwd` matching), title index, participants, ingest.
- `src/remote.rs` — shell-command parsing for inferred ssh/scp/rsync/docker/kubectl targets.
- `src/watch.rs` — recursive filesystem watcher, `.gitignore`-aware (including nested ones).
- `src/worktree.rs` — Git worktree discovery and common-dir resolution.
- `src/attrib.rs` — attributes watcher events to tool-call windows; persists/replays the JSONL state log.
- `src/snapshot.rs` — file content snapshots and unified diffs.
- `src/disk.rs` — background checkout listings, merged disk snapshot cache and project-only content seeding.
- `src/index.rs` — binary session index, log-tail validation and worktree-root remapping.
- `src/timeline.rs` — zoom levels, time↔column mapping, idle-gap span merging.
- `src/tree.rs` — builds the `PARTICIPANTS`/`FILES`/`MOUNTS`/`WEB`/`MCP` row forests.
- `src/search.rs` — smartcase full-tree search and wrapped match stepping.
- `src/stats.rs` — stats subjects for tree rows; `Tally` sums tool-call, file and time totals across models, counting each file once per owning project, with visibility and time-range filters.
- `src/ui/mod.rs` — `Mode`, layout math, top-level `draw`.
- `src/ui/tree_pane.rs` — left pane: sections, participants, file tree rows.
- `src/ui/gantt_pane.rs` — right pane: the Gantt timeline.
- `src/ui/status.rs` — bottom status bar and command line.
- `src/ui/detail.rs` — Detail overlay: event/span list and diff text.
- `src/ui/diff_view.rs` — full-screen diff overlay.
- `src/ui/picker.rs` — session picker overlay.
- `src/ui/actions.rs` — action filter overlay (`F`).
- `src/ui/stats.rs` — selection-vs-all comparison for the stats side panel and popup; drawing comes from `antty::tui`.
- `src/ui/help.rs` — help overlay and `KEYS` table.
- `src/bin/formi/main.rs` — formi entry point: flags, `--list`, event loop with input draining and live updates.
- `src/bin/formi/discover.rs` — formi's project and session discovery: cwd grouping into projects, session file groups, header/tail metadata.
- `src/bin/formi/activity.rs` — background indexer: per-session ingest, display-cache reuse and the once-a-second live poll.
- `src/bin/formi/activity/tests.rs` — split-ingest, cache-invalidation and live-poll tests for the indexer.
- `src/bin/formi/cache.rs` — on-disk display cache (`<state-dir>/formi/activity.bin`): format, load, save, validation.
- `src/bin/formi/app.rs` — formi `App` state: project forest, folds, search, wheel, stats, dashboard and action filter.
- `src/bin/formi/ui.rs` — formi drawing: project tree, timeline, stats panel, dashboard, action overlay, status bar, `HELP` table.

## Conventions

- Harness-agnostic naming: "omp"/"Claude Code"/"Codex CLI" appear only when
  describing that harness's on-disk format (`parse.rs`, `claude.rs`, `codex.rs`,
  `sessions.rs`, `dirs.rs`, the `--omp-dir`/`--claude-dir`/`--codex-dir` flags and defaults).
- Any change that affects the built binary (`src/`, `Cargo.toml`
  dependency/feature/profile changes, `build.rs`) bumps the `version` in
  `Cargo.toml`, then runs `cargo build` (or `cargo update -p antty`) in the
  same commit to refresh `Cargo.lock`. Follow semver; while antty is pre-1.0
  (`0.x.y`), a minor bump (`0.x` → `0.(x+1)`) is the breaking-change signal
  (CLI flags, on-disk state format, output format) and a patch bump
  (`0.x.y` → `0.x.(y+1)`) covers fixes and backward-compatible changes.
  Doc-only, `AGENTS.md`-only, or CI-only changes are exempt.
- Branching (soft policy): don't start new work directly on `main`; create
  a `<type>/<topic>` branch (`feat/`, `fix/`, `chore/`, `docs/`) unless the
  user explicitly asks to work on `main`.
- Test temp dirs go under `std::env::temp_dir()` with the prefix
  `antty-<module>-test-`.
- Keep `docs/usage.md` in sync when changing `help.rs`'s `KEYS` or `cli.rs`'s
  flags.
- Smoke-test TUI changes with a tmux capture:
  ```
  tmux new-session -d -s antty-smoke -x 200 -y 50 "target/debug/antty --no-watch --state-dir /tmp/antty-smoke-state"
  sleep 2; tmux capture-pane -p -t antty-smoke; tmux kill-session -t antty-smoke; rm -rf /tmp/antty-smoke-state
  ```

See `docs/` for installation, usage, and how-it-works details.
