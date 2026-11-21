use std::collections::HashSet;

use crate::model::{FileEvent, Scope, TouchKind, TouchSource};

/// Fixed action categories in overlay order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Action {
    Eval,
    Read,
    Write,
    Edit,
    Search,
    Fetch,
    Mcp,
    Inferred,
    Fs,
}

impl Action {
    pub const ALL: [Self; 9] = [
        Self::Eval,
        Self::Read,
        Self::Write,
        Self::Edit,
        Self::Search,
        Self::Fetch,
        Self::Mcp,
        Self::Inferred,
        Self::Fs,
    ];
}

/// A fixed category, or a watcher change attributed to a named tool's call window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActionKey<'a> {
    Fixed(Action),
    FsDuring(&'a str),
}

pub const SCRIPT_EXTS: &[&str] =
    &["sh", "bash", "zsh", "fish", "py", "js", "mjs", "cjs", "ts", "rb", "pl", "lua", "ps1"];

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ActionFilter {
    pub actions: HashSet<Action>,
    /// Tool names whose attributed watcher changes are shown.
    pub fs_during: HashSet<String>,
    /// Narrow file-path actions to `SCRIPT_EXTS` targets.
    pub scripts_only: bool,
}

pub fn action_key(e: &FileEvent) -> ActionKey<'_> {
    let action = match &e.source {
        TouchSource::Tool(tool) => match tool.as_str() {
            "eval" => Action::Eval,
            "read" if e.scope == Scope::WebFetch => Action::Fetch,
            "read" => Action::Read,
            "write" => Action::Write,
            "edit" => Action::Edit,
            "web_search" => Action::Search,
            "mcp" => Action::Mcp,
            _ if e.kind == TouchKind::Read => Action::Read,
            _ => Action::Write,
        },
        TouchSource::Window(tool) => return ActionKey::FsDuring(tool),
        TouchSource::Inferred => Action::Inferred,
        TouchSource::Watcher => Action::Fs,
    };
    ActionKey::Fixed(action)
}

pub fn label(key: ActionKey<'_>) -> String {
    match key {
        ActionKey::Fixed(Action::Eval) => "eval",
        ActionKey::Fixed(Action::Read) => "read",
        ActionKey::Fixed(Action::Write) => "write",
        ActionKey::Fixed(Action::Edit) => "edit",
        ActionKey::Fixed(Action::Search) => "search",
        ActionKey::Fixed(Action::Fetch) => "fetch",
        ActionKey::Fixed(Action::Mcp) => "mcp",
        ActionKey::Fixed(Action::Inferred) => "inferred",
        ActionKey::Fixed(Action::Fs) => "fs",
        ActionKey::FsDuring(tool) => return format!("fs during {tool}"),
    }
    .into()
}

pub fn is_script(e: &FileEvent) -> bool {
    e.rel.extension().is_some_and(|ext| SCRIPT_EXTS.iter().any(|script| ext.eq_ignore_ascii_case(script)))
}

impl ActionFilter {
    pub fn matches(&self, e: &FileEvent) -> bool {
        let (checked, file_path) = match action_key(e) {
            ActionKey::Fixed(action) => (
                self.actions.contains(&action),
                matches!(action, Action::Read | Action::Write | Action::Edit | Action::Fs | Action::Inferred),
            ),
            ActionKey::FsDuring(tool) => (self.fs_during.contains(tool), true),
        };
        checked && (!self.scripts_only || !file_path || is_script(e))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{EventDetail, Model, Ts};

    fn tool(name: &str) -> TouchSource {
        TouchSource::Tool(name.into())
    }

    fn event(source: TouchSource, rel: &str, scope: Scope, kind: TouchKind) -> FileEvent {
        FileEvent {
            who: Model::YOU,
            rel: rel.into(),
            scope,
            kind,
            source,
            start: Ts::UNIX_EPOCH,
            end: Ts::UNIX_EPOCH,
            tool_call_id: None,
            detail: EventDetail::None,
        }
    }

    #[test]
    fn classifies_actions_by_source_with_scope_and_kind_fallbacks() {
        use ActionKey::{Fixed, FsDuring};
        use Scope::Project;
        use TouchKind::{Read, Write};
        use TouchSource::{Inferred, Watcher, Window};

        for (source, scope, kind, expected) in [
            (tool("eval"), Scope::Session(0), Write, Fixed(Action::Eval)),
            (tool("read"), Scope::WebFetch, Read, Fixed(Action::Fetch)),
            (tool("read"), Project(0), Read, Fixed(Action::Read)),
            (tool("write"), Project(0), Write, Fixed(Action::Write)),
            (tool("edit"), Project(0), Write, Fixed(Action::Edit)),
            (tool("web_search"), Scope::WebSearch, Read, Fixed(Action::Search)),
            (tool("mcp"), Scope::Mcp, Read, Fixed(Action::Mcp)),
            (tool("unknown"), Project(0), Read, Fixed(Action::Read)),
            (tool("unknown"), Project(0), Write, Fixed(Action::Write)),
            (Window("bash".into()), Project(0), Write, FsDuring("bash")),
            (Inferred, Scope::Remote, Read, Fixed(Action::Inferred)),
            (Watcher, Project(0), Write, Fixed(Action::Fs)),
        ] {
            assert_eq!(action_key(&event(source, "target", scope, kind)), expected);
        }
    }

    #[test]
    fn scripts_only_narrows_selected_file_actions_but_keeps_evals() {
        let filter =
            ActionFilter { actions: [Action::Eval, Action::Write].into(), scripts_only: true, ..Default::default() };
        for (name, path, scope, kind, expected) in [
            ("write", "a.py", Scope::Project(0), TouchKind::Write, true),
            ("write", "A.PY", Scope::Project(0), TouchKind::Write, true),
            ("eval", "cell.txt", Scope::Session(0), TouchKind::Write, true),
            ("write", "a.rs", Scope::Project(0), TouchKind::Write, false),
            ("edit", "a.py", Scope::Project(0), TouchKind::Write, false),
            ("web_search", "a.py", Scope::WebSearch, TouchKind::Read, false),
        ] {
            assert_eq!(filter.matches(&event(tool(name), path, scope, kind)), expected, "{name}: {path}");
        }
    }

    #[test]
    fn attributed_changes_need_the_selected_tool_name() {
        let filter = ActionFilter { fs_during: ["bash".to_owned()].into(), ..Default::default() };
        for (source, expected) in [
            (TouchSource::Window("bash".into()), true),
            (TouchSource::Window("ast_edit".into()), false),
            (TouchSource::Watcher, false),
        ] {
            assert_eq!(filter.matches(&event(source, "a.py", Scope::Project(0), TouchKind::Write)), expected);
        }
    }
}
