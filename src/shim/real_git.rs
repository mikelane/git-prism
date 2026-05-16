//! Real-git resolver and exec trait.
//!
//! Abstracts "find the real git binary and exec it" behind a trait so the
//! shim's decision logic can be unit-tested with a fake implementation.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use crate::agent_detection::EnvSource;

/// Abstracts exec-ing the real git binary.
pub(crate) trait RealGitExec {
    /// Replace the current process with the real git binary, passing `argv`
    /// (argv[0] should be `"git"`).  Returns `ExitCode` only when exec
    /// failed (in production this never returns on success).
    fn passthrough(&self, argv: &[&str]) -> ExitCode;
}

/// Production implementation that walks `$PATH` to find the real git binary
/// and `execvp`s it after setting `GIT_PRISM_INSIDE_SHIM=1`.
pub(crate) struct StdRealGitExec<'e, E: EnvSource> {
    pub(crate) env: &'e E,
    pub(crate) argv0: &'e str,
}

impl<E: EnvSource> RealGitExec for StdRealGitExec<'_, E> {
    fn passthrough(&self, argv: &[&str]) -> ExitCode {
        let real = match resolve_real_git(self.argv0, self.env) {
            Some(p) => p,
            None => {
                eprintln!("git-prism shim: could not find real git binary");
                return ExitCode::from(127);
            }
        };

        use std::os::unix::process::CommandExt;
        let mut cmd = std::process::Command::new(&real);
        cmd.args(argv.iter().skip(1)); // skip argv[0] ("git")
        cmd.env("GIT_PRISM_INSIDE_SHIM", "1");
        let err = cmd.exec(); // never returns on success
        eprintln!("git-prism shim: exec failed: {err}");
        ExitCode::from(127)
    }
}

/// Walk `$PATH`, skip any candidate that resolves (via symlink) to the shim
/// binary itself, and return the first `<entry>/git` that is executable.
///
/// Falls back to `/usr/bin/git`, `/usr/local/bin/git`, `/opt/homebrew/bin/git`
/// if nothing is found in `$PATH`.  Returns `None` only when no git binary
/// exists anywhere.
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

/// Return the canonicalized path of the shim binary (`argv0`), or `None`
/// when canonicalization fails (e.g. relative paths, missing binary).
fn shim_canonical_path(argv0: &str) -> Option<PathBuf> {
    Path::new(argv0).canonicalize().ok()
}

/// Return `true` if `a` and `b` refer to the same directory after
/// canonicalization.  Falls back to a direct comparison when either
/// canonicalization fails.
fn canonical_eq(a: &Path, b: &Path) -> bool {
    let ca = a.canonicalize().unwrap_or_else(|_| a.to_path_buf());
    let cb = b.canonicalize().unwrap_or_else(|_| b.to_path_buf());
    ca == cb
}

/// Return `true` when `path` exists and is executable by the current user.
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    path.metadata()
        .map(|m| m.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;

    use tempfile::TempDir;

    use super::*;

    struct MapEnv(HashMap<String, String>);

    impl EnvSource for MapEnv {
        fn get(&self, key: &str) -> Option<String> {
            self.0.get(key).cloned()
        }
    }

    fn make_executable_file(dir: &Path, name: &str) -> PathBuf {
        let p = dir.join(name);
        fs::write(&p, b"#!/bin/sh\n").unwrap();
        let mut perms = fs::metadata(&p).unwrap().permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&p, perms).unwrap();
        p
    }

    fn env_with_path(path_val: &str) -> MapEnv {
        MapEnv(HashMap::from([("PATH".to_string(), path_val.to_string())]))
    }

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

    #[test]
    fn it_uses_fallback_chain_when_path_has_no_git() {
        // Use an empty PATH so $PATH search finds nothing.
        let env = env_with_path("");
        // We can't guarantee /usr/bin/git exists, but we can verify no panic.
        let _ = resolve_real_git("/nonexistent/git-prism", &env);
    }

    #[test]
    fn it_returns_none_when_no_git_found_anywhere() {
        // No PATH key at all.
        let env = MapEnv(HashMap::new());
        // On a machine where none of the hardcoded fallbacks exist this
        // returns None.  Just verify no panic.
        let _ = resolve_real_git("/nonexistent/path/git-prism", &env);
    }

    #[test]
    fn it_finds_git_in_path_when_shim_dir_is_absent() {
        let real_dir = TempDir::new().unwrap();
        let expected = make_executable_file(real_dir.path(), "git");

        let path_val = real_dir.path().display().to_string();
        let env = env_with_path(&path_val);

        let result = resolve_real_git("/nonexistent/git-prism", &env);
        assert_eq!(result, Some(expected));
    }

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
