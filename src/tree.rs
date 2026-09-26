use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use crate::model::{Model, ParticipantId, ParticipantKind};

pub struct Node {
    pub name: String,
    pub rel: PathBuf,
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
    pub roots: Vec<usize>,
    /// rel-path -> node index; not yet consumed (no feature needs point lookups yet).
    #[allow(dead_code)]
    pub by_rel: HashMap<PathBuf, usize>,
}

fn push_ancestors(rel: &Path, dirs: &mut HashSet<PathBuf>) {
    let mut cur = rel.parent();
    while let Some(p) = cur {
        if p.as_os_str().is_empty() {
            break;
        }
        if !dirs.insert(p.to_path_buf()) {
            break; // already present, so its ancestors are too
        }
        cur = p.parent();
    }
}

fn sort_key(rel: &Path, is_dir_of: &HashMap<PathBuf, bool>) -> (bool, String) {
    let is_dir = *is_dir_of.get(rel).unwrap_or(&false);
    let name = rel
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("")
        .to_lowercase();
    (!is_dir, name) // dirs (is_dir=true -> !is_dir=false) sort first
}

#[allow(clippy::too_many_arguments)]
fn build_level(
    parent_rel: Option<&Path>,
    depth: usize,
    child_map: &HashMap<Option<PathBuf>, Vec<PathBuf>>,
    is_dir_of: &HashMap<PathBuf, bool>,
    event_files: &HashMap<PathBuf, Vec<usize>>,
    root: &Path,
    nodes: &mut Vec<Node>,
    by_rel: &mut HashMap<PathBuf, usize>,
) -> Vec<usize> {
    let key = parent_rel.map(|p| p.to_path_buf());
    let mut children_rels = child_map.get(&key).cloned().unwrap_or_default();
    children_rels.sort_by_key(|a| sort_key(a, is_dir_of));

    let mut idxs = Vec::with_capacity(children_rels.len());
    for rel in children_rels {
        let is_dir = *is_dir_of.get(&rel).unwrap_or(&false);
        let name = rel.file_name().and_then(|n| n.to_str()).unwrap_or("").to_string();
        let own_events = event_files.get(&rel).cloned().unwrap_or_default();
        let deleted = !is_dir && !root.join(&rel).exists();
        let idx = nodes.len();
        nodes.push(Node { name, rel: rel.clone(), is_dir, depth, children: Vec::new(), events: own_events, deleted });
        by_rel.insert(rel.clone(), idx);
        idxs.push(idx);
        if is_dir {
            let child_idxs = build_level(Some(&rel), depth + 1, child_map, is_dir_of, event_files, root, nodes, by_rel);
            let mut agg: Vec<usize> = Vec::new();
            for &ci in &child_idxs {
                agg.extend_from_slice(&nodes[ci].events);
            }
            agg.sort_unstable();
            agg.dedup();
            nodes[idx].children = child_idxs;
            nodes[idx].events = agg;
        }
    }
    idxs
}

impl Tree {
    /// Node set = union of a gitignore-respecting disk walk and every `FileEvent.rel` (plus its
    /// ancestor dirs). Historical/deleted/gitignored files still appear; only disk-walk entries
    /// get their `is_dir` confirmed from the filesystem.
    pub fn build(model: &Model) -> Tree {
        let mut dirs: HashSet<PathBuf> = HashSet::new();
        let mut files: HashSet<PathBuf> = HashSet::new();

        let walker = ignore::WalkBuilder::new(&model.root)
            .hidden(false)
            .git_ignore(true)
            .filter_entry(|e| e.file_name() != ".git")
            .build();
        for entry in walker.flatten() {
            let path = entry.path();
            if path == model.root {
                continue;
            }
            let Ok(rel) = path.strip_prefix(&model.root) else { continue };
            if rel.as_os_str().is_empty() {
                continue;
            }
            let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
            if is_dir {
                dirs.insert(rel.to_path_buf());
            } else {
                files.insert(rel.to_path_buf());
            }
        }

        let mut event_files: HashMap<PathBuf, Vec<usize>> = HashMap::new();
        for (i, e) in model.events.iter().enumerate() {
            event_files.entry(e.rel.clone()).or_default().push(i);
        }
        for rel in event_files.keys() {
            files.insert(rel.clone());
        }
        let all_files: Vec<PathBuf> = files.iter().cloned().collect();
        for rel in &all_files {
            push_ancestors(rel, &mut dirs);
        }

        let mut is_dir_of: HashMap<PathBuf, bool> = HashMap::new();
        for d in &dirs {
            is_dir_of.insert(d.clone(), true);
        }
        for f in &files {
            is_dir_of.entry(f.clone()).or_insert(false);
        }

        let mut child_map: HashMap<Option<PathBuf>, Vec<PathBuf>> = HashMap::new();
        for rel in is_dir_of.keys() {
            let parent = rel.parent().filter(|p| !p.as_os_str().is_empty()).map(|p| p.to_path_buf());
            child_map.entry(parent).or_default().push(rel.clone());
        }

        let mut nodes = Vec::new();
        let mut by_rel = HashMap::new();
        let roots = build_level(None, 0, &child_map, &is_dir_of, &event_files, &model.root, &mut nodes, &mut by_rel);
        Tree { nodes, roots, by_rel }
    }
}

// ---------------------------------------------------------------------------
// row list
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy)]
pub enum Row {
    Section(&'static str),
    Participant(ParticipantId, usize),
    Node(usize),
}

#[derive(Default)]
pub struct ExpandState {
    /// explicit user toggles overriding the default open/closed rule for a dir, keyed by rel path.
    pub dir_overrides: HashMap<PathBuf, bool>,
    /// session indices explicitly expanded (sessions are collapsed by default).
    pub expanded_sessions: HashSet<usize>,
    pub touched_only: bool,
}

impl ExpandState {
    /// Dirs default open at depth 0 or when they carry any event, closed otherwise.
    pub fn is_dir_expanded(&self, node: &Node) -> bool {
        if let Some(&v) = self.dir_overrides.get(&node.rel) {
            return v;
        }
        node.depth < 1 || !node.events.is_empty()
    }

    pub fn toggle_dir(&mut self, node: &Node) {
        let cur = self.is_dir_expanded(node);
        self.dir_overrides.insert(node.rel.clone(), !cur);
    }

    pub fn toggle_session(&mut self, session_idx: usize) {
        if !self.expanded_sessions.remove(&session_idx) {
            self.expanded_sessions.insert(session_idx);
        }
    }
}

fn node_has_visible_events(model: &Model, node: &Node) -> bool {
    node.events.iter().any(|&i| model.visible(model.events[i].who))
}

fn push_node_rows(model: &Model, tree: &Tree, state: &ExpandState, idx: usize, rows: &mut Vec<Row>) {
    let node = &tree.nodes[idx];
    if state.touched_only && !node_has_visible_events(model, node) {
        return;
    }
    rows.push(Row::Node(idx));
    if node.is_dir && state.is_dir_expanded(node) {
        for &c in &node.children {
            push_node_rows(model, tree, state, c, rows);
        }
    }
}

/// Build the shared row list: participants section (you + one row per visible session, with
/// subagent/advisor rows nested beneath expanded sessions), then the files section (flattened
/// tree honoring `state`).
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
        }
    }

    rows.push(Row::Section("files"));
    for &root_idx in &tree.roots {
        push_node_rows(model, tree, state, root_idx, &mut rows);
    }
    rows
}
