# How it works

## Session discovery

Each harness (`sessions::Harness`: omp, Claude Code) has its own session
root (`--omp-dir`, `--claude-dir`). Per root, `discover_project_dir` finds the
direct child whose most-recent top-level session file records a `cwd` equal
to the project root (omp: the header line; Claude Code: the first record
carrying `cwd`, in `claude.rs`); matching the logged cwd is authoritative, so
neither harness's directory-name encoding is reimplemented. Session files are
tailed live: `load_initial` reads every jsonl file of each root once at
startup, and `spawn_tailer` polls every root's project dir and each file once
per second afterward, resuming exactly where the initial read left off.

## Parsing tool calls into file events

`parse.rs` (omp) and `claude.rs` (Claude Code) ingest one jsonl line at a
time (`ingest`), tolerant of schema drift — unrecognized types/shapes are
no-ops. Both normalize tool-arg path strings into project-relative or
external/session/web scopes (`normalize`, `locate`), turn read/write/edit tool
results into `FileEvent`s with before/after content snapshots, and accumulate
per-participant raw activity intervals that later get merged into spans.
Claude Code logs no per-message completion time, so the gap between an
assistant record and the previous user/assistant record counts as activity.

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
