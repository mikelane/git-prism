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

/// The on-disk directory where the path-shim symlink is installed.
/// Relative to `$HOME`.
pub const PATH_SHIM_REL_DIR: &str = ".local/share/git-prism/bin";

/// Names of the shim symlinks managed inside `PATH_SHIM_REL_DIR`.
///
/// Both `git` and `gh` are intercepted by the running `git-prism` binary.
/// Install/status/uninstall iterate over every name in this slice.
pub const PATH_SHIM_LINK_NAMES: &[&str] = &["git", "gh"];

/// Status of the path-shim symlink reported by `hooks status`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PathShimStatus {
    /// Symlink exists and resolves to `target`.
    ///
    /// `staleness_warning` is `Some` when the target is version-pinned (contains
    /// a `Cellar` path component) and will break after `brew upgrade`. The user
    /// should re-run `git-prism shim install` to update the shim to a stable path.
    Installed {
        target: PathBuf,
        staleness_warning: Option<String>,
    },
    /// Symlink does not exist.
    NotInstalled,
    /// Symlink exists but is broken or cannot be read.
    BrokenLink { reason: String },
}

/// Return the expected path to a shim symlink: `$HOME/.local/share/git-prism/bin/<name>`.
fn path_shim_link(home: &Path, name: &str) -> PathBuf {
    home.join(PATH_SHIM_REL_DIR).join(name)
}

/// Given the canonicalized current-exe path, return a stable shim target.
///
/// If the path is inside a Homebrew Cellar
/// (`<prefix>/Cellar/<formula>/<version>/bin/<bin>`) and the stable linked
/// path `<prefix>/bin/<bin>` exists on disk, return that path — it survives
/// `brew upgrade` because Homebrew keeps `<prefix>/bin/<bin>` pointing at
/// the current Cellar version.
///
/// Otherwise return the input unchanged (cargo-install, source builds, and
/// non-existent stable paths all fall back to the canonical exe path).
///
/// Does NOT shell out to `brew`; the prefix is derived purely from path
/// structure. The Homebrew Cellar shape is validated strictly: the suffix
/// after `Cellar` must be exactly `<formula>/<version>/bin/<bin>` (four
/// components), so a stray directory named `Cellar` in a non-Homebrew path
/// is never mistaken for a Homebrew install.
pub(crate) fn stable_shim_target(canonical_exe: &Path) -> PathBuf {
    // Expected layout: <prefix>/Cellar/<formula>/<version>/bin/<bin>
    //
    // Validate the full suffix shape rather than just finding the first
    // `Cellar` component. `Cellar` must be exactly 4 components from the end,
    // with "bin" as the second-to-last component. This prevents a stray
    // directory named `Cellar` in a non-Homebrew path from being mistaken for
    // a Homebrew install and silently repointing the shim at an unrelated binary.
    let components: Vec<_> = canonical_exe.components().collect();
    let n = components.len();

    // Need at least: <root> Cellar <formula> <version> bin <bin> = 6 components.
    if n < 6 {
        return canonical_exe.to_path_buf();
    }

    // `Cellar` must sit at index n-5 (exactly 4 components follow it).
    let cellar_idx = n - 5;
    if components[cellar_idx].as_os_str() != "Cellar" {
        return canonical_exe.to_path_buf();
    }

    // The second-to-last component must be "bin" (index n-2).
    if components[n - 2].as_os_str() != "bin" {
        return canonical_exe.to_path_buf();
    }

    // The prefix is everything before "Cellar".
    let prefix: PathBuf = components[..cellar_idx].iter().collect();

    // The binary filename is the last component of the canonical exe.
    let bin_name = match canonical_exe.file_name() {
        Some(name) => name,
        None => return canonical_exe.to_path_buf(),
    };

    let stable = prefix.join("bin").join(bin_name);
    if stable.exists() {
        stable
    } else {
        canonical_exe.to_path_buf()
    }
}

/// Classify what kind of obstacle (if any) sits at `link` before an install.
///
/// Returns:
/// - `Ok(None)` — path is absent, proceed with creation.
/// - `Ok(Some(AlreadyOwned))` — symlink already points at `target`, skip.
/// - `Ok(Some(NeedsReplacement))` — stale/broken git-prism symlink or `force`
///   is true; safe to remove and recreate.
/// - `Err(_)` — foreign regular file or foreign symlink without `force`.
#[derive(Debug)]
enum InstallObstacle {
    AlreadyOwned,
    NeedsReplacement,
}

fn classify_install_obstacle(
    link: &Path,
    target: &Path,
    shim_dir: &Path,
    force: bool,
) -> Result<Option<InstallObstacle>> {
    if !link.exists() && !link.is_symlink() {
        return Ok(None);
    }

    let meta = link
        .symlink_metadata()
        .with_context(|| format!("failed to stat {}", link.display()))?;

    if !meta.file_type().is_symlink() {
        // Regular file — only allow clobber with --force.
        if !force {
            anyhow::bail!(
                "{} is a regular file, not a symlink; remove it manually or re-run with --force",
                link.display()
            );
        }
        return Ok(Some(InstallObstacle::NeedsReplacement));
    }

    // Symlink — inspect the target.
    match std::fs::read_link(link) {
        Ok(existing_target) => {
            if existing_target == target {
                // Already correct — idempotent skip.
                return Ok(Some(InstallObstacle::AlreadyOwned));
            }
            // A symlink pointing at something else.
            // If the target is dangling (canonicalize fails), it is safe to
            // replace unconditionally — the target is gone regardless of who
            // originally owned it.
            // If the target is live but not ours, it is a foreign symlink:
            // refuse without --force, matching how we treat regular files.
            match existing_target.canonicalize() {
                Ok(canonical) if canonical != *target => {
                    if !force {
                        anyhow::bail!(
                            "{} is a symlink pointing at {} which is not the git-prism \
                             shim target; remove it manually or re-run with --force",
                            link.display(),
                            existing_target.display()
                        );
                    }
                    Ok(Some(InstallObstacle::NeedsReplacement))
                }
                Ok(_) => {
                    // Canonical target matches — git-prism-owned, stale raw path.
                    Ok(Some(InstallObstacle::NeedsReplacement))
                }
                Err(_) => {
                    // Dangling symlink — target no longer exists on disk.
                    // Check raw target provenance: only treat the link as
                    // git-prism-owned (safe to replace) when its raw target
                    // is recognisably ours. A dangling link whose raw target
                    // is foreign is treated like a live foreign symlink —
                    // refuse without --force.
                    if !dangling_target_is_git_prism_owned(&existing_target, shim_dir) && !force {
                        anyhow::bail!(
                            "{} is a dangling symlink pointing at {} which is outside \
                             the git-prism managed directory; remove it manually or \
                             re-run with --force",
                            link.display(),
                            existing_target.display()
                        );
                    }
                    Ok(Some(InstallObstacle::NeedsReplacement))
                }
            }
        }
        Err(_) => {
            // Broken symlink — always safe to replace.
            Ok(Some(InstallObstacle::NeedsReplacement))
        }
    }
}

/// Create shim symlinks under `~/.local/share/git-prism/bin/` for every name
/// in [`PATH_SHIM_LINK_NAMES`] (`git` and `gh`), each pointing at the running
/// `git-prism` binary.
///
/// Idempotent: symlinks that already point at the current binary are left
/// unchanged. Returns the path of the first symlink (`git`) so callers can
/// report it.
///
/// If a regular file (not a symlink) or a foreign symlink already exists at
/// any target path, this function returns an error rather than overwriting it
/// — unless `force` is `true`. The pre-flight check runs for ALL names before
/// any filesystem mutation, so a refusal on `gh` never leaves `git` partially
/// installed.
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

    let canonical_exe = std::env::current_exe()
        .context("failed to resolve current executable path")?
        .canonicalize()
        .context("failed to canonicalize current executable path")?;

    // Use a stable path that survives `brew upgrade`. For Homebrew installs the
    // canonical exe is a version-pinned Cellar path that gets GC'd on upgrade;
    // `stable_shim_target` maps it to `<prefix>/bin/<bin>` which Homebrew keeps
    // up-to-date. For cargo-install / source builds the path is unchanged.
    let target = stable_shim_target(&canonical_exe);

    // Pre-flight: validate ALL names before mutating any.  A refusal on the
    // second name must not leave the first name half-installed.
    let obstacles: Vec<Option<InstallObstacle>> = PATH_SHIM_LINK_NAMES
        .iter()
        .map(|name| {
            classify_install_obstacle(&path_shim_link(home, name), &target, &shim_dir, force)
        })
        .collect::<Result<_>>()?;

    // Apply mutations only after all pre-flight checks passed.
    let mut first_link: Option<PathBuf> = None;

    for (name, obstacle) in PATH_SHIM_LINK_NAMES.iter().zip(obstacles) {
        let link = path_shim_link(home, name);

        match obstacle {
            Some(InstallObstacle::AlreadyOwned) => {
                if first_link.is_none() {
                    first_link = Some(link);
                }
                continue;
            }
            Some(InstallObstacle::NeedsReplacement) => {
                std::fs::remove_file(&link).with_context(|| {
                    format!("failed to remove existing path {}", link.display())
                })?;
            }
            None => {}
        }

        #[cfg(unix)]
        std::os::unix::fs::symlink(&target, &link)
            .with_context(|| format!("failed to create symlink {}", link.display()))?;

        #[cfg(not(unix))]
        anyhow::bail!("path-shim install is not supported on non-Unix platforms");

        if first_link.is_none() {
            first_link = Some(link);
        }
    }

    Ok(first_link.unwrap_or_else(|| path_shim_link(home, PATH_SHIM_LINK_NAMES[0])))
}

/// Classify whether a shim link is safe to remove during uninstall.
///
/// Returns `Ok(true)` when the link should be removed, `Ok(false)` when it is
/// absent (nothing to do), and `Err` when it exists but is not git-prism-owned.
fn is_uninstallable_shim(link: &Path, current_exe: &Path, shim_dir: &Path) -> Result<bool> {
    if !link.exists() && !link.is_symlink() {
        return Ok(false);
    }

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

    match std::fs::read_link(link) {
        Ok(raw_target) => {
            // Try to canonicalize the target.  For a live symlink this resolves
            // the real path; for a dangling symlink canonicalize fails.
            match raw_target.canonicalize() {
                Ok(canonical_target) => {
                    if canonical_target != current_exe {
                        anyhow::bail!(
                            "{} is a symlink pointing at {} which is not the running \
                             git-prism binary ({}); remove it manually if you want to \
                             clean up this path",
                            link.display(),
                            canonical_target.display(),
                            current_exe.display()
                        );
                    }
                    // Canonical target matches — git-prism-owned.
                    Ok(true)
                }
                Err(_) => {
                    // Dangling symlink: the target no longer exists on disk.
                    // Check raw target provenance: only remove the link when
                    // its raw target is recognisably git-prism-owned (the
                    // brew-upgrade GC scenario). A dangling link whose raw
                    // target is foreign is treated like a live foreign
                    // symlink — refuse to remove it.
                    debug_assert!(
                        link.parent() == Some(shim_dir),
                        "is_uninstallable_shim called for a link outside the managed shim dir"
                    );
                    if !dangling_target_is_git_prism_owned(&raw_target, shim_dir) {
                        anyhow::bail!(
                            "{} is a dangling symlink pointing at {} which is outside \
                             the git-prism managed directory; remove it manually if \
                             you want to clean up this path",
                            link.display(),
                            raw_target.display()
                        );
                    }
                    Ok(true)
                }
            }
        }
        Err(e) => {
            // read_link itself failed — treat as broken/dangling, same as above.
            // Surface the underlying errno so that if the subsequent remove_file
            // also fails, the user has the original cause (e.g. EACCES on the
            // parent dir) instead of a bare "broken link" assumption with no trail.
            eprintln!(
                "git-prism: could not read symlink {} ({e}); treating as broken and removing",
                link.display()
            );
            Ok(true)
        }
    }
}

/// Remove all shim symlinks under `~/.local/share/git-prism/bin/` (one per
/// entry in [`PATH_SHIM_LINK_NAMES`]), then remove the parent directory if it
/// is empty.
///
/// Only removes a path when it is a symlink pointing at the running
/// `git-prism` binary, or a dangling symlink inside the managed bin directory
/// (which git-prism itself created — the post-`brew upgrade` GC scenario).
/// Regular files and foreign symlinks cause an error rather than silent deletion.
///
/// The pre-flight check validates ALL names before any filesystem mutation so
/// that a refusal on one name never leaves sibling links in a partially-removed
/// state.
pub fn uninstall_path_shim(home: &Path) -> Result<()> {
    let current_exe = std::env::current_exe()
        .context("failed to resolve current executable path")?
        .canonicalize()
        .context("failed to canonicalize current executable path")?;

    let shim_dir = home.join(PATH_SHIM_REL_DIR);

    // Pre-flight: validate every name before removing any.
    let should_remove: Vec<bool> = PATH_SHIM_LINK_NAMES
        .iter()
        .map(|name| is_uninstallable_shim(&path_shim_link(home, name), &current_exe, &shim_dir))
        .collect::<Result<_>>()?;

    // Apply removals only after all checks passed.
    for (name, remove) in PATH_SHIM_LINK_NAMES.iter().zip(should_remove) {
        if remove {
            let link = path_shim_link(home, name);
            std::fs::remove_file(&link)
                .with_context(|| format!("failed to remove symlink {}", link.display()))?;
        }
    }

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

/// Return true if `path` contains a `Cellar` component, indicating it is a
/// version-pinned Homebrew Cellar path that will break after `brew upgrade`.
fn path_contains_cellar_component(path: &Path) -> bool {
    path.components().any(|c| c.as_os_str() == "Cellar")
}

/// Return true when a dangling symlink's raw (unresolved) `target` is
/// recognisably owned by git-prism and therefore safe to remove or replace.
///
/// A target is git-prism-owned when it either:
/// - Lives inside `shim_dir` itself (e.g. a relative shim pointing back into
///   the managed bin directory), or
/// - Contains a `git-prism` path component (covers `brew`-installed Cellar
///   paths such as `.../Cellar/git-prism/0.9.0/bin/git-prism` that get GC'd
///   after `brew upgrade`).
///
/// Any other dangling target (e.g. a user's own wrapper that has since been
/// moved) is treated as a foreign symlink that must not be touched without
/// explicit user action.
fn dangling_target_is_git_prism_owned(raw_target: &Path, shim_dir: &Path) -> bool {
    raw_target.starts_with(shim_dir)
        || raw_target
            .components()
            .any(|c| c.as_os_str() == "git-prism")
}

/// The advisory message shown when a shim target is a version-pinned Cellar path.
pub(crate) const CELLAR_STALENESS_ADVISORY: &str = "shim points at a version-pinned Homebrew Cellar path; \
     run `git-prism shim install` to update to a stable path \
     that survives `brew upgrade`";

/// Query the current state of the `git` shim symlink without modifying anything.
///
/// This is a convenience wrapper around [`path_shim_status_for`] for the `git`
/// symlink, used by tests and the legacy CLI path.
pub fn path_shim_status(home: &Path) -> PathShimStatus {
    path_shim_status_for(home, PATH_SHIM_LINK_NAMES[0])
}

/// Query the current state of the named shim symlink without modifying anything.
pub fn path_shim_status_for(home: &Path, name: &str) -> PathShimStatus {
    let link = path_shim_link(home, name);
    if !link.exists() && !link.is_symlink() {
        return PathShimStatus::NotInstalled;
    }
    match std::fs::read_link(&link) {
        Ok(target) => {
            // Check for a Cellar-pinned target BEFORE resolving existence.
            // A dangling Cellar target (brew upgrade GC\'d the old version) is
            // the primary scenario this fix addresses; the advisory must fire
            // even when the target no longer exists on disk.
            let cellar_pinned = path_contains_cellar_component(&target);

            let resolved = if target.is_absolute() {
                target.clone()
            } else {
                link.parent()
                    .map(|p| p.join(&target))
                    .unwrap_or_else(|| target.clone())
            };

            if resolved.exists() {
                let staleness_warning = if cellar_pinned {
                    Some(CELLAR_STALENESS_ADVISORY.to_string())
                } else {
                    None
                };
                PathShimStatus::Installed {
                    target,
                    staleness_warning,
                }
            } else if cellar_pinned {
                // Dangling Cellar target: the exact post-upgrade scenario.
                // Surface as BrokenLink but include the reinstall advisory so
                // the user knows the remedy, not just the broken path.
                PathShimStatus::BrokenLink {
                    reason: format!(
                        "symlink target does not exist: {} — {}",
                        target.display(),
                        CELLAR_STALENESS_ADVISORY
                    ),
                }
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
        // is_symlink() alone passes even for a symlink pointing at nowhere.
        // The link must point at the running git-prism binary.
        assert_eq!(
            std::fs::read_link(&link).unwrap(),
            expected_shim_target(),
            "git symlink must point at the git-prism binary"
        );
    }

    #[test]
    #[cfg(unix)]
    fn install_path_shim_is_idempotent() {
        let dir = TempDir::new().unwrap();
        let home = dir.path();

        install_path_shim(home, false).unwrap();
        let link = home.join(".local/share/git-prism/bin/git");
        let target_before = std::fs::read_link(&link).unwrap();

        install_path_shim(home, false).unwrap();

        assert!(
            link.is_symlink(),
            "symlink must still exist after second install"
        );
        // Idempotency means the target is UNCHANGED, not merely that some
        // symlink survives. A second install must leave the git target alone.
        let target_after = std::fs::read_link(&link).unwrap();
        assert_eq!(
            target_before, target_after,
            "idempotent re-install must not change the git symlink target"
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
        // Simulate a previously-installed git-prism shim whose target was GC'd
        // by `brew upgrade` — the raw target contains "git-prism" in its path,
        // making it recognisably ours.  The dangling-ownership check must allow
        // this to be replaced without --force.
        let stale_target = "/nonexistent/Cellar/git-prism/0.8.0/bin/git-prism";
        std::os::unix::fs::symlink(stale_target, &link).unwrap();

        install_path_shim(home, false).unwrap();

        assert!(link.is_symlink());
        let target = std::fs::read_link(&link).unwrap();
        assert_ne!(target, std::path::Path::new(stale_target));
        // assert_ne! alone is satisfied by ANY non-stale target (even garbage).
        // Pin it: the replacement must point at the running git-prism binary,
        // i.e. exactly the target install_path_shim computes for a fresh install.
        let expected = expected_shim_target();
        assert_eq!(
            target, expected,
            "stale replacement must repoint at the current git-prism binary, not just away from the stale path"
        );
    }

    /// The shim target `install_path_shim` writes for a fresh install: the
    /// canonicalized current executable, mapped through `stable_shim_target`.
    /// Tests use this to assert the symlink points at the RIGHT binary, not
    /// merely that it points somewhere (which a `assert_ne!` against a stale
    /// path would tolerate even for a garbage target).
    #[cfg(unix)]
    fn expected_shim_target() -> PathBuf {
        let canonical = std::env::current_exe()
            .expect("current_exe")
            .canonicalize()
            .expect("canonicalize current_exe");
        stable_shim_target(&canonical)
    }

    #[test]
    #[cfg(unix)]
    fn path_shim_status_returns_installed_after_install() {
        let dir = TempDir::new().unwrap();
        let home = dir.path();

        install_path_shim(home, false).unwrap();

        let status = path_shim_status(home);
        // Don't just match the variant — verify the REPORTED target (the CLI
        // surfaces this) is the real binary and that a fresh install carries no
        // spurious staleness warning.
        match status {
            PathShimStatus::Installed {
                target,
                staleness_warning,
            } => {
                assert_eq!(
                    target,
                    expected_shim_target(),
                    "status must report the actual shim target"
                );
                assert!(
                    staleness_warning.is_none(),
                    "a fresh non-Cellar install must not carry a staleness warning; got {staleness_warning:?}"
                );
            }
            other => panic!("expected Installed, got {other:?}"),
        }
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
    // path_shim_status staleness warning
    // -------------------------------------------------------------------------

    #[test]
    #[cfg(unix)]
    fn path_shim_status_warns_when_target_is_cellar_path() {
        // A symlink pointing into a Cellar directory is version-pinned and will
        // break after `brew upgrade`. The status must include a staleness advisory.
        let dir = TempDir::new().unwrap();
        let home = dir.path();

        // Create a fake Cellar target so the symlink is not dangling.
        let cellar_bin = home.join("Cellar/git-prism/1.0.0/bin");
        std::fs::create_dir_all(&cellar_bin).unwrap();
        let cellar_exe = cellar_bin.join("git-prism");
        std::fs::write(&cellar_exe, b"binary").unwrap();

        // Install the shim pointing directly at the Cellar path.
        let shim_dir = home.join(PATH_SHIM_REL_DIR);
        std::fs::create_dir_all(&shim_dir).unwrap();
        let link = shim_dir.join(PATH_SHIM_LINK_NAMES[0]);
        std::os::unix::fs::symlink(&cellar_exe, &link).unwrap();

        let status = path_shim_status(home);
        match status {
            PathShimStatus::Installed {
                staleness_warning: Some(ref w),
                ..
            } => {
                assert_eq!(
                    w, CELLAR_STALENESS_ADVISORY,
                    "staleness warning must equal the canonical advisory string"
                );
            }
            other => panic!("expected Installed with exact staleness warning; got {other:?}"),
        }
    }

    #[test]
    #[cfg(unix)]
    fn path_shim_status_no_warning_for_stable_target() {
        // A symlink pointing to a stable (non-Cellar) path needs no advisory.
        let dir = TempDir::new().unwrap();
        let home = dir.path();

        let stable_bin = home.join("bin");
        std::fs::create_dir_all(&stable_bin).unwrap();
        let stable_exe = stable_bin.join("git-prism");
        std::fs::write(&stable_exe, b"binary").unwrap();

        let shim_dir = home.join(PATH_SHIM_REL_DIR);
        std::fs::create_dir_all(&shim_dir).unwrap();
        let link = shim_dir.join(PATH_SHIM_LINK_NAMES[0]);
        std::os::unix::fs::symlink(&stable_exe, &link).unwrap();

        let status = path_shim_status(home);
        assert!(
            matches!(status, PathShimStatus::Installed { ref staleness_warning, .. } if staleness_warning.is_none()),
            "expected Installed with no staleness warning for stable target; got {status:?}"
        );
    }

    // -------------------------------------------------------------------------
    // stable_shim_target
    // -------------------------------------------------------------------------

    #[test]
    #[cfg(unix)]
    fn stable_shim_target_maps_homebrew_cellar_exe_to_prefix_bin() {
        // Build a fake Homebrew layout:
        //   <tmp>/Cellar/git-prism/1.0.0/bin/git-prism  (the canonical exe)
        //   <tmp>/bin/git-prism                          (the stable linked path)
        let dir = TempDir::new().unwrap();
        let prefix = dir.path();

        let cellar_bin = prefix.join("Cellar/git-prism/1.0.0/bin");
        std::fs::create_dir_all(&cellar_bin).unwrap();
        let cellar_exe = cellar_bin.join("git-prism");
        std::fs::write(&cellar_exe, b"fake binary").unwrap();

        let stable_bin = prefix.join("bin");
        std::fs::create_dir_all(&stable_bin).unwrap();
        let stable_exe = stable_bin.join("git-prism");
        std::fs::write(&stable_exe, b"fake binary").unwrap();

        let result = stable_shim_target(&cellar_exe);
        assert_eq!(
            result, stable_exe,
            "Homebrew Cellar exe should map to <prefix>/bin/<binary>"
        );
    }

    #[test]
    #[cfg(unix)]
    fn stable_shim_target_survives_brew_upgrade() {
        // Simulate a brew upgrade: the stable bin symlink is repointed from
        // 1.0.0 to 1.1.0. A shim installed targeting <prefix>/bin/git-prism
        // still resolves correctly after the upgrade.
        let dir = TempDir::new().unwrap();
        let prefix = dir.path();

        // v1.0.0 layout
        let cellar_bin_100 = prefix.join("Cellar/git-prism/1.0.0/bin");
        std::fs::create_dir_all(&cellar_bin_100).unwrap();
        let cellar_exe_100 = cellar_bin_100.join("git-prism");
        std::fs::write(&cellar_exe_100, b"v1.0.0 binary").unwrap();

        // v1.1.0 layout (post-upgrade)
        let cellar_bin_110 = prefix.join("Cellar/git-prism/1.1.0/bin");
        std::fs::create_dir_all(&cellar_bin_110).unwrap();
        let cellar_exe_110 = cellar_bin_110.join("git-prism");
        std::fs::write(&cellar_exe_110, b"v1.1.0 binary").unwrap();

        // stable <prefix>/bin/git-prism initially points at 1.0.0
        let stable_bin = prefix.join("bin");
        std::fs::create_dir_all(&stable_bin).unwrap();
        let stable_exe = stable_bin.join("git-prism");
        std::os::unix::fs::symlink(&cellar_exe_100, &stable_exe).unwrap();

        // Before upgrade: stable_shim_target on 1.0.0 returns the stable path
        let target_before = stable_shim_target(&cellar_exe_100);
        assert_eq!(target_before, stable_exe);

        // Simulate brew upgrade: repoint stable symlink to 1.1.0
        std::fs::remove_file(&stable_exe).unwrap();
        std::os::unix::fs::symlink(&cellar_exe_110, &stable_exe).unwrap();

        // The install-time target (`target_before`) must re-resolve to the
        // post-upgrade Cellar exe, proving a stable-path shim automatically
        // follows `brew upgrade` without reinstalling.
        // Canonicalize both sides: on macOS /tmp is a symlink to /private/tmp.
        let resolved = std::fs::canonicalize(&target_before).unwrap();
        let expected = std::fs::canonicalize(&cellar_exe_110).unwrap();
        assert_eq!(
            resolved, expected,
            "shim target chosen at install time must re-resolve to the post-upgrade version"
        );
    }

    #[test]
    fn stable_shim_target_returns_input_for_non_homebrew_path() {
        // A cargo-install path has no Cellar component — must be returned unchanged.
        let dir = TempDir::new().unwrap();
        let cargo_bin = dir.path().join(".cargo/bin");
        std::fs::create_dir_all(&cargo_bin).unwrap();
        let exe = cargo_bin.join("git-prism");
        std::fs::write(&exe, b"binary").unwrap();

        let result = stable_shim_target(&exe);
        assert_eq!(
            result, exe,
            "non-Homebrew path should be returned unchanged"
        );
    }

    #[test]
    #[cfg(unix)]
    fn stable_shim_target_falls_back_when_stable_bin_does_not_exist() {
        // Cellar layout exists but <prefix>/bin/git-prism does NOT exist.
        // Must fall back to the canonical Cellar path rather than pointing at a phantom.
        let dir = TempDir::new().unwrap();
        let prefix = dir.path();

        let cellar_bin = prefix.join("Cellar/git-prism/1.0.0/bin");
        std::fs::create_dir_all(&cellar_bin).unwrap();
        let cellar_exe = cellar_bin.join("git-prism");
        std::fs::write(&cellar_exe, b"binary").unwrap();

        // Intentionally do NOT create <prefix>/bin/git-prism
        let result = stable_shim_target(&cellar_exe);
        assert_eq!(
            result, cellar_exe,
            "should fall back to canonical exe when stable bin does not exist"
        );
    }

    // -------------------------------------------------------------------------
    // stable_shim_target — positional boundary unit tests (hand-built paths,
    // no canonicalize, so the arithmetic is tested directly)
    // -------------------------------------------------------------------------

    #[test]
    #[cfg(unix)]
    fn cellar_at_n_minus_6_via_libexec_is_not_rewritten() {
        // Layout: <root>/Cellar/<formula>/<version>/libexec/bin/<bin>
        // Cellar sits at n-6 (5 trailing components), NOT n-5.
        // A trap <root>/bin/<bin> exists to catch a false rewrite.
        let dir = tempfile::TempDir::new().unwrap();
        let r = dir.path();
        let cellar_exe = r.join("Cellar/git-prism/1.0.0/libexec/bin/git-prism");
        std::fs::create_dir_all(cellar_exe.parent().unwrap()).unwrap();
        std::fs::write(&cellar_exe, b"binary").unwrap();
        let trap = r.join("bin/git-prism");
        std::fs::create_dir_all(trap.parent().unwrap()).unwrap();
        std::fs::write(&trap, b"trap").unwrap();

        let result = stable_shim_target(&cellar_exe);
        assert_eq!(
            result, cellar_exe,
            "Cellar at n-6 (libexec/bin layout) must not be rewritten; got {result:?}"
        );
    }

    #[test]
    #[cfg(unix)]
    fn cellar_at_n_minus_5_with_sbin_not_bin_is_not_rewritten() {
        // Layout: <root>/Cellar/<formula>/<version>/sbin/<bin>
        // Cellar is at n-5 but "sbin" ≠ "bin" at n-2.
        let dir = tempfile::TempDir::new().unwrap();
        let r = dir.path();
        let cellar_exe = r.join("Cellar/git-prism/1.0.0/sbin/git-prism");
        std::fs::create_dir_all(cellar_exe.parent().unwrap()).unwrap();
        std::fs::write(&cellar_exe, b"binary").unwrap();
        let trap = r.join("bin/git-prism");
        std::fs::create_dir_all(trap.parent().unwrap()).unwrap();
        std::fs::write(&trap, b"trap").unwrap();

        let result = stable_shim_target(&cellar_exe);
        assert_eq!(
            result, cellar_exe,
            "sbin (not bin) at n-2 must not be rewritten; got {result:?}"
        );
    }

    #[test]
    fn path_with_fewer_than_six_components_is_not_rewritten() {
        // A path with exactly 5 components hits the n<6 guard.
        // Hand-built as an absolute path so we control component count exactly.
        // Layout: /a/Cellar/x/bin/git-prism — 5 components total.
        let short = std::path::PathBuf::from("/a/Cellar/x/bin/git-prism");
        let result = stable_shim_target(&short);
        assert_eq!(
            result, short,
            "path with n<6 components must be returned unchanged; got {result:?}"
        );
    }

    // -------------------------------------------------------------------------
    // gh symlink — install/status/uninstall manage both git and gh
    // -------------------------------------------------------------------------

    #[test]
    #[cfg(unix)]
    fn install_path_shim_creates_gh_symlink() {
        let dir = TempDir::new().unwrap();
        let home = dir.path();

        install_path_shim(home, false).unwrap();

        let link = home.join(".local/share/git-prism/bin/gh");
        assert!(
            link.is_symlink(),
            "expected a gh symlink at {}",
            link.display()
        );
        // is_symlink() alone passes even for a symlink pointing at nowhere.
        // The link must point at the running git-prism binary.
        assert_eq!(
            std::fs::read_link(&link).unwrap(),
            expected_shim_target(),
            "gh symlink must point at the git-prism binary"
        );
    }

    #[test]
    #[cfg(unix)]
    fn install_path_shim_creates_both_symlinks_pointing_at_same_target() {
        let dir = TempDir::new().unwrap();
        let home = dir.path();

        install_path_shim(home, false).unwrap();

        let git_link = home.join(".local/share/git-prism/bin/git");
        let gh_link = home.join(".local/share/git-prism/bin/gh");
        assert!(git_link.is_symlink(), "git symlink must exist");
        assert!(gh_link.is_symlink(), "gh symlink must exist");
        assert_eq!(
            std::fs::read_link(&git_link).unwrap(),
            std::fs::read_link(&gh_link).unwrap(),
            "git and gh symlinks must point at the same target"
        );
    }

    #[test]
    #[cfg(unix)]
    fn uninstall_path_shim_removes_both_symlinks() {
        let dir = TempDir::new().unwrap();
        let home = dir.path();

        install_path_shim(home, false).unwrap();
        uninstall_path_shim(home).unwrap();

        let git_link = home.join(".local/share/git-prism/bin/git");
        let gh_link = home.join(".local/share/git-prism/bin/gh");
        assert!(
            !git_link.exists() && !git_link.is_symlink(),
            "git symlink must be removed"
        );
        assert!(
            !gh_link.exists() && !gh_link.is_symlink(),
            "gh symlink must be removed"
        );
    }

    #[test]
    #[cfg(unix)]
    fn install_path_shim_is_idempotent_for_gh_symlink() {
        let dir = TempDir::new().unwrap();
        let home = dir.path();

        install_path_shim(home, false).unwrap();
        let target_before = std::fs::read_link(home.join(".local/share/git-prism/bin/gh")).unwrap();
        install_path_shim(home, false).unwrap();
        let target_after = std::fs::read_link(home.join(".local/share/git-prism/bin/gh")).unwrap();

        assert_eq!(
            target_before, target_after,
            "idempotent re-install must not change the gh symlink target"
        );
    }

    #[test]
    #[cfg(unix)]
    fn path_shim_status_for_gh_reports_installed_target() {
        // path_shim_status_for is per-name; `gh` is the new name added in #347.
        // Exercise the gh branch directly, not just the default `git` wrapper.
        let dir = TempDir::new().unwrap();
        let home = dir.path();

        install_path_shim(home, false).unwrap();

        match path_shim_status_for(home, "gh") {
            PathShimStatus::Installed {
                target,
                staleness_warning,
            } => {
                assert_eq!(
                    target,
                    expected_shim_target(),
                    "gh status must report the actual shim target"
                );
                assert!(
                    staleness_warning.is_none(),
                    "fresh gh install must not warn; got {staleness_warning:?}"
                );
            }
            other => panic!("expected gh Installed, got {other:?}"),
        }
    }

    #[test]
    #[cfg(unix)]
    fn path_shim_status_for_gh_reports_not_installed_when_only_git_present() {
        // Installing only a `git` symlink by hand must leave `gh` reporting
        // NotInstalled — status is genuinely per-name, not a shared verdict.
        let dir = TempDir::new().unwrap();
        let home = dir.path();
        let shim_dir = home.join(PATH_SHIM_REL_DIR);
        std::fs::create_dir_all(&shim_dir).unwrap();
        let exe = shim_dir.join("git-prism-fake");
        std::fs::write(&exe, b"binary").unwrap();
        std::os::unix::fs::symlink(&exe, shim_dir.join("git")).unwrap();

        assert_eq!(
            path_shim_status_for(home, "gh"),
            PathShimStatus::NotInstalled,
            "gh must report NotInstalled when only git is present"
        );
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

    // -------------------------------------------------------------------------
    // Adversarial QA — gh symlink (issue #347): partial-state / mixed-ownership
    // -------------------------------------------------------------------------

    /// PARTIAL-STATE BUG (uninstall): when the `git` link is git-prism-owned but
    /// the `gh` link is a FOREIGN symlink (points at some other binary),
    /// `uninstall_path_shim` processes `git` first and removes it, THEN bails on
    /// `gh`. Result: the user's `git` interception silently vanishes while the
    /// error message only talks about `gh`. Uninstall should either be
    /// all-or-nothing, or at minimum must not tear down a valid managed link
    /// when it is going to abort on a sibling it refuses to touch.
    #[test]
    #[cfg(unix)]
    fn adversarial_uninstall_must_not_remove_git_link_when_bailing_on_foreign_gh() {
        let dir = TempDir::new().unwrap();
        let home = dir.path();

        // Both links created pointing at the running binary (git-prism owned).
        install_path_shim(home, false).unwrap();

        // Replace the `gh` link with a FOREIGN symlink (a target that is not the
        // running git-prism binary).
        let gh_link = home.join(".local/share/git-prism/bin/gh");
        let foreign_target = dir.path().join("some-other-binary");
        std::fs::write(&foreign_target, b"not git-prism").unwrap();
        std::fs::remove_file(&gh_link).unwrap();
        std::os::unix::fs::symlink(&foreign_target, &gh_link).unwrap();

        let git_link = home.join(".local/share/git-prism/bin/git");
        assert!(git_link.is_symlink(), "precondition: git link exists");

        // Uninstall MUST refuse (foreign gh) — that part is expected.
        let result = uninstall_path_shim(home);
        assert!(
            result.is_err(),
            "uninstall must error when gh is a foreign symlink"
        );

        // BUG: the git link is gone even though uninstall aborted. The managed
        // `git` interception was torn down as a side effect of a refused
        // operation, leaving the system in a half-uninstalled state.
        assert!(
            git_link.is_symlink(),
            "uninstall removed the git symlink while bailing on a foreign gh link; \
             refused-uninstall left a partial state"
        );
    }

    /// PARTIAL-STATE BUG (install): when `gh` is a regular user file and `git`
    /// does not yet exist, `install_path_shim(force=false)` processes `git`
    /// first — creating the symlink — and only THEN reaches `gh`, where it
    /// bails. Result: install reports an error but has already created the
    /// `git` symlink. A failed install should not leave a half-installed state.
    #[test]
    #[cfg(unix)]
    fn adversarial_install_must_not_create_git_link_when_bailing_on_regular_gh_file() {
        let dir = TempDir::new().unwrap();
        let home = dir.path();
        let shim_dir = home.join(".local/share/git-prism/bin");
        std::fs::create_dir_all(&shim_dir).unwrap();

        // A regular (non-symlink) user file sits at the `gh` target. No `git`
        // link exists yet.
        let gh_file = shim_dir.join("gh");
        std::fs::write(&gh_file, b"USER OWNS THIS gh FILE\n").unwrap();

        let result = install_path_shim(home, false);
        assert!(
            result.is_err(),
            "install must error when gh is a regular file and force is false"
        );

        // BUG: install aborted (returned Err) but already created the git
        // symlink during the first loop iteration, leaving a partial install.
        let git_link = shim_dir.join("git");
        assert!(
            !git_link.exists() && !git_link.is_symlink(),
            "install created the git symlink despite aborting on the gh regular file; \
             failed install left a partial state"
        );
    }

    /// DANGLING-LINK BUG (uninstall): a git-prism-created shim symlink whose
    /// target was later removed (e.g. `brew upgrade` GC'd the Cellar binary —
    /// the very scenario the staleness advisory in this file addresses) becomes
    /// a dangling symlink. `uninstall_path_shim` cannot remove it: `read_link`
    /// returns Ok(dangling_target), `canonicalize()` of that target fails and
    /// falls back to the raw path, which never equals `current_exe`, so the
    /// function BAILS with "is not the running git-prism binary". The inline
    /// comment ("Broken symlink — safe to remove") proves the intended behavior
    /// is to remove it, but the code never reaches that Err branch for a
    /// dangling link. The user is left unable to clean up a shim git-prism
    /// itself created.
    #[test]
    #[cfg(unix)]
    fn adversarial_uninstall_must_remove_dangling_git_prism_owned_symlink() {
        let dir = TempDir::new().unwrap();
        let home = dir.path();
        let shim_dir = home.join(".local/share/git-prism/bin");
        std::fs::create_dir_all(&shim_dir).unwrap();

        // Simulate a git-prism shim install whose target was GC'd: a dangling
        // symlink under the managed bin dir.
        let dangling_target = home.join("Cellar/git-prism/0.9.0/bin/git-prism");
        let git_link = shim_dir.join("git");
        std::os::unix::fs::symlink(&dangling_target, &git_link).unwrap();
        assert!(git_link.is_symlink(), "precondition: dangling git link");
        assert!(!git_link.exists(), "precondition: target is gone");

        let result = uninstall_path_shim(home);

        // BUG: uninstall errors instead of cleaning up the dangling link.
        assert!(
            result.is_ok(),
            "uninstall failed to remove a dangling git-prism-owned symlink: {:?}",
            result.err()
        );
        assert!(
            !git_link.is_symlink(),
            "dangling shim symlink was left in place after uninstall"
        );
    }

    /// TRIANGULATION: uninstall partial-state — git is foreign, gh is owned.
    /// The git-prism-owned `gh` link must NOT be removed when the git link
    /// is foreign and causes a refusal (reverse order of the primary test).
    #[test]
    #[cfg(unix)]
    fn adversarial_uninstall_must_not_remove_gh_link_when_bailing_on_foreign_git() {
        let dir = TempDir::new().unwrap();
        let home = dir.path();

        install_path_shim(home, false).unwrap();

        // Replace `git` with a foreign symlink.
        let git_link = home.join(".local/share/git-prism/bin/git");
        let foreign_target = dir.path().join("foreign-binary");
        std::fs::write(&foreign_target, b"not git-prism").unwrap();
        std::fs::remove_file(&git_link).unwrap();
        std::os::unix::fs::symlink(&foreign_target, &git_link).unwrap();

        let gh_link = home.join(".local/share/git-prism/bin/gh");
        assert!(gh_link.is_symlink(), "precondition: gh link exists");

        let result = uninstall_path_shim(home);
        assert!(
            result.is_err(),
            "uninstall must error on foreign git symlink"
        );

        // gh must not have been removed — the refusal on git must have
        // pre-flighted before any mutation.
        assert!(
            gh_link.is_symlink(),
            "uninstall removed gh symlink while bailing on foreign git; partial state"
        );
    }

    /// TRIANGULATION: install partial-state — git is the obstacle, not gh.
    /// When git already has a foreign symlink, install must refuse BEFORE
    /// creating anything (including gh).
    #[test]
    #[cfg(unix)]
    fn adversarial_install_must_not_create_gh_link_when_bailing_on_foreign_git_symlink() {
        let dir = TempDir::new().unwrap();
        let home = dir.path();
        let shim_dir = home.join(".local/share/git-prism/bin");
        std::fs::create_dir_all(&shim_dir).unwrap();

        // Foreign symlink at git, nothing at gh.
        let foreign_target = dir.path().join("foreign-binary");
        std::fs::write(&foreign_target, b"not git-prism").unwrap();
        let git_link = shim_dir.join("git");
        std::os::unix::fs::symlink(&foreign_target, &git_link).unwrap();

        let result = install_path_shim(home, false);
        assert!(result.is_err(), "install must error on foreign git symlink");

        // gh must not have been created.
        let gh_link = shim_dir.join("gh");
        assert!(
            !gh_link.exists() && !gh_link.is_symlink(),
            "install created gh symlink despite aborting on foreign git; partial state"
        );
    }

    /// TRIANGULATION: install with --force on a foreign symlink must succeed
    /// and replace the foreign link.
    #[test]
    #[cfg(unix)]
    fn install_with_force_replaces_foreign_symlink() {
        let dir = TempDir::new().unwrap();
        let home = dir.path();
        let shim_dir = home.join(".local/share/git-prism/bin");
        std::fs::create_dir_all(&shim_dir).unwrap();

        let foreign_target = dir.path().join("foreign-gh");
        std::fs::write(&foreign_target, b"user's gh").unwrap();
        let gh_link = shim_dir.join("gh");
        std::os::unix::fs::symlink(&foreign_target, &gh_link).unwrap();

        install_path_shim(home, true).unwrap();

        // gh must now point at the git-prism shim target, not the foreign binary.
        let new_target = std::fs::read_link(&gh_link).unwrap();
        assert_ne!(
            new_target, foreign_target,
            "install --force must replace foreign symlink with git-prism shim"
        );
        // assert_ne! alone tolerates a garbage replacement. Pin the exact target:
        // --force must repoint at the running git-prism binary.
        assert_eq!(
            new_target,
            expected_shim_target(),
            "install --force must repoint gh at the git-prism binary, not just away from the foreign target"
        );
    }

    /// ASYMMETRY BUG (install clobbers foreign symlinks without --force): the
    /// `--force` guard only protects REGULAR files. A user-created foreign
    /// SYMLINK at the `gh` (or `git`) target — e.g. `gh` -> their own gh
    /// wrapper — is silently removed and replaced by `install_path_shim` even
    /// when `force` is false, with no error and no warning. This is
    /// inconsistent with `uninstall_path_shim`, which REFUSES to touch a
    /// foreign symlink. Install should refuse a foreign symlink without
    /// `--force`, the same way it refuses a regular file.
    #[test]
    #[cfg(unix)]
    fn adversarial_install_must_not_clobber_foreign_symlink_without_force() {
        let dir = TempDir::new().unwrap();
        let home = dir.path();
        let shim_dir = home.join(".local/share/git-prism/bin");
        std::fs::create_dir_all(&shim_dir).unwrap();

        // A user's pre-existing foreign symlink at the gh target.
        let users_gh = dir.path().join("users-own-gh");
        std::fs::write(&users_gh, b"user's gh wrapper").unwrap();
        let gh_link = shim_dir.join("gh");
        std::os::unix::fs::symlink(&users_gh, &gh_link).unwrap();

        let result = install_path_shim(home, false);

        // Without --force, install must NOT silently clobber the user's foreign
        // symlink. Either it errors, or it leaves the user's symlink intact.
        let still_points_at_users_target = std::fs::read_link(&gh_link)
            .map(|t| t == users_gh)
            .unwrap_or(false);
        assert!(
            result.is_err() || still_points_at_users_target,
            "install silently replaced a user's foreign gh symlink without --force \
             (result={result:?}); the --force guard only protects regular files, not \
             foreign symlinks"
        );
    }

    // -------------------------------------------------------------------------
    // Adversarial QA pass 2 — probing the validate-then-apply refactor
    // -------------------------------------------------------------------------

    /// PASS-2 PROBE (install asymmetry — dangling foreign symlink): the pass-1
    /// fix made `install_path_shim` REFUSE to clobber a *live* foreign symlink
    /// without `--force` (matching uninstall). But `classify_install_obstacle`
    /// treats a symlink whose target fails to canonicalize (a DANGLING symlink)
    /// as `NeedsReplacement` UNCONDITIONALLY — ignoring `force`. So a user's
    /// pre-existing foreign symlink that happens to be dangling (its target was
    /// renamed/removed) is silently deleted and replaced on a plain
    /// `install_path_shim(home, false)`, with no error.
    ///
    /// This is the same asymmetry the pass-1 "clobber foreign symlink" fix
    /// closed for live links, but left open for dangling ones. A live foreign
    /// link is protected; a dangling foreign link is not.
    #[test]
    #[cfg(unix)]
    fn adversarial_install_must_not_clobber_dangling_foreign_symlink_without_force() {
        let dir = TempDir::new().unwrap();
        let home = dir.path();
        let shim_dir = home.join(".local/share/git-prism/bin");
        std::fs::create_dir_all(&shim_dir).unwrap();

        // A user's foreign symlink at `gh` whose target does NOT exist (dangling).
        // e.g. they symlinked their own gh wrapper which they later moved.
        let users_missing_target = dir.path().join("users-own-gh-wrapper-now-moved");
        let gh_link = shim_dir.join("gh");
        std::os::unix::fs::symlink(&users_missing_target, &gh_link).unwrap();
        assert!(
            gh_link.is_symlink(),
            "precondition: dangling foreign gh link"
        );
        assert!(!gh_link.exists(), "precondition: its target is gone");

        let result = install_path_shim(home, false);

        // Without --force, install must NOT silently clobber the user's foreign
        // symlink just because it happens to be dangling. Either it errors, or it
        // leaves the user's symlink pointing where it was.
        let still_points_at_users_target = std::fs::read_link(&gh_link)
            .map(|t| t == users_missing_target)
            .unwrap_or(false);
        assert!(
            result.is_err() || still_points_at_users_target,
            "install silently replaced a user's DANGLING foreign gh symlink without \
             --force (result={result:?}); classify_install_obstacle treats any \
             canonicalize-failure as NeedsReplacement, bypassing the --force guard \
             that protects live foreign symlinks"
        );
    }

    /// PASS-2 PROBE (uninstall over-correction — dangling link pointing OUTSIDE
    /// the managed dir at a non-git-prism path): the pass-1 fix for bug 3 made
    /// `uninstall_path_shim` remove dangling symlinks inside the managed bin
    /// dir, on the theory that git-prism owns that dir exclusively. But the
    /// dangling branch removes the link regardless of where it POINTED — so a
    /// foreign dangling symlink (a user's own link to an external wrapper that
    /// later vanished) is deleted by `uninstall`, even though uninstall REFUSES
    /// to remove a *live* foreign symlink. The `debug_assert` only checks the
    /// link's parent dir, never the target's provenance.
    ///
    /// This test documents the over-correction the task asked about: does the
    /// fix delete something it shouldn't? It removes a dangling link that points
    /// at a clearly non-git-prism external path.
    #[test]
    #[cfg(unix)]
    fn adversarial_uninstall_removes_dangling_link_pointing_outside_managed_dir() {
        let dir = TempDir::new().unwrap();
        let home = dir.path();
        let shim_dir = home.join(".local/share/git-prism/bin");
        std::fs::create_dir_all(&shim_dir).unwrap();

        // A user's own foreign symlink at `git`, pointing at an EXTERNAL wrapper
        // outside the managed dir, whose target has since been removed (dangling).
        let external_target = dir.path().join("opt/some-tool/bin/git-wrapper");
        let git_link = shim_dir.join("git");
        std::os::unix::fs::symlink(&external_target, &git_link).unwrap();
        assert!(
            git_link.is_symlink(),
            "precondition: dangling foreign git link"
        );
        assert!(!git_link.exists(), "precondition: external target is gone");

        let _ = uninstall_path_shim(home);

        // Symmetry expectation: uninstall refuses to remove a LIVE foreign
        // symlink (it errors). A dangling foreign symlink that points outside
        // the managed dir should be treated with the same caution rather than
        // silently deleted. Currently it is deleted unconditionally.
        assert!(
            git_link.is_symlink(),
            "uninstall removed a dangling symlink that points OUTSIDE the managed dir \
             at a non-git-prism path ({}); the dangling branch removes by directory \
             membership alone, ignoring the target's provenance — over-correcting the \
             bug-3 fix",
            external_target.display()
        );
    }
}
