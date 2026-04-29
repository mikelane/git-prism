//! Response-size estimation helpers shared by the MCP tool handlers.
//!
//! These live in their own module so the pagination / token-budget work in
//! later PRs of issue #212 can import a single canonical estimate function
//! instead of re-deriving the "~4 characters per token" rule at every call
//! site. Keeping the function free (rather than a method on a type) matches
//! the rest of `src/tools/` where pure utilities are free functions and stateful
//! orchestration lives on structs.

use serde::Serialize;

/// Estimate the number of tokens in a serialized JSON payload from its character
/// count, using the standard "~4 characters per token" heuristic.
///
/// `char_count` is the length of the UTF-8 JSON payload in bytes or characters —
/// for ASCII-heavy JSON the two are equivalent, and the helper deliberately
/// accepts whichever the caller has cheapest to compute. Uses integer division
/// (floor), which matches the existing inline implementation in
/// [`crate::tools::snapshots`] so the refactor there is a behavior-preserving
/// swap.
#[must_use]
pub fn estimate_tokens(char_count: usize) -> usize {
    char_count / 4
}

/// Estimate tokens for any `Serialize` value by serializing it to JSON and
/// applying [`estimate_tokens`] to the resulting string length.
///
/// Returns `0` if serialization fails. For types that derive `Serialize`
/// cleanly this is effectively unreachable, but we prefer a degraded budgeting
/// hint over a panic at the metric boundary. Callers that care about real
/// serialization errors should serialize themselves — this helper is meant
/// to be called right before returning a response struct that will be
/// serialized anyway, where a failure here would also break the actual
/// response.
#[must_use]
pub fn estimate_response_tokens<T: Serialize>(value: &T) -> usize {
    serde_json::to_string(value)
        .map(|s| estimate_tokens(s.len()))
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn it_returns_zero_for_zero_chars() {
        assert_eq!(estimate_tokens(0), 0);
    }

    #[test]
    fn it_rounds_down_three_chars_to_zero_tokens() {
        assert_eq!(estimate_tokens(3), 0);
    }

    #[test]
    fn it_counts_four_chars_as_one_token() {
        assert_eq!(estimate_tokens(4), 1);
    }

    #[test]
    fn it_rounds_down_seven_chars_to_one_token() {
        assert_eq!(estimate_tokens(7), 1);
    }

    #[test]
    fn it_counts_eight_chars_as_two_tokens() {
        assert_eq!(estimate_tokens(8), 2);
    }

    #[test]
    fn it_counts_one_hundred_chars_as_twenty_five_tokens() {
        assert_eq!(estimate_tokens(100), 25);
    }

    #[test]
    fn it_estimates_tokens_for_a_serializable_struct() {
        #[derive(Serialize)]
        struct Payload {
            name: &'static str,
            count: usize,
        }
        let payload = Payload {
            name: "manifest",
            count: 42,
        };
        let serialized = serde_json::to_string(&payload).unwrap();
        let expected = estimate_tokens(serialized.len());

        assert_eq!(estimate_response_tokens(&payload), expected);
    }

    #[test]
    fn it_returns_a_positive_estimate_for_a_non_trivial_struct() {
        #[derive(Serialize)]
        struct Payload {
            items: Vec<&'static str>,
        }
        let payload = Payload {
            items: vec!["alpha", "beta", "gamma", "delta"],
        };

        // "items":["alpha","beta","gamma","delta"] serializes to well over 4
        // characters, so the estimate must be strictly positive and match the
        // direct computation on the serialized string.
        let serialized = serde_json::to_string(&payload).unwrap();
        assert!(serialized.len() >= 4);
        assert!(estimate_response_tokens(&payload) > 0);
        assert_eq!(
            estimate_response_tokens(&payload),
            estimate_tokens(serialized.len())
        );
    }
}
