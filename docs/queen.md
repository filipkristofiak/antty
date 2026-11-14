# queen (working name)

A companion binary to antty: an overview of **all** projects with coding-agent
sessions, rather than antty's single-project view.

## Scope

- Grain is project → sessions. No file breakdown; that stays in antty.
  Subagent files fold into their parent session's row.
- A project is a session's canonical launch cwd. Cwds that are worktrees of
  one repository (`worktree::git_common_dir`) fold into one project. Nothing walks
  up to a git toplevel: a session launched in a subdirectory is its own
  project, matching antty, which only loads sessions whose cwd equals a
  worktree root (`RootIndex::files`).
- Cwds with no git repo, or that no longer exist, are their own projects keyed
  by the raw cwd. This is the common case, not an edge: in local logs,
  27 of 38 omp cwds and 12 of 14 Claude Code cwds.
- Drill-down runs `antty --project <cwd>`. For a folded worktree project, use
  the worktree whose session was most recently active. Cwds that no longer
  exist have no drill-down: antty rejects a missing `--project` path.

## Packaging

- Same repo, one package: extract `src/lib.rs`, keep `src/main.rs` as antty,
  and add `src/bin/queen/main.rs` with `discover.rs`, `app.rs` and `ui.rs`.
- Extracting the lib is a separate PR that lands before any queen code.
- The lib must be closed under `crate::` imports. Today's graph has
  `sessions` → `claude`/`codex`/`parse`/`index`/`disk`/`watch`,
  `model` → `disk` → `tree`, `index` → `attrib` and `watch` → `sessions`, so
  the lib is every module except `main`, `keys`, `cli`, `cmdline`, `ui/*`,
  `delta`, `editor` and `search`. Shrinking it needs decoupling first, which
  is not part of the extraction PR.
- The harness-root and state-dir defaults (`default_claude_dir`,
  `default_codex_dir`, `default_state_dir`, the omp default) move from
  `cli.rs` into the lib, so both binaries share them.
- `antty::tui` shares highlight colours, the timeline ruler and terminal handoff
  between both binaries.
- One version and the existing bump rule. Move to a Cargo workspace only if
  release cadences or dependencies diverge.
- Cache lives under the antty state root (e.g. `~/.local/state/antty/queen/`).

## Session discovery

- Reuse antty's index. `Harness::index_root` maps every canonical launch cwd
  to its top-level session files across all session dirs of a harness root,
  so its keys are queen's cwds and `antty --project <cwd>` loads exactly the
  sessions queen lists. The lib-extraction PR adds the one missing accessor,
  `RootIndex::cwds()`, iterating the keys; `RootIndex::files(cwd)` already
  lists each top-level session followed by its subagent logs.
- Cwd rules come with the index: omp reads each top-level file's line-2
  header `cwd`; Claude Code takes the first `cwd` within a transcript's first
  50 lines; Codex CLI groups `rollout-*.jsonl` by `session_meta.cwd`. A file
  without a readable cwd is skipped and picked up by a later index pass.
- Row membership: register every file of a cwd with
  `Harness::ensure_participant`, in `RootIndex::files` order, into one
  per-cwd `Model` (header reads only). A file belongs to the row of its
  participant's `Session`. The order matters for Codex CLI: a subagent
  rollout finds its parent only when the parent is registered first, and
  `RootIndex::files` lists parents first.
- omp subagent discovery scans `<stem>/` recursively, so it also lists
  non-session JSONL such as `<stem>/local/sessionize-*.jsonl` metrics logs.
  Their records have no `type`, so ingest yields no activity from them and
  queen needs no filter.
- Compressed Codex rollouts (`*.jsonl.zst`) are skipped by antty and queen
  alike.

## Data flow

1. Startup (synchronous, fast): run `Harness::index_root` for each harness
   root, register each cwd's files (see Session discovery), then read each
   top-level session's header and seek to its tail.
   That gives cwd, start, last-seen (the last line in the tail that has a
   top-level `timestamp`) and, for omp, the end (a `session_exit` record).
   Titles: omp from the line-1 title slot, Codex from `session_index.jsonl`
   (`thread_name`), Claude Code from `custom-title`/`ai-title` when encountered
   in the header or tail, else `(untitled)`. Render one `▒` bar per session,
   marked as not yet indexed, with `▓` union bars on project and directory rows.
   Startup does not yet read an activity cache.
2. Background indexing: a worker pool scans files that are missing from the
   cache or fail validation, highest last-seen first. Each file sends its
   result when done and its bar switches to real activity. Progress is shown
   in the status bar (same pattern as antty's `Msg::Progress`).
3. Live: poll every second like antty's `spawn_tailer`: re-run
   `Harness::index_root` (cwd lookups are cached per file) to find new
   sessions, and read appended bytes of files that grew. Nothing watches the
   harness roots or the repos.

## Activity cache

- Activity has the same meaning as in antty. Each file is ingested with the
  existing harness readers (`Harness::ensure_participant` +
  `Harness::ingest`) into a scratch `Model`, and its `raw_intervals` (before
  `flush_dirty_spans`) are converted to per-minute buckets. A minute is active
  if any raw interval overlaps it. The scratch `Model` is then dropped. Line
  timestamps alone are not enough: a long tool call writes no lines between
  its call and its result, so it would show as idle.
- Raw intervals come from model start → `completedAt` and tool call → result
  (omp), previous entry → assistant entry (Claude Code), and turn start → event
  (Codex CLI).
- Spans are merged from the buckets at draw time using the idle gap, so
  changing the idle gap or the zoom never needs a rescan. Because the grain is
  one minute, idle gaps under 60 s have no effect in queen.
- Each cached file stores its buckets, title, byte offset (trimmed back to the
  last full line) and the bytes just before that offset. On startup, a file
  whose length still equals the offset and whose stored bytes still match is
  reused as is. Any other file is rescanned from 0, because ingest state such
  as open tool calls is not cached.
- While queen runs, each file that grows keeps its scratch `Model` in memory,
  and only the appended lines are ingested.
- omp rewrites its padded line-1 title slot in place, so omp titles are
  re-read from the header and never served from the cache.

## Cost (measured)

Local logs, warm page cache, single-threaded Python: 749 files, 560 MB, 147k
lines. A full JSON parse of every line took 2.8 s; extracting only the
timestamps took 0.7 s. The largest file is 27.8 MB. Cold-cache cost has not
been measured.

## Harness integration

None. Antty stays a read-only observer. Only omp records a session end;
Claude Code and Codex CLI liveness is last-activity only. If that proves too
weak at catching agents blocked on the user, consider optional harness hooks
(not model instructions) that write a small state file such as
`<state-dir>/live/<session-id>.json`, which queen reads when present.

## Build plan

Queen stays local on `feat/queen` until finished: no pushes, no PRs. Library
changes it needs land on the same branch. Rebase onto `origin/main` whenever
it moves. Each bundle ends in a local commit that builds and passes the CI
checks; the version bump happens once, when queen lands.

### Bundle A: discovery (steps 1–2)

1. **Binary skeleton.** `src/bin/queen.rs` with antty's harness-root flags,
   resolved through `antty::dirs` (`--state-dir` arrives with the cache in bundle C).
2. **Project list.** `Harness::index_root` for every harness, cwds from
   `RootIndex::cwds()`, worktree cwds folded by `worktree::git_common_dir`, missing
   or repo-less cwds kept as their own projects. Plain-text output: project,
   cwds, session counts per harness.

Done when the printed projects and counts match the local logs, including the
raw-cwd projects (27 of 38 omp cwds, 12 of 14 Claude Code cwds today). This
checks the project model before any UI exists.

### Bundle B: first screen (steps 3–4)

Implemented locally. `queen` opens collapsed projects in recent-activity order;
`o` switches to an alphabetical path trie. `queen --list` retains bundle A's
plain-text output. See [usage](usage.md#queen) for controls. Discovery runs only
at startup in this bundle.

3. **Startup reads.** Per top-level session: header and tail only, for start,
   last-seen, omp's end and titles. Claude Code titles are used when encountered.
   Subagent files folded into their parent row via `ensure_participant` in
   `RootIndex::files` order.
4. **TUI.** Project → session rows with one not-yet-indexed bar per session,
   on `timeline` zoom and column mapping. Drill-down runs
   `antty --project <most recently active worktree>`; no drill-down for
   missing cwds.

Done when queen opens in under a second on the local logs and drill-down
lands in antty showing the same sessions as the row.

Header reads stop at the first timestamped record or 50 lines. Tail reads start
with 64 KiB and double up to 8 MiB until finding a timestamped record (or reaching
the header). Only newline-terminated records are ingested; subagent logs are
registered but not read. An explicit session end wins over last-seen.

### Bundle C: real activity (steps 5–6)

5. **Activity cache.** Worker pool, highest last-seen first, ingests each file
   into a scratch `Model` and stores per-minute buckets from `raw_intervals`
   under `<state-dir>/queen/`, with offset and trailing-byte validation. Bars
   switch to real activity as results arrive; progress in the status bar.
6. **Live.** Once a second: re-run `Harness::index_root` for new sessions and
   ingest only appended lines of grown files, keeping their scratch `Model`.

Done when a cold first scan is measured (open question below), a warm start
reuses every unchanged file, and an active session's bar grows without a
restart.

### Landing

Docs (`docs/usage.md`, `AGENTS.md` module map), help keys, version bump, then
the first public push.

## Open questions

- Cold-cache first-scan cost.
- Whether Codex `*.jsonl.zst` rollouts should be read (affects antty too).
