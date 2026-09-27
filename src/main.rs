mod attrib;
mod cli;
mod cmdline;
mod model;
mod parse;
mod search;
mod sessions;
mod snapshot;
mod timeline;
mod tree;
mod ui;
mod watch;

use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::Duration;

use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::layout::Rect;

use attrib::Attributor;
use cli::Args;
use cmdline::{CmdKind, CmdLine};
use model::{Model, Ts};
use sessions::Msg;
use timeline::View;
use tree::{Row, Tree};
use ui::{AppRef, Mode, UiState};

/// The direct child of `sessions_root` that `file` lives under, regardless of whether that dir
/// has been confirmed (via header cwd match) as belonging to the current project yet. Message
/// routing uses this instead of a cached "the" project dir so a session created after startup
/// (e.g. the live-demo scenario, brand-new project with zero history) is still classified
/// correctly on its very first line.
fn project_dir_of(file: &Path, sessions_root: &Path) -> Option<PathBuf> {
    let rel = file.strip_prefix(sessions_root).ok()?;
    let first = rel.components().next()?;
    Some(sessions_root.join(first.as_os_str()))
}

/// Earliest/latest timestamp across everything currently in the model, for the initial "fit".
fn model_time_range(model: &Model) -> Option<(Ts, Ts)> {
    let mut min: Option<Ts> = None;
    let mut max: Option<Ts> = None;
    let bump = |t: Ts, min: &mut Option<Ts>, max: &mut Option<Ts>| {
        *min = Some(min.map_or(t, |m| m.min(t)));
        *max = Some(max.map_or(t, |m| m.max(t)));
    };
    for s in &model.sessions {
        bump(s.start, &mut min, &mut max);
        if let Some(e) = s.end {
            bump(e, &mut min, &mut max);
        }
    }
    for e in &model.events {
        bump(e.start, &mut min, &mut max);
        bump(e.end, &mut min, &mut max);
    }
    for s in &model.spans {
        bump(s.start, &mut min, &mut max);
        bump(s.end, &mut min, &mut max);
    }
    for p in &model.prompts {
        bump(p.at, &mut min, &mut max);
    }
    match (min, max) {
        (Some(a), Some(b)) => Some((a, b)),
        _ => None,
    }
}

/// Scroll a `height`-row viewport over `len` rows by `delta`, returning `(scroll, selected)`.
/// `carry`: the selection moves by `delta` too (paging); otherwise it stays put unless it would
/// leave the viewport, in which case it's pushed to the nearest visible edge (vim `Ctrl-e`/`Ctrl-y`).
fn scrolled(scroll: usize, selected: usize, len: usize, height: usize, delta: i64, carry: bool) -> (usize, usize) {
    let max_scroll = len.saturating_sub(height);
    let new_scroll = (scroll as i64 + delta).clamp(0, max_scroll as i64) as usize;
    let new_sel = if carry {
        (selected as i64 + delta).clamp(0, len as i64 - 1) as usize
    } else {
        selected.clamp(new_scroll, (new_scroll + height - 1).min(len - 1))
    };
    (new_scroll, new_sel)
}

/// Largest accepted Normal-mode count; further digits are ignored.
const MAX_COUNT: usize = 9999;

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

/// Viewport scroll placing the selection `offset` rows below its top, clamped at list ends.
fn scroll_placing(selected: usize, len: usize, height: usize, offset: usize) -> usize {
    selected.saturating_sub(offset).min(len.saturating_sub(height))
}

/// Nth distinct activity after/before the cursor; clamp to the farthest available timestamp.
fn nth_activity(times: &[Ts], cursor: Ts, forward: bool, n: usize) -> Option<Ts> {
    if forward {
        times.iter().filter(|&&t| t > cursor).take(n.max(1)).last().copied()
    } else {
        times.iter().rev().filter(|&&t| t < cursor).take(n.max(1)).last().copied()
    }
}

struct App {
    model: Model,
    tree: Tree,
    rows: Vec<Row>,
    view: View,
    ui: UiState,
    attributor: Attributor,
    sessions_root: PathBuf,
    idle_gap: i64,
    layout: ui::LayoutInfo,
}

impl App {
    fn rebuild(&mut self) {
        let row = self.selected_row();
        // A `Row::Node` index is only stable within the current tree; capture its `key` (stable
        // across a rebuild, unlike the index `Tree::build` reassigns) so the selection can be
        // relocated by identity afterward. Participant/Section rows need no such translation.
        let node_key = match row {
            Some(Row::Node(idx)) => Some(self.tree.nodes[idx].key.clone()),
            _ => None,
        };
        self.tree = Tree::build(&self.model);
        // A background data refresh, not a cursor movement: recompute `auto_open` from the
        // *existing* `auto_focus` (see `refresh_rows`) rather than re-deriving it from `row` —
        // otherwise every incoming tail line while parked on a session row would undo a `zM`
        // that had just cleared it.
        self.ui.expand.refresh_auto_open(&self.model, &self.tree);
        let relocated_row = match row {
            Some(Row::Node(_)) => node_key.as_deref().and_then(|k| self.tree.find_by_key(k)).map(Row::Node),
            other => other,
        };
        self.finish_row_update(relocated_row);
    }

    /// Rebuild the row list in place (no disk walk, tree node indices unchanged) from the
    /// *current* `auto_focus` — a pure display toggle (expand/collapse, `touched_only`,
    /// `auto_follow`) that must not re-derive focus from wherever the cursor merely happens to
    /// be sitting (that would undo e.g. `zM` while still parked on a session row). Keeps the
    /// same logical row selected even if the toggle shifted its position.
    fn refresh_rows(&mut self) {
        let row = self.selected_row();
        self.ui.expand.refresh_auto_open(&self.model, &self.tree);
        self.finish_row_update(row);
    }

    /// The cursor landed on `row` (a movement, not a toggle): refresh `auto_focus` from it if
    /// it's a `Participant` row (`ExpandState::refocus` leaves it sticky otherwise), then rebuild
    /// rows and keep `row` selected even if that shifted its position.
    fn land_on(&mut self, row: Option<Row>) {
        self.ui.expand.refocus(&self.model, &self.tree, row);
        self.finish_row_update(row);
    }

    /// Shared tail of `rebuild`/`refresh_rows`/`land_on`: rebuild `self.rows` from the current
    /// tree/expand state, then relocate the selection to wherever `row` ended up (if it's still
    /// present), falling back to a plain clamp. A `Row::Node` that came from *before* a
    /// `Tree::build` (indices reassigned) must be translated via `Tree::find_by_key` first —
    /// see `rebuild`.
    fn finish_row_update(&mut self, row: Option<Row>) {
        self.rows = tree::build_rows(&self.model, &self.tree, &self.ui.expand);
        if let Some(r) = row
            && let Some(pos) = self.rows.iter().position(|x| *x == r)
        {
            self.ui.selected = pos;
        }
        if self.rows.is_empty() {
            self.ui.selected = 0;
        } else if self.ui.selected >= self.rows.len() {
            self.ui.selected = self.rows.len() - 1;
        }
        self.clamp_scroll();
    }

    fn clamp_scroll(&mut self) {
        let h = self.layout.body_height.max(1) as usize;
        if self.ui.selected < self.ui.scroll {
            self.ui.scroll = self.ui.selected;
        } else if self.ui.selected >= self.ui.scroll + h {
            self.ui.scroll = self.ui.selected + 1 - h;
        }
        let max_scroll = self.rows.len().saturating_sub(h);
        if self.ui.scroll > max_scroll {
            self.ui.scroll = max_scroll;
        }
    }

    fn move_selected(&mut self, delta: i64) {
        let len = self.rows.len();
        if len == 0 {
            return;
        }
        let cur = self.ui.selected as i64;
        let next = (cur + delta).clamp(0, len as i64 - 1);
        self.ui.selected = next as usize;
        let row = self.selected_row();
        self.land_on(row);
    }

    fn goto_row(&mut self, idx: usize) {
        self.move_selected(idx as i64 - self.ui.selected as i64);
    }

    fn place_selected(&mut self, offset: usize) {
        let h = self.layout.body_height.max(1) as usize;
        self.ui.scroll = scroll_placing(self.ui.selected, self.rows.len(), h, offset);
    }

    fn scroll_viewport(&mut self, delta: i64, carry: bool) {
        let len = self.rows.len();
        if len == 0 {
            return;
        }
        let h = self.layout.body_height.max(1) as usize;
        let (scroll, selected) = scrolled(self.ui.scroll, self.ui.selected, len, h, delta, carry);
        self.ui.scroll = scroll;
        if selected != self.ui.selected {
            self.ui.selected = selected;
            let row = self.selected_row();
            self.land_on(row);
        }
    }

    fn as_ref(&self) -> AppRef<'_> {
        AppRef { model: &self.model, tree: &self.tree, rows: &self.rows, view: &self.view, ui: &self.ui }
    }

    fn fit(&mut self) {
        let now = chrono::Utc::now();
        let (mut min, mut max) = model_time_range(&self.model).unwrap_or((now - chrono::Duration::hours(1), now));
        if max < min {
            std::mem::swap(&mut min, &mut max);
        }
        if now - max <= chrono::Duration::seconds(60) {
            max = now;
        }
        self.view = View::fit(min, max, self.layout.gantt_width.max(1) as usize);
    }

    fn selected_row(&self) -> Option<Row> {
        self.rows.get(self.ui.selected).copied()
    }

    /// Open the ancestors needed to show a match in its current forest.
    fn reveal(&mut self, row: Row) {
        match row {
            Row::Node(idx) => {
                let mut top = idx;
                while let Some(parent) = self.tree.nodes[top].parent {
                    top = parent;
                    self.ui.expand.dir_overrides.insert(self.tree.nodes[parent].key.clone(), true);
                }
                for (&session, roots) in &self.tree.session_files {
                    if roots.contains(&top) {
                        self.ui.expand.expanded_sessions.insert(session);
                    }
                }
            }
            Row::Participant(pid, _) if self.model.participants[pid.0].kind != model::ParticipantKind::Main => {
                if let Some(session) = self.model.participants[pid.0].session {
                    self.ui.expand.expanded_sessions.insert(session);
                }
            }
            _ => {}
        }
        self.land_on(Some(row));
    }

    fn search_step(&mut self, query: &str, forward: bool, n: usize) -> bool {
        let full = search::full_rows(&self.model, &self.tree, self.ui.expand.touched_only);
        let matches: Vec<usize> = full.iter().enumerate()
            .filter(|&(_, &row)| search::is_match(&tree::row_label(&self.model, &self.tree, row), query))
            .map(|(idx, _)| idx)
            .collect();
        let cur = self.selected_row().and_then(|row| full.iter().position(|&candidate| candidate == row));
        let Some((idx, wrapped)) = search::step(&matches, cur, forward, n) else {
            self.ui.flash = Some(format!("Pattern not found: {query}"));
            return false;
        };
        if wrapped {
            self.ui.flash = Some(if forward {
                "search hit BOTTOM, continuing at TOP"
            } else {
                "search hit TOP, continuing at BOTTOM"
            }.into());
        }
        self.reveal(full[idx]);
        true
    }

    fn toggle_expand(&mut self) {
        match self.selected_row() {
            Some(Row::Node(idx)) if self.tree.nodes[idx].is_dir => {
                self.ui.expand.toggle_dir(&self.tree, idx);
            }
            Some(Row::Node(idx)) => {
                // Neotree-style close-node on a file: collapse its parent dir and reselect it,
                // since the parent sits above the file and so survives the collapse.
                if let Some(p) = self.tree.nodes[idx].parent {
                    self.ui.expand.dir_overrides.insert(self.tree.nodes[p].key.clone(), false);
                    if let Some(pos) = self.rows.iter().position(|r| *r == Row::Node(p)) {
                        self.ui.selected = pos;
                    }
                }
            }
            Some(Row::Participant(pid, _)) => {
                if let Some(sidx) = self.model.participants[pid.0].session
                    && self.model.participants[pid.0].kind == model::ParticipantKind::Main {
                        self.ui.expand.toggle_session(sidx);
                    }
            }
            _ => {}
        }
        self.refresh_rows();
    }

    fn fold(&mut self, open: bool, recursive: bool) {
        let Some(row) = self.selected_row() else { return };
        if let Some(target) = self.ui.expand.fold(&self.model, &self.tree, row, open, recursive)
            && let Some(pos) = self.rows.iter().position(|r| *r == target)
        {
            self.ui.selected = pos;
        }
        self.refresh_rows();
    }

    fn enter_detail(&mut self) {
        if matches!(self.selected_row(), Some(Row::Section(_)) | None) {
            return;
        }
        self.ui.mode = Mode::Detail;
        let app_ref = self.as_ref();
        let items = ui::detail::resolve_items(&app_ref);
        self.ui.detail_selected = ui::detail::closest_to(&app_ref, &items, self.view.cursor);
        self.ui.detail_scroll = 0;
        self.sync_cursor_to_detail();
    }

    /// Put the gantt time cursor on the selected Detail item (scrolling the gantt if needed), so
    /// the main timeline follows navigation in the Detail list. No-op when the list is empty.
    fn sync_cursor_to_detail(&mut self) {
        let Some(t) = ({
            let app_ref = self.as_ref();
            ui::detail::selected_item(&app_ref).map(|it| ui::detail::item_time(&it, &app_ref))
        }) else {
            return;
        };
        self.view.cursor = t;
        self.view.clamp_to_view(self.layout.gantt_width.max(1) as usize);
    }

    /// Select Detail item `idx`, reset the text scroll, and move the gantt cursor to it.
    fn select_detail(&mut self, idx: usize) {
        self.ui.detail_selected = idx;
        self.ui.detail_scroll = 0;
        self.sync_cursor_to_detail();
    }

    fn open_picker(&mut self) {
        self.ui.mode = Mode::Picker;
        self.ui.picker_selected = if self.model.session_filter.is_empty() {
            (0..self.model.sessions.len()).collect()
        } else {
            self.model.session_filter.clone()
        };
        self.ui.picker_cursor = 0;
    }

    fn apply_picker(&mut self) {
        let n = self.model.sessions.len();
        if self.ui.picker_selected.len() >= n {
            self.model.session_filter.clear();
        } else if self.ui.picker_selected.is_empty() {
            // An empty HashSet means "all visible" elsewhere in the model; represent "show
            // none" with a sentinel index that never matches a real session instead.
            self.model.session_filter = std::collections::HashSet::from([usize::MAX]);
        } else {
            self.model.session_filter = self.ui.picker_selected.clone();
        }
        self.ui.mode = Mode::Normal;
        self.rebuild();
    }

    fn jump_to_activity(&mut self, forward: bool, n: usize) {
        let Some(row) = self.selected_row() else { return };
        let mut times: Vec<Ts> = match row {
            Row::Node(idx) => self.tree.nodes[idx]
                .events
                .iter()
                .filter(|&&i| self.model.visible(self.model.events[i].who))
                .map(|&i| self.model.events[i].start)
                .collect(),
            Row::Participant(pid, _) => self.model.spans.iter().filter(|s| s.who == pid).map(|s| s.start).collect(),
            Row::Section(_) => Vec::new(),
        };
        times.sort();
        times.dedup();
        let candidate = nth_activity(&times, self.view.cursor, forward, n);
        if let Some(t) = candidate {
            self.view.cursor = t;
            self.view.clamp_to_view(self.layout.gantt_width.max(1) as usize);
        }
    }

    fn handle_message(&mut self, msg: Msg) -> bool {
        match msg {
            Msg::Lines { participant_file, lines } => {
                let Some(dir) = project_dir_of(&participant_file, &self.sessions_root) else {
                    return false;
                };
                if self.ui.status_extra.as_deref().is_some_and(|s| s.starts_with("no omp sessions")) {
                    self.ui.status_extra = None;
                }
                let who = sessions::ensure_participant(&mut self.model, &participant_file, &dir);
                for line in lines {
                    if let Ok(v) = serde_json::from_str::<serde_json::Value>(&line) {
                        parse::ingest(&mut self.model, who, &v, self.idle_gap);
                    }
                }
                parse::flush_dirty_spans(&mut self.model, self.idle_gap);
                self.rebuild();
                true
            }
            Msg::Reset(path) => {
                if let Some(&who) = self.model.file_participant.get(&path) {
                    self.model.clear_participant_data(who);
                    self.rebuild();
                    return true;
                }
                false
            }
            Msg::Fs(raw) => {
                self.attributor.push(raw);
                false
            }
            Msg::Tick => {
                let pushed = self.attributor.classify_and_apply(&mut self.model, chrono::Utc::now());
                if pushed > 0 {
                    self.rebuild();
                }
                self.ui.mode == Mode::Picker || pushed > 0
            }
            Msg::WatchError(e) => {
                self.ui.status_extra = Some(format!("watch: {e}"));
                true
            }
        }
    }

    fn handle_key(&mut self, key: KeyEvent) -> bool {
        self.ui.flash = None;
        if self.ui.cmdline.is_some() {
            return self.handle_key_command(key);
        }
        match self.ui.mode {
            Mode::Normal => self.handle_key_normal(key),
            Mode::Detail => self.handle_key_detail(key),
            Mode::Diff => self.handle_key_diff(key),
            Mode::Picker => self.handle_key_picker(key),
            Mode::Help => {
                if matches!(key.code, KeyCode::Char('q') | KeyCode::Esc | KeyCode::Char('?')) {
                    self.ui.mode = Mode::Normal;
                }
                false
            }
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
                            && self.search_step(&query, true, 1) {
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
            KeyCode::Char(c @ ('z' | 'g')) => {
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
            KeyCode::Char(' ') => self.toggle_expand(),
            KeyCode::Char('a') => {
                self.ui.expand.auto_follow = !self.ui.expand.auto_follow;
                self.refresh_rows();
            }
            KeyCode::Enter => self.enter_detail(),
            KeyCode::Char('T') => {
                self.ui.expand.touched_only = !self.ui.expand.touched_only;
                self.refresh_rows();
            }
            KeyCode::Char('s') => self.open_picker(),
            KeyCode::Char('?') => self.ui.mode = Mode::Help,
            KeyCode::Esc => self.ui.search = None,
            _ => {}
        }
        false
    }

    fn handle_key_detail(&mut self, key: KeyEvent) -> bool {
        let app_ref = self.as_ref();
        let n = ui::detail::resolve_items(&app_ref).len();
        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => self.ui.mode = Mode::Normal,
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
        let app_ref = self.as_ref();
        let total = ui::detail::selected_item(&app_ref)
            .map(|it| ui::detail::detail_lines(&app_ref, &it).len())
            .unwrap_or(0);
        let page = self.layout.body_height.max(2) / 2;
        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => self.ui.mode = Mode::Detail,
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
}

fn main() -> anyhow::Result<()> {
    let args = Args::parse()?;

    let mut model = Model::new(args.project.clone());
    model.sessions_root = args.sessions_dir.clone();
    let (project_dir, offsets) = sessions::load_initial(&mut model, &args.sessions_dir, &args.project, args.idle_gap);

    let mut status_extra = None;
    if project_dir.is_none() {
        status_extra = Some(format!("no omp sessions for {}", args.project.display()));
    }

    let attributor = Attributor::new(&args.state_dir, &args.project);
    if let Some(e) = &attributor.persist_error {
        status_extra = Some(e.clone());
    }
    if let Some(e) = attrib::replay(&mut model, &args.state_dir, &args.project) {
        status_extra = Some(e);
    }
    if !args.no_watch {
        snapshot::seed_from_disk(&mut model, chrono::Utc::now());
    }

    let (tx, rx) = mpsc::channel::<Msg>();
    let _tailer = sessions::spawn_tailer(args.sessions_dir.clone(), args.project.clone(), project_dir, offsets, tx.clone());

    let mut watch_on = false;
    if !args.no_watch {
        match watch::spawn_watcher(args.project.clone(), tx.clone()) {
            Ok(()) => watch_on = true,
            Err(e) => status_extra = Some(format!("watch: {e}")),
        }
    }

    let mut ui_state = UiState::new(watch_on);
    ui_state.status_extra = status_extra;

    let mut terminal = ratatui::init();
    let size = terminal.size()?;
    let root_rect = Rect::new(0, 0, size.width, size.height);
    let layout = ui::compute_layout(root_rect, ui_state.mode);

    let tree = Tree::build(&model);
    let rows = tree::build_rows(&model, &tree, &ui_state.expand);
    let mut app = App {
        model,
        tree,
        rows,
        view: View::fit(chrono::Utc::now() - chrono::Duration::hours(1), chrono::Utc::now(), layout.gantt_width.max(1) as usize),
        ui: ui_state,
        attributor,
        sessions_root: args.sessions_dir.clone(),
        idle_gap: args.idle_gap,
        layout,
    };
    app.fit();

    let result = run_loop(&mut terminal, &mut app, rx);
    ratatui::restore();
    result
}

fn run_loop(
    terminal: &mut ratatui::DefaultTerminal,
    app: &mut App,
    rx: mpsc::Receiver<Msg>,
) -> anyhow::Result<()> {
    let mut dirty = true;
    let mut drawn_now_col = i64::MIN;
    loop {
        while let Ok(msg) = rx.try_recv() {
            dirty |= app.handle_message(msg);
        }

        if event::poll(Duration::from_millis(200))? {
            match event::read()? {
                Event::Key(key) if key.kind == KeyEventKind::Press => {
                    if app.handle_key(key) {
                        return Ok(());
                    }
                    dirty = true;
                }
                Event::Resize(_, _) => dirty = true,
                _ => {}
            }
        }

        if !dirty && app.view.col_for(chrono::Utc::now()) == drawn_now_col {
            continue;
        }

        let size = terminal.size()?;
        let root_rect = Rect::new(0, 0, size.width, size.height);
        app.layout = ui::compute_layout(root_rect, app.ui.mode);
        app.clamp_scroll();
        app.view.clamp_to_view(app.layout.gantt_width.max(1) as usize);
        drawn_now_col = app.view.col_for(chrono::Utc::now());

        terminal.draw(|f| {
            let app_ref = app.as_ref();
            ui::draw(f, &app_ref);
        })?;
        dirty = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scroll_by_one_pushes_selection_to_new_top() {
        assert_eq!(scrolled(0, 0, 30, 10, 1, false), (1, 1));
    }

    #[test]
    fn scroll_by_one_leaves_visible_selection_in_place() {
        assert_eq!(scrolled(5, 12, 30, 10, 1, false), (6, 12));
    }

    #[test]
    fn scroll_at_max_is_a_no_op() {
        assert_eq!(scrolled(20, 29, 30, 10, 1, false), (20, 29));
    }

    #[test]
    fn scroll_up_by_one_pushes_selection_to_new_bottom() {
        assert_eq!(scrolled(10, 19, 30, 10, -1, false), (9, 18));
    }

    #[test]
    fn page_down_carries_selection() {
        assert_eq!(scrolled(0, 3, 30, 10, 8, true), (8, 11));
    }

    #[test]
    fn page_down_on_last_page_clamps_scroll_and_selection() {
        assert_eq!(scrolled(20, 25, 30, 10, 8, true), (20, 29));
    }

    #[test]
    fn page_down_with_fewer_rows_than_viewport() {
        assert_eq!(scrolled(0, 0, 5, 10, 8, true), (0, 4));
    }

    #[test]
    fn page_up_clamps_at_top() {
        assert_eq!(scrolled(3, 5, 30, 10, -8, true), (0, 0));
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
    fn placing_selected_clamps_at_both_ends() {
        assert_eq!(scroll_placing(25, 30, 10, 0), 20);
        assert_eq!(scroll_placing(15, 30, 10, 5), 10);
        assert_eq!(scroll_placing(3, 30, 10, 9), 0);
        assert_eq!(scroll_placing(2, 5, 10, 0), 0);
    }

    #[test]
    fn counted_activity_jumps_clamp_to_last_available_time() {
        let t0 = chrono::Utc::now();
        let t1 = t0 + chrono::Duration::seconds(10);
        let t2 = t1 + chrono::Duration::seconds(10);
        let times = [t0, t1, t2];
        let cursor = t0 + chrono::Duration::seconds(1);
        assert_eq!(nth_activity(&times, cursor, true, 1), Some(t1));
        assert_eq!(nth_activity(&times, cursor, true, 2), Some(t2));
        assert_eq!(nth_activity(&times, cursor, true, 5), Some(t2));
        assert_eq!(nth_activity(&times, cursor, false, 1), Some(t0));
        assert_eq!(nth_activity(&times, t2 + chrono::Duration::seconds(1), true, 1), None);
    }

    #[test]
    fn normal_mode_counts_prefixes_and_escape_move_the_selected_row() {
        let root = std::env::temp_dir().join(format!("antty-main-test-vim-grammar-{}", std::process::id()));
        let state_dir = root.with_extension("state");
        std::fs::create_dir_all(&root).unwrap();
        for i in 0..8 {
            std::fs::write(root.join(format!("{i}.txt")), "").unwrap();
        }
        let model = Model::new(root.clone());
        let tree = Tree::build(&model);
        let ui = UiState::new(false);
        let rows = tree::build_rows(&model, &tree, &ui.expand);
        let layout = ui::compute_layout(Rect::new(0, 0, 200, 50), Mode::Normal);
        let now = chrono::Utc::now();
        let mut app = App {
            model,
            tree,
            rows,
            view: View::fit(now - chrono::Duration::hours(1), now, layout.gantt_width as usize),
            ui,
            attributor: Attributor::new(&state_dir, &root),
            sessions_root: root.clone(),
            idle_gap: 30,
            layout,
        };
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
    fn search_reveals_collapsed_matches_wraps_and_preserves_last_success() {
        let root = std::env::temp_dir().join(format!("antty-main-test-search-{}", std::process::id()));
        let state_dir = root.with_extension("state");
        let _ = std::fs::remove_dir_all(&root);
        for dir in ["alpha", "beta"] {
            std::fs::create_dir_all(root.join(dir)).unwrap();
            std::fs::write(root.join(dir).join("needle.txt"), "").unwrap();
        }
        std::fs::write(root.join("top.txt"), "").unwrap();
        let model = Model::new(root.clone());
        let tree = Tree::build(&model);
        let ui = UiState::new(false);
        let rows = tree::build_rows(&model, &tree, &ui.expand);
        let layout = ui::compute_layout(Rect::new(0, 0, 200, 50), Mode::Normal);
        let now = chrono::Utc::now();
        let mut app = App {
            model,
            tree,
            rows,
            view: View::fit(now - chrono::Duration::hours(1), now, layout.gantt_width as usize),
            ui,
            attributor: Attributor::new(&state_dir, &root),
            sessions_root: root.clone(),
            idle_gap: 30,
            layout,
        };
        let key = |app: &mut App, code| { app.handle_key(KeyEvent::new(code, KeyModifiers::NONE)); };
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
}
