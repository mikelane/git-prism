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
use std::time::Duration;

use crate::agent_detection::EnvSource;
use crate::metrics::{ShimOutcome, ShimSubcommand};
use crate::shim::classify::{Classification, classify};
use crate::shim::real_git::RealGitExec;
use crate::telemetry::TelemetryGuard;

/// Flush deadline used on passthrough paths (before execvp).
///
/// A DOWN OTLP collector makes `force_flush()` block up to `EXPORT_TIMEOUT`
/// (5 s).  That would stall the shell on every intercepted git command even
/// when the collector is unreachable.  This cap bounds the wait; the flush
/// thread is abandoned if it hasn't completed in time and will be torn down
/// when the process image is replaced by execvp.
const PASSTHROUGH_FLUSH_TIMEOUT: Duration = Duration::from_millis(300);

/// Flush deadline used on the structured-output path (after JSON is printed).
///
/// The agent has already received its response by the time the flush runs, so
/// a longer cap is acceptable — but it must still be finite so a saturated or
/// unreachable OTLP collector cannot stall a `git diff`/`log`/`show`/`blame`
/// call indefinitely (the #360 multi-minute stall scenario).
pub(crate) const STRUCTURED_FLUSH_TIMEOUT: Duration = Duration::from_millis(500);

/// Returns `true` when the caller has requested an immediate passthrough,
/// skipping all shim processing and telemetry.
///
/// Checked in `main.rs` **before** `telemetry::init_quiet()` so opted-out
/// calls have zero added latency and emit no telemetry at all.  Also checked
/// at the top of `run_shim` for callers that already hold a guard.
///
/// Truthy values: `"1"`, `"true"` (case-insensitive).
/// Falsy / absent: empty string, `"0"`, `"false"`, or unset.
pub(crate) fn passthrough_opt_out_requested(env: &dyn EnvSource) -> bool {
    for key in &["GIT_PRISM_PASSTHROUGH", "GIT_PRISM_DISABLE"] {
        if let Some(val) = env.get(key) {
            let lower = val.to_ascii_lowercase();
            if lower == "1" || lower == "true" {
                return true;
            }
        }
    }
    false
}

/// Main entry point for shim mode.
///
/// Decision tree:
/// 0. `GIT_PRISM_PASSTHROUGH` / `GIT_PRISM_DISABLE` is truthy → immediate exec
///    (opt-out; no metrics recorded, no telemetry flush needed).
/// 1. `GIT_PRISM_INSIDE_SHIM` is set → passthrough (loop-break sentinel).
/// 2. `detect_calling_agent` returns `None` → passthrough (non-agent caller).
/// 3. `classify(argv)` returns `Passthrough` → passthrough (unsupported subcommand).
/// 4. Otherwise → call the appropriate handler and return structured JSON.
///
/// # Metrics invariant
///
/// Steps 1–3 call `record_shim_invocation` **exactly once**.  Step 0
/// (opt-out) short-circuits before any metric recording so opted-out calls
/// do not appear in dashboards.  `record_shim_classification` is called at
/// most once, only on the structured-dispatch path (step 4).  This invariant
/// ensures dashboards that aggregate `shim_invocations_total` get an accurate
/// per-call count for non-opted-out invocations.
///
/// # Telemetry flush on passthrough
///
/// On passthrough paths `exec.passthrough()` calls `execvp`, which replaces
/// the process image and never returns.  `Drop` glue and the flush in
/// `main.rs` are unreachable on those paths.  `telemetry_guard.force_flush_bounded()`
/// is called on every passthrough branch **before** `exec.passthrough()` so
/// the buffered metric reaches the OTLP endpoint.  The step-0 opt-out path
/// skips the flush entirely (no telemetry was initialized).
pub(crate) fn run_shim<E: EnvSource, G: RealGitExec>(
    argv: &[&str],
    env: &E,
    exec: &G,
    telemetry_guard: &mut TelemetryGuard,
) -> ExitCode {
    let metrics = crate::metrics::get();

    // 0. Per-invocation passthrough opt-out: exec the real binary immediately,
    //    skipping telemetry and classification entirely.
    if passthrough_opt_out_requested(env) {
        return exec.passthrough(argv);
    }

    // 1. Loop-break sentinel: a nested git call from within the shim.
    if env.get("GIT_PRISM_INSIDE_SHIM").is_some() {
        metrics.record_shim_invocation(ShimOutcome::LoopBreak);
        telemetry_guard.force_flush_bounded(PASSTHROUGH_FLUSH_TIMEOUT);
        return exec.passthrough(argv);
    }

    // 2. Only intercept when an AI agent is the caller.
    if crate::agent_detection::detect_calling_agent(env).is_none() {
        metrics.record_shim_invocation(ShimOutcome::NoAgent);
        telemetry_guard.force_flush_bounded(PASSTHROUGH_FLUSH_TIMEOUT);
        return exec.passthrough(argv);
    }

    // 3. Classify the subcommand.
    let classification = classify(argv);
    let subcommand = classification_to_subcommand(&classification);
    if classification == Classification::Passthrough {
        metrics.record_shim_invocation(ShimOutcome::Passthrough);
        telemetry_guard.force_flush_bounded(PASSTHROUGH_FLUSH_TIMEOUT);
        return exec.passthrough(argv);
    }

    // 4. Dispatch to the handler.
    let repo_path = match resolve_repo_path(env) {
        Some(p) => p,
        None => {
            metrics.record_shim_invocation(ShimOutcome::Passthrough);
            telemetry_guard.force_flush_bounded(PASSTHROUGH_FLUSH_TIMEOUT);
            return exec.passthrough(argv);
        }
    };
    // Only record classification once we are committed to structured dispatch.
    metrics.record_shim_classification(subcommand);
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
        Classification::FunctionContext { .. } => ShimSubcommand::Log, // git log -S/-G is still log
        Classification::ShowSnapshot { .. } => ShimSubcommand::Show,
        Classification::BlameSnapshot { .. } => ShimSubcommand::Blame,
        Classification::GhPrDiff { .. } => ShimSubcommand::Diff,
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

    // ---- passthrough_opt_out_requested unit tests ----

    #[test]
    fn opt_out_recognizes_passthrough_equals_one() {
        let env = MapEnv(HashMap::from([("GIT_PRISM_PASSTHROUGH", "1")]));
        assert!(passthrough_opt_out_requested(&env));
    }

    #[test]
    fn opt_out_recognizes_passthrough_equals_true_case_insensitive() {
        let env = MapEnv(HashMap::from([("GIT_PRISM_PASSTHROUGH", "TRUE")]));
        assert!(passthrough_opt_out_requested(&env));
    }

    #[test]
    fn opt_out_recognizes_disable_alias() {
        let env = MapEnv(HashMap::from([("GIT_PRISM_DISABLE", "1")]));
        assert!(passthrough_opt_out_requested(&env));
    }

    #[test]
    fn opt_out_inactive_when_var_is_zero() {
        let env = MapEnv(HashMap::from([("GIT_PRISM_PASSTHROUGH", "0")]));
        assert!(!passthrough_opt_out_requested(&env));
    }

    #[test]
    fn opt_out_inactive_when_var_is_false() {
        let env = MapEnv(HashMap::from([("GIT_PRISM_PASSTHROUGH", "false")]));
        assert!(!passthrough_opt_out_requested(&env));
    }

    #[test]
    fn opt_out_inactive_when_var_is_empty() {
        let env = MapEnv(HashMap::from([("GIT_PRISM_PASSTHROUGH", "")]));
        assert!(!passthrough_opt_out_requested(&env));
    }

    #[test]
    fn opt_out_inactive_when_var_is_unset() {
        let env = MapEnv(HashMap::new());
        assert!(!passthrough_opt_out_requested(&env));
    }

    /// With `GIT_PRISM_PASSTHROUGH=1`, the shim must exec the real binary
    /// immediately (before any classification or telemetry).
    #[test]
    fn it_passes_through_immediately_when_passthrough_env_var_is_set() {
        let env = MapEnv(HashMap::from([
            ("CLAUDECODE", "1"),
            ("GIT_PRISM_PASSTHROUGH", "1"),
        ]));
        let exec = SpyExec::new(ExitCode::SUCCESS);
        let mut guard = crate::telemetry::TelemetryGuard::noop();

        // Even though this is a classified command (diff), the opt-out must
        // cause an immediate passthrough before any classification runs.
        let code = run_shim(&["git", "diff", "HEAD~1..HEAD"], &env, &exec, &mut guard);

        assert!(
            exec.called.get(),
            "expected immediate passthrough when GIT_PRISM_PASSTHROUGH=1"
        );
        assert_eq!(code, ExitCode::SUCCESS);
    }

    /// `GIT_PRISM_DISABLE` is an alias for `GIT_PRISM_PASSTHROUGH`.
    #[test]
    fn it_passes_through_immediately_when_disable_alias_is_set() {
        let env = MapEnv(HashMap::from([
            ("CLAUDECODE", "1"),
            ("GIT_PRISM_DISABLE", "1"),
        ]));
        let exec = SpyExec::new(ExitCode::SUCCESS);
        let mut guard = crate::telemetry::TelemetryGuard::noop();

        run_shim(&["git", "diff", "HEAD~1..HEAD"], &env, &exec, &mut guard);

        assert!(
            exec.called.get(),
            "expected immediate passthrough when GIT_PRISM_DISABLE=1"
        );
    }

    /// With opt-out unset, a classified agent command must NOT passthrough immediately.
    #[test]
    fn it_does_not_passthrough_immediately_when_opt_out_var_is_unset() {
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

        let repo_str: &'static str =
            Box::leak(repo_path.to_string_lossy().into_owned().into_boxed_str());

        // No opt-out var — normal agent dispatch should run the handler.
        let env = MapEnv(HashMap::from([
            ("CLAUDECODE", "1"),
            ("GIT_PRISM_REPO", repo_str),
        ]));
        let exec = SpyExec::new(ExitCode::SUCCESS);
        let mut guard = crate::telemetry::TelemetryGuard::noop();

        run_shim(&["git", "diff", "HEAD~1..HEAD"], &env, &exec, &mut guard);

        assert!(
            !exec.called.get(),
            "without opt-out, a classified agent command must dispatch to the handler, not passthrough"
        );
    }

    #[test]
    fn it_passes_through_when_inside_shim_sentinel_is_set() {
        let env = MapEnv(HashMap::from([
            ("GIT_PRISM_INSIDE_SHIM", "1"),
            ("CLAUDECODE", "1"),
        ]));
        let exec = SpyExec::new(ExitCode::SUCCESS);
        let mut guard = crate::telemetry::TelemetryGuard::noop();

        run_shim(&["git", "diff", "main..HEAD"], &env, &exec, &mut guard);

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
        let mut guard = crate::telemetry::TelemetryGuard::noop();

        run_shim(&["git", "diff", "main..HEAD"], &env, &exec, &mut guard);

        assert!(
            exec.called.get(),
            "expected passthrough when no agent env var is set"
        );
    }

    #[test]
    fn it_passes_through_when_subcommand_is_not_on_watch_list() {
        let env = MapEnv(HashMap::from([("CLAUDECODE", "1")]));
        let exec = SpyExec::new(ExitCode::SUCCESS);
        let mut guard = crate::telemetry::TelemetryGuard::noop();

        run_shim(&["git", "status"], &env, &exec, &mut guard);

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
        let mut guard = crate::telemetry::TelemetryGuard::noop();

        run_shim(&["git", "diff", "main..HEAD"], &env, &exec, &mut guard);

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
        let mut guard = crate::telemetry::TelemetryGuard::noop();

        // git diff main..HEAD is a classified command that routes to handle_manifest.
        let code = run_shim(&["git", "diff", "HEAD~1..HEAD"], &env, &exec, &mut guard);

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
        let mut guard = crate::telemetry::TelemetryGuard::noop();

        // argv is a classified command so it would normally dispatch — but
        // the cwd failure must cause passthrough instead.
        run_shim(&["git", "diff", "main..HEAD"], &env, &exec, &mut guard);

        assert!(
            exec.called.get(),
            "expected passthrough when current directory cannot be determined"
        );
    }

    /// The structured-path and passthrough-path flush timeouts must be distinct
    /// values.  The structured path can afford a longer cap (the agent already
    /// received its JSON response) while the passthrough path is latency-critical.
    /// This test kills the mutation that collapses one constant to the other.
    #[test]
    fn structured_flush_timeout_differs_from_passthrough_flush_timeout() {
        assert_ne!(
            STRUCTURED_FLUSH_TIMEOUT, PASSTHROUGH_FLUSH_TIMEOUT,
            "structured and passthrough flush timeouts must be different values; \
             the structured path can afford a longer cap"
        );
        assert!(
            STRUCTURED_FLUSH_TIMEOUT > PASSTHROUGH_FLUSH_TIMEOUT,
            "structured-path timeout should be >= passthrough timeout \
             since the agent already has its response"
        );
    }

    /// `STRUCTURED_FLUSH_TIMEOUT` must exist and be reasonable (non-zero, shorter
    /// than `EXPORT_TIMEOUT` which is 5 s) so an uncapped flush cannot occur on
    /// the structured-output path.
    #[test]
    fn structured_flush_timeout_constant_is_positive_and_below_export_timeout() {
        // The constant must be > 0 (no instant-timeout) and < 5 s (below the
        // SDK export timeout) so a saturated collector cannot block git calls.
        assert!(
            STRUCTURED_FLUSH_TIMEOUT.as_millis() > 0,
            "STRUCTURED_FLUSH_TIMEOUT must be positive"
        );
        assert!(
            STRUCTURED_FLUSH_TIMEOUT.as_secs() < 5,
            "STRUCTURED_FLUSH_TIMEOUT must be below the 5 s SDK export timeout"
        );
    }

    /// `force_flush_bounded(STRUCTURED_FLUSH_TIMEOUT)` on an active guard with an
    /// unreachable OTLP endpoint must return within the structured-path deadline.
    /// This mirrors the passthrough-path black-hole test in telemetry.rs and proves
    /// the structured path cannot stall for minutes when the collector is saturated.
    #[tokio::test]
    async fn structured_path_bounded_flush_returns_within_deadline_on_black_hole_endpoint() {
        use std::sync::Mutex;
        static ENV_MUTEX: Mutex<()> = Mutex::new(());
        let _lock = ENV_MUTEX.lock().unwrap();
        // SAFETY: ENV_MUTEX is held — no concurrent env mutation.
        unsafe {
            std::env::set_var("GIT_PRISM_OTLP_ENDPOINT", "http://192.0.2.1:4318");
        }
        let mut guard = crate::telemetry::init_quiet();
        assert!(
            guard.is_active(),
            "guard must be active to exercise the structured-path bounded flush"
        );
        let start = std::time::Instant::now();
        guard.force_flush_bounded(STRUCTURED_FLUSH_TIMEOUT);
        let elapsed = start.elapsed();
        // SAFETY: cleanup
        unsafe {
            std::env::remove_var("GIT_PRISM_OTLP_ENDPOINT");
        }
        drop(guard);
        // Must return well before the 5 s SDK export timeout.
        assert!(
            elapsed.as_secs() < 5,
            "structured-path bounded flush must not stall for the full SDK export timeout; \
             took {elapsed:?}"
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
            classification_to_subcommand(&Classification::GhPrDiff { pr_number: "42" }),
            ShimSubcommand::Diff
        );
        assert_eq!(
            classification_to_subcommand(&Classification::Passthrough),
            ShimSubcommand::Other
        );
    }
}
