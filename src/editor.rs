use std::ffi::OsString;
use std::path::{Path, PathBuf};

use crate::model::{EventDetail, FileEvent, Model, Scope};
use crate::snapshot;

/// On-disk path of an event's target; None for remote/web targets.
pub fn event_path(model: &Model, e: &FileEvent) -> Option<PathBuf> {
    match e.scope {
        Scope::Project => Some(model.root.join(&e.rel)),
        Scope::External | Scope::Session(_) => Some(e.rel.clone()),
        Scope::Remote | Scope::WebSearch | Scope::WebFetch => None,
    }
}

/// 1-based line of the first change in diff text, in the new file's numbering.
pub fn first_changed_line(diff: &str) -> Option<usize> {
    if diff.lines().any(|line| line.starts_with("@@")) {
        let mut cur = None;
        for line in diff.lines() {
            if line.starts_with("@@") {
                cur = line.split_once(" +").and_then(|(_, rest)| {
                    rest.split([',', ' ']).next()?.parse::<usize>().ok()
                });
            } else if let Some(n) = cur.as_mut() {
                if (line.starts_with('+') && !line.starts_with("+++"))
                    || (line.starts_with('-') && !line.starts_with("---"))
                {
                    return Some((*n).max(1));
                }
                if line.starts_with(' ') || line.is_empty() {
                    *n += 1;
                }
            }
        }
        None
    } else {
        diff.lines().find_map(|line| {
            let rest = line.strip_prefix(['+', '-'])?.trim_start();
            let (number, _) = rest.split_once('|')?;
            if number.is_empty() || !number.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            number.parse().ok()
        })
    }
}

/// First changed line of a write event, when its detail carries enough to compute one.
pub fn event_line(model: &Model, e: &FileEvent) -> Option<usize> {
    match &e.detail {
        EventDetail::Diff(d) | EventDetail::Fs { diff: Some(d), .. } => first_changed_line(d),
        EventDetail::Written { content } => {
            let (_, prev) = model.snapshot_before_with_ts(&e.rel, e.start)?;
            first_changed_line(&snapshot::unified(prev, content))
        }
        _ => None,
    }
}

pub struct Invocation {
    pub program: OsString,
    pub args: Vec<OsString>,
    pub suspend: bool,
}

pub fn invocation(
    path: &Path,
    line: Option<usize>,
    nvim: Option<OsString>,
    editor: Option<OsString>,
) -> Result<Invocation, String> {
    if let Some(nvim) = nvim.filter(|v| !v.is_empty()) {
        let cmd = line.map_or_else(|| "drop ".to_string(), |n| format!("drop +{n} "));
        let p = path.to_string_lossy().replace('\'', "''");
        let expr = format!("execute('{cmd}' .. fnameescape('{p}'))");
        return Ok(Invocation {
            program: "nvim".into(),
            args: vec!["--server".into(), nvim, "--remote-expr".into(), expr.into()],
            suspend: false,
        });
    }
    let editor = editor.filter(|v| !v.is_empty()).ok_or("$EDITOR is not set")?;
    let mut words = editor.to_string_lossy().split_whitespace().map(OsString::from).collect::<Vec<_>>();
    if words.is_empty() {
        return Err("$EDITOR is not set".into());
    }
    let program = words.remove(0);
    if let Some(n) = line {
        words.push(format!("+{n}").into());
    }
    words.push(path.as_os_str().to_os_string());
    Ok(Invocation { program, args: words, suspend: true })
}

/// Run `inv`; returns a flash message on failure. Suspending invocations hand the terminal to
/// the child and restore the TUI afterwards.
pub fn run(terminal: &mut ratatui::DefaultTerminal, inv: &Invocation) -> std::io::Result<Option<String>> {
    use std::io::stdout;
    use std::process::{Command, Stdio};

    let program = inv.program.to_string_lossy();
    if !inv.suspend {
        let result = Command::new(&inv.program).args(&inv.args).stdin(Stdio::null()).output();
        return Ok(match result {
            Err(err) => Some(format!("{program}: {err}")),
            Ok(out) if !out.status.success() => Some(
                String::from_utf8_lossy(&out.stderr)
                    .lines()
                    .find(|line| !line.trim().is_empty())
                    .map(str::to_owned)
                    .unwrap_or_else(|| format!("{program} exited with {}", out.status)),
            ),
            Ok(_) => None,
        });
    }

    ratatui::restore();
    crossterm::execute!(stdout(), crossterm::cursor::Show)?;
    let result = Command::new(&inv.program).args(&inv.args).status();
    crossterm::terminal::enable_raw_mode()?;
    crossterm::execute!(stdout(), crossterm::terminal::EnterAlternateScreen)?;
    terminal.clear()?;
    terminal.hide_cursor()?;
    Ok(match result {
        Err(err) => Some(format!("{program}: {err}")),
        Ok(status) if !status.success() => Some(format!("{program} exited with {status}")),
        Ok(_) => None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_first_changed_line() {
        assert_eq!(first_changed_line("@@ -10,4 +12,5 @@\n ctx\n ctx\n-old\n+new\n"), Some(14));
        assert_eq!(first_changed_line("@@ -0,0 +1,2 @@\n+a\n+b\n"), Some(1));
        assert_eq!(first_changed_line("@@ -1,1 +1,1 @@\n-old\n+new\n"), Some(1));
        assert_eq!(first_changed_line(" 145|pub struct Model {\n-173|    old\n+173|    new\n"), Some(173));
        assert_eq!(first_changed_line("+plain inserted line\n"), None);
        assert_eq!(first_changed_line(""), None);
    }

    #[test]
    fn builds_invocations() {
        let path = Path::new("/p/it's.rs");
        let remote = invocation(path, Some(7), Some("/tmp/s".into()), None).unwrap();
        assert_eq!(remote.program, "nvim");
        assert_eq!(remote.args, ["--server", "/tmp/s", "--remote-expr", "execute('drop +7 ' .. fnameescape('/p/it''s.rs'))"]);
        assert!(!remote.suspend);
        let local = invocation(path, Some(3), None, Some("code -w".into())).unwrap();
        assert_eq!(local.program, "code");
        assert_eq!(local.args, ["-w", "+3", "/p/it's.rs"]);
        assert!(local.suspend);
        let no_line = invocation(path, None, None, Some("code -w".into())).unwrap();
        assert_eq!(no_line.args, ["-w", "/p/it's.rs"]);
        let remote_no_line = invocation(path, None, Some("/tmp/s".into()), None).unwrap();
        assert_eq!(remote_no_line.args[3], "execute('drop ' .. fnameescape('/p/it''s.rs'))");
        assert!(invocation(path, None, None, None).is_err_and(|e| e == "$EDITOR is not set"));
        assert!(invocation(path, None, Some(OsString::new()), Some(OsString::new())).is_err_and(|e| e == "$EDITOR is not set"));
    }
}
