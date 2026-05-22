//! Opt-in shadow-run capture for token-savings instrumentation.
//!
//! After the structured response is flushed to the agent, an optional shadow
//! run captures the raw `git` byte length for the same invocation so dashboards
//! can compute how many bytes (and approximate tokens) the shim saved.
//!
//! Shadow runs are gated by `GIT_PRISM_SHADOW_SAMPLE_PCT` (integer 0–100,
//! default 0).  The default disables shadow runs entirely, adding zero latency
//! or overhead to normal operation.

use crate::agent_detection::EnvSource;
use crate::metrics::{self, ShimSubcommand};
use crate::shim::real_git::RealGitExec;

/// Parse `GIT_PRISM_SHADOW_SAMPLE_PCT` from the environment.
///
/// Returns a value in `0..=100`:
/// - Missing var or empty string → 0 (disabled)
/// - Non-integer string → 0 (warn and disable)
/// - Negative → clamped to 0
/// - > 100 → clamped to 100
pub(crate) fn parse_sample_pct(env: &dyn EnvSource) -> u8 {
    let s = match env.get("GIT_PRISM_SHADOW_SAMPLE_PCT") {
        None => return 0,
        Some(s) if s.is_empty() => return 0,
        Some(s) => s,
    };
    let s_trimmed = s.trim();
    if s_trimmed.is_empty() {
        return 0;
    }
    match s_trimmed.parse::<i64>() {
        Ok(n) => n.clamp(0, 100) as u8,
        Err(e) if matches!(e.kind(), std::num::IntErrorKind::PosOverflow) => 100,
        Err(e) if matches!(e.kind(), std::num::IntErrorKind::NegOverflow) => 0,
        Err(_) => {
            tracing::warn!(
                value = %s,
                "GIT_PRISM_SHADOW_SAMPLE_PCT is not a valid integer; shadow runs disabled"
            );
            0
        }
    }
}

/// Maybe run a shadow git invocation and record the output byte length.
///
/// Decision logic:
/// 1. Read `GIT_PRISM_SHADOW_SAMPLE_PCT`.  If 0, return immediately — no overhead.
/// 2. Roll a random u32 in `0..100`.  If `roll >= sample_pct`, skip.
/// 3. Execute `argv` via `exec` with stdout captured into a buffer.
/// 4. Record the buffer length as `shim_shadow_git_bytes{git_subcommand}`.
///
/// The buffer is dropped immediately after recording — we only need its length.
///
/// # Sampling bias note
///
/// We use `rand::random::<u32>() % 100` rather than `rand::random::<u8>() % 100`.
/// A u8 has 256 values; 256 % 100 = 56, so rolls 0–55 each have 3 preimages while
/// rolls 56–99 have only 2 — a 17% relative bias at sample_pct=50.  With u32
/// (4 294 967 296 values; bias = 96 / 4 294 967 296 ≈ 2e-8), the distortion is
/// negligible.  This is non-cryptographic sampling — use a CSPRNG if you need
/// security properties.
pub(crate) fn maybe_shadow_capture<E: EnvSource, G: RealGitExec>(
    env: &E,
    subcommand: ShimSubcommand,
    argv: &[&str],
    exec: &G,
) {
    let sample_pct = parse_sample_pct(env);
    if sample_pct == 0 {
        return;
    }

    let roll = (rand::random::<u32>() % 100) as u8;
    if roll >= sample_pct {
        return;
    }

    match exec.capture(argv) {
        Ok(bytes) => {
            metrics::get().record_shim_shadow_git_bytes(subcommand, bytes as u64);
        }
        Err(e) => {
            tracing::warn!(error = %e, "shadow git capture failed");
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    struct MapEnv(HashMap<&'static str, &'static str>);

    impl EnvSource for MapEnv {
        fn get(&self, key: &str) -> Option<String> {
            self.0.get(key).map(|v| v.to_string())
        }
    }

    fn env_with(pct: &'static str) -> MapEnv {
        MapEnv(HashMap::from([("GIT_PRISM_SHADOW_SAMPLE_PCT", pct)]))
    }

    fn empty_env() -> MapEnv {
        MapEnv(HashMap::new())
    }

    // --- parse_sample_pct exhaustive coverage ---

    #[test]
    fn missing_var_returns_zero() {
        assert_eq!(parse_sample_pct(&empty_env()), 0);
    }

    #[test]
    fn empty_string_returns_zero() {
        assert_eq!(parse_sample_pct(&env_with("")), 0);
    }

    #[test]
    fn zero_string_returns_zero() {
        assert_eq!(parse_sample_pct(&env_with("0")), 0);
    }

    #[test]
    fn hundred_string_returns_hundred() {
        assert_eq!(parse_sample_pct(&env_with("100")), 100);
    }

    #[test]
    fn negative_value_clamps_to_zero() {
        assert_eq!(parse_sample_pct(&env_with("-5")), 0);
    }

    #[test]
    fn over_hundred_clamps_to_hundred() {
        assert_eq!(parse_sample_pct(&env_with("200")), 100);
    }

    #[test]
    fn non_integer_returns_zero() {
        assert_eq!(parse_sample_pct(&env_with("abc")), 0);
    }

    #[test]
    fn mid_range_value_passes_through() {
        assert_eq!(parse_sample_pct(&env_with("50")), 50);
        assert_eq!(parse_sample_pct(&env_with("1")), 1);
        assert_eq!(parse_sample_pct(&env_with("99")), 99);
    }

    // --- CountingExec spy for sampling tests ---

    struct CountingExec {
        capture_calls: AtomicUsize,
        passthrough_calls: AtomicUsize,
        stdout_len: usize,
    }

    impl CountingExec {
        fn new(stdout_len: usize) -> Self {
            Self {
                capture_calls: AtomicUsize::new(0),
                passthrough_calls: AtomicUsize::new(0),
                stdout_len,
            }
        }
    }

    impl RealGitExec for CountingExec {
        fn passthrough(&self, _argv: &[&str]) -> std::process::ExitCode {
            self.passthrough_calls.fetch_add(1, Ordering::SeqCst);
            std::process::ExitCode::SUCCESS
        }

        fn capture(&self, _argv: &[&str]) -> Result<usize, String> {
            self.capture_calls.fetch_add(1, Ordering::SeqCst);
            Ok(self.stdout_len)
        }
    }

    // --- AC-required sampling tests ---

    #[test]
    fn sample_pct_100_always_calls_capture() {
        // SAMPLE_PCT=100 → capture() must be called on every invocation.
        let env = env_with("100");
        let exec = CountingExec::new(42);
        maybe_shadow_capture(&env, ShimSubcommand::Diff, &["git", "diff"], &exec);
        assert_eq!(
            exec.capture_calls.load(Ordering::SeqCst),
            1,
            "SAMPLE_PCT=100 must call capture() exactly once"
        );
        assert_eq!(
            exec.passthrough_calls.load(Ordering::SeqCst),
            0,
            "shadow path must not call passthrough()"
        );
    }

    #[test]
    fn sample_pct_0_never_calls_capture() {
        // SAMPLE_PCT=0 (default) → capture() must never be called regardless
        // of how many times maybe_shadow_capture is invoked.
        let env = env_with("0");
        let exec = CountingExec::new(42);
        for _ in 0..100 {
            maybe_shadow_capture(&env, ShimSubcommand::Diff, &["git", "diff"], &exec);
        }
        assert_eq!(
            exec.capture_calls.load(Ordering::SeqCst),
            0,
            "SAMPLE_PCT=0 must never call capture()"
        );
    }

    #[test]
    fn overflow_value_clamps_to_hundred() {
        assert_eq!(parse_sample_pct(&env_with("99999999999999999999999")), 100);
    }

    #[test]
    fn surrounding_whitespace_is_trimmed() {
        assert_eq!(parse_sample_pct(&env_with(" 50 ")), 50);
    }

    #[test]
    fn decimal_value_returns_zero() {
        // Decimal is unparseable as integer — disable (don't accept fractional pct)
        assert_eq!(parse_sample_pct(&env_with("50.5")), 0);
    }

    #[test]
    fn whitespace_only_returns_zero() {
        assert_eq!(parse_sample_pct(&env_with("   ")), 0);
    }
}
