//! Key handling: the per-mode key dispatch and Normal-mode vim grammar (counts, `z`/`g`/`Z` prefixes, the plain-vs-Ctrl modifier guard).

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseEvent, MouseEventKind};
use ratatui::layout::Position;

use crate::App;
use crate::cmdline::{self, CmdKind, CmdLine};
use crate::ui::{self, Mode};
use antty::timeline;
use antty::tree::Row;

/// Largest accepted Normal-mode count; further digits are ignored.
const MAX_COUNT: usize = 9999;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum WheelPane {
    Rows,
    DetailText,
    Diff,
    Help,
}

/// Index of the nth section header strictly after/before selection, or the end of the list.
fn section_target(rows: &[Row], selected: usize, forward: bool, n: usize) -> usize {
    if rows.is_empty() {
        return 0;
    }
    let nth = n.max(1) - 1;
    let sections = if forward {
        rows.iter()
            .enumerate()
            .skip(selected.saturating_add(1))
            .filter(|(_, row)| matches!(row, Row::Section(_)))
            .map(|(i, _)| i)
            .nth(nth)
    } else {
        rows[..selected.min(rows.len())]
            .iter()
            .enumerate()
            .rev()
            .filter(|(_, row)| matches!(row, Row::Section(_)))
            .map(|(i, _)| i)
            .nth(nth)
    };
    sections.unwrap_or(if forward { rows.len() - 1 } else { 0 })
}

impl App {
    pub(super) fn wheel_action(&self, event: MouseEvent) -> Option<(WheelPane, i64)> {
        let delta = match event.kind {
            MouseEventKind::ScrollUp => -3,
            MouseEventKind::ScrollDown => 3,
            _ => return None,
        };
        let point = Position::new(event.column, event.row);
        let layout = &self.layout;
        let pane = match self.ui.mode {
            Mode::Help if layout.help_area.contains(point) => WheelPane::Help,
            Mode::Diff if point.y < layout.status_area.y && point.x < layout.status_area.right() => WheelPane::Diff,
            Mode::Detail if layout.detail_area.is_some_and(|area| ui::detail::text_area(area).contains(point)) => {
                WheelPane::DetailText
            }
            Mode::Normal | Mode::Detail
                if layout.body_height > 0
                    && (layout.tree_area.contains(point) || layout.gantt_area.contains(point)) =>
            {
                WheelPane::Rows
            }
            _ => return None,
        };
        Some((pane, delta))
    }

    pub(super) fn apply_wheel(&mut self, action: Option<(WheelPane, i64)>) -> bool {
        let Some((pane, delta)) = action else { return false };
        match pane {
            WheelPane::Rows => {
                let before = self.ui.scroll;
                let max = self.rows.len().saturating_sub(self.layout.body_height.into());
                self.ui.scroll = before.saturating_add_signed(delta as isize).min(max);
                if before != self.ui.scroll {
                    self.wheel_view = true;
                    self.clamp_scroll();
                }
                before != self.ui.scroll
            }
            WheelPane::Diff | WheelPane::DetailText => {
                let app = self.as_ref();
                let max = ui::detail::selected_item(&app)
                    .map(|item| ui::detail::detail_lines(&app, &item).len().saturating_sub(1))
                    .unwrap_or_default()
                    .min(u16::MAX as usize) as u16;
                let scroll =
                    if pane == WheelPane::Diff { &mut self.ui.diff_scroll } else { &mut self.ui.detail_scroll };
                step_scroll(scroll, delta, max)
            }
            WheelPane::Help => {
                let page = self.layout.help_area.height.saturating_sub(2);
                let max = (ui::help::line_count() as u16).saturating_sub(page);
                step_scroll(&mut self.ui.help_scroll, delta, max)
            }
        }
    }

    pub(super) fn handle_key(&mut self, key: KeyEvent) -> bool {
        self.ui.flash = None;
        if self.ui.cmdline.is_some() {
            return self.handle_key_command(key);
        }
        if key.code == KeyCode::Char('z') && key.modifiers == KeyModifiers::CONTROL {
            self.ui.pending = None;
            self.ui.count = None;
            self.suspend = true;
            return false;
        }
        match self.ui.mode {
            Mode::Normal => self.handle_key_normal(key),
            Mode::Detail => self.handle_key_detail(key),
            Mode::Diff => self.handle_key_diff(key),
            Mode::Picker => self.handle_key_picker(key),
            Mode::Actions => self.handle_key_actions(key),
            Mode::Stats => self.handle_key_stats(key),
            Mode::Help => self.handle_key_help(key),
        }
    }

    /// Handle the `:` command line and `/` search line.
    fn handle_key_command(&mut self, key: KeyEvent) -> bool {
        match key.code {
            KeyCode::Esc => self.ui.cmdline = None,
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => self.ui.cmdline = None,
            KeyCode::Backspace => {
                if !self.ui.cmdline.as_mut().unwrap().backspace() {
                    self.ui.cmdline = None;
                }
            }
            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.ui.cmdline.as_mut().unwrap().clear();
            }
            KeyCode::Char('w') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.ui.cmdline.as_mut().unwrap().delete_word();
            }
            KeyCode::Up | KeyCode::Down => {
                let line = self.ui.cmdline.as_mut().unwrap();
                let history = match line.kind {
                    CmdKind::Command => &self.ui.command_history,
                    CmdKind::Search => &self.ui.search_history,
                };
                if key.code == KeyCode::Up { line.older(history) } else { line.newer(history) }
            }
            KeyCode::Enter => {
                let line = self.ui.cmdline.take().unwrap();
                match line.kind {
                    CmdKind::Command => {
                        let cmd = line.text.trim();
                        if !cmd.is_empty() {
                            cmdline::record(&mut self.ui.command_history, &line.text);
                        }
                        match cmd {
                            "q" | "q!" | "qa" | "qa!" | "quit" => return true,
                            "" => {}
                            other => self.ui.flash = Some(format!("not an editor command: {other}")),
                        }
                    }
                    CmdKind::Search => {
                        let query = if line.text.is_empty() {
                            self.ui.search_history.last().cloned()
                        } else {
                            cmdline::record(&mut self.ui.search_history, &line.text);
                            Some(line.text)
                        };
                        if let Some(query) = query
                            && self.search_step(&query, true, 1)
                        {
                            self.ui.search = Some(query);
                        }
                    }
                }
            }
            KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.ui.cmdline.as_mut().unwrap().push(c);
            }
            _ => {}
        }
        false
    }

    fn handle_key_normal(&mut self, key: KeyEvent) -> bool {
        let width = self.layout.gantt_width.max(1) as usize;
        let body = self.layout.body_height.max(1) as i64;
        let half_page = (body / 2).max(1);
        // Letter bindings fire only when typed plain (Shift is part of the letter); Ctrl chords
        // have their own table below, and any other modifier combination is a no-op — a vim
        // reflex must never trigger a different action.
        let plain = key.modifiers.difference(KeyModifiers::SHIFT).is_empty();
        if let Some(prefix) = self.ui.pending.take() {
            let count = self.ui.count.take();
            if plain {
                match (prefix, key.code) {
                    ('Z', KeyCode::Char('Z') | KeyCode::Char('Q')) => return true,
                    ('z', KeyCode::Char('a')) => self.toggle_expand(),
                    ('z', KeyCode::Char('o')) => self.fold(true, false),
                    ('z', KeyCode::Char('O')) => self.fold(true, true),
                    ('z', KeyCode::Char('c')) => self.fold(false, false),
                    ('z', KeyCode::Char('C')) => self.fold(false, true),
                    ('z', KeyCode::Char('M')) => {
                        self.ui.expand.collapse_all();
                        self.refresh_rows();
                    }
                    ('z', KeyCode::Char('R')) => {
                        self.ui.expand.expand_all();
                        self.refresh_rows();
                    }
                    ('z', KeyCode::Char('z')) => self.place_selected(body as usize / 2),
                    ('z', KeyCode::Char('t')) => self.place_selected(0),
                    ('z', KeyCode::Char('b')) => self.place_selected(body as usize - 1),
                    ('g', KeyCode::Char('g')) => self.goto_row(count.map_or(0, |c| c - 1)),
                    _ => {}
                }
            }
            return false;
        }
        if !plain {
            let count = self.ui.count.take();
            if key.modifiers == KeyModifiers::CONTROL {
                let n = count.unwrap_or(1) as i64;
                match key.code {
                    KeyCode::Char('d') => self.move_selected(count.map_or(half_page, |c| c as i64)),
                    KeyCode::Char('u') => self.move_selected(-count.map_or(half_page, |c| c as i64)),
                    KeyCode::Char('f') => self.scroll_viewport(n * (body - 2).max(1), true),
                    KeyCode::Char('b') => self.scroll_viewport(-n * (body - 2).max(1), true),
                    KeyCode::Char('e') => self.scroll_viewport(n, false),
                    KeyCode::Char('y') => self.scroll_viewport(-n, false),
                    KeyCode::Char('c') => self.ui.flash = Some("Type :q and press <Enter> to exit".into()),
                    _ => {}
                }
            }
            return false;
        }
        match key.code {
            KeyCode::Char(c @ '1'..='9') | KeyCode::Char(c @ '0') if c != '0' || self.ui.count.is_some() => {
                let digit = c.to_digit(10).unwrap() as usize;
                self.ui.count = Some((self.ui.count.unwrap_or(0) * 10 + digit).min(MAX_COUNT));
                return false;
            }
            KeyCode::Char(c @ ('z' | 'g' | 'Z')) => {
                self.ui.pending = Some(c);
                return false;
            }
            _ => {}
        }
        let count = self.ui.count.take();
        let n = count.unwrap_or(1);
        let ni = n as i64;
        match key.code {
            KeyCode::Char(':') => self.ui.cmdline = Some(CmdLine::new(CmdKind::Command)),
            KeyCode::Char('/') => self.ui.cmdline = Some(CmdLine::new(CmdKind::Search)),
            KeyCode::Char('j') | KeyCode::Down => self.move_selected(ni),
            KeyCode::Char('k') | KeyCode::Up => self.move_selected(-ni),
            KeyCode::Char('G') => self.goto_row(count.map_or(self.rows.len().saturating_sub(1), |c| c - 1)),
            KeyCode::Char('h') | KeyCode::Left => self.view.move_cursor_cols(-ni, width),
            KeyCode::Char('l') | KeyCode::Right => self.view.move_cursor_cols(ni, width),
            KeyCode::Char('H') => self.view.move_cursor_cols(-ni * (width as i64 / 2), width),
            KeyCode::Char('L') => self.view.move_cursor_cols(ni * (width as i64 / 2), width),
            KeyCode::Char('+') | KeyCode::Char('=') => {
                for _ in 0..n.min(timeline::ZOOM_LEVELS.len()) {
                    self.view.zoom_in();
                }
            }
            KeyCode::Char('-') => {
                for _ in 0..n.min(timeline::ZOOM_LEVELS.len()) {
                    self.view.zoom_out();
                }
            }
            KeyCode::Char('n') | KeyCode::Char('N') => {
                let forward = key.code == KeyCode::Char('n');
                if let Some(query) = self.ui.search.clone() {
                    self.search_step(&query, forward, n);
                } else {
                    self.jump_to_activity(forward, n);
                }
            }
            KeyCode::Char('}') => self.goto_row(section_target(&self.rows, self.ui.selected, true, n)),
            KeyCode::Char('{') => self.goto_row(section_target(&self.rows, self.ui.selected, false, n)),
            KeyCode::Char('0') => self.view.cursor = self.view.origin,
            KeyCode::Char('$') => {
                self.view.cursor = chrono::Utc::now();
                self.view.clamp_to_view(width);
            }
            KeyCode::Char('t') => self.view.anchor_right(chrono::Utc::now(), width),
            KeyCode::Char('f') => self.fit(),
            KeyCode::Char('c') => self.view.toggle_collapse_gaps(),
            KeyCode::Char(' ') => self.toggle_expand(),
            KeyCode::Char('a') => {
                self.ui.expand.auto_follow = !self.ui.expand.auto_follow;
                self.refresh_rows();
            }
            KeyCode::Enter => self.enter_detail(),
            KeyCode::Char('e') => self.open_in_editor(self.selected_file_target()),
            KeyCode::Char('T') => {
                self.ui.expand.touched_only = !self.ui.expand.touched_only;
                self.refresh_rows();
            }
            KeyCode::Char('p') => self.toggle_session_fold(),
            KeyCode::Char('w') => self.toggle_files_view(),
            KeyCode::Char('s') => self.open_picker(),
            KeyCode::Char('F') => self.open_actions(),
            KeyCode::Char('i') => self.ui.stats_panel = !self.ui.stats_panel,
            KeyCode::Char('I') => self.ui.mode = Mode::Stats,
            KeyCode::Char('?') => self.open_help(),
            KeyCode::Esc => self.ui.search = None,
            _ => {}
        }
        false
    }

    fn handle_key_stats(&mut self, key: KeyEvent) -> bool {
        match key.code {
            KeyCode::Char('q' | 'I') | KeyCode::Esc => self.ui.mode = Mode::Normal,
            KeyCode::Char('?') => self.open_help(),
            KeyCode::Char('v') => self.ui.stats_window = !self.ui.stats_window,
            KeyCode::Char('j') | KeyCode::Down => self.move_selected(1),
            KeyCode::Char('k') | KeyCode::Up => self.move_selected(-1),
            _ => {}
        }
        false
    }

    fn handle_key_help(&mut self, key: KeyEvent) -> bool {
        let page = self.layout.help_area.height.saturating_sub(2);
        let max = (ui::help::line_count() as u16).saturating_sub(page);
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let scroll = &mut self.ui.help_scroll;
        // A taller terminal may already be drawing at a lower offset.
        *scroll = (*scroll).min(max);
        match key.code {
            KeyCode::Char('q' | '?') | KeyCode::Esc => self.ui.mode = self.ui.help_return,
            KeyCode::Char('d') if ctrl => *scroll = scroll.saturating_add(page / 2).min(max),
            KeyCode::Char('u') if ctrl => *scroll = scroll.saturating_sub(page / 2),
            KeyCode::Char('j') | KeyCode::Down => *scroll = scroll.saturating_add(1).min(max),
            KeyCode::Char('k') | KeyCode::Up => *scroll = scroll.saturating_sub(1),
            KeyCode::Char('g') => *scroll = 0,
            KeyCode::Char('G') => *scroll = max,
            _ => {}
        }
        false
    }

    fn handle_key_detail(&mut self, key: KeyEvent) -> bool {
        let app_ref = self.as_ref();
        let n = ui::detail::resolve_items(&app_ref).len();
        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => self.ui.mode = Mode::Normal,
            KeyCode::Char('?') => self.open_help(),
            KeyCode::Char('e') => self.open_in_editor(self.detail_target()),
            KeyCode::Char('D') => self.open_in_delta(),
            KeyCode::Enter => {
                if n > 0 {
                    self.ui.mode = Mode::Diff;
                    self.ui.diff_scroll = 0;
                }
            }
            KeyCode::Char('j') | KeyCode::Down => {
                self.select_detail(if n > 0 { (self.ui.detail_selected + 1).min(n - 1) } else { 0 });
            }
            KeyCode::Char('k') | KeyCode::Up => self.select_detail(self.ui.detail_selected.saturating_sub(1)),
            KeyCode::Char('g') => self.select_detail(0),
            KeyCode::Char('G') => self.select_detail(n.saturating_sub(1)),
            KeyCode::Char('d') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.ui.detail_scroll = self.ui.detail_scroll.saturating_add(10);
            }
            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.ui.detail_scroll = self.ui.detail_scroll.saturating_sub(10);
            }
            _ => {}
        }
        false
    }

    /// Full-screen diff view (`Mode::Diff`), opened from Detail with `Enter`.
    fn handle_key_diff(&mut self, key: KeyEvent) -> bool {
        if key.code == KeyCode::Char('?') {
            self.open_help();
            return false;
        }
        let app_ref = self.as_ref();
        let total =
            ui::detail::selected_item(&app_ref).map(|it| ui::detail::detail_lines(&app_ref, &it).len()).unwrap_or(0);
        let page = self.layout.body_height.max(2) / 2;
        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => self.ui.mode = Mode::Detail,
            KeyCode::Char('e') => self.open_in_editor(self.detail_target()),
            KeyCode::Char('D') => self.open_in_delta(),
            KeyCode::Char('j') | KeyCode::Down => self.ui.diff_scroll = self.ui.diff_scroll.saturating_add(1),
            KeyCode::Char('k') | KeyCode::Up => self.ui.diff_scroll = self.ui.diff_scroll.saturating_sub(1),
            KeyCode::Char('d') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.ui.diff_scroll = self.ui.diff_scroll.saturating_add(page);
            }
            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.ui.diff_scroll = self.ui.diff_scroll.saturating_sub(page);
            }
            KeyCode::Char('g') => self.ui.diff_scroll = 0,
            KeyCode::Char('G') => self.ui.diff_scroll = total.saturating_sub(1).min(u16::MAX as usize) as u16,
            _ => {}
        }
        let max_scroll = total.saturating_sub(1).min(u16::MAX as usize) as u16;
        self.ui.diff_scroll = self.ui.diff_scroll.min(max_scroll);
        false
    }

    fn handle_key_picker(&mut self, key: KeyEvent) -> bool {
        let n = self.model.sessions.len();
        let half = (self.layout.picker_area.height.saturating_sub(2) / 2).max(1) as usize;
        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => self.ui.mode = Mode::Normal,
            KeyCode::Char('?') => self.open_help(),
            KeyCode::Enter => self.apply_picker(),
            KeyCode::Char('a') => self.ui.picker_selected = (0..n).collect(),
            KeyCode::Char(' ') => {
                if n > 0 {
                    let order = ui::picker::sorted_indices(&self.model);
                    let session_idx = order[self.ui.picker_cursor];
                    if !self.ui.picker_selected.remove(&session_idx) {
                        self.ui.picker_selected.insert(session_idx);
                    }
                }
            }
            KeyCode::Char('j') | KeyCode::Down => {
                if n > 0 {
                    self.ui.picker_cursor = (self.ui.picker_cursor + 1).min(n - 1);
                }
            }
            KeyCode::Char('k') | KeyCode::Up => self.ui.picker_cursor = self.ui.picker_cursor.saturating_sub(1),
            KeyCode::Char('g') => self.ui.picker_cursor = 0,
            KeyCode::Char('G') => self.ui.picker_cursor = n.saturating_sub(1),
            KeyCode::Char('d') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                if n > 0 {
                    self.ui.picker_cursor = (self.ui.picker_cursor + half).min(n - 1);
                }
            }
            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.ui.picker_cursor = self.ui.picker_cursor.saturating_sub(half)
            }
            _ => {}
        }
        false
    }

    fn handle_key_actions(&mut self, key: KeyEvent) -> bool {
        // ScriptsOnly is always the last row and uses a separate checkbox.
        let last = self.ui.actions_rows.len() - 1;
        let half = (self.layout.picker_area.height.saturating_sub(2) / 2).max(1) as usize;
        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => self.ui.mode = Mode::Normal,
            KeyCode::Char('?') => self.open_help(),
            KeyCode::Enter => self.apply_actions(),
            KeyCode::Char('a') => self.ui.actions_checked = (0..last).collect(),
            KeyCode::Char('n') => self.ui.actions_checked.clear(),
            KeyCode::Char(' ') => {
                let cursor = self.ui.actions_cursor;
                if cursor == last {
                    self.ui.actions_scripts = !self.ui.actions_scripts;
                } else if !self.ui.actions_checked.remove(&cursor) {
                    self.ui.actions_checked.insert(cursor);
                }
            }
            KeyCode::Char('j') | KeyCode::Down => self.ui.actions_cursor = (self.ui.actions_cursor + 1).min(last),
            KeyCode::Char('k') | KeyCode::Up => self.ui.actions_cursor = self.ui.actions_cursor.saturating_sub(1),
            KeyCode::Char('g') => self.ui.actions_cursor = 0,
            KeyCode::Char('G') => self.ui.actions_cursor = last,
            KeyCode::Char('d') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.ui.actions_cursor = (self.ui.actions_cursor + half).min(last);
            }
            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.ui.actions_cursor = self.ui.actions_cursor.saturating_sub(half);
            }
            _ => {}
        }
        false
    }
}

fn step_scroll(scroll: &mut u16, delta: i64, max: u16) -> bool {
    let next = (i64::from((*scroll).min(max)) + delta).clamp(0, i64::from(max)) as u16;
    next != std::mem::replace(scroll, next)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::UiState;
    use antty::attrib::Attributor;
    use antty::model::{EventDetail, FileEvent, Model, ParticipantKind, Scope, Session, TouchKind, TouchSource};
    use antty::timeline::View;
    use antty::tree::{self, FilesView, Tree};
    use ratatui::layout::Rect;

    fn test_app(root: &std::path::Path, state_dir: &std::path::Path) -> App {
        let mut model = Model::new(vec![root.to_path_buf()]);
        antty::disk::list_now(&mut model);
        let tree = Tree::build(&model, FilesView::Merged);
        let ui = UiState::new(false);
        let rows = tree::build_rows(&model, &tree, &ui.expand);
        let layout = ui::compute_layout(Rect::new(0, 0, 200, 50), Mode::Normal, false);
        let now = chrono::Utc::now();
        let mut view = View::new(true);
        view.fit(now - chrono::Duration::hours(1), now, layout.gantt_width as usize);
        let (tx, _) = std::sync::mpsc::sync_channel(16);
        App {
            model,
            tree,
            rows,
            view,
            ui,
            attributor: Attributor::new(state_dir, &[root.to_path_buf()]),
            idle_gap: 30,
            layout,
            wheel_view: false,
            session_budget: 12,
            launch: None,
            suspend: false,
            disk_tx: None,
            walk_pending: Default::default(),
            walk_stale: Default::default(),
            pending_seed: None,
            needs_rebuild: false,
            state_dir: state_dir.to_path_buf(),
            index_spec: antty::index::Spec {
                path: antty::index::path_for(state_dir, root),
                harness_roots: Vec::new(),
                idle_gap: 30,
                roots: vec![root.to_path_buf()],
            },
            tx,
        }
    }

    #[test]
    fn wheel_scrolls_shared_rows_without_moving_selection_or_consuming_a_count() {
        let base = std::env::temp_dir().join(format!("antty-keys-test-wheel-{}", std::process::id()));
        let root = base.join("root");
        std::fs::create_dir_all(&root).unwrap();
        for i in 0..64 {
            std::fs::write(root.join(format!("{i:02}.txt")), "").unwrap();
        }
        let mut app = test_app(&root, &base.join("state"));
        let screen = Rect::new(0, 0, 120, 25);
        app.ui.stats_panel = true;
        app.update_layout(screen);
        app.handle_key(KeyEvent::new(KeyCode::Char('2'), KeyModifiers::NONE));
        let wheel = |app: &mut App, area: Rect, kind| {
            let action = app.wheel_action(MouseEvent {
                kind,
                column: area.x + 1,
                row: area.y + 4,
                modifiers: KeyModifiers::NONE,
            });
            app.apply_wheel(action)
        };
        let (tree, gantt, stats) = (app.layout.tree_area, app.layout.gantt_area, app.layout.stats_area.unwrap());
        assert!(!wheel(&mut app, stats, MouseEventKind::ScrollDown));
        assert!(wheel(&mut app, tree, MouseEventKind::ScrollDown));
        assert!(wheel(&mut app, gantt, MouseEventKind::ScrollDown));
        app.rebuild();
        app.clamp_view();
        assert_eq!((app.ui.scroll, app.selected_row()), (6, Some(Row::Section("participants"))));

        app.handle_key(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE));
        assert_eq!((app.ui.scroll, app.ui.selected), (2, 2));
        drop(app);
        std::fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn participant_fold_keys_show_all_and_restore_selection() {
        let root = std::env::temp_dir().join(format!("antty-keys-test-fold-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let mut app = test_app(&root, &root.join("state"));
        let now = chrono::Utc::now();
        for i in 0..20 {
            let title = format!("s{i}");
            let file = root.join(format!("{title}.jsonl"));
            let at = now - chrono::Duration::hours(i as i64 + 1);
            let main = app.model.get_or_create_participant(&file, ParticipantKind::Main, title.clone(), Some(i), None);
            app.model.sessions.push(Session {
                file,
                id: String::new(),
                title,
                cwd: root.clone(),
                start: at,
                last_seen: at,
                end: None,
                main,
            });
        }
        app.rebuild();

        let count_sessions = |app: &App| {
            app.rows.iter().filter(|row| matches!(row, Row::Participant(pid, 0) if *pid != Model::YOU)).count()
        };
        let press = |app: &mut App, code| app.handle_key(KeyEvent::new(code, KeyModifiers::NONE));
        assert!(app.rows.contains(&Row::Fold));
        assert_eq!(count_sessions(&app), 12);

        press(&mut app, KeyCode::Char('p'));
        assert!(!app.rows.contains(&Row::Fold));
        assert_eq!(count_sessions(&app), 20);

        let last = Row::Participant(app.model.sessions[19].main, 0);
        app.land_on(Some(last));
        press(&mut app, KeyCode::Char('p'));
        assert_eq!(count_sessions(&app), 12);
        assert_eq!(app.selected_row(), Some(Row::Fold));
        press(&mut app, KeyCode::Enter);
        assert!(!app.ui.fold_sessions);
        assert_eq!(count_sessions(&app), 20);
        assert_eq!(app.selected_row(), Some(Row::Participant(app.model.sessions[12].main, 0)));

        press(&mut app, KeyCode::Char('p'));
        assert!(app.search_step("s19", true, 1));
        assert!(!app.rows.contains(&Row::Fold));
        assert_eq!(app.selected_row(), Some(last));
        drop(app);
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn stats_keys_toggle_panel_and_navigate_popup() {
        let root = std::env::temp_dir().join(format!("antty-keys-test-stats-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let mut app = test_app(&root, &root.join("state"));
        let press = |app: &mut App, c| app.handle_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
        assert!(!app.ui.stats_panel);
        assert!(!app.ui.stats_window);
        press(&mut app, 'v');
        assert!(!app.ui.stats_window);
        press(&mut app, 'i');
        assert!(app.ui.stats_panel);
        press(&mut app, 'I');
        assert_eq!(app.ui.mode, Mode::Stats);
        press(&mut app, 'v');
        assert!(app.ui.stats_window);
        let selected = app.ui.selected;
        press(&mut app, 'j');
        assert_eq!(app.ui.selected, selected + 1);
        press(&mut app, 'v');
        assert!(!app.ui.stats_window);
        press(&mut app, '?');
        assert_eq!(app.ui.mode, Mode::Help);
        assert_eq!(app.ui.view_mode(), Mode::Stats);
        press(&mut app, 'q');
        assert_eq!(app.ui.mode, Mode::Stats);
        press(&mut app, 'q');
        assert_eq!(app.ui.mode, Mode::Normal);
        press(&mut app, 'i');
        assert!(!app.ui.stats_panel);
        drop(app);
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn section_motions_stop_at_headers_or_list_ends() {
        let rows = [
            Row::Section("participants"),
            Row::Participant(Model::YOU, 0),
            Row::Participant(Model::YOU, 0),
            Row::Section("files"),
            Row::Node(0),
            Row::Node(0),
            Row::Section("mounts"),
            Row::Node(0),
        ];
        assert_eq!(section_target(&rows, 1, true, 1), 3);
        assert_eq!(section_target(&rows, 1, true, 2), 6);
        assert_eq!(section_target(&rows, 6, true, 1), 7);
        assert_eq!(section_target(&rows, 1, true, 5), 7);
        assert_eq!(section_target(&rows, 5, false, 1), 3);
        assert_eq!(section_target(&rows, 3, false, 1), 0);
        assert_eq!(section_target(&rows, 5, false, 5), 0);
        assert_eq!(section_target(&[], 0, true, 1), 0);
    }

    #[test]
    fn normal_mode_counts_prefixes_and_escape_move_the_selected_row() {
        let root = std::env::temp_dir().join(format!("antty-keys-test-vim-grammar-{}", std::process::id()));
        let state_dir = root.with_extension("state");
        std::fs::create_dir_all(&root).unwrap();
        for i in 0..8 {
            std::fs::write(root.join(format!("{i}.txt")), "").unwrap();
        }
        let mut app = test_app(&root, &state_dir);
        fn keys(app: &mut App, chars: &str) {
            for c in chars.chars() {
                app.handle_key_normal(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
            }
        }
        keys(&mut app, "5j");
        assert_eq!(app.ui.selected, 5);
        keys(&mut app, "g");
        assert_eq!(app.ui.selected, 5);
        keys(&mut app, "g");
        assert_eq!(app.ui.selected, 0);
        keys(&mut app, "3gg");
        assert_eq!(app.ui.selected, 2);
        keys(&mut app, "2");
        app.handle_key_normal(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        keys(&mut app, "j");
        assert_eq!(app.ui.selected, 3);
        drop(app);
        std::fs::remove_dir_all(&root).unwrap();
        std::fs::remove_dir_all(&state_dir).unwrap();
    }
    #[test]
    fn zz_and_zq_quit_but_other_z_chords_do_not() {
        let root = std::env::temp_dir().join(format!("antty-keys-test-z-{}", std::process::id()));
        let state_dir = root.with_extension("state");
        std::fs::create_dir_all(&root).unwrap();
        for i in 0..8 {
            std::fs::write(root.join(format!("{i}.txt")), "").unwrap();
        }
        let mut app = test_app(&root, &state_dir);
        let key = |code| KeyEvent::new(code, KeyModifiers::NONE);
        assert!(!app.handle_key(key(KeyCode::Char('Z'))));
        assert!(app.handle_key(key(KeyCode::Char('Z'))));
        assert!(!app.handle_key(key(KeyCode::Char('Z'))));
        assert!(app.handle_key(key(KeyCode::Char('Q'))));
        assert!(!app.handle_key(key(KeyCode::Char('Z'))));
        assert!(!app.handle_key(key(KeyCode::Char('x'))));
        let selected = app.ui.selected;
        assert!(!app.handle_key(key(KeyCode::Char('j'))));
        assert_eq!(app.ui.selected, selected + 1);
        assert!(!app.handle_key(key(KeyCode::Char('Z'))));
        assert!(!app.handle_key(key(KeyCode::Esc)));
        assert_eq!(app.ui.pending, None);
        drop(app);
        std::fs::remove_dir_all(&root).unwrap();
        std::fs::remove_dir_all(&state_dir).unwrap();
    }

    #[test]
    fn search_reveals_collapsed_matches_wraps_and_preserves_last_success() {
        let root = std::env::temp_dir().join(format!("antty-keys-test-search-{}", std::process::id()));
        let state_dir = root.with_extension("state");
        let _ = std::fs::remove_dir_all(&root);
        for dir in ["alpha", "beta"] {
            std::fs::create_dir_all(root.join(dir)).unwrap();
            std::fs::write(root.join(dir).join("needle.txt"), "").unwrap();
        }
        std::fs::write(root.join("top.txt"), "").unwrap();
        let mut app = test_app(&root, &state_dir);
        let key = |app: &mut App, code| {
            app.handle_key(KeyEvent::new(code, KeyModifiers::NONE));
        };
        key(&mut app, KeyCode::Char('/'));
        for c in "needle".chars() {
            key(&mut app, KeyCode::Char(c));
        }
        assert_eq!(app.selected_row(), Some(Row::Section("participants")), "typing does not jump");
        key(&mut app, KeyCode::Enter);
        let assert_parent = |app: &App, expected: &str| {
            let Row::Node(idx) = app.selected_row().unwrap() else { panic!("expected file row") };
            assert_eq!(app.tree.nodes[idx].name, "needle.txt");
            let parent = app.tree.nodes[idx].parent.unwrap();
            assert!(app.ui.expand.is_dir_expanded(&app.tree, parent));
            assert_eq!(app.tree.nodes[parent].name, expected);
        };
        assert_parent(&app, "alpha");
        key(&mut app, KeyCode::Char('n'));
        assert_parent(&app, "beta");
        key(&mut app, KeyCode::Char('n'));
        assert_parent(&app, "alpha");
        assert_eq!(app.ui.flash.as_deref(), Some("search hit BOTTOM, continuing at TOP"));
        key(&mut app, KeyCode::Char('/'));
        for c in "Needle".chars() {
            key(&mut app, KeyCode::Char(c));
        }
        key(&mut app, KeyCode::Enter);
        assert_eq!(app.ui.flash.as_deref(), Some("Pattern not found: Needle"));
        assert_eq!(app.ui.search.as_deref(), Some("needle"));
        key(&mut app, KeyCode::Esc);
        assert_eq!(app.ui.search, None);
        drop(app);
        std::fs::remove_dir_all(&root).unwrap();
        std::fs::remove_dir_all(&state_dir).unwrap();
    }

    #[test]
    fn help_opens_from_every_view_and_returns_there_with_positions_intact() {
        let root = std::env::temp_dir().join(format!("antty-keys-test-help-{}", std::process::id()));
        let state_dir = root.with_extension("state");
        std::fs::create_dir_all(&root).unwrap();
        for i in 0..8 {
            std::fs::write(root.join(format!("{i}.txt")), "").unwrap();
        }
        let mut app = test_app(&root, &state_dir);
        let key = |code| KeyEvent::new(code, KeyModifiers::NONE);
        assert!(app.rows.len() > 2);
        for mode in [Mode::Normal, Mode::Detail, Mode::Diff, Mode::Picker] {
            app.ui.mode = mode;
            app.ui.selected = 2;
            app.ui.scroll = 1;
            app.ui.detail_selected = 1;
            app.ui.detail_scroll = 2;
            app.ui.diff_scroll = 3;
            app.ui.picker_cursor = 1;
            app.ui.picker_selected.insert(0);
            assert!(!app.handle_key(key(KeyCode::Char('?'))));
            assert_eq!(app.ui.mode, Mode::Help);
            assert_eq!(app.ui.view_mode(), mode);
            assert!(!app.handle_key(key(KeyCode::Char('?'))));
            assert_eq!(app.ui.mode, mode);
            assert_eq!(app.ui.selected, 2);
            assert_eq!(app.ui.scroll, 1);
            assert_eq!(app.ui.detail_selected, 1);
            assert_eq!(app.ui.detail_scroll, 2);
            assert_eq!(app.ui.diff_scroll, 3);
            assert_eq!(app.ui.picker_cursor, 1);
            assert!(app.ui.picker_selected.contains(&0));
        }
        app.ui.mode = Mode::Normal;
        assert!(!app.handle_key(key(KeyCode::Char('/'))));
        assert!(!app.handle_key(key(KeyCode::Char('?'))));
        assert_eq!(app.ui.cmdline.as_ref().unwrap().text, "?");
        assert_eq!(app.ui.mode, Mode::Normal);
        drop(app);
        std::fs::remove_dir_all(&root).unwrap();
        std::fs::remove_dir_all(&state_dir).unwrap();
    }

    #[test]
    fn help_scrolls_within_bounds_and_resets_on_open() {
        let base = std::env::temp_dir().join(format!("antty-keys-test-help-scroll-{}", std::process::id()));
        let root = base.join("root");
        let state_dir = base.join("state");
        std::fs::create_dir_all(&root).unwrap();
        let mut app = test_app(&root, &state_dir);
        let page = app.layout.help_area.height - 2;
        let max = ui::help::line_count() as u16 - page;
        assert!(max > 0);

        let key = |code| KeyEvent::new(code, KeyModifiers::NONE);
        app.handle_key(key(KeyCode::Char('?')));
        for (code, modifiers, expected) in [
            (KeyCode::Char('j'), KeyModifiers::NONE, 1),
            (KeyCode::Char('k'), KeyModifiers::NONE, 0),
            (KeyCode::Char('k'), KeyModifiers::NONE, 0),
            (KeyCode::Down, KeyModifiers::NONE, 1),
            (KeyCode::Up, KeyModifiers::NONE, 0),
            (KeyCode::Char('G'), KeyModifiers::NONE, max),
            (KeyCode::Char('j'), KeyModifiers::NONE, max),
            (KeyCode::Char('g'), KeyModifiers::NONE, 0),
            (KeyCode::Char('d'), KeyModifiers::CONTROL, (page / 2).min(max)),
            (KeyCode::Char('u'), KeyModifiers::CONTROL, 0),
        ] {
            app.handle_key(KeyEvent::new(code, modifiers));
            assert_eq!(app.ui.help_scroll, expected, "{modifiers:?} {code:?}");
        }

        app.layout = ui::compute_layout(Rect::new(0, 0, 120, 30), app.ui.view_mode(), false);
        app.handle_key(key(KeyCode::Char('G')));
        assert!(app.ui.help_scroll > max);
        app.layout = ui::compute_layout(Rect::new(0, 0, 200, 50), app.ui.view_mode(), false);
        app.handle_key(key(KeyCode::Char('k')));
        assert_eq!(app.ui.help_scroll, max - 1);

        app.ui.help_scroll = 5;
        app.handle_key(key(KeyCode::Char('q')));
        assert_eq!(app.ui.mode, Mode::Normal);
        app.handle_key(key(KeyCode::Char('?')));
        assert_eq!(app.ui.mode, Mode::Help);
        assert_eq!(app.ui.help_scroll, 0);
        app.handle_key(key(KeyCode::Esc));
        assert_eq!(app.ui.mode, Mode::Normal);
        drop(app);
        std::fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn merged_editor_target_uses_existing_checkout_and_its_own_line() {
        let base = std::env::temp_dir().join(format!("antty-keys-test-merged-editor-{}", std::process::id()));
        let a = base.join("a");
        let b = base.join("b");
        let state_dir = base.join("state");
        std::fs::create_dir_all(&a).unwrap();
        std::fs::create_dir_all(&b).unwrap();
        std::fs::write(b.join("shared.rs"), "b").unwrap();
        let mut app = test_app(&a, &state_dir);
        app.model = Model::new(vec![a.clone(), b.clone()]);
        antty::disk::list_now(&mut app.model);
        let now = chrono::Utc::now();
        for (root, line, end) in [(0, 42, now), (1, 7, now - chrono::Duration::seconds(1))] {
            app.model.events.push(FileEvent {
                who: Model::YOU,
                rel: "shared.rs".into(),
                scope: Scope::Project(root),
                kind: TouchKind::Write,
                source: TouchSource::Tool("edit".into()),
                start: end,
                end,
                tool_call_id: None,
                detail: EventDetail::Diff(format!("@@ -1,1 +{line},1 @@\n-old\n+new")),
            });
        }
        app.tree = Tree::build(&app.model, FilesView::Merged);
        app.rows = tree::build_rows(&app.model, &app.tree, &app.ui.expand);
        let idx = app.tree.find_by_key("f*:shared.rs").unwrap();
        app.ui.selected = app.rows.iter().position(|&r| r == Row::Node(idx)).unwrap();
        assert_eq!(app.selected_file_target().unwrap(), (b.join("shared.rs"), Some(7)));

        std::fs::write(a.join("shared.rs"), "a").unwrap();
        antty::disk::list_now(&mut app.model);
        app.rebuild();
        assert_eq!(app.selected_file_target().unwrap(), (a.join("shared.rs"), Some(42)));
        drop(app);
        std::fs::remove_dir_all(base).unwrap();
    }
}
