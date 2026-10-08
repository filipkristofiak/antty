mod attrib;
mod claude;
mod cli;
mod cmdline;
mod codex;
mod delta;
mod editor;
mod keys;
mod model;
mod parse;
mod search;
mod sessions;
mod snapshot;
mod timeline;
mod tree;
mod ui;
mod watch;
mod worktree;

use std::sync::mpsc;
use std::time::Duration;

use crossterm::event::{self, Event, KeyEventKind};
use ratatui::layout::Rect;

use attrib::Attributor;
use cli::Args;
use model::{Model, Ts};
use sessions::{Harness, Msg};
use timeline::View;
use tree::{Row, Tree};
use ui::{AppRef, Mode, UiState};

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
    launch: Option<editor::Invocation>,
    suspend: bool,
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
                self.model.visible(e.who) && project_root.is_none_or(|root| e.scope == model::Scope::Project(root))
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
                    && self.model.participants[pid.0].kind == model::ParticipantKind::Main
                {
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

    fn open_help(&mut self) {
        self.ui.help_return = self.ui.mode;
        self.ui.mode = Mode::Help;
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
            Msg::Lines { harness, project_dir, participant_file, lines } => {
                if self.ui.status_extra.as_deref().is_some_and(|s| s.starts_with("no agent sessions")) {
                    self.ui.status_extra = None;
                }
                let who = harness.ensure_participant(&mut self.model, &participant_file, &project_dir);
                for line in lines {
                    if let Ok(v) = serde_json::from_str::<serde_json::Value>(&line) {
                        harness.ingest(&mut self.model, who, &v, &project_dir, self.idle_gap);
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
                self.ui.view_mode() == Mode::Picker || pushed > 0
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
    for root in &roots {
        for (harness, dir) in
            [(Harness::Omp, &args.omp_dir), (Harness::Claude, &args.claude_dir), (Harness::Codex, &args.codex_dir)]
        {
            sources.push(sessions::load_initial(&mut model, harness, dir, root, args.idle_gap));
        }
    }

    let mut status_extra = None;
    if sources.iter().all(|s| s.project_dir.is_none()) {
        status_extra = Some(if roots.len() == 1 {
            format!("no agent sessions for {}", args.project.display())
        } else {
            format!("no agent sessions for {} or its {} other worktrees", args.project.display(), roots.len() - 1)
        });
    }

    let attributor = Attributor::new(&args.state_dir, &roots);
    if let Some(e) = &attributor.persist_error {
        status_extra = Some(e.clone());
    }
    if let Some(e) = attrib::replay(&mut model, &args.state_dir) {
        status_extra = Some(e);
    }
    if !args.no_watch {
        snapshot::seed_from_disk(&mut model, chrono::Utc::now());
    }

    let (tx, rx) = mpsc::channel::<Msg>();
    let _tailer = sessions::spawn_tailer(sources, tx.clone());

    let mut watch_on = false;
    if !args.no_watch {
        for (i, root) in roots.iter().enumerate() {
            match watch::spawn_watcher(root.clone(), model.nested_roots(i), tx.clone()) {
                Ok(()) => watch_on = true,
                Err(e) => status_extra = Some(format!("watch: {e}")),
            }
        }
    }

    let mut ui_state = UiState::new(watch_on);
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
    let layout = ui::compute_layout(root_rect, ui_state.view_mode());

    let tree = Tree::build(&model, ui_state.files_view);
    let rows = tree::build_rows(&model, &tree, &ui_state.expand);
    let mut app = App {
        model,
        tree,
        rows,
        view: View::new(!args.no_collapse_gaps),
        ui: ui_state,
        attributor,
        idle_gap: args.idle_gap,
        layout,
        launch: None,
        suspend: false,
    };
    app.view.set_activity(timeline::visible_activity(&app.model));
    app.fit();

    let result = run_loop(&mut terminal, &mut app, rx);
    ratatui::restore();
    result
}

fn run_loop(terminal: &mut ratatui::DefaultTerminal, app: &mut App, rx: mpsc::Receiver<Msg>) -> anyhow::Result<()> {
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
                    if let Some(inv) = app.launch.take() {
                        app.ui.flash = editor::run(terminal, &inv)?;
                    }
                    if std::mem::take(&mut app.suspend) {
                        app.ui.flash = editor::suspend(terminal)?;
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
        app.layout = ui::compute_layout(root_rect, app.ui.view_mode());
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
