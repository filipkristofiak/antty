mod attrib;
mod cli;
mod model;
mod parse;
mod sessions;
mod timeline;
mod tree;
mod ui;
mod watch;

use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::Duration;

use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use notify::RecommendedWatcher;
use ratatui::layout::Rect;

use attrib::Attributor;
use cli::Args;
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
        self.tree = Tree::build(&self.model);
        self.rows = tree::build_rows(&self.model, &self.tree, &self.ui.expand);
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
        self.clamp_scroll();
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

    fn toggle_expand(&mut self) {
        match self.selected_row() {
            Some(Row::Node(idx)) if self.tree.nodes[idx].is_dir => {
                self.ui.expand.toggle_dir(&self.tree.nodes[idx]);
            }
            Some(Row::Participant(pid, _)) => {
                if let Some(sidx) = self.model.participants[pid.0].session
                    && self.model.participants[pid.0].kind == model::ParticipantKind::Main {
                        self.ui.expand.toggle_session(sidx);
                    }
            }
            _ => {}
        }
        self.rebuild();
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

    fn jump_to_activity(&mut self, forward: bool) {
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
        let cursor = self.view.cursor;
        let candidate = if forward {
            times.into_iter().find(|t| *t > cursor)
        } else {
            times.into_iter().rev().find(|t| *t < cursor)
        };
        if let Some(t) = candidate {
            self.view.cursor = t;
            self.view.clamp_to_view(self.layout.gantt_width.max(1) as usize);
        }
    }

    fn handle_message(&mut self, msg: Msg) {
        match msg {
            Msg::Lines { participant_file, lines } => {
                let Some(dir) = project_dir_of(&participant_file, &self.sessions_root) else {
                    return;
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
            }
            Msg::Reset(path) => {
                if let Some(&who) = self.model.file_participant.get(&path) {
                    self.model.clear_participant_data(who);
                    self.rebuild();
                }
            }
            Msg::Fs(raw) => {
                self.attributor.push(raw);
            }
            Msg::Tick => {
                let pushed = self.attributor.classify_and_apply(&mut self.model, chrono::Utc::now());
                if pushed > 0 {
                    self.rebuild();
                }
            }
        }
    }

    fn handle_key(&mut self, key: KeyEvent) -> bool {
        match self.ui.mode {
            Mode::Normal => self.handle_key_normal(key),
            Mode::Detail => self.handle_key_detail(key),
            Mode::Picker => self.handle_key_picker(key),
            Mode::Help => {
                if matches!(key.code, KeyCode::Char('q') | KeyCode::Esc | KeyCode::Char('?')) {
                    self.ui.mode = Mode::Normal;
                }
                false
            }
        }
    }

    fn handle_key_normal(&mut self, key: KeyEvent) -> bool {
        let width = self.layout.gantt_width.max(1) as usize;
        let half_page = (self.layout.body_height.max(1) / 2).max(1) as i64;
        match key.code {
            KeyCode::Char('q') => return true,
            KeyCode::Char('j') | KeyCode::Down => self.move_selected(1),
            KeyCode::Char('k') | KeyCode::Up => self.move_selected(-1),
            KeyCode::Char('g') => self.move_selected(-(self.rows.len() as i64)),
            KeyCode::Char('G') => self.move_selected(self.rows.len() as i64),
            KeyCode::Char('d') if key.modifiers.contains(KeyModifiers::CONTROL) => self.move_selected(half_page),
            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => self.move_selected(-half_page),
            KeyCode::Char('h') | KeyCode::Left => self.view.move_cursor_cols(-1, width),
            KeyCode::Char('l') | KeyCode::Right => self.view.move_cursor_cols(1, width),
            KeyCode::Char('H') => {
                self.view.move_cursor_cols(-(width as i64 / 2), width);
            }
            KeyCode::Char('L') => {
                self.view.move_cursor_cols(width as i64 / 2, width);
            }
            KeyCode::Char('+') | KeyCode::Char('=') => self.view.zoom_in(),
            KeyCode::Char('-') => self.view.zoom_out(),
            KeyCode::Char('t') => {
                self.view.cursor = chrono::Utc::now();
                self.view.clamp_to_view(width);
            }
            KeyCode::Char('f') => self.fit(),
            KeyCode::Char('n') => self.jump_to_activity(true),
            KeyCode::Char('N') => self.jump_to_activity(false),
            KeyCode::Char(' ') => self.toggle_expand(),
            KeyCode::Enter => self.enter_detail(),
            KeyCode::Char('T') => {
                self.ui.expand.touched_only = !self.ui.expand.touched_only;
                self.rebuild();
            }
            KeyCode::Char('s') => self.open_picker(),
            KeyCode::Char('?') => self.ui.mode = Mode::Help,
            _ => {}
        }
        false
    }

    fn handle_key_detail(&mut self, key: KeyEvent) -> bool {
        let app_ref = self.as_ref();
        let n = ui::detail::resolve_items(&app_ref).len();
        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => self.ui.mode = Mode::Normal,
            KeyCode::Char('j') | KeyCode::Down => {
                if n > 0 {
                    self.ui.detail_selected = (self.ui.detail_selected + 1).min(n - 1);
                }
                self.ui.detail_scroll = 0;
            }
            KeyCode::Char('k') | KeyCode::Up => {
                self.ui.detail_selected = self.ui.detail_selected.saturating_sub(1);
                self.ui.detail_scroll = 0;
            }
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

    fn handle_key_picker(&mut self, key: KeyEvent) -> bool {
        let n = self.model.sessions.len();
        match key.code {
            KeyCode::Esc => self.ui.mode = Mode::Normal,
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
            _ => {}
        }
        false
    }
}

fn main() -> anyhow::Result<()> {
    let args = Args::parse()?;

    let mut model = Model::new(args.project.clone());
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

    let (tx, rx) = mpsc::channel::<Msg>();
    let _tailer = sessions::spawn_tailer(args.sessions_dir.clone(), args.project.clone(), project_dir, offsets, tx.clone());

    let _watcher: Option<RecommendedWatcher> =
        if args.no_watch { None } else { watch::spawn_watcher(args.project.clone(), tx.clone()) };

    let mut ui_state = UiState::new(!args.no_watch);
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
    loop {
        while let Ok(msg) = rx.try_recv() {
            app.handle_message(msg);
        }

        if event::poll(Duration::from_millis(200))? {
            match event::read()? {
                Event::Key(key) if key.kind == KeyEventKind::Press => {
                    if app.handle_key(key) {
                        return Ok(());
                    }
                }
                Event::Resize(_, _) => {}
                _ => {}
            }
        }

        let size = terminal.size()?;
        let root_rect = Rect::new(0, 0, size.width, size.height);
        app.layout = ui::compute_layout(root_rect, app.ui.mode);
        app.clamp_scroll();
        app.view.clamp_to_view(app.layout.gantt_width.max(1) as usize);

        terminal.draw(|f| {
            let app_ref = app.as_ref();
            ui::draw(f, &app_ref);
        })?;
    }
}
