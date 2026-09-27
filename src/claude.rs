//! Claude Code's on-disk session format: `<projects root>/<project dir>/<sessionId>.jsonl`,
//! with subagents under `<sessionId>/subagents/agent-<id>.jsonl` (+ `agent-<id>.meta.json`).

use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::model::{
    EventDetail, FileEvent, Model, ParticipantId, ParticipantKind, Pending, Prompt, Scope, Session, ToolWindow, Ts,
    TouchKind, TouchSource,
};
use crate::parse::{locate, parse_ts_iso, push_raw_interval};
use crate::sessions::{is_dotfile, is_jsonl};

/// The launch `cwd` appears within the first few lines; bound the scan so a file without one
/// never gets read in full.
const FIRST_CWD_SCAN_LINES: usize = 50;

const UNTITLED: &str = "(untitled)";

/// The first `cwd` field in `path`, i.e. the directory Claude Code was launched in (later lines
/// drift when the agent `cd`s).
fn read_first_cwd(path: &Path) -> Option<PathBuf> {
    let f = fs::File::open(path).ok()?;
    BufReader::new(f).lines().take(FIRST_CWD_SCAN_LINES).map_while(Result::ok).find_map(|line| {
        let v: Value = serde_json::from_str(&line).ok()?;
        v.get("cwd").and_then(|c| c.as_str()).map(PathBuf::from)
    })
}

/// Top-level non-dotfile `*.jsonl` files directly inside `dir`.
fn top_level_jsonl(dir: &Path) -> Vec<PathBuf> {
    let Ok(rd) = fs::read_dir(dir) else { return Vec::new() };
    rd.flatten()
        .map(|e| e.path())
        .filter(|p| p.is_file() && is_jsonl(p) && !is_dotfile(p))
        .collect()
}

/// Find the direct child of `root` whose most-recently-modified session file was launched in
/// `project_root`. Matches on the logged `cwd`; Claude Code's dir-name encoding is never
/// recomputed.
pub fn discover_project_dir(root: &Path, project_root: &Path) -> Option<PathBuf> {
    let rd = fs::read_dir(root).ok()?;
    for entry in rd.flatten() {
        let dir = entry.path();
        if !dir.is_dir() {
            continue;
        }
        let mut files: Vec<(std::time::SystemTime, PathBuf)> = top_level_jsonl(&dir)
            .into_iter()
            .filter_map(|p| Some((fs::metadata(&p).ok()?.modified().ok()?, p)))
            .collect();
        files.sort_by(|a, b| b.0.cmp(&a.0));
        let Some(cwd) = files.iter().find_map(|(_, p)| read_first_cwd(p)) else { continue };
        if cwd.canonicalize().unwrap_or(cwd) == project_root {
            return Some(dir);
        }
    }
    None
}

/// Every transcript for a project dir: top-level session files plus each session's
/// `<sessionId>/subagents/*.jsonl`. `tool-results/`, `memory/` etc. are not transcripts.
pub fn scan_files(project_dir: &Path) -> Vec<PathBuf> {
    let mut out = top_level_jsonl(project_dir);
    let Ok(rd) = fs::read_dir(project_dir) else { return out };
    for entry in rd.flatten() {
        let d = entry.path();
        if is_dotfile(&d) || !d.is_dir() {
            continue;
        }
        out.extend(top_level_jsonl(&d.join("subagents")));
    }
    out
}

/// Subagent label from its sibling `agent-<id>.meta.json`: `description`, else `agentType`,
/// else the file stem.
fn subagent_label(file: &Path) -> String {
    let meta: Option<Value> = fs::read_to_string(file.with_extension("meta.json"))
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok());
    let field = |k: &str| {
        meta.as_ref()
            .and_then(|m| m.get(k))
            .and_then(|x| x.as_str())
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    };
    field("description")
        .or_else(|| field("agentType"))
        .unwrap_or_else(|| file.file_stem().and_then(|s| s.to_str()).unwrap_or_default().to_string())
}

/// Ensure `file`'s participant (and, for a subagent, its session's main) is registered in
/// `model`. Idempotent.
pub fn ensure_participant(model: &mut Model, file: &Path, project_dir: &Path) -> ParticipantId {
    if let Some(&id) = model.file_participant.get(file) {
        return id;
    }
    let parent = file.parent();
    let session_dir = parent.filter(|p| p.file_name().is_some_and(|n| n == "subagents")).and_then(Path::parent);
    if let Some(session_dir) = session_dir
        && session_dir.parent() == Some(project_dir)
        && let Some(session_id) = session_dir.file_name().and_then(|n| n.to_str())
    {
        let main_file = project_dir.join(format!("{session_id}.jsonl"));
        let main = ensure_participant(model, &main_file, project_dir);
        let session = model.participants[main.0].session;
        return model.get_or_create_participant(file, ParticipantKind::Subagent, subagent_label(file), session, Some(main));
    }
    let id = model.get_or_create_participant(file, ParticipantKind::Main, "main".to_string(), None, None);
    let idx = model.sessions.len();
    model.sessions.push(Session {
        file: file.to_path_buf(),
        id: file.file_stem().and_then(|s| s.to_str()).unwrap_or_default().to_string(),
        title: UNTITLED.to_string(),
        cwd: PathBuf::new(),
        start: chrono::Utc::now(),
        end: None,
        last_seen: chrono::DateTime::<chrono::Utc>::MIN_UTC,
        main: id,
    });
    model.participants[id.0].session = Some(idx);
    id
}

/// Ingest one Claude Code jsonl record from `who`'s file. Unknown record types are no-ops.
pub fn ingest(model: &mut Model, who: ParticipantId, v: &Value) {
    let ts = v.get("timestamp").and_then(|x| x.as_str()).and_then(parse_ts_iso);
    let is_main = model.participants[who.0].kind == ParticipantKind::Main;
    let session_idx = model.participants[who.0].session;
    if let Some(s) = session_idx {
        let session = &mut model.sessions[s];
        if let Some(ts) = ts {
            session.last_seen = session.last_seen.max(ts);
            session.start = session.start.min(ts);
        }
        if is_main
            && session.cwd.as_os_str().is_empty()
            && let Some(cwd) = v.get("cwd").and_then(|x| x.as_str())
        {
            session.cwd = PathBuf::from(cwd);
            model.note_root_alias(Path::new(cwd));
        }
    }
    let title = |k: &str| v.get(k).and_then(|x| x.as_str()).filter(|s| !s.is_empty());
    match (v.get("type").and_then(|t| t.as_str()).unwrap_or(""), session_idx) {
        ("custom-title", Some(s)) if is_main => {
            if let Some(t) = title("customTitle") {
                model.sessions[s].title = t.to_string();
            }
        }
        ("ai-title", Some(s)) if is_main => {
            // A user's custom title wins regardless of order: the AI title only fills a blank.
            if let Some(t) = title("aiTitle")
                && model.sessions[s].title == UNTITLED
            {
                model.sessions[s].title = t.to_string();
            }
        }
        ("assistant", _) => {
            if let Some(ts) = ts {
                ingest_assistant(model, who, v, ts);
            }
        }
        ("user", _) => {
            if let Some(ts) = ts {
                ingest_user(model, who, v, ts);
            }
        }
        _ => {}
    }
}

fn content_blocks(v: &Value) -> Option<&Vec<Value>> {
    v.get("message").and_then(|m| m.get("content")).and_then(|c| c.as_array())
}

/// Assistant records carry no completion time, so the time since the previous user/assistant
/// entry counts as generation activity.
fn ingest_assistant(model: &mut Model, who: ParticipantId, v: &Value, ts: Ts) {
    let prev = model.last_entry_ts.get(&who.0).copied();
    if let Some(prev) = prev
        && prev < ts
    {
        push_raw_interval(model, who, prev, ts);
    }
    model.last_entry_ts.insert(who.0, prev.map_or(ts, |p| p.max(ts)));
    let Some(blocks) = content_blocks(v) else { return };
    for block in blocks {
        if block.get("type").and_then(|t| t.as_str()) != Some("tool_use") {
            continue;
        }
        let Some(id) = block.get("id").and_then(|x| x.as_str()) else { continue };
        let key = (who.0, id.to_string());
        if model.pending_tools.contains_key(&key) {
            continue;
        }
        let tool = block.get("name").and_then(|x| x.as_str()).unwrap_or("").to_string();
        if tool == "Bash" {
            model.tool_windows.push(ToolWindow {
                who,
                tool_call_id: id.to_string(),
                tool: "bash".into(),
                start: ts,
                end: None,
            });
        }
        let args = block.get("input").cloned().unwrap_or(Value::Null);
        model.pending_tools.insert(key, Pending { tool, args, start: ts });
    }
}

fn ingest_user(model: &mut Model, who: ParticipantId, v: &Value, ts: Ts) {
    let last = model.last_entry_ts.entry(who.0).or_insert(ts);
    *last = (*last).max(ts);

    let content = v.get("message").and_then(|m| m.get("content"));
    if let Some(blocks) = content.and_then(|c| c.as_array()) {
        let results: Vec<&Value> =
            blocks.iter().filter(|b| b.get("type").and_then(|t| t.as_str()) == Some("tool_result")).collect();
        if !results.is_empty() {
            // `toolUseResult` is record-level, so it only belongs to a lone result block.
            let tur = if results.len() == 1 { v.get("toolUseResult") } else { None };
            for block in results {
                ingest_tool_result(model, who, block, tur, ts);
            }
            return;
        }
    }

    if model.participants[who.0].kind != ParticipantKind::Main {
        return;
    }
    let Some(session) = model.participants[who.0].session else { return };
    if v.get("isMeta").and_then(|x| x.as_bool()) == Some(true) {
        return;
    }
    if v.get("origin")
        .and_then(|o| o.get("kind"))
        .and_then(|k| k.as_str())
        .is_some_and(|k| k != "human")
    {
        return;
    }
    let text = match content {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(blocks)) => blocks
            .iter()
            .filter(|b| b.get("type").and_then(|t| t.as_str()) == Some("text"))
            .filter_map(|b| b.get("text").and_then(|t| t.as_str()))
            .collect::<Vec<_>>()
            .join("\n"),
        _ => return,
    };
    let text = text.trim();
    // Slash-command, bash-mode and notification wrappers (`<command-name>`, `<bash-input>`, …).
    if text.is_empty() || text.starts_with('<') {
        return;
    }
    model.prompts.push(Prompt { at: ts, session });
}

/// Hunks of a `structuredPatch` rendered as unified-diff text; empty if absent.
fn patch_text(sp: Option<&Value>) -> String {
    let mut out = String::new();
    for hunk in sp.and_then(|x| x.as_array()).into_iter().flatten() {
        let n = |k: &str| hunk.get(k).and_then(|x| x.as_i64()).unwrap_or(0);
        out.push_str(&format!(
            "@@ -{},{} +{},{} @@\n",
            n("oldStart"),
            n("oldLines"),
            n("newStart"),
            n("newLines")
        ));
        for line in hunk.get("lines").and_then(|x| x.as_array()).into_iter().flatten() {
            if let Some(l) = line.as_str() {
                out.push_str(l);
                out.push('\n');
            }
        }
    }
    out
}

/// Replay `(old, new, replace_all)` edits over `original`, as Claude Code's Edit/MultiEdit do.
fn apply_edits(original: &str, edits: &[(String, String, bool)]) -> String {
    let mut s = original.to_string();
    for (old, new, replace_all) in edits {
        if old.is_empty() {
            // Edit with an empty `old_string` creates an empty file's content.
            if s.is_empty() {
                s = new.clone();
            }
        } else if *replace_all {
            s = s.replace(old.as_str(), new);
        } else {
            s = s.replacen(old.as_str(), new, 1);
        }
    }
    s
}

fn edit_triple(e: &Value) -> (String, String, bool) {
    let s = |k: &str| e.get(k).and_then(|x| x.as_str()).unwrap_or("").to_string();
    (s("old_string"), s("new_string"), e.get("replace_all").and_then(|x| x.as_bool()).unwrap_or(false))
}

fn ingest_tool_result(model: &mut Model, who: ParticipantId, block: &Value, tur: Option<&Value>, end: Ts) {
    let Some(id) = block.get("tool_use_id").and_then(|x| x.as_str()) else { return };
    let Some(pending) = model.pending_tools.remove(&(who.0, id.to_string())) else { return };
    push_raw_interval(model, who, pending.start, end);
    if let Some(w) = model
        .tool_windows
        .iter_mut()
        .rev()
        .find(|w| w.who == who && w.tool_call_id == id && w.end.is_none())
    {
        w.end = Some(end);
    }
    if block.get("is_error").and_then(|x| x.as_bool()) == Some(true) {
        return;
    }

    let file_cwd = model.participants[who.0]
        .session
        .map(|s| model.sessions[s].cwd.clone())
        .filter(|c| !c.as_os_str().is_empty())
        .unwrap_or_else(|| model.root.clone());
    let input = &pending.args;
    let in_str = |k: &str| input.get(k).and_then(|x| x.as_str());
    let tur_str = |k: &str| tur.and_then(|t| t.get(k)).and_then(|x| x.as_str());
    let start = pending.start;
    let event = |rel: PathBuf, scope: Scope, kind: TouchKind, tool: &str, detail: EventDetail| FileEvent {
        who,
        rel,
        scope,
        kind,
        source: TouchSource::Tool(tool.to_string()),
        start,
        end,
        tool_call_id: Some(id.to_string()),
        detail,
    };

    match pending.tool.as_str() {
        "Read" => {
            let Some(path) = in_str("file_path") else { return };
            let Some((scope, rel)) = locate(model, who, path, &file_cwd) else { return };
            model.events.push(event(rel, scope, TouchKind::Read, "read", EventDetail::None));
        }
        "Write" => {
            let Some(path) = tur_str("filePath").or_else(|| in_str("file_path")) else { return };
            let Some((scope, rel)) = locate(model, who, path, &file_cwd) else { return };
            let content = in_str("content").or_else(|| tur_str("content")).unwrap_or("").to_string();
            let detail = if let Some(original) = tur_str("originalFile") {
                model.record_snapshot(&rel, start, original.to_string());
                model.record_snapshot(&rel, end, content.clone());
                let diff = patch_text(tur.and_then(|t| t.get("structuredPatch")));
                EventDetail::Diff(if diff.is_empty() { crate::snapshot::unified(original, &content) } else { diff })
            } else {
                model.record_snapshot(&rel, end, content.clone());
                EventDetail::Written { content }
            };
            model.events.push(event(rel, scope, TouchKind::Write, "write", detail));
        }
        "Edit" | "MultiEdit" => {
            let Some(path) = tur_str("filePath").or_else(|| in_str("file_path")) else { return };
            let Some((scope, rel)) = locate(model, who, path, &file_cwd) else { return };
            let edits: Vec<(String, String, bool)> = if pending.tool == "Edit" {
                vec![edit_triple(input)]
            } else {
                input.get("edits").and_then(|x| x.as_array()).into_iter().flatten().map(edit_triple).collect()
            };
            let snapshots = tur_str("originalFile").map(|original| (original, apply_edits(original, &edits)));
            if let Some((original, new)) = &snapshots {
                model.record_snapshot(&rel, start, original.to_string());
                model.record_snapshot(&rel, end, new.clone());
            }
            let mut diff = patch_text(tur.and_then(|t| t.get("structuredPatch")));
            if diff.is_empty()
                && let Some((original, new)) = &snapshots
            {
                diff = crate::snapshot::unified(original, new);
            }
            model.events.push(event(rel, scope, TouchKind::Write, "edit", EventDetail::Diff(diff)));
        }
        "NotebookEdit" => {
            let Some(path) = in_str("notebook_path") else { return };
            let Some((scope, rel)) = locate(model, who, path, &file_cwd) else { return };
            model.events.push(event(rel, scope, TouchKind::Write, "edit", EventDetail::None));
        }
        "WebFetch" => {
            let Some(url) = in_str("url") else { return };
            model.events.push(event(PathBuf::from(url), Scope::WebFetch, TouchKind::Read, "read", EventDetail::None));
        }
        "WebSearch" => {
            let Some(query) = in_str("query") else { return };
            let query = query.replace(['\n', '\r'], " ");
            let sources: Vec<(String, String)> = tur
                .and_then(|t| t.get("results"))
                .and_then(|r| r.as_array())
                .into_iter()
                .flatten()
                .filter_map(|r| r.get("content").and_then(|c| c.as_array()))
                .flatten()
                .filter_map(|s| {
                    let url = s.get("url").and_then(|u| u.as_str())?;
                    let title = s.get("title").and_then(|t| t.as_str()).unwrap_or("");
                    Some((title.to_string(), url.to_string()))
                })
                .collect();
            model.events.push(event(
                PathBuf::from(query),
                Scope::WebSearch,
                TouchKind::Read,
                "web_search",
                EventDetail::Search { sources },
            ));
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn t(s: &str) -> Ts {
        parse_ts_iso(&format!("2026-01-01T00:00:{s}Z")).unwrap()
    }

    #[test]
    fn ingest_builds_title_prompts_events_windows_and_spans() {
        let root = PathBuf::from("/Users/x/proj");
        let root_dir = PathBuf::from("/cc");
        let mut model = Model::new(root.clone());
        let who = ensure_participant(&mut model, &root_dir.join("-proj/s1.jsonl"), &root_dir.join("-proj"));
        let a_rs = root.join("a.rs").to_string_lossy().into_owned();
        let missing = root.join("missing.rs").to_string_lossy().into_owned();
        let ts = |s: &str| format!("2026-01-01T00:00:{s}Z");
        let tool_use = |at: &str, id: &str, name: &str, input: Value| {
            json!({"type":"assistant","timestamp":ts(at),"message":{"role":"assistant",
                "content":[{"type":"tool_use","id":id,"name":name,"input":input}]}})
        };
        let tool_result = |at: &str, id: &str, is_error: bool, tur: Value| {
            json!({"type":"user","timestamp":ts(at),"message":{"role":"user",
                "content":[{"type":"tool_result","tool_use_id":id,"is_error":is_error,"content":"x"}]},
                "toolUseResult":tur})
        };
        let lines = [
            json!({"type":"user","timestamp":ts("00.000"),"cwd":root,"origin":{"kind":"human"},
                "message":{"role":"user","content":"fix it"}}),
            json!({"type":"user","timestamp":ts("00.500"),"isMeta":true,
                "message":{"role":"user","content":"<local-command-caveat>x</local-command-caveat>"}}),
            json!({"type":"ai-title","aiTitle":"AI"}),
            tool_use("05.000", "e1", "Edit", json!({"file_path":a_rs,"old_string":"old","new_string":"new","replace_all":false})),
            tool_result("06.000", "e1", false, json!({"filePath":a_rs,"originalFile":"old\n",
                "structuredPatch":[{"oldStart":1,"oldLines":1,"newStart":1,"newLines":1,"lines":["-old","+new"]}]})),
            tool_use("07.000", "b1", "Bash", json!({"command":"ls"})),
            tool_result("09.000", "b1", false, json!({"stdout":""})),
            tool_use("10.000", "r1", "Read", json!({"file_path":missing})),
            tool_result("11.000", "r1", true, json!("Error: no such file")),
            json!({"type":"custom-title","customTitle":"mine"}),
            json!({"type":"ai-title","aiTitle":"AI 2"}),
        ];
        for v in &lines {
            ingest(&mut model, who, v);
        }
        crate::parse::flush_dirty_spans(&mut model, 30);

        let s = &model.sessions[0];
        assert_eq!(s.title, "mine");
        assert_eq!(s.start, t("00.000"));
        assert_eq!(s.cwd, root);
        assert_eq!(model.prompts.len(), 1);
        assert_eq!(model.prompts[0].at, t("00.000"));

        assert_eq!(model.events.len(), 1);
        let e = &model.events[0];
        assert_eq!(e.rel, PathBuf::from("a.rs"));
        assert_eq!(e.scope, Scope::Project);
        assert_eq!(e.source, TouchSource::Tool("edit".into()));
        match &e.detail {
            EventDetail::Diff(d) => assert_eq!(d, "@@ -1,1 +1,1 @@\n-old\n+new\n"),
            other => panic!("expected a diff, got {other:?}"),
        }
        assert_eq!(
            model.snapshots[Path::new("a.rs")],
            vec![(t("05.000"), "old\n".to_string()), (t("06.000"), "new\n".to_string())]
        );

        assert_eq!(model.tool_windows.len(), 1);
        let w = &model.tool_windows[0];
        assert_eq!((w.tool.as_str(), w.start, w.end), ("bash", t("07.000"), Some(t("09.000"))));

        let spans: Vec<(Ts, Ts)> = model.spans.iter().map(|s| (s.start, s.end)).collect();
        assert_eq!(spans, vec![(t("00.500"), t("11.000"))]);
    }

    #[test]
    fn discovery_scan_and_subagent_labels() {
        let base = std::env::temp_dir().join(format!("antty-claude-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        let project = base.join("proj");
        fs::create_dir_all(&project).unwrap();
        let project = project.canonicalize().unwrap();
        let root = base.join("cc");
        let dir_a = root.join("-a");
        let subagents = dir_a.join("s1/subagents");
        fs::create_dir_all(&subagents).unwrap();
        fs::create_dir_all(dir_a.join("s1/tool-results")).unwrap();
        fs::create_dir_all(root.join("-decoy")).unwrap();

        let user = |cwd: &Path| json!({"type":"user","timestamp":"2026-01-01T00:00:00.000Z","cwd":cwd,
            "message":{"role":"user","content":"hi"}});
        fs::write(dir_a.join("s1.jsonl"), format!("{}\n{}\n", json!({"type":"mode"}), user(&project))).unwrap();
        fs::write(root.join("-decoy/s2.jsonl"), format!("{}\n", user(Path::new("/elsewhere")))).unwrap();
        let agent = subagents.join("agent-x.jsonl");
        fs::write(&agent, format!("{}\n", user(&project))).unwrap();
        fs::write(
            subagents.join("agent-x.meta.json"),
            json!({"agentType":"Explore","description":"Find the bug"}).to_string(),
        )
        .unwrap();
        fs::write(dir_a.join("s1/tool-results/y.jsonl"), "{}\n").unwrap();

        assert_eq!(discover_project_dir(&root, &project), Some(dir_a.clone()));
        let mut scanned = scan_files(&dir_a);
        scanned.sort();
        let mut expected = vec![dir_a.join("s1.jsonl"), agent.clone()];
        expected.sort();
        assert_eq!(scanned, expected);

        let mut model = Model::new(project.clone());
        let sub = ensure_participant(&mut model, &agent, &dir_a);
        let main = model.file_participant[&dir_a.join("s1.jsonl")];
        assert_eq!(model.participants[main.0].kind, ParticipantKind::Main);
        let p = &model.participants[sub.0];
        assert_eq!(p.kind, ParticipantKind::Subagent);
        assert_eq!(p.label, "Find the bug");
        assert_eq!(p.parent, Some(main));
        assert_eq!(p.session, model.participants[main.0].session);
        assert!(p.session.is_some());

        let _ = fs::remove_dir_all(&base);
    }
}
