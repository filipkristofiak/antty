use crate::editor;
use crate::model::{EventDetail, FileEvent, Model};
use crate::snapshot;

/// Convert omp's numbered edit lines into unified hunks. Context numbers refer to old
/// lines, additions to new lines; blank lines or skipped line numbers separate hunks.
fn numbered_hunks(diff: &str) -> Option<String> {
    struct Hunk {
        old_start: i64,
        new_start: i64,
        old_next: i64,
        new_next: i64,
        old_count: usize,
        new_count: usize,
        changed: bool,
        body: String,
    }

    fn finish(hunk: Hunk, out: &mut String) -> i64 {
        if hunk.changed {
            let old_start = if hunk.old_count == 0 { hunk.old_start - 1 } else { hunk.old_start };
            let new_start = if hunk.new_count == 0 { hunk.new_start - 1 } else { hunk.new_start };
            out.push_str(&format!(
                "@@ -{old_start},{} +{new_start},{} @@\n{}",
                hunk.old_count, hunk.new_count, hunk.body
            ));
        }
        hunk.new_next - hunk.old_next
    }

    let mut out = String::new();
    let mut current: Option<Hunk> = None;
    let mut offset = 0i64;
    for line in diff.lines() {
        if line.is_empty() {
            if let Some(hunk) = current.take() {
                offset = finish(hunk, &mut out);
            }
            continue;
        }
        let mark = line.chars().next()?;
        if !matches!(mark, '+' | '-' | ' ') {
            return None;
        }
        let rest = &line[mark.len_utf8()..];
        let (number, text) = rest.trim_start().split_once('|')?;
        if number.is_empty() || !number.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        let number: i64 = number.parse().ok()?;
        if number < 1 {
            return None;
        }
        let contiguous = current.as_ref().is_some_and(|hunk| {
            number == if mark == '+' { hunk.new_next } else { hunk.old_next }
        });
        if current.is_some() && !contiguous {
            offset = finish(current.take().unwrap(), &mut out);
        }
        let hunk = current.get_or_insert_with(|| {
            let (old, new) = if mark == '+' {
                (number - offset, number)
            } else {
                (number, number + offset)
            };
            Hunk {
                old_start: old, new_start: new, old_next: old, new_next: new,
                old_count: 0, new_count: 0, changed: false, body: String::new(),
            }
        });
        if hunk.old_next < 1 || hunk.new_next < 1 {
            return None;
        }
        hunk.body.push(mark);
        hunk.body.push_str(text);
        hunk.body.push('\n');
        if mark != '+' {
            hunk.old_count += 1;
            hunk.old_next += 1;
        }
        if mark != '-' {
            hunk.new_count += 1;
            hunk.new_next += 1;
        }
        hunk.changed |= mark != ' ';
    }
    if let Some(hunk) = current {
        finish(hunk, &mut out);
    }
    Some(out)
}

/// Unified diff with `---`/`+++` headers for `e`, for delta's stdin.
pub fn event_patch(model: &Model, e: &FileEvent) -> Result<String, &'static str> {
    let hunks = match &e.detail {
        EventDetail::Written { content } => {
            let prev = model.snapshot_before_with_ts(&e.rel, e.start).map_or("", |(_, s)| s);
            snapshot::unified(prev, content)
        }
        EventDetail::Fs { diff: Some(d), .. } => d.clone(),
        EventDetail::Diff(d) if d.lines().any(|line| line.starts_with("@@")) => d.clone(),
        EventDetail::Diff(d) => numbered_hunks(d).ok_or("no unified diff for this event")?,
        EventDetail::Fs { diff: None, .. } | EventDetail::Moved { .. } | EventDetail::Removed
        | EventDetail::Search { .. } | EventDetail::None => return Err("no diff for this event"),
    };
    if hunks.is_empty() {
        return Err("no changes to show");
    }
    let label = e.rel.to_string_lossy();
    Ok(format!("--- {label}\n+++ {label}\n{hunks}"))
}

pub fn invocation(patch: String) -> editor::Invocation {
    editor::Invocation {
        program: "delta".into(),
        args: vec!["--paging".into(), "always".into()],
        suspend: true,
        stdin: Some(patch),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{FsChange, ParticipantId, Scope, TouchKind, TouchSource};
    use std::path::PathBuf;

    fn event(detail: EventDetail) -> FileEvent {
        let start = chrono::Utc::now();
        FileEvent {
            who: ParticipantId(0),
            rel: PathBuf::from("a.rs"),
            scope: Scope::Project,
            kind: TouchKind::Write,
            source: TouchSource::Tool("edit".into()),
            start,
            end: start + chrono::Duration::seconds(1),
            tool_call_id: None,
            detail,
        }
    }

    #[test]
    fn claude_hunks_keep_their_original_content() {
        let model = Model::new(PathBuf::from("/tmp/x"));
        let e = event(EventDetail::Diff("@@ -1,1 +1,1 @@\n-old\n+new\n".into()));
        assert_eq!(event_patch(&model, &e), Ok("--- a.rs\n+++ a.rs\n@@ -1,1 +1,1 @@\n-old\n+new\n".into()));
    }

    #[test]
    fn numbered_edit_renders_without_snapshots() {
        let model = Model::new(PathBuf::from("/tmp/x"));
        let e = event(EventDetail::Diff("-1|old\n+1|new\n".into()));
        assert_eq!(event_patch(&model, &e), Ok("--- a.rs\n+++ a.rs\n@@ -1,1 +1,1 @@\n-old\n+new\n".into()));
    }

    #[test]
    fn numbered_edit_matches_live_omp_insertion() {
        let model = Model::new(PathBuf::from("/tmp/x"));
        let e = event(EventDetail::Diff(
            " 3|mod cli;\n 4|mod cmdline;\n+5|mod delta;\n 5|mod editor;\n 6|mod model;".into()
        ));
        assert_eq!(event_patch(&model, &e), Ok(concat!(
            "--- a.rs\n+++ a.rs\n@@ -3,4 +3,5 @@\n",
            " mod cli;\n mod cmdline;\n+mod delta;\n mod editor;\n mod model;\n"
        ).into()));
    }

    #[test]
    fn numbered_edit_keeps_disjoint_hunks_and_adjusts_new_line_numbers() {
        let model = Model::new(PathBuf::from("/tmp/x"));
        let e = event(EventDetail::Diff(
            " 261|old context\n\n 274|before\n 275|near\n+276|first\n+277|second\n 276|after\n\n 299|far\n-300|old\n+302|replacement\n".into()
        ));
        assert_eq!(event_patch(&model, &e), Ok(concat!(
            "--- a.rs\n+++ a.rs\n",
            "@@ -274,3 +274,5 @@\n before\n near\n+first\n+second\n after\n",
            "@@ -299,2 +301,2 @@\n far\n-old\n+replacement\n"
        ).into()));
    }

    #[test]
    fn malformed_numbered_edit_has_no_unified_diff() {
        let model = Model::new(PathBuf::from("/tmp/x"));
        assert_eq!(event_patch(&model, &event(EventDetail::Diff("not a numbered diff".into()))), Err("no unified diff for this event"));
        assert_eq!(event_patch(&model, &event(EventDetail::Diff("… truncated\n".into()))), Err("no unified diff for this event"));
    }

    #[test]
    fn written_file_without_prior_snapshot_is_an_addition() {
        let model = Model::new(PathBuf::from("/tmp/x"));
        let e = event(EventDetail::Written { content: "x\n".into() });
        assert!(event_patch(&model, &e).unwrap().contains("+x"));
    }

    #[test]
    fn fs_diff_is_used_and_events_without_diffs_are_rejected() {
        let model = Model::new(PathBuf::from("/tmp/x"));
        let e = event(EventDetail::Fs { change: FsChange::Modified, diff: Some("@@ -1 +1 @@\n-a\n+b\n".into()) });
        assert_eq!(event_patch(&model, &e), Ok("--- a.rs\n+++ a.rs\n@@ -1 +1 @@\n-a\n+b\n".into()));
        assert_eq!(event_patch(&model, &event(EventDetail::Removed)), Err("no diff for this event"));
        assert_eq!(event_patch(&model, &event(EventDetail::Fs { change: FsChange::Modified, diff: None })), Err("no diff for this event"));
        assert_eq!(event_patch(&model, &event(EventDetail::Diff(String::new()))), Err("no changes to show"));
        assert_eq!(event_patch(&model, &event(EventDetail::Fs { change: FsChange::Modified, diff: Some(String::new()) })), Err("no changes to show"));
    }
}
