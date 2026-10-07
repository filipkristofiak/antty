//! Key handling: the per-mode key dispatch and Normal-mode vim grammar (counts, `z`/`g`/`Z` prefixes, the plain-vs-Ctrl modifier guard).

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::App;
use crate::cmdline::{self, CmdKind, CmdLine};
use crate::timeline;
use crate::tree::Row;
use crate::ui::{self, Mode};

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

impl App {
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
            Mode::Help => {
                if matches!(key.code, KeyCode::Char('q') | KeyCode::Esc | KeyCode::Char('?')) {
                    self.ui.mode = self.ui.help_return;
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
            KeyCode::Char('s') => self.open_picker(),
            KeyCode::Char('?') => self.open_help(),
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::attrib::Attributor;
    use crate::model::Model;
    use crate::timeline::View;
    use crate::tree::{self, Tree};
    use crate::ui::UiState;
    use ratatui::layout::Rect;

    fn test_app(root: &std::path::Path, state_dir: &std::path::Path) -> App {
        let model = Model::new(vec![root.to_path_buf()]);
        let tree = Tree::build(&model);
        let ui = UiState::new(false);
        let rows = tree::build_rows(&model, &tree, &ui.expand);
        let layout = ui::compute_layout(Rect::new(0, 0, 200, 50), Mode::Normal);
        let now = chrono::Utc::now();
        let mut view = View::new(true);
        view.fit(now - chrono::Duration::hours(1), now, layout.gantt_width as usize);
        App {
            model,
            tree,
            rows,
            view,
            ui,
            attributor: Attributor::new(state_dir, &[root.to_path_buf()]),
            idle_gap: 30,
            layout,
            launch: None,
            suspend: false,
        }
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
}
