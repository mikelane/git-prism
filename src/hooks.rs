//! `git-prism hooks` subcommand: uninstall / status legacy redirect-hook entries,
//! and install / uninstall / status the PATH shim.
//!
//! The redirect hook (`bash_redirect_hook.py`) was removed in v0.9.0. See
//! ADR-0011 (`docs/decisions/0011-redirect-hook-removal.md`) for rationale.
//! `git-prism hooks install` (without `--path-shim`) now exits non-zero and
//! directs users to `git-prism shim install`.
//!
//! `hooks uninstall` and `hooks status` remain available for users who had the
//! old hook installed and need to clean it up.
//!
//! Three scopes for uninstall:
//! - `user`    -> `~/.claude/settings.json`
//! - `project` -> `<cwd>/.claude/settings.json`
//! - `local`   -> `<cwd>/.claude/settings.local.json`

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow};
use serde_json::{Map, Value};

/// Resolve `$HOME` for the current process. Returns an error rather than a
/// silent fallback so misconfigured environments fail loudly instead of
/// quietly writing into `/.claude/settings.json`.
pub fn home_dir() -> Result<PathBuf> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or_else(|| anyhow!("HOME environment variable is not set"))
}

/// Where on disk a given install scope lives, expanded against `$HOME` and
/// the current working directory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    User,
    Project,
    Local,
}

impl Scope {
    /// Parse the CLI argument value back into a `Scope`.
    pub fn parse(value: &str) -> Result<Self> {
        match value {
            "user" => Ok(Self::User),
            "project" => Ok(Self::Project),
            "local" => Ok(Self::Local),
            other => anyhow::bail!("unknown scope {other:?} (expected user|project|local)"),
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Scope::User => "user",
            Scope::Project => "project",
            Scope::Local => "local",
        }
    }
}

/// Resolved on-disk locations for a scope.
#[derive(Debug, Clone)]
pub struct ScopePaths {
    pub settings_file: PathBuf,
}

impl ScopePaths {
    pub fn resolve(scope: Scope, home: &Path, cwd: &Path) -> Self {
        match scope {
            Scope::User => {
                let claude = home.join(".claude");
                Self {
                    settings_file: claude.join("settings.json"),
                }
            }
            Scope::Project => {
                let claude = cwd.join(".claude");
                Self {
                    settings_file: claude.join("settings.json"),
                }
            }
            Scope::Local => {
                let claude = cwd.join(".claude");
                Self {
                    settings_file: claude.join("settings.local.json"),
                }
            }
        }
    }
}

/// Read the settings file at `path` if it exists. Returns an empty object
/// when the file is absent. Malformed JSON is a hard error.
fn read_settings(path: &Path) -> Result<Value> {
    if !path.exists() {
        return Ok(Value::Object(Map::new()));
    }
    let raw = std::fs::read_to_string(path)
        .with_context(|| format!("failed to read settings at {}", path.display()))?;
    if raw.trim().is_empty() {
        return Ok(Value::Object(Map::new()));
    }
    serde_json::from_str(&raw)
        .with_context(|| format!("settings file at {} is not valid JSON", path.display()))
}

/// Write `settings` JSON to `path` with 2-space indentation.
fn write_settings(path: &Path, settings: &Value) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    let mut serialized =
        serde_json::to_string_pretty(settings).context("failed to serialize settings JSON")?;
    serialized.push('\n');
    std::fs::write(path, serialized)
        .with_context(|| format!("failed to write settings to {}", path.display()))?;
    Ok(())
}

/// Borrow (or insert) the `hooks.PreToolUse` array on `settings`.
fn pretool_use_array_mut(settings: &mut Value) -> &mut Vec<Value> {
    if !settings.is_object() {
        *settings = Value::Object(Map::new());
    }
    let root = settings.as_object_mut().expect("ensured object above");
    let hooks = root
        .entry("hooks".to_string())
        .or_insert_with(|| Value::Object(Map::new()));
    if !hooks.is_object() {
        *hooks = Value::Object(Map::new());
    }
    let hooks_obj = hooks.as_object_mut().expect("ensured object above");
    let pretool_use = hooks_obj
        .entry("PreToolUse".to_string())
        .or_insert_with(|| Value::Array(Vec::new()));
    if !pretool_use.is_array() {
        *pretool_use = Value::Array(Vec::new());
    }
    pretool_use.as_array_mut().expect("ensured array above")
}

/// Remove every `PreToolUse` entry whose id starts with
/// `git-prism-bash-redirect-`. Other entries are left untouched.
pub fn uninstall_redirect_hook(scope: Scope, home: &Path, cwd: &Path) -> Result<()> {
    let paths = ScopePaths::resolve(scope, home, cwd);
    if !paths.settings_file.exists() {
        return Ok(());
    }
    let mut settings = read_settings(&paths.settings_file)?;
    let entry_count_before = {
        let entries = pretool_use_array_mut(&mut settings);
        let len = entries.len();
        entries.retain(|entry| {
            let id = entry.get("id").and_then(|v| v.as_str()).unwrap_or("");
            !id.starts_with("git-prism-bash-redirect-")
        });
        len
    };
    let entry_count_after = pretool_use_array_mut(&mut settings).len();
    if entry_count_after < entry_count_before {
        write_settings(&paths.settings_file, &settings)?;
    }
    Ok(())
}

/// The legacy on-disk directory where releases before the rc-function shim
/// installed the `git`/`gh` symlinks, relative to `$HOME`. Install, uninstall
/// and status still look here to migrate it away.
pub const PATH_SHIM_REL_DIR: &str = ".local/share/git-prism/bin";

/// Names of the legacy shim symlinks inside `PATH_SHIM_REL_DIR`.
pub const PATH_SHIM_LINK_NAMES: &[&str] = &["git", "gh"];

/// Lines reported by `hooks status` — one per scope that has a legacy
/// redirect-hook sentinel, or a single `not installed` line when nothing is
/// found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatusReport {
    pub lines: Vec<String>,
}

/// Build the `hooks status` report by scanning all three scopes for any
/// lingering redirect-hook entries.
///
/// `cwd_is_repo` controls whether project/local scopes are considered.
pub fn status_report(home: &Path, cwd: &Path, cwd_is_repo: bool) -> Result<StatusReport> {
    let mut lines = Vec::new();
    for scope in [Scope::User, Scope::Project, Scope::Local] {
        if !cwd_is_repo && matches!(scope, Scope::Project | Scope::Local) {
            continue;
        }
        let paths = ScopePaths::resolve(scope, home, cwd);
        let settings = read_settings(&paths.settings_file)?;
        let entries = settings
            .get("hooks")
            .and_then(|h| h.get("PreToolUse"))
            .and_then(|p| p.as_array());
        let Some(entries) = entries else { continue };
        for entry in entries {
            let id = entry.get("id").and_then(|v| v.as_str()).unwrap_or("");
            if id.starts_with("git-prism-bash-redirect-") {
                lines.push(format!("{}: {}", scope.as_str(), id));
                break;
            }
        }
    }
    if lines.is_empty() {
        lines.push("not installed".to_string());
    }
    Ok(StatusReport { lines })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tempfile::TempDir;

    // -------------------------------------------------------------------------
    // uninstall_redirect_hook
    // -------------------------------------------------------------------------

    #[test]
    fn uninstall_removes_only_our_entries() {
        let dir = TempDir::new().unwrap();
        let home = dir.path();
        let settings = home.join(".claude").join("settings.json");
        let unrelated = json!({
            "hooks": {
                "PreToolUse": [
                    {"id": "user-custom-hook", "matcher": "Bash", "command": "echo unrelated"},
                    {"id": "git-prism-bash-redirect-v1", "matcher": "Bash", "command": "/abs/git-prism-redirect.sh"}
                ]
            }
        });
        std::fs::create_dir_all(settings.parent().unwrap()).unwrap();
        std::fs::write(&settings, serde_json::to_string_pretty(&unrelated).unwrap()).unwrap();

        uninstall_redirect_hook(Scope::User, home, home).unwrap();

        let data: Value =
            serde_json::from_str(&std::fs::read_to_string(&settings).unwrap()).unwrap();
        let entries = data["hooks"]["PreToolUse"].as_array().unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0]["id"], json!("user-custom-hook"));
    }

    #[test]
    fn uninstall_is_a_no_op_when_settings_file_does_not_exist() {
        let dir = TempDir::new().unwrap();
        let home = dir.path();
        let result = uninstall_redirect_hook(Scope::User, home, home);
        assert!(result.is_ok(), "uninstall on absent settings must succeed");
        assert!(
            !home.join(".claude").join("settings.json").exists(),
            "uninstall must not create settings.json when it did not exist"
        );
    }

    // -------------------------------------------------------------------------
    // status_report
    // -------------------------------------------------------------------------

    #[test]
    fn status_reports_not_installed_when_no_settings_files_exist() {
        let dir = TempDir::new().unwrap();
        let home = dir.path();
        let report = status_report(home, home, false).unwrap();
        assert_eq!(report.lines, vec!["not installed".to_string()]);
    }

    #[test]
    fn local_scope_resolves_to_settings_local_json() {
        let home = Path::new("/home/u");
        let cwd = Path::new("/proj");
        let user = ScopePaths::resolve(Scope::User, home, cwd);
        let project = ScopePaths::resolve(Scope::Project, home, cwd);
        let local = ScopePaths::resolve(Scope::Local, home, cwd);

        assert_eq!(
            user.settings_file,
            Path::new("/home/u/.claude/settings.json")
        );
        assert_eq!(
            project.settings_file,
            Path::new("/proj/.claude/settings.json")
        );
        assert_eq!(
            local.settings_file,
            Path::new("/proj/.claude/settings.local.json")
        );
    }
}
