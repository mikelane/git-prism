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
    match s.parse::<i64>() {
        Ok(n) => n.clamp(0, 100) as u8,
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
/// 2. Roll a random u8 in `0..100`.  If `roll >= sample_pct`, skip.
/// 3. Execute `argv` via `exec` with stdout captured into a buffer.
/// 4. Record the buffer length as `shim_shadow_git_bytes{git_subcommand}`.
///
/// The buffer is dropped immediately after recording — we only need its length.
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

    let roll = rand::random::<u8>() % 100;
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
}
