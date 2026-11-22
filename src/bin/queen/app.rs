use std::collections::{BTreeMap, HashSet};
use std::ffi::OsString;
use std::path::{Component, Path, PathBuf};

use antty::model::Ts;

use crate::discover::{Drill, Kind, Origin, Project, ProjectKey, SessionInfo};

pub struct ProjectView {
    pub key: ProjectKey,
    pub kind: Kind,
    pub drill: Drill,
    pub sessions: Vec<SessionInfo>,
}

pub fn project_views(projects: Vec<Project<Vec<SessionInfo>>>) -> Vec<ProjectView> {
    projects
        .into_iter()
        .map(|p| {
            let mut sessions: Vec<_> = p.cwds.into_iter().flat_map(|(_, s)| s).collect();
            sessions.sort_by(|a, b| {
                b.span.map(|(_, e)| e).cmp(&a.span.map(|(_, e)| e)).then_with(|| a.title.cmp(&b.title))
            });
            let drill =
                sessions.iter().find(|s| matches!(s.drill, Drill::Local(_))).map_or(Drill::None, |s| s.drill.clone());
            ProjectView { key: p.key, kind: p.kind, drill, sessions }
        })
        .collect()
}

fn last_active(p: &ProjectView) -> Option<Ts> {
    p.sessions.first().and_then(|s| s.span.map(|(_, e)| e))
}

fn display_path(p: &Path, home: &Path) -> String {
    if !home.as_os_str().is_empty()
        && let Ok(rest) = p.strip_prefix(home)
    {
        if rest.as_os_str().is_empty() { "~".into() } else { format!("~/{}", rest.display()) }
    } else {
        p.display().to_string()
    }
}

pub struct Node {
    pub key: ProjectKey,
    pub label: String,
    pub project: Option<usize>,
    pub children: Vec<usize>,
    pub spans: Vec<(Ts, Ts)>,
    pub sessions: usize,
}

pub struct Forest {
    pub nodes: Vec<Node>,
    pub roots: Vec<usize>,
}

fn merge(mut spans: Vec<(Ts, Ts)>) -> Vec<(Ts, Ts)> {
    spans.sort_unstable();
    let mut merged: Vec<(Ts, Ts)> = Vec::with_capacity(spans.len());
    for (start, end) in spans {
        if let Some(last) = merged.last_mut()
            && start <= last.1
        {
            last.1 = last.1.max(end);
        } else {
            merged.push((start, end));
        }
    }
    merged
}

pub fn overlaps(spans: &[(Ts, Ts)], bs: Ts, be: Ts) -> bool {
    let i = spans.partition_point(|&(_, e)| e < bs);
    i < spans.len() && spans[i].0 < be
}

impl Forest {
    fn aggregate(&mut self, node: usize, projects: &[ProjectView]) {
        let mut spans = Vec::new();
        let mut sessions = 0;
        if let Some(p) = self.nodes[node].project {
            sessions += projects[p].sessions.len();
            spans.extend(projects[p].sessions.iter().filter_map(|s| s.span));
        }
        for i in 0..self.nodes[node].children.len() {
            let child = self.nodes[node].children[i];
            self.aggregate(child, projects);
            sessions += self.nodes[child].sessions;
            spans.extend_from_slice(&self.nodes[child].spans);
        }
        self.nodes[node].spans = merge(spans);
        self.nodes[node].sessions = sessions;
    }
}

pub fn recent_forest(projects: &[ProjectView], home: &Path) -> Forest {
    let nodes = projects
        .iter()
        .enumerate()
        .map(|(p, project)| Node {
            key: project.key.clone(),
            label: display_path(&project.key.path, home),
            project: Some(p),
            children: Vec::new(),
            spans: Vec::new(),
            sessions: 0,
        })
        .collect();
    let mut roots: Vec<_> = (0..projects.len()).collect();
    roots.sort_by(|&a, &b| {
        last_active(&projects[b]).cmp(&last_active(&projects[a])).then_with(|| projects[a].key.cmp(&projects[b].key))
    });
    let mut forest = Forest { nodes, roots };
    for n in 0..forest.nodes.len() {
        forest.aggregate(n, projects);
    }
    forest
}

#[derive(Default)]
struct Trie {
    children: BTreeMap<OsString, usize>,
    project: Option<usize>,
}

pub fn tree_forest(projects: &[ProjectView], home: &Path) -> Forest {
    fn emit(
        trie: &[Trie],
        mut at: usize,
        mut key: ProjectKey,
        parent: Option<&Path>,
        home: &Path,
        forest: &mut Forest,
    ) -> usize {
        while trie[at].project.is_none() && trie[at].children.len() == 1 {
            let (name, &child) = trie[at].children.first_key_value().unwrap();
            key.path.push(name);
            at = child;
        }
        let node = forest.nodes.len();
        forest.nodes.push(Node {
            label: match parent {
                Some(parent) => key.path.strip_prefix(parent).unwrap().display().to_string(),
                None => display_path(&key.path, home),
            },
            key: key.clone(),
            project: trie[at].project,
            children: Vec::new(),
            spans: Vec::new(),
            sessions: 0,
        });
        for (name, &child) in &trie[at].children {
            let child_key = ProjectKey { origin: key.origin, path: key.path.join(name) };
            let child = emit(trie, child, child_key, Some(&key.path), home, forest);
            forest.nodes[node].children.push(child);
        }
        node
    }
    let mut forest = Forest { nodes: Vec::new(), roots: Vec::new() };
    let mut origins = BTreeMap::<Origin, Vec<usize>>::new();
    for (p, project) in projects.iter().enumerate() {
        origins.entry(project.key.origin).or_default().push(p);
    }
    for (origin, indices) in origins {
        let mut trie = vec![Trie::default()];
        for p in indices {
            let mut at = 0;
            for component in projects[p].key.path.components() {
                if matches!(component, Component::RootDir | Component::Prefix(_) | Component::CurDir) {
                    continue;
                }
                let name = component.as_os_str();
                at = if let Some(&child) = trie[at].children.get(name) {
                    child
                } else {
                    let child = trie.len();
                    trie.push(Trie::default());
                    trie[at].children.insert(name.to_os_string(), child);
                    child
                };
            }
            trie[at].project = Some(p);
        }
        // Root itself can be a project (a session launched in /).
        if trie[0].project.is_some() {
            let key = ProjectKey { origin, path: PathBuf::from("/") };
            let root = emit(&trie, 0, key, None, home, &mut forest);
            forest.roots.push(root);
        } else {
            for (name, &child) in &trie[0].children {
                let key = ProjectKey { origin, path: Path::new("/").join(name) };
                let root = emit(&trie, child, key, None, home, &mut forest);
                forest.roots.push(root);
            }
        }
    }
    for i in 0..forest.roots.len() {
        forest.aggregate(forest.roots[i], projects);
    }
    forest
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Order {
    Recent,
    Tree,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Row {
    Node { node: usize, depth: usize },
    Session { project: usize, session: usize, depth: usize },
}

pub struct App {
    pub projects: Vec<ProjectView>,
    pub home: PathBuf,
    pub order: Order,
    pub forest: Forest,
    pub expanded: HashSet<ProjectKey>,
    pub rows: Vec<Row>,
    pub selected: usize,
    pub scroll: usize,
    pub view: antty::timeline::View,
    pub layout: crate::ui::Layout,
    pub flash: Option<String>,
    pub cmdline: Option<String>,
    pub no_color: bool,
    pub launch: Option<antty::tui::Invocation>,
    pub suspend: bool,
    count: usize,
    pending: Option<char>,
}

impl App {
    pub fn new(projects: Vec<ProjectView>, home: PathBuf) -> Self {
        let mut view = antty::timeline::View::new(true);
        let spans = merge(projects.iter().flat_map(|p| p.sessions.iter().filter_map(|s| s.span)).collect());
        view.set_activity(spans.iter().map(|(s, e)| (s.timestamp(), e.timestamp())).collect());
        let mut app = Self {
            projects,
            home,
            order: Order::Recent,
            forest: Forest { nodes: Vec::new(), roots: Vec::new() },
            expanded: Default::default(),
            rows: Vec::new(),
            selected: 0,
            scroll: 0,
            view,
            layout: crate::ui::layout(ratatui::layout::Rect::default()),
            flash: None,
            cmdline: None,
            no_color: std::env::var_os("NO_COLOR").is_some_and(|v| !v.is_empty()),
            launch: None,
            suspend: false,
            count: 0,
            pending: None,
        };
        app.rebuild();
        app
    }

    fn rebuild(&mut self) {
        let old = self.rows.get(self.selected).copied();
        let key = match old {
            Some(Row::Node { node, .. }) => Some(self.forest.nodes[node].key.clone()),
            Some(Row::Session { project, .. }) => Some(self.projects[project].key.clone()),
            None => None,
        };
        self.forest = match self.order {
            Order::Recent => recent_forest(&self.projects, &self.home),
            Order::Tree => tree_forest(&self.projects, &self.home),
        };
        fn walk(app: &App, node: usize, depth: usize, rows: &mut Vec<Row>) {
            rows.push(Row::Node { node, depth });
            let n = &app.forest.nodes[node];
            if app.expanded.contains(&n.key) {
                if let Some(project) = n.project {
                    rows.extend((0..app.projects[project].sessions.len()).map(|session| Row::Session {
                        project,
                        session,
                        depth: depth + 1,
                    }));
                }
                for &child in &n.children {
                    walk(app, child, depth + 1, rows);
                }
            }
        }
        let mut rows = Vec::new();
        for &root in &self.forest.roots {
            walk(self, root, 0, &mut rows);
        }
        let exact = rows.iter().position(|row| match (old, *row) {
            (Some(Row::Node { .. }), Row::Node { node, .. }) => key.as_ref() == Some(&self.forest.nodes[node].key),
            (Some(Row::Session { project: p, session: s, .. }), Row::Session { project, session, .. }) => {
                p == project && s == session
            }
            _ => false,
        });
        let project_node = || {
            rows.iter().position(|row| match row {
                Row::Node { node, .. } => key.as_ref() == Some(&self.forest.nodes[*node].key),
                _ => false,
            })
        };
        self.selected = exact.or_else(project_node).unwrap_or(self.selected.min(rows.len().saturating_sub(1)));
        self.rows = rows;
        self.clamp_scroll();
    }

    pub fn fit(&mut self) {
        let spans = merge(self.forest.roots.iter().flat_map(|&n| self.forest.nodes[n].spans.iter().copied()).collect());
        let now = chrono::Utc::now();
        let (min, mut max) =
            spans.first().zip(spans.last()).map(|(a, b)| (a.0, b.1)).unwrap_or((now - chrono::Duration::hours(1), now));
        if now - max <= chrono::Duration::seconds(60) {
            max = now;
        }
        self.view.fit(min, max, self.layout.gantt_width.max(1) as usize);
    }

    pub fn clamp_scroll(&mut self) {
        self.selected = self.selected.min(self.rows.len().saturating_sub(1));
        let height = self.layout.body_height.max(1) as usize;
        self.scroll = self.scroll.min(self.rows.len().saturating_sub(height));
        if self.selected < self.scroll {
            self.scroll = self.selected;
        }
        if self.selected >= self.scroll + height {
            self.scroll = self.selected + 1 - height;
        }
    }

    fn fold(&mut self, open: Option<bool>) {
        let key = match self.rows.get(self.selected) {
            Some(Row::Node { node, .. }) => self.forest.nodes[*node].key.clone(),
            Some(Row::Session { project, .. }) => {
                // Space/za on a session closes its project rather than opening it.
                if open.is_none() {
                    let key = self.projects[*project].key.clone();
                    self.expanded.remove(&key);
                    self.rebuild();
                    return;
                }
                return;
            }
            None => return,
        };
        if open.unwrap_or(!self.expanded.contains(&key)) {
            self.expanded.insert(key);
        } else {
            self.expanded.remove(&key);
        }
        self.rebuild();
    }

    fn drill_down(&mut self) {
        let (drill, cwd) = match self.rows.get(self.selected).copied() {
            Some(Row::Session { project, session, .. }) => {
                let session = &self.projects[project].sessions[session];
                (&session.drill, &session.cwd)
            }
            Some(Row::Node { node, .. }) => {
                let Some(p) = self.forest.nodes[node].project else {
                    self.fold(None);
                    return;
                };
                let project = &self.projects[p];
                (&project.drill, project.sessions.first().map_or(&project.key.path, |s| &s.cwd))
            }
            None => return,
        };
        let Drill::Local(target) = drill else {
            self.flash = Some(format!("cwd no longer exists: {}", cwd.display()));
            return;
        };
        self.launch = Some(antty_invocation(target));
    }

    pub fn handle_key(&mut self, key: crossterm::event::KeyEvent) -> bool {
        use crossterm::event::{KeyCode, KeyModifiers};
        self.flash = None;
        let plain = key.modifiers.difference(KeyModifiers::SHIFT).is_empty();
        if let Some(text) = self.cmdline.as_mut() {
            match key.code {
                KeyCode::Esc => self.cmdline = None,
                KeyCode::Char('c') if key.modifiers == KeyModifiers::CONTROL => self.cmdline = None,
                KeyCode::Backspace => {
                    text.pop();
                    if text.is_empty() {
                        self.cmdline = None;
                    }
                }
                KeyCode::Enter => {
                    let text = self.cmdline.take().unwrap();
                    let text = text.trim();
                    if matches!(text, "q" | "q!" | "qa" | "qa!" | "quit") {
                        return true;
                    }
                    self.flash = Some(format!("not an editor command: {text}"));
                }
                KeyCode::Char(c) if plain && !c.is_control() => text.push(c),
                _ => {}
            }
            return false;
        }
        if key.code == KeyCode::Char('z') && key.modifiers == KeyModifiers::CONTROL {
            self.count = 0;
            self.pending = None;
            self.suspend = true;
            return false;
        }
        let width = self.layout.gantt_width.max(1) as usize;
        if let Some(prefix) = self.pending.take() {
            let count = std::mem::take(&mut self.count);
            if plain {
                match (prefix, key.code) {
                    ('Z', KeyCode::Char('Z' | 'Q')) => return true,
                    ('g', KeyCode::Char('g')) => self.selected = count.saturating_sub(1),
                    ('z', KeyCode::Char('a')) => self.fold(None),
                    ('z', KeyCode::Char('o')) => self.fold(Some(true)),
                    ('z', KeyCode::Char('c')) => self.fold(Some(false)),
                    ('z', KeyCode::Char('R')) => {
                        self.expanded.extend(self.forest.nodes.iter().map(|n| n.key.clone()));
                        self.rebuild();
                    }
                    ('z', KeyCode::Char('M')) => {
                        self.expanded.clear();
                        self.rebuild();
                    }
                    _ => {}
                }
            }
            self.clamp_scroll();
            return false;
        }
        if !plain {
            let count = std::mem::take(&mut self.count);
            if key.modifiers == KeyModifiers::CONTROL {
                let half = if count > 0 { count } else { (self.layout.body_height as usize / 2).max(1) };
                match key.code {
                    KeyCode::Char('c') => self.flash = Some("Type :q and press <Enter> to exit".into()),
                    KeyCode::Char('d') => self.selected = self.selected.saturating_add(half),
                    KeyCode::Char('u') => self.selected = self.selected.saturating_sub(half),
                    _ => {}
                }
            }
            self.clamp_scroll();
            return false;
        }
        match key.code {
            KeyCode::Char(c @ '1'..='9') | KeyCode::Char(c @ '0') if c != '0' || self.count > 0 => {
                self.count = (self.count * 10 + c.to_digit(10).unwrap() as usize).min(9999);
                return false;
            }
            KeyCode::Char(c @ ('g' | 'z' | 'Z')) => {
                self.pending = Some(c);
                return false;
            }
            _ => {}
        }
        let count = std::mem::take(&mut self.count);
        let n = count.max(1);
        match key.code {
            KeyCode::Char(':') => self.cmdline = Some(String::new()),
            KeyCode::Char('j') | KeyCode::Down => self.selected = self.selected.saturating_add(n),
            KeyCode::Char('k') | KeyCode::Up => self.selected = self.selected.saturating_sub(n),
            KeyCode::Char('G') => self.selected = if count > 0 { count - 1 } else { self.rows.len().saturating_sub(1) },
            KeyCode::Char('h') | KeyCode::Left => self.view.move_cursor_cols(-(n as i64), width),
            KeyCode::Char('l') | KeyCode::Right => self.view.move_cursor_cols(n as i64, width),
            KeyCode::Char('H') => self.view.move_cursor_cols(-(n as i64) * (width as i64 / 2), width),
            KeyCode::Char('L') => self.view.move_cursor_cols(n as i64 * (width as i64 / 2), width),
            KeyCode::Char('+' | '=') => {
                for _ in 0..n.min(antty::timeline::ZOOM_LEVELS.len()) {
                    self.view.zoom_in();
                }
            }
            KeyCode::Char('-') => {
                for _ in 0..n.min(antty::timeline::ZOOM_LEVELS.len()) {
                    self.view.zoom_out();
                }
            }
            KeyCode::Char('t') => self.view.anchor_right(chrono::Utc::now(), width),
            KeyCode::Char('f') => self.fit(),
            KeyCode::Char('c') => self.view.toggle_collapse_gaps(),
            KeyCode::Char('o') => {
                self.order = if self.order == Order::Recent { Order::Tree } else { Order::Recent };
                self.rebuild();
            }
            KeyCode::Char(' ') => self.fold(None),
            KeyCode::Enter => self.drill_down(),
            _ => {}
        }
        self.clamp_scroll();
        false
    }
}

fn antty_invocation(target: &Path) -> antty::tui::Invocation {
    let program = std::env::current_exe()
        .ok()
        .map(|p| p.with_file_name(format!("antty{}", std::env::consts::EXE_SUFFIX)))
        .filter(|p| p.exists())
        .map(PathBuf::into_os_string)
        .unwrap_or_else(|| "antty".into());
    antty::tui::Invocation {
        program,
        args: vec!["--project".into(), target.as_os_str().to_owned()],
        suspend: true,
        stdin: None,
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn folding_restores_project_and_modified_keys_do_not_launch() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let cwd = std::env::temp_dir();
        let session = crate::discover::SessionInfo {
            harness: antty::sessions::Harness::Omp,
            cwd: cwd.clone(),
            drill: Drill::Local(cwd.clone()),
            title: "session".into(),
            span: Some((Ts::UNIX_EPOCH, Ts::UNIX_EPOCH)),
        };
        let mut app = super::App::new(
            vec![super::ProjectView {
                key: ProjectKey { origin: Origin::Local, path: cwd.clone() },
                kind: crate::discover::Kind::NoGit,
                drill: Drill::Local(cwd.clone()),
                sessions: vec![session],
            }],
            PathBuf::new(),
        );
        let key = |c| KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE);
        app.handle_key(key(' '));
        app.handle_key(key('j'));
        assert!(matches!(app.rows[app.selected], super::Row::Session { .. }));
        app.handle_key(key(' '));
        assert!(matches!(app.rows[app.selected], super::Row::Node { .. }));
        assert!(!app.expanded.contains(&ProjectKey { origin: Origin::Local, path: cwd.clone() }));
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::ALT));
        assert!(app.launch.is_none());
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        let inv = app.launch.take().unwrap();
        assert_eq!(inv.args, [OsString::from("--project"), cwd.into_os_string()]);
        app.handle_key(key('9'));
        app.handle_key(key('g'));
        app.handle_key(key('g'));
        assert_eq!(app.selected, 0);
        app.handle_key(key(':'));
        app.handle_key(key('q'));
        assert!(app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)));
    }

    use super::*;
    use antty::sessions::Harness;

    #[test]
    fn missing_discovery_target_does_not_launch() {
        let cwd = PathBuf::from("/q/gone");
        let mut app = App::new(
            project_views(vec![Project {
                key: ProjectKey { origin: Origin::Local, path: PathBuf::from("/q/repo") },
                kind: Kind::Git,
                cwds: vec![(
                    cwd.clone(),
                    vec![SessionInfo {
                        harness: Harness::Omp,
                        cwd,
                        drill: Drill::None,
                        title: "gone".into(),
                        span: None,
                    }],
                )],
            }]),
            PathBuf::new(),
        );
        app.handle_key(crossterm::event::KeyEvent::from(crossterm::event::KeyCode::Enter));
        assert!(app.launch.is_none());
        assert_eq!(app.flash.as_deref(), Some("cwd no longer exists: /q/gone"));
    }

    #[test]
    fn tree_forest_merges_single_child_dirs() {
        let paths = [
            "/h/projects",
            "/h/projects/sandbox",
            "/h/projects/sandbox/tmp",
            "/h/projects/tools/antty",
            "/private/tmp",
        ];
        let projects = project_views(
            paths
                .iter()
                .enumerate()
                .map(|(i, path)| {
                    let start = Ts::UNIX_EPOCH + chrono::Duration::hours(i as i64);
                    let session = SessionInfo {
                        harness: Harness::Omp,
                        cwd: PathBuf::from(path),
                        drill: Drill::Local(PathBuf::from(path)),
                        title: path.to_string(),
                        span: Some((start, start + chrono::Duration::minutes(1))),
                    };
                    let sessions = if i == 3 { vec![session.clone(), session] } else { vec![session] };
                    Project {
                        key: ProjectKey { origin: Origin::Local, path: PathBuf::from(path) },
                        kind: Kind::NoGit,
                        cwds: vec![(PathBuf::from(path), sessions)],
                    }
                })
                .collect(),
        );
        let home = Path::new("/h");
        let tree = tree_forest(&projects, home);
        let labels = |indices: &[usize]| indices.iter().map(|&i| tree.nodes[i].label.as_str()).collect::<Vec<_>>();
        assert_eq!(labels(&tree.roots), ["~/projects", "/private/tmp"]);
        let root = tree.roots[0];
        assert_eq!(labels(&tree.nodes[root].children), ["sandbox", "tools/antty"]);
        let sandbox = tree.nodes[root].children[0];
        assert_eq!(labels(&tree.nodes[sandbox].children), ["tmp"]);
        assert_eq!(tree.nodes[root].sessions, 5);
        let recent = recent_forest(&projects, home);
        assert_eq!(
            recent.roots.iter().map(|&i| recent.nodes[i].key.path.as_path()).collect::<Vec<_>>(),
            paths.iter().rev().map(Path::new).collect::<Vec<_>>()
        );
    }
}
