use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, SyncSender};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use antty::footprint;
use antty::index::cached_file;
use antty::model::{EventDetail, FileEvent, Model};
use antty::sessions::{Harness, SessionFile, TailState, apply_records, read_appended};
use chrono::{DateTime, Utc};

use crate::cache;
use crate::discover::{IDLE_GAP, SessionInfo, Source, drill_for, session_span, sessions_of};

const TICK: Duration = Duration::from_secs(1);
const SAVE_EVERY: Duration = Duration::from_secs(60);

pub struct Resume {
    pub model: Model,
    pub tails: HashMap<PathBuf, TailState>,
}

pub struct Job {
    pub key: PathBuf,
    pub harness: Harness,
    pub cwd: PathBuf,
    pub files: Vec<SessionFile>,
    pub resume: Option<Resume>,
}

pub struct Scanned {
    pub key: PathBuf,
    pub resume: Resume,
    pub info: SessionInfo,
    /// Thin projection behind `info`, shared with the cache entry and the UI's stats.
    pub model: Arc<Model>,
    pub entry: Option<cache::Entry>,
}

fn thin(model: &Model, cwd: &Path) -> Model {
    let mut thin = Model::new(vec![cwd.to_path_buf()]);
    thin.participants = model.participants.clone();
    thin.sessions = model.sessions.clone();
    thin.spans = model.spans.clone();
    thin.prompts = model.prompts.clone();
    thin.tool_calls = model.tool_calls.clone();
    thin.file_participant = model.file_participant.clone();
    thin.events = model
        .events
        .iter()
        .map(|e| FileEvent {
            who: e.who,
            rel: e.rel.clone(),
            scope: e.scope,
            kind: e.kind,
            source: e.source.clone(),
            start: e.start,
            end: e.end,
            tool_call_id: e.tool_call_id.clone(),
            detail: EventDetail::None,
        })
        .collect();
    thin
}

fn session_footprint(files: &[SessionFile]) -> u64 {
    match files {
        [file] => footprint::of_log(&file.path),
        files => {
            let logs: Vec<_> = files.iter().map(|f| f.path.as_path()).collect();
            footprint::session(&logs, footprint::of_log)
        }
    }
}

fn info_from(model: &Model, key: &Path, harness: Harness, cwd: &Path, disk: u64) -> SessionInfo {
    let session = model.sessions.iter().find(|s| s.file == key);
    let activity = antty::timeline::visible_activity(model)
        .into_iter()
        .map(|(s, e)| (DateTime::from_timestamp(s, 0).unwrap(), DateTime::from_timestamp(e, 0).unwrap()))
        .collect();
    SessionInfo {
        harness,
        cwd: cwd.to_path_buf(),
        file: key.to_path_buf(),
        drill: drill_for(cwd),
        title: session.map_or_else(|| "(untitled)".into(), |s| s.title.clone()),
        span: session.and_then(session_span),
        activity: Some(activity),
        disk: Some(disk),
    }
}

fn ingest_session(job: Job, session_roots: &[PathBuf]) -> Scanned {
    let Job { key, harness, cwd, files, resume } = job;
    let mut resume = resume.unwrap_or_else(|| {
        let mut model = Model::new(vec![cwd.clone()]);
        model.session_roots = session_roots.to_vec();
        Resume { model, tails: HashMap::new() }
    });
    for f in &files {
        let state = resume.tails.entry(f.path.clone()).or_insert(TailState { offset: 0, partial: Vec::new() });
        let records = read_appended(&f.path, state);
        // Register even a file with no complete records, preserving main-first order.
        apply_records(&mut resume.model, harness, &f.project_dir, &f.path, &records, IDLE_GAP);
    }
    let model = Arc::new(thin(&resume.model, &cwd));
    let info = info_from(&model, &key, harness, &cwd, session_footprint(&files));
    let cached_files: std::io::Result<Vec<_>> = files
        .into_iter()
        .map(|f| {
            let file = cached_file(&f.path, resume.tails[&f.path].offset)?;
            Ok(cache::CachedSessionFile { project_dir: f.project_dir, file })
        })
        .collect();
    let entry = cached_files.ok().map(|files| cache::Entry { harness, cwd, files, model: model.clone() });
    Scanned { key, resume, info, model, entry }
}

pub enum Msg {
    Session { info: SessionInfo, fresh_title: bool, model: Arc<Model> },
    Progress { done: usize, total: usize },
    Indexed { sessions: usize, scanned: usize, elapsed: Duration },
    Notice(String),
}

struct Tracked {
    harness: Harness,
    cwd: PathBuf,
    files: Vec<SessionFile>,
    offsets: HashMap<PathBuf, u64>,
    resume: Option<Resume>,
    entry: Option<cache::Entry>,
}

pub struct Indexer {
    roots: [(Harness, PathBuf); 3],
    session_roots: Vec<PathBuf>,
    cache: Option<PathBuf>,
    tracked: HashMap<PathBuf, Tracked>,
    last_files: HashMap<(Harness, PathBuf), Vec<PathBuf>>,
    dirty: bool,
    last_save: Instant,
    tx: SyncSender<Msg>,
}

fn run_jobs(mut jobs: Vec<Job>, session_roots: &[PathBuf], mut each: impl FnMut(Scanned) -> bool) -> bool {
    if jobs.is_empty() {
        return true;
    }
    let workers = thread::available_parallelism().map_or(1, |n| n.get()).min(jobs.len());
    jobs.reverse();
    let jobs = Mutex::new(jobs);
    let stop = AtomicBool::new(false);
    let (tx, rx) = mpsc::channel();
    thread::scope(|scope| {
        for _ in 0..workers {
            let (tx, stop, jobs) = (tx.clone(), &stop, &jobs);
            scope.spawn(move || {
                while !stop.load(Ordering::Relaxed) {
                    let job = jobs.lock().unwrap_or_else(PoisonError::into_inner).pop();
                    let Some(job) = job else { break };
                    if tx.send(ingest_session(job, session_roots)).is_err() {
                        break;
                    }
                }
            });
        }
        drop(tx);
        for scanned in rx {
            if !each(scanned) {
                stop.store(true, Ordering::Relaxed);
                return false;
            }
        }
        true
    })
}

impl Indexer {
    pub fn new(roots: [(Harness, PathBuf); 3], cache: Option<PathBuf>, tx: SyncSender<Msg>) -> Self {
        let session_roots = roots.iter().filter(|(h, _)| *h != Harness::Codex).map(|(_, p)| p.clone()).collect();
        Self {
            roots,
            session_roots,
            cache,
            tracked: HashMap::new(),
            last_files: HashMap::new(),
            dirty: false,
            last_save: Instant::now(),
            tx,
        }
    }

    fn absorb(&mut self, s: Scanned, fresh_title: bool) -> bool {
        let tracked = self.tracked.get_mut(&s.key).expect("scan for a tracked session");
        tracked.offsets = s.resume.tails.iter().map(|(p, t)| (p.clone(), t.offset)).collect();
        tracked.entry = s.entry;
        let live_since = Utc::now() - chrono::Duration::hours(antty::tui::LIVE_HOURS);
        tracked.resume = s.info.span.is_some_and(|(_, end)| end >= live_since).then_some(s.resume);
        self.dirty = true;
        self.tx.send(Msg::Session { info: s.info, fresh_title, model: s.model }).is_ok()
    }

    fn process(&mut self, jobs: Vec<Job>, progress: bool) -> bool {
        let total = jobs.len();
        let mut done = 0;
        let roots = std::mem::take(&mut self.session_roots);
        let ok = run_jobs(jobs, &roots, |s| {
            done += 1;
            self.absorb(s, true) && (!progress || self.tx.send(Msg::Progress { done, total }).is_ok())
        });
        self.session_roots = roots;
        ok
    }

    fn save(&mut self) -> bool {
        self.last_save = Instant::now();
        self.dirty = false;
        if let Some(path) = &self.cache
            && let Err(e) = cache::save(path, self.tracked.values().filter_map(|s| s.entry.as_ref()))
        {
            return self.tx.send(Msg::Notice(format!("activity cache not saved: {e}"))).is_ok();
        }
        true
    }

    pub fn initial(&mut self, sources: Vec<Source>) -> bool {
        let start = Instant::now();
        let sessions = sources.len();
        let mut cached = self.cache.as_deref().map(cache::load).unwrap_or_default();
        let loaded = cached.len();
        let mut jobs = Vec::new();
        let mut hits = 0;
        for Source { harness, cwd, files } in sources {
            let key = files[0].path.clone();
            let entry = cached.remove(&key).filter(|e| cache::valid(e, harness, &cwd, &files));
            let offsets = if let Some(e) = &entry {
                hits += 1;
                let info = info_from(&e.model, &key, harness, &cwd, session_footprint(&files));
                if self.tx.send(Msg::Session { info, fresh_title: false, model: e.model.clone() }).is_err() {
                    return false;
                }
                e.files.iter().map(|f| (f.file.path.clone(), f.file.offset)).collect()
            } else {
                jobs.push(Job { key: key.clone(), harness, cwd: cwd.clone(), files: files.clone(), resume: None });
                HashMap::new()
            };
            self.tracked.insert(key, Tracked { harness, cwd, files, offsets, resume: None, entry });
        }
        drop(cached);
        let scanned = jobs.len();
        if !self.process(jobs, true) || ((scanned > 0 || loaded != hits) && !self.save()) {
            return false;
        }
        self.tx.send(Msg::Indexed { sessions, scanned, elapsed: start.elapsed() }).is_ok()
    }

    fn rediscover(&mut self) {
        for (harness, root) in &self.roots {
            let index = harness.index_root(root);
            for cwd in index.cwds() {
                let paths = self.last_files.entry((*harness, cwd.to_path_buf())).or_default();
                let files = index.files(cwd);
                if paths.iter().eq(files.iter().map(|f| &f.path)) {
                    continue;
                }
                *paths = files.iter().map(|f| f.path.clone()).collect();
                let (_, groups) = sessions_of(*harness, cwd, &files);
                for group in groups {
                    let Some(first) = group.first() else { continue };
                    let tracked = self.tracked.entry(first.path.clone()).or_insert_with(|| Tracked {
                        harness: *harness,
                        cwd: cwd.to_path_buf(),
                        files: Vec::new(),
                        offsets: HashMap::new(),
                        resume: None,
                        entry: None,
                    });
                    // A removed child must not survive in a projection for the new group.
                    if tracked.files.iter().any(|old| !group.iter().any(|f| f.path == old.path)) {
                        tracked.offsets.clear();
                        tracked.resume = None;
                    }
                    tracked.files = group;
                }
            }
        }
    }

    pub fn tick(&mut self) -> bool {
        self.rediscover();
        let mut jobs = Vec::new();
        for (key, tracked) in &mut self.tracked {
            let mut restart = false;
            let mut changed = false;
            for f in &tracked.files {
                let Ok(meta) = fs::metadata(&f.path) else { continue };
                let offset = tracked.offsets.get(&f.path).copied().unwrap_or(0);
                changed |= meta.len() != offset;
                restart |= meta.len() < offset;
            }
            if changed {
                let resume = tracked.resume.take().filter(|_| !restart);
                jobs.push(Job {
                    key: key.clone(),
                    harness: tracked.harness,
                    cwd: tracked.cwd.clone(),
                    files: tracked.files.clone(),
                    resume,
                });
            }
        }
        self.process(jobs, false) && (!self.dirty || self.last_save.elapsed() < SAVE_EVERY || self.save())
    }
}

pub fn spawn(
    roots: [(Harness, PathBuf); 3],
    sources: Vec<Source>,
    cache: Option<PathBuf>,
    tx: SyncSender<Msg>,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        let mut indexer = Indexer::new(roots, cache, tx);
        if !indexer.initial(sources) {
            return;
        }
        loop {
            thread::sleep(TICK);
            if !indexer.tick() {
                return;
            }
        }
    })
}

#[cfg(test)]
mod tests;
