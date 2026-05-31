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

use anyhow::{anyhow, Context, Result};
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

/// The on-disk directory where the path-shim symlink is installed.
/// Relative to `$HOME`.
pub const PATH_SHIM_REL_DIR: &str = ".local/share/git-prism/bin";

/// Name of the git shim symlink inside `PATH_SHIM_REL_DIR`.
pub const PATH_SHIM_LINK_NAME: &str = "git";

/// Status of the path-shim symlink reported by `hooks status`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PathShimStatus {
    /// Symlink exists and resolves to `target`.
    Installed { target: PathBuf },
    /// Symlink does not exist.
    NotInstalled,
    /// Symlink exists but is broken or cannot be read.
    BrokenLink { reason: String },
}

/// Return the expected path to the shim symlink: `$HOME/.local/share/git-prism/bin/git`.
fn path_shim_link(home: &Path) -> PathBuf {
    home.join(PATH_SHIM_REL_DIR).join(PATH_SHIM_LINK_NAME)
}

/// Create `~/.local/share/git-prism/bin/git` as a symlink pointing at the
/// running `git-prism` binary.
///
/// Idempotent: if the symlink already exists and points at the current binary,
/// this is a no-op. Returns the symlink path so callers can report it.
///
/// If a regular file (not a symlink) already exists at the target path, this
/// function returns an error rather than overwriting it — unless `force` is
/// `true`, in which case the file is removed and the symlink is created.
pub fn install_path_shim(home: &Path, force: bool) -> Result<PathBuf> {
    let shim_dir = home.join(PATH_SHIM_REL_DIR);
    std::fs::create_dir_all(&shim_dir)
        .with_context(|| format!("failed to create {}", shim_dir.display()))?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = std::fs::Permissions::from_mode(0o755);
        std::fs::set_permissions(&shim_dir, perms)
            .with_context(|| format!("failed to chmod {}", shim_dir.display()))?;
    }

    let current_exe = std::env::current_exe()
        .context("failed to resolve current executable path")?
        .canonicalize()
        .context("failed to canonicalize current executable path")?;

    let link = path_shim_link(home);

    if link.exists() || link.is_symlink() {
        let meta = link
            .symlink_metadata()
            .with_context(|| format!("failed to stat {}", link.display()))?;

        if !meta.file_type().is_symlink() {
            if !force {
                anyhow::bail!(
                    "{} is a regular file, not a symlink; remove it manually or re-run with --force",
                    link.display()
                );
            }
            std::fs::remove_file(&link)
                .with_context(|| format!("failed to remove existing file {}", link.display()))?;
        } else {
            match std::fs::read_link(&link) {
                Ok(existing_target) => {
                    if existing_target == current_exe {
                        return Ok(link);
                    }
                    std::fs::remove_file(&link).with_context(|| {
                        format!("failed to remove stale symlink {}", link.display())
                    })?;
                }
                Err(e) => {
                    std::fs::remove_file(&link).with_context(|| {
                        format!("failed to remove broken symlink {}: {e}", link.display())
                    })?;
                }
            }
        }
    }

    #[cfg(unix)]
    std::os::unix::fs::symlink(&current_exe, &link)
        .with_context(|| format!("failed to create symlink {}", link.display()))?;

    #[cfg(not(unix))]
    anyhow::bail!("path-shim install is not supported on non-Unix platforms");

    Ok(link)
}

/// Remove `~/.local/share/git-prism/bin/git` if it exists, then remove the
/// parent directory if it is empty.
///
/// Only removes the path when it is a symlink pointing at the running
/// `git-prism` binary. If the path is a regular file (not owned by us) this
/// function returns an error rather than silently deleting it.
pub fn uninstall_path_shim(home: &Path) -> Result<()> {
    let link = path_shim_link(home);

    if link.exists() || link.is_symlink() {
        let meta = link
            .symlink_metadata()
            .with_context(|| format!("failed to stat {}", link.display()))?;

        if !meta.file_type().is_symlink() {
            anyhow::bail!(
                "{} is a regular file, not a symlink managed by git-prism; \
                 remove it manually if you want to clean up this path",
                link.display()
            );
        }

        let current_exe = std::env::current_exe()
            .context("failed to resolve current executable path")?
            .canonicalize()
            .context("failed to canonicalize current executable path")?;

        match std::fs::read_link(&link) {
            Ok(target) => {
                let canonical_target = target.canonicalize().unwrap_or(target);
                if canonical_target != current_exe {
                    anyhow::bail!(
                        "{} is a symlink pointing at {} which is not the running git-prism binary ({}); \
                         remove it manually if you want to clean up this path",
                        link.display(),
                        canonical_target.display(),
                        current_exe.display()
                    );
                }
            }
            Err(_) => {
                // Broken symlink — safe to remove (target doesn't exist anyway).
            }
        }

        std::fs::remove_file(&link)
            .with_context(|| format!("failed to remove symlink {}", link.display()))?;
    }

    let shim_dir = home.join(PATH_SHIM_REL_DIR);
    if shim_dir.exists() {
        let is_empty = shim_dir
            .read_dir()
            .with_context(|| format!("failed to read directory {}", shim_dir.display()))?
            .next()
            .is_none();
        if is_empty {
            std::fs::remove_dir(&shim_dir)
                .with_context(|| format!("failed to remove directory {}", shim_dir.display()))?;
        }
    }

    Ok(())
}

/// Query the current state of the path-shim symlink without modifying anything.
pub fn path_shim_status(home: &Path) -> PathShimStatus {
    let link = path_shim_link(home);
    if !link.exists() && !link.is_symlink() {
        return PathShimStatus::NotInstalled;
    }
    match std::fs::read_link(&link) {
        Ok(target) => {
            let resolved = if target.is_absolute() {
                target.clone()
            } else {
                link.parent()
                    .map(|p| p.join(&target))
                    .unwrap_or_else(|| target.clone())
            };
            if resolved.exists() {
                PathShimStatus::Installed { target }
            } else {
                PathShimStatus::BrokenLink {
                    reason: format!("symlink target does not exist: {}", target.display()),
                }
            }
        }
        Err(e) => PathShimStatus::BrokenLink {
            reason: e.to_string(),
        },
    }
}

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

    // -------------------------------------------------------------------------
    // path_shim_status
    // -------------------------------------------------------------------------

    #[test]
    fn path_shim_status_returns_not_installed_when_symlink_absent() {
        let dir = TempDir::new().unwrap();
        let status = path_shim_status(dir.path());
        assert_eq!(status, PathShimStatus::NotInstalled);
    }

    // -------------------------------------------------------------------------
    // install_path_shim
    // -------------------------------------------------------------------------

    #[test]
    #[cfg(unix)]
    fn install_path_shim_creates_symlink_in_expected_location() {
        let dir = TempDir::new().unwrap();
        let home = dir.path();

        install_path_shim(home, false).unwrap();

        let link = home.join(".local/share/git-prism/bin/git");
        assert!(
            link.is_symlink(),
            "expected a symlink at {}",
            link.display()
        );
    }

    #[test]
    #[cfg(unix)]
    fn install_path_shim_is_idempotent() {
        let dir = TempDir::new().unwrap();
        let home = dir.path();

        install_path_shim(home, false).unwrap();
        install_path_shim(home, false).unwrap();

        let link = home.join(".local/share/git-prism/bin/git");
        assert!(
            link.is_symlink(),
            "symlink must still exist after second install"
        );
    }

    #[test]
    #[cfg(unix)]
    fn install_path_shim_replaces_stale_symlink() {
        let dir = TempDir::new().unwrap();
        let home = dir.path();

        let shim_dir = home.join(".local/share/git-prism/bin");
        std::fs::create_dir_all(&shim_dir).unwrap();
        let link = shim_dir.join("git");
        std::os::unix::fs::symlink("/nonexistent/old-binary", &link).unwrap();

        install_path_shim(home, false).unwrap();

        assert!(link.is_symlink());
        let target = std::fs::read_link(&link).unwrap();
        assert_ne!(target, std::path::Path::new("/nonexistent/old-binary"));
    }

    #[test]
    #[cfg(unix)]
    fn path_shim_status_returns_installed_after_install() {
        let dir = TempDir::new().unwrap();
        let home = dir.path();

        install_path_shim(home, false).unwrap();

        let status = path_shim_status(home);
        assert!(
            matches!(status, PathShimStatus::Installed { .. }),
            "expected Installed, got {status:?}"
        );
    }

    // -------------------------------------------------------------------------
    // uninstall_path_shim
    // -------------------------------------------------------------------------

    #[test]
    #[cfg(unix)]
    fn uninstall_path_shim_removes_symlink() {
        let dir = TempDir::new().unwrap();
        let home = dir.path();

        install_path_shim(home, false).unwrap();
        uninstall_path_shim(home).unwrap();

        let link = home.join(".local/share/git-prism/bin/git");
        assert!(!link.exists());
        assert!(!link.is_symlink());
    }

    #[test]
    #[cfg(unix)]
    fn uninstall_path_shim_removes_empty_bin_directory() {
        let dir = TempDir::new().unwrap();
        let home = dir.path();

        install_path_shim(home, false).unwrap();
        uninstall_path_shim(home).unwrap();

        let shim_dir = home.join(".local/share/git-prism/bin");
        assert!(!shim_dir.exists());
    }

    #[test]
    fn uninstall_path_shim_is_no_op_when_not_installed() {
        let dir = TempDir::new().unwrap();
        let home = dir.path();
        uninstall_path_shim(home).unwrap();
    }

    #[test]
    #[cfg(unix)]
    fn path_shim_status_returns_not_installed_after_uninstall() {
        let dir = TempDir::new().unwrap();
        let home = dir.path();

        install_path_shim(home, false).unwrap();
        uninstall_path_shim(home).unwrap();

        let status = path_shim_status(home);
        assert_eq!(status, PathShimStatus::NotInstalled);
    }

    // -------------------------------------------------------------------------
    // Adversarial QA — path-shim safety
    // -------------------------------------------------------------------------

    #[test]
    #[cfg(unix)]
    fn adversarial_uninstall_path_shim_must_not_delete_unrelated_regular_file() {
        let dir = TempDir::new().unwrap();
        let home = dir.path();
        let shim_dir = home.join(".local/share/git-prism/bin");
        std::fs::create_dir_all(&shim_dir).unwrap();
        let user_file = shim_dir.join("git");
        std::fs::write(&user_file, b"USER OWNS THIS FILE - DO NOT DELETE\n").unwrap();

        assert!(user_file.exists());
        assert!(!user_file.is_symlink());

        let _ = uninstall_path_shim(home);

        assert!(
            user_file.exists(),
            "uninstall_path_shim deleted an unrelated regular file at {}",
            user_file.display()
        );
    }

    #[test]
    #[cfg(unix)]
    fn adversarial_install_path_shim_must_not_overwrite_regular_file_without_force() {
        let dir = TempDir::new().unwrap();
        let home = dir.path();
        let shim_dir = home.join(".local/share/git-prism/bin");
        std::fs::create_dir_all(&shim_dir).unwrap();
        let user_file = shim_dir.join("git");
        let user_content = b"USER OWNS THIS REGULAR FILE\n";
        std::fs::write(&user_file, user_content).unwrap();

        let _ = install_path_shim(home, false);

        let after = std::fs::read(&user_file).unwrap_or_default();
        assert_eq!(
            after,
            user_content,
            "install_path_shim silently overwrote a regular file at {}",
            user_file.display()
        );
    }
}
