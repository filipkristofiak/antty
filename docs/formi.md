# formi

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
  and add `src/bin/formi/main.rs` with `discover.rs`, `app.rs` and `ui.rs`.
- Extracting the lib is a separate PR that lands before any formi code.
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
- Cache lives under the antty state root (e.g. `~/.local/state/antty/formi/`).

## Session discovery

- Reuse antty's index. `Harness::index_root` maps every canonical launch cwd
  to its top-level session files across all session dirs of a harness root,
  so its keys are formi's cwds and `antty --project <cwd>` loads exactly the
  sessions formi lists. The lib-extraction PR adds the one missing accessor,
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
  formi needs no filter.
- Compressed Codex rollouts (`*.jsonl.zst`) are skipped by antty and formi
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
   The activity cache is read by the background indexer.
2. Background indexing: use cached activity for unchanged sessions. A worker
   pool scans sessions missing from the cache or failing validation, highest
   last-seen first. Each session's bar switches to `█` real activity when
   its result arrives. The status bar shows indexing progress.
3. Live: poll every second like antty's `spawn_tailer`: re-run
   `Harness::index_root` (cwd lookups are cached per file) to find new
   sessions. Read only appended bytes when a session has in-memory resume
   state; otherwise rescan its files. Nothing watches the harness roots or
   the repos.

## Activity cache

- Activity has the same meaning as in antty. All files of a session are
  ingested with the existing harness readers (`Harness::ensure_participant`
  + `Harness::ingest`) into a scratch `Model`. Line timestamps alone are not
  enough: a long tool call writes no lines between its call and its result,
  so it would show as idle.
- Raw intervals come from model start → `completedAt` and tool call → result
  (omp), previous entry → assistant entry (Claude Code), and turn start → event
  (Codex CLI).
- The cache has two parts with different jobs:
  - **Display cache (on disk at `<state-dir>/formi/activity.bin`):** one
    entry per session, covering all its files. It stores a thin projection of
    the scratch `Model` — events without details, tool calls without commands
    or arguments, spans, prompts and session metadata. Each file's offset is
    the number of bytes read, including any trailing partial line. A hit
    requires the same ordered file group and each file's length and saved
    tail bytes to match. The format has its own version number, not
    `CARGO_PKG_VERSION`.
  - **Resume state (in memory only):** a freshly scanned or updated session
    within the live window keeps its full scratch `Model` and per-file
    offsets and partial lines. The next append ingests only new bytes.
- The display projection is never a point to resume ingest from. Resuming
  needs state that the projection drops: `pending_tools` (a result whose call
  is missing is dropped, `parse.rs` `ingest_tool_result`), `last_entry_ts`
  (anchors the next interval) and open `tool_windows`/`window_keys`. The
  Claude Code and Codex CLI readers use the same fields.
- If a file changed while formi was closed, its session is rescanned from 0,
  with its main file registered first (Codex CLI subagents need this).
- Persisting resume state to disk is deferred. Add it only if rescanning grown
  files proves slow, and only if the split-call test (bundle C) passes for a
  resume from disk. antty's index does this by storing the whole `Model`.
- omp rewrites its padded line-1 title slot in place, so omp titles are
  re-read from the header and never served from the cache.

## Cost (measured)

Local logs, warm page cache, single-threaded Python: 749 files, 560 MB, 147k
lines. A full JSON parse of every line took 2.8 s; extracting only the
timestamps took 0.7 s. The largest file is 27.8 MB.

Rust, warm page cache, 2026-11-22 (throwaway probe with the harness readers):

| Measurement | Value |
|---|---|
| Logs read | 596 MB, 52 cwds |
| Full ingest, 1 thread | 1.65 s |
| Full ingest, 8 threads | 0.37 s |
| `stats::compute` over all cwds | ~4–5 ms |
| Resulting models | 22.6k events, 40.9k tool calls |
| All full models serialized (rmp) | 68 MB, decode 57 ms |
| `formi --list` (header and tail only) | 0.36 s |

Release `formi`, default harness roots, 240×50 tmux pane, warm OS page cache
(2026-11-23). Timings are for background indexing after header/tail discovery.

| Measurement | Value |
|---|---|
| Cold first scan, empty activity cache | 0.46 s (374 of 374 sessions scanned) |
| Warm start | 0.21 s (0 of 374 sessions scanned) |
| Display cache size | 9,208,436 bytes (9.21 MB) |

## Harness integration

None. Antty stays a read-only observer. Only omp records a session end;
Claude Code and Codex CLI liveness is last-activity only. If that proves too
weak at catching agents blocked on the user, consider optional harness hooks
(not model instructions) that write a small state file such as
`<state-dir>/live/<session-id>.json`, which formi reads when present.

## Folding

Same rules, glyphs and keys as antty's PARTICIPANTS fold
([participants-defrag](participants-defrag.md)): `p` toggles; Enter, Space or
`zo` on a fold row shows everything behind it; search unfolds when a match is
inside a fold.

- **Sessions in an expanded project:** sessions active in the last 2 h, plus
  the 5 most recent, then a `⋯ N older` row with the density strip (`▄` one,
  `▆` two, `█` three or more sessions active per column). 5 rather than
  antty's 12, because several projects can be expanded at once.
- **Projects, recent order:** projects active in the last 2 h, plus the 12
  most recent, then `⋯ N older projects` with a density strip.
- **Tree order (`o`): no project folding.** It browses by location; folding
  projects by time there mixes two axes. Session folding still applies.
- Not folded by size (one-session scratch cwds): recency already sinks old
  scratch work, and a size rule would hide yesterday's.

## Stats and dashboard

Needs the display cache (bundle C).

- **Stats panel `i`:** selection vs all, as in antty. A session row uses that
  session; a project row sums over its cwds' models; a directory row in tree
  order sums its projects. Distinct files read/written are a union of path
  sets, so worktrees of one project don't count a path twice. Adds `share`:
  the selection's share of all calls.
- **Dashboard `I`:** left column is selection vs all (projects, sessions per
  harness, calls, files, prompts, tool mix); right column is projects ranked
  by calls and a fixed "last 7 days" block. `v` limits everything to the
  visible timeline range.
- The panel and popup rendering moves from antty's `src/ui/stats.rs` into
  `antty::tui`, shared by both binaries.
- **Action filter** (which projects and sessions used web, MCP, eval, …):
  after stats, least certain to pay off.

Drafts at 2026-11-22 numbers:

```
┌projects────────────────────────────┐┌timeline───────────────────────────────┐┌Stats──────────────────────┐
│                                    ││Nov 08 10 12 14 16 18 20 22            ││~/p/tools/antty · 3 wt     │
│                                    ││S S M T W T F S S M T W T F S S M T W│ ││            sel      all   │
│▾ ~/p/tools/antty  3wt   83  live   ││                 ▓▓▓▓▓▓  ▓▓▓▓▓▓▓▓▓▓▓▓▓ ││calls      8.6k    40.9k   │
│    Formi overview · omp   11-22    ││                                   ██▌ ││read        431     2.8k   │
│    Stats panel · omp      11-22    ││                                  ██   ││written     514     5.4k   │
│    Help sections · omp    11-21    ││                                 █     ││prompts     254     2.3k   │
│    ⋯ 80 older                      ││▄ ▆█▄▄ ▆█▄ ▄ ▆▄ ▄█▆▄▄▆  ▄▆▄█           ││share       21%            │
│▸ ~/p/personal/side-project   9 1d  ││                       ▓▓▓▓▓▓▓▓  ▓▓    ││───────────────────────────│
│▸ ~/p/workstations/dotfiles… 16 2d  ││  ▓▓    ▓▓▓▓▓▓   ▓      ▓     ▓        ││read     █████████  2.6k   │
│▸ ~/p/web/my-website.org     38 7d  ││  ▓▓▓▓▓▓▓▓▓ ▓▓▓  ▓▓▓▓▓▓▓▓▓             ││bash     ████████▎  2.4k   │
│▸ ~/p/home-infra             61 7d  ││ ▓▓▓▓▓▓▓▓▓▓▓▓▓▓      ▓▓                ││grep     ██▊        0.8k   │
│▸ ~/p/exercises/try-tools     3 8d  ││               ▓                       ││+ more                     │
│▸ ~/p/personal/demo-sandbox   2 8d  ││               ▓                       ││                           │
│▸ ~/p/everyday/notes          1 9d  ││              ▓                        ││                           │
│▸ ~/p/etl-experiments        23 9d  ││      ▓▓▓▓▓▓▓▓▓▓▓▓▓▓                   ││                           │
│▸ ~/p/sandbox/slides-draft…   1 12d ││   ▓                                   ││                           │
│▸ ~/p/sandbox/wip             6 17d ││▓▓▓▓                                   ││                           │
│▸ ~/p/personal/demo-poster    1 18d ││▓▓▓▓▓▓▓                                ││                           │
│⋯ 34 older projects                 ││▄▄▆▄ ▄ ▆ ▄▄ ▄ ▄  ▄                     ││                           │
└────────────────────────────────────┘└───────────────────────────────────────┘└───────────────────────────┘
Nav j/k h/l | Open ⏎ | Fold ␣ p | Stats i I | ? help        1d · 46 projects · 368 sessions · fold:on
```

```
┌Dashboard  (v range · j/k move selection · Enter open · q/Esc close)──────────────────────────────────┐
│ all projects · all time                             │ projects by calls                              │
│                                                     │                                                │
│ projects        46   sessions    368                │ ▶tools/antty           ████████████████  8.6k  │
│ calls        40.9k   omp         330                │  web/my-website.org    ████████████▌     6.8k  │
│ read          2.8k   claude       38                │  home-infra            ███████████▎      6.1k  │
│ written       5.4k   codex         0                │  tools/hlit            ██████▎           3.4k  │
│ prompts       2.3k                                  │  etl-experiments       █████             2.8k  │
│ ────────────────────────────────────────────────    │  everyday/recipes-db   ████▉             2.6k  │
│ tool mix                                            │  workstations/dotfiles ███▋              2.0k  │
│ read       ████████████████████████████  11.8k      │  tools/notebook        ███               1.7k  │
│ bash       ███████████████████████▉      10.0k      │  personal/side-project ███               1.6k  │
│ eval       █████████▌                     4.0k      │  tools/formatter       ██▎               1.2k  │
│ edit       █████████▌                     4.0k      │  ⋯ 36 more · Σ 4.0k                            │
│ grep       ███████                        3.0k      │                                                │
│ todo       █████▌                         2.3k      │ last 7 days                                    │
│ write      ███▊                           1.6k      │                                                │
│ advise     ███▍                           1.5k      │  tools/antty           ████████████████   783  │
│ web_search █▊                             0.7k      │  personal/side-project █████████▍         464  │
│ + more                                              │  web/my-website.org    ▏                    7  │
│                                                     │  workstations/dotfiles                      1  │
└──────────────────────────────────────────────────────────────────────────────────────────────────────┘
```

The read/written figures in the drafts are sums of per-cwd distinct counts, so
they overcount; the shipped numbers use the path-set union above.

## Remote readiness

Later, formi may show another machine's projects. Nothing remote is built now,
but these keep it possible:

- **Identity:** a project key is `{ origin, path }` (`Origin::Local` only for
  now). `App.expanded`, `Node.key` and `ProjectView.key` must not use a bare
  `PathBuf`: `~/projects/x` on two machines would collide.
- **Drill target decided in the data layer:** `Drill::Local(path) | None` is
  set at discovery, not by `exists()` when Enter is pressed. A later
  `Drill::Ssh { host, path }` runs `ssh -t host antty --project path`.
- **Serializable summaries:** the UI reads only what the data layer produces.
  With its own format version, that output is the remote protocol:
  `ssh host formi --dump`, no daemon, no ports.
- Not a data source: antty's `index/*.bin` files (machine-local,
  invalidated on every version bump, only for projects opened in antty).
- Polling carries over to remote; watchers don't.

## Build plan

Formi stays local on `feat/formi` until finished: no pushes, no PRs. Library
changes it needs land on the same branch. Rebase onto `origin/main` whenever
it moves. Each bundle ends in a local commit that builds and passes the CI
checks; the version bump happens once, when formi lands.

### Done

- **Bundle A, discovery:** `formi --list` prints projects, cwds and per-harness
  session counts. Worktree cwds fold by `worktree::git_common_dir`; missing or
  repo-less cwds are their own projects.
- **Bundle B, first screen:** collapsed projects in recent-activity order; `o`
  switches to an alphabetical path trie. Startup reads only each top-level
  session's header (first timestamped record or 50 lines) and tail (64 KiB,
  doubling up to 8 MiB), so bars span start → last-seen, not real activity.
  Subagent logs are registered, not read; an explicit session end wins over
  last-seen. Drill-down runs `antty --project <most recently active worktree>`.
  Discovery runs only at startup. See [usage](usage.md#formi).
- **Bundle B.5, interaction:** project identity includes its origin, drill
  targets are chosen at discovery, and older sessions and projects fold into
  summary rows. `?` opens shared help; `/` searches paths and session titles
  and reveals matches hidden by folds.
- **Bundle C, real activity:** `█` replaces the startup estimate once session
  activity is indexed. The display cache reuses unchanged sessions, and a
  one-second poll finds new sessions and updates running ones. Split and
  resumed ingest matched cold scans for all three harnesses; a grown file
  invalidated its cache entry and matched a cold scan. The tmux smoke test
  confirmed a warm cache hit and a second activity bar after a live append
  without restarting.
- **Wheel scrolling:** copied from antty's #20 fix: shared mouse capture
  (`antty::tui::MouseCapture`, released while antty or a shell owns the
  terminal), input drained under an 8 ms budget with same-direction wheel
  reports merged, and the wheel scrolling the hovered rows or Help without
  moving the selection.

### Bundle D: analysis

1. Stats panel `i` (see Stats and dashboard).
2. Dashboard `I`.
3. Action filter.

### Landing

Docs (`docs/usage.md`, `AGENTS.md` module map), help keys, version bump (a
new binary is backward-compatible: patch), then the first public push.

## Open questions

- Whether Codex `*.jsonl.zst` rollouts should be read (affects antty too).
