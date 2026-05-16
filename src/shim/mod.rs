//! PATH-shim entry point for git-prism.
//!
//! When the `git-prism` binary is invoked as `git` (via a symlink), `run_shim`
//! intercepts agent-issued git commands and routes them to structured JSON
//! output from the existing `tools::*` functions.  Non-agent invocations and
//! unrecognised subcommands are passed through to the real git binary.

pub(crate) mod classify;
pub(crate) mod handlers;
pub(crate) mod real_git;

use std::path::PathBuf;
use std::process::ExitCode;

use crate::agent_detection::EnvSource;
use crate::shim::classify::{Classification, classify};
use crate::shim::real_git::RealGitExec;

/// Main entry point for shim mode.
///
/// Decision tree:
/// 1. `GIT_PRISM_INSIDE_SHIM` is set → passthrough (loop-break sentinel).
/// 2. `detect_calling_agent` returns `None` → passthrough (non-agent caller).
/// 3. `classify(argv)` returns `Passthrough` → passthrough (unsupported subcommand).
/// 4. Otherwise → call the appropriate handler and return structured JSON.
pub(crate) fn run_shim<E: EnvSource, G: RealGitExec>(argv: &[&str], env: &E, exec: &G) -> ExitCode {
    // 1. Loop-break sentinel: a nested git call from within the shim.
    if env.get("GIT_PRISM_INSIDE_SHIM").is_some() {
        return exec.passthrough(argv);
    }

    // 2. Only intercept when an AI agent is the caller.
    if crate::agent_detection::detect_calling_agent(env).is_none() {
        return exec.passthrough(argv);
    }

    // 3. Classify the subcommand.
    let classification = classify(argv);
    if classification == Classification::Passthrough {
        return exec.passthrough(argv);
    }

    // 4. Dispatch to the handler.
    let repo_path = match resolve_repo_path(env) {
        Some(p) => p,
        None => return exec.passthrough(argv),
    };
    let mut stdout = std::io::stdout();
    handlers::handle(&classification, &repo_path, &mut stdout)
}

/// Return the repository path from `$GIT_PRISM_REPO` if set, otherwise use
/// the current working directory.  Returns `None` when the cwd cannot be
/// determined (deleted directory, permission error) — callers should fall
/// through to passthrough so real git can handle the error gracefully.
///
/// The `GIT_PRISM_CWD_UNAVAILABLE` env key is reserved for testing: when set,
/// this function behaves as if `current_dir()` failed.
fn resolve_repo_path(env: &dyn EnvSource) -> Option<PathBuf> {
    if let Some(repo) = env.get("GIT_PRISM_REPO") {
        return Some(PathBuf::from(repo));
    }
    // Allow tests to inject a cwd-unavailable condition without touching the
    // real process working directory.
    if env.get("GIT_PRISM_CWD_UNAVAILABLE").is_some() {
        return None;
    }
    std::env::current_dir().ok()
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::process::ExitCode;

    use super::*;

    // ---- test doubles ----

    struct MapEnv(HashMap<&'static str, &'static str>);

    impl EnvSource for MapEnv {
        fn get(&self, key: &str) -> Option<String> {
            self.0.get(key).map(|v| v.to_string())
        }
    }

    /// Records whether `passthrough` was called.
    struct SpyExec {
        pub called: std::cell::Cell<bool>,
        pub exit_code: ExitCode,
    }

    impl SpyExec {
        fn new(exit_code: ExitCode) -> Self {
            Self {
                called: std::cell::Cell::new(false),
                exit_code,
            }
        }
    }

    impl RealGitExec for SpyExec {
        fn passthrough(&self, _argv: &[&str]) -> ExitCode {
            self.called.set(true);
            self.exit_code
        }
    }

    // ---- decision path tests ----

    #[test]
    fn it_passes_through_when_inside_shim_sentinel_is_set() {
        let env = MapEnv(HashMap::from([
            ("GIT_PRISM_INSIDE_SHIM", "1"),
            ("CLAUDECODE", "1"),
        ]));
        let exec = SpyExec::new(ExitCode::SUCCESS);

        run_shim(&["git", "diff", "main..HEAD"], &env, &exec);

        assert!(
            exec.called.get(),
            "expected passthrough when sentinel is set"
        );
    }

    #[test]
    fn it_passes_through_when_no_agent_env_var_is_set() {
        // No CLAUDECODE, no AI_AGENT — detect_calling_agent returns None.
        let env = MapEnv(HashMap::new());
        let exec = SpyExec::new(ExitCode::SUCCESS);

        run_shim(&["git", "diff", "main..HEAD"], &env, &exec);

        assert!(
            exec.called.get(),
            "expected passthrough when no agent env var is set"
        );
    }

    #[test]
    fn it_passes_through_when_subcommand_is_not_on_watch_list() {
        let env = MapEnv(HashMap::from([("CLAUDECODE", "1")]));
        let exec = SpyExec::new(ExitCode::SUCCESS);

        run_shim(&["git", "status"], &env, &exec);

        assert!(
            exec.called.get(),
            "expected passthrough for unrecognised subcommand"
        );
    }

    #[test]
    fn it_passes_through_when_sentinel_takes_priority_over_agent_detection() {
        // Even when CLAUDECODE=1, the sentinel wins.
        let env = MapEnv(HashMap::from([
            ("GIT_PRISM_INSIDE_SHIM", "1"),
            ("CLAUDECODE", "1"),
        ]));
        let exec = SpyExec::new(ExitCode::SUCCESS);

        run_shim(&["git", "diff", "main..HEAD"], &env, &exec);

        assert!(
            exec.called.get(),
            "sentinel must take priority over agent detection"
        );
    }

    #[test]
    fn it_dispatches_to_handler_when_agent_set_and_subcommand_classified() {
        use std::process::Command;
        use tempfile::TempDir;

        // Build a minimal two-commit repo so the handler has something to work with.
        let dir = TempDir::new().unwrap();
        let repo_path = dir.path().to_path_buf();
        let run = |args: &[&str]| {
            Command::new("git")
                .args(args)
                .current_dir(&repo_path)
                .output()
                .unwrap()
        };
        run(&["init", "-b", "main"]);
        run(&["config", "user.email", "t@t.com"]);
        run(&["config", "user.name", "T"]);
        std::fs::write(repo_path.join("a.txt"), "hello\n").unwrap();
        run(&["add", "a.txt"]);
        run(&["commit", "-m", "first"]);
        std::fs::write(repo_path.join("b.txt"), "world\n").unwrap();
        run(&["add", "b.txt"]);
        run(&["commit", "-m", "second"]);

        // Env: agent set, no sentinel, repo path injected via GIT_PRISM_REPO.
        let env = MapEnv(HashMap::from([
            ("CLAUDECODE", "1"),
            // We can't pass a &'static str for the path, so use GIT_PRISM_REPO
            // via a separate owned-string approach. Use the String-keyed variant
            // by leaking for this test only.
        ]));
        // Leak the path string so it lives long enough for the 'static lifetime.
        let repo_str: &'static str =
            Box::leak(repo_path.to_string_lossy().into_owned().into_boxed_str());

        let env = MapEnv(HashMap::from([
            ("CLAUDECODE", "1"),
            ("GIT_PRISM_REPO", repo_str),
        ]));
        let exec = SpyExec::new(ExitCode::SUCCESS);

        // git diff main..HEAD is a classified command that routes to handle_manifest.
        let code = run_shim(&["git", "diff", "HEAD~1..HEAD"], &env, &exec);

        // SpyExec must NOT have been called — the handler ran instead.
        assert!(
            !exec.called.get(),
            "expected handler dispatch, not passthrough"
        );
        // Handler should return SUCCESS.
        assert_eq!(code, ExitCode::SUCCESS, "handler should return SUCCESS");
    }

    #[test]
    fn it_passes_through_when_current_dir_is_unavailable() {
        // GIT_PRISM_REPO not set, and current_dir cannot be determined.
        // run_shim must fall through to passthrough rather than panicking.
        // We simulate the failure via GIT_PRISM_REPO pointing to a path that
        // doesn't exist — but the real test is that a broken cwd_source falls
        // through. We use a MapEnv with a CWD_FAIL sentinel key that triggers
        // the error path.
        let env = MapEnv(HashMap::from([
            ("CLAUDECODE", "1"),
            ("GIT_PRISM_CWD_UNAVAILABLE", "1"),
        ]));
        let exec = SpyExec::new(ExitCode::SUCCESS);

        // argv is a classified command so it would normally dispatch — but
        // the cwd failure must cause passthrough instead.
        run_shim(&["git", "diff", "main..HEAD"], &env, &exec);

        assert!(
            exec.called.get(),
            "expected passthrough when current directory cannot be determined"
        );
    }
}
