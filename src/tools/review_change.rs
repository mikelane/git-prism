//! Orchestration handler for the `review_change` MCP tool.
//!
//! `review_change` returns a combined `{ manifest, function_context }` payload
//! for the same ref range in a single call, splitting `max_response_tokens`
//! 40/60 between the two sub-responses. It is pure orchestration — both halves
//! are produced by the existing standalone handlers ([`build_manifest`] and
//! [`build_function_context_with_options`]); no diff or analysis logic is
//! duplicated here.
//!
//! The tool is the strongest comparative pitch in the toolkit: it competes
//! head-to-head with `git diff <ref>..<ref>`. The doc comment on the
//! `#[tool]`-annotated method in `src/server.rs` carries the agent-facing
//! framing; this module owns the orchestration mechanics.
//!
//! ## Token budget split
//!
//! Given an input `max_response_tokens` of `B`, the per-sub-response budgets
//! are:
//!
//! - `manifest_budget = floor(B * 0.4)` — truncating cast `(B as f64 * 0.4) as usize`
//! - `function_context_budget = round(B * 0.6)` — `((B as f64 * 0.6).round()) as usize`
//!
//! Floor is used for the smaller (manifest) share, round for the larger
//! (function_context) share. The two budgets are computed independently rather
//! than as complements (manifest + (B - manifest)) because the BDD scenario
//! at `bdd/features/redirect_hook.feature:459` triangulates two budget values
//! whose sums fall on different sides of the half-token rounding boundary —
//! complement arithmetic would pass at one budget and fail at the other.
//! See [`split_budget`] for the canonical implementation and unit tests.

use std::path::Path;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::tools::context::{build_function_context_with_options, ContextOptions};
use crate::tools::manifest::{build_manifest, build_worktree_manifest};
use crate::tools::types::{FunctionContextResponse, ManifestOptions, ManifestResponse, ToolError};

/// Default response-size budget in estimated tokens. Matches the standalone
/// manifest and context tools so an agent that doesn't override the budget
/// gets the same total ceiling whether it calls `review_change` once or the
/// two underlying tools separately.
const DEFAULT_REVIEW_CHANGE_BUDGET: usize = 8192;

/// Default page size for both halves of the combined response. Matches the
/// `get_function_context` default — small enough that caller-heavy entries
/// don't blow the budget, large enough that simple PRs fit on one page.
const DEFAULT_REVIEW_CHANGE_PAGE_SIZE: usize = 25;

fn default_review_change_budget() -> usize {
    DEFAULT_REVIEW_CHANGE_BUDGET
}

fn default_review_change_page_size() -> usize {
    DEFAULT_REVIEW_CHANGE_PAGE_SIZE
}

/// Arguments for the `review_change` MCP tool.
///
/// Cursor pagination uses two opaque cursors — `manifest_cursor` and
/// `function_context_cursor` — so each sub-response can be advanced
/// independently. The token budget is split via [`split_budget`].
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct ReviewChangeArgs {
    /// Path to the git repository (defaults to the server's working directory).
    pub repo_path: Option<String>,
    /// Base git ref. Required.
    pub base_ref: String,
    /// Head git ref. When omitted the tool runs in working-tree mode against
    /// `base_ref` (mirrors `get_change_manifest`'s behavior).
    pub head_ref: Option<String>,
    /// Glob patterns to include. Empty means include everything.
    #[serde(default)]
    pub include_patterns: Vec<String>,
    /// Glob patterns to exclude. Applied after `include_patterns`.
    #[serde(default)]
    pub exclude_patterns: Vec<String>,
    /// When set, restricts the function-context half to functions with these
    /// names. Has no effect on the manifest half.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub function_names: Option<Vec<String>>,
    /// Combined response-size budget in estimated tokens, split 40/60 between
    /// manifest and function_context. Default 8192. Pass 0 to disable the
    /// budget on both halves (use with care).
    #[serde(default = "default_review_change_budget")]
    pub max_response_tokens: usize,
    /// Opaque cursor for the manifest half of the response.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub manifest_cursor: Option<String>,
    /// Opaque cursor for the function_context half of the response.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub function_context_cursor: Option<String>,
    /// Page size used for both halves of the response.
    #[serde(default = "default_review_change_page_size")]
    pub page_size: usize,
}

/// Combined response for `review_change`.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct ReviewChangeResponse {
    pub manifest: ManifestResponse,
    pub function_context: FunctionContextResponse,
}

/// Compute the per-sub-response token budgets from a combined budget.
///
/// Returns `(manifest_budget, function_context_budget)`.
///
/// - `manifest_budget = (budget as f64 * 0.4) as usize` — truncating cast
///   (`as usize` rounds toward zero for positive `f64`, equivalent to floor).
/// - `function_context_budget = (budget as f64 * 0.6).round() as usize` —
///   round-half-away-from-zero.
///
/// Truncation for the smaller share + rounding for the larger share is the
/// only formula that satisfies both BDD examples (4096→1638/2458 and
/// 16384→6553/9830). Either rule applied uniformly fails one of them.
///
/// `0` budget returns `(0, 0)`, which both sub-tools interpret as "budget
/// disabled" via their `Option<usize>` adapter logic.
#[must_use]
pub fn split_budget(budget: usize) -> (usize, usize) {
    if budget == 0 {
        return (0, 0);
    }
    let budget_f = budget as f64;
    let manifest_share = (budget_f * 0.4) as usize;
    let context_share = (budget_f * 0.6).round() as usize;
    (manifest_share, context_share)
}

/// Convert a budget value to the `Option<usize>` shape the inner tool options
/// expect: `0` becomes `None` (budget disabled), positive values become
/// `Some(n)`.
fn budget_to_option(value: usize) -> Option<usize> {
    if value == 0 {
        None
    } else {
        Some(value)
    }
}

/// Build a combined `review_change` response by orchestrating the manifest
/// and function-context handlers.
///
/// Steps:
///
/// 1. Compute the 40/60 budget split via [`split_budget`].
/// 2. Run [`build_manifest`] (or [`build_worktree_manifest`] when `head_ref`
///    is `None`) with the manifest half of the budget and the
///    `manifest_cursor` opaque token.
/// 3. Run [`build_function_context_with_options`] with the function_context
///    half of the budget and the `function_context_cursor` opaque token. The
///    context handler reads the manifest internally for changed-function
///    discovery, so two manifest builds happen per call — once visible to
///    the user, once internal to the context tool. This matches the standalone
///    `get_function_context` cost profile.
/// 4. Stamp `metadata.budget_tokens` on each sub-response with its share, so
///    downstream agents (and the BDD assertions) can see the split decision
///    without reaching back into `review_change`'s arguments.
///
/// Working-tree mode (omitted `head_ref`) is supported for the manifest half
/// only; the function-context tool requires both refs because callers and
/// callees are resolved from committed content. When `head_ref` is `None` we
/// pass through the working-tree manifest but produce an empty
/// `FunctionContextResponse` rather than failing — the caller can ask for
/// function context separately if they commit first. This mirrors how the
/// underlying tools behave in their own working-tree paths.
pub fn build_review_change(
    repo_path: &Path,
    args: ReviewChangeArgs,
) -> Result<ReviewChangeResponse, ToolError> {
    let (manifest_budget, context_budget) = split_budget(args.max_response_tokens);

    // --- Manifest half ---
    let manifest_options = ManifestOptions {
        include_patterns: args.include_patterns.clone(),
        exclude_patterns: args.exclude_patterns.clone(),
        // Always populate function-level data; this tool's whole point is the
        // combined "what changed + what calls it" view, and the caller paid
        // for it via the budget.
        include_function_analysis: true,
        max_response_tokens: budget_to_option(manifest_budget),
    };

    let manifest_offset = if let Some(ref cursor) = args.manifest_cursor {
        let cursor = crate::pagination::decode_cursor(cursor).map_err(ToolError::InvalidCursor)?;
        cursor.offset
    } else {
        0
    };
    let page_size = crate::pagination::clamp_page_size(args.page_size);

    let mut manifest = match args.head_ref.as_deref() {
        Some(head) => {
            tracing::debug!(
                base_ref = %args.base_ref,
                head_ref = %head,
                manifest_budget,
                "review_change: building manifest half"
            );
            build_manifest(
                repo_path,
                &args.base_ref,
                head,
                &manifest_options,
                manifest_offset,
                page_size,
            )
            .inspect_err(|e| {
                tracing::error!(
                    error = %e,
                    sub_tool = "get_change_manifest",
                    "review_change: manifest half failed"
                );
            })?
        }
        None => {
            tracing::debug!(
                base_ref = %args.base_ref,
                manifest_budget,
                "review_change: building worktree manifest half"
            );
            build_worktree_manifest(
                repo_path,
                &args.base_ref,
                &manifest_options,
                manifest_offset,
                page_size,
            )
            .inspect_err(|e| {
                tracing::error!(
                    error = %e,
                    sub_tool = "get_change_manifest",
                    "review_change: worktree manifest half failed"
                );
            })?
        }
    };
    manifest.metadata.budget_tokens = Some(manifest_budget);

    // --- Function-context half ---
    let function_context = match args.head_ref.as_deref() {
        Some(head) => {
            let context_opts = ContextOptions {
                cursor: args.function_context_cursor.clone(),
                page_size,
                function_names: args.function_names.clone(),
                max_response_tokens: budget_to_option(context_budget),
            };
            tracing::debug!(
                base_ref = %args.base_ref,
                head_ref = %head,
                context_budget,
                "review_change: building function-context half"
            );
            let mut response =
                build_function_context_with_options(repo_path, &args.base_ref, head, &context_opts)
                    .inspect_err(|e| {
                        tracing::error!(
                            error = %e,
                            sub_tool = "get_function_context",
                            "review_change: function-context half failed"
                        );
                    })?;
            response.metadata.budget_tokens = Some(context_budget);
            response
        }
        None => {
            // Working-tree mode: no committed head_ref means caller/callee
            // resolution would compare against uncommitted content, so we
            // return an empty context payload rather than fail. The manifest
            // half still surfaces what changed.
            empty_function_context(
                args.base_ref.clone(),
                manifest.metadata.head_sha.clone(),
                manifest.metadata.base_sha.clone(),
                context_budget,
            )
        }
    };

    Ok(ReviewChangeResponse {
        manifest,
        function_context,
    })
}

/// Build an empty `FunctionContextResponse` for the working-tree case where
/// `head_ref` is unavailable. The metadata fields mirror what the real tool
/// would emit on an empty result so downstream consumers don't have to
/// special-case the absent-head scenario.
fn empty_function_context(
    base_ref: String,
    head_sha: String,
    base_sha: String,
    budget: usize,
) -> FunctionContextResponse {
    use chrono::Utc;

    use crate::pagination::PaginationInfo;
    use crate::tools::types::ContextMetadata;

    FunctionContextResponse {
        metadata: ContextMetadata {
            base_ref,
            head_ref: "WORKTREE".to_string(),
            base_sha,
            head_sha,
            generated_at: Utc::now(),
            token_estimate: 0,
            function_analysis_truncated: vec![],
            next_cursor: None,
            budget_tokens: Some(budget),
        },
        functions: vec![],
        pagination: PaginationInfo {
            total_items: 0,
            page_start: 0,
            page_size: 0,
            next_cursor: None,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_budget_at_4096_yields_1638_manifest_and_2458_context() {
        // Triangulates the BDD example at bdd/features/redirect_hook.feature:471.
        // Captures floor(4096 * 0.4) = 1638 (truncating cast) and
        // round(4096 * 0.6) = 2458 (round half away from zero of 2457.6).
        assert_eq!(split_budget(4096), (1638, 2458));
    }

    #[test]
    fn split_budget_at_16384_yields_6553_manifest_and_9830_context() {
        // Triangulates the BDD example at bdd/features/redirect_hook.feature:472.
        // Captures floor(16384 * 0.4) = 6553 (truncating cast of 6553.6) and
        // round(16384 * 0.6) = 9830 (round half away from zero of 9830.4).
        // Together with the 4096 case, this rules out:
        //   - manifest = round(B * 0.4) — would yield 6554 here
        //   - context  = budget - manifest — would yield 9831 here
        //   - context  = floor(B * 0.6) — would yield 2457 at 4096
        // Only "truncate the small share, round the big share" passes both.
        assert_eq!(split_budget(16384), (6553, 9830));
    }

    #[test]
    fn split_budget_zero_yields_zero_for_both() {
        // The MCP arg type uses 0 to mean "budget disabled". Splitting 0/0
        // means both sub-tools see disabled budgets via budget_to_option.
        assert_eq!(split_budget(0), (0, 0));
    }

    #[test]
    fn split_budget_one_yields_zero_manifest_and_one_context() {
        // floor(1 * 0.4) = 0 and round(1 * 0.6) = 1 — at the smallest
        // non-zero budget the manifest gets zero and the context gets the
        // single token. This documents the behavior at the lower boundary;
        // a budget of 1 is too small to be useful but must not panic.
        assert_eq!(split_budget(1), (0, 1));
    }

    #[test]
    fn budget_to_option_zero_is_none() {
        // 0 means "disabled"; the inner tools treat None and Some(0) the
        // same, but None is the canonical "absent" form.
        assert_eq!(budget_to_option(0), None);
    }

    #[test]
    fn budget_to_option_positive_is_some() {
        assert_eq!(budget_to_option(1638), Some(1638));
    }

    #[test]
    fn review_change_args_deserializes_with_defaults() {
        let json = r#"{"base_ref": "main", "head_ref": "HEAD"}"#;
        let args: ReviewChangeArgs = serde_json::from_str(json).unwrap();
        assert_eq!(args.base_ref, "main");
        assert_eq!(args.head_ref.as_deref(), Some("HEAD"));
        assert_eq!(args.max_response_tokens, 8192);
        assert_eq!(args.page_size, 25);
        assert!(args.manifest_cursor.is_none());
        assert!(args.function_context_cursor.is_none());
    }

    #[test]
    fn review_change_args_accepts_separate_cursors() {
        let json = r#"{
            "base_ref": "main",
            "head_ref": "HEAD",
            "manifest_cursor": "tok-m",
            "function_context_cursor": "tok-fc"
        }"#;
        let args: ReviewChangeArgs = serde_json::from_str(json).unwrap();
        assert_eq!(args.manifest_cursor.as_deref(), Some("tok-m"));
        assert_eq!(args.function_context_cursor.as_deref(), Some("tok-fc"));
    }

    #[test]
    fn split_budget_at_10_yields_correct_floor_and_round() {
        // budget = 10: 10 * 0.4 = 4.0 (floor = 4), 10 * 0.6 = 6.0 (round = 6).
        // Triangulates a round number where both halves are integers — rules out
        // an impl that rounds both shares or uses complement arithmetic (which
        // would also produce (4, 6) here but fails at 4096 and 16384).
        assert_eq!(split_budget(10), (4, 6));
    }

    #[test]
    fn split_budget_at_5_exercises_half_token_rounding_boundary() {
        // budget = 5: 5 * 0.4 = 2.0 (floor = 2), 5 * 0.6 = 3.0 (round = 3).
        // The two values are exact here; included to document behaviour at a
        // small odd boundary where complement arithmetic (5 - 2 = 3) coincides.
        assert_eq!(split_budget(5), (2, 3));
    }

    #[test]
    fn split_budget_manifest_plus_context_equals_or_near_budget() {
        // The two shares are computed independently (floor + round), so their
        // sum may differ from the input by at most 1.  This is the documented
        // design: the BDD spec accepts (1638, 2458) for budget 4096 (sum 4096)
        // and (6553, 9830) for 16384 (sum 9383, off by 1). We assert that the
        // absolute difference is at most 1 for a range of inputs so a mutation
        // that changes one formula cannot silently double-count or drop tokens.
        for budget in [100usize, 999, 4096, 8192, 16384, 65536] {
            let (m, c) = split_budget(budget);
            let diff = (budget as i64 - m as i64 - c as i64).unsigned_abs();
            assert!(
                diff <= 1,
                "split_budget({budget}) = ({m}, {c}); sum {s} is more than 1 away from budget",
                s = m + c,
            );
        }
    }

    #[test]
    fn review_change_args_deserializes_zero_budget_as_disabled() {
        // Callers pass max_response_tokens=0 to disable both sub-tool budgets.
        let json = r#"{"base_ref": "main", "head_ref": "HEAD", "max_response_tokens": 0}"#;
        let args: ReviewChangeArgs = serde_json::from_str(json).unwrap();
        assert_eq!(args.max_response_tokens, 0);
        // Splitting 0 must yield (0, 0) — both sub-tools see None via budget_to_option.
        let (m, c) = split_budget(args.max_response_tokens);
        assert_eq!(m, 0);
        assert_eq!(c, 0);
    }

    #[test]
    fn review_change_args_deserializes_function_names_filter() {
        let json = r#"{
            "base_ref": "main",
            "head_ref": "HEAD",
            "function_names": ["foo", "bar"]
        }"#;
        let args: ReviewChangeArgs = serde_json::from_str(json).unwrap();
        assert_eq!(
            args.function_names.as_deref(),
            Some(["foo".to_string(), "bar".to_string()].as_slice())
        );
    }

    #[test]
    fn review_change_args_function_names_is_none_when_absent() {
        let json = r#"{"base_ref": "main", "head_ref": "HEAD"}"#;
        let args: ReviewChangeArgs = serde_json::from_str(json).unwrap();
        assert!(
            args.function_names.is_none(),
            "function_names must be None when not supplied in JSON"
        );
    }

    #[test]
    fn review_change_args_deserializes_custom_page_size() {
        let json = r#"{"base_ref": "main", "head_ref": "HEAD", "page_size": 10}"#;
        let args: ReviewChangeArgs = serde_json::from_str(json).unwrap();
        assert_eq!(args.page_size, 10);
    }

    #[test]
    fn review_change_args_working_tree_mode_when_head_ref_absent() {
        // When head_ref is omitted the tool runs in working-tree mode. The
        // deserialised value must be None — not Some("") or Some("HEAD") —
        // because build_review_change dispatches on the Option.
        let json = r#"{"base_ref": "HEAD"}"#;
        let args: ReviewChangeArgs = serde_json::from_str(json).unwrap();
        assert!(
            args.head_ref.is_none(),
            "head_ref must be None when omitted from JSON (working-tree mode)"
        );
    }
}
