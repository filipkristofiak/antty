# Condensing PARTICIPANTS

Status: shipped in 0.2.23 (files first).

## Problem

PARTICIPANTS used one row per session, so on a busy project it pushed FILES off screen:
this repository's 58 sessions filled the whole left pane at 200×50. People open antty mostly to
see which files agents touched; sessions are context for who changed a file and when.

## Shipped: files first

- Show at most 12 session rows by default, most recently active first. The budget is
  `min(12, max(0, body − 14))` from the Normal layout, leaving room for 10 FILES rows when
  space allows. Opening Detail does not change the budget.
- Sessions active in the last two hours always show, even if they exceed the budget.
  Expanded sessions can also take additional rows.
- Older sessions share a selectable `⋯ N older` row. When a file or directory is selected,
  `· N touched` counts folded sessions that touched it, and the label becomes bold.
- The Gantt strip counts folded sessions active in each column: `▄` for one, `▆` for two,
  `█` for three or more. A lone session keeps its colour; two or more use dark gray.
  The bar heights distinguish the counts under `NO_COLOR`.
- `p` toggles folding. Enter, Space or `zo` on the fold row shows all sessions and selects
  the newest previously folded session.
- Search unfolds the list when a match is inside the fold.
- If the selected session folds away, the cursor moves to the fold row.

A real frame at 112×34, with `Cargo.toml` selected (some FILES rows omitted):

```
┌files──────────────────────────────┐┌Gantt Chart Timeline─────────────────────────────────────────────────────┐
│                                   ││Sep 27   30    ~   ~4d18h       ~   ~    ~6d18h   ~      ~4d6h   ~6d Nov │
│                                   ││ M   T   W   T ~   ~  T   F   S ~ T ~ T  ~    W   ~    T ~   S   ~  S   M│
│PARTICIPANTS                       ││               ~   ~            ~   ~    ~        ~      ~       ~       │
│you                                ││ ●●  ●●  ●● ●● ~ ● ~ ●●● ●●   ● ~ ● ~ ●● ~ ●  ●●● ~ ●● ● ~ ●●  ● ~ ●● ●●●│
│▸ Queen overview · 11-22 11:53     ││               ~   ~            ~   ~    ~        ~      ~       ~     ██│
│▸ Merge help rebase · 11-22 11:57  ││               ~   ~            ~   ~    ~        ~      ~       ~    █  │
│▸ Help sections · 11-22 11:38      ││               ~   ~            ~   ~    ~        ~      ~       ~    █  │
│▸ Defrag fold · 11-22 11:14        ││               ~   ~            ~   ~    ~        ~      ~       ~    █  │
│▸ Stats panel · 11-22 00:05        ││               ~   ~            ~   ~    ~        ~      ~       ~  █ █  │
│▸ Ship stats · 11-22 10:49         ││               ~   ~            ~   ~    ~        ~      ~       ~    █  │
│▸ Action distribution · 11-22 10:12││               ~   ~            ~   ~    ~        ~      ~       ~  █    │
│▸ Merge install rebase · 11-21 17:…││               ~   ~            ~   ~    ~        ~      ~       ~ █     │
│▸ Hide filtered sessions · 11-21 1…││               ~   ~            ~   ~    ~        ~      ~       ~ █     │
│▸ Action filter · 11-21 16:58      ││               ~   ~            ~   ~    ~        ~      ~       ~ █     │
│▸ Detail panel output · 11-21 16:20││               ~   ~            ~   ~    ~        ~      ~       ~ █     │
│▸ Touched tint Soft background tin…││               ~   ~            ~   ~    ~        ~      ~     █ ~       │
│⋯ 71 older · 32 touched            ││ █▄  ▄▆  ██ ▆▆ ~ ▄ ~ ▆█▄ ▄█   ▄ ~ ▆ ~ █▆ ~ ▄  ▆▄▄ ~ ▄▆ ▄ ~ ▄█  █ ~       │  ← fold row
│FILES                              ││               ~   ~            ~   ~    ~        ~      ~       ~       │
│▸ .github                          ││          █  ░ ~   ~      ░     ~   ~ ░░ ~      ░ ~      ~     ░ ~ ░     │
│▸ docs                             ││ ██  ░█  ██ ██ ~ ░ ~ ██   ░     ~ █ ~ █  ~    █ █ ~  █ █ ~ ██  █ ~ ██ ██ │
│▸ src                              ││ █   ░█  ██ ░█ ~ ░ ~ ██░        ~ █ ~ █  ~    █   ~ ░█   ~ ░█  █ ~ ██ █░ │
│  Cargo.lock                       ││ ░        ░    ~   ~            ~   ~    ~        ~      ~       ~ ░     │
│  Cargo.toml                       ││ █    █  ██  █ ~   ~ █░   ░     ~ █ ~ █  ~    █   ~  █   ~  █  █ ~ ██ █  │  ← selected
└───────────────────────────────────┘└─────────────────────────────────────────────────────────────────────────┘
```

## Why files first

Sorted by latest activity, the session chart forms a diagonal: rank and time carry much the
same information. Folding by recency therefore loses little, and every visible session keeps
its title. A terminal cell has one glyph, one foreground and one background colour, so any
denser packing caps exact per-session colour at two sessions per cell.

## Alternatives considered

All were rendered from this repository's real sessions (2026-11-09).

| Candidate | First FILES row at 200×50 | Why not |
|---|---:|---|
| Unchanged | below the fold | FILES unreachable without scrolling |
| **Recent 12 + fold (shipped)** | **18** | older sessions are anonymous until unfolded |
| Lane packing (sessions share lanes over time) | 10 | new 2-D selection model; neighbours in a lane look alike |
| Half blocks, 2 sessions per row | 36 | live sessions lose their titles; FILES gone at 120×40 |
| Braille, 4 sessions per row | 21 | shared cells lose colour identity; dots too small |
| Fisheye (full rows near the cursor) | 28 | one `j` press redraws many unrelated rows |
| History first (recent head, paired history, tail fold) | 38 | identity only under the cursor; new scrolling model and keys |
| Hide sessions idle in the visible window | varies | rows change while panning and zooming |

The one-row fold strip saturates when zoomed out: the busiest column holds 3 folded sessions at
30m, 8 at 3h, 11 at 6h and 16 at 1d, so from 3h on most active columns are a full block. Taller
fold charts were drafted for this and left out to keep the fold one row.

## Later options

### Sessions tab in the side panel (roadmap)

- The left pane goes files-first: `you`, the selected session and the fold row.
- Browsing moves to a Sessions tab next to Stats in the right-hand side panel. The compact
  list is sorted by activity, with a small activity bar and date for each session. `Tab`
  focuses the panel; `j`/`k` select a session, and the Gantt follows.
- Requires panel focus, panel tabs and a second selection model.
- Below 110 columns the panel is hidden, so the fold row stays as the fallback.
- Revisit once the side panel has a use besides Stats, such as listing scripts per session.

```
┌files──────────────────────┐┌Gantt─────────────────────────────┐┌Sessions ▸ Stats────────────┐
│PARTICIPANTS               ││                                  ││ 78 · by activity           │
│you                        ││ ● ●●  ●   ●●●                    ││▶Stats panel     ▁▁▁▃█ 11-22│
│▸ Stats panel · 11-22      ││                       ██ █       ││ Merge install   ▁▁▅▂  11-21│
│⋯ 77 older · Tab → panel   ││ ▄▆█▄ ▄  ▆█▄▄ ▄  ▆ ▄              ││ Hide filtered   ▁▂▇   11-21│
│FILES                      ││                                  ││ Action filter   ▃█▁   11-21│
│  src/stats.rs             ││              ●  ●●               ││ …                          │
│  scripts/gen_fixtures.py  ││     ●     ●                      ││ scripts: 3 · calls 68      │
```

### One lens for all narrowing

The session picker (`s`, who), action filter (`F`, what) and stats range (`v`, when) are
separate toggles, and the range narrows only the stats numbers. A lens would combine them and
apply to the tree, the Gantt and the stats panel together, shown on one line above the tree.
PARTICIPANTS lists only sessions inside the lens; the fold row becomes "outside lens", so the
shipped fold carries over unchanged. The lens changes only through those keys, never while
panning. Needs a clear way to see and reset it.

```
│lens: scripts · range 11-14…11-22 · 9/78 sessions          [F what · v when · s who]   │
│PARTICIPANTS                        ││                                                 │
│you                                 ││ ●   ●●  ●     ●●●                               │
│▸ Stats panel · 11-22 00:05         ││                            ██ █                 │
│▸ Queen bundle b · 11-14 15:30      ││       ██                                        │
│  … 7 more in lens                  ││                                                 │
│⋯ 69 outside lens                   ││ ▄  ▆ ▄  ▄▄ ▆                                    │
│FILES  (scripts only)               ││                                                 │
│  scripts/gen_fixtures.py           ││     ●     ●                                     │
```

### Touched sessions as rows

A variant of the shipped fold: while a file is selected, folded sessions that touched it show
as their own rows (taken from the 12-row budget, so FILES does not move) instead of only the
`· N touched` count. Lets you trace a file's history without unfolding.

## Colour issues found

Measured on Kanagawa Wave; they affect the unfolded view too.

- `CURSOR_BG` has 1.14:1 contrast against the background, so the time cursor is nearly
  invisible.
- Six of seven session colours are below 3:1 on `SELECTED_BG`.
- Red/LightRed and Magenta/LightMagenta read as the same colour.
