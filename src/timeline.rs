use crate::model::{Model, Ts};

/// Seconds-per-column zoom levels, from most to least detailed.
pub const ZOOM_LEVELS: [i64; 16] =
    [10, 15, 30, 60, 120, 180, 300, 600, 900, 1800, 3600, 2 * 3600, 3 * 3600, 6 * 3600, 12 * 3600, 86400];

/// Empty columns kept to the right of a right-anchored time (`t`, `f`).
pub const RIGHT_MARGIN: i64 = 3;
/// Idle runs at least this many grid columns long collapse into a break.
pub const MIN_GAP_COLS: i64 = 6;
/// One blank column on either side of the `~` marker.
const BREAK_WIDTH: i64 = 3;

/// One collapsed idle run, in grid columns (`epoch_secs.div_euclid(col_secs)`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Gap {
    first: i64,
    end: i64,
    removed_before: i64,
}

/// What one screen column shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Column {
    /// A normal `[start, end)` bucket, `col_secs` long.
    Time { start: Ts, end: Ts },
    /// The first or last idle grid column around a collapsed break.
    GapPad { start: Ts, end: Ts },
    /// The entire collapsed idle run `[start, end)`, drawn with `~` in its middle column.
    Break { start: Ts, end: Ts },
}

impl Column {
    pub fn range(self) -> (Ts, Ts) {
        match self {
            Column::Time { start, end } | Column::GapPad { start, end } | Column::Break { start, end } => (start, end),
        }
    }
}

/// Merged, sorted visible-activity intervals as inclusive epoch-second pairs.
pub fn visible_activity(model: &Model) -> Vec<(i64, i64)> {
    let mut activity: Vec<_> = model
        .events
        .iter()
        .filter(|e| model.visible(e.who))
        .map(|e| (e.start.timestamp().min(e.end.timestamp()), e.start.timestamp().max(e.end.timestamp())))
        .chain(
            model
                .spans
                .iter()
                .filter(|s| model.visible(s.who))
                .map(|s| (s.start.timestamp().min(s.end.timestamp()), s.start.timestamp().max(s.end.timestamp()))),
        )
        .chain(model.prompts.iter().filter(|p| model.session_visible(p.session)).map(|p| {
            let at = p.at.timestamp();
            (at, at)
        }))
        .collect();
    activity.sort_unstable();
    let mut merged: Vec<(i64, i64)> = Vec::with_capacity(activity.len());
    for (start, end) in activity {
        if let Some(last) = merged.last_mut()
            && start <= last.1
        {
            last.1 = last.1.max(end);
            continue;
        }
        merged.push((start, end));
    }
    merged
}

fn compute_gaps(activity: &[(i64, i64)], secs: i64) -> Vec<Gap> {
    let mut gaps = Vec::new();
    let mut prev_last: Option<i64> = None;
    let mut removed_before = 0;
    for &(start, end) in activity {
        let (a, b) = (start.div_euclid(secs), end.div_euclid(secs));
        if let Some(prev) = prev_last {
            let len = a - prev - 1;
            if len >= MIN_GAP_COLS {
                gaps.push(Gap { first: prev + 1, end: a, removed_before });
                removed_before += len - BREAK_WIDTH;
            }
            prev_last = Some(prev.max(b));
        } else {
            prev_last = Some(b);
        }
    }
    gaps
}

/// Column a right-anchored time lands on: `RIGHT_MARGIN` columns in from the right edge,
/// or column 0 when the pane is too narrow.
fn anchor_col(width: usize) -> i64 {
    (width.max(1) as i64 - 1 - RIGHT_MARGIN).max(0)
}

pub struct View {
    zoom_idx: usize,
    pub origin: Ts,
    pub cursor: Ts,
    collapse_gaps: bool,
    activity: Vec<(i64, i64)>,
    gaps: Vec<Gap>,
}

fn floor_to_secs(t: Ts, secs: i64) -> Ts {
    let epoch = t.timestamp();
    let floored = epoch - epoch.rem_euclid(secs);
    chrono::DateTime::from_timestamp(floored, 0).unwrap_or(t)
}

impl View {
    /// Construct a view; callers must `fit` it before drawing.
    pub fn new(collapse_gaps: bool) -> View {
        View {
            zoom_idx: 0,
            origin: Ts::UNIX_EPOCH,
            cursor: Ts::UNIX_EPOCH,
            collapse_gaps,
            activity: Vec::new(),
            gaps: Vec::new(),
        }
    }

    fn recompute_gaps(&mut self) {
        self.gaps = if self.collapse_gaps { compute_gaps(&self.activity, self.col_secs()) } else { Vec::new() };
    }

    pub fn set_activity(&mut self, activity: Vec<(i64, i64)>) {
        self.activity = activity;
        self.recompute_gaps();
    }

    fn grid(&self, t: Ts) -> i64 {
        t.timestamp().div_euclid(self.col_secs())
    }

    fn display_col(&self, g: i64) -> i64 {
        let i = self.gaps.partition_point(|x| x.first <= g);
        if i == 0 {
            return g;
        }
        let gp = self.gaps[i - 1];
        if g < gp.end {
            let first_display = gp.first - gp.removed_before;
            if g == gp.first {
                first_display
            } else if g == gp.end - 1 {
                first_display + BREAK_WIDTH - 1
            } else {
                first_display + 1
            }
        } else {
            g - gp.removed_before - (gp.end - gp.first - BREAK_WIDTH)
        }
    }

    fn column_at(&self, d: i64) -> Column {
        let secs = self.col_secs();
        let i = self.gaps.partition_point(|x| x.first - x.removed_before <= d);
        let timestamp = |s| chrono::DateTime::from_timestamp(s, 0).unwrap_or(Ts::UNIX_EPOCH);
        if i > 0 {
            let gp = self.gaps[i - 1];
            let gap_col = d - (gp.first - gp.removed_before);
            if gap_col == 0 || gap_col == BREAK_WIDTH - 1 {
                let g = if gap_col == 0 { gp.first } else { gp.end - 1 };
                return Column::GapPad { start: timestamp(g * secs), end: timestamp((g + 1) * secs) };
            }
            if gap_col == 1 {
                return Column::Break { start: timestamp(gp.first * secs), end: timestamp(gp.end * secs) };
            }
            let g = d + gp.removed_before + (gp.end - gp.first - BREAK_WIDTH);
            return Column::Time { start: timestamp(g * secs), end: timestamp((g + 1) * secs) };
        }
        Column::Time { start: timestamp(d * secs), end: timestamp((d + 1) * secs) }
    }

    fn col_start(&self, d: i64) -> Ts {
        match self.column_at(d) {
            Column::Break { start, .. } => start + chrono::Duration::seconds(self.col_secs()),
            column => column.range().0,
        }
    }

    fn reanchor(&mut self, screen_col: i64) {
        self.origin = self.col_start(self.display_col(self.grid(self.cursor)) - screen_col);
    }

    pub fn collapse_gaps(&self) -> bool {
        self.collapse_gaps
    }

    pub fn toggle_collapse_gaps(&mut self) {
        let screen_col = self.col_for(self.cursor);
        self.collapse_gaps = !self.collapse_gaps;
        self.recompute_gaps();
        self.reanchor(screen_col);
    }
    pub fn col_secs(&self) -> i64 {
        ZOOM_LEVELS[self.zoom_idx]
    }

    pub fn zoom_label(&self) -> &'static str {
        match self.col_secs() {
            10 => "10s",
            15 => "15s",
            30 => "30s",
            60 => "1m",
            120 => "2m",
            180 => "3m",
            300 => "5m",
            600 => "10m",
            900 => "15m",
            1800 => "30m",
            3600 => "1h",
            7200 => "2h",
            10800 => "3h",
            21600 => "6h",
            43200 => "12h",
            86400 => "1d",
            _ => "?",
        }
    }

    /// Put the cursor on `t` and pan so `t`'s column is `anchor_col(width)`. `origin` stays a
    /// multiple of `col_secs` (floored), which the header's hour-mark labelling relies on.
    pub fn anchor_right(&mut self, t: Ts, width: usize) {
        self.cursor = t;
        self.origin = self.col_start(self.display_col(self.grid(t)) - anchor_col(width));
    }

    /// Pick the smallest zoom level whose `[min, max]` range fits in `anchor_col(width) + 1`
    /// display columns, falling back to the coarsest level (where `min` may fall off the left
    /// edge). `max` is right-anchored via `anchor_right`; the cursor starts on `max`. If a session
    /// tailed within the last 60s, callers should pass `max = now`.
    pub fn fit(&mut self, min: Ts, max: Ts, width: usize) {
        for idx in 0..ZOOM_LEVELS.len() {
            self.zoom_idx = idx;
            self.recompute_gaps();
            if self.display_col(self.grid(max)) - self.display_col(self.grid(min)) <= anchor_col(width) {
                break;
            }
        }
        self.anchor_right(max, width);
    }

    /// The `[start, end)` time range shown by screen column `col` (0-based from `origin`).
    pub fn column(&self, col: i64) -> Column {
        self.column_at(self.display_col(self.grid(self.origin)) + col)
    }

    /// The (possibly negative or out-of-view) column index containing `t`.
    pub fn col_for(&self, t: Ts) -> i64 {
        self.display_col(self.grid(t)) - self.display_col(self.grid(self.origin))
    }

    /// Rezoom, keeping the cursor's screen column fixed. `origin` is re-derived from the
    /// grid-aligned start of a display column, not the raw cursor, so it stays a multiple of
    /// `col_secs` — the same invariant `anchor_right` maintains — otherwise hour marks in the
    /// header drift off the hour after a `+`/`-` press.
    fn set_zoom(&mut self, new_idx: usize) {
        let new_idx = new_idx.min(ZOOM_LEVELS.len() - 1);
        if new_idx == self.zoom_idx {
            return;
        }
        let screen_col = self.col_for(self.cursor);
        self.zoom_idx = new_idx;
        self.recompute_gaps();
        self.reanchor(screen_col);
    }

    pub fn zoom_in(&mut self) {
        if self.zoom_idx > 0 {
            self.set_zoom(self.zoom_idx - 1);
        }
    }

    pub fn zoom_out(&mut self) {
        self.set_zoom(self.zoom_idx + 1);
    }

    /// Move the cursor by `delta` display columns, panning if it leaves the visible pane.
    pub fn move_cursor_cols(&mut self, delta: i64, width: usize) {
        let secs = self.col_secs();
        let offset = self.cursor - floor_to_secs(self.cursor, secs);
        self.cursor = self.col_start(self.display_col(self.grid(self.cursor)) + delta) + offset;
        self.clamp_to_view(width);
    }

    pub fn clamp_to_view(&mut self, width: usize) {
        let col = self.col_for(self.cursor);
        let w = width.max(1) as i64;
        if col < 0 {
            self.origin = self.col_start(self.display_col(self.grid(self.cursor)));
        } else if col >= w {
            self.origin = self.col_start(self.display_col(self.grid(self.cursor)) - (w - 1));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Duration, TimeZone, Utc};

    fn fitted(min: Ts, max: Ts, width: usize) -> View {
        let mut view = View::new(true);
        view.fit(min, max, width);
        view
    }

    fn minute_view(activity: Vec<(i64, i64)>, t0: i64) -> View {
        let mut view = View::new(true);
        view.set_activity(activity);
        view.zoom_idx = 3;
        view.recompute_gaps();
        view.origin = Utc.timestamp_opt(t0, 0).unwrap();
        view
    }

    #[test]
    fn fit_picks_smallest_zoom_that_fits() {
        let min = Utc.timestamp_opt(1_700_000_000, 0).unwrap();
        let max = min + Duration::seconds(3600);
        let view = fitted(min, max, 100);
        // width 100 -> anchor col 96: 3600/60 = 60 <= 96, 3600/30 = 120 > 96.
        assert_eq!(view.col_secs(), 60);
    }

    #[test]
    fn fit_right_anchors_max_and_keeps_min_visible() {
        let min = Utc.timestamp_opt(1_700_000_000, 0).unwrap();
        let max = min + Duration::seconds(7200);
        let view = fitted(min, max, 100);
        // The old ladder would have picked 300 (fills 24 of 96 cols); the finer ladder picks 120.
        assert_eq!(view.col_secs(), 120);
        assert_eq!(view.col_for(max), 96);
        assert_eq!(view.col_for(min), 36);
        assert_eq!(view.cursor, max);
    }

    #[test]
    fn anchor_right_pins_time_three_cols_from_right_edge() {
        let min = Utc.timestamp_opt(1_700_000_000, 0).unwrap();
        let mut view = fitted(min, min + Duration::seconds(3600), 100);
        let t = min + Duration::seconds(50_000);
        view.anchor_right(t, 100);
        assert_eq!(view.col_for(t), 96);
        assert_eq!(view.cursor, t);

        view.anchor_right(t, 2);
        assert_eq!(view.col_for(t), 0);
    }

    #[test]
    fn zoom_keeps_origin_grid_aligned_after_fit() {
        // `now` timestamps aren't col_secs-aligned; a `-`/`+` after `fit`/`anchor_right` used to
        // rebuild `origin` from the raw (unfloored) cursor, drifting hour marks off the hour.
        let min = Utc.timestamp_opt(1_700_000_037, 0).unwrap();
        let max = min + Duration::seconds(3600);
        let mut view = fitted(min, max, 100);
        assert_eq!(view.origin.timestamp().rem_euclid(view.col_secs()), 0);

        view.zoom_out();
        assert_eq!(view.origin.timestamp().rem_euclid(view.col_secs()), 0);

        view.zoom_in();
        assert_eq!(view.origin.timestamp().rem_euclid(view.col_secs()), 0);
    }

    #[test]
    fn fit_falls_back_to_coarsest_zoom_for_huge_range() {
        let min = Utc.timestamp_opt(0, 0).unwrap();
        let max = min + Duration::days(365);
        let view = fitted(min, max, 50);
        assert_eq!(view.col_secs(), *ZOOM_LEVELS.last().unwrap());
    }

    #[test]
    fn zoom_round_trip_keeps_cursor_column() {
        let min = Utc.timestamp_opt(1_700_000_000, 0).unwrap();
        let max = min + Duration::seconds(3600);
        let mut view = fitted(min, max, 100);
        let before = view.col_for(view.cursor);
        view.zoom_in();
        view.zoom_out();
        let after = view.col_for(view.cursor);
        assert_eq!(before, after);
        assert_eq!(view.col_secs(), 60);
    }

    #[test]
    fn column_covers_col_secs_span() {
        let min = Utc.timestamp_opt(1_700_000_000, 0).unwrap();
        let view = fitted(min, min + Duration::seconds(600), 60);
        let Column::Time { start, end } = view.column(0) else { panic!("expected time column") };
        assert_eq!((end - start).num_seconds(), view.col_secs());
        assert_eq!(start, view.origin);
    }

    #[test]
    fn visible_activity_merges_sources_and_excludes_filtered_sessions() {
        use crate::model::{
            EventDetail, FileEvent, Participant, ParticipantId, ParticipantKind, Prompt, Scope, Span, TouchKind,
            TouchSource,
        };

        let t0 = 1_699_999_980;
        let at = |offset| Utc.timestamp_opt(t0 + offset, 0).unwrap();
        let mut model = Model::new(std::env::temp_dir().join("antty-timeline-test-visible-activity"));
        let agent = ParticipantId(1);
        model.participants.push(Participant {
            kind: ParticipantKind::Main,
            label: "agent".into(),
            session: Some(0),
            parent: None,
            color_idx: 1,
            file: None,
        });
        for (who, start, end) in [(Model::YOU, 0, 20), (agent, 420, 470)] {
            model.events.push(FileEvent {
                who,
                rel: "file".into(),
                scope: Scope::Project,
                kind: TouchKind::Write,
                source: TouchSource::Watcher,
                start: at(start),
                end: at(end),
                tool_call_id: None,
                detail: EventDetail::None,
            });
        }
        model.spans.push(Span { who: Model::YOU, start: at(15), end: at(50) });
        model.spans.push(Span { who: agent, start: at(430), end: at(480) });
        model.prompts.push(Prompt { session: 0, at: at(480) });
        assert_eq!(visible_activity(&model), vec![(t0, t0 + 50), (t0 + 420, t0 + 480)]);

        // The picker represents "show none" with an impossible session index; YOU remains visible.
        model.session_filter.insert(usize::MAX);
        assert_eq!(visible_activity(&model), vec![(t0, t0 + 50)]);
    }

    #[test]
    fn six_idle_columns_collapse_five_do_not() {
        let t0 = 1_699_999_980;
        let at = |offset| Utc.timestamp_opt(t0 + offset, 0).unwrap();
        let view = minute_view(vec![(t0, t0 + 59), (t0 + 420, t0 + 479)], t0);
        assert_eq!(view.column(1), Column::GapPad { start: at(60), end: at(120) });
        assert_eq!(view.column(2), Column::Break { start: at(60), end: at(420) });
        assert_eq!(view.column(3), Column::GapPad { start: at(360), end: at(420) });
        assert_eq!(view.column(4), Column::Time { start: at(420), end: at(480) });
        assert_eq!(view.col_for(at(60)), 1);
        assert_eq!(view.col_for(at(200)), 2);
        assert_eq!(view.col_for(at(360)), 3);
        assert_eq!(view.col_for(at(420)), 4);

        let view = minute_view(vec![(t0, t0 + 59), (t0 + 360, t0 + 419)], t0);
        assert_eq!(view.col_for(at(360)), 6);
        assert!((0..8).all(|c| matches!(view.column(c), Column::Time { .. })));
    }

    #[test]
    fn successive_gaps_keep_three_columns_each() {
        let t0 = 1_699_999_980;
        let at = |offset| Utc.timestamp_opt(t0 + offset, 0).unwrap();
        let view = minute_view(vec![(t0, t0 + 59), (t0 + 420, t0 + 479), (t0 + 840, t0 + 899)], t0);
        assert_eq!(view.column(4), Column::Time { start: at(420), end: at(480) });
        assert_eq!(view.column(5), Column::GapPad { start: at(480), end: at(540) });
        assert_eq!(view.column(6), Column::Break { start: at(480), end: at(840) });
        assert_eq!(view.column(7), Column::GapPad { start: at(780), end: at(840) });
        assert_eq!(view.column(8), Column::Time { start: at(840), end: at(900) });
        assert_eq!(view.col_for(at(840)), 8);
    }

    #[test]
    fn cursor_crosses_break_one_column_per_step() {
        let t0 = 1_699_999_980;
        let at = |offset| Utc.timestamp_opt(t0 + offset, 0).unwrap();
        let mut view = minute_view(vec![(t0, t0 + 59), (t0 + 420, t0 + 479)], t0);
        view.cursor = at(30);
        view.move_cursor_cols(1, 100);
        assert_eq!(view.col_for(view.cursor), 1);
        assert_eq!(view.cursor, at(90));
        view.move_cursor_cols(1, 100);
        assert_eq!(view.cursor, at(150));
        assert_eq!(view.col_for(view.cursor), 2);
        view.move_cursor_cols(1, 100);
        assert_eq!(view.cursor, at(390));
        assert_eq!(view.col_for(view.cursor), 3);
        view.move_cursor_cols(1, 100);
        assert_eq!(view.cursor, at(450));
        assert_eq!(view.col_for(view.cursor), 4);
        view.move_cursor_cols(-4, 100);
        assert_eq!(view.cursor, at(30));
    }

    #[test]
    fn toggle_keeps_cursor_column_and_grid_alignment() {
        let t0 = 1_699_999_980;
        let at = |offset| Utc.timestamp_opt(t0 + offset, 0).unwrap();
        let mut view = minute_view(vec![(t0, t0 + 59), (t0 + 420, t0 + 479)], t0);
        view.cursor = at(450);
        let before = view.col_for(view.cursor);
        view.toggle_collapse_gaps();
        assert_eq!(view.col_for(view.cursor), before);
        assert_eq!(view.origin.timestamp().rem_euclid(60), 0);
        assert_eq!(view.col_for(at(420)) - view.col_for(at(0)), 7);
        view.toggle_collapse_gaps();
        assert_eq!(view.col_for(at(420)) - view.col_for(at(0)), 4);
    }

    #[test]
    fn fit_counts_collapsed_columns() {
        let t0 = 1_699_999_980;
        let min = Utc.timestamp_opt(t0, 0).unwrap();
        let max = Utc.timestamp_opt(t0 + 259_800, 0).unwrap();
        let activity = vec![(t0, t0 + 600), (t0 + 259_200, t0 + 259_800)];
        let mut view = View::new(true);
        view.set_activity(activity.clone());
        view.fit(min, max, 100);
        assert_eq!(view.col_secs(), 15);
        assert_eq!(view.col_for(max) - view.col_for(min), 84);
        assert_eq!(view.col_for(max), 96);
        let mut linear = View::new(false);
        linear.set_activity(activity);
        linear.fit(min, max, 100);
        assert_eq!(linear.col_secs(), 3600);
    }
}
