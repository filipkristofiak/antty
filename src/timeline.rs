use crate::model::Ts;

/// Seconds-per-column zoom levels, from most to least detailed.
pub const ZOOM_LEVELS: [i64; 8] = [10, 30, 60, 300, 900, 3600, 6 * 3600, 86400];

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
            30 => "30s",
            60 => "1m",
            300 => "5m",
            900 => "15m",
            3600 => "1h",
            21600 => "6h",
            86400 => "1d",
            _ => "?",
        }
    }

    /// Pick the smallest zoom level whose whole `[min, max]` range fits within `width` columns,
    /// falling back to the coarsest level. `origin` floors to a column boundary; `cursor` starts
    /// at `max`. If a session tailed within the last 60s, callers should pass `max = now`.
    pub fn fit(min: Ts, max: Ts, width: usize) -> View {
        let width = width.max(1) as i64;
        let range_secs = (max - min).num_seconds().max(1);
        let zoom_idx = ZOOM_LEVELS
            .iter()
            .position(|&s| range_secs / s <= width)
            .unwrap_or(ZOOM_LEVELS.len() - 1);
        let col_secs = ZOOM_LEVELS[zoom_idx];
        View { zoom_idx, origin: floor_to_secs(min, col_secs), cursor: max }
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

    fn set_zoom(&mut self, new_idx: usize) {
        let new_idx = new_idx.min(ZOOM_LEVELS.len() - 1);
        if new_idx == self.zoom_idx {
            return;
        }
        let screen_col = self.col_for(self.cursor);
        self.zoom_idx = new_idx;
        let new_secs = self.col_secs();
        self.origin = self.cursor - chrono::Duration::seconds(screen_col * new_secs);
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
        // 3600/60 = 60 <= 100, 3600/30 = 120 > 100 -> 60s is the smallest that fits.
        assert_eq!(view.col_secs(), 60);
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
