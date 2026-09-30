# How it works

## Session discovery

Each harness (`sessions::Harness`: omp, Claude Code, Codex CLI) has its own
root (`--omp-dir`, `--claude-dir`, `--codex-dir`). For omp and Claude Code,
`discover_project_dir` finds the project directory by matching the logged
launch `cwd` (omp's header, Claude Code's first `cwd`) to the project root;
neither harness's directory-name encoding is reimplemented. Codex has no
project directories: `codex.rs` scans dated rollout files under
`$CODEX_HOME/sessions` (or `~/.codex/sessions`) and selects files whose
first-line `session_meta.cwd` matches the project root. Subagents are linked
by `parent_thread_id`; titles come from the adjacent `session_index.jsonl`.
Session files are tailed live: `load_initial` reads selected jsonl files
at startup, and `spawn_tailer` polls for new files and lines once per second.

## Parsing tool calls into file events

`parse.rs` (omp), `claude.rs` (Claude Code), and `codex.rs` (Codex CLI) ingest
one jsonl line at a time, tolerant of schema drift — unrecognized types/shapes
are no-ops. They resolve touched paths into project-relative or
external/session/web scopes, turn tool results into `FileEvent`s and content
snapshots, and accumulate per-participant activity intervals merged into spans.
Claude Code logs no per-message completion time, so the gap between an
assistant record and the previous user/assistant record counts as activity.
Codex uses turn start/end and intervening rollout records for activity;
`history_mode: "paginated"` rollouts use `item_completed` for prompts, file
changes, command reads and web events; older `"legacy"` rollouts use separate
events. A declared mode selects one format when both appear; older rollouts
without a mode accept either record shape. Shell call/output pairs delimit
windows used for watcher attribution.

## Filesystem watcher

`watch.rs` spawns a recursive `notify` watcher on the project root. A
`GitignoreSet` layers every `.gitignore` found in the tree — not just the
root's — so a nested crate's own `.gitignore` (e.g. its `target/`) is honored
even when there's no root-level `.gitignore`.

## Attribution of watcher changes to tool-call windows

`attrib.rs`'s `Attributor` classifies each raw filesystem event: if it falls
inside a tool-call's time window (with slack before/after), it's attributed to
that participant's tool call; otherwise it's attributed to "you". Events too
close to an already-recorded tool write are deduped. Every classified change is
appended as JSON to a state-log file at `persist_path_for` (state dir + the
project root path with `/` replaced by `_`, plus `.jsonl`), including a diff
against the last known snapshot. `replay` reads that log back in at startup,
after the initial session load, so history survives a restart.

## Timeline bucketing and `--idle-gap` span merging

`timeline.rs` defines the zoom ladder (`ZOOM_LEVELS`, 10s to 1 day per column)
and a `View` that maps time to screen columns and back (`fit`, `bucket`,
`col_for`). Separately, `parse.rs`'s `recompute_spans` merges a participant's
raw activity intervals into visible spans whenever the gap between two
intervals exceeds `--idle-gap` seconds; smaller gaps are absorbed into a single
span.
