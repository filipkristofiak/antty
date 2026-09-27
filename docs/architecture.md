# Architecture

How antty is put together, for people changing it. Per-module mechanics
(session discovery, gitignore handling, span merging) are in
[how-it-works.md](how-it-works.md).

## Threads

Two background producers send `Msg`s over one `mpsc` channel:

| Producer | Sends |
| --- | --- |
| tailer thread (`sessions::spawn_tailer`), polls every harness's session files (omp, Claude Code) every 1s | `Msg::Lines` (tagged with its `Harness` and project dir), `Msg::Reset`, one `Msg::Tick` per poll |
| `notify`'s watcher thread, via the `watch::handle_event` callback | `Msg::Fs` |

The main thread is the only consumer and the only code that touches
`Model`, so `Model` is a plain struct with no locks. `run_loop` (`main.rs`)
drains the channel, waits up to 200ms for a key, then redraws.

## Data flow

```
tailer thread --Lines/Reset/Tick--> mpsc --> main thread (App::handle_message)
notify thread --Fs----------------> mpsc
  Lines --> parse::ingest ----------> Model
  Reset --> clear_participant_data -> Model
  Fs    --> Attributor::push (queued)
  Tick  --> Attributor::classify_and_apply --> Model, state log
Model --> Tree::build --> build_rows --> ui::draw
```

Two paths write `FileEvent`s into `Model`:

- `parse.rs` reads what the agent logged: tool calls and their results.
  It's exact, but it only sees work done through tools.
- `attrib.rs` reads raw filesystem events and guesses who caused them: a
  running bash/eval tool call, otherwise "you".

## Watcher attribution

`Attributor` does the guessing, and it depends on timing:

1. `push` queues each raw event in `pending`, one entry per path (latest
   timestamp, strongest kind: removed > created > modified).
2. On each `Tick`, `classify_and_apply` takes entries older than
   `CLASSIFY_DELAY_SECS` (5s). A path created and then removed within that
   window (e.g. `sed -i`'s temp file) is dropped.
3. `classify_one` drops the event if a tool already logged a write to that
   path ending between 10s before and 1s after it (`DEDUP_TOOL_*`).
   Otherwise the event goes to the latest-starting bash/eval tool window
   open from 1s before to 8s after it (`WINDOW_*_SLACK_SECS`), or to "you"
   if none matches.

These constants encode observed FSEvents latency (see the comment above
the dedup check), not a protocol guarantee. Change them only with a
reproduction.

## Persistence

Watcher events have no other record, so each classified one is appended,
with its diff, as a JSON line to
`<state-dir>/<project path with / replaced by _>.jsonl`. There is no
rotation, so the file grows with edit volume.

At startup `attrib::replay` reads the log back. It must run after
`sessions::load_initial` for every harness, which registers the participants the logged
lines refer to. Lines that fail to parse or resolve are skipped silently,
so getting this order wrong loses history with no error. If the file
can't be read at all, the status bar shows an error.

## Rebuilding the view

`App` derives `Tree` (forests of touched paths) from `Model`, and `rows`
(the flattened, expand-aware list on screen) from `Tree`. Refresh through
one of:

- `rebuild`: after `Model` changes. Rebuilds `Tree`.
- `refresh_rows`: after a display toggle (`zM`, touched-only). Keeps `Tree`.
- `land_on`: after the cursor moves. Keeps `Tree`, updates auto-focus.

`Tree::build` renumbers nodes, so `rebuild` finds the selection again by
`Node.key`. Don't mutate `Model` and then call `tree::build_rows` directly.

## Adding a new kind of target

Touched targets are classified by `model::Scope`:

1. Add the variant.
2. Fix the resulting compile errors in `tree.rs` (`segments`,
   `Tree::build`) and `ui/detail.rs` (`display_target`), which match
   `Scope` exhaustively.
3. Make `parse::locate` return the new variant. The compiler won't flag a
   missing producer; without this step the variant just never shows up.

## Errors

Problems with the data sources (no matching session dir, unwritable state
dir, unreadable state log) show up in the status bar, and antty keeps
running. Malformed session jsonl lines are skipped.
