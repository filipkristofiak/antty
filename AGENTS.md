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

- `src/main.rs` — event loop, key handling, `App` state, startup wiring.
- `src/cli.rs` — `RawArgs`/`Args`: flag parsing, tilde expansion, defaults.
- `src/cmdline.rs` — `:`/`/` line editing and in-memory history.
- `src/model.rs` — core data model: participants, sessions, file events, spans, snapshots.
- `src/sessions.rs` — `discover_project_dir` (header-`cwd` matching), initial load + live tailer.
- `src/parse.rs` — ingests jsonl lines into model events; path normalization and scoping.
- `src/watch.rs` — recursive filesystem watcher, `.gitignore`-aware (including nested ones).
- `src/attrib.rs` — attributes watcher events to tool-call windows; persists/replays the JSONL state log.
- `src/snapshot.rs` — file content snapshots and unified diffs.
- `src/timeline.rs` — zoom levels, time↔column mapping, idle-gap span merging.
- `src/tree.rs` — builds the `PARTICIPANTS`/`FILES`/`MOUNTS`/`WEB` row forests.
- `src/ui/mod.rs` — `Mode`, layout math, top-level `draw`.
- `src/ui/tree_pane.rs` — left pane: sections, participants, file tree rows.
- `src/ui/gantt_pane.rs` — right pane: the Gantt timeline.
- `src/ui/status.rs` — bottom status bar and command line.
- `src/ui/detail.rs` — Detail overlay: event/span list and diff text.
- `src/ui/diff_view.rs` — full-screen diff overlay.
- `src/ui/picker.rs` — session picker overlay.
- `src/ui/help.rs` — help overlay and `KEYS` table.

## Conventions

- Harness-agnostic naming: "omp" appears only where describing omp's on-disk
  format (`parse.rs`, `sessions.rs`, the default `--sessions-dir`, the
  "no omp sessions" status string).
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
