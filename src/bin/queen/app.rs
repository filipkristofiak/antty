use std::collections::{BTreeMap, HashSet};
use std::ffi::OsString;
use std::path::{Component, Path, PathBuf};

use antty::cmdline::{self, CmdKind, CmdLine};
use antty::model::Ts;
use antty::search;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::discover::{Drill, Kind, Origin, Project, ProjectKey, SessionInfo};

const SESSIONS_SHOWN: usize = 5;
const PROJECTS_SHOWN: usize = 12;

fn live_cutoff() -> Ts {
    chrono::Utc::now() - chrono::Duration::hours(antty::tui::LIVE_HOURS)
}

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
    pub full_label: String,
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
        .map(|(p, project)| {
            let label = display_path(&project.key.path, home);
            Node {
                key: project.key.clone(),
                full_label: label.clone(),
                label,
                project: Some(p),
                children: Vec::new(),
                spans: Vec::new(),
                sessions: 0,
            }
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
        let full_label = display_path(&key.path, home);
        forest.nodes.push(Node {
            label: match parent {
                Some(parent) => key.path.strip_prefix(parent).unwrap().display().to_string(),
                None => full_label.clone(),
            },
            full_label,
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
    SessionFold { project: usize, depth: usize },
    ProjectFold,
}

pub struct App {
    pub projects: Vec<ProjectView>,
    pub home: PathBuf,
    pub order: Order,
    pub forest: Forest,
    pub expanded: HashSet<ProjectKey>,
    pub fold: bool,
    pub unfolded: HashSet<ProjectKey>,
    pub projects_unfolded: bool,
    pub rows: Vec<Row>,
    pub live_since: Ts,
    pub selected: usize,
    pub scroll: usize,
    pub view: antty::timeline::View,
    pub layout: crate::ui::Layout,
    pub flash: Option<String>,
    pub cmdline: Option<CmdLine>,
    pub search: Option<String>,
    pub help: Option<u16>,
    pub no_color: bool,
    pub launch: Option<antty::tui::Invocation>,
    pub suspend: bool,
    count: usize,
    pending: Option<char>,
    command_history: Vec<String>,
    search_history: Vec<String>,
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
            fold: true,
            unfolded: HashSet::new(),
            projects_unfolded: false,
            rows: Vec::new(),
            live_since: live_cutoff(),
            selected: 0,
            scroll: 0,
            view,
            layout: crate::ui::layout(ratatui::layout::Rect::default()),
            flash: None,
            cmdline: None,
            search: None,
            help: None,
            no_color: std::env::var_os("NO_COLOR").is_some_and(|v| !v.is_empty()),
            launch: None,
            suspend: false,
            count: 0,
            pending: None,
            command_history: Vec::new(),
            search_history: Vec::new(),
        };
        app.rebuild_at(app.live_since);
        app
    }

    pub fn sessions_shown(&self, project: usize, live_since: Ts) -> usize {
        let project = &self.projects[project];
        let sessions = &project.sessions;
        if !self.fold || self.unfolded.contains(&project.key) {
            return sessions.len();
        }
        let live = sessions.iter().take_while(|s| s.span.is_some_and(|(_, end)| end >= live_since)).count();
        SESSIONS_SHOWN.max(live).min(sessions.len())
    }

    pub fn roots_shown(&self, live_since: Ts) -> usize {
        let roots = &self.forest.roots;
        if !self.fold || self.order == Order::Tree || self.projects_unfolded {
            return roots.len();
        }
        let live = roots
            .iter()
            .take_while(|&&root| self.forest.nodes[root].spans.last().is_some_and(|&(_, end)| end >= live_since))
            .count();
        PROJECTS_SHOWN.max(live).min(roots.len())
    }

    fn rebuild(&mut self) {
        self.rebuild_at(live_cutoff());
    }

    fn rebuild_at(&mut self, live_since: Ts) {
        let old = self.rows.get(self.selected).copied();
        let key = match old {
            Some(Row::Node { node, .. }) => Some(self.forest.nodes[node].key.clone()),
            Some(Row::Session { project, .. } | Row::SessionFold { project, .. }) => {
                Some(self.projects[project].key.clone())
            }
            Some(Row::ProjectFold) | None => None,
        };
        self.forest = match self.order {
            Order::Recent => recent_forest(&self.projects, &self.home),
            Order::Tree => tree_forest(&self.projects, &self.home),
        };
        self.live_since = live_since;
        let rows = self.rows_for(false);
        let exact = rows.iter().position(|row| match (old, *row) {
            (Some(Row::Node { .. }), Row::Node { node, .. }) => key.as_ref() == Some(&self.forest.nodes[node].key),
            (Some(Row::Session { project: p, session: s, .. }), Row::Session { project, session, .. }) => {
                p == project && s == session
            }
            (Some(Row::ProjectFold), Row::ProjectFold) => true,
            _ => false,
        });
        let session_fold = || {
            if let Some(Row::Session { project, .. } | Row::SessionFold { project, .. }) = old {
                rows.iter().position(|r| matches!(*r, Row::SessionFold { project: p, .. } if p == project))
            } else {
                None
            }
        };
        let project_node = || {
            rows.iter().position(|row| match row {
                Row::Node { node, .. } => key.as_ref() == Some(&self.forest.nodes[*node].key),
                _ => false,
            })
        };
        self.selected = exact
            .or_else(session_fold)
            .or_else(project_node)
            .or_else(|| {
                (matches!(old, Some(Row::Node { .. })) && rows.last() == Some(&Row::ProjectFold))
                    .then(|| rows.len() - 1)
            })
            .unwrap_or(self.selected.min(rows.len().saturating_sub(1)));
        self.rows = rows;
        self.clamp_scroll();
    }

    fn rows_for(&self, full: bool) -> Vec<Row> {
        fn walk(app: &App, node: usize, depth: usize, full: bool, live_since: Ts, rows: &mut Vec<Row>) {
            rows.push(Row::Node { node, depth });
            let n = &app.forest.nodes[node];
            if full || app.expanded.contains(&n.key) {
                if let Some(project) = n.project {
                    let total = app.projects[project].sessions.len();
                    let shown = if full { total } else { app.sessions_shown(project, live_since) };
                    rows.extend((0..shown).map(|session| Row::Session { project, session, depth: depth + 1 }));
                    if shown < total {
                        rows.push(Row::SessionFold { project, depth: depth + 1 });
                    }
                }
                for &child in &n.children {
                    walk(app, child, depth + 1, full, live_since, rows);
                }
            }
        }
        let mut rows = Vec::new();
        let live_since = self.live_since;
        let total = self.forest.roots.len();
        let shown = if full { total } else { self.roots_shown(live_since) };
        for &root in &self.forest.roots[..shown] {
            walk(self, root, 0, full, live_since, &mut rows);
        }
        if shown < total {
            rows.push(Row::ProjectFold);
        }
        rows
    }

    pub fn search_label(&self, row: Row) -> Option<&str> {
        match row {
            Row::Node { node, .. } => Some(&self.forest.nodes[node].full_label),
            Row::Session { project, session, .. } => Some(&self.projects[project].sessions[session].title),
            Row::SessionFold { .. } | Row::ProjectFold => None,
        }
    }

    fn path_to(&self, target: usize) -> Vec<usize> {
        fn visit(forest: &Forest, node: usize, target: usize, path: &mut Vec<usize>) -> bool {
            path.push(node);
            if node == target || forest.nodes[node].children.iter().any(|&child| visit(forest, child, target, path)) {
                return true;
            }
            path.pop();
            false
        }
        let mut path = Vec::new();
        for &root in &self.forest.roots {
            if visit(&self.forest, root, target, &mut path) {
                break;
            }
        }
        path
    }

    fn reveal(&mut self, row: Row) {
        let target = match row {
            Row::Node { node, .. } => node,
            Row::Session { project, .. } => self.forest.nodes.iter().position(|n| n.project == Some(project)).unwrap(),
            Row::SessionFold { .. } | Row::ProjectFold => return,
        };
        let path = self.path_to(target);
        let live_since = live_cutoff();
        let shown = self.roots_shown(live_since);
        if self.forest.roots[shown..].contains(&path[0]) {
            self.projects_unfolded = true;
        }
        for &node in &path[..path.len() - 1] {
            self.expanded.insert(self.forest.nodes[node].key.clone());
        }
        if let Row::Session { project, session, .. } = row {
            self.expanded.insert(self.forest.nodes[target].key.clone());
            if session >= self.sessions_shown(project, live_since) {
                self.unfolded.insert(self.projects[project].key.clone());
            }
        }
        self.rebuild_at(live_since);
        if let Some(i) = self.rows.iter().position(|&r| same_row(r, row)) {
            self.selected = i;
        }
        self.clamp_scroll();
    }

    fn search_step(&mut self, query: &str, forward: bool, n: usize) -> bool {
        let full = self.rows_for(true);
        let matches: Vec<_> = full
            .iter()
            .enumerate()
            .filter_map(|(i, &r)| self.search_label(r).filter(|label| search::is_match(label, query)).map(|_| i))
            .collect();
        let cur = self.rows.get(self.selected).and_then(|&row| full.iter().position(|&r| same_row(r, row)));
        let Some((idx, wrapped)) = search::step(&matches, cur, forward, n) else {
            self.flash = Some(format!("Pattern not found: {query}"));
            return false;
        };
        if wrapped {
            self.flash = Some(
                if forward { "search hit BOTTOM, continuing at TOP" } else { "search hit TOP, continuing at BOTTOM" }
                    .into(),
            );
        }
        self.reveal(full[idx]);
        true
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

    fn unfold_sessions(&mut self, project: usize) {
        let first_hidden = self.sessions_shown(project, self.live_since);
        self.unfolded.insert(self.projects[project].key.clone());
        self.rebuild();
        if let Some(i) = self.rows.iter().position(
            |r| matches!(*r, Row::Session { project: p, session, .. } if p == project && session == first_hidden),
        ) {
            self.selected = i;
        }
    }

    fn unfold_projects(&mut self) {
        let first_hidden = self.roots_shown(self.live_since);
        self.projects_unfolded = true;
        self.rebuild();
        if let Some(&root) = self.forest.roots.get(first_hidden)
            && let Some(i) = self.rows.iter().position(|r| *r == (Row::Node { node: root, depth: 0 }))
        {
            self.selected = i;
        }
    }

    fn fold(&mut self, open: Option<bool>) {
        let key = match self.rows.get(self.selected).copied() {
            Some(Row::Node { node, .. }) => self.forest.nodes[node].key.clone(),
            Some(Row::Session { project, .. }) => {
                // Space/za on a session closes its project rather than opening it.
                if open.is_none() {
                    let key = &self.projects[project].key;
                    self.expanded.remove(key);
                    self.unfolded.remove(key);
                    self.rebuild();
                    return;
                }
                return;
            }
            Some(Row::SessionFold { project, .. }) => {
                if open != Some(false) {
                    self.unfold_sessions(project);
                }
                return;
            }
            Some(Row::ProjectFold) => {
                if open != Some(false) {
                    self.unfold_projects();
                }
                return;
            }
            None => return,
        };
        if open.unwrap_or(!self.expanded.contains(&key)) {
            self.expanded.insert(key);
        } else {
            self.expanded.remove(&key);
            self.unfolded.remove(&key);
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
            Some(Row::SessionFold { project, .. }) => {
                self.unfold_sessions(project);
                return;
            }
            Some(Row::ProjectFold) => {
                self.unfold_projects();
                return;
            }
            None => return,
        };
        let Drill::Local(target) = drill else {
            self.flash = Some(format!("cwd no longer exists: {}", cwd.display()));
            return;
        };
        self.launch = Some(antty_invocation(target));
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> bool {
        self.flash = None;
        let plain = key.modifiers.difference(KeyModifiers::SHIFT).is_empty();
        if self.cmdline.is_some() {
            return self.handle_key_command(key);
        }
        if let Some(scroll) = &mut self.help {
            let page = self.layout.help.height.saturating_sub(2);
            if antty::tui::help_key(key, scroll, page, crate::ui::HELP) {
                self.help = None;
                if key.code == KeyCode::Esc {
                    self.search = None;
                }
            }
            return false;
        }
        if key.code == KeyCode::Char('z') && key.modifiers == KeyModifiers::CONTROL {
            self.count = 0;
            self.pending = None;
            self.suspend = true;
            return false;
        }
        if plain && key.code == KeyCode::Esc {
            self.search = None;
            self.count = 0;
            self.pending = None;
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
                        self.unfolded.clear();
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
            KeyCode::Char(':') => self.cmdline = Some(CmdLine::new(CmdKind::Command)),
            KeyCode::Char('/') => self.cmdline = Some(CmdLine::new(CmdKind::Search)),
            KeyCode::Char('n' | 'N') => {
                if let Some(query) = self.search.take() {
                    self.search_step(&query, key.code == KeyCode::Char('n'), n);
                    self.search = Some(query);
                }
            }
            KeyCode::Char('?') => self.help = Some(0),
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
            KeyCode::Char('p') => {
                self.fold = !self.fold;
                if self.fold {
                    self.unfolded.clear();
                    self.projects_unfolded = false;
                }
                self.rebuild();
            }
            KeyCode::Char(' ') => self.fold(None),
            KeyCode::Enter => self.drill_down(),
            _ => {}
        }
        self.clamp_scroll();
        false
    }

    fn handle_key_command(&mut self, key: KeyEvent) -> bool {
        let line = self.cmdline.as_mut().unwrap();
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Esc => self.cmdline = None,
            KeyCode::Char('c') if ctrl => self.cmdline = None,
            KeyCode::Backspace => {
                if !line.backspace() {
                    self.cmdline = None;
                }
            }
            KeyCode::Char('u') if ctrl => line.clear(),
            KeyCode::Char('w') if ctrl => line.delete_word(),
            KeyCode::Up | KeyCode::Down => {
                let history = match line.kind {
                    CmdKind::Command => &self.command_history,
                    CmdKind::Search => &self.search_history,
                };
                if key.code == KeyCode::Up { line.older(history) } else { line.newer(history) }
            }
            KeyCode::Enter => {
                let line = self.cmdline.take().unwrap();
                match line.kind {
                    CmdKind::Command => {
                        let cmd = line.text.trim();
                        if !cmd.is_empty() {
                            cmdline::record(&mut self.command_history, &line.text);
                        }
                        match cmd {
                            "q" | "q!" | "qa" | "qa!" | "quit" => return true,
                            "" => {}
                            other => self.flash = Some(format!("not an editor command: {other}")),
                        }
                    }
                    CmdKind::Search => {
                        let query = if line.text.is_empty() {
                            self.search_history.last().cloned()
                        } else {
                            cmdline::record(&mut self.search_history, &line.text);
                            Some(line.text)
                        };
                        if let Some(query) = query
                            && self.search_step(&query, true, 1)
                        {
                            self.search = Some(query);
                        }
                    }
                }
            }
            KeyCode::Char(c) if !ctrl && !c.is_control() => line.push(c),
            _ => {}
        }
        false
    }
}

fn same_row(a: Row, b: Row) -> bool {
    match (a, b) {
        (Row::Node { node: a, .. }, Row::Node { node: b, .. }) => a == b,
        (Row::Session { project: a, session: s, .. }, Row::Session { project: b, session: t, .. }) => a == b && s == t,
        _ => false,
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
    use crossterm::event::{KeyCode, KeyEvent};

    fn project(index: usize, minutes_ago: impl IntoIterator<Item = i64>) -> ProjectView {
        let path = PathBuf::from(format!("/q/p{index}"));
        let now = chrono::Utc::now();
        let sessions = minutes_ago
            .into_iter()
            .enumerate()
            .map(|(i, offset)| {
                let end = now - chrono::Duration::minutes(offset);
                SessionInfo {
                    harness: Harness::Omp,
                    cwd: path.clone(),
                    drill: Drill::Local(path.clone()),
                    title: format!("session {i}"),
                    span: Some((end - chrono::Duration::minutes(1), end)),
                }
            })
            .collect();
        ProjectView {
            key: ProjectKey { origin: Origin::Local, path: path.clone() },
            kind: Kind::NoGit,
            drill: Drill::Local(path),
            sessions,
        }
    }

    #[test]
    fn session_fold_opens_at_first_hidden_row_and_restores_selection() {
        let mut app = App::new(vec![project(0, (3..11).map(|hours| hours * 60))], PathBuf::new());
        app.handle_key(KeyEvent::from(KeyCode::Char(' ')));
        assert_eq!(app.rows.len(), 7);
        assert!(app.rows[1..6].iter().all(|r| matches!(r, Row::Session { .. })));
        assert!(matches!(app.rows[6], Row::SessionFold { project: 0, .. }));
        app.handle_key(KeyEvent::from(KeyCode::Char('G')));
        app.handle_key(KeyEvent::from(KeyCode::Enter));
        assert_eq!(app.rows.len(), 9);
        assert!(app.rows[1..].iter().all(|r| matches!(r, Row::Session { .. })));
        assert!(matches!(app.rows[app.selected], Row::Session { project: 0, session: 5, .. }));
        for _ in 0..2 {
            app.handle_key(KeyEvent::from(KeyCode::Char('p')));
        }
        assert!(matches!(app.rows[app.selected], Row::SessionFold { project: 0, .. }));
    }

    #[test]
    fn session_fold_keeps_all_live_sessions_visible() {
        let mut app = App::new(vec![project(0, [30, 30, 30, 30, 30, 30, 30, 180])], PathBuf::new());
        app.handle_key(KeyEvent::from(KeyCode::Char(' ')));
        assert_eq!(app.rows.len(), 9);
        assert!(app.rows[1..8].iter().all(|r| matches!(r, Row::Session { .. })));
        assert!(matches!(app.rows[8], Row::SessionFold { project: 0, .. }));
    }

    #[test]
    fn fold_label_uses_the_cutoff_the_rows_were_built_with() {
        let mut app = App::new(vec![project(0, [60, 60, 60, 60, 60, 150, 240])], PathBuf::new());
        app.handle_key(KeyEvent::from(KeyCode::Char(' ')));
        app.rebuild_at(chrono::Utc::now() - chrono::Duration::hours(3));
        assert_eq!(app.rows.len(), 8);

        app.layout = crate::ui::layout(ratatui::layout::Rect::new(0, 0, 160, 20));
        app.clamp_scroll();
        let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(160, 20)).unwrap();
        terminal.draw(|f| crate::ui::draw(f, &app)).unwrap();
        let screen = terminal.backend().buffer().content().iter().map(|c| c.symbol()).collect::<String>();
        assert!(screen.contains("⋯ 1 older"));
        assert!(!screen.contains("⋯ 2 older"));
    }

    #[test]
    fn project_fold_applies_in_recent_order_and_opens_at_first_hidden_root() {
        let projects = (0..14).map(|i| project(i, [(i as i64 + 3) * 60])).collect();
        let mut app = App::new(projects, PathBuf::new());
        let folded = |app: &App| {
            assert_eq!(app.rows.len(), 13);
            assert!(app.rows[..12].iter().all(|r| matches!(r, Row::Node { .. })));
            assert!(matches!(app.rows[12], Row::ProjectFold));
        };
        folded(&app);
        app.handle_key(KeyEvent::from(KeyCode::Char('o')));
        // Tree order groups these projects beneath /q; expand that directory.
        app.handle_key(KeyEvent::from(KeyCode::Char(' ')));
        assert_eq!(app.rows.len(), 15);
        assert!(app.rows.iter().all(|r| matches!(r, Row::Node { .. })));
        app.handle_key(KeyEvent::from(KeyCode::Char('o')));
        folded(&app);
        app.handle_key(KeyEvent::from(KeyCode::Char('G')));
        app.handle_key(KeyEvent::from(KeyCode::Enter));
        assert_eq!(app.rows.len(), 14);
        assert!(app.rows.iter().all(|r| matches!(r, Row::Node { .. })));
        assert!(matches!(app.rows[app.selected], Row::Node { node: 12, depth: 0 }));
        for _ in 0..2 {
            app.handle_key(KeyEvent::from(KeyCode::Char('p')));
        }
        folded(&app);
        assert!(matches!(app.rows[app.selected], Row::ProjectFold));
    }

    #[test]
    fn help_scrolls_without_moving_the_selected_row() {
        let mut app = App::new(vec![project(0, [180]), project(1, [240])], PathBuf::new());
        app.layout = crate::ui::layout(ratatui::layout::Rect::new(0, 0, 160, 24));
        app.handle_key(KeyEvent::from(KeyCode::Char('?')));
        assert_eq!(app.help, Some(0));
        app.handle_key(KeyEvent::from(KeyCode::Char('j')));
        assert_eq!(app.help, Some(1));
        assert_eq!(app.selected, 0);
        assert!(!app.handle_key(KeyEvent::from(KeyCode::Char('q'))));
        assert_eq!(app.help, None);
    }

    fn enter_search(app: &mut App, query: &str) {
        app.handle_key(KeyEvent::from(KeyCode::Char('/')));
        for c in query.chars() {
            app.handle_key(KeyEvent::from(KeyCode::Char(c)));
        }
        app.handle_key(KeyEvent::from(KeyCode::Enter));
    }

    #[test]
    fn search_reveals_older_sessions_and_wraps_in_smartcase_order() {
        let mut first = project(0, (3..11).map(|hours| hours * 60));
        first.sessions[7].title = "needle".into();
        let mut second = project(1, [20 * 60]);
        second.sessions[0].title = "Needle two".into();
        let mut app = App::new(vec![first, second], PathBuf::new());
        enter_search(&mut app, "needle");
        assert!(matches!(app.rows[app.selected], Row::Session { project: 0, session: 7, .. }));
        let key = &app.projects[0].key;
        assert!(app.expanded.contains(key));
        assert!(app.unfolded.contains(key));
        app.handle_key(KeyEvent::from(KeyCode::Char('n')));
        assert!(matches!(app.rows[app.selected], Row::Session { project: 1, session: 0, .. }));
        app.handle_key(KeyEvent::from(KeyCode::Char('n')));
        assert!(matches!(app.rows[app.selected], Row::Session { project: 0, session: 7, .. }));
        assert_eq!(app.flash.as_deref(), Some("search hit BOTTOM, continuing at TOP"));
        let selected = app.selected;
        enter_search(&mut app, "zzz");
        assert_eq!(app.flash.as_deref(), Some("Pattern not found: zzz"));
        assert_eq!(app.selected, selected);
        assert_eq!(app.search.as_deref(), Some("needle"));
    }

    #[test]
    fn search_reveals_folded_projects_and_collapsed_tree_ancestors() {
        let projects = (0..14).map(|i| project(i, [(i as i64 + 3) * 60])).collect();
        let mut app = App::new(projects, PathBuf::new());
        enter_search(&mut app, "p13");
        assert!(app.projects_unfolded);
        assert_eq!(app.search_label(app.rows[app.selected]), Some("/q/p13"));
        app.handle_key(KeyEvent::from(KeyCode::Char('o')));
        enter_search(&mut app, "/q/p13");
        let row = app.rows[app.selected];
        let Row::Node { node, depth: 1 } = row else {
            panic!("search must select the project under its directory");
        };
        assert_eq!(app.forest.nodes[node].label, "p13");
        assert_eq!(app.search_label(row), Some("/q/p13"));
    }

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
