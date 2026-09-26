# How it works

## Session discovery

`sessions.rs`'s `discover_project_dir` finds the direct child of the sessions
root whose most-recent top-level session file has a header `cwd` equal to the
project root; header matching is authoritative, so omp's directory-name
encoding is never reimplemented. Session files are tailed live: `load_initial`
reads every jsonl file once at startup, and `spawn_tailer` polls the project
dir and each file once per second afterward, resuming exactly where the
initial read left off.

## Parsing tool calls into file events

`parse.rs` ingests one jsonl line at a time (`ingest`), tolerant of schema
drift — unrecognized types/shapes are no-ops. It normalizes tool-arg path
strings into project-relative or external/session/web scopes (`normalize`,
`locate`), turns `read`/`write`/`edit` tool results into `FileEvent`s with
before/after content snapshots, and accumulates per-participant raw activity
intervals that later get merged into spans.

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
