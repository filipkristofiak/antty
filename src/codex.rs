//! Codex CLI's on-disk session format: `<sessions root>/YYYY/MM/DD/rollout-<ts>-<thread id>.jsonl`,
//! first line `session_meta`; titles in `<sessions root>/../session_index.jsonl`.

use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex, PoisonError};
use std::time::SystemTime;

use serde_json::Value;

use crate::model::{
    EventDetail, FileEvent, Model, ParticipantId, ParticipantKind, Prompt, Scope, Session, TouchKind, TouchSource, Ts,
};
use crate::parse::{locate, mcp_label, parse_ts_iso, parse_ts_ms, push_mcp_event, push_raw_interval};
use crate::sessions::scan_jsonl_files;

const UNTITLED: &str = "(untitled)";
#[derive(Clone, Copy, PartialEq, Eq)]
enum HistoryMode {
    Legacy,
    Paginated,
}

#[derive(Clone)]
struct RolloutMeta {
    id: String,
    /// Launch cwd, canonicalized when it still exists (raw otherwise).
    cwd: PathBuf,
    parent: Option<String>,
    label: Option<String>,
    history_mode: Option<HistoryMode>,
}

static METAS: LazyLock<Mutex<HashMap<PathBuf, Option<RolloutMeta>>>> = LazyLock::new(|| Mutex::new(HashMap::new()));

type TitleIndex = (PathBuf, u64, SystemTime, HashMap<String, String>);
static INDEX: LazyLock<Mutex<Option<TitleIndex>>> = LazyLock::new(|| Mutex::new(None));

fn rollout_meta(path: &Path) -> Option<RolloutMeta> {
    let mut cache = METAS.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some(meta) = cache.get(path) {
        return meta.clone();
    }
    let file = File::open(path).ok()?;
    let mut line = String::new();
    BufReader::new(file).read_line(&mut line).ok()?;
    if !line.ends_with('\n') {
        return None;
    }
    let meta = serde_json::from_str::<Value>(&line).ok().and_then(|v| {
        if v.get("type")?.as_str()? != "session_meta" {
            return None;
        }
        let p = v.get("payload")?;
        let id = p.get("id")?.as_str()?.to_owned();
        let cwd = PathBuf::from(p.get("cwd")?.as_str()?);
        let spawn = p.get("source").and_then(|s| s.get("subagent")).and_then(|s| s.get("thread_spawn"));
        let parent = p
            .get("parent_thread_id")
            .and_then(Value::as_str)
            .or_else(|| spawn.and_then(|s| s.get("parent_thread_id")).and_then(Value::as_str))
            .map(str::to_owned);
        let label = spawn
            .and_then(|s| s.get("agent_nickname"))
            .and_then(Value::as_str)
            .or_else(|| spawn.and_then(|s| s.get("agent_role")).and_then(Value::as_str))
            .map(str::to_owned);
        let history_mode = match p.get("history_mode").and_then(Value::as_str) {
            Some("legacy") => Some(HistoryMode::Legacy),
            Some("paginated") => Some(HistoryMode::Paginated),
            _ => None,
        };
        Some(RolloutMeta { id, cwd: cwd.canonicalize().unwrap_or(cwd), parent, label, history_mode })
    });
    cache.insert(path.to_path_buf(), meta.clone());
    meta
}

/// Cached metadata is immutable after the first complete header. Avoid cloning its large
/// path/labels when polling every historical rollout for a project match.
fn rollout_matches(path: &Path, project_root: &Path) -> bool {
    if let Some(meta) = METAS.lock().unwrap_or_else(PoisonError::into_inner).get(path) {
        return meta.as_ref().is_some_and(|m| m.cwd == project_root);
    }
    rollout_meta(path).is_some_and(|m| m.cwd == project_root)
}

/// Codex has no project directories: filter rollouts by exact launch cwd and sort so parents
/// precede their spawned subagents.
pub fn scan_files(root: &Path, project_root: &Path) -> Vec<PathBuf> {
    let mut files: Vec<_> = scan_jsonl_files(root)
        .into_iter()
        .filter(|p| p.file_name().is_some_and(|name| name.to_string_lossy().starts_with("rollout-")))
        .filter(|p| rollout_matches(p, project_root))
        .collect();
    files.sort();
    files
}

/// The sessions root itself is the "project dir"; scanning filters rollouts per project.
pub fn discover_project_dir(root: &Path, project_root: &Path) -> Option<PathBuf> {
    (!scan_files(root, project_root).is_empty()).then(|| root.to_path_buf())
}

fn thread_name(project_dir: &Path, id: &str) -> Option<String> {
    let path = project_dir.parent()?.join("session_index.jsonl");
    let stat = fs::metadata(&path).ok()?;
    let (len, modified) = (stat.len(), stat.modified().ok()?);
    let mut cache = INDEX.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some((cached_path, cached_len, cached_modified, names)) = cache.as_ref()
        && *cached_path == path
        && *cached_len == len
        && *cached_modified == modified
    {
        return names.get(id).cloned();
    }
    let file = File::open(&path).ok()?;
    let mut names = HashMap::new();
    for line in BufReader::new(file).lines().map_while(Result::ok) {
        if let Ok(v) = serde_json::from_str::<Value>(&line)
            && let (Some(id), Some(name)) =
                (v.get("id").and_then(Value::as_str), v.get("thread_name").and_then(Value::as_str))
            && !name.is_empty()
        {
            names.insert(id.to_owned(), name.to_owned());
        }
    }
    let title = names.get(id).cloned();
    *cache = Some((path, len, modified, names));
    title
}

/// Register a rollout as a main session or a subagent of an already registered project rollout.
pub fn ensure_participant(model: &mut Model, file: &Path, project_dir: &Path) -> ParticipantId {
    if let Some(&id) = model.file_participant.get(file) {
        return id;
    }
    let meta = rollout_meta(file);
    if let Some(pid) = meta.as_ref().and_then(|m| m.parent.as_ref())
        && let Some(&parent) = model.file_participant.iter().find_map(|(path, who)| {
            (path.starts_with(project_dir)
                && path
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .is_some_and(|stem| stem.strip_suffix(pid).is_some_and(|prefix| prefix.ends_with('-'))))
            .then_some(who)
        })
    {
        let session = model.participants[parent.0].session;
        return model.get_or_create_participant(
            file,
            ParticipantKind::Subagent,
            meta.as_ref().and_then(|m| m.label.as_deref()).unwrap_or("subagent").to_owned(),
            session,
            Some(parent),
        );
    }
    let session_id =
        meta.map(|m| m.id).unwrap_or_else(|| file.file_stem().and_then(|s| s.to_str()).unwrap_or_default().to_owned());
    let title = thread_name(project_dir, &session_id).unwrap_or_else(|| UNTITLED.to_owned());
    let id = model.get_or_create_participant(file, ParticipantKind::Main, "main".to_owned(), None, None);
    let idx = model.sessions.len();
    model.sessions.push(Session {
        file: file.to_path_buf(),
        id: session_id,
        title,
        cwd: PathBuf::new(),
        start: chrono::Utc::now(),
        end: None,
        last_seen: chrono::DateTime::<chrono::Utc>::MIN_UTC,
        main: id,
    });
    model.participants[id.0].session = Some(idx);
    id
}

const SHELL_TOOLS: &[&str] = &["exec", "exec_command", "write_stdin", "shell", "shell_command"];

/// A declared history mode takes precedence when a rollout contains both record shapes.
/// Older headers without one still accept either shape.
fn history_mode(model: &Model, who: ParticipantId) -> Option<HistoryMode> {
    let file = model.participants[who.0].file.as_ref()?;
    METAS.lock().unwrap_or_else(PoisonError::into_inner).get(file)?.as_ref()?.history_mode
}

fn uses_history(model: &Model, who: ParticipantId, mode: HistoryMode) -> bool {
    history_mode(model, who).is_none_or(|recorded| recorded == mode)
}

/// Ingest one Codex rollout record from `who`'s file. Unknown record types are no-ops.
pub fn ingest(model: &mut Model, who: ParticipantId, v: &Value, project_dir: &Path) {
    let Some(ts) = v.get("timestamp").and_then(Value::as_str).and_then(parse_ts_iso) else { return };
    let session_idx = model.participants[who.0].session;
    if let Some(s) = session_idx {
        let session = &mut model.sessions[s];
        session.last_seen = session.last_seen.max(ts);
        session.start = session.start.min(ts);
    }
    let ty = v.get("type").and_then(Value::as_str).unwrap_or("");
    let payload = &v["payload"];
    let pty = payload.get("type").and_then(Value::as_str).unwrap_or("");
    let started = ty == "event_msg" && matches!(pty, "task_started" | "turn_started");
    let ended = ty == "event_msg" && matches!(pty, "task_complete" | "turn_complete" | "turn_aborted");
    if started {
        model.last_entry_ts.insert(who.0, ts);
    } else if let Some(prev) = model.last_entry_ts.get(&who.0).copied() {
        if prev < ts {
            push_raw_interval(model, who, prev, ts);
        }
        model.last_entry_ts.insert(who.0, ts);
    }
    if ended {
        model.last_entry_ts.remove(&who.0);
        for window in model.tool_windows.iter_mut().filter(|w| w.who == who && w.end.is_none()) {
            window.end = Some(ts);
        }
        if model.participants[who.0].kind == ParticipantKind::Main
            && let Some(s) = session_idx
            && let Some(title) = thread_name(project_dir, &model.sessions[s].id)
        {
            model.sessions[s].title = title;
        }
    }
    match (ty, pty) {
        ("session_meta", _) if model.participants[who.0].kind == ParticipantKind::Main => {
            if let Some(s) = session_idx
                && model.sessions[s].cwd.as_os_str().is_empty()
                && let Some(cwd) = payload.get("cwd").and_then(Value::as_str)
            {
                model.sessions[s].cwd = PathBuf::from(cwd);
                model.note_root_alias(Path::new(cwd));
            }
        }
        ("event_msg", "item_completed") if uses_history(model, who, HistoryMode::Paginated) => {
            ingest_item(model, who, payload, ts);
        }
        ("event_msg", "user_message") if uses_history(model, who, HistoryMode::Legacy) => {
            if let Some(text) = payload.get("message").and_then(Value::as_str) {
                push_prompt(model, who, text, ts);
            }
        }
        ("event_msg", "patch_apply_end")
            if uses_history(model, who, HistoryMode::Legacy)
                && payload.get("success").and_then(Value::as_bool) != Some(false)
                && !matches!(payload.get("status").and_then(Value::as_str), Some("failed" | "declined")) =>
        {
            file_changes(model, who, &payload["changes"], payload.get("call_id").and_then(Value::as_str), ts, ts);
        }
        ("event_msg", "web_search_end") if uses_history(model, who, HistoryMode::Legacy) => {
            web_search(model, who, payload, payload.get("call_id").and_then(Value::as_str), ts, ts);
        }
        ("event_msg", "mcp_tool_call_begin") if uses_history(model, who, HistoryMode::Legacy) => {
            if let Some(id) = payload.get("call_id").and_then(Value::as_str) {
                let invocation = &payload["invocation"];
                let server = invocation.get("server").and_then(Value::as_str).unwrap_or("");
                let tool = invocation.get("tool").and_then(Value::as_str).unwrap_or("");
                let args = &invocation["arguments"];
                let input = if args.is_null() { String::new() } else { args.to_string() };
                model.open_window(who, id, mcp_label(server, tool), ts, input);
            }
        }
        ("event_msg", "mcp_tool_call_end") if uses_history(model, who, HistoryMode::Legacy) => {
            if let Some(id) = payload.get("call_id").and_then(Value::as_str) {
                let start = model
                    .tool_windows
                    .iter()
                    .rev()
                    .find(|w| w.who == who && w.tool_call_id == id)
                    .map(|w| w.start)
                    .unwrap_or(ts);
                model.close_window(who, id, ts);
                let result = &payload["result"];
                if result.get("Err").is_some()
                    || result.get("Ok").and_then(|ok| ok.get("isError")).and_then(Value::as_bool) == Some(true)
                {
                    return;
                }
                let invocation = &payload["invocation"];
                let server = invocation.get("server").and_then(Value::as_str).unwrap_or("");
                let tool = invocation.get("tool").and_then(Value::as_str).unwrap_or("");
                push_mcp_event(model, who, id, server, tool, start, ts);
            }
        }
        ("event_msg", "exec_command_end") if uses_history(model, who, HistoryMode::Legacy) => {
            if let Some(id) = payload.get("call_id").and_then(Value::as_str) {
                let script =
                    argv_script(payload.get("command").and_then(Value::as_array).map(Vec::as_slice).unwrap_or(&[]));
                let start = model
                    .tool_windows
                    .iter()
                    .rev()
                    .find(|w| w.who == who && w.tool_call_id == id)
                    .map(|w| w.start)
                    .unwrap_or(ts);
                crate::remote::push_inferred(model, who, id, &script, start, ts);
                model.record_call(who, id, "bash".into(), script);
            }
        }
        ("response_item", "custom_tool_call" | "function_call")
            if payload.get("name").and_then(Value::as_str).is_some_and(|name| SHELL_TOOLS.contains(&name)) =>
        {
            if let Some(id) = payload.get("call_id").and_then(Value::as_str) {
                model.open_window(who, id, "bash".into(), ts, codex_call_input(payload));
            }
        }
        ("response_item", "local_shell_call") => {
            if let Some(id) = payload.get("call_id").and_then(Value::as_str) {
                model.open_window(who, id, "bash".into(), ts, codex_call_input(payload));
            }
        }
        ("response_item", "custom_tool_call_output" | "function_call_output") => {
            if let Some(id) = payload.get("call_id").and_then(Value::as_str) {
                model.close_window(who, id, ts);
            }
        }
        _ => {}
    }
}

fn codex_call_input(payload: &Value) -> String {
    match payload.get("type").and_then(Value::as_str) {
        Some("custom_tool_call") => payload.get("input").and_then(Value::as_str).unwrap_or("").to_string(),
        Some("function_call") => {
            let raw = payload.get("arguments").and_then(Value::as_str).unwrap_or("");
            let Ok(args) = serde_json::from_str::<Value>(raw) else {
                return raw.to_string();
            };
            if let Some(cmd) = args.get("cmd").and_then(Value::as_str) {
                cmd.to_string()
            } else if let Some(command) = args.get("command").and_then(Value::as_array) {
                command.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(" ")
            } else {
                args.get("command").and_then(Value::as_str).unwrap_or("").to_string()
            }
        }
        Some("local_shell_call") => payload
            .get("action")
            .and_then(|action| action.get("command"))
            .and_then(Value::as_array)
            .map(|command| command.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(" "))
            .unwrap_or_default(),
        _ => String::new(),
    }
}

fn argv_script(argv: &[Value]) -> String {
    if argv.len() >= 3
        && argv[0]
            .as_str()
            .and_then(|program| Path::new(program).file_name())
            .and_then(|name| name.to_str())
            .is_some_and(|name| matches!(name, "sh" | "bash" | "zsh"))
        && matches!(argv[1].as_str(), Some("-c" | "-lc"))
    {
        return argv[2].as_str().unwrap_or("").to_string();
    }
    argv.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(" ")
}

fn push_prompt(model: &mut Model, who: ParticipantId, text: &str, ts: Ts) {
    if model.participants[who.0].kind == ParticipantKind::Main
        && !text.trim().is_empty()
        && let Some(session) = model.participants[who.0].session
    {
        model.prompts.push(Prompt { at: ts, session });
    }
}

fn file_cwd(model: &Model, who: ParticipantId) -> PathBuf {
    model.participants[who.0]
        .session
        .map(|s| model.sessions[s].cwd.clone())
        .filter(|cwd| !cwd.as_os_str().is_empty())
        .unwrap_or_else(|| model.roots[0].path.clone())
}

struct EventCtx<'a> {
    who: ParticipantId,
    id: Option<&'a str>,
    start: Ts,
    end: Ts,
}

impl EventCtx<'_> {
    fn event(&self, rel: PathBuf, scope: Scope, kind: TouchKind, tool: &str, detail: EventDetail) -> FileEvent {
        FileEvent {
            who: self.who,
            rel,
            scope,
            kind,
            source: TouchSource::Tool(tool.to_owned()),
            start: self.start,
            end: self.end,
            tool_call_id: self.id.map(str::to_owned),
            detail,
        }
    }
}

fn ingest_item(model: &mut Model, who: ParticipantId, payload: &Value, ts: Ts) {
    let item = &payload["item"];
    let id = item.get("id").and_then(Value::as_str);
    let start = payload.get("started_at_ms").and_then(Value::as_i64).map(parse_ts_ms).unwrap_or(ts);
    let end = payload.get("completed_at_ms").and_then(Value::as_i64).map(parse_ts_ms).unwrap_or(ts);
    let ctx = EventCtx { who, id, start, end };
    match item.get("type").and_then(Value::as_str).unwrap_or("") {
        "UserMessage" => {
            let text = item
                .get("content")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter(|part| part.get("type").and_then(Value::as_str) == Some("text"))
                .filter_map(|part| part.get("text").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join("\n");
            push_prompt(model, who, &text, ts);
        }
        "CommandExecution" if item.get("source").and_then(Value::as_str) != Some("user_shell") => {
            let status = item.get("status").and_then(Value::as_str);
            if matches!(status, Some("completed" | "failed"))
                && let Some(id) = id
            {
                let script =
                    argv_script(item.get("command").and_then(Value::as_array).map(Vec::as_slice).unwrap_or(&[]));
                crate::remote::push_inferred(model, who, id, &script, start, end);
                model.record_call(who, id, "bash".into(), script);
            }
            if status != Some("completed") {
                return;
            }
            let cwd =
                item.get("cwd").and_then(Value::as_str).and_then(file_url_path).unwrap_or_else(|| file_cwd(model, who));
            for cmd in item.get("parsed_cmd").and_then(Value::as_array).into_iter().flatten() {
                if cmd.get("type").and_then(Value::as_str) != Some("read") {
                    continue;
                }
                if let Some(path) = cmd.get("path").and_then(Value::as_str)
                    && let Some((scope, rel)) = locate(model, who, path, &cwd)
                {
                    model.events.push(ctx.event(rel, scope, TouchKind::Read, "read", EventDetail::None));
                }
            }
        }
        "FileChange" if !matches!(item.get("status").and_then(Value::as_str), Some("failed" | "declined")) => {
            file_changes(model, who, &item["changes"], id, start, end);
        }
        "McpToolCall" if matches!(item.get("status").and_then(Value::as_str), Some("completed" | "failed")) => {
            if let Some(id) = id {
                let server = item.get("server").and_then(Value::as_str).unwrap_or("");
                let tool = item.get("tool").and_then(Value::as_str).unwrap_or("");
                model.open_window(who, id, mcp_label(server, tool), start, item["arguments"].to_string());
                model.close_window(who, id, end);
                if item.get("status").and_then(Value::as_str) == Some("completed") {
                    push_mcp_event(model, who, id, server, tool, start, end);
                }
            }
        }
        "WebSearch" => web_search(model, who, item, id, start, end),
        _ => {}
    }
}

fn file_changes(model: &mut Model, who: ParticipantId, changes: &Value, id: Option<&str>, start: Ts, end: Ts) {
    let Some(changes) = changes.as_object() else { return };
    let cwd = file_cwd(model, who);
    let ctx = EventCtx { who, id, start, end };
    for (path, change) in changes {
        let Some((scope, rel)) = locate(model, who, path, &cwd) else { continue };
        let kind = change.get("type").and_then(Value::as_str).unwrap_or("");
        let (tool, detail) = match kind {
            "add" => {
                let content = change.get("content").and_then(Value::as_str).unwrap_or("").to_owned();
                model.record_snapshot(scope, &rel, end, content.clone());
                ("write", EventDetail::Written { content })
            }
            "delete" => {
                if let Some(content) = change.get("content").and_then(Value::as_str) {
                    model.record_snapshot(scope, &rel, start, content.to_owned());
                }
                ("edit", EventDetail::Removed)
            }
            "update" => {
                ("edit", EventDetail::Diff(change.get("unified_diff").and_then(Value::as_str).unwrap_or("").to_owned()))
            }
            _ => continue,
        };
        model.events.push(ctx.event(rel.clone(), scope, TouchKind::Write, tool, detail));
        if kind == "update"
            && let Some(to) = change.get("move_path").and_then(Value::as_str)
            && let Some((_, to_rel)) = locate(model, who, to, &cwd)
        {
            model.events.push(ctx.event(rel, scope, TouchKind::Write, "edit", EventDetail::Moved { to: to_rel }));
        }
    }
}

fn web_search(model: &mut Model, who: ParticipantId, obj: &Value, id: Option<&str>, start: Ts, end: Ts) {
    let action = &obj["action"];
    if matches!(action.get("type").and_then(Value::as_str), Some("open_page" | "find_in_page"))
        && let Some(url) = action.get("url").and_then(Value::as_str).filter(|url| !url.is_empty())
    {
        let ctx = EventCtx { who, id, start, end };
        model.events.push(ctx.event(PathBuf::from(url), Scope::WebFetch, TouchKind::Read, "read", EventDetail::None));
        return;
    }
    let query = obj
        .get("query")
        .and_then(Value::as_str)
        .filter(|q| !q.is_empty())
        .or_else(|| action.get("query").and_then(Value::as_str).filter(|q| !q.is_empty()))
        .or_else(|| {
            action
                .get("queries")
                .and_then(Value::as_array)
                .and_then(|a| a.first())
                .and_then(Value::as_str)
                .filter(|q| !q.is_empty())
        });
    if let Some(query) = query {
        let ctx = EventCtx { who, id, start, end };
        model.events.push(ctx.event(
            PathBuf::from(query.replace(['\n', '\r'], " ")),
            Scope::WebSearch,
            TouchKind::Read,
            "web_search",
            EventDetail::Search { sources: Vec::new() },
        ));
    }
}

fn file_url_path(s: &str) -> Option<PathBuf> {
    let Some(encoded) = s.strip_prefix("file://") else {
        return Path::new(s).is_absolute().then(|| PathBuf::from(s));
    };
    let bytes = encoded.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let (Some(hi), Some(lo)) = ((bytes[i + 1] as char).to_digit(16), (bytes[i + 2] as char).to_digit(16))
        {
            decoded.push((hi * 16 + lo) as u8);
            i += 3;
        } else {
            decoded.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(decoded).ok().map(PathBuf::from)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse::flush_dirty_spans;
    use serde_json::json;
    use std::io::Write;

    fn t(sec: u32) -> Ts {
        parse_ts_iso(&format!("2026-01-01T00:00:{sec:02}Z")).unwrap()
    }

    fn record(sec: u32, ty: &str, payload: Value) -> Value {
        json!({"timestamp": format!("2026-01-01T00:00:{sec:02}Z"), "type":ty, "payload":payload})
    }

    #[test]
    fn argv_scripts_unwrap_shells_and_join_other_commands() {
        for (argv, expected) in [
            (json!(["/bin/zsh", "-lc", "ssh h ls"]), "ssh h ls"),
            (json!(["bash", "-c", "ssh h uptime", "extra"]), "ssh h uptime"),
            (json!(["sh", "-lc", "ssh h pwd"]), "ssh h pwd"),
            (json!(["ssh", "h", "ls"]), "ssh h ls"),
            (json!(["fish", "-c", "ssh h ls"]), "fish -c ssh h ls"),
            (json!([]), ""),
        ] {
            assert_eq!(argv_script(argv.as_array().unwrap()), expected);
        }
    }

    #[test]
    fn paginated_commands_infer_terminal_agent_scripts_and_read_only_on_success() {
        let mut model = Model::new(vec![PathBuf::from("/Users/x/proj")]);
        let dir = Path::new("/nonexistent/codex/sessions");
        let who = ensure_participant(&mut model, &dir.join("rollout-remote-items.jsonl"), dir);
        for (id, status, source) in [
            ("success", "completed", "unified_exec_startup"),
            ("failure", "failed", "unified_exec_startup"),
            ("running", "inProgress", "unified_exec_startup"),
            ("user", "completed", "user_shell"),
            ("user-failed", "failed", "user_shell"),
        ] {
            ingest(
                &mut model,
                who,
                &record(
                    9,
                    "event_msg",
                    json!({
                        "type":"item_completed","started_at_ms":t(3).timestamp_millis(),
                        "completed_at_ms":t(7).timestamp_millis(),
                        "item":{"type":"CommandExecution","id":id,"status":status,"source":source,
                            "command":["/bin/zsh","-lc","ssh h ls"],"cwd":"/Users/x/proj",
                            "parsed_cmd":[{"type":"read","path":"a.rs"}]}
                    }),
                ),
                dir,
            );
        }
        assert_eq!(model.events.len(), 3);
        for id in ["success", "failure"] {
            let event = model
                .events
                .iter()
                .find(|event| event.scope == Scope::Remote && event.tool_call_id.as_deref() == Some(id))
                .unwrap();
            assert_eq!(event.rel, Path::new("ssh://h"));
            assert_eq!(event.source, TouchSource::Inferred);
            assert_eq!(event.kind, TouchKind::Read);
            assert_eq!((event.start, event.end), (t(3), t(7)));
            assert_eq!(model.call(who, id).unwrap().tool, "bash");
            assert_eq!(model.call(who, id).unwrap().input, "ssh h ls");
        }
        let read = model.events.iter().find(|event| matches!(event.source, TouchSource::Tool(_))).unwrap();
        assert_eq!(read.tool_call_id.as_deref(), Some("success"));
        assert_eq!(read.rel, Path::new("a.rs"));
        for id in ["running", "user", "user-failed"] {
            assert!(model.call(who, id).is_none());
        }
    }

    #[test]
    fn command_history_modes_select_producers_and_legacy_errors_overwrite_wrappers() {
        let base = std::env::temp_dir().join(format!("antty-codex-test-remote-history-{}", std::process::id()));
        fs::create_dir_all(&base).unwrap();
        for mode in ["legacy", "paginated"] {
            let file = base.join(format!("rollout-{mode}.jsonl"));
            fs::write(
                &file,
                record(0, "session_meta", json!({"id":mode,"cwd":base,"history_mode":mode})).to_string() + "\n",
            )
            .unwrap();
            let mut model = Model::new(vec![base.clone()]);
            let who = ensure_participant(&mut model, &file, &base);
            ingest(
                &mut model,
                who,
                &record(
                    3,
                    "response_item",
                    json!({
                        "type":"custom_tool_call","name":"exec","call_id":"legacy","input":"tools.exec_command({cmd: 'ssh h ls'})"
                    }),
                ),
                &base,
            );
            ingest(
                &mut model,
                who,
                &record(
                    7,
                    "event_msg",
                    json!({
                        "type":"exec_command_end","call_id":"legacy","command":["/bin/bash","-lc","ssh h ls"],
                        "exit_code":1,"status":"failed"
                    }),
                ),
                &base,
            );
            ingest(
                &mut model,
                who,
                &record(
                    9,
                    "event_msg",
                    json!({
                        "type":"item_completed","item":{"type":"CommandExecution","id":"paginated",
                            "status":"failed","source":"unified_exec_startup","command":["ssh","h","ls"]}
                    }),
                ),
                &base,
            );
            assert_eq!(model.events.len(), 1);
            let event = &model.events[0];
            assert_eq!(event.tool_call_id.as_deref(), Some(mode));
            assert_eq!(event.rel, Path::new("ssh://h"));
            assert_eq!(event.source, TouchSource::Inferred);
            assert_eq!(model.call(who, mode).unwrap().input, "ssh h ls");
            if mode == "legacy" {
                assert_eq!((event.start, event.end), (t(3), t(7)));
                assert!(model.call(who, "paginated").is_none());
                ingest(
                    &mut model,
                    who,
                    &record(
                        11,
                        "event_msg",
                        json!({
                            "type":"exec_command_end","call_id":"no-window","command":["ssh","other","ls"],"exit_code":1
                        }),
                    ),
                    &base,
                );
                assert_eq!((model.events[1].start, model.events[1].end), (t(11), t(11)));
                assert_eq!(model.call(who, "no-window").unwrap().input, "ssh other ls");
            } else {
                assert_eq!(model.call(who, "legacy").unwrap().input, "tools.exec_command({cmd: 'ssh h ls'})");
            }
        }
        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn ingest_builds_prompts_events_windows_and_spans() {
        let mut model = Model::new(vec![PathBuf::from("/Users/x/proj")]);
        let dir = Path::new("/nonexistent/codex/sessions");
        let who = ensure_participant(&mut model, &dir.join("rollout-p1.jsonl"), dir);
        let mut ingest_at = |sec, ty, payload| ingest(&mut model, who, &record(sec, ty, payload), dir);
        ingest_at(0, "session_meta", json!({"cwd":"/Users/x/proj"}));
        ingest_at(0, "event_msg", json!({"type":"task_started"}));
        ingest_at(
            1,
            "event_msg",
            json!({"type":"item_completed", "item":{"type":"UserMessage",
            "content":[{"type":"text","text":"make files"}]}}),
        );
        ingest_at(2, "response_item", json!({"type":"custom_tool_call","name":"exec","call_id":"c1"}));
        ingest_at(
            3,
            "event_msg",
            json!({"type":"item_completed", "item":{"type":"CommandExecution",
            "id":"read1","status":"completed","source":"unified_exec_startup",
            "cwd":"file:///Users/x/proj/sub%20dir",
            "parsed_cmd":[{"type":"read","cmd":"cat a.rs","name":"a.rs","path":"a.rs"}]}}),
        );
        ingest_at(4, "response_item", json!({"type":"custom_tool_call_output","call_id":"c1"}));
        ingest_at(
            5,
            "event_msg",
            json!({"type":"item_completed", "item":{"type":"FileChange","id":"edit1",
            "status":"completed","changes":{
                "new.rs":{"type":"add","content":"hi\n"},
                "/Users/x/proj/b.rs":{"type":"update","unified_diff":"@@ -1 +1 @@","move_path":"/Users/x/proj/c.rs"},
                "gone.rs":{"type":"delete","content":"old\n"}
            }}}),
        );
        ingest_at(
            6,
            "event_msg",
            json!({"type":"item_completed", "item":{"type":"FileChange",
            "status":"failed","changes":{"failed.rs":{"type":"add","content":"no"}}}}),
        );
        ingest_at(
            7,
            "event_msg",
            json!({"type":"item_completed", "item":{"type":"WebSearch",
            "query":"rust tui","action":{"type":"search"}}}),
        );
        ingest_at(
            8,
            "event_msg",
            json!({"type":"item_completed", "item":{"type":"WebSearch",
            "action":{"type":"open_page","url":"https://example.com"}}}),
        );
        ingest_at(
            9,
            "event_msg",
            json!({"type":"patch_apply_end", "success":true, "call_id":"legacy",
            "changes":{"legacy.rs":{"type":"add","content":"legacy"}}}),
        );
        ingest_at(20, "event_msg", json!({"type":"task_complete"}));
        assert_eq!(model.prompts.len(), 1);
        assert_eq!(model.prompts[0].at, t(1));
        assert_eq!(model.sessions[0].cwd, Path::new("/Users/x/proj"));
        assert_eq!(model.events.len(), 8);
        let read = &model.events[0];
        assert_eq!(read.rel, Path::new("sub dir/a.rs"));
        assert_eq!(read.scope, Scope::Project(0));
        assert_eq!(read.source, TouchSource::Tool("read".into()));
        assert_eq!(read.kind, TouchKind::Read);
        let events = |rel: &str| model.events.iter().filter(|e| e.rel == Path::new(rel)).collect::<Vec<_>>();
        let b = events("b.rs");
        assert_eq!(b.len(), 2);
        assert!(matches!(&b[0].detail, EventDetail::Diff(diff) if diff == "@@ -1 +1 @@"));
        assert!(matches!(&b[1].detail, EventDetail::Moved { to } if to == Path::new("c.rs")));
        assert!(matches!(&events("new.rs")[0].detail, EventDetail::Written { content } if content == "hi\n"));
        assert!(matches!(&events("gone.rs")[0].detail, EventDetail::Removed));
        assert!(matches!(&events("legacy.rs")[0].detail, EventDetail::Written { content } if content == "legacy"));
        assert_eq!(events("rust tui")[0].scope, Scope::WebSearch);
        assert!(matches!(&events("rust tui")[0].detail, EventDetail::Search { sources } if sources.is_empty()));
        assert_eq!(events("https://example.com")[0].scope, Scope::WebFetch);
        assert_eq!(model.snapshot_before(Scope::Project(0), Path::new("new.rs"), t(20)), Some("hi\n"));
        assert_eq!(model.tool_windows.len(), 1);
        assert_eq!(model.call(who, "c1").unwrap().tool, "bash");
        assert_eq!(model.tool_windows[0].start, t(2));
        assert_eq!(model.tool_windows[0].end, Some(t(4)));
        flush_dirty_spans(&mut model, 30);
        assert_eq!(model.spans.iter().map(|s| (s.start, s.end)).collect::<Vec<_>>(), vec![(t(0), t(20))]);
    }

    #[test]
    fn shell_call_inputs_are_captured_by_payload_type() {
        let mut model = Model::new(vec![PathBuf::from("/Users/x/proj")]);
        let dir = Path::new("/nonexistent/codex/sessions");
        let who = ensure_participant(&mut model, &dir.join("rollout-inputs.jsonl"), dir);
        let cases = [
            (
                json!({"type":"custom_tool_call","name":"exec","call_id":"custom",
                "input":"const r = await tools.exec_command({cmd: 'ls'});"}),
                "const r = await tools.exec_command({cmd: 'ls'});",
            ),
            (
                json!({"type":"function_call","name":"exec_command","call_id":"cmd",
                "arguments":"{\"cmd\":\"ls\\npwd\",\"command\":\"ignored\"}"}),
                "ls\npwd",
            ),
            (
                json!({"type":"function_call","name":"shell","call_id":"argv",
                "arguments":"{\"command\":[\"sh\",\"-c\",\"pwd\"]}"}),
                "sh -c pwd",
            ),
            (
                json!({"type":"function_call","name":"shell_command","call_id":"string",
                "arguments":"{\"command\":\"pwd\"}"}),
                "pwd",
            ),
            (
                json!({"type":"function_call","name":"exec_command","call_id":"raw",
                "arguments":"not JSON"}),
                "not JSON",
            ),
            (
                json!({"type":"local_shell_call","call_id":"local",
                "action":{"command":["/bin/sh","-c","ls"]}}),
                "/bin/sh -c ls",
            ),
            (json!({"type":"local_shell_call","call_id":"missing"}), ""),
        ];
        for (payload, expected) in cases {
            let id = payload["call_id"].as_str().unwrap().to_string();
            ingest(&mut model, who, &record(1, "response_item", payload), dir);
            let call = model.call(who, &id).unwrap();
            assert_eq!(call.tool, "bash");
            assert_eq!(call.input, expected);
        }
    }

    #[test]
    fn legacy_mcp_begin_captures_arguments_and_respects_history_mode() {
        let base = std::env::temp_dir().join(format!("antty-codex-test-mcp-{}", std::process::id()));
        fs::create_dir_all(&base).unwrap();
        for mode in ["legacy", "paginated"] {
            let file = base.join(format!("rollout-{mode}.jsonl"));
            fs::write(
                &file,
                record(0, "session_meta", json!({"id":mode,"cwd":base,"history_mode":mode})).to_string() + "\n",
            )
            .unwrap();
            let mut model = Model::new(vec![base.clone()]);
            let who = ensure_participant(&mut model, &file, &base);
            for (id, args) in [("m1", json!({"title":"x"})), ("m2", Value::Null)] {
                let v = record(
                    3,
                    "event_msg",
                    json!({"type":"mcp_tool_call_begin","call_id":id,
                    "invocation":{"server":"github","tool":"create_issue","arguments":args}}),
                );
                ingest(&mut model, who, &v, &base);
                ingest(&mut model, who, &v, &base);
                if mode == "legacy" {
                    let call = model.call(who, id).unwrap();
                    assert_eq!(call.tool, "mcp:github/create_issue");
                    assert_eq!(call.input, if args.is_null() { String::new() } else { args.to_string() });
                } else {
                    assert!(model.call(who, id).is_none());
                }
            }
            assert_eq!(model.tool_windows.len(), if mode == "legacy" { 2 } else { 0 });
            assert!(model.tool_windows.iter().all(|w| w.start == t(3) && w.end.is_none()));
            assert!(model.events.is_empty());
        }
        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn legacy_mcp_results_emit_success_and_close_error_windows() {
        let mut model = Model::new(vec![PathBuf::from("/Users/x/proj")]);
        let dir = Path::new("/nonexistent/codex/sessions");
        let who = ensure_participant(&mut model, &dir.join("rollout-mcp-results.jsonl"), dir);
        for (id, result) in [
            ("success", json!({"Ok":{"content":[]}})),
            ("transport-error", json!({"Err":"x"})),
            ("tool-error", json!({"Ok":{"content":[],"isError":true}})),
        ] {
            let invocation = json!({"server":"github","tool":"create_issue","arguments":{"title":"x"}});
            ingest(
                &mut model,
                who,
                &record(
                    3,
                    "event_msg",
                    json!({
                        "type":"mcp_tool_call_begin","call_id":id,"invocation":invocation
                    }),
                ),
                dir,
            );
            ingest(
                &mut model,
                who,
                &record(
                    7,
                    "event_msg",
                    json!({
                        "type":"mcp_tool_call_end","call_id":id,"invocation":invocation,"result":result
                    }),
                ),
                dir,
            );
        }
        assert_eq!(model.events.len(), 1);
        let event = &model.events[0];
        assert_eq!(event.who, who);
        assert_eq!(event.scope, Scope::Mcp);
        assert_eq!(event.rel, Path::new("github/create_issue"));
        assert_eq!(event.kind, TouchKind::Read);
        assert_eq!(event.source, TouchSource::Tool("mcp".into()));
        assert_eq!(event.tool_call_id.as_deref(), Some("success"));
        assert_eq!((event.start, event.end), (t(3), t(7)));
        assert!(matches!(event.detail, EventDetail::None));
        assert_eq!(model.tool_windows.len(), 3);
        assert!(model.tool_windows.iter().all(|w| w.start == t(3) && w.end == Some(t(7))));
        assert_eq!(model.call(who, "success").unwrap().input, json!({"title":"x"}).to_string());

        ingest(
            &mut model,
            who,
            &record(
                9,
                "event_msg",
                json!({
                    "type":"mcp_tool_call_end","call_id":"no-begin",
                    "invocation":{"server":"github","tool":"list_issues"},"result":{"Ok":{"content":[]}}
                }),
            ),
            dir,
        );
        assert_eq!(model.events.len(), 2);
        assert_eq!((model.events[1].start, model.events[1].end), (t(9), t(9)));
        assert_eq!(model.events[1].rel, Path::new("github/list_issues"));
    }

    #[test]
    fn paginated_mcp_items_capture_terminal_windows_and_skip_in_progress() {
        let mut model = Model::new(vec![PathBuf::from("/Users/x/proj")]);
        let dir = Path::new("/nonexistent/codex/sessions");
        let who = ensure_participant(&mut model, &dir.join("rollout-mcp-items.jsonl"), dir);
        for (id, status, args) in [
            ("success", "completed", json!({"title":"x"})),
            ("failure", "failed", Value::Null),
            ("running", "inProgress", json!({"title":"pending"})),
        ] {
            ingest(
                &mut model,
                who,
                &record(
                    9,
                    "event_msg",
                    json!({
                        "type":"item_completed","started_at_ms":t(3).timestamp_millis(),
                        "completed_at_ms":t(7).timestamp_millis(),
                        "item":{"type":"McpToolCall","id":id,"server":"github","tool":"create_issue",
                            "arguments":args,"status":status}
                    }),
                ),
                dir,
            );
            if status != "inProgress" {
                let call = model.call(who, id).unwrap();
                assert_eq!(call.tool, "mcp:github/create_issue");
                assert_eq!(call.input, args.to_string());
            }
        }
        assert_eq!(model.events.len(), 1);
        let event = &model.events[0];
        assert_eq!(event.who, who);
        assert_eq!(event.scope, Scope::Mcp);
        assert_eq!(event.rel, Path::new("github/create_issue"));
        assert_eq!(event.kind, TouchKind::Read);
        assert_eq!(event.source, TouchSource::Tool("mcp".into()));
        assert_eq!(event.tool_call_id.as_deref(), Some("success"));
        assert_eq!((event.start, event.end), (t(3), t(7)));
        assert!(matches!(event.detail, EventDetail::None));
        assert_eq!(model.tool_windows.len(), 2);
        assert!(model.tool_windows.iter().all(|w| w.start == t(3) && w.end == Some(t(7))));
        assert!(model.call(who, "running").is_none());
    }

    #[test]
    fn declared_history_mode_avoids_duplicate_mcp_calls() {
        let base = std::env::temp_dir().join(format!("antty-codex-test-mcp-history-{}", std::process::id()));
        fs::create_dir_all(&base).unwrap();
        for mode in ["legacy", "paginated"] {
            let file = base.join(format!("rollout-{mode}.jsonl"));
            fs::write(
                &file,
                record(0, "session_meta", json!({"id":mode,"cwd":base,"history_mode":mode})).to_string() + "\n",
            )
            .unwrap();
            let mut model = Model::new(vec![base.clone()]);
            let who = ensure_participant(&mut model, &file, &base);
            for (sec, payload) in [
                (
                    3,
                    json!({"type":"mcp_tool_call_begin","call_id":"legacy",
                    "invocation":{"server":"github","tool":"create_issue","arguments":{"title":"legacy"}}}),
                ),
                (
                    7,
                    json!({"type":"mcp_tool_call_end","call_id":"legacy",
                    "invocation":{"server":"github","tool":"create_issue"},"result":{"Ok":{"content":[]}}}),
                ),
                (
                    9,
                    json!({"type":"item_completed","started_at_ms":t(4).timestamp_millis(),
                    "completed_at_ms":t(8).timestamp_millis(),
                    "item":{"type":"McpToolCall","id":"paginated","server":"github","tool":"create_issue",
                        "arguments":{"title":"paginated"},"status":"completed"}}),
                ),
            ] {
                ingest(&mut model, who, &record(sec, "event_msg", payload), &base);
            }
            assert_eq!(model.events.len(), 1);
            assert_eq!(model.events[0].tool_call_id.as_deref(), Some(mode));
            assert_eq!(model.events[0].scope, Scope::Mcp);
            assert_eq!(model.tool_windows.len(), 1);
            let window = &model.tool_windows[0];
            let expected = if mode == "legacy" { (t(3), Some(t(7))) } else { (t(4), Some(t(8))) };
            assert_eq!((window.start, window.end), expected);
            assert_eq!(model.call(who, mode).unwrap().input, json!({"title":mode}).to_string());
            let ignored = if mode == "legacy" { "paginated" } else { "legacy" };
            assert!(model.call(who, ignored).is_none());
        }
        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn turn_end_closes_open_windows_and_restart_does_not_bridge() {
        let mut model = Model::new(vec![PathBuf::from("/Users/x/proj")]);
        let dir = Path::new("/nonexistent/codex/sessions");
        let who = ensure_participant(&mut model, &dir.join("rollout-p2.jsonl"), dir);
        for (sec, ty, payload) in [
            (0, "event_msg", json!({"type":"task_started"})),
            (1, "response_item", json!({"type":"function_call","name":"exec_command","call_id":"c2"})),
            (5, "event_msg", json!({"type":"turn_aborted"})),
            (10, "event_msg", json!({"type":"task_started"})),
            (12, "event_msg", json!({"type":"token_count"})),
            (50, "event_msg", json!({"type":"task_started"})),
            (55, "event_msg", json!({"type":"task_complete"})),
        ] {
            ingest(&mut model, who, &record(sec, ty, payload), dir);
        }
        assert_eq!(model.tool_windows[0].end, Some(t(5)));
        flush_dirty_spans(&mut model, 1);
        assert_eq!(
            model.spans.iter().map(|s| (s.start, s.end)).collect::<Vec<_>>(),
            vec![(t(0), t(5)), (t(10), t(12)), (t(50), t(55))]
        );
    }

    #[test]
    fn declared_history_mode_avoids_duplicate_prompts_and_file_changes() {
        let base = std::env::temp_dir().join(format!("antty-codex-test-history-{}", std::process::id()));
        let dir = base.join("codex/sessions/2026/01/01");
        let project = base.join("project");
        fs::create_dir_all(&dir).unwrap();
        fs::create_dir_all(&project).unwrap();
        let project = project.canonicalize().unwrap();
        for (mode, id) in [("paginated", "p1"), ("legacy", "l1")] {
            let records = [
                record(0, "session_meta", json!({"id":id,"cwd":project,"history_mode":mode})),
                record(
                    1,
                    "event_msg",
                    json!({"type":"item_completed",
                    "item":{"type":"UserMessage","content":[{"type":"text","text":"create x"}]}}),
                ),
                record(1, "event_msg", json!({"type":"user_message","message":"create x"})),
                record(
                    2,
                    "event_msg",
                    json!({"type":"item_completed",
                    "item":{"type":"FileChange","id":"c1","status":"completed",
                        "changes":{"x.txt":{"type":"add","content":"X"}}}}),
                ),
                record(
                    2,
                    "event_msg",
                    json!({"type":"patch_apply_end","call_id":"c1","success":true,
                    "changes":{"x.txt":{"type":"add","content":"X"}}}),
                ),
            ];
            let file = dir.join(format!("rollout-2026-01-01T00-00-00-{id}.jsonl"));
            fs::write(file, records.iter().map(Value::to_string).collect::<Vec<_>>().join("\n") + "\n").unwrap();
        }
        let mut model = Model::new(vec![project.clone()]);
        crate::sessions::load_initial(
            &mut model,
            crate::sessions::Harness::Codex,
            &base.join("codex/sessions"),
            &project,
            30,
        );
        assert_eq!(model.sessions.len(), 2);
        assert_eq!(model.prompts.len(), 2);
        assert_eq!(model.events.len(), 2);
        assert!(model.events.iter().all(|e| e.rel == Path::new("x.txt")));
        for session in &model.sessions {
            let who = session.main;
            assert_eq!(
                model.prompts.iter().filter(|p| p.session == model.participants[who.0].session.unwrap()).count(),
                1
            );
            assert_eq!(model.events.iter().filter(|e| e.who == who).count(), 1);
        }
        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn discovery_scan_subagents_and_titles() {
        let base = std::env::temp_dir().join(format!("antty-codex-test-discovery-{}", std::process::id()));
        let dir = base.join("codex/sessions/2026/01/01");
        let project = base.join("project");
        fs::create_dir_all(&dir).unwrap();
        fs::create_dir_all(&project).unwrap();
        let project = project.canonicalize().unwrap();
        let sessions = base.join("codex/sessions");
        let file = |stamp: &str, id: &str| dir.join(format!("rollout-2026-01-01T{stamp}-{id}.jsonl"));
        let main = file("00-00-00", "p1");
        let child = file("00-01-00", "k1");
        let decoy = file("00-02-00", "d1");
        let partial = file("00-03-00", "z9");
        let meta = |id: &str, cwd: &Path, source: Value| {
            record(0, "session_meta", json!({"id":id,"cwd":cwd,"source":source})).to_string()
        };
        fs::write(&main, format!("{}\n", meta("p1", &project, json!("cli")))).unwrap();
        fs::write(
            &child,
            format!(
                "{}\n",
                meta(
                    "k1",
                    &project,
                    json!({"subagent":{"thread_spawn":{
            "parent_thread_id":"p1","agent_nickname":"worker","agent_role":"explorer"}}})
                )
            ),
        )
        .unwrap();
        fs::write(&decoy, format!("{}\n", meta("d1", Path::new("/elsewhere"), json!("cli")))).unwrap();
        fs::write(dir.join("notes.jsonl"), format!("{}\n", meta("n1", &project, json!("cli")))).unwrap();
        fs::write(&partial, meta("z9", &project, json!("cli"))).unwrap();
        let index = base.join("codex/session_index.jsonl");
        fs::write(&index, "{\"id\":\"p1\",\"thread_name\":\"First\"}\n{\"id\":\"p1\",\"thread_name\":\"Renamed\"}\n")
            .unwrap();
        assert_eq!(discover_project_dir(&sessions, &project), Some(sessions.clone()));
        assert_eq!(scan_files(&sessions, &project), vec![main.clone(), child.clone()]);
        File::options().append(true).open(&partial).unwrap().write_all(b"\n").unwrap();
        assert_eq!(scan_files(&sessions, &project), vec![main.clone(), child.clone(), partial]);
        let mut model = Model::new(vec![project]);
        let p = ensure_participant(&mut model, &main, &sessions);
        let k = ensure_participant(&mut model, &child, &sessions);
        assert_eq!(model.participants[k.0].kind, ParticipantKind::Subagent);
        assert_eq!(model.participants[k.0].label, "worker");
        assert_eq!(model.participants[k.0].parent, Some(p));
        assert_eq!(model.participants[k.0].session, model.participants[p.0].session);
        assert_eq!(model.sessions[0].title, "Renamed");
        File::options()
            .append(true)
            .open(&index)
            .unwrap()
            .write_all(b"{\"id\":\"p1\",\"thread_name\":\"Final\"}\n")
            .unwrap();
        ingest(&mut model, p, &record(20, "event_msg", json!({"type":"task_complete"})), &sessions);
        assert_eq!(model.sessions[0].title, "Final");
        fs::remove_dir_all(base).unwrap();
    }
}
