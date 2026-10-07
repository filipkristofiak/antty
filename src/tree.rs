use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};

use crate::model::{FileEvent, Model, ParticipantId, ParticipantKind, Scope};

pub struct Node {
    pub name: String,
    /// unique across every forest; keys `ExpandState::dir_overrides`.
    pub key: String,
    pub parent: Option<usize>,
    pub is_dir: bool,
    pub depth: usize,
    pub children: Vec<usize>,
    /// indices into Model.events; for dirs this is the union of every descendant's events.
    pub events: Vec<usize>,
    /// true for a file that shows up only through history (edited/read) but is absent on disk.
    pub deleted: bool,
}

pub struct Tree {
    pub nodes: Vec<Node>,
    /// FILES forest roots: single-root file entries, or one checkout wrapper per project root.
    pub files: Vec<usize>,
    /// MOUNTS forest roots: `Scope::External`/`Scope::Remote` events, path-compacted.
    pub mounts: Vec<usize>,
    /// WEB forest roots: `Scope::WebSearch`/`Scope::WebFetch` events.
    pub web: Vec<usize>,
    /// Per-session forest roots (a session's own temp files), keyed by session index; rendered
    /// nested under that session's row when expanded.
    pub session_files: HashMap<usize, Vec<usize>>,
}

/// Prefix for file nodes belonging to a particular checkout.
pub fn files_key_prefix(root: usize) -> String {
    format!("f{root}:")
}

/// Decode a FILES key into its checkout index and root-relative path.
pub fn parse_files_key(key: &str) -> Option<(usize, &str)> {
    let (root, rel) = key.strip_prefix('f')?.split_once(':')?;
    Some((root.parse().ok()?, rel))
}

// ---------------------------------------------------------------------------
// generic path trie -> forest
// ---------------------------------------------------------------------------

#[derive(Default)]
struct Trie {
    children: BTreeMap<String, Trie>,
    is_dir: bool,
    deleted: bool,
    events: Vec<usize>,
}

fn effective_is_dir(t: &Trie) -> bool {
    t.is_dir || !t.children.is_empty()
}

impl Trie {
    fn insert(&mut self, segs: &[String], is_dir: bool, deleted: bool, event: Option<usize>) {
        let Some((head, rest)) = segs.split_first() else { return };
        let child = self.children.entry(head.clone()).or_default();
        if rest.is_empty() {
            if is_dir {
                child.is_dir = true;
            }
            if deleted {
                child.deleted = true;
            }
            if let Some(e) = event {
                child.events.push(e);
            }
        } else {
            child.is_dir = true;
            child.insert(rest, is_dir, deleted, event);
        }
    }

    /// Neotree-style path compaction: while a dir has exactly one child and that child is
    /// itself a dir, merge them into a single `"a/b"`-named node. Files never merge into dirs.
    fn compact(&mut self) {
        let keys: Vec<String> = self.children.keys().cloned().collect();
        for key in keys {
            let mut child = self.children.remove(&key).unwrap();
            child.compact();
            let (merged_name, merged_child) = Self::merge_chain(key, child);
            self.children.insert(merged_name, merged_child);
        }
    }

    fn merge_chain(mut name: String, mut node: Trie) -> (String, Trie) {
        loop {
            if !effective_is_dir(&node) || node.children.len() != 1 {
                break;
            }
            let only_key = node.children.keys().next().unwrap().clone();
            let only_child = node.children.remove(&only_key).unwrap();
            if !effective_is_dir(&only_child) {
                node.children.insert(only_key, only_child);
                break;
            }
            name = format!("{name}/{only_key}");
            node = only_child;
        }
        (name, node)
    }

    /// Emit this trie's children as `Node`s (recursively), sorted dirs-first then
    /// case-insensitively by name. Returns the emitted children's indices.
    fn emit(
        &self,
        key_prefix: &str,
        path: &str,
        depth: usize,
        parent: Option<usize>,
        nodes: &mut Vec<Node>,
    ) -> Vec<usize> {
        let mut entries: Vec<(&String, &Trie)> = self.children.iter().collect();
        entries.sort_by_key(|(name, t)| (!effective_is_dir(t), name.to_lowercase()));

        let mut idxs = Vec::with_capacity(entries.len());
        for (name, trie) in entries {
            let is_dir = effective_is_dir(trie);
            let new_path = if path.is_empty() { name.clone() } else { format!("{path}/{name}") };
            let key = format!("{key_prefix}{new_path}");
            let idx = nodes.len();
            nodes.push(Node {
                name: name.clone(),
                key,
                parent,
                is_dir,
                depth,
                children: Vec::new(),
                events: Vec::new(),
                deleted: trie.deleted,
            });
            idxs.push(idx);
            let mut agg = trie.events.clone();
            if is_dir {
                let child_idxs = trie.emit(key_prefix, &new_path, depth + 1, Some(idx), nodes);
                for &ci in &child_idxs {
                    agg.extend_from_slice(&nodes[ci].events);
                }
                nodes[idx].children = child_idxs;
            }
            agg.sort_unstable();
            agg.dedup();
            nodes[idx].events = agg;
        }
        idxs
    }
}

fn path_components(p: &Path) -> Vec<String> {
    p.components()
        .filter_map(|c| match c {
            std::path::Component::Normal(s) => Some(s.to_string_lossy().to_string()),
            _ => None,
        })
        .collect()
}

/// Segments for an absolute path outside the project: `["~", rest…]` under `$HOME`, else
/// `["/<first component>", rest…]`.
fn external_style_segments(rel: &Path) -> Vec<String> {
    if let Some(home) = std::env::var_os("HOME").map(PathBuf::from)
        && let Ok(rest) = rel.strip_prefix(&home)
    {
        let mut v = vec!["~".to_string()];
        v.extend(path_components(rest));
        return v;
    }
    let mut comps = path_components(rel);
    if comps.is_empty() {
        return Vec::new();
    }
    let first = comps.remove(0);
    let mut v = vec![format!("/{first}")];
    v.extend(comps);
    v
}

/// Segment lists a `FileEvent.rel` decomposes into, for the forest its `scope` belongs to.
fn segments(model: &Model, e: &FileEvent) -> Vec<String> {
    match e.scope {
        Scope::Project(_) => path_components(&e.rel),
        Scope::External => external_style_segments(&e.rel),
        Scope::Remote => {
            let s = e.rel.to_string_lossy();
            let rest = s.strip_prefix("ssh://").unwrap_or(&s);
            let (host, path) = rest.split_once('/').unwrap_or((rest, ""));
            let mut v = vec![format!("ssh://{host}")];
            v.extend(path.split('/').filter(|p| !p.is_empty()).map(|p| p.to_string()));
            v
        }
        Scope::Session(_) => match model.session_dir_split(&e.rel) {
            Some((_, rest)) => path_components(&rest),
            None => external_style_segments(&e.rel),
        },
        Scope::WebSearch => vec!["search".to_string(), e.rel.to_string_lossy().to_string()],
        Scope::WebFetch => {
            let s = e.rel.to_string_lossy();
            let rest = s.strip_prefix("https://").or_else(|| s.strip_prefix("http://")).unwrap_or(&s);
            match rest.split_once('/') {
                Some((host, path)) => vec![host.to_string(), format!("/{path}")],
                None => vec![rest.to_string(), "/".to_string()],
            }
        }
    }
}

impl Tree {
    /// FILES = a gitignore-respecting disk walk plus every `Scope::Project` event (historical/
    /// deleted/gitignored files still appear); not path-compacted. MOUNTS = `External`/`Remote`
    /// events, path-compacted. WEB = `WebSearch`/`WebFetch` events, not compacted. One forest
    /// per session index touched by a `Scope::Session` event, path-compacted, rendered at depth
    /// 1 (nested under that session's row). Only FILES does disk I/O.
    pub fn build(model: &Model) -> Tree {
        let mut files_tries: Vec<Trie> = (0..model.roots.len()).map(|_| Trie::default()).collect();

        for (i, root) in model.roots.iter().enumerate() {
            for entry in crate::snapshot::project_walker(&root.path, model.nested_roots(i)).flatten() {
                let path = entry.path();
                if path == root.path {
                    continue;
                }
                let Ok(rel) = path.strip_prefix(&root.path) else { continue };
                if rel.as_os_str().is_empty() {
                    continue;
                }
                let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
                files_tries[i].insert(&path_components(rel), is_dir, false, None);
            }
        }

        let mut mounts_trie = Trie::default();
        let mut web_trie = Trie::default();
        let mut session_tries: HashMap<usize, Trie> = HashMap::new();

        for (i, e) in model.events.iter().enumerate() {
            match e.scope {
                Scope::Project(root) => {
                    let deleted = !model.roots[root].path.join(&e.rel).exists();
                    files_tries[root].insert(&path_components(&e.rel), false, deleted, Some(i));
                }
                Scope::External | Scope::Remote => {
                    mounts_trie.insert(&segments(model, e), false, false, Some(i));
                }
                Scope::WebSearch | Scope::WebFetch => {
                    web_trie.insert(&segments(model, e), false, false, Some(i));
                }
                Scope::Session(s) => {
                    session_tries.entry(s).or_default().insert(&segments(model, e), false, false, Some(i));
                }
            }
        }
        mounts_trie.compact();
        for t in session_tries.values_mut() {
            t.compact();
        }

        let mut nodes = Vec::new();
        let files = if files_tries.len() == 1 {
            files_tries[0].emit(&files_key_prefix(0), "", 0, None, &mut nodes)
        } else {
            let mut files = Vec::with_capacity(files_tries.len());
            for (i, trie) in files_tries.iter().enumerate() {
                let idx = nodes.len();
                nodes.push(Node {
                    name: model.roots[i].label.clone(),
                    key: files_key_prefix(i),
                    parent: None,
                    is_dir: true,
                    depth: 0,
                    children: Vec::new(),
                    events: Vec::new(),
                    deleted: false,
                });
                let children = trie.emit(&files_key_prefix(i), "", 1, Some(idx), &mut nodes);
                let mut events = Vec::new();
                for &child in &children {
                    events.extend_from_slice(&nodes[child].events);
                }
                events.sort_unstable();
                events.dedup();
                nodes[idx].children = children;
                nodes[idx].events = events;
                files.push(idx);
            }
            files
        };
        let mounts = mounts_trie.emit("m:", "", 0, None, &mut nodes);
        let web = web_trie.emit("w:", "", 0, None, &mut nodes);
        let mut session_files = HashMap::new();
        for (s, trie) in session_tries {
            let roots = trie.emit(&format!("s{s}:"), "", 1, None, &mut nodes);
            session_files.insert(s, roots);
        }

        Tree { nodes, files, mounts, web, session_files }
    }

    /// Index of the node with this `key`. Keys are stable across a `Tree::build` (derived from
    /// path strings), unlike node indices, so this is how a `Row::Node` selection survives a
    /// rebuild that reassigns every index.
    pub fn find_by_key(&self, key: &str) -> Option<usize> {
        self.nodes.iter().position(|n| n.key == key)
    }
}

// ---------------------------------------------------------------------------
// row list
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Row {
    Section(&'static str),
    Participant(ParticipantId, usize),
    Node(usize),
}

/// A row's searchable name without its indentation or expand/collapse marker.
pub fn row_label(model: &Model, tree: &Tree, row: Row) -> String {
    match row {
        Row::Section(name) => name.to_uppercase(),
        Row::Participant(pid, _) => {
            let p = &model.participants[pid.0];
            if pid == Model::YOU {
                "you".to_string()
            } else if p.kind == ParticipantKind::Main {
                match p.session {
                    Some(sidx) => {
                        let s = &model.sessions[sidx];
                        format!("{} · {}", s.title, s.start.with_timezone(&chrono::Local).format("%m-%d %H:%M"))
                    }
                    None => p.label.clone(),
                }
            } else {
                p.label.clone()
            }
        }
        Row::Node(idx) => tree.nodes[idx].name.clone(),
    }
}

pub struct ExpandState {
    /// explicit user toggles, keyed by `Node.key`; always wins over `auto_open`/`all_open`.
    pub dir_overrides: HashMap<String, bool>,
    /// default for dirs without an override: false (neotree: everything collapsed) until `zR`.
    pub all_open: bool,
    /// session indices explicitly expanded (sessions are collapsed by default).
    pub expanded_sessions: HashSet<usize>,
    pub touched_only: bool,
    /// cursor-on-session auto-expansion enabled (`a` toggles).
    pub auto_follow: bool,
    /// node indices force-opened for the currently focused session; recomputed from
    /// `auto_focus` by `App::refresh_rows`/`rebuild`. An explicit `dir_overrides` entry always
    /// wins over this, so the user can still collapse an auto-opened dir.
    pub auto_open: HashSet<usize>,
    /// participants an auto-opened dir set was last computed for; only updated when the cursor
    /// lands on a `Row::Participant` (so browsing into the revealed Node rows doesn't collapse
    /// them again — `auto_open_nodes` is recomputed from this against the current tree on every
    /// row-list rebuild, so it stays correct across a `Tree::build` even though node indices
    /// change).
    pub auto_focus: Vec<ParticipantId>,
}

impl Default for ExpandState {
    fn default() -> Self {
        ExpandState {
            dir_overrides: HashMap::new(),
            all_open: false,
            expanded_sessions: HashSet::new(),
            touched_only: false,
            auto_follow: true,
            auto_open: HashSet::new(),
            auto_focus: Vec::new(),
        }
    }
}

impl ExpandState {
    pub fn is_dir_expanded(&self, tree: &Tree, idx: usize) -> bool {
        self.dir_overrides
            .get(&tree.nodes[idx].key)
            .copied()
            .unwrap_or_else(|| self.auto_open.contains(&idx) || self.all_open)
    }

    pub fn toggle_dir(&mut self, tree: &Tree, idx: usize) {
        let cur = self.is_dir_expanded(tree, idx);
        self.dir_overrides.insert(tree.nodes[idx].key.clone(), !cur);
    }

    /// Set a dir open/closed; recursively apply the state to its descendant dirs when requested.
    pub fn set_dir(&mut self, tree: &Tree, idx: usize, open: bool, recursive: bool) {
        let node = &tree.nodes[idx];
        if !node.is_dir {
            return;
        }
        self.dir_overrides.insert(node.key.clone(), open);
        if recursive {
            for &child in &node.children {
                self.set_dir(tree, child, open, true);
            }
        }
    }

    /// Apply `zo`/`zO`/`zc`/`zC`, returning an enclosing row when closing it moves the cursor.
    pub fn fold(&mut self, model: &Model, tree: &Tree, row: Row, open: bool, recursive: bool) -> Option<Row> {
        match row {
            Row::Node(idx) => {
                let node = &tree.nodes[idx];
                if open {
                    self.set_dir(tree, idx, true, recursive);
                } else if !node.is_dir {
                    let parent = node.parent?;
                    self.set_dir(tree, parent, false, recursive);
                    return Some(Row::Node(parent));
                } else if recursive || self.is_dir_expanded(tree, idx) {
                    self.set_dir(tree, idx, false, recursive);
                } else if let Some(parent) = node.parent {
                    self.set_dir(tree, parent, false, false);
                    return Some(Row::Node(parent));
                }
            }
            Row::Participant(pid, _) if pid != Model::YOU => {
                let participant = &model.participants[pid.0];
                let session = participant.session?;
                match participant.kind {
                    ParticipantKind::Main => {
                        if open {
                            self.expanded_sessions.insert(session);
                        } else {
                            self.expanded_sessions.remove(&session);
                        }
                        if recursive && let Some(roots) = tree.session_files.get(&session) {
                            for &root in roots {
                                self.set_dir(tree, root, open, true);
                            }
                        }
                    }
                    ParticipantKind::Subagent | ParticipantKind::Advisor if !open => {
                        self.expanded_sessions.remove(&session);
                        return Some(Row::Participant(model.sessions[session].main, 0));
                    }
                    _ => {}
                }
            }
            _ => {}
        }
        None
    }

    /// Close every dir. Also clears `auto_focus`/`auto_open`, so a session that had auto-opened
    /// dirs stays fully closed even while the cursor is still sitting on its row — it reopens
    /// only once the cursor next lands on a participant row (`refocus`).
    pub fn collapse_all(&mut self) {
        self.dir_overrides.clear();
        self.all_open = false;
        self.auto_focus.clear();
        self.auto_open.clear();
    }

    pub fn expand_all(&mut self) {
        self.dir_overrides.clear();
        self.all_open = true;
    }

    /// The cursor landed on `row` (from a movement, not a display toggle): if it's a
    /// `Participant` row, refresh `auto_focus` from it — otherwise `auto_focus` is left as-is,
    /// so browsing into the Node rows an auto-opened session revealed doesn't collapse them
    /// again. Either way, `auto_open` is then recomputed fresh from `auto_focus` against
    /// `tree`, so it stays correct across a `Tree::build` even though node indices change.
    pub fn refocus(&mut self, model: &Model, tree: &Tree, row: Option<Row>) {
        if let Some(r @ Row::Participant(..)) = row {
            self.auto_focus = focus_of(model, r);
        }
        self.refresh_auto_open(model, tree);
    }

    /// Recompute `auto_open` from the current `auto_focus` without touching it: for display
    /// toggles (Space/za, zM/zR, `T`, `a`) that must not re-derive focus from wherever the
    /// cursor merely happens to be sitting.
    pub fn refresh_auto_open(&mut self, model: &Model, tree: &Tree) {
        self.auto_open = if self.auto_follow { auto_open_nodes(model, tree, &self.auto_focus) } else { HashSet::new() };
    }

    pub fn toggle_session(&mut self, session_idx: usize) {
        if !self.expanded_sessions.remove(&session_idx) {
            self.expanded_sessions.insert(session_idx);
        }
    }
}

/// The participants whose events should auto-expand dirs when `row` is selected: a Main
/// session row focuses every participant in that session (main + subagents/advisors); a
/// subagent/advisor row focuses just itself. Everything else (You, a Node, a Section) is empty.
pub fn focus_of(model: &Model, row: Row) -> Vec<ParticipantId> {
    let Row::Participant(pid, _) = row else { return Vec::new() };
    if pid == Model::YOU {
        return Vec::new();
    }
    match model.participants[pid.0].kind {
        ParticipantKind::Main => match model.participants[pid.0].session {
            Some(s) => model
                .participants
                .iter()
                .enumerate()
                .filter(|(_, p)| p.session == Some(s))
                .map(|(i, _)| ParticipantId(i))
                .collect(),
            None => Vec::new(),
        },
        ParticipantKind::Subagent | ParticipantKind::Advisor => vec![pid],
        ParticipantKind::You => Vec::new(),
    }
}

/// Every dir node (in any forest) carrying an event from `focus`, for temporary auto-expansion.
/// Dir `events` already aggregate every descendant, so this opens every ancestor of each
/// touched file too. Empty `focus` (nothing selected, or a non-participant row) opens nothing.
pub fn auto_open_nodes(model: &Model, tree: &Tree, focus: &[ParticipantId]) -> HashSet<usize> {
    if focus.is_empty() {
        return HashSet::new();
    }
    tree.nodes
        .iter()
        .enumerate()
        .filter(|(_, n)| {
            n.is_dir
                && n.events.iter().any(|&i| focus.contains(&model.events[i].who) && model.visible(model.events[i].who))
        })
        .map(|(idx, _)| idx)
        .collect()
}

fn node_has_visible_events(model: &Model, node: &Node) -> bool {
    node.events.iter().any(|&i| model.visible(model.events[i].who))
}

fn push_node_rows(model: &Model, tree: &Tree, state: &ExpandState, idx: usize, events_only: bool, rows: &mut Vec<Row>) {
    let node = &tree.nodes[idx];
    if (state.touched_only || events_only) && !node_has_visible_events(model, node) {
        return;
    }
    rows.push(Row::Node(idx));
    if node.is_dir && state.is_dir_expanded(tree, idx) {
        for &c in &node.children {
            push_node_rows(model, tree, state, c, events_only, rows);
        }
    }
}

/// Build the shared row list: participants section (you + one row per visible session, with
/// subagent/advisor rows and the session's own touched-temp-files tree nested beneath an
/// expanded session), then FILES (always shown), then MOUNTS/WEB (only when either has any
/// visible event, since most projects never touch a mount or the web).
pub fn build_rows(model: &Model, tree: &Tree, state: &ExpandState) -> Vec<Row> {
    let mut rows = Vec::new();
    rows.push(Row::Section("participants"));
    rows.push(Row::Participant(Model::YOU, 0));

    for (sidx, session) in model.sessions.iter().enumerate() {
        if !model.visible(session.main) {
            continue;
        }
        rows.push(Row::Participant(session.main, 0));
        if state.expanded_sessions.contains(&sidx) {
            for (pidx, p) in model.participants.iter().enumerate() {
                if p.session == Some(sidx) && matches!(p.kind, ParticipantKind::Subagent | ParticipantKind::Advisor) {
                    rows.push(Row::Participant(ParticipantId(pidx), 1));
                }
            }
            if let Some(roots) = tree.session_files.get(&sidx) {
                for &root_idx in roots {
                    push_node_rows(model, tree, state, root_idx, true, &mut rows);
                }
            }
        }
    }

    rows.push(Row::Section("files"));
    for &root_idx in &tree.files {
        push_node_rows(model, tree, state, root_idx, false, &mut rows);
    }

    let mut mount_rows = Vec::new();
    for &root_idx in &tree.mounts {
        push_node_rows(model, tree, state, root_idx, true, &mut mount_rows);
    }
    if !mount_rows.is_empty() {
        rows.push(Row::Section("mounts"));
        rows.extend(mount_rows);
    }

    let mut web_rows = Vec::new();
    for &root_idx in &tree.web {
        push_node_rows(model, tree, state, root_idx, true, &mut web_rows);
    }
    if !web_rows.is_empty() {
        rows.push(Row::Section("web"));
        rows.extend(web_rows);
    }

    rows
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{EventDetail, ParticipantKind, Session, TouchKind, TouchSource};
    use std::fs;

    fn temp_root(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("antty-tree-test-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("src/ui")).unwrap();
        fs::write(dir.join("src/a.rs"), "").unwrap();
        fs::write(dir.join("src/ui/b.rs"), "").unwrap();
        fs::write(dir.join("Cargo.toml"), "").unwrap();
        dir
    }

    fn model_with_main(root: PathBuf) -> (Model, ParticipantId) {
        let mut model = Model::new(vec![root.clone()]);
        let file = root.join("session.jsonl");
        let who = model.get_or_create_participant(&file, ParticipantKind::Main, "main".into(), None, None);
        let idx = model.sessions.len();
        model.sessions.push(Session {
            file,
            id: String::new(),
            title: "(untitled)".into(),
            cwd: root,
            start: chrono::Utc::now(),
            last_seen: chrono::DateTime::<chrono::Utc>::MIN_UTC,
            end: None,
            main: who,
        });
        model.participants[who.0].session = Some(idx);
        (model, who)
    }

    fn push_event(model: &mut Model, who: ParticipantId, rel: PathBuf, scope: Scope) {
        let now = chrono::Utc::now();
        model.events.push(FileEvent {
            who,
            rel,
            scope,
            kind: TouchKind::Read,
            source: TouchSource::Tool("read".into()),
            start: now,
            end: now,
            tool_call_id: None,
            detail: EventDetail::None,
        });
    }

    fn node_names(tree: &Tree, rows: &[Row]) -> Vec<String> {
        rows.iter()
            .filter_map(|r| match r {
                Row::Node(idx) => Some(tree.nodes[*idx].name.clone()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn default_rows_show_only_top_level_files_entries() {
        let root = temp_root("default");
        let (model, _) = model_with_main(root);
        let tree = Tree::build(&model);
        let rows = build_rows(&model, &tree, &ExpandState::default());
        let names = node_names(&tree, &rows);
        assert!(names.contains(&"src".to_string()));
        assert!(names.contains(&"Cargo.toml".to_string()));
        assert!(!names.contains(&"a.rs".to_string()), "src should start collapsed: {names:?}");
    }

    #[test]
    fn separate_worktrees_have_distinct_files_nodes_and_events() {
        let a = temp_root("worktree-a").canonicalize().unwrap();
        let b = temp_root("worktree-b").canonicalize().unwrap();
        let mut model = Model::new(vec![a, b]);
        push_event(&mut model, Model::YOU, PathBuf::from("src/a.rs"), Scope::Project(0));
        push_event(&mut model, Model::YOU, PathBuf::from("src/a.rs"), Scope::Project(1));

        let tree = Tree::build(&model);
        assert_eq!(tree.files.len(), 2);
        for i in 0..2 {
            let wrapper = &tree.nodes[tree.files[i]];
            assert_eq!(wrapper.name, model.roots[i].label);
            assert_eq!(wrapper.key, files_key_prefix(i));
            assert_eq!(wrapper.depth, 0);
            assert_eq!(wrapper.parent, None);
            assert_eq!(wrapper.events, vec![i]);
            let key = format!("f{i}:src/a.rs");
            assert_eq!(parse_files_key(&key), Some((i, "src/a.rs")));
            assert_eq!(parse_files_key(&wrapper.key), Some((i, "")));
            let file = &tree.nodes[tree.find_by_key(&key).unwrap()];
            assert_eq!(file.name, "a.rs");
            assert_eq!(file.events, vec![i]);
        }
        assert_eq!(parse_files_key("m:src/a.rs"), None);
    }

    #[test]
    fn nested_worktree_is_walked_only_under_its_own_root() {
        let outer = temp_root("nested-outer").canonicalize().unwrap();
        let inner = outer.join("nested");
        fs::create_dir_all(inner.join("src")).unwrap();
        fs::write(inner.join("src/a.rs"), "inner").unwrap();
        let model = Model::new(vec![outer, inner.canonicalize().unwrap()]);

        let tree = Tree::build(&model);
        assert_eq!(tree.files.len(), 2);
        assert!(tree.find_by_key("f0:nested").is_none());
        assert!(tree.find_by_key("f0:nested/src/a.rs").is_none());
        assert!(tree.find_by_key("f0:src/a.rs").is_some());
        assert!(tree.find_by_key("f1:src/a.rs").is_some());
    }

    #[test]
    fn mounts_section_only_appears_with_events_and_compacts_common_prefix() {
        let root = temp_root("mounts");
        let (mut model, who) = model_with_main(root);
        let tree_empty = Tree::build(&model);
        let rows_empty = build_rows(&model, &tree_empty, &ExpandState::default());
        assert!(!rows_empty.iter().any(|r| matches!(r, Row::Section("mounts"))));

        let home = PathBuf::from(std::env::var("HOME").unwrap());
        push_event(&mut model, who, home.join("x/y/one.txt"), Scope::External);
        push_event(&mut model, who, home.join("x/y/two.txt"), Scope::External);
        let tree = Tree::build(&model);
        let rows = build_rows(&model, &tree, &ExpandState::default());
        assert!(rows.iter().any(|r| matches!(r, Row::Section("mounts"))));
        assert_eq!(tree.mounts.len(), 1);
        assert_eq!(tree.nodes[tree.mounts[0]].name, "~/x/y");
        assert_eq!(tree.nodes[tree.mounts[0]].children.len(), 2);
    }

    #[test]
    fn auto_open_expands_dirs_touched_by_the_focused_row_only() {
        let root = temp_root("autoopen");
        let (mut model, who) = model_with_main(root);
        push_event(&mut model, who, PathBuf::from("src/ui/b.rs"), Scope::Project(0));
        let tree = Tree::build(&model);

        let main_row = Row::Participant(model.sessions[0].main, 0);
        let focus = focus_of(&model, main_row);
        let mut state = ExpandState { auto_open: auto_open_nodes(&model, &tree, &focus), ..ExpandState::default() };
        let rows = build_rows(&model, &tree, &state);
        assert!(node_names(&tree, &rows).contains(&"b.rs".to_string()));

        state.auto_open = HashSet::new();
        let rows_no_focus = build_rows(&model, &tree, &state);
        assert!(!node_names(&tree, &rows_no_focus).contains(&"b.rs".to_string()));
    }

    #[test]
    fn explicit_toggle_collapses_a_dir_that_auto_open_opened() {
        // Regression: `is_dir_expanded` must let an explicit user toggle override `auto_open`,
        // and `toggle_dir` must compute its "currently open" baseline including `auto_open` —
        // otherwise Space on an auto-opened dir is a no-op (or takes two presses).
        let root = temp_root("override-beats-autoopen");
        let (mut model, who) = model_with_main(root);
        push_event(&mut model, who, PathBuf::from("src/ui/b.rs"), Scope::Project(0));
        let tree = Tree::build(&model);

        let focus = focus_of(&model, Row::Participant(model.sessions[0].main, 0));
        let mut state = ExpandState { auto_open: auto_open_nodes(&model, &tree, &focus), ..ExpandState::default() };
        let src_idx = tree.files.iter().copied().find(|&i| tree.nodes[i].name == "src").unwrap();
        assert!(state.is_dir_expanded(&tree, src_idx), "auto_open should open src");

        state.toggle_dir(&tree, src_idx);
        assert!(!state.is_dir_expanded(&tree, src_idx), "one explicit toggle must collapse an auto-opened dir");

        let rows = build_rows(&model, &tree, &state);
        assert!(!node_names(&tree, &rows).contains(&"b.rs".to_string()));
    }

    #[test]
    fn refocus_is_sticky_for_node_and_section_rows_but_not_for_participant_rows() {
        // Regression: the sticky-focus logic lives in `ExpandState::refocus`, exercised
        // directly here rather than reimplemented in the test (App::land_on is a thin wrapper
        // around it plus row-list bookkeeping that isn't unit-testable without a real Tree/Model).
        let root = temp_root("refocus-sticky");
        let (mut model, who) = model_with_main(root);
        push_event(&mut model, who, PathBuf::from("src/ui/b.rs"), Scope::Project(0));
        let tree = Tree::build(&model);
        let mut state = ExpandState::default();

        let main_row = Row::Participant(model.sessions[0].main, 0);
        state.refocus(&model, &tree, Some(main_row));
        assert!(node_names(&tree, &build_rows(&model, &tree, &state)).contains(&"b.rs".to_string()));

        // Landing on a Node row (the user browsing into the dirs that just opened) must not
        // clear auto_focus, or the files it revealed would vanish out from under the cursor.
        let src_idx = tree.files.iter().copied().find(|&i| tree.nodes[i].name == "src").unwrap();
        state.refocus(&model, &tree, Some(Row::Node(src_idx)));
        assert!(
            node_names(&tree, &build_rows(&model, &tree, &state)).contains(&"b.rs".to_string()),
            "browsing into a Node row must not collapse the auto-opened dirs"
        );

        // Landing on a Section row: same (sticky).
        state.refocus(&model, &tree, Some(Row::Section("files")));
        assert!(node_names(&tree, &build_rows(&model, &tree, &state)).contains(&"b.rs".to_string()));

        // Landing back on You (a Participant row) does refresh focus, clearing it.
        state.refocus(&model, &tree, Some(Row::Participant(Model::YOU, 0)));
        assert!(!node_names(&tree, &build_rows(&model, &tree, &state)).contains(&"b.rs".to_string()));
    }

    #[test]
    fn collapse_all_closes_dirs_even_while_still_on_the_focused_session_row() {
        // Regression: zM must mean "closed now", not "closed until the next unrelated
        // recompute reopens it" — `refresh_auto_open` (what a pure toggle like zM/zR/`T`/`a`
        // calls, as opposed to `refocus`) must not resurrect a cleared `auto_focus`.
        let root = temp_root("zm-clears-focus");
        let (mut model, who) = model_with_main(root);
        push_event(&mut model, who, PathBuf::from("src/ui/b.rs"), Scope::Project(0));
        let tree = Tree::build(&model);
        let mut state = ExpandState::default();

        state.refocus(&model, &tree, Some(Row::Participant(model.sessions[0].main, 0)));
        assert!(node_names(&tree, &build_rows(&model, &tree, &state)).contains(&"b.rs".to_string()));

        state.collapse_all();
        // No `refocus` call: simulates zM firing as a pure toggle while the cursor is still
        // parked on the same session row, exactly like `App::refresh_rows` would drive it.
        state.refresh_auto_open(&model, &tree);
        assert!(
            !node_names(&tree, &build_rows(&model, &tree, &state)).contains(&"b.rs".to_string()),
            "zM must close dirs auto_open had opened, even while still on that session's row"
        );
    }

    #[test]
    fn expand_all_and_collapse_all_toggle_every_dir() {
        let root = temp_root("expandall");
        let (mut model, who) = model_with_main(root);
        push_event(&mut model, who, PathBuf::from("src/ui/b.rs"), Scope::Project(0));
        let tree = Tree::build(&model);

        let mut state = ExpandState::default();
        state.expand_all();
        let rows = build_rows(&model, &tree, &state);
        assert!(node_names(&tree, &rows).contains(&"b.rs".to_string()));

        state.collapse_all();
        let rows2 = build_rows(&model, &tree, &state);
        assert!(!node_names(&tree, &rows2).contains(&"b.rs".to_string()));
    }

    #[test]
    fn zc_on_file_closes_parent_and_returns_it() {
        let root = temp_root("zc-file");
        let (model, _) = model_with_main(root);
        let tree = Tree::build(&model);
        let src = tree.nodes.iter().position(|n| n.name == "src" && n.is_dir).unwrap();
        let a_rs = tree.nodes.iter().position(|n| n.name == "a.rs").unwrap();
        let mut state = ExpandState::default();
        state.expand_all();

        assert_eq!(state.fold(&model, &tree, Row::Node(a_rs), false, false), Some(Row::Node(src)));
        assert!(!state.is_dir_expanded(&tree, src));
    }

    #[test]
    fn zc_on_collapsed_dir_escalates_to_parent() {
        let root = temp_root("zc-dir");
        let (model, _) = model_with_main(root);
        let tree = Tree::build(&model);
        let src = tree.nodes.iter().position(|n| n.name == "src" && n.is_dir).unwrap();
        let ui = tree.nodes.iter().position(|n| n.name == "ui" && n.is_dir).unwrap();
        let mut state = ExpandState::default();
        state.expand_all();

        assert_eq!(state.fold(&model, &tree, Row::Node(ui), false, false), None);
        assert!(!state.is_dir_expanded(&tree, ui));
        assert_eq!(state.fold(&model, &tree, Row::Node(ui), false, false), Some(Row::Node(src)));
        assert!(!state.is_dir_expanded(&tree, src));
    }

    #[test]
    fn zc_recursive_then_zo_reopens_one_level_only() {
        let root = temp_root("zc-recursive");
        let (model, _) = model_with_main(root);
        let tree = Tree::build(&model);
        let src = tree.nodes.iter().position(|n| n.name == "src" && n.is_dir).unwrap();
        let mut state = ExpandState::default();
        state.expand_all();
        state.fold(&model, &tree, Row::Node(src), false, true);
        state.fold(&model, &tree, Row::Node(src), true, false);

        let names = node_names(&tree, &build_rows(&model, &tree, &state));
        assert!(names.contains(&"ui".to_string()));
        assert!(names.contains(&"a.rs".to_string()));
        assert!(!names.contains(&"b.rs".to_string()));
    }

    #[test]
    fn zo_recursive_opens_every_descendant() {
        let root = temp_root("zo-recursive");
        let (model, _) = model_with_main(root);
        let tree = Tree::build(&model);
        let src = tree.nodes.iter().position(|n| n.name == "src" && n.is_dir).unwrap();
        let mut state = ExpandState::default();
        state.fold(&model, &tree, Row::Node(src), true, true);

        assert!(node_names(&tree, &build_rows(&model, &tree, &state)).contains(&"b.rs".to_string()));
    }

    #[test]
    fn zc_on_subagent_collapses_session_and_returns_main_row() {
        let root = temp_root("zc-subagent");
        let (mut model, _) = model_with_main(root.clone());
        let sub = model.get_or_create_participant(
            &root.join("sub.jsonl"),
            ParticipantKind::Subagent,
            "sub".into(),
            None,
            None,
        );
        model.participants[sub.0].session = Some(0);
        let tree = Tree::build(&model);
        let mut state = ExpandState::default();
        state.expanded_sessions.insert(0);

        assert_eq!(
            state.fold(&model, &tree, Row::Participant(sub, 1), false, false),
            Some(Row::Participant(model.sessions[0].main, 0))
        );
        assert!(state.expanded_sessions.is_empty());
    }
}
