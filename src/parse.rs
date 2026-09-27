use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::model::{
    EventDetail, FileEvent, Model, ParticipantId, ParticipantKind, Pending, Prompt, Scope, ToolWindow, Ts, TouchKind,
    TouchSource,
};

// ---------------------------------------------------------------------------
// path normalization
// ---------------------------------------------------------------------------

const SELECTOR_CHARS: &str = "0123456789,-+";

fn strip_one_selector(p: &str) -> &str {
    let Some(idx) = p.rfind(':') else { return p };
    let suffix = &p[idx + 1..];
    let is_keyword = matches!(suffix, "raw" | "img" | "conflicts");
    let is_range = !suffix.is_empty()
        && suffix.chars().all(|c| SELECTOR_CHARS.contains(c))
        && suffix
            .chars()
            .next()
            .map(|c| c.is_ascii_digit() || c == '-')
            .unwrap_or(false);
    if is_keyword || is_range { &p[..idx] } else { p }
}

fn strip_selector(p: &str) -> &str {
    strip_one_selector(strip_one_selector(p))
}

fn lexical_clean(p: &Path) -> PathBuf {
    use std::path::Component;
    let mut out: Vec<Component> = Vec::new();
    for comp in p.components() {
        match comp {
            Component::CurDir => {}
            Component::ParentDir => {
                let can_pop = matches!(
                    out.last(),
                    Some(Component::Normal(_))
                );
                if can_pop {
                    out.pop();
                } else {
                    out.push(comp);
                }
            }
            other => out.push(other),
        }
    }
    out.iter().collect()
}

/// Turn a raw tool-arg path string into an absolute, lexically-cleaned path: strip any
/// selector/query suffix, expand a leading `~`, and join a relative path against `file_cwd`.
/// None for a URL/tool-device target (anything containing `://`).
fn absolutize(raw: &str, file_cwd: &Path) -> Option<PathBuf> {
    if raw.contains("://") {
        return None;
    }
    let without_query = raw.split('?').next().unwrap_or(raw);
    let stripped = strip_selector(without_query);
    let expanded: PathBuf = if let Some(rest) = stripped.strip_prefix("~/") {
        let home = std::env::var("HOME").ok()?;
        Path::new(&home).join(rest)
    } else if stripped == "~" {
        PathBuf::from(std::env::var("HOME").ok()?)
    } else {
        PathBuf::from(stripped)
    };
    let joined = if expanded.is_absolute() {
        expanded
    } else {
        file_cwd.join(expanded)
    };
    Some(lexical_clean(&joined))
}

/// Resolve a tool-arg path string to a project-root-relative path, or None if it
/// falls outside the project (or is a URL / tool device target).
pub fn normalize(raw: &str, file_cwd: &Path, root: &Path, aliases: &[PathBuf]) -> Option<PathBuf> {
    let cleaned = absolutize(raw, file_cwd)?;
    if let Ok(rel) = cleaned.strip_prefix(root) {
        return Some(rel.to_path_buf());
    }
    for alias in aliases {
        if let Ok(rel) = cleaned.strip_prefix(alias) {
            return Some(rel.to_path_buf());
        }
    }
    None
}

/// Absolute roots under which a file is considered "temporary": `/tmp`, `/private/tmp`, the
/// process's own temp dir, and (on macOS, where `TMPDIR` typically resolves under `/var/folders`
/// which is itself a symlink to `/private/var/folders`) that dir's `/private`-prefixed twin.
fn temp_roots() -> &'static [PathBuf] {
    static ROOTS: std::sync::LazyLock<Vec<PathBuf>> = std::sync::LazyLock::new(|| {
        let mut roots = vec![PathBuf::from("/tmp"), PathBuf::from("/private/tmp")];
        let env_tmp = lexical_clean(&std::env::temp_dir());
        if let Some(s) = env_tmp.to_str()
            && let Some(rest) = s.strip_prefix("/var/")
        {
            roots.push(PathBuf::from(format!("/private/var/{rest}")));
        }
        roots.push(env_tmp);
        roots
    });
    &ROOTS
}

/// Classify a tool path (argument or result path) for participant `who`: which section it
/// belongs to and the `FileEvent.rel` to store. None for other internal schemes (omp://,
/// xd://, agent://, artifact://, skill://, …), http(s) (handled by the `read` arm), and
/// unresolvable paths.
pub(crate) fn locate(model: &Model, who: ParticipantId, raw: &str, file_cwd: &Path) -> Option<(Scope, PathBuf)> {
    if let Some(rest) = raw.strip_prefix("ssh://") {
        let without_query = rest.split('?').next().unwrap_or(rest);
        let clean = strip_selector(without_query);
        let (host, path) = clean.split_once('/')?;
        if host.is_empty() || path.trim_matches('/').is_empty() {
            return None;
        }
        return Some((Scope::Remote, PathBuf::from(format!("ssh://{host}/{path}"))));
    }
    if let Some(rest) = raw.strip_prefix("local://") {
        let s = model.participants[who.0].session?;
        let without_query = rest.split('?').next().unwrap_or(rest);
        let name = strip_selector(without_query);
        if name.is_empty() {
            return None;
        }
        return Some((Scope::Session(s), model.sessions[s].file.with_extension("").join("local").join(name)));
    }
    if let Some(rel) = normalize(raw, file_cwd, &model.root, &model.root_aliases) {
        return Some((Scope::Project, rel));
    }
    let abs = absolutize(raw, file_cwd)?;
    if let Some((s, _)) = model.session_dir_split(&abs) {
        return Some((Scope::Session(s), abs));
    }
    if let Some(s) = model.participants[who.0].session
        && temp_roots().iter().any(|t| abs.starts_with(t))
    {
        return Some((Scope::Session(s), abs));
    }
    Some((Scope::External, abs))
}

// ---------------------------------------------------------------------------
// timestamps
// ---------------------------------------------------------------------------

pub(crate) fn parse_ts_iso(s: &str) -> Option<Ts> {
    chrono::DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|dt| dt.with_timezone(&chrono::Utc))
}

fn parse_ts_ms(n: i64) -> Ts {
    chrono::DateTime::<chrono::Utc>::from_timestamp_millis(n).unwrap_or_else(chrono::Utc::now)
}

// ---------------------------------------------------------------------------
// span bookkeeping
// ---------------------------------------------------------------------------

pub(crate) fn push_raw_interval(model: &mut Model, who: ParticipantId, start: Ts, end: Ts) {
    let (start, end) = if start <= end { (start, end) } else { (end, start) };
    model.raw_intervals.entry(who.0).or_default().push((start, end));
    model.dirty_spans.insert(who.0);
}

fn recompute_spans(model: &mut Model, who: ParticipantId, idle_gap: i64) {
    model.spans.retain(|s| s.who != who);
    let Some(intervals) = model.raw_intervals.get(&who.0) else {
        return;
    };
    let mut sorted = intervals.clone();
    sorted.sort_by_key(|(s, _)| *s);
    let mut merged: Vec<(Ts, Ts)> = Vec::new();
    let gap = chrono::Duration::seconds(idle_gap);
    for (s, e) in sorted {
        if let Some(last) = merged.last_mut()
            && s - last.1 <= gap
        {
            if e > last.1 {
                last.1 = e;
            }
            continue;
        }
        merged.push((s, e));
    }
    for (s, e) in merged {
        model.spans.push(crate::model::Span { who, start: s, end: e });
    }
}

/// Recompute spans for every participant marked dirty since the last flush, then clear the
/// dirty set. Call once per ingested batch (a `Msg::Lines` payload, or the whole initial load)
/// rather than once per interval: `recompute_spans` re-sorts and re-merges a participant's
/// entire history each time, so batching avoids quadratic work across a large historical file.
pub fn flush_dirty_spans(model: &mut Model, idle_gap: i64) {
    let dirty: Vec<usize> = model.dirty_spans.drain().collect();
    for idx in dirty {
        recompute_spans(model, ParticipantId(idx), idle_gap);
    }
}

// ---------------------------------------------------------------------------
// top-level dispatch
// ---------------------------------------------------------------------------

/// Ingest one parsed jsonl line for participant `who`. Tolerant of schema drift:
/// unrecognized types/shapes are no-ops rather than errors.
pub fn ingest(model: &mut Model, who: ParticipantId, v: &Value, idle_gap: i64) {
    let ty = v.get("type").and_then(|t| t.as_str()).unwrap_or("");
    let is_exit = ty == "custom" && v.get("customType").and_then(|c| c.as_str()) == Some("session_exit");
    if let Some(session_idx) = model.participants[who.0].session {
        if let Some(ts) = v.get("timestamp").and_then(|x| x.as_str()).and_then(parse_ts_iso)
            && ts > model.sessions[session_idx].last_seen
        {
            model.sessions[session_idx].last_seen = ts;
        }
        // A session can be resumed after a clean exit (another `session_exit` follows later in
        // the same file); any non-exit activity after one means it's live again.
        if !is_exit {
            model.sessions[session_idx].end = None;
        }
    }
    match ty {
        "title" => ingest_title_slot(model, who, v),
        "session" => ingest_session_header(model, who, v),
        "title_change" => ingest_title_change(model, who, v),
        "custom" => ingest_custom(model, who, v),
        "message" => ingest_message(model, who, v, idle_gap),
        _ => {}
    }
}

/// Physical line 1 of every session file: a padded `{"type":"title",...}` slot that omp
/// rewrites in place. It carries the session's real title from the start, before any
/// `title_change` entry exists.
fn ingest_title_slot(model: &mut Model, who: ParticipantId, v: &Value) {
    if model.participants[who.0].kind != ParticipantKind::Main {
        return;
    }
    let Some(session_idx) = model.participants[who.0].session else {
        return;
    };
    if let Some(title) = v.get("title").and_then(|x| x.as_str())
        && !title.is_empty()
    {
        model.sessions[session_idx].title = title.to_string();
    }
}

fn ingest_session_header(model: &mut Model, who: ParticipantId, v: &Value) {
    if let Some(cwd) = v.get("cwd").and_then(|x| x.as_str()) {
        model.note_root_alias(Path::new(cwd));
    }
    // Subagent/advisor files carry their own `session` header too, sharing the parent's
    // session_idx; only the Main participant's header should set the shared id/cwd/start.
    if model.participants[who.0].kind != ParticipantKind::Main {
        return;
    }
    let Some(session_idx) = model.participants[who.0].session else {
        return;
    };
    if let Some(id) = v.get("id").and_then(|x| x.as_str()) {
        model.sessions[session_idx].id = id.to_string();
    }
    if let Some(cwd) = v.get("cwd").and_then(|x| x.as_str()) {
        model.sessions[session_idx].cwd = PathBuf::from(cwd);
    }
    if let Some(ts) = v.get("timestamp").and_then(|x| x.as_str()).and_then(parse_ts_iso) {
        model.sessions[session_idx].start = ts;
    }
}

fn ingest_title_change(model: &mut Model, who: ParticipantId, v: &Value) {
    if model.participants[who.0].kind != ParticipantKind::Main {
        return;
    }
    let Some(session_idx) = model.participants[who.0].session else {
        return;
    };
    if let Some(title) = v.get("title").and_then(|x| x.as_str())
        && !title.is_empty()
    {
        model.sessions[session_idx].title = title.to_string();
    }
}

fn ingest_custom(model: &mut Model, who: ParticipantId, v: &Value) {
    match v.get("customType").and_then(|x| x.as_str()).unwrap_or("") {
        "tool_execution_start" => {
            let data = v.get("data").cloned().unwrap_or(Value::Null);
            let Some(tool_call_id) = data.get("toolCallId").and_then(|x| x.as_str()) else {
                return;
            };
            let tool = data.get("toolName").and_then(|x| x.as_str()).unwrap_or("").to_string();
            let started_at = data
                .get("startedAt")
                .and_then(|x| x.as_str())
                .and_then(parse_ts_iso)
                .or_else(|| v.get("timestamp").and_then(|x| x.as_str()).and_then(parse_ts_iso))
                .unwrap_or_else(chrono::Utc::now);
            let new_args = data.get("args").cloned().unwrap_or(Value::Null);
            if tool == "bash" || tool == "eval" {
                model.tool_windows.push(ToolWindow {
                    who,
                    tool_call_id: tool_call_id.to_string(),
                    tool: tool.clone(),
                    start: started_at,
                    end: None,
                });
            }
            let key = (who.0, tool_call_id.to_string());
            // The assistant `toolCall` block (processed separately) may have already inserted
            // this pending entry with the real `arguments`; `tool_execution_start`'s own `data`
            // is sometimes missing `args` entirely. Never let a null overwrite real args.
            let args = match model.pending_tools.get(&key) {
                Some(existing) if !existing.args.is_null() && new_args.is_null() => existing.args.clone(),
                _ => new_args,
            };
            model.pending_tools.insert(key, Pending { tool, args, start: started_at });
        }
        "session_exit" => {
            let Some(session_idx) = model.participants[who.0].session else {
                return;
            };
            if let Some(recorded_at) = v
                .get("data")
                .and_then(|d| d.get("recordedAt"))
                .and_then(|x| x.as_str())
                .and_then(parse_ts_iso)
            {
                model.sessions[session_idx].end = Some(recorded_at);
            }
        }
        _ => {}
    }
}

fn ingest_message(model: &mut Model, who: ParticipantId, v: &Value, idle_gap: i64) {
    let Some(msg) = v.get("message") else { return };
    match msg.get("role").and_then(|x| x.as_str()).unwrap_or("") {
        "assistant" => ingest_assistant(model, who, v, msg, idle_gap),
        "toolResult" => ingest_tool_result(model, who, v, msg, idle_gap),
        "user" => ingest_user(model, who, msg),
        _ => {}
    }
}

fn ingest_assistant(model: &mut Model, who: ParticipantId, v: &Value, msg: &Value, _idle_gap: i64) {
    let start_ms = msg.get("timestamp").and_then(|x| x.as_i64());
    let end_ms = msg.get("completedAt").and_then(|x| x.as_i64());
    if let (Some(s), Some(e)) = (start_ms, end_ms) {
        push_raw_interval(model, who, parse_ts_ms(s), parse_ts_ms(e));
    }
    let entry_ts = v
        .get("timestamp")
        .and_then(|x| x.as_str())
        .and_then(parse_ts_iso)
        .or_else(|| start_ms.map(parse_ts_ms))
        .unwrap_or_else(chrono::Utc::now);
    let Some(content) = msg.get("content").and_then(|c| c.as_array()) else {
        return;
    };
    for block in content {
        if block.get("type").and_then(|t| t.as_str()) != Some("toolCall") {
            continue;
        }
        let Some(id) = block.get("id").and_then(|x| x.as_str()) else {
            continue;
        };
        let key = (who.0, id.to_string());
        if model.pending_tools.contains_key(&key) {
            continue;
        }
        let tool = block.get("name").and_then(|x| x.as_str()).unwrap_or("").to_string();
        let args = block.get("arguments").cloned().unwrap_or(Value::Null);
        model.pending_tools.insert(key, Pending { tool, args, start: entry_ts });
    }
}

fn ingest_user(model: &mut Model, who: ParticipantId, msg: &Value) {
    if model.participants[who.0].kind != ParticipantKind::Main {
        return;
    }
    if msg.get("attribution").and_then(|x| x.as_str()) != Some("user") {
        return;
    }
    let Some(session_idx) = model.participants[who.0].session else {
        return;
    };
    let at = msg
        .get("timestamp")
        .and_then(|x| x.as_i64())
        .map(parse_ts_ms)
        .unwrap_or_else(chrono::Utc::now);
    model.prompts.push(Prompt { at, session: session_idx });
}

/// Paths a `read` result reports having actually read, in preference order:
/// `details.meta.source.value` when `details.meta.source.type == "path"`, else
/// `details.resolvedPath`, else every non-null string in `details.displayReadTargetLinks`
/// (multi-target reads). These are absolute, so they don't depend on the session header's
/// `cwd`, which omp rewrites for the whole file when a session is `/move`d. Empty when the
/// result carries none of them.
fn read_result_paths(details: &Value) -> Vec<String> {
    if let Some("path") = details
        .get("meta")
        .and_then(|m| m.get("source"))
        .and_then(|s| s.get("type"))
        .and_then(|t| t.as_str())
        && let Some(value) = details
            .get("meta")
            .and_then(|m| m.get("source"))
            .and_then(|s| s.get("value"))
            .and_then(|v| v.as_str())
    {
        return vec![value.to_string()];
    }
    if let Some(resolved) = details.get("resolvedPath").and_then(|x| x.as_str()) {
        return vec![resolved.to_string()];
    }
    if let Some(links) = details.get("displayReadTargetLinks").and_then(|x| x.as_array()) {
        let paths: Vec<String> = links.iter().filter_map(|x| x.as_str().map(|s| s.to_string())).collect();
        if !paths.is_empty() {
            return paths;
        }
    }
    Vec::new()
}

fn ingest_tool_result(model: &mut Model, who: ParticipantId, v: &Value, msg: &Value, _idle_gap: i64) {
    let Some(tool_call_id) = msg.get("toolCallId").and_then(|x| x.as_str()).map(|s| s.to_string()) else {
        return;
    };
    let key = (who.0, tool_call_id.clone());
    let Some(pending) = model.pending_tools.remove(&key) else {
        return;
    };
    let end_ts = msg
        .get("timestamp")
        .and_then(|x| x.as_i64())
        .map(parse_ts_ms)
        .or_else(|| v.get("timestamp").and_then(|x| x.as_str()).and_then(parse_ts_iso))
        .unwrap_or_else(chrono::Utc::now);
    push_raw_interval(model, who, pending.start, end_ts);

    if let Some(w) = model
        .tool_windows
        .iter_mut()
        .rev()
        .find(|w| w.who == who && w.tool_call_id == tool_call_id && w.end.is_none())
    {
        w.end = Some(end_ts);
    }

    if msg.get("isError").and_then(|x| x.as_bool()).unwrap_or(false) {
        return;
    }

    let session_idx = model.participants[who.0].session;
    let file_cwd = session_idx
        .map(|i| model.sessions[i].cwd.clone())
        .unwrap_or_else(|| model.root.clone());
    let details = msg.get("details").cloned().unwrap_or(Value::Null);

    match pending.tool.as_str() {
        "read" => {
            if details.get("isDirectory").and_then(|x| x.as_bool()).unwrap_or(false) {
                return;
            }
            let path_arg = pending.args.get("path").and_then(|x| x.as_str());
            if path_arg.is_some_and(|p| p.starts_with("http://") || p.starts_with("https://")) {
                let url = details.get("url").and_then(|x| x.as_str()).or(path_arg).unwrap_or_default();
                model.events.push(FileEvent {
                    who,
                    rel: PathBuf::from(url),
                    scope: Scope::WebFetch,
                    kind: TouchKind::Read,
                    source: TouchSource::Tool("read".to_string()),
                    start: pending.start,
                    end: end_ts,
                    tool_call_id: Some(tool_call_id.clone()),
                    detail: EventDetail::None,
                });
                return;
            }
            let mut paths = read_result_paths(&details);
            if paths.is_empty() {
                let Some(path_arg) = path_arg else {
                    return;
                };
                paths.push(path_arg.to_string());
            }
            for p in &paths {
                let Some((scope, rel)) = locate(model, who, p, &file_cwd) else {
                    continue;
                };
                model.events.push(FileEvent {
                    who,
                    rel,
                    scope,
                    kind: TouchKind::Read,
                    source: TouchSource::Tool("read".to_string()),
                    start: pending.start,
                    end: end_ts,
                    tool_call_id: Some(tool_call_id.clone()),
                    detail: EventDetail::None,
                });
            }
        }
        "write" => {
            let path_str = details
                .get("resolvedPath")
                .and_then(|x| x.as_str())
                .or_else(|| pending.args.get("path").and_then(|x| x.as_str()));
            let Some(path_str) = path_str else { return };
            let Some((scope, rel)) = locate(model, who, path_str, &file_cwd) else {
                return;
            };
            let content = pending
                .args
                .get("content")
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .to_string();
            model.record_snapshot(&rel, end_ts, content.clone());
            model.events.push(FileEvent {
                who,
                rel,
                scope,
                kind: TouchKind::Write,
                source: TouchSource::Tool("write".to_string()),
                start: pending.start,
                end: end_ts,
                tool_call_id: Some(tool_call_id),
                detail: EventDetail::Written { content },
            });
        }
        "web_search" => {
            let Some(query) = pending.args.get("query").and_then(|x| x.as_str()) else {
                return;
            };
            let query = query.replace(['\n', '\r'], " ");
            let sources: Vec<(String, String)> = details
                .get("response")
                .and_then(|r| r.get("sources"))
                .and_then(|s| s.as_array())
                .into_iter()
                .flatten()
                .filter_map(|s| {
                    let url = s.get("url").and_then(|u| u.as_str())?;
                    let title = s.get("title").and_then(|t| t.as_str()).unwrap_or("");
                    Some((title.to_string(), url.to_string()))
                })
                .collect();
            model.events.push(FileEvent {
                who,
                rel: PathBuf::from(query),
                scope: Scope::WebSearch,
                kind: TouchKind::Read,
                source: TouchSource::Tool("web_search".to_string()),
                start: pending.start,
                end: end_ts,
                tool_call_id: Some(tool_call_id),
                detail: EventDetail::Search { sources },
            });
        }
        "edit" => {
            ingest_edit_result(model, who, &pending, &tool_call_id, end_ts, &details, &file_cwd);
        }
        _ => {}
    }
}

struct EditBlock {
    path: String,
    body: String,
    mv: Option<String>,
    rem: bool,
}

/// A line `[PATH#XXXX]` (4 hex chars) opens a block; `MV <dest>` / `REM` within it are ops.
fn header_path(line: &str) -> Option<String> {
    let line = line.trim_end();
    if !line.starts_with('[') || !line.ends_with(']') {
        return None;
    }
    let inner = &line[1..line.len() - 1];
    let hash = inner.rfind('#')?;
    let tag = &inner[hash + 1..];
    if tag.len() == 4 && tag.chars().all(|c| c.is_ascii_hexdigit()) {
        Some(inner[..hash].to_string())
    } else {
        None
    }
}

fn parse_edit_input_headers(input: &str) -> Vec<EditBlock> {
    let mut blocks = Vec::new();
    let mut current: Option<EditBlock> = None;
    for line in input.lines() {
        if let Some(path) = header_path(line) {
            if let Some(b) = current.take() {
                blocks.push(b);
            }
            current = Some(EditBlock { path, body: String::new(), mv: None, rem: false });
            continue;
        }
        if let Some(b) = current.as_mut() {
            if let Some(dest) = line.strip_prefix("MV ") {
                b.mv = Some(dest.trim().to_string());
            } else if line.trim() == "REM" {
                b.rem = true;
            } else {
                if !b.body.is_empty() {
                    b.body.push('\n');
                }
                b.body.push_str(line);
            }
        }
    }
    if let Some(b) = current.take() {
        blocks.push(b);
    }
    blocks
}

fn ingest_edit_result(
    model: &mut Model,
    who: ParticipantId,
    pending: &Pending,
    tool_call_id: &str,
    end: Ts,
    details: &Value,
    file_cwd: &Path,
) {
    let start = pending.start;
    if let Some(per_file) = details.get("perFileResults").and_then(|x| x.as_array()) {
        for entry in per_file {
            let Some(path_str) = entry.get("path").and_then(|x| x.as_str()) else {
                continue;
            };
            let Some((scope, rel)) = locate(model, who, path_str, file_cwd) else {
                continue;
            };
            if let (Some(old), Some(new)) = (
                entry.get("oldText").and_then(|x| x.as_str()),
                entry.get("newText").and_then(|x| x.as_str()),
            ) {
                model.record_snapshot(&rel, start, old.to_string());
                model.record_snapshot(&rel, end, new.to_string());
            }
            let diff = entry.get("diff").and_then(|x| x.as_str()).unwrap_or("").to_string();
            model.events.push(FileEvent {
                who,
                rel,
                scope,
                kind: TouchKind::Write,
                source: TouchSource::Tool("edit".to_string()),
                start,
                end,
                tool_call_id: Some(tool_call_id.to_string()),
                detail: EventDetail::Diff(diff),
            });
        }
        return;
    }
    if let Some(path_str) = details.get("path").and_then(|x| x.as_str()) {
        if let Some((scope, rel)) = locate(model, who, path_str, file_cwd) {
            if let (Some(old), Some(new)) = (
                details.get("oldText").and_then(|x| x.as_str()),
                details.get("newText").and_then(|x| x.as_str()),
            ) {
                model.record_snapshot(&rel, start, old.to_string());
                model.record_snapshot(&rel, end, new.to_string());
            }
            let diff = details.get("diff").and_then(|x| x.as_str()).unwrap_or("").to_string();
            model.events.push(FileEvent {
                who,
                rel,
                scope,
                kind: TouchKind::Write,
                source: TouchSource::Tool("edit".to_string()),
                start,
                end,
                tool_call_id: Some(tool_call_id.to_string()),
                detail: EventDetail::Diff(diff),
            });
        }
        return;
    }
    let Some(input) = pending.args.get("input").and_then(|x| x.as_str()) else {
        return;
    };
    for block in parse_edit_input_headers(input) {
        let Some((scope, rel)) = locate(model, who, &block.path, file_cwd) else {
            continue;
        };
        let detail = if block.rem {
            EventDetail::Removed
        } else {
            EventDetail::Diff(block.body.clone())
        };
        model.events.push(FileEvent {
            who,
            rel: rel.clone(),
            scope,
            kind: TouchKind::Write,
            source: TouchSource::Tool("edit".to_string()),
            start,
            end,
            tool_call_id: Some(tool_call_id.to_string()),
            detail,
        });
        if let Some(dest) = &block.mv
            && let Some((_, to_rel)) = locate(model, who, dest, file_cwd) {
                model.events.push(FileEvent {
                    who,
                    rel,
                    scope,
                    kind: TouchKind::Write,
                    source: TouchSource::Tool("edit".to_string()),
                    start,
                    end,
                    tool_call_id: Some(tool_call_id.to_string()),
                    detail: EventDetail::Moved { to: to_rel },
                });
            }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::ParticipantKind;
    use std::path::Path;

    fn root() -> PathBuf {
        PathBuf::from("/Users/x/projects/tools/hlit")
    }

    #[test]
    fn normalize_strips_line_selector() {
        let r = root();
        assert_eq!(
            normalize("renderer/effect.js:33-343", &r, &r, &[]),
            Some(PathBuf::from("renderer/effect.js"))
        );
    }

    #[test]
    fn normalize_strips_chained_selector_and_raw() {
        let r = root();
        assert_eq!(normalize("a.md:5-16,960-973:raw", &r, &r, &[]), Some(PathBuf::from("a.md")));
    }

    #[test]
    fn normalize_rejects_urls() {
        let r = root();
        assert_eq!(normalize("local://x.md", &r, &r, &[]), None);
    }

    #[test]
    fn normalize_rejects_outside_root() {
        let r = root();
        assert_eq!(normalize("/tmp/x", &r, &r, &[]), None);
    }

    #[test]
    fn normalize_expands_tilde() {
        // normalize() is purely lexical; the directory need not exist on disk.
        let home = std::env::var("HOME").unwrap();
        let root = PathBuf::from(&home).join("projects/x");
        assert_eq!(normalize("~/projects/x/a.txt", &root, &root, &[]), Some(PathBuf::from("a.txt")));
    }

    #[test]
    fn normalize_cleans_dotdot() {
        let r = root();
        let cwd = r.join("renderer");
        assert_eq!(normalize("../scripts/check.mjs", &cwd, &r, &[]), Some(PathBuf::from("scripts/check.mjs")));
    }

    #[test]
    fn normalize_strips_query_suffix() {
        let r = root();
        let cwd = r.join("docs/demo");
        assert_eq!(
            normalize("hlit_demo.gif?q=Does this GIF show the current UI?", &cwd, &r, &[]),
            Some(PathBuf::from("docs/demo/hlit_demo.gif"))
        );
    }

    #[test]
    fn normalize_resolves_via_root_alias() {
        // e.g. root canonicalized to /private/tmp/x, but the session's own (uncanonicalized)
        // cwd and every tool arg still say /tmp/x.
        let root = PathBuf::from("/private/tmp/x");
        let alias = PathBuf::from("/tmp/x");
        assert_eq!(normalize("a.txt", &alias, &root, std::slice::from_ref(&alias)), Some(PathBuf::from("a.txt")));
    }

    fn model_with_main(root: PathBuf) -> (Model, ParticipantId) {
        let mut model = Model::new(root.clone());
        let file = root.join("session.jsonl");
        let who = model.get_or_create_participant(&file, ParticipantKind::Main, "main".into(), None, None);
        let idx = model.sessions.len();
        model.sessions.push(crate::model::Session {
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

    #[test]
    fn edit_details_path_produces_one_event() {
        let (mut model, who) = model_with_main(root());
        let start_v: Value = serde_json::json!({
            "type":"custom","customType":"tool_execution_start",
            "data":{"toolCallId":"t1","toolName":"edit","startedAt":"2026-01-01T00:00:00.000Z","args":{"input":"[a.js#0000]\nPUT 1.=1:\n+x"}}
        });
        ingest(&mut model, who, &start_v, 30);
        let result_v: Value = serde_json::json!({
            "type":"message",
            "message":{
                "role":"toolResult","toolCallId":"t1","toolName":"edit",
                "details":{"path":"/Users/x/projects/tools/hlit/a.js","diff":" 1|x\n","op":"update"},
                "isError":false,"timestamp":1735689600000i64
            }
        });
        ingest(&mut model, who, &result_v, 30);
        assert_eq!(model.events.len(), 1);
        assert_eq!(model.events[0].rel, Path::new("a.js"));
        assert!(matches!(model.events[0].detail, EventDetail::Diff(_)));
    }

    #[test]
    fn write_events_record_content_snapshots() {
        let (mut model, who) = model_with_main(root());
        let start1: Value = serde_json::json!({
            "type":"custom","customType":"tool_execution_start",
            "data":{"toolCallId":"w1","toolName":"write","startedAt":"2026-01-01T00:00:00.000Z","args":{"path":"a.js","content":"a\n"}}
        });
        ingest(&mut model, who, &start1, 30);
        let result1: Value = serde_json::json!({
            "type":"message",
            "message":{
                "role":"toolResult","toolCallId":"w1","toolName":"write",
                "details":{},"isError":false,"timestamp":1767225601000i64
            }
        });
        ingest(&mut model, who, &result1, 30);

        let start2: Value = serde_json::json!({
            "type":"custom","customType":"tool_execution_start",
            "data":{"toolCallId":"w2","toolName":"write","startedAt":"2026-01-01T00:00:02.000Z","args":{"path":"a.js","content":"b\n"}}
        });
        ingest(&mut model, who, &start2, 30);
        let result2: Value = serde_json::json!({
            "type":"message",
            "message":{
                "role":"toolResult","toolCallId":"w2","toolName":"write",
                "details":{},"isError":false,"timestamp":1767225603000i64
            }
        });
        ingest(&mut model, who, &result2, 30);

        assert_eq!(model.events.len(), 2);
        let rel = Path::new("a.js");
        let second_start = model.events[1].start;
        assert_eq!(model.snapshot_before(rel, second_start), Some("a\n"));
        let d = crate::snapshot::unified("a\n", "b\n");
        assert!(d.contains("-a"));
        assert!(d.contains("+b"));
    }

    #[test]
    fn edit_old_new_text_records_both_snapshots() {
        let (mut model, who) = model_with_main(root());
        let start_v: Value = serde_json::json!({
            "type":"custom","customType":"tool_execution_start",
            "data":{"toolCallId":"t5","toolName":"edit","startedAt":"2026-01-01T00:00:00.000Z","args":{"input":"[a.js#0000]\nPUT 1.=1:\n+x"}}
        });
        ingest(&mut model, who, &start_v, 30);
        let result_v: Value = serde_json::json!({
            "type":"message",
            "message":{
                "role":"toolResult","toolCallId":"t5","toolName":"edit",
                "details":{
                    "path":"/Users/x/projects/tools/hlit/a.js",
                    "diff":" 1|x\n",
                    "oldText":"old\n",
                    "newText":"new\n"
                },
                "isError":false,"timestamp":1767225601000i64
            }
        });
        ingest(&mut model, who, &result_v, 30);
        assert_eq!(model.events.len(), 1);
        let rel = Path::new("a.js");
        let e = &model.events[0];
        let snaps = model.snapshots.get(rel).unwrap();
        assert_eq!(snaps.iter().find(|(t, _)| *t == e.start).map(|(_, c)| c.as_str()), Some("old\n"));
        assert_eq!(snaps.iter().find(|(t, _)| *t == e.end).map(|(_, c)| c.as_str()), Some("new\n"));
    }

    #[test]
    fn edit_per_file_results_produces_multiple_events() {
        let (mut model, who) = model_with_main(root());
        let start_v: Value = serde_json::json!({
            "type":"custom","customType":"tool_execution_start",
            "data":{"toolCallId":"t2","toolName":"edit","startedAt":"2026-01-01T00:00:00.000Z","args":{"input":"…"}}
        });
        ingest(&mut model, who, &start_v, 30);
        let result_v: Value = serde_json::json!({
            "type":"message",
            "message":{
                "role":"toolResult","toolCallId":"t2","toolName":"edit",
                "details":{"perFileResults":[
                    {"path":"/Users/x/projects/tools/hlit/a.js","diff":"+a"},
                    {"path":"/Users/x/projects/tools/hlit/b.js","diff":"+b"}
                ]},
                "isError":false,"timestamp":1735689600000i64
            }
        });
        ingest(&mut model, who, &result_v, 30);
        assert_eq!(model.events.len(), 2);
    }

    #[test]
    fn edit_input_header_fallback_with_mv() {
        let (mut model, who) = model_with_main(root());
        let input = "[old.js#ABCD]\nMV new.js\n";
        let start_v: Value = serde_json::json!({
            "type":"custom","customType":"tool_execution_start",
            "data":{"toolCallId":"t3","toolName":"edit","startedAt":"2026-01-01T00:00:00.000Z","args":{"input": input}}
        });
        ingest(&mut model, who, &start_v, 30);
        let result_v: Value = serde_json::json!({
            "type":"message",
            "message":{
                "role":"toolResult","toolCallId":"t3","toolName":"edit",
                "details":{},
                "isError":false,"timestamp":1735689600000i64
            }
        });
        ingest(&mut model, who, &result_v, 30);
        assert_eq!(model.events.len(), 2);
        assert_eq!(model.events[0].rel, Path::new("old.js"));
        assert!(matches!(model.events[1].detail, EventDetail::Moved { .. }));
    }

    #[test]
    fn error_edit_produces_no_event() {
        let (mut model, who) = model_with_main(root());
        let start_v: Value = serde_json::json!({
            "type":"custom","customType":"tool_execution_start",
            "data":{"toolCallId":"t4","toolName":"edit","startedAt":"2026-01-01T00:00:00.000Z","args":{"input":"x"}}
        });
        ingest(&mut model, who, &start_v, 30);
        let result_v: Value = serde_json::json!({
            "type":"message",
            "message":{
                "role":"toolResult","toolCallId":"t4","toolName":"edit",
                "details":{},
                "isError":true,"timestamp":1735689600000i64
            }
        });
        ingest(&mut model, who, &result_v, 30);
        assert_eq!(model.events.len(), 0);
    }

    #[test]
    fn agent_attribution_prompt_is_ignored() {
        let (mut model, who) = model_with_main(root());
        let v: Value = serde_json::json!({
            "type":"message",
            "message":{"role":"user","attribution":"agent","timestamp":1735689600000i64}
        });
        ingest(&mut model, who, &v, 30);
        assert_eq!(model.prompts.len(), 0);
    }

    #[test]
    fn user_attribution_prompt_is_recorded() {
        let (mut model, who) = model_with_main(root());
        let v: Value = serde_json::json!({
            "type":"message",
            "message":{"role":"user","attribution":"user","timestamp":1735689600000i64}
        });
        ingest(&mut model, who, &v, 30);
        assert_eq!(model.prompts.len(), 1);
}

    #[test]
    fn title_slot_sets_initial_session_title() {
        let (mut model, who) = model_with_main(root());
        let v: Value = serde_json::json!({"type":"title","v":1,"title":"Set up repo, review files"});
        ingest(&mut model, who, &v, 30);
        assert_eq!(model.sessions[0].title, "Set up repo, review files");
    }

    #[test]
    fn subagent_session_header_does_not_overwrite_main_start_or_id() {
        let (mut model, main_who) = model_with_main(root());
        model.sessions[0].id = "main-id".to_string();
        let main_start = model.sessions[0].start;

        let sub_file = root().join("session/Sub.jsonl");
        let sub_who = model.get_or_create_participant(
            &sub_file,
            ParticipantKind::Subagent,
            "Sub".into(),
            Some(0),
            Some(main_who),
        );
        let header: Value = serde_json::json!({
            "type":"session","version":3,"id":"sub-id",
            "timestamp":"2099-01-01T00:00:00.000Z","cwd": root().to_string_lossy()
        });
        ingest(&mut model, sub_who, &header, 30);

        assert_eq!(model.sessions[0].id, "main-id");
        assert_eq!(model.sessions[0].start, main_start);
    }

    #[test]
    fn tool_execution_start_preserves_earlier_toolcall_args() {
        let (mut model, who) = model_with_main(root());
        let assistant: Value = serde_json::json!({
            "type":"message","timestamp":"2026-01-01T00:00:00.000Z",
            "message":{
                "role":"assistant","timestamp":1735689600000i64,
                "content":[{"type":"toolCall","id":"t1","name":"edit","arguments":{"input":"[a.js#0000]\nPUT 1.=1:\n+x"}}]
            }
        });
        ingest(&mut model, who, &assistant, 30);
        // tool_execution_start arrives with no `args` at all, as observed in real logs.
        let start_v: Value = serde_json::json!({
            "type":"custom","customType":"tool_execution_start",
            "data":{"toolCallId":"t1","toolName":"edit","startedAt":"2026-01-01T00:00:00.500Z"}
        });
        ingest(&mut model, who, &start_v, 30);
        let pending = model.pending_tools.get(&(who.0, "t1".to_string())).expect("pending entry survives");
        assert_eq!(pending.args.get("input").and_then(|v| v.as_str()), Some("[a.js#0000]\nPUT 1.=1:\n+x"));
    }

    #[test]
    fn session_resumes_after_exit_when_activity_continues() {
        let (mut model, who) = model_with_main(root());
        let exit_v: Value = serde_json::json!({
            "type":"custom","customType":"session_exit",
            "timestamp":"2026-01-01T00:00:00.000Z",
            "data":{"reason":"dispose","recordedAt":"2026-01-01T00:00:00.000Z"}
        });
        ingest(&mut model, who, &exit_v, 30);
        assert!(model.sessions[0].end.is_some());

        let resumed: Value = serde_json::json!({
            "type":"message","timestamp":"2026-01-01T00:19:00.000Z",
            "message":{"role":"user","attribution":"user","timestamp":1735690740000i64}
        });
        ingest(&mut model, who, &resumed, 30);
        assert!(model.sessions[0].end.is_none(), "activity after an exit means the session resumed");
    }
    #[test]
    fn spans_are_deferred_until_flush() {
        let (mut model, who) = model_with_main(root());
        let result_v: Value = serde_json::json!({
            "type":"message",
            "message":{
                "role":"toolResult","toolCallId":"t1","toolName":"read",
                "details":{},"isError":false,"timestamp":1735689605000i64
            }
        });
        let start_v: Value = serde_json::json!({
            "type":"custom","customType":"tool_execution_start",
            "data":{"toolCallId":"t1","toolName":"read","startedAt":"2026-01-01T00:00:00.000Z","args":{"path":"a.txt"}}
        });
        ingest(&mut model, who, &start_v, 30);
        ingest(&mut model, who, &result_v, 30);
        assert!(model.spans.is_empty(), "spans should not be recomputed until flush_dirty_spans runs");
        assert!(model.dirty_spans.contains(&who.0));

        flush_dirty_spans(&mut model, 30);
        assert_eq!(model.spans.len(), 1);
        assert!(model.dirty_spans.is_empty());
    }

    #[test]
    fn read_prefers_result_source_path_over_relative_argument() {
        let (mut model, who) = model_with_main(root());
        let start_v: Value = serde_json::json!({
            "type":"custom","customType":"tool_execution_start",
            "data":{"toolCallId":"t1","toolName":"read","startedAt":"2026-01-01T00:00:00.000Z","args":{"path":"sub/a.js"}}
        });
        ingest(&mut model, who, &start_v, 30);
        let result_v: Value = serde_json::json!({
            "type":"message",
            "message":{
                "role":"toolResult","toolCallId":"t1","toolName":"read",
                "details":{"meta":{"source":{"type":"path","value":"/Users/x/projects/tools/hlit/a.js"}}},
                "isError":false,"timestamp":1735689605000i64
            }
        });
        ingest(&mut model, who, &result_v, 30);
        assert_eq!(model.events.len(), 1);
        assert_eq!(model.events[0].rel, Path::new("a.js"));
    }

    #[test]
    fn multi_target_read_uses_display_read_target_links() {
        let (mut model, who) = model_with_main(root());
        let start_v: Value = serde_json::json!({
            "type":"custom","customType":"tool_execution_start",
            "data":{"toolCallId":"t1","toolName":"read","startedAt":"2026-01-01T00:00:00.000Z","args":{"path":"a.js:295:raw,300-360,420-520"}}
        });
        ingest(&mut model, who, &start_v, 30);
        let result_v: Value = serde_json::json!({
            "type":"message",
            "message":{
                "role":"toolResult","toolCallId":"t1","toolName":"read",
                "details":{
                    "displayReadTargets":["a.js:295:raw","300-360","420-520"],
                    "displayReadTargetLinks":["/Users/x/projects/tools/hlit/a.js",null,null]
                },
                "isError":false,"timestamp":1735689605000i64
            }
        });
        ingest(&mut model, who, &result_v, 30);
        assert_eq!(model.events.len(), 1);
        assert_eq!(model.events[0].rel, Path::new("a.js"));
    }

    #[test]
    fn read_without_result_paths_falls_back_to_argument() {
        let (mut model, who) = model_with_main(root());
        let start_v: Value = serde_json::json!({
            "type":"custom","customType":"tool_execution_start",
            "data":{"toolCallId":"t1","toolName":"read","startedAt":"2026-01-01T00:00:00.000Z","args":{"path":"a.txt"}}
        });
        ingest(&mut model, who, &start_v, 30);
        let result_v: Value = serde_json::json!({
            "type":"message",
            "message":{
                "role":"toolResult","toolCallId":"t1","toolName":"read",
                "details":{},"isError":false,"timestamp":1735689605000i64
            }
        });
        ingest(&mut model, who, &result_v, 30);
        assert_eq!(model.events.len(), 1);
        assert_eq!(model.events[0].rel, Path::new("a.txt"));
    }

    #[test]
    fn ssh_read_produces_remote_scope() {
        let (mut model, who) = model_with_main(root());
        let start_v: Value = serde_json::json!({
            "type":"custom","customType":"tool_execution_start",
            "data":{"toolCallId":"t1","toolName":"read","startedAt":"2026-01-01T00:00:00.000Z","args":{"path":"ssh://h/etc/x.conf:5-9"}}
        });
        ingest(&mut model, who, &start_v, 30);
        let result_v: Value = serde_json::json!({
            "type":"message",
            "message":{
                "role":"toolResult","toolCallId":"t1","toolName":"read",
                "details":{},"isError":false,"timestamp":1735689605000i64
            }
        });
        ingest(&mut model, who, &result_v, 30);
        assert_eq!(model.events.len(), 1);
        assert_eq!(model.events[0].scope, Scope::Remote);
        assert_eq!(model.events[0].rel, Path::new("ssh://h/etc/x.conf"));
    }

    #[test]
    fn write_under_sessions_root_matches_session_by_stem_regardless_of_project_dir() {
        let (mut model, who) = model_with_main(root());
        // model_with_main's session file stem is "session"; the resolved path here sits under a
        // *different* project dir ("-other-proj") than the session's current `cwd`, simulating a
        // session that has been `/move`d since this write happened.
        model.session_roots = vec![PathBuf::from("/sr")];
        let start_v: Value = serde_json::json!({
            "type":"custom","customType":"tool_execution_start",
            "data":{"toolCallId":"w1","toolName":"write","startedAt":"2026-01-01T00:00:00.000Z","args":{"path":"local://plan.md","content":"x"}}
        });
        ingest(&mut model, who, &start_v, 30);
        let result_v: Value = serde_json::json!({
            "type":"message",
            "message":{
                "role":"toolResult","toolCallId":"w1","toolName":"write",
                "details":{"resolvedPath":"/sr/-other-proj/session/local/plan.md"},
                "isError":false,"timestamp":1735689605000i64
            }
        });
        ingest(&mut model, who, &result_v, 30);
        assert_eq!(model.events.len(), 1);
        assert_eq!(model.events[0].scope, Scope::Session(0));
        assert_eq!(model.events[0].rel, Path::new("/sr/-other-proj/session/local/plan.md"));
    }

    #[test]
    fn write_under_tmp_is_session_scope_write_elsewhere_absolute_is_external() {
        let (mut model, who) = model_with_main(root());
        let start1: Value = serde_json::json!({
            "type":"custom","customType":"tool_execution_start",
            "data":{"toolCallId":"w1","toolName":"write","startedAt":"2026-01-01T00:00:00.000Z","args":{"path":"/tmp/probe.py","content":"x"}}
        });
        ingest(&mut model, who, &start1, 30);
        let result1: Value = serde_json::json!({
            "type":"message",
            "message":{
                "role":"toolResult","toolCallId":"w1","toolName":"write",
                "details":{},"isError":false,"timestamp":1735689605000i64
            }
        });
        ingest(&mut model, who, &result1, 30);

        let start2: Value = serde_json::json!({
            "type":"custom","customType":"tool_execution_start",
            "data":{"toolCallId":"w2","toolName":"write","startedAt":"2026-01-01T00:00:02.000Z","args":{"path":"/opt/elsewhere/a.txt","content":"x"}}
        });
        ingest(&mut model, who, &start2, 30);
        let result2: Value = serde_json::json!({
            "type":"message",
            "message":{
                "role":"toolResult","toolCallId":"w2","toolName":"write",
                "details":{},"isError":false,"timestamp":1735689607000i64
            }
        });
        ingest(&mut model, who, &result2, 30);

        assert_eq!(model.events.len(), 2);
        assert_eq!(model.events[0].scope, Scope::Session(0));
        assert_eq!(model.events[0].rel, Path::new("/tmp/probe.py"));
        assert_eq!(model.events[1].scope, Scope::External);
        assert_eq!(model.events[1].rel, Path::new("/opt/elsewhere/a.txt"));
    }

    #[test]
    fn read_of_https_url_produces_web_fetch_scope() {
        let (mut model, who) = model_with_main(root());
        let start_v: Value = serde_json::json!({
            "type":"custom","customType":"tool_execution_start",
            "data":{"toolCallId":"t1","toolName":"read","startedAt":"2026-01-01T00:00:00.000Z","args":{"path":"https://example.com/a?b=1"}}
        });
        ingest(&mut model, who, &start_v, 30);
        let result_v: Value = serde_json::json!({
            "type":"message",
            "message":{
                "role":"toolResult","toolCallId":"t1","toolName":"read",
                "details":{"url":"https://example.com/a?b=1","meta":{"source":{"type":"url"}}},
                "isError":false,"timestamp":1735689605000i64
            }
        });
        ingest(&mut model, who, &result_v, 30);
        assert_eq!(model.events.len(), 1);
        assert_eq!(model.events[0].scope, Scope::WebFetch);
        assert_eq!(model.events[0].rel, Path::new("https://example.com/a?b=1"));
    }

    #[test]
    fn web_search_produces_web_search_scope_with_sources() {
        let (mut model, who) = model_with_main(root());
        let start_v: Value = serde_json::json!({
            "type":"custom","customType":"tool_execution_start",
            "data":{"toolCallId":"t1","toolName":"web_search","startedAt":"2026-01-01T00:00:00.000Z","args":{"query":"rust lazylock"}}
        });
        ingest(&mut model, who, &start_v, 30);
        let result_v: Value = serde_json::json!({
            "type":"message",
            "message":{
                "role":"toolResult","toolCallId":"t1","toolName":"web_search",
                "details":{"response":{"sources":[
                    {"title":"A","url":"http://a.example"},
                    {"title":"B","url":"http://b.example"}
                ]}},
                "isError":false,"timestamp":1735689605000i64
            }
        });
        ingest(&mut model, who, &result_v, 30);
        assert_eq!(model.events.len(), 1);
        assert_eq!(model.events[0].scope, Scope::WebSearch);
        assert_eq!(model.events[0].rel, Path::new("rust lazylock"));
        match &model.events[0].detail {
            EventDetail::Search { sources } => assert_eq!(sources.len(), 2),
            other => panic!("expected Search, got {other:?}"),
        }
    }
}
