//! Default harness session roots and the antty state dir, plus ~ expansion.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use anyhow::{Result, bail};

pub fn expand_tilde(p: &Path) -> PathBuf {
    if let Ok(s) = p.strip_prefix("~")
        && let Ok(home) = std::env::var("HOME")
    {
        return Path::new(&home).join(s);
    }
    p.to_path_buf()
}

pub fn default_state_dir(xdg_state_home: Option<OsString>, home: Option<OsString>) -> Result<PathBuf> {
    if let Some(dir) = xdg_state_home.filter(|p| !p.is_empty() && Path::new(p).is_absolute()) {
        return Ok(PathBuf::from(dir).join("antty"));
    }
    if let Some(dir) = home.filter(|p| !p.is_empty() && Path::new(p).is_absolute()) {
        return Ok(PathBuf::from(dir).join(".local/state/antty"));
    }
    bail!(
        "cannot choose a default --state-dir: neither $XDG_STATE_HOME nor $HOME is an absolute path; pass --state-dir"
    )
}

pub fn default_claude_dir(claude_config_dir: Option<OsString>, home: &str) -> PathBuf {
    match claude_config_dir.filter(|v| !v.is_empty()) {
        Some(v) => expand_tilde(Path::new(&v)).join("projects"),
        None => Path::new(home).join(".claude/projects"),
    }
}

pub fn default_codex_dir(codex_home: Option<OsString>, home: &str) -> PathBuf {
    match codex_home.filter(|v| !v.is_empty()) {
        Some(v) => expand_tilde(Path::new(&v)).join("sessions"),
        None => Path::new(home).join(".codex/sessions"),
    }
}

pub fn default_omp_dir(home: &str) -> PathBuf {
    Path::new(home).join(".omp/agent/sessions")
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;
    use std::path::PathBuf;

    use super::{default_claude_dir, default_codex_dir, default_state_dir};

    #[test]
    fn default_state_dir_uses_absolute_xdg_or_home() {
        let home = Some(OsString::from("/home/tester"));
        assert_eq!(
            default_state_dir(Some(OsString::from("/var/state")), home.clone()).unwrap(),
            PathBuf::from("/var/state/antty")
        );
        for xdg in [OsString::from("rel"), OsString::new()] {
            assert_eq!(
                default_state_dir(Some(xdg), home.clone()).unwrap(),
                PathBuf::from("/home/tester/.local/state/antty")
            );
        }
        assert!(default_state_dir(None, None).is_err());
        assert!(default_state_dir(None, Some(OsString::new())).is_err());
    }

    #[test]
    fn default_claude_dir_prefers_config_dir() {
        assert_eq!(default_claude_dir(Some(OsString::from("/cfg")), "/home/t"), PathBuf::from("/cfg/projects"));
        for unset in [Some(OsString::new()), None] {
            assert_eq!(default_claude_dir(unset, "/home/t"), PathBuf::from("/home/t/.claude/projects"));
        }
    }
    #[test]
    fn default_codex_dir_prefers_codex_home() {
        assert_eq!(default_codex_dir(Some(OsString::from("/c")), "/home/t"), PathBuf::from("/c/sessions"));
        for unset in [Some(OsString::new()), None] {
            assert_eq!(default_codex_dir(unset, "/home/t"), PathBuf::from("/home/t/.codex/sessions"));
        }
    }
}
