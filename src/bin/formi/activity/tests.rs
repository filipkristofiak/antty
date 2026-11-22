use std::fmt::Write as _;
use std::io::Write as _;

use chrono::TimeDelta;
use serde_json::{Value, json};

use super::*;
use crate::discover::collect;

fn digest(m: &Model) -> Vec<String> {
    let mut rows = Vec::new();
    rows.extend(m.spans.iter().map(|s| format!("span {:?} {} {}", s.who, s.start, s.end)));
    rows.extend(m.raw_intervals.iter().map(|(who, times)| {
        let mut times = times.clone();
        times.sort_unstable();
        format!("raw {who} {times:?}")
    }));
    rows.extend(m.calls.iter().map(|(key, c)| format!("call {key:?} {} {}", c.tool, c.input)));
    rows.extend(m.tool_calls.iter().map(|(key, c)| format!("tool {key:?} {} {}", c.tool, c.start)));
    rows.extend(m.events.iter().map(|e| format!("event {e:?}")));
    rows.extend(m.prompts.iter().map(|p| format!("prompt {p:?}")));
    rows.extend(
        m.sessions
            .iter()
            .map(|s| format!("session {:?} {:?} {} {:?} {}", s.file, s.title, s.start, s.end, s.last_seen)),
    );
    rows.extend(m.pending_tools.keys().map(|key| format!("pending {key:?}")));
    rows.extend(m.last_entry_ts.iter().map(|(who, ts)| format!("last {who} {ts}")));
    rows.sort_unstable();
    rows
}

fn base(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!("antty-formi-test-{name}-{}", std::process::id()))
}

fn roots(base: &Path) -> [(Harness, PathBuf); 3] {
    [(Harness::Omp, base.join("omp")), (Harness::Claude, base.join("claude")), (Harness::Codex, base.join("codex"))]
}

fn job(harness: Harness, base: &Path, project_dir: &str, log: &str) -> Job {
    let project_dir = base.join(project_dir);
    let key = project_dir.join(log);
    fs::create_dir_all(key.parent().unwrap()).unwrap();
    Job {
        files: vec![SessionFile { project_dir, path: key.clone() }],
        key,
        harness,
        cwd: base.canonicalize().unwrap(),
        resume: None,
    }
}

fn scan(job: &Job, resume: Option<Resume>) -> Scanned {
    ingest_session(
        Job { key: job.key.clone(), harness: job.harness, cwd: job.cwd.clone(), files: job.files.clone(), resume },
        &[job.cwd.join("omp"), job.cwd.join("claude")],
    )
}

fn records(values: &[Value]) -> String {
    let mut data = String::new();
    for value in values {
        writeln!(data, "{value}").unwrap();
    }
    data
}

fn append(path: &Path, s: &str) {
    fs::OpenOptions::new().append(true).open(path).unwrap().write_all(s.as_bytes()).unwrap();
}

fn omp_header(cwd: &Path, at: DateTime<Utc>) -> String {
    records(&[
        json!({"type":"title","v":1,"title":"Fixture"}),
        json!({"type":"session","id":"s1","cwd":cwd,"timestamp":at}),
    ])
}

fn omp_turn(at: DateTime<Utc>, tool_id: &str) -> [Value; 4] {
    let stamp = |seconds| at + TimeDelta::seconds(seconds);
    let ms = |seconds| stamp(seconds).timestamp_millis();
    [
        json!({"type":"message","timestamp":stamp(0),
            "message":{"role":"user","attribution":"user","timestamp":ms(0)}}),
        json!({"type":"message","timestamp":stamp(1),
            "message":{"role":"assistant","timestamp":ms(1),"completedAt":ms(2),
            "content":[{"type":"toolCall","id":tool_id,"name":"bash","arguments":{"command":"sleep 60"}}]}}),
        json!({"type":"message","timestamp":stamp(61),
            "message":{"role":"toolResult","toolCallId":tool_id,"toolName":"bash","timestamp":ms(61)}}),
        json!({"type":"message","timestamp":stamp(62),
            "message":{"role":"assistant","timestamp":ms(62),"completedAt":ms(63)}}),
    ]
}

fn start() -> DateTime<Utc> {
    "2026-01-01T00:00:00Z".parse().unwrap()
}

fn check_split(job: Job, a: &str, b: &str, check: impl FnOnce(&Resume)) {
    fs::write(&job.key, a).unwrap();
    let split = scan(&job, None);
    check(&split.resume);
    append(&job.key, b);
    let split = scan(&job, Some(split.resume));
    let cold = scan(&job, None);
    assert_eq!(digest(&split.resume.model), digest(&cold.resume.model), "{:?}", job.harness);
}

#[test]
fn split_ingest_equals_cold_scan_for_every_harness() {
    let base = base("split");
    let at = start();
    let omp = job(Harness::Omp, &base, "omp/-proj-", "s1.jsonl");
    let mut a = omp_header(&omp.cwd, at);
    let turn = omp_turn(at, "t1");
    a.push_str(&records(&turn[..2]));
    let b = records(&turn[2..]);
    let mid = b.find('\n').unwrap() / 2;
    a.push_str(&b[..mid]);
    check_split(omp, &a, &b[mid..], |r| {
        assert!(r.model.pending_tools.keys().any(|(_, id)| id == "t1"));
        assert_eq!(r.tails.values().next().unwrap().partial, b.as_bytes()[..mid]);
    });

    let claude = job(Harness::Claude, &base, "claude/-proj", "s1.jsonl");
    let stamp = |seconds| at + TimeDelta::seconds(seconds);
    let a = records(&[json!({"type":"user","timestamp":at,"cwd":claude.cwd,
        "origin":{"kind":"human"},"message":{"role":"user","content":"fix it"}})]);
    let b = records(&[
        json!({"type":"assistant","timestamp":stamp(50),"message":{"role":"assistant",
            "content":[{"type":"tool_use","id":"b1","name":"Bash","input":{"command":"ls"}}]}}),
        json!({"type":"user","timestamp":stamp(51),"message":{"role":"user",
            "content":[{"type":"tool_result","tool_use_id":"b1","is_error":false,"content":"x"}]},
            "toolUseResult":{"stdout":""}}),
        json!({"type":"assistant","timestamp":stamp(55),"message":{"role":"assistant",
            "content":[{"type":"text","text":"done"}]}}),
    ]);
    check_split(claude, &a, &b, |r| {
        assert_eq!(r.model.last_entry_ts[&r.model.sessions[0].main.0], at);
    });

    let codex = job(Harness::Codex, &base, "codex", "2026/01/01/rollout-2026-01-01T00-00-00-split.jsonl");
    let record = |s, ty, payload: Value| json!({"timestamp":stamp(s),"type":ty,"payload":payload});
    let a = records(&[
        record(0, "session_meta", json!({"id":"split","cwd":codex.cwd})),
        record(0, "event_msg", json!({"type":"task_started"})),
        record(
            1,
            "event_msg",
            json!({"type":"item_completed","item":{
            "type":"UserMessage","content":[{"type":"text","text":"fix it"}]}}),
        ),
    ]);
    let b = records(&[
        record(30, "response_item", json!({"type":"custom_tool_call","name":"exec","call_id":"c1"})),
        record(32, "response_item", json!({"type":"custom_tool_call_output","call_id":"c1"})),
        record(40, "event_msg", json!({"type":"task_complete"})),
    ]);
    check_split(codex, &a, &b, |r| {
        assert_eq!(r.model.last_entry_ts[&r.model.sessions[0].main.0], stamp(1));
    });
    fs::remove_dir_all(base).unwrap();
}

#[test]
fn grown_file_is_rescanned_and_equals_a_cold_scan() {
    let base = base("grown");
    let job = job(Harness::Omp, &base, "omp/-proj-", "s1.jsonl");
    let roots = roots(&base);
    let path = base.join("activity.bin");
    let turn = omp_turn(start(), "t1");
    let a = omp_header(&job.cwd, start()) + &records(&turn[..2]);
    fs::write(&job.key, a).unwrap();
    let initial = |expected| {
        let (tx, rx) = mpsc::sync_channel(1024);
        let mut indexer = Indexer::new(roots.clone(), Some(path.clone()), tx);
        assert!(indexer.initial(collect(&roots).1));
        assert!(
            rx.try_iter().any(|msg| matches!(msg, Msg::Indexed { sessions: 1, scanned, .. } if scanned == expected))
        );
        indexer
    };
    initial(1);
    initial(0);
    append(&job.key, &records(&turn[2..]));
    let rescanned = initial(1);
    let model = &rescanned.tracked[&job.key].entry.as_ref().unwrap().model;
    let cold = scan(&job, None);
    assert_eq!(digest(model), digest(&thin(&cold.resume.model, &job.cwd)));
    fs::remove_dir_all(base).unwrap();
}

#[test]
fn live_tick_extends_activity_and_finds_new_sessions() {
    let base = base("live");
    let job = job(Harness::Omp, &base, "omp/-proj-", "s1.jsonl");
    let roots = roots(&base);
    let now = Utc::now();
    let old = now - TimeDelta::minutes(10);
    fs::write(&job.key, omp_header(&job.cwd, old) + &records(&omp_turn(old, "t1"))).unwrap();
    let (tx, rx) = mpsc::sync_channel(1024);
    let mut indexer = Indexer::new(roots.clone(), None, tx);
    assert!(indexer.initial(collect(&roots).1));
    rx.try_iter().for_each(drop);
    assert!(indexer.tracked[&job.key].resume.is_some());

    let recent = now - TimeDelta::minutes(2);
    append(&job.key, &records(&omp_turn(recent, "t2")));
    let other = job.key.with_file_name("s2.jsonl");
    fs::write(&other, omp_header(&job.cwd, recent) + &records(&omp_turn(recent, "t3"))).unwrap();
    assert!(indexer.tick());
    let infos: Vec<_> = rx
        .try_iter()
        .filter_map(|msg| match msg {
            Msg::Session { info, .. } => Some(info),
            _ => None,
        })
        .collect();
    let updated = infos.iter().find(|i| i.file == job.key).unwrap();
    assert!(updated.bars().last().is_some_and(|&(_, end)| end >= now - TimeDelta::minutes(1)));
    assert!(infos.iter().any(|i| i.file == other));
    fs::remove_dir_all(base).unwrap();
}
