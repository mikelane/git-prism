//! `git-prism hooks` subcommand: install / uninstall / status the bundled
//! redirect hook into Claude Code's settings JSON.
//!
//! See ADR-0008 (`docs/decisions/0008-redirect-hook-architecture.md`) for the
//! decision rationale. The on-disk shape is:
//!
//! ```json
//! {
//!   "hooks": {
//!     "PreToolUse": [
//!       {
//!         "id": "git-prism-bash-redirect-v1",
//!         "matcher": "Bash",
//!         "command": "<abs-path-to-hooks-dir>/git-prism-redirect.sh"
//!       }
//!     ]
//!   }
//! }
//! ```
//!
//! Three scopes:
//! - `user`    -> `~/.claude/settings.json`         + `~/.claude/hooks/`
//! - `project` -> `<cwd>/.claude/settings.json`     + `<cwd>/.claude/hooks/`
//! - `local`   -> `<cwd>/.claude/settings.local.json` + `<cwd>/.claude/hooks/`
//!
//! Idempotency follows ADR-0008 paths 1-4 (fresh install, identical no-op,
//! stale path update, user-edited preserved unless `--force`). Downgrades
//! (binary writes v1 but a v2 entry already exists) are refused so an older
//! binary cannot silently regress a newer install.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use serde_json::{Map, Value, json};

/// Sentinel id this binary writes into `PreToolUse` entries. Bumping the
/// trailing version is how we communicate breaking changes in the hook's
/// on-disk contract; older binaries refuse to downgrade.
pub const SENTINEL_ID: &str = "git-prism-bash-redirect-v1";

/// Filename of the redirect launcher script that's exec'd by Claude Code.
pub const REDIRECT_SCRIPT_NAME: &str = "git-prism-redirect.sh";

/// Sibling Python helper invoked by the bash launcher.
pub const REDIRECT_PY_NAME: &str = "bash_redirect_hook.py";

/// Embedded copy of `hooks/git-prism-redirect.sh` written next to the
/// settings file at install time.
const REDIRECT_SH_CONTENT: &str = include_str!("../hooks/git-prism-redirect.sh");

/// Embedded copy of `hooks/bash_redirect_hook.py` — referenced by the bash
/// launcher via `${SCRIPT_DIR}/bash_redirect_hook.py`, so it must live in
/// the same hooks directory.
const REDIRECT_PY_CONTENT: &str = include_str!("../hooks/bash_redirect_hook.py");

/// Permissions applied to copied script files. Claude Code exec's these
/// directly, so the `+x` bits are load-bearing.
#[cfg(unix)]
const SCRIPT_MODE: u32 = 0o755;

/// Where on disk a given install scope lives, expanded against `$HOME` and
/// the current working directory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    User,
    Project,
    Local,
}

impl Scope {
    /// Parse the CLI argument value back into a `Scope`. Returned `Err`
    /// on unknown strings is propagated by clap's value parser hooks.
    pub fn parse(value: &str) -> Result<Self> {
        match value {
            "user" => Ok(Self::User),
            "project" => Ok(Self::Project),
            "local" => Ok(Self::Local),
            other => bail!("unknown scope {other:?} (expected user|project|local)"),
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

/// Resolved on-disk locations for a scope, computed once per command so
/// every code path agrees on which file/directory to read or write.
#[derive(Debug, Clone)]
pub struct ScopePaths {
    pub settings_file: PathBuf,
    pub hooks_dir: PathBuf,
}

impl ScopePaths {
    /// Resolve the settings file + hooks dir for a scope.
    ///
    /// `home` is treated as the user's `$HOME`. `cwd` is the project root for
    /// `project` and `local`. Tests inject both so they never touch the real
    /// home directory.
    pub fn resolve(scope: Scope, home: &Path, cwd: &Path) -> Self {
        match scope {
            Scope::User => {
                let claude = home.join(".claude");
                Self {
                    settings_file: claude.join("settings.json"),
                    hooks_dir: claude.join("hooks"),
                }
            }
            Scope::Project => {
                let claude = cwd.join(".claude");
                Self {
                    settings_file: claude.join("settings.json"),
                    hooks_dir: claude.join("hooks"),
                }
            }
            Scope::Local => {
                let claude = cwd.join(".claude");
                Self {
                    settings_file: claude.join("settings.local.json"),
                    hooks_dir: claude.join("hooks"),
                }
            }
        }
    }
}

/// The action taken (or simulated) by `install`. The CLI converts these into
/// stdout/stderr lines per the BDD scenarios.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InstallOutcome {
    /// Fresh install — settings file did not have our sentinel.
    Installed,
    /// Already had the canonical entry; nothing changed.
    AlreadyInstalled,
    /// Sentinel existed but command was a stale path; entry was overwritten.
    Updated,
    /// Sentinel existed with a user-edited command; preserved untouched.
    Skipped,
    /// Sentinel existed with a user-edited command; overwritten by --force.
    UpdatedForced,
    /// `--dry-run` short-circuit: nothing was written, JSON preview stored.
    DryRun(Value),
}

/// Knobs passed into `install_redirect_hook`. Everything is positional in
/// the CLI; this struct keeps the function signature small.
#[derive(Debug, Clone)]
pub struct InstallOptions {
    pub scope: Scope,
    pub dry_run: bool,
    pub force: bool,
}

/// Compose the canonical `PreToolUse` entry written by this binary.
fn canonical_entry(hooks_dir: &Path) -> Value {
    json!({
        "id": SENTINEL_ID,
        "matcher": "Bash",
        "command": hooks_dir.join(REDIRECT_SCRIPT_NAME).to_string_lossy(),
    })
}

/// Read the settings file at `path` if it exists. Returns an empty object
/// when the file is absent, on the assumption that an install creates the
/// file from scratch. Malformed JSON is a hard error — we refuse to
/// silently overwrite something we can't parse.
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

/// Find the index in `entries` of the first object whose `id` field matches
/// `id`. Returns `None` if no match is found.
fn find_entry_index(entries: &[Value], id: &str) -> Option<usize> {
    entries
        .iter()
        .position(|v| v.get("id").and_then(|s| s.as_str()) == Some(id))
}

/// Detect a higher-version sentinel (`git-prism-bash-redirect-v2`,
/// `...v3`, ...) so older binaries never silently downgrade a newer
/// install. We only know we own ids prefixed with `git-prism-bash-redirect-`,
/// so any version > 1 we see is treated as "newer".
fn newer_sentinel_id(entries: &[Value]) -> Option<String> {
    for entry in entries {
        let Some(id) = entry.get("id").and_then(|v| v.as_str()) else {
            continue;
        };
        if let Some(rest) = id.strip_prefix("git-prism-bash-redirect-v")
            && let Ok(version) = rest.parse::<u32>()
            && version > 1
        {
            return Some(id.to_string());
        }
    }
    None
}

/// Borrow (or insert) the `hooks.PreToolUse` array on `settings`, creating
/// missing intermediate objects as needed.
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

/// Write `settings` JSON to `path` with 2-space indentation. The parent
/// directory is created if it does not exist (Claude Code creates
/// `~/.claude` lazily, and the hooks dir for a fresh project is empty).
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

/// Copy the bundled redirect scripts into `hooks_dir`, creating the
/// directory if needed and marking the launcher executable on Unix.
fn copy_bundled_scripts(hooks_dir: &Path) -> Result<()> {
    std::fs::create_dir_all(hooks_dir)
        .with_context(|| format!("failed to create {}", hooks_dir.display()))?;

    let sh_path = hooks_dir.join(REDIRECT_SCRIPT_NAME);
    std::fs::write(&sh_path, REDIRECT_SH_CONTENT)
        .with_context(|| format!("failed to write {}", sh_path.display()))?;
    set_executable(&sh_path)?;

    let py_path = hooks_dir.join(REDIRECT_PY_NAME);
    std::fs::write(&py_path, REDIRECT_PY_CONTENT)
        .with_context(|| format!("failed to write {}", py_path.display()))?;
    set_executable(&py_path)?;

    Ok(())
}

#[cfg(unix)]
fn set_executable(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let perms = std::fs::Permissions::from_mode(SCRIPT_MODE);
    std::fs::set_permissions(path, perms)
        .with_context(|| format!("failed to chmod {}", path.display()))?;
    Ok(())
}

#[cfg(not(unix))]
fn set_executable(_path: &Path) -> Result<()> {
    Ok(())
}

/// True when `existing_command` is "ours but stale" — the path no longer
/// points to the current `hooks_dir` but the suffix still matches the
/// bundled launcher name. This catches users who moved `~/.claude` or
/// upgraded an install whose absolute path drifted.
fn is_stale_path(existing_command: &str, hooks_dir: &Path) -> bool {
    let canonical = hooks_dir
        .join(REDIRECT_SCRIPT_NAME)
        .to_string_lossy()
        .to_string();
    if existing_command == canonical {
        return false;
    }
    existing_command.ends_with(REDIRECT_SCRIPT_NAME)
}

/// Detect whether *any other* scope already has the canonical sentinel id.
/// Used to drive the cross-scope "duplicate redirects" prompt.
pub fn other_scopes_with_sentinel(scope: Scope, home: &Path, cwd: &Path) -> Vec<Scope> {
    let mut hits = Vec::new();
    for candidate in [Scope::User, Scope::Project, Scope::Local] {
        if candidate == scope {
            continue;
        }
        let paths = ScopePaths::resolve(candidate, home, cwd);
        let Ok(settings) = read_settings(&paths.settings_file) else {
            continue;
        };
        let entries = settings
            .get("hooks")
            .and_then(|h| h.get("PreToolUse"))
            .and_then(|p| p.as_array());
        if let Some(entries) = entries
            && find_entry_index(entries, SENTINEL_ID).is_some()
        {
            hits.push(candidate);
        }
    }
    hits
}

/// Compute the install outcome for a settings document and apply it.
///
/// This is the testable core: pure file IO is delegated to small helpers,
/// while `install_redirect_hook` (CLI entry) wires up `home`/`cwd` and
/// the cross-scope prompt. Splitting them lets the unit tests drive every
/// idempotency branch without invoking subprocesses.
pub fn plan_and_apply_install(
    settings_path: &Path,
    hooks_dir: &Path,
    options: &InstallOptions,
) -> Result<InstallOutcome> {
    let mut settings = read_settings(settings_path)?;
    {
        let entries = pretool_use_array_mut(&mut settings);
        if let Some(newer) = newer_sentinel_id(entries) {
            bail!(
                "{newer} already installed; this binary writes v1 — run `git-prism hooks uninstall` first"
            );
        }
    }

    let canonical = canonical_entry(hooks_dir);
    let canonical_command = canonical
        .get("command")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("canonical entry missing command field"))?
        .to_string();

    if options.dry_run {
        let entries = pretool_use_array_mut(&mut settings);
        match find_entry_index(entries, SENTINEL_ID) {
            Some(idx) => entries[idx] = canonical.clone(),
            None => entries.push(canonical.clone()),
        }
        return Ok(InstallOutcome::DryRun(settings));
    }

    let outcome = {
        let entries = pretool_use_array_mut(&mut settings);
        match find_entry_index(entries, SENTINEL_ID) {
            None => {
                entries.push(canonical);
                InstallOutcome::Installed
            }
            Some(idx) => {
                let existing_command = entries[idx]
                    .get("command")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                if existing_command == canonical_command {
                    return Ok(InstallOutcome::AlreadyInstalled);
                }
                if is_stale_path(&existing_command, hooks_dir) {
                    entries[idx] = canonical;
                    InstallOutcome::Updated
                } else if options.force {
                    entries[idx] = canonical;
                    InstallOutcome::UpdatedForced
                } else {
                    return Ok(InstallOutcome::Skipped);
                }
            }
        }
    };

    write_settings(settings_path, &settings)?;
    copy_bundled_scripts(hooks_dir)?;
    Ok(outcome)
}

/// Top-level CLI entry point for `git-prism hooks install`. Resolves the
/// scope's paths, optionally drives the cross-scope prompt, and prints the
/// per-outcome message expected by the BDD scenarios.
pub fn install_redirect_hook(
    options: &InstallOptions,
    home: &Path,
    cwd: &Path,
    stdin: &mut dyn std::io::Read,
    stdout: &mut dyn std::io::Write,
    stderr: &mut dyn std::io::Write,
) -> Result<i32> {
    let paths = ScopePaths::resolve(options.scope, home, cwd);

    if !options.dry_run
        && !options.force
        && let Some(other) = other_scopes_with_sentinel(options.scope, home, cwd).first()
    {
        writeln!(
            stderr,
            "Warning: redirect hook already installed at {} scope — duplicate redirects will fire on every Bash call. Continue? [y/N]",
            other.as_str()
        )?;
        let mut buf = [0u8; 1];
        let user_confirmation_char = match stdin.read(&mut buf)? {
            0 => 'n',
            _ => buf[0] as char,
        };
        if user_confirmation_char != 'y' && user_confirmation_char != 'Y' {
            return Ok(0);
        }
    }

    match plan_and_apply_install(&paths.settings_file, &paths.hooks_dir, options) {
        Ok(InstallOutcome::Installed) => {
            writeln!(
                stdout,
                "Installed git-prism redirect hook at {} scope",
                options.scope.as_str()
            )?;
            Ok(0)
        }
        Ok(InstallOutcome::AlreadyInstalled) => Ok(0),
        Ok(InstallOutcome::Updated) => {
            writeln!(
                stdout,
                "Updated git-prism redirect hook at {} scope (stale path replaced)",
                options.scope.as_str()
            )?;
            Ok(0)
        }
        Ok(InstallOutcome::Skipped) => {
            writeln!(
                stdout,
                "skipped: user-customized entry preserved; use --force to overwrite"
            )?;
            Ok(0)
        }
        Ok(InstallOutcome::UpdatedForced) => {
            writeln!(
                stdout,
                "Updated git-prism redirect hook at {} scope (--force overwrote user-edited entry)",
                options.scope.as_str()
            )?;
            Ok(0)
        }
        Ok(InstallOutcome::DryRun(preview)) => {
            writeln!(stdout, "{}", serde_json::to_string_pretty(&preview)?)?;
            Ok(0)
        }
        Err(err) => {
            writeln!(stderr, "{err:#}")?;
            Ok(1)
        }
    }
}

/// Remove every `PreToolUse` entry whose id starts with
/// `git-prism-bash-redirect-`. Other entries are left untouched.
pub fn uninstall_redirect_hook(scope: Scope, home: &Path, cwd: &Path) -> Result<()> {
    let paths = ScopePaths::resolve(scope, home, cwd);
    if !paths.settings_file.exists() {
        return Ok(());
    }
    let mut settings = read_settings(&paths.settings_file)?;
    let entry_count_before_removal = {
        let entries = pretool_use_array_mut(&mut settings);
        let len = entries.len();
        entries.retain(|entry| {
            let id = entry.get("id").and_then(|v| v.as_str()).unwrap_or("");
            !id.starts_with("git-prism-bash-redirect-")
        });
        len
    };
    let entry_count_after_removal = pretool_use_array_mut(&mut settings).len();
    if entry_count_after_removal < entry_count_before_removal {
        write_settings(&paths.settings_file, &settings)?;
    }
    Ok(())
}

/// Lines reported by `hooks status` — one per scope that has the sentinel,
/// or a single `not installed` line when nothing is found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatusReport {
    pub lines: Vec<String>,
}

/// Build the `hooks status` report by scanning all three scopes.
///
/// `cwd_is_repo` controls whether project/local scopes are considered:
/// when run outside a repo, scanning would walk into unrelated `.claude/`
/// directories and report misleading state.
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

    fn install_options(scope: Scope) -> InstallOptions {
        InstallOptions {
            scope,
            dry_run: false,
            force: false,
        }
    }

    fn read_json(path: &Path) -> Value {
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
    }

    #[test]
    fn fresh_install_writes_pretool_use_entry_with_sentinel_id() {
        let dir = TempDir::new().unwrap();
        let settings = dir.path().join("settings.json");
        let hooks = dir.path().join("hooks");

        let outcome =
            plan_and_apply_install(&settings, &hooks, &install_options(Scope::User)).unwrap();
        assert!(matches!(outcome, InstallOutcome::Installed));

        let data = read_json(&settings);
        let entries = data["hooks"]["PreToolUse"].as_array().unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0]["id"], json!(SENTINEL_ID));
        assert_eq!(entries[0]["matcher"], json!("Bash"));
        let cmd = entries[0]["command"].as_str().unwrap();
        assert!(cmd.ends_with(REDIRECT_SCRIPT_NAME), "command was {cmd:?}");
    }

    #[test]
    fn fresh_install_copies_bundled_scripts_and_marks_them_executable() {
        let dir = TempDir::new().unwrap();
        let settings = dir.path().join("settings.json");
        let hooks = dir.path().join("hooks");

        plan_and_apply_install(&settings, &hooks, &install_options(Scope::User)).unwrap();

        let sh = hooks.join(REDIRECT_SCRIPT_NAME);
        let py = hooks.join(REDIRECT_PY_NAME);
        assert!(sh.is_file(), "missing {}", sh.display());
        assert!(py.is_file(), "missing {}", py.display());

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(sh.metadata().unwrap().permissions().mode() & 0o777, 0o755);
            assert_eq!(py.metadata().unwrap().permissions().mode() & 0o777, 0o755);
        }
    }

    #[test]
    fn second_install_with_canonical_command_is_a_no_op() {
        let dir = TempDir::new().unwrap();
        let settings = dir.path().join("settings.json");
        let hooks = dir.path().join("hooks");

        plan_and_apply_install(&settings, &hooks, &install_options(Scope::User)).unwrap();
        let sha_before = sha256_of_file(&settings);

        let outcome =
            plan_and_apply_install(&settings, &hooks, &install_options(Scope::User)).unwrap();
        assert!(matches!(outcome, InstallOutcome::AlreadyInstalled));
        let sha_after = sha256_of_file(&settings);
        assert_eq!(sha_before, sha_after);

        let data = read_json(&settings);
        let entries = data["hooks"]["PreToolUse"].as_array().unwrap();
        assert_eq!(entries.len(), 1, "duplicate entry was appended on no-op");
    }

    #[test]
    fn install_with_stale_path_is_overwritten_in_place() {
        let dir = TempDir::new().unwrap();
        let settings = dir.path().join("settings.json");
        let hooks = dir.path().join("hooks");

        let stale = json!({
            "hooks": {
                "PreToolUse": [
                    {
                        "id": SENTINEL_ID,
                        "matcher": "Bash",
                        "command": "/old/stale/path/git-prism-redirect.sh"
                    }
                ]
            }
        });
        std::fs::create_dir_all(settings.parent().unwrap()).unwrap();
        std::fs::write(&settings, serde_json::to_string_pretty(&stale).unwrap()).unwrap();

        let outcome =
            plan_and_apply_install(&settings, &hooks, &install_options(Scope::User)).unwrap();
        assert!(matches!(outcome, InstallOutcome::Updated));

        let data = read_json(&settings);
        let entries = data["hooks"]["PreToolUse"].as_array().unwrap();
        assert_eq!(entries.len(), 1);
        let cmd = entries[0]["command"].as_str().unwrap();
        assert!(
            !cmd.contains("/old/stale/path"),
            "command still stale: {cmd}"
        );
    }

    #[test]
    fn user_edited_entry_is_preserved_without_force() {
        let dir = TempDir::new().unwrap();
        let settings = dir.path().join("settings.json");
        let hooks = dir.path().join("hooks");

        let edited = json!({
            "hooks": {
                "PreToolUse": [
                    {"id": SENTINEL_ID, "matcher": "Bash", "command": "echo HAND-EDITED"}
                ]
            }
        });
        std::fs::create_dir_all(settings.parent().unwrap()).unwrap();
        std::fs::write(&settings, serde_json::to_string_pretty(&edited).unwrap()).unwrap();

        let outcome =
            plan_and_apply_install(&settings, &hooks, &install_options(Scope::User)).unwrap();
        assert!(matches!(outcome, InstallOutcome::Skipped));

        let data = read_json(&settings);
        let entries = data["hooks"]["PreToolUse"].as_array().unwrap();
        assert_eq!(entries[0]["command"], json!("echo HAND-EDITED"));
    }

    #[test]
    fn user_edited_entry_is_overwritten_with_force() {
        let dir = TempDir::new().unwrap();
        let settings = dir.path().join("settings.json");
        let hooks = dir.path().join("hooks");

        let edited = json!({
            "hooks": {
                "PreToolUse": [
                    {"id": SENTINEL_ID, "matcher": "Bash", "command": "echo HAND-EDITED"}
                ]
            }
        });
        std::fs::create_dir_all(settings.parent().unwrap()).unwrap();
        std::fs::write(&settings, serde_json::to_string_pretty(&edited).unwrap()).unwrap();

        let mut options = install_options(Scope::User);
        options.force = true;
        let outcome = plan_and_apply_install(&settings, &hooks, &options).unwrap();
        assert!(matches!(outcome, InstallOutcome::UpdatedForced));

        let data = read_json(&settings);
        let entries = data["hooks"]["PreToolUse"].as_array().unwrap();
        let cmd = entries[0]["command"].as_str().unwrap();
        assert_ne!(cmd, "echo HAND-EDITED");
        assert!(cmd.contains(REDIRECT_SCRIPT_NAME));
    }

    #[test]
    fn install_refuses_to_downgrade_v2_to_v1() {
        let dir = TempDir::new().unwrap();
        let settings = dir.path().join("settings.json");
        let hooks = dir.path().join("hooks");

        let v2 = json!({
            "hooks": {
                "PreToolUse": [
                    {"id": "git-prism-bash-redirect-v2", "matcher": "Bash", "command": "echo v2"}
                ]
            }
        });
        std::fs::create_dir_all(settings.parent().unwrap()).unwrap();
        std::fs::write(&settings, serde_json::to_string_pretty(&v2).unwrap()).unwrap();

        let err = plan_and_apply_install(&settings, &hooks, &install_options(Scope::User))
            .expect_err("downgrade must be refused");
        let msg = err.to_string();
        assert!(msg.contains("v2"), "{msg}");
        assert!(msg.contains("uninstall"), "{msg}");

        // Settings are unchanged.
        let data = read_json(&settings);
        let entries = data["hooks"]["PreToolUse"].as_array().unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0]["id"], json!("git-prism-bash-redirect-v2"));
    }

    #[test]
    fn uninstall_removes_only_our_entries() {
        let dir = TempDir::new().unwrap();
        let home = dir.path();
        let settings = home.join(".claude").join("settings.json");
        let unrelated = json!({
            "hooks": {
                "PreToolUse": [
                    {"id": "user-custom-hook", "matcher": "Bash", "command": "echo unrelated"},
                    {"id": SENTINEL_ID, "matcher": "Bash", "command": "/abs/git-prism-redirect.sh"}
                ]
            }
        });
        std::fs::create_dir_all(settings.parent().unwrap()).unwrap();
        std::fs::write(&settings, serde_json::to_string_pretty(&unrelated).unwrap()).unwrap();

        uninstall_redirect_hook(Scope::User, home, home).unwrap();

        let data = read_json(&settings);
        let entries = data["hooks"]["PreToolUse"].as_array().unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0]["id"], json!("user-custom-hook"));
    }

    #[test]
    fn dry_run_does_not_write_settings_or_copy_scripts() {
        let dir = TempDir::new().unwrap();
        let settings = dir.path().join("settings.json");
        let hooks = dir.path().join("hooks");

        let mut options = install_options(Scope::User);
        options.dry_run = true;
        let outcome = plan_and_apply_install(&settings, &hooks, &options).unwrap();
        match outcome {
            InstallOutcome::DryRun(preview) => {
                let entries = preview["hooks"]["PreToolUse"].as_array().unwrap();
                assert_eq!(entries[0]["id"], json!(SENTINEL_ID));
            }
            other => panic!("expected DryRun, got {other:?}"),
        }
        assert!(
            !settings.exists(),
            "settings.json was written during dry-run"
        );
        assert!(!hooks.exists(), "hooks dir was created during dry-run");
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
        assert_eq!(local.hooks_dir, Path::new("/proj/.claude/hooks"));
    }

    #[test]
    fn status_reports_not_installed_when_no_settings_files_exist() {
        let dir = TempDir::new().unwrap();
        let home = dir.path();
        let cwd = home; // unused; cwd_is_repo=false
        let report = status_report(home, cwd, false).unwrap();
        assert_eq!(report.lines, vec!["not installed".to_string()]);
    }

    #[test]
    fn status_reports_user_scope_when_only_user_is_installed() {
        let dir = TempDir::new().unwrap();
        let home = dir.path();
        let settings = home.join(".claude").join("settings.json");
        let hooks = home.join(".claude").join("hooks");
        plan_and_apply_install(&settings, &hooks, &install_options(Scope::User)).unwrap();

        let report = status_report(home, home, false).unwrap();
        assert_eq!(report.lines, vec![format!("user: {SENTINEL_ID}")]);
    }

    #[test]
    fn status_reports_both_scopes_when_user_and_project_installed() {
        let home_dir = TempDir::new().unwrap();
        let proj_dir = TempDir::new().unwrap();
        let home = home_dir.path();
        let cwd = proj_dir.path();

        plan_and_apply_install(
            &home.join(".claude/settings.json"),
            &home.join(".claude/hooks"),
            &install_options(Scope::User),
        )
        .unwrap();
        plan_and_apply_install(
            &cwd.join(".claude/settings.json"),
            &cwd.join(".claude/hooks"),
            &install_options(Scope::Project),
        )
        .unwrap();

        let report = status_report(home, cwd, true).unwrap();
        // Assert exact count so a bug that emits duplicates cannot pass.
        assert_eq!(
            report.lines.len(),
            2,
            "expected exactly 2 status lines, got: {:?}",
            report.lines
        );
        assert!(
            report
                .lines
                .iter()
                .any(|l| l == &format!("user: {SENTINEL_ID}")),
            "user scope line missing from: {:?}",
            report.lines
        );
        assert!(
            report
                .lines
                .iter()
                .any(|l| l == &format!("project: {SENTINEL_ID}")),
            "project scope line missing from: {:?}",
            report.lines
        );
    }

    #[test]
    fn newer_sentinel_id_returns_v2_when_present() {
        let entries = vec![json!({"id": "git-prism-bash-redirect-v2", "command": "x"})];
        assert_eq!(
            newer_sentinel_id(&entries),
            Some("git-prism-bash-redirect-v2".to_string())
        );
    }

    #[test]
    fn newer_sentinel_id_returns_v3_when_present() {
        // Triangulates that the function parses the version digit rather than
        // matching the literal string "v2". A hardcoded match would return None here.
        let entries = vec![json!({"id": "git-prism-bash-redirect-v3", "command": "x"})];
        assert_eq!(
            newer_sentinel_id(&entries),
            Some("git-prism-bash-redirect-v3".to_string())
        );
    }

    #[test]
    fn newer_sentinel_id_returns_v10_when_present() {
        // Triangulates multi-digit version parsing: v10 > 1 but is not "v2".
        let entries = vec![json!({"id": "git-prism-bash-redirect-v10", "command": "x"})];
        assert_eq!(
            newer_sentinel_id(&entries),
            Some("git-prism-bash-redirect-v10".to_string())
        );
    }

    #[test]
    fn newer_sentinel_id_returns_none_for_v1_only() {
        let entries = vec![json!({"id": SENTINEL_ID, "command": "x"})];
        assert_eq!(newer_sentinel_id(&entries), None);
    }

    #[test]
    fn install_into_empty_settings_file_succeeds() {
        let dir = TempDir::new().unwrap();
        let settings = dir.path().join("settings.json");
        let hooks = dir.path().join("hooks");
        std::fs::create_dir_all(settings.parent().unwrap()).unwrap();
        std::fs::write(&settings, "").unwrap();

        let outcome =
            plan_and_apply_install(&settings, &hooks, &install_options(Scope::User)).unwrap();
        assert!(matches!(outcome, InstallOutcome::Installed));
    }

    #[test]
    fn install_preserves_existing_unrelated_pretool_use_entries() {
        let dir = TempDir::new().unwrap();
        let settings = dir.path().join("settings.json");
        let hooks = dir.path().join("hooks");

        let existing = json!({
            "hooks": {
                "PreToolUse": [
                    {"id": "user-custom-hook", "matcher": "Bash", "command": "echo unrelated"}
                ]
            }
        });
        std::fs::create_dir_all(settings.parent().unwrap()).unwrap();
        std::fs::write(&settings, serde_json::to_string_pretty(&existing).unwrap()).unwrap();

        plan_and_apply_install(&settings, &hooks, &install_options(Scope::User)).unwrap();

        let data = read_json(&settings);
        let entries = data["hooks"]["PreToolUse"].as_array().unwrap();
        assert_eq!(entries.len(), 2);
        let ids: Vec<_> = entries.iter().map(|e| e["id"].as_str().unwrap()).collect();
        assert!(ids.contains(&"user-custom-hook"));
        assert!(ids.contains(&SENTINEL_ID));
    }

    fn sha256_of_file(path: &Path) -> String {
        use sha2::{Digest, Sha256};
        let bytes = std::fs::read(path).unwrap();
        let mut h = Sha256::new();
        h.update(&bytes);
        format!("{:x}", h.finalize())
    }
}
