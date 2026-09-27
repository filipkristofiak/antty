use crate::model::Ts;

/// Seconds-per-column zoom levels, from most to least detailed.
pub const ZOOM_LEVELS: [i64; 16] =
    [10, 15, 30, 60, 120, 180, 300, 600, 900, 1800, 3600, 2 * 3600, 3 * 3600, 6 * 3600, 12 * 3600, 86400];

/// Empty columns kept to the right of a right-anchored time (`t`, `f`).
pub const RIGHT_MARGIN: i64 = 3;

/// Column a right-anchored time lands on: `RIGHT_MARGIN` columns in from the right edge,
/// or column 0 when the pane is too narrow.
fn anchor_col(width: usize) -> i64 {
    (width.max(1) as i64 - 1 - RIGHT_MARGIN).max(0)
}

pub struct View {
    zoom_idx: usize,
    pub origin: Ts,
    pub cursor: Ts,
}

fn floor_to_secs(t: Ts, secs: i64) -> Ts {
    let epoch = t.timestamp();
    let floored = epoch - epoch.rem_euclid(secs);
    chrono::DateTime::from_timestamp(floored, 0).unwrap_or(t)
}

impl View {
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
        let secs = self.col_secs();
        self.cursor = t;
        self.origin = floor_to_secs(t, secs) - chrono::Duration::seconds(anchor_col(width) * secs);
    }

    /// Pick the smallest zoom level whose `[min, max]` range fits in `anchor_col(width) + 1`
    /// columns, falling back to the coarsest level (where `min` may fall off the left edge, same
    /// as today). `max` is right-anchored via `anchor_right`; the cursor starts on `max`. If a
    /// session tailed within the last 60s, callers should pass `max = now`.
    pub fn fit(min: Ts, max: Ts, width: usize) -> View {
        let max_cols = anchor_col(width);
        let zoom_idx = ZOOM_LEVELS
            .iter()
            .position(|&s| max.timestamp().div_euclid(s) - min.timestamp().div_euclid(s) <= max_cols)
            .unwrap_or(ZOOM_LEVELS.len() - 1);
        let mut view = View { zoom_idx, origin: max, cursor: max };
        view.anchor_right(max, width);
        view
    }

    /// The `[start, end)` time range covered by screen column `col` (0-based from `origin`).
    pub fn bucket(&self, col: i64) -> (Ts, Ts) {
        let secs = self.col_secs();
        let start = self.origin + chrono::Duration::seconds(col * secs);
        let end = start + chrono::Duration::seconds(secs);
        (start, end)
    }

    /// The (possibly negative or out-of-view) column index containing `t`.
    pub fn col_for(&self, t: Ts) -> i64 {
        (t - self.origin).num_seconds().div_euclid(self.col_secs())
    }

    /// Rezoom, keeping the cursor's screen column fixed. `origin` is re-derived from
    /// `floor_to_secs(cursor, new_secs)`, not the raw cursor, so it stays a multiple of
    /// `new_secs` — the same grid-alignment invariant `anchor_right` maintains — otherwise hour
    /// marks in the header drift off the hour after a `+`/`-` press.
    fn set_zoom(&mut self, new_idx: usize) {
        let new_idx = new_idx.min(ZOOM_LEVELS.len() - 1);
        if new_idx == self.zoom_idx {
            return;
        }
        let screen_col = self.col_for(self.cursor);
        self.zoom_idx = new_idx;
        let new_secs = self.col_secs();
        self.origin = floor_to_secs(self.cursor, new_secs) - chrono::Duration::seconds(screen_col * new_secs);
    }

    pub fn zoom_in(&mut self) {
        if self.zoom_idx > 0 {
            self.set_zoom(self.zoom_idx - 1);
        }
    }

    pub fn zoom_out(&mut self) {
        self.set_zoom(self.zoom_idx + 1);
    }

    /// Move the cursor by `delta` columns, panning `origin` by one column at a time whenever the
    /// cursor would leave the visible `[origin, origin + width*col_secs)` window.
    pub fn move_cursor_cols(&mut self, delta: i64, width: usize) {
        self.cursor += chrono::Duration::seconds(delta * self.col_secs());
        self.clamp_to_view(width);
    }

    pub fn clamp_to_view(&mut self, width: usize) {
        let secs = self.col_secs();
        let width = width.max(1) as i64;
        loop {
            let col = self.col_for(self.cursor);
            if col < 0 {
                self.origin -= chrono::Duration::seconds(secs);
            } else if col >= width {
                self.origin += chrono::Duration::seconds(secs);
            } else {
                break;
            }
        }
    }

}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Duration, TimeZone, Utc};

    #[test]
    fn fit_picks_smallest_zoom_that_fits() {
        let min = Utc.timestamp_opt(1_700_000_000, 0).unwrap();
        let max = min + Duration::seconds(3600);
        let view = View::fit(min, max, 100);
        // width 100 -> anchor col 96: 3600/60 = 60 <= 96, 3600/30 = 120 > 96.
        assert_eq!(view.col_secs(), 60);
    }

    #[test]
    fn fit_right_anchors_max_and_keeps_min_visible() {
        let min = Utc.timestamp_opt(1_700_000_000, 0).unwrap();
        let max = min + Duration::seconds(7200);
        let view = View::fit(min, max, 100);
        // The old ladder would have picked 300 (fills 24 of 96 cols); the finer ladder picks 120.
        assert_eq!(view.col_secs(), 120);
        assert_eq!(view.col_for(max), 96);
        assert_eq!(view.col_for(min), 36);
        assert_eq!(view.cursor, max);
    }

    #[test]
    fn anchor_right_pins_time_three_cols_from_right_edge() {
        let min = Utc.timestamp_opt(1_700_000_000, 0).unwrap();
        let mut view = View::fit(min, min + Duration::seconds(3600), 100);
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
        let mut view = View::fit(min, max, 100);
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
        let view = View::fit(min, max, 50);
        assert_eq!(view.col_secs(), *ZOOM_LEVELS.last().unwrap());
    }

    #[test]
    fn zoom_round_trip_keeps_cursor_column() {
        let min = Utc.timestamp_opt(1_700_000_000, 0).unwrap();
        let max = min + Duration::seconds(3600);
        let mut view = View::fit(min, max, 100);
        let before = view.col_for(view.cursor);
        view.zoom_in();
        view.zoom_out();
        let after = view.col_for(view.cursor);
        assert_eq!(before, after);
        assert_eq!(view.col_secs(), 60);
    }

    #[test]
    fn bucket_covers_col_secs_span() {
        let min = Utc.timestamp_opt(1_700_000_000, 0).unwrap();
        let view = View::fit(min, min + Duration::seconds(600), 60);
        let (s, e) = view.bucket(0);
        assert_eq!((e - s).num_seconds(), view.col_secs());
        assert_eq!(s, view.origin);
    }
}
