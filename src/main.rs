mod cli;
mod cmdline;
mod delta;
mod editor;
mod keys;
mod search;
mod ui;

use std::collections::BTreeMap;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use antty::filter::{Action, ActionFilter, ActionKey, action_key};
use antty::{attrib, disk, index, model, sessions, timeline, tree, watch, worktree};
use crossterm::event::{self, DisableMouseCapture, EnableMouseCapture, Event, KeyEventKind};
use ratatui::layout::Rect;

use attrib::Attributor;
use cli::Args;
use model::{Model, Ts};
use sessions::{Harness, Msg};
use timeline::View;
use tree::{Row, Tree};
use ui::{ActionRow, AppRef, Mode, UiState};

/// Sessions active this recently stay as PARTICIPANTS rows even past the fold budget.
const LIVE_HOURS: i64 = 2;

/// Earliest/latest timestamp for fit, restricted to matching events when filtered.
fn model_time_range(model: &Model) -> Option<(Ts, Ts)> {
    if model.action_filter.is_some() {
        return model
            .events
            .iter()
            .filter(|e| model.event_visible(e))
            .map(|e| (e.start, e.end))
            .reduce(|(min, max), (start, end)| (min.min(start), max.max(end)));
    }
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
    idle_gap: i64,
    layout: ui::LayoutInfo,
    /// Wheel scrolling may leave the selected row outside the shared tree/Gantt viewport.
    wheel_view: bool,
    session_budget: usize,
    launch: Option<antty::tui::Invocation>,
    suspend: bool,
    disk_tx: Option<mpsc::Sender<usize>>,
    walk_pending: std::collections::HashSet<usize>,
    walk_stale: std::collections::HashSet<usize>,
    pending_seed: Option<(Ts, Vec<(std::path::PathBuf, String)>)>,
    needs_rebuild: bool,
    state_dir: std::path::PathBuf,
    index_spec: index::Spec,
    tx: mpsc::SyncSender<Msg>,
}

impl App {
    fn rebuild(&mut self) {
        self.needs_rebuild = false;
        let row = self.selected_row();
        // A `Row::Node` index is only stable within the current tree; capture its `key` (stable
        // across a rebuild, unlike the index `Tree::build` reassigns) so the selection can be
        // relocated by identity afterward. Participant/Section rows need no such translation.
        let node_key = match row {
            Some(Row::Node(idx)) => Some(self.tree.nodes[idx].key.clone()),
            _ => None,
        };
        self.model.recolor();
        self.tree = Tree::build(&self.model, self.ui.files_view);
        self.view.set_activity(timeline::visible_activity(&self.model));
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

    fn toggle_files_view(&mut self) {
        if self.model.roots.len() < 2 {
            self.ui.flash = Some("no other worktrees".into());
            return;
        }
        let other = self.selected_row();
        let key = match other {
            Some(Row::Node(idx)) => Some(self.tree.nodes[idx].key.clone()),
            _ => None,
        };
        self.ui.files_view = match self.ui.files_view {
            tree::FilesView::Merged => tree::FilesView::Separate,
            tree::FilesView::Separate => tree::FilesView::Merged,
        };
        self.tree = Tree::build(&self.model, self.ui.files_view);
        self.ui.expand.refresh_auto_open(&self.model, &self.tree);
        if let Some(idx) = key.as_deref().and_then(|k| tree::equivalent_node(&self.tree, self.model.roots.len(), k)) {
            self.reveal(Row::Node(idx));
        } else {
            self.finish_row_update(if key.is_some() { None } else { other });
        }
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
        self.wheel_view = false;
        self.ui.expand.refocus(&self.model, &self.tree, row);
        self.finish_row_update(row);
    }

    /// Shared tail of `rebuild`/`refresh_rows`/`land_on`: rebuild `self.rows` from the current
    /// tree/expand state, then relocate the selection to wherever `row` ended up (including a
    /// session that folded away), falling back to a plain clamp. A `Row::Node` that came from *before* a
    /// `Tree::build` (indices reassigned) must be translated via `Tree::find_by_key` first —
    /// see `rebuild`.
    fn finish_row_update(&mut self, row: Option<Row>) {
        self.ui.expand.session_fold = self.ui.fold_sessions.then(|| tree::SessionFold {
            budget: self.session_budget,
            live_since: chrono::Utc::now() - chrono::Duration::hours(LIVE_HOURS),
        });
        self.rows = tree::build_rows(&self.model, &self.tree, &self.ui.expand);
        if let Some(r) = row
            && let Some(pos) = self.rows.iter().position(|x| *x == r)
        {
            self.ui.selected = pos;
        } else if matches!(row, Some(Row::Participant(..)))
            && let Some(pos) = self.rows.iter().position(|r| *r == Row::Fold)
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
        if !self.wheel_view {
            if self.ui.selected < self.ui.scroll {
                self.ui.scroll = self.ui.selected;
            } else if self.ui.selected >= self.ui.scroll + h {
                self.ui.scroll = self.ui.selected + 1 - h;
            }
        }
        self.ui.scroll = self.ui.scroll.min(self.rows.len().saturating_sub(h));
        self.wheel_view &= !(self.ui.scroll..self.ui.scroll + h).contains(&self.ui.selected);
    }

    fn update_layout(&mut self, root: Rect) {
        let mode = self.ui.view_mode();
        self.layout = ui::compute_layout(root, mode, self.ui.stats_panel);
        // Detail shrinks the body without changing how many sessions should be listed.
        let normal_body = if mode == Mode::Detail {
            ui::compute_layout(root, Mode::Normal, false).body_height
        } else {
            self.layout.body_height
        };
        let budget = ui::session_budget(normal_body);
        if budget != self.session_budget {
            self.session_budget = budget;
            self.refresh_rows();
        }
    }

    fn clamp_view(&mut self) {
        self.clamp_scroll();
        self.view.clamp_to_view(self.layout.gantt_width.max(1) as usize);
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
        self.wheel_view = false;
        let h = self.layout.body_height.max(1) as usize;
        self.ui.scroll = scroll_placing(self.ui.selected, self.rows.len(), h, offset);
    }

    fn scroll_viewport(&mut self, delta: i64, carry: bool) {
        let len = self.rows.len();
        if len == 0 {
            return;
        }
        self.wheel_view = false;
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
        self.view.fit(min, max, self.layout.gantt_width.max(1) as usize);
    }

    fn selected_row(&self) -> Option<Row> {
        self.rows.get(self.ui.selected).copied()
    }

    fn open_in_editor(&mut self, target: Result<(std::path::PathBuf, Option<usize>), &'static str>) {
        match target {
            Err(msg) => self.ui.flash = Some(msg.into()),
            Ok((path, _)) if !path.is_file() => {
                self.ui.flash = Some(format!("no longer on disk: {}", path.display()));
            }
            Ok((path, line)) => {
                match editor::invocation(&path, line, std::env::var_os("NVIM"), std::env::var_os("EDITOR")) {
                    Ok(inv) => self.launch = Some(inv),
                    Err(msg) => self.ui.flash = Some(msg),
                }
            }
        }
    }

    fn open_in_delta(&mut self) {
        let Some(ui::DetailItem::Event(idx)) = ui::detail::selected_item(&self.as_ref()) else {
            self.ui.flash = Some("no diff for this item".into());
            return;
        };
        match delta::event_patch(&self.model, &self.model.events[idx]) {
            Ok(patch) => self.launch = Some(delta::invocation(patch)),
            Err(msg) => self.ui.flash = Some(msg.into()),
        }
    }

    fn selected_file_target(&self) -> Result<(std::path::PathBuf, Option<usize>), &'static str> {
        let Some(Row::Node(idx)) = self.selected_row() else { return Err("not a file") };
        let node = &self.tree.nodes[idx];
        if node.is_dir {
            return Err("not a file");
        }
        let (path, project_root) = if let Some(rel) = node.key.strip_prefix(tree::MERGED_FILES_PREFIX) {
            let root = (0..self.model.roots.len()).find(|&i| self.model.roots[i].path.join(rel).exists()).unwrap_or(0);
            (self.model.roots[root].path.join(rel), Some(root))
        } else if let Some((root, rel)) = tree::parse_files_key(&node.key) {
            (self.model.roots[root].path.join(rel), None)
        } else if let Some(&idx) = node.events.first() {
            (editor::event_path(&self.model, &self.model.events[idx]).ok_or("not a local file")?, None)
        } else {
            return Err("not a file");
        };
        let mut events: Vec<_> = node
            .events
            .iter()
            .map(|&idx| &self.model.events[idx])
            .filter(|e| {
                self.model.event_visible(e) && project_root.is_none_or(|root| e.scope == model::Scope::Project(root))
            })
            .collect();
        events.sort_by_key(|e| std::cmp::Reverse(e.end));
        let line = events.into_iter().find_map(|e| editor::event_line(&self.model, e));
        Ok((path, line))
    }

    fn detail_target(&self) -> Result<(std::path::PathBuf, Option<usize>), &'static str> {
        let Some(ui::DetailItem::Event(idx)) = ui::detail::selected_item(&self.as_ref()) else {
            return Err("not a file");
        };
        let e = &self.model.events[idx];
        let path = editor::event_path(&self.model, e).ok_or("not a local file")?;
        Ok((path, editor::event_line(&self.model, e)))
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
        if self.ui.fold_sessions && !self.rows.contains(&row) {
            self.ui.fold_sessions = false;
            self.land_on(Some(row));
        }
    }

    fn search_step(&mut self, query: &str, forward: bool, n: usize) -> bool {
        let full = search::full_rows(&self.model, &self.tree, self.ui.expand.touched_only);
        let matches: Vec<usize> = full
            .iter()
            .enumerate()
            .filter(|&(_, &row)| search::is_match(&tree::row_label(&self.model, &self.tree, row), query))
            .map(|(idx, _)| idx)
            .collect();
        let cur = self.selected_row().and_then(|row| full.iter().position(|&candidate| candidate == row));
        let Some((idx, wrapped)) = search::step(&matches, cur, forward, n) else {
            self.ui.flash = Some(format!("Pattern not found: {query}"));
            return false;
        };
        if wrapped {
            self.ui.flash = Some(
                if forward { "search hit BOTTOM, continuing at TOP" } else { "search hit TOP, continuing at BOTTOM" }
                    .into(),
            );
        }
        self.reveal(full[idx]);
        true
    }

    fn toggle_session_fold(&mut self) {
        self.ui.fold_sessions = !self.ui.fold_sessions;
        self.refresh_rows();
    }

    fn show_folded_sessions(&mut self) {
        let (_, folded) = tree::session_rows(&self.model, &self.ui.expand);
        self.ui.fold_sessions = false;
        self.land_on(folded.first().map(|&s| Row::Participant(self.model.sessions[s].main, 0)));
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
                        self.wheel_view = false;
                        self.ui.selected = pos;
                    }
                }
            }
            Some(Row::Participant(pid, _)) => {
                if let Some(sidx) = self.model.participants[pid.0].session
                    && self.model.participants[pid.0].kind == model::ParticipantKind::Main
                {
                    self.ui.expand.toggle_session(sidx);
                }
            }
            Some(Row::Fold) => return self.show_folded_sessions(),
            _ => {}
        }
        self.refresh_rows();
    }

    fn fold(&mut self, open: bool, recursive: bool) {
        let Some(row) = self.selected_row() else { return };
        if row == Row::Fold {
            if open {
                self.show_folded_sessions();
            }
            return;
        }
        if let Some(target) = self.ui.expand.fold(&self.model, &self.tree, row, open, recursive)
            && let Some(pos) = self.rows.iter().position(|r| *r == target)
        {
            self.wheel_view = false;
            self.ui.selected = pos;
        }
        self.refresh_rows();
    }

    fn enter_detail(&mut self) {
        if self.selected_row() == Some(Row::Fold) {
            self.show_folded_sessions();
            return;
        }
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

    fn open_help(&mut self) {
        self.ui.help_return = self.ui.mode;
        self.ui.mode = Mode::Help;
        self.ui.help_scroll = 0;
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

    fn open_actions(&mut self) {
        let filter = self.model.action_filter.as_ref();
        let mut fixed = [0; Action::ALL.len()];
        let mut fs_during = BTreeMap::new();
        for e in self.model.events.iter().filter(|e| self.model.visible(e.who)) {
            match action_key(e) {
                ActionKey::Fixed(action) => fixed[action as usize] += 1,
                ActionKey::FsDuring(tool) => *fs_during.entry(tool).or_default() += 1,
            }
        }
        if let Some(filter) = filter {
            for tool in &filter.fs_during {
                fs_during.entry(tool.as_str()).or_default();
            }
        }
        self.ui.actions_rows = Action::ALL
            .into_iter()
            .filter_map(|action| {
                let count = fixed[action as usize];
                (count > 0 || filter.is_some_and(|f| f.actions.contains(&action)))
                    .then_some(ActionRow::Fixed(action, count))
            })
            .chain(fs_during.into_iter().map(|(tool, count)| ActionRow::FsDuring(tool.to_owned(), count)))
            .chain(std::iter::once(ActionRow::ScriptsOnly))
            .collect();
        self.ui.actions_checked = self
            .ui
            .actions_rows
            .iter()
            .enumerate()
            .filter_map(|(i, row)| {
                match row {
                    ActionRow::Fixed(action, _) => filter.is_none_or(|f| f.actions.contains(action)),
                    ActionRow::FsDuring(tool, _) => filter.is_none_or(|f| f.fs_during.contains(tool)),
                    ActionRow::ScriptsOnly => false,
                }
                .then_some(i)
            })
            .collect();
        self.ui.actions_scripts = filter.is_some_and(|f| f.scripts_only);
        self.ui.actions_cursor = 0;
        self.ui.mode = Mode::Actions;
    }

    fn apply_actions(&mut self) {
        let scripts_only = self.ui.actions_scripts;
        self.model.action_filter = if !scripts_only && self.ui.actions_checked.len() == self.ui.actions_rows.len() - 1 {
            None
        } else {
            let mut filter = ActionFilter { scripts_only, ..ActionFilter::default() };
            for &i in &self.ui.actions_checked {
                match &self.ui.actions_rows[i] {
                    ActionRow::Fixed(action, _) => {
                        filter.actions.insert(*action);
                    }
                    ActionRow::FsDuring(tool, _) => {
                        filter.fs_during.insert(tool.clone());
                    }
                    ActionRow::ScriptsOnly => {}
                }
            }
            Some(filter)
        };
        self.ui.mode = Mode::Normal;
        self.rebuild();
    }

    fn jump_to_activity(&mut self, forward: bool, n: usize) {
        let Some(row) = self.selected_row() else { return };
        let mut times: Vec<Ts> = match row {
            Row::Node(idx) => self.tree.nodes[idx]
                .events
                .iter()
                .filter(|&&i| self.model.event_visible(&self.model.events[i]))
                .map(|&i| self.model.events[i].start)
                .collect(),
            Row::Participant(pid, _) if self.model.action_filter.is_some() => self
                .model
                .events
                .iter()
                .filter(|e| e.who == pid && self.model.event_visible(e))
                .map(|e| e.start)
                .collect(),
            Row::Participant(pid, _) => self.model.spans.iter().filter(|s| s.who == pid).map(|s| s.start).collect(),
            Row::Section(_) | Row::Fold => Vec::new(),
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
            Msg::Records { harness, project_dir, participant_file, records } => {
                if self.ui.status_extra.as_deref().is_some_and(|s| s.starts_with("no agent sessions")) {
                    self.ui.status_extra = None;
                }
                sessions::apply_records(
                    &mut self.model,
                    harness,
                    &project_dir,
                    &participant_file,
                    &records,
                    self.idle_gap,
                );
                self.needs_rebuild = true;
                true
            }
            Msg::Reset(path) => {
                if let Some(&who) = self.model.file_participant.get(&path) {
                    self.model.clear_participant_data(who);
                    self.needs_rebuild = true;
                    return true;
                }
                false
            }
            Msg::Fs(raw) => {
                if matches!(raw.kind, watch::FsKind::Created | watch::FsKind::Removed)
                    && let Some(tx) = &self.disk_tx
                    && let Some((r, _)) = self.model.project_rel(&raw.path)
                {
                    if self.walk_pending.insert(r) {
                        let _ = tx.send(r);
                    } else {
                        self.walk_stale.insert(r);
                    }
                }
                self.attributor.push(raw);
                false
            }
            Msg::Tick => {
                if self.ui.loading.is_some() {
                    return self.ui.view_mode() == Mode::Picker;
                }
                let pushed = self.attributor.classify_and_apply(&mut self.model, chrono::Utc::now());
                if pushed > 0 {
                    self.needs_rebuild = true;
                }
                if let Some(e) = self.attributor.persist_error.take() {
                    self.ui.status_extra = Some(e);
                    return true;
                }
                self.ui.view_mode() == Mode::Picker || pushed > 0
            }
            Msg::Disk { snapshot, walked } => {
                self.model.disk = *snapshot;
                for r in walked {
                    self.walk_pending.remove(&r);
                    if self.walk_stale.remove(&r)
                        && let Some(tx) = &self.disk_tx
                    {
                        self.walk_pending.insert(r);
                        let _ = tx.send(r);
                    }
                }
                self.needs_rebuild = true;
                true
            }
            Msg::Seed { at, files } => {
                if self.ui.loading.is_some() {
                    self.pending_seed = Some((at, files));
                } else {
                    for (rel, content) in files {
                        self.model.record_snapshot(model::Scope::Project(0), &rel, at, content);
                    }
                }
                false
            }
            Msg::Progress { done, total } => {
                let previous = self.ui.loading.map_or(0, |(done, _)| done);
                self.ui.loading = Some((previous.max(done), total));
                true
            }
            Msg::Notice(s) => {
                self.ui.status_extra = Some(s);
                true
            }
            Msg::Cached(model) => {
                let disk = std::mem::take(&mut self.model.disk);
                let session_roots = std::mem::take(&mut self.model.session_roots);
                self.model = *model;
                self.model.disk = disk;
                self.model.session_roots = session_roots;
                self.needs_rebuild = true;
                true
            }
            Msg::Loaded { found, tails } => {
                self.ui.loading = None;
                if !found {
                    let root = &self.model.roots[0].path;
                    self.ui.status_extra = Some(if self.model.roots.len() == 1 {
                        format!("no agent sessions for {}", root.display())
                    } else {
                        format!(
                            "no agent sessions for {} or its {} other worktrees",
                            root.display(),
                            self.model.roots.len() - 1
                        )
                    });
                }
                match tails.and_then(|tails| index::encode(&self.index_spec, &self.model, &tails)) {
                    Ok(bytes) => {
                        let path = self.index_spec.path.clone();
                        let tx = self.tx.clone();
                        std::thread::spawn(move || {
                            if let Err(e) = index::write(&path, &bytes) {
                                let _ = tx.send(Msg::Notice(format!("index not saved: {e}")));
                            }
                        });
                    }
                    Err(e) => self.ui.status_extra = Some(format!("index not saved: {e}")),
                }
                if let Some((at, files)) = self.pending_seed.take() {
                    for (rel, content) in files {
                        self.model.record_snapshot(model::Scope::Project(0), &rel, at, content);
                    }
                }
                if let Some(e) = attrib::replay(&mut self.model, &self.state_dir) {
                    self.ui.status_extra = Some(e);
                }
                self.fit();
                self.needs_rebuild = true;
                true
            }
            Msg::WatchError(e) => {
                self.ui.status_extra = Some(format!("watch: {e}"));
                true
            }
        }
    }
}

fn main() -> anyhow::Result<()> {
    let args = Args::parse()?;

    let roots = if args.no_worktrees { vec![args.project.clone()] } else { worktree::discover(&args.project) };
    let mut model = Model::new(roots.clone());
    model.session_roots = vec![args.omp_dir.clone(), args.claude_dir.clone()];
    let mut sources = Vec::new();
    let harnesses =
        [(Harness::Omp, &args.omp_dir), (Harness::Claude, &args.claude_dir), (Harness::Codex, &args.codex_dir)];
    for root in &roots {
        for (harness, dir) in &harnesses {
            sources.push(sessions::TailSource {
                harness: *harness,
                root: (*dir).clone(),
                project_root: root.clone(),
                states: Default::default(),
            });
        }
    }

    let attributor = Attributor::new(&args.state_dir, &roots);
    let status_extra = attributor.persist_error.clone();
    let source_count = sources.len();
    let index_spec = index::Spec {
        path: index::path_for(&args.state_dir, &args.project),
        harness_roots: harnesses.iter().map(|(_, dir)| (*dir).clone()).collect(),
        idle_gap: args.idle_gap,
        roots: roots.clone(),
    };

    let (tx, rx) = mpsc::sync_channel::<Msg>(256);
    let _tailer = sessions::spawn_tailer(sources, Some(index_spec.clone()), tx.clone());

    let disk_roots: Vec<_> = roots.iter().enumerate().map(|(i, r)| (r.clone(), model.nested_roots(i))).collect();
    let lister_tx = disk::spawn_lister(disk_roots.clone(), !args.no_watch, tx.clone());
    let disk_tx = (!args.no_watch).then_some(lister_tx);
    if !args.no_watch {
        watch::spawn_watchers(disk_roots, tx.clone());
    }

    let mut ui_state = UiState::new(!args.no_watch);
    ui_state.loading = Some((0, source_count));
    ui_state.files_view = if args.separate_worktrees { tree::FilesView::Separate } else { tree::FilesView::Merged };
    ui_state.status_extra = status_extra;
    ui_state.no_color = std::env::var_os("NO_COLOR").is_some_and(|v| !v.is_empty());
    if roots.len() > 1 {
        for i in 0..roots.len() {
            ui_state.expand.dir_overrides.insert(tree::files_key_prefix(i), true);
        }
    }

    let mut terminal = ratatui::init();
    let size = terminal.size()?;
    let root_rect = Rect::new(0, 0, size.width, size.height);
    let layout = ui::compute_layout(root_rect, ui_state.view_mode(), ui_state.stats_panel);

    let tree = Tree::build(&model, ui_state.files_view);
    let session_budget = ui::session_budget(layout.body_height);
    let mut app = App {
        model,
        tree,
        rows: Vec::new(),
        view: View::new(!args.no_collapse_gaps),
        ui: ui_state,
        attributor,
        idle_gap: args.idle_gap,
        layout,
        wheel_view: false,
        session_budget,
        launch: None,
        suspend: false,
        disk_tx,
        walk_pending: Default::default(),
        walk_stale: Default::default(),
        pending_seed: None,
        needs_rebuild: false,
        state_dir: args.state_dir,
        index_spec,
        tx,
    };
    app.refresh_rows();
    app.view.set_activity(timeline::visible_activity(&app.model));
    app.fit();

    let mut mouse = MouseCapture(false);
    let result = mouse
        .set(!args.no_mouse)
        .map_err(anyhow::Error::from)
        .and_then(|()| run_loop(&mut terminal, &mut app, rx, &mut mouse, root_rect));
    drop(mouse);
    ratatui::restore();
    result
}

struct MouseCapture(bool);

impl MouseCapture {
    fn set(&mut self, enabled: bool) -> std::io::Result<()> {
        if enabled && !self.0 {
            // Also attempt cleanup if enabling fails after writing only part of the sequence.
            self.0 = true;
            crossterm::execute!(std::io::stdout(), EnableMouseCapture)?;
        } else if !enabled && self.0 {
            crossterm::execute!(std::io::stdout(), DisableMouseCapture)?;
            self.0 = false;
        }
        Ok(())
    }

    fn hand_off<T>(&mut self, run: impl FnOnce() -> std::io::Result<T>) -> std::io::Result<T> {
        let enabled = self.0;
        self.set(false)?;
        let result = run()?;
        self.set(enabled)?;
        Ok(result)
    }
}

impl Drop for MouseCapture {
    fn drop(&mut self) {
        let _ = self.set(false);
    }
}

fn run_loop(
    terminal: &mut ratatui::DefaultTerminal,
    app: &mut App,
    rx: mpsc::Receiver<Msg>,
    mouse: &mut MouseCapture,
    mut root: Rect,
) -> anyhow::Result<()> {
    let mut dirty = true;
    let mut drawn_now_col = i64::MIN;
    const DRAIN_BUDGET: Duration = Duration::from_millis(30);
    const INPUT_BUDGET: Duration = Duration::from_millis(8);
    const MIN_LOADING_REFRESH: Duration = Duration::from_millis(250);
    let mut last_refresh = Instant::now();
    let mut refresh_delay = MIN_LOADING_REFRESH;
    loop {
        let drain_started = Instant::now();
        let mut backlog = false;
        while let Ok(msg) = rx.try_recv() {
            dirty |= app.handle_message(msg);
            if drain_started.elapsed() >= DRAIN_BUDGET {
                backlog = true;
                break;
            }
        }
        let loading = app.ui.loading.is_some();
        let refresh_due = !loading || last_refresh.elapsed() >= refresh_delay;
        let mut refresh_cost = Duration::ZERO;
        if app.needs_rebuild && refresh_due {
            let started = loading.then(Instant::now);
            app.rebuild();
            if let Some(started) = started {
                refresh_cost += started.elapsed();
            }
            dirty = true;
        }

        let timeout = if backlog {
            0
        } else if app.needs_rebuild {
            50
        } else {
            200
        };
        let mut input_changed = false;
        if event::poll(Duration::from_millis(timeout))? {
            let started = Instant::now();
            let mut wheel = None;
            loop {
                match event::read()? {
                    // Keep keyboard actions in order. A motion can refocus a participant and
                    // change the rows that the very next key sees.
                    Event::Key(key) if key.kind == KeyEventKind::Press => {
                        app.apply_wheel(wheel.take());
                        let layout_before = (app.ui.view_mode(), app.ui.stats_panel);
                        if app.handle_key(key) {
                            return Ok(());
                        }
                        if let Some(inv) = app.launch.take() {
                            app.ui.flash = if inv.suspend {
                                mouse.hand_off(|| antty::tui::run(terminal, &inv))?
                            } else {
                                antty::tui::run(terminal, &inv)?
                            };
                        }
                        let suspended = std::mem::take(&mut app.suspend);
                        if suspended {
                            app.ui.flash = mouse.hand_off(|| antty::tui::suspend(terminal))?;
                        }
                        if layout_before != (app.ui.view_mode(), app.ui.stats_panel) {
                            app.update_layout(root);
                        }
                        app.clamp_view();
                        input_changed = true;
                    }
                    Event::Mouse(event) if mouse.0 => {
                        if let Some((pane, delta)) = app.wheel_action(event) {
                            // Do not cancel opposite directions at a scroll boundary.
                            if pane == keys::WheelPane::OverlayArrows {
                                input_changed |= app.apply_wheel(Some((pane, delta)));
                                app.clamp_view();
                            } else if let Some((target, amount)) = &mut wheel
                                && *target == pane
                                && amount.signum() == delta.signum()
                            {
                                *amount += delta;
                            } else {
                                input_changed |= app.apply_wheel(wheel.replace((pane, delta)));
                            }
                        }
                    }
                    Event::Resize(width, height) => {
                        app.apply_wheel(wheel.take());
                        root = Rect::new(0, 0, width, height);
                        app.update_layout(root);
                        app.clamp_view();
                        input_changed = true;
                    }
                    _ => {}
                }
                if started.elapsed() >= INPUT_BUDGET || !event::poll(Duration::ZERO)? {
                    break;
                }
            }
            input_changed |= app.apply_wheel(wheel);
            dirty |= input_changed;
        }

        if !dirty && app.view.col_for(chrono::Utc::now()) == drawn_now_col {
            continue;
        }
        if loading && !refresh_due && !input_changed && drawn_now_col != i64::MIN {
            continue;
        }

        let size = terminal.size()?;
        let current_root = Rect::new(0, 0, size.width, size.height);
        if root != current_root {
            root = current_root;
            app.update_layout(root);
        }
        app.clamp_view();
        drawn_now_col = app.view.col_for(chrono::Utc::now());

        let draw_started = (loading && refresh_due).then(Instant::now);
        terminal.draw(|f| {
            let app_ref = app.as_ref();
            ui::draw(f, &app_ref);
        })?;
        if let Some(started) = draw_started {
            refresh_cost += started.elapsed();
            // Bound background view work to roughly one fifth of loading time.
            // Input-driven redraws bypass this cadence.
            refresh_delay = MIN_LOADING_REFRESH.max(refresh_cost * 4);
            last_refresh = Instant::now();
        }
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
}
