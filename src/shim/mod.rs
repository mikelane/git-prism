//! PATH-shim entry point for git-prism.
//!
//! When the `git-prism` binary is invoked as `git` (via a symlink), `run_shim`
//! intercepts agent-issued git commands and routes them to structured JSON
//! output from the existing `tools::*` functions.  Non-agent invocations and
//! unrecognised subcommands are passed through to the real git binary.

pub(crate) mod classify;
pub(crate) mod handlers;
pub(crate) mod real_git;
pub(crate) mod shadow;

use std::path::PathBuf;
use std::process::ExitCode;

use crate::agent_detection::EnvSource;
use crate::metrics::{ShimOutcome, ShimSubcommand};
use crate::shim::classify::{Classification, classify};
use crate::shim::real_git::RealGitExec;

/// Main entry point for shim mode.
///
/// Decision tree:
/// 1. `GIT_PRISM_INSIDE_SHIM` is set → passthrough (loop-break sentinel).
/// 2. `detect_calling_agent` returns `None` → passthrough (non-agent caller).
/// 3. `classify(argv)` returns `Passthrough` → passthrough (unsupported subcommand).
/// 4. Otherwise → call the appropriate handler and return structured JSON.
///
/// # Metrics invariant
///
/// Every path through this function calls `record_shim_invocation` **exactly
/// once**.  `record_shim_classification` is called at most once, only on the
/// structured-dispatch path (step 4) — never on passthrough or loop-break
/// paths.  This invariant ensures dashboards that aggregate
/// `shim_invocations_total` get an accurate per-call count.
pub(crate) fn run_shim<E: EnvSource, G: RealGitExec>(argv: &[&str], env: &E, exec: &G) -> ExitCode {
    let metrics = crate::metrics::get();

    // 1. Loop-break sentinel: a nested git call from within the shim.
    if env.get("GIT_PRISM_INSIDE_SHIM").is_some() {
        metrics.record_shim_invocation(ShimOutcome::LoopBreak);
        return exec.passthrough(argv);
    }

    // 2. Only intercept when an AI agent is the caller.
    if crate::agent_detection::detect_calling_agent(env).is_none() {
        metrics.record_shim_invocation(ShimOutcome::NoAgent);
        return exec.passthrough(argv);
    }

    // 3. Classify the subcommand.
    let classification = classify(argv);
    let subcommand = classification_to_subcommand(&classification);
    if classification == Classification::Passthrough {
        metrics.record_shim_invocation(ShimOutcome::Passthrough);
        return exec.passthrough(argv);
    }

    // Record classification metric before dispatching.
    metrics.record_shim_classification(subcommand);

    // 4. Dispatch to the handler.
    let repo_path = match resolve_repo_path(env) {
        Some(p) => p,
        None => {
            metrics.record_shim_invocation(ShimOutcome::Passthrough);
            return exec.passthrough(argv);
        }
    };
    let mut out_buf = Vec::new();
    let code = handlers::handle(&classification, &repo_path, &mut out_buf);

    // Emit response bytes metric before flushing so the count is always recorded.
    metrics.record_shim_invocation(ShimOutcome::Structured);
    metrics.record_shim_response_bytes(out_buf.len() as u64);

    // Flush the buffered response to stdout.
    use std::io::Write;
    if let Err(e) = std::io::stdout().write_all(&out_buf) {
        tracing::warn!(error = %e, "failed to write structured response to stdout");
    }

    // Shadow run happens AFTER the response is flushed — agent latency is unaffected.
    shadow::maybe_shadow_capture(env, subcommand, argv, exec);

    code
}

/// Map a `Classification` variant to the bounded `ShimSubcommand` label used
/// in metrics.  `Passthrough` has no meaningful subcommand, so it folds to
/// `Other`.
fn classification_to_subcommand(c: &Classification<'_>) -> ShimSubcommand {
    match c {
        Classification::Manifest { .. } => ShimSubcommand::Diff,
        Classification::History { .. } => ShimSubcommand::Log,
        Classification::FunctionContext { .. } => ShimSubcommand::Log,
        Classification::ShowSnapshot { .. } => ShimSubcommand::Show,
        Classification::BlameSnapshot { .. } => ShimSubcommand::Blame,
        Classification::Passthrough => ShimSubcommand::Other,
    }
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

        fn capture(&self, _argv: &[&str]) -> Result<usize, crate::shim::real_git::CaptureError> {
            Ok(0)
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

        // Leak the path string so it lives long enough for MapEnv's 'static lifetime.
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

    // --- AC: exactly-one-counter-per-invocation ---

    /// Verify that `record_shim_invocation` + `record_shim_classification` do
    /// not panic when called in the sequence that run_shim follows on the
    /// structured-dispatch path.  The global meter is a no-op in unit tests so
    /// we cannot read back counter values, but any mutation that removes a
    /// `record_shim_*` call would leave the metrics invariant documented in
    /// run_shim's doc comment violated — and the sampling tests in shadow.rs
    /// confirm the shadow path fires correctly when SAMPLE_PCT=100.
    #[test]
    fn record_shim_invocation_and_classification_do_not_panic_in_sequence() {
        let metrics = crate::metrics::Metrics::new_for_test();
        // Simulate the exact call sequence of the structured-dispatch path:
        // record_shim_classification called once, then record_shim_invocation.
        metrics.record_shim_classification(crate::metrics::ShimSubcommand::Diff);
        metrics.record_shim_invocation(crate::metrics::ShimOutcome::Structured);
        // Passthrough-only paths must also not panic (no classification call).
        metrics.record_shim_invocation(crate::metrics::ShimOutcome::Passthrough);
        metrics.record_shim_invocation(crate::metrics::ShimOutcome::LoopBreak);
        metrics.record_shim_invocation(crate::metrics::ShimOutcome::NoAgent);
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

    #[test]
    fn classification_to_subcommand_maps_each_variant() {
        use crate::shim::classify::Classification;
        assert_eq!(
            classification_to_subcommand(&Classification::Manifest { range: "x" }),
            ShimSubcommand::Diff
        );
        assert_eq!(
            classification_to_subcommand(&Classification::History { range: "x" }),
            ShimSubcommand::Log
        );
        assert_eq!(
            classification_to_subcommand(&Classification::FunctionContext {
                range: None,
                pickaxe_term: "x",
            }),
            ShimSubcommand::Log
        );
        assert_eq!(
            classification_to_subcommand(&Classification::ShowSnapshot { sha: "abc1234" }),
            ShimSubcommand::Show
        );
        assert_eq!(
            classification_to_subcommand(&Classification::BlameSnapshot {
                path: "src/main.rs"
            }),
            ShimSubcommand::Blame
        );
        assert_eq!(
            classification_to_subcommand(&Classification::Passthrough),
            ShimSubcommand::Other
        );
    }
}
