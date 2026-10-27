# How it works

## Session discovery

Each harness (`sessions::Harness`: omp, Claude Code, Codex CLI) has its own
root (`--omp-dir`, `--claude-dir`, `--codex-dir`). A shared `RootIndex` maps
canonical launch cwd to session sources in one pass per harness root, rather
than scanning again for every worktree. omp matches header cwd; Claude Code
matches the first cwd in the newest session. Neither directory-name encoding
is reimplemented. Codex groups dated rollout files by first-line
`session_meta.cwd`; subagents are linked by `parent_thread_id`, with titles
from the adjacent `session_index.jsonl`.

`spawn_tailer` discovers and parses sources on background workers, sending parsed
records in batches of at most 2048. It reports initial indexing progress and then
polls for new files and appended bytes every second. Incomplete final lines are
buffered until their newline arrives. `index.rs` saves log-derived model state and
complete-line offsets under `<state-dir>/index/`. On restart it validates the saved
log tails and resumes from those offsets; incompatible settings or rewritten tails
cause a cold load. Watcher history is replayed only after initial records are applied.

## Parsing tool calls into file events

`parse.rs` (omp), `claude.rs` (Claude Code), and `codex.rs` (Codex CLI) ingest
one jsonl line at a time, tolerant of schema drift — unrecognized types/shapes
are no-ops. They resolve touched paths into project-relative or
external/session/web/MCP scopes, turn tool results into `FileEvent`s and content
snapshots, and accumulate per-participant activity intervals merged into spans.
Claude Code logs no per-message completion time, so the gap between an
assistant record and the previous user/assistant record counts as activity.
Codex uses turn start/end and intervening rollout records for activity;
`history_mode: "paginated"` rollouts use `item_completed` for prompts, file
changes, command reads and web events; older `"legacy"` rollouts use separate
events. A declared mode selects one format when both appear; older rollouts
without a mode accept either record shape. Shell, file-mutating device and MCP calls
delimit windows used for watcher attribution. MCP calls appear under server → tool
in their own section; successful calls are read marks because their effects are unknown.

## Filesystem watcher

`watch.rs` sets up a recursive `notify` watcher for each checkout on bounded
background workers. A `GitignoreSet` layers every `.gitignore` found in the tree,
so nested rules are honored even without a root `.gitignore`.

`disk.rs` walks checkouts in parallel, excluding nested checkout roots, and caches
their listings plus a merged FILES trie. File creation/removal requests coalesced
relists; `--no-watch` lists only once. Tree rebuilds never walk disk, and only stat
unlisted event paths in their own checkout. Content seeding reads only `--project`.

## Attribution of watcher changes to tool-call windows

`attrib.rs`'s `Attributor` classifies each raw filesystem event: if it falls
inside a tool-call's time window (with slack before/after), it's attributed to
that participant's tool call; otherwise it's attributed to "you". Events too
close to an already-recorded tool write are deduped. Every classified change is
appended as JSON to a state-log file at `persist_path_for` (state dir + the
project root path with `/` replaced by `_`, plus `.jsonl`), including a diff
against the last known snapshot. `replay` reads that log back in at startup,
after the initial session load, so history survives a restart.

Call labels and command/code text are retained in the model and shown in Detail and
full-screen Diff. The state log stores a UTF-8-safe 4 KiB input prefix plus a truncation
marker; replay keeps complete transcript inputs when available.

## Inferred remote targets

`remote.rs` parses shell intent for ssh/scp/rsync/docker exec/kubectl exec and
bounded shell `-c` wrappers. Inferred targets appear in MOUNTS with an `inferred`
event label and the originating command; host-only targets use a `$` leaf.
Copy sources/destinations retain read/write direction. Remote side effects are not
observed, and failed commands still record targets. Comments, quoted prose and heredoc
bodies are skipped; interactive stdin, Makefiles and subprocess indirection are not traced.

## Timeline bucketing and `--idle-gap` span merging

`timeline.rs` defines the zoom ladder (`ZOOM_LEVELS`, 10s to 1 day per column)
and a `View` that maps time to screen columns and back (`fit`, `bucket`,
`col_for`). Separately, `parse.rs`'s `recompute_spans` merges a participant's
raw activity intervals into visible spans whenever the gap between two
intervals exceeds `--idle-gap` seconds; smaller gaps are absorbed into a single
span.
