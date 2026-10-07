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

- `src/main.rs` — event loop, `App` state and actions, startup wiring.
- `src/keys.rs` — per-mode key dispatch and Normal-mode vim grammar.
- `src/cli.rs` — `RawArgs`/`Args`: flag parsing, tilde expansion, defaults.
- `src/cmdline.rs` — `:`/`/` line editing and in-memory history.
- `src/delta.rs` — unified patch for an event and the optional `delta` invocation.
- `src/model.rs` — core data model: participants, sessions, file events, spans, snapshots.
- `src/sessions.rs` — `Harness` dispatch, omp's `discover_omp_project_dir` (header-`cwd` matching), initial load + live tailer over every harness.
- `src/parse.rs` — ingests omp jsonl lines into model events; path normalization and scoping.
- `src/claude.rs` — Claude Code reader: project-dir discovery (first-`cwd` matching), transcript scan, participants, ingest.
- `src/codex.rs` — Codex CLI reader: rollout discovery (header-`cwd` matching), title index, participants, ingest.
- `src/watch.rs` — recursive filesystem watcher, `.gitignore`-aware (including nested ones).
- `src/worktree.rs` — Git worktree discovery and common-dir resolution.
- `src/attrib.rs` — attributes watcher events to tool-call windows; persists/replays the JSONL state log.
- `src/snapshot.rs` — file content snapshots and unified diffs.
- `src/timeline.rs` — zoom levels, time↔column mapping, idle-gap span merging.
- `src/tree.rs` — builds the `PARTICIPANTS`/`FILES`/`MOUNTS`/`WEB` row forests.
- `src/search.rs` — smartcase full-tree search and wrapped match stepping.
- `src/ui/mod.rs` — `Mode`, layout math, top-level `draw`.
- `src/ui/tree_pane.rs` — left pane: sections, participants, file tree rows.
- `src/ui/gantt_pane.rs` — right pane: the Gantt timeline.
- `src/ui/status.rs` — bottom status bar and command line.
- `src/ui/detail.rs` — Detail overlay: event/span list and diff text.
- `src/ui/diff_view.rs` — full-screen diff overlay.
- `src/ui/picker.rs` — session picker overlay.
- `src/ui/help.rs` — help overlay and `KEYS` table.

## Conventions

- Harness-agnostic naming: "omp"/"Claude Code"/"Codex CLI" appear only when
  describing that harness's on-disk format (`parse.rs`, `claude.rs`, `codex.rs`,
  `sessions.rs`, the `--omp-dir`/`--claude-dir`/`--codex-dir` flags and defaults).
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
