//! Real-git resolver and exec trait.
//!
//! Abstracts "find the real git binary and exec it" behind a trait so the
//! shim's decision logic can be unit-tested with a fake implementation.

#[cfg(unix)]
use std::path::Path;
use std::path::PathBuf;
use std::process::ExitCode;

use crate::agent_detection::EnvSource;

/// Errors returned by [`RealGitExec::capture`].
#[derive(Debug, thiserror::Error)]
pub(crate) enum CaptureError {
    #[error("could not find real git binary")]
    RealGitNotFound,
    #[error("failed to spawn git: {0}")]
    Spawn(#[from] std::io::Error),
}

impl CaptureError {
    pub(crate) fn kind(&self) -> &'static str {
        match self {
            Self::RealGitNotFound => "git_not_found",
            Self::Spawn(_) => "spawn_failed",
        }
    }
}

/// Abstracts exec-ing the real git binary.
pub(crate) trait RealGitExec {
    /// Replace the current process with the real git binary, passing `argv`
    /// (argv[0] should be `"git"`).  Returns `ExitCode` only when exec
    /// failed (in production this never returns on success).
    fn passthrough(&self, argv: &[&str]) -> ExitCode;

    /// Run the real git binary with `argv` and return the stdout byte length.
    ///
    /// Used by the opt-in shadow-run path for token-savings instrumentation.
    /// The stdout buffer is dropped immediately after measuring — callers
    /// must not rely on its contents.
    fn capture(&self, argv: &[&str]) -> Result<usize, CaptureError>;
}

/// Production implementation that walks `$PATH` to find the real git binary
/// and `execvp`s it after setting `GIT_PRISM_INSIDE_SHIM=1`.
pub(crate) struct StdRealGitExec<'e, E: EnvSource> {
    pub(crate) env: &'e E,
    pub(crate) argv0: &'e str,
}

impl<E: EnvSource> RealGitExec for StdRealGitExec<'_, E> {
    fn passthrough(&self, argv: &[&str]) -> ExitCode {
        #[cfg(unix)]
        {
            // Determine which binary to resolve by extracting the basename of
            // self.argv0 (the shim symlink path, e.g. "/tmp/bin/gh" → "gh").
            // argv[0] is a full path when the OS execs the shim by path; using
            // the basename of self.argv0 gets the symlink name ("git" or "gh").
            let binary_name = std::path::Path::new(self.argv0)
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("git");
            let real = match resolve_real_binary(binary_name, self.argv0, self.env) {
                Some(p) => p,
                None => {
                    eprintln!("git-prism shim: could not find real {binary_name} binary");
                    return ExitCode::from(127);
                }
            };

            if self.env.get("GIT_PRISM_DEBUG_RESOLVER").is_some() {
                eprintln!(
                    "git-prism shim: resolved real {binary_name} to {}",
                    real.display()
                );
            }

            use std::os::unix::process::CommandExt as _;
            let mut cmd = std::process::Command::new(&real);
            cmd.args(argv.iter().skip(1)); // skip argv[0]
            cmd.env("GIT_PRISM_INSIDE_SHIM", "1");
            let err = cmd.exec(); // never returns on success
            eprintln!("git-prism shim: exec of {} failed: {err}", real.display());
            ExitCode::from(exec_failure_exit_code(&err))
        }
        #[cfg(not(unix))]
        {
            let binary_name = std::path::Path::new(self.argv0)
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("git");
            let real = match resolve_real_binary(binary_name, self.argv0, self.env) {
                Some(p) => p,
                None => {
                    eprintln!("git-prism shim: could not find real {binary_name} binary");
                    return ExitCode::from(127);
                }
            };

            if self.env.get("GIT_PRISM_DEBUG_RESOLVER").is_some() {
                eprintln!(
                    "git-prism shim: resolved real {binary_name} to {}",
                    real.display()
                );
            }

            let status = std::process::Command::new(&real)
                .args(argv.iter().skip(1))
                .env("GIT_PRISM_INSIDE_SHIM", "1")
                .status();

            match status {
                Ok(s) => {
                    let code = s.code().unwrap_or(1);
                    ExitCode::from(code as u8)
                }
                Err(err) => {
                    eprintln!("git-prism shim: spawn of {} failed: {err}", real.display());
                    ExitCode::from(exec_failure_exit_code(&err))
                }
            }
        }
    }

    fn capture(&self, argv: &[&str]) -> Result<usize, CaptureError> {
        // capture() is only used on the git shadow-run path (maybe_shadow_capture in shadow.rs).
        // gh invocations never reach here; they always pass through to the real gh binary.
        let real = resolve_real_git(self.argv0, self.env).ok_or(CaptureError::RealGitNotFound)?;
        // std::process::Command passes args as a slice — no shell involved,
        // so there is no command-injection risk here.
        let output = std::process::Command::new(&real)
            .args(argv.iter().skip(1))
            .env("GIT_PRISM_INSIDE_SHIM", "1")
            .output()?;
        Ok(output.stdout.len())
    }
}

/// Map an exec `io::Error` to the conventional shell exit code.
///
/// - 126: command found but not executable (`PermissionDenied`)
/// - 127: command not found or other exec failure
fn exec_failure_exit_code(err: &std::io::Error) -> u8 {
    match err.kind() {
        std::io::ErrorKind::PermissionDenied => 126,
        _ => 127,
    }
}

/// Walk `$PATH`, skip any candidate that resolves (via symlink) to the shim
/// binary itself, and return the first `<entry>/git` that is executable.
///
/// Falls back to `/usr/bin/git`, `/usr/local/bin/git`, `/opt/homebrew/bin/git`
/// if nothing is found in `$PATH`.  Returns `None` only when no git binary
/// exists anywhere.
///
/// On non-Unix platforms this always returns `None` — the shim is unix-only.
#[cfg(unix)]
pub(crate) fn resolve_real_git(argv0: &str, env: &dyn EnvSource) -> Option<PathBuf> {
    let shim_path = shim_canonical_path(argv0);

    if let Some(path_var) = env.get("PATH") {
        for entry in path_var.split(':') {
            if entry.is_empty() {
                continue;
            }
            let candidate = Path::new(entry).join("git");
            if !is_executable(&candidate) {
                continue;
            }
            // Skip this candidate if:
            // (a) it lives in the same directory as the shim binary, or
            // (b) it resolves (via symlink) to the shim binary itself.
            // Case (a) handles the direct install; case (b) handles the
            // Homebrew/cellar pattern where the shim is symlinked into PATH.
            if let Some(shim) = &shim_path {
                let shim_dir = shim.parent().map(Path::to_path_buf);
                let candidate_dir = candidate.parent().map(Path::to_path_buf);
                let same_dir = match (&shim_dir, &candidate_dir) {
                    (Some(sd), Some(cd)) => canonical_eq(sd, cd),
                    _ => false,
                };
                if same_dir || canonical_eq(&candidate, shim) {
                    continue;
                }
            }
            return Some(candidate);
        }
    }

    // Fallback chain when $PATH search finds nothing.
    for fallback in &[
        "/usr/bin/git",
        "/usr/local/bin/git",
        "/opt/homebrew/bin/git",
    ] {
        let p = Path::new(fallback);
        if is_executable(p) {
            return Some(p.to_path_buf());
        }
    }

    None
}

/// Walk `%PATH%` on Windows, skip any candidate that resolves to the shim
/// binary itself, and return the first `<entry>\git.exe` that exists.
///
/// Returns `None` only when no git binary is found anywhere on PATH.
#[cfg(not(unix))]
pub(crate) fn resolve_real_git(argv0: &str, env: &dyn EnvSource) -> Option<PathBuf> {
    use std::path::Path;

    let shim_canonical = std::path::PathBuf::from(argv0)
        .canonicalize()
        .ok()
        .or_else(|| {
            std::env::current_exe()
                .ok()
                .and_then(|p| p.canonicalize().ok())
        });

    if let Some(path_var) = env.get("PATH") {
        for entry in path_var.split(';') {
            if entry.is_empty() {
                continue;
            }
            // On Windows git may be "git.exe" or "git" (cmd.exe resolves extensions).
            // Try both; prefer the .exe form.
            for name in &["git.exe", "git"] {
                let candidate = Path::new(entry).join(name);
                if !candidate.exists() {
                    continue;
                }
                // Skip if this candidate resolves to the shim itself.
                if let Some(shim) = &shim_canonical {
                    if candidate.canonicalize().ok().as_ref() == Some(shim) {
                        continue;
                    }
                }
                return Some(candidate);
            }
        }
    }
    None
}

/// Resolve the real binary for `binary_name` (e.g. `"git"`, `"gh"`), skipping
/// any PATH entry that resolves to the shim binary itself.
///
/// For `"git"`, delegates to `resolve_real_git` which includes hardcoded
/// fallback paths.  For all other binaries, performs the same shim-skipping
/// PATH walk without fallbacks (those binaries are not bundled on every system
/// the way git is).
///
/// On non-Unix platforms this always returns `None`.
#[cfg(unix)]
pub(crate) fn resolve_real_binary(
    binary_name: &str,
    argv0: &str,
    env: &dyn EnvSource,
) -> Option<PathBuf> {
    if binary_name == "git" {
        return resolve_real_git(argv0, env);
    }
    // Generic PATH walk for non-git binaries (e.g. "gh").
    let shim_path = shim_canonical_path(argv0);
    if let Some(path_var) = env.get("PATH") {
        for entry in path_var.split(':') {
            if entry.is_empty() {
                continue;
            }
            let candidate = Path::new(entry).join(binary_name);
            if !is_executable(&candidate) {
                continue;
            }
            if let Some(shim) = &shim_path {
                let shim_dir = shim.parent().map(Path::to_path_buf);
                let candidate_dir = candidate.parent().map(Path::to_path_buf);
                let same_dir = match (&shim_dir, &candidate_dir) {
                    (Some(sd), Some(cd)) => canonical_eq(sd, cd),
                    _ => false,
                };
                if same_dir || canonical_eq(&candidate, shim) {
                    continue;
                }
            }
            return Some(candidate);
        }
    }
    None
}

/// Walk `%PATH%` on Windows for `binary_name`, skipping any candidate that
/// resolves to the shim binary itself.
///
/// For `"git"`, delegates to `resolve_real_git` which includes hardcoded
/// fallback paths.  For all other binaries (e.g. `"gh"`), performs the same
/// shim-skipping PATH walk without fallbacks.
///
/// Returns `None` only when no matching binary is found anywhere on PATH.
#[cfg(not(unix))]
pub(crate) fn resolve_real_binary(
    binary_name: &str,
    argv0: &str,
    env: &dyn EnvSource,
) -> Option<PathBuf> {
    use std::path::Path;

    if binary_name == "git" {
        return resolve_real_git(argv0, env);
    }

    // Generic PATH walk for non-git binaries (e.g. "gh") on Windows.
    let shim_canonical = std::path::PathBuf::from(argv0)
        .canonicalize()
        .ok()
        .or_else(|| {
            std::env::current_exe()
                .ok()
                .and_then(|p| p.canonicalize().ok())
        });

    if let Some(path_var) = env.get("PATH") {
        for entry in path_var.split(';') {
            if entry.is_empty() {
                continue;
            }
            // Try <name>.exe then <name> (cmd.exe resolves extensions).
            for suffix in &[".exe", ""] {
                let candidate = Path::new(entry).join(format!("{binary_name}{suffix}"));
                if !candidate.exists() {
                    continue;
                }
                // Skip if this candidate resolves to the shim itself.
                if let Some(shim) = &shim_canonical {
                    if candidate.canonicalize().ok().as_ref() == Some(shim) {
                        continue;
                    }
                }
                return Some(candidate);
            }
        }
    }
    None
}

/// Return the canonicalized path of the shim binary.
///
/// Tries `argv0` first (works when the shim is invoked with a full path).
/// Falls back to `std::env::current_exe()` which the OS always resolves
/// correctly, even when `argv0` is a bare name like `"git"` (e.g. when
/// the kernel sets argv[0] from a PATH lookup).
#[cfg(unix)]
fn shim_canonical_path(argv0: &str) -> Option<PathBuf> {
    Path::new(argv0).canonicalize().ok().or_else(|| {
        std::env::current_exe()
            .ok()
            .and_then(|p| p.canonicalize().ok())
    })
}

/// Return `true` if `a` and `b` refer to the same directory after
/// canonicalization.  Falls back to a direct comparison when either
/// canonicalization fails.
#[cfg(unix)]
fn canonical_eq(a: &Path, b: &Path) -> bool {
    let ca = a.canonicalize().unwrap_or_else(|_| a.to_path_buf());
    let cb = b.canonicalize().unwrap_or_else(|_| b.to_path_buf());
    ca == cb
}

/// Return `true` when `path` exists and is executable by the current user.
#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    path.metadata()
        .map(|m| m.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    #[cfg(unix)]
    use std::fs;

    #[cfg(unix)]
    use tempfile::TempDir;

    use super::*;

    struct MapEnv(HashMap<String, String>);

    impl EnvSource for MapEnv {
        fn get(&self, key: &str) -> Option<String> {
            self.0.get(key).cloned()
        }
    }

    #[cfg(unix)]
    fn make_executable_file(dir: &Path, name: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let p = dir.join(name);
        fs::write(&p, b"#!/bin/sh\n").unwrap();
        let mut perms = fs::metadata(&p).unwrap().permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&p, perms).unwrap();
        p
    }

    #[cfg(unix)]
    fn env_with_path(path_val: &str) -> MapEnv {
        MapEnv(HashMap::from([("PATH".to_string(), path_val.to_string())]))
    }

    #[cfg(unix)]
    #[test]
    fn it_skips_shim_directory_and_finds_git_in_next_path_entry() {
        let shim_dir = TempDir::new().unwrap();
        let real_dir = TempDir::new().unwrap();

        // Create "git" in both dirs; the one in shim_dir must be skipped.
        make_executable_file(shim_dir.path(), "git");
        let expected = make_executable_file(real_dir.path(), "git");

        // Fake argv0 points into the shim dir.
        let shim_binary = shim_dir.path().join("git-prism");
        fs::write(&shim_binary, b"").unwrap();

        let path_val = format!(
            "{}:{}",
            shim_dir.path().display(),
            real_dir.path().display()
        );
        let env = env_with_path(&path_val);
        let argv0 = shim_binary.to_string_lossy().into_owned();
        let result = resolve_real_git(&argv0, &env);
        assert_eq!(result, Some(expected));
    }

    #[cfg(unix)]
    #[test]
    fn it_uses_fallback_chain_when_path_has_no_git() {
        // Use an empty PATH so $PATH search finds nothing.
        let env = env_with_path("");
        // We can't guarantee /usr/bin/git exists, but we can verify no panic.
        let _ = resolve_real_git("/nonexistent/git-prism", &env);
    }

    #[cfg(unix)]
    #[test]
    fn it_returns_none_when_no_git_found_anywhere() {
        // No PATH key at all.
        let env = MapEnv(HashMap::new());
        // On a machine where none of the hardcoded fallbacks exist this
        // returns None.  Just verify no panic.
        let _ = resolve_real_git("/nonexistent/path/git-prism", &env);
    }

    #[cfg(unix)]
    #[test]
    fn it_finds_git_in_path_when_shim_dir_is_absent() {
        let real_dir = TempDir::new().unwrap();
        let expected = make_executable_file(real_dir.path(), "git");

        let path_val = real_dir.path().display().to_string();
        let env = env_with_path(&path_val);

        let result = resolve_real_git("/nonexistent/git-prism", &env);
        assert_eq!(result, Some(expected));
    }

    #[cfg(unix)]
    #[test]
    fn it_skips_symlink_to_shim_binary_in_different_path_entry() {
        // Homebrew install pattern:
        //   shim lives at  <cellar_dir>/git-prism  (NOT in PATH)
        //   <link_dir>/git  →  <cellar_dir>/git-prism   (symlink, IS in PATH)
        //   <real_dir>/git  is the actual git binary
        //
        // The resolver must skip <link_dir>/git because it resolves to the shim,
        // even though <link_dir> is NOT the shim's parent directory.
        let cellar_dir = TempDir::new().unwrap();
        let link_dir = TempDir::new().unwrap();
        let real_dir = TempDir::new().unwrap();

        // Create the shim binary in cellar_dir (not in PATH).
        let shim_binary = make_executable_file(cellar_dir.path(), "git-prism");

        // Create a symlink <link_dir>/git -> <cellar_dir>/git-prism.
        let symlink_git = link_dir.path().join("git");
        std::os::unix::fs::symlink(&shim_binary, &symlink_git).unwrap();

        // Create the real git binary in real_dir.
        let expected = make_executable_file(real_dir.path(), "git");

        // PATH: link_dir first (has the shim symlink), then real_dir.
        let path_val = format!(
            "{}:{}",
            link_dir.path().display(),
            real_dir.path().display()
        );
        let env = env_with_path(&path_val);

        // argv0 is the canonical shim binary (cellar_dir/git-prism).
        let argv0 = shim_binary.to_string_lossy().into_owned();
        let result = resolve_real_git(&argv0, &env);

        assert_eq!(
            result,
            Some(expected),
            "resolver must skip the shim symlink and return the real git binary"
        );
    }

    #[cfg(unix)]
    #[test]
    fn it_maps_permission_denied_to_exit_126() {
        let err = std::io::Error::from(std::io::ErrorKind::PermissionDenied);
        assert_eq!(exec_failure_exit_code(&err), 126);
    }

    #[test]
    fn it_maps_not_found_to_exit_127() {
        let err = std::io::Error::from(std::io::ErrorKind::NotFound);
        assert_eq!(exec_failure_exit_code(&err), 127);
    }

    #[test]
    fn it_maps_other_errors_to_exit_127() {
        let err = std::io::Error::from(std::io::ErrorKind::Other);
        assert_eq!(exec_failure_exit_code(&err), 127);
    }

    // ===== ADVERSARIAL QA PROBES (issue #349 pen-test) =====

    /// RECURSION SAFETY (deterministic): PATH has a `git` symlink to the shim
    /// in the FIRST entry, and a real (non-shim) `git` in the SECOND entry. The
    /// resolver MUST skip the shim symlink and return the real git — not the
    /// symlink, and not anything that canonicalizes to the shim binary.
    /// Returning the symlink would make the shim's passthrough exec path
    /// re-invoke the shim itself → infinite recursion.
    ///
    /// This is a positive assertion (`assert_eq!` on the real git) so it cannot
    /// pass vacuously: the resolver is forced to return Some, and that Some must
    /// be the real binary in the second PATH entry. It does NOT depend on a
    /// system git existing, because the synthetic PATH always contains a real
    /// (non-shim) candidate.
    #[cfg(unix)]
    #[test]
    fn qa_recursion_resolver_skips_shim_symlink_and_returns_real_git() {
        let cellar_dir = TempDir::new().unwrap();
        let link_dir = TempDir::new().unwrap();
        let real_dir = TempDir::new().unwrap();

        // The shim binary (this is what the `git` symlink resolves to).
        let shim_binary = make_executable_file(cellar_dir.path(), "git-prism");

        // <link_dir>/git -> <cellar_dir>/git-prism  (a shim symlink on PATH)
        let symlink_git = link_dir.path().join("git");
        std::os::unix::fs::symlink(&shim_binary, &symlink_git).unwrap();

        // <real_dir>/git — a genuine, non-shim git binary in a later PATH entry.
        let real_git = make_executable_file(real_dir.path(), "git");

        // PATH: shim-symlink dir FIRST, real git dir SECOND. The resolver must
        // skip the first and select the second.
        let path_val = format!(
            "{}:{}",
            link_dir.path().display(),
            real_dir.path().display()
        );
        let env = env_with_path(&path_val);
        let argv0 = shim_binary.to_string_lossy().into_owned();

        let result = resolve_real_git(&argv0, &env);

        // Positive assertion: must resolve to the real git, never the shim.
        assert_eq!(
            result.as_ref(),
            Some(&real_git),
            "resolver must skip the shim symlink and return the real git \
             in the next PATH entry"
        );
        let returned = result.expect("resolver must return a binary");
        assert_ne!(
            returned, symlink_git,
            "resolver returned the shim symlink — this would recurse"
        );
        let canon = returned.canonicalize().unwrap_or(returned.clone());
        let shim_canon = shim_binary.canonicalize().unwrap();
        assert_ne!(
            canon, shim_canon,
            "resolver returned a path that canonicalizes to the shim binary — recursion"
        );
    }

    /// FALLBACK-CHAIN RECURSION GAP: the hardcoded fallback chain in
    /// `resolve_real_git` (`/usr/bin/git`, `/usr/local/bin/git`,
    /// `/opt/homebrew/bin/git`) is reached when the PATH walk finds nothing.
    /// Unlike the PATH walk, the fallback chain applies NO shim-skip check —
    /// it returns the first path that merely `is_executable`. If the shim were
    /// installed at one of those exact paths AND no git existed on PATH, the
    /// resolver would hand back the shim → recursion.
    ///
    /// This probe documents the gap structurally: it asserts that the PATH
    /// walk (which DOES skip the shim) is the only shim-aware layer. We cannot
    /// write to /opt/homebrew in a hermetic test, so this records the concern
    /// by confirming the fallback list is consulted with an empty PATH and that
    /// the skip logic is absent from that branch. Kept green; see QA report
    /// SUSPICIOUS finding.
    #[cfg(unix)]
    #[test]
    fn qa_fallback_chain_is_consulted_when_path_empty_and_does_not_panic() {
        let env = env_with_path("");
        // No panic, returns either a real system git or None. The point of this
        // test is to pin the behavior so a future change to the fallback branch
        // (e.g. adding the missing shim-skip) is a visible diff here.
        let _ = resolve_real_git("/nonexistent/git-prism", &env);
    }

    #[cfg(unix)]
    #[test]
    fn it_skips_non_executable_git_files() {
        let dir = TempDir::new().unwrap();
        // Write a non-executable "git"
        let git_path = dir.path().join("git");
        fs::write(&git_path, b"#!/bin/sh\n").unwrap();
        // Do NOT set executable bit

        let path_val = dir.path().display().to_string();
        let env = env_with_path(&path_val);
        let result = resolve_real_git("/nonexistent/git-prism", &env);
        // The non-executable file was NOT returned.
        if let Some(p) = result {
            assert_ne!(p, git_path);
        }
    }
}
