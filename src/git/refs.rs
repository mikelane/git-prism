//! Ref-range parsing for git-prism CLI and shim.
//!
//! Centralised here so both `main.rs` subcommands and `shim/handlers.rs`
//! can parse user-supplied range strings without duplicating logic.

/// A parsed git ref range.
pub(crate) enum RefRange<'a> {
    /// A range between two refs (e.g. "main..HEAD", "HEAD~1..HEAD").
    CommitRange { base: &'a str, head: &'a str },
    /// A single ref compared against the working tree (e.g. "HEAD").
    WorktreeCompare { base: &'a str },
}

/// Parse a ref-range string into a `RefRange`.
///
/// Three-dot ranges (`a...b`) are normalised to two-dot commit ranges.
/// A bare ref with no `..` becomes a `WorktreeCompare`.
pub(crate) fn parse_range(range: &str) -> RefRange<'_> {
    if let Some((base, head)) = range.split_once("...") {
        RefRange::CommitRange {
            base,
            head: if head.is_empty() { "HEAD" } else { head },
        }
    } else if let Some((base, head)) = range.split_once("..") {
        RefRange::CommitRange {
            base,
            head: if head.is_empty() { "HEAD" } else { head },
        }
    } else {
        RefRange::WorktreeCompare { base: range }
    }
}

/// Return an error if `range` is a working-tree compare (not a commit range).
///
/// Used by CLI subcommands that only accept commit-to-commit ranges.
pub(crate) fn validate_commit_range(range: &RefRange<'_>, subcommand: &str) -> anyhow::Result<()> {
    match range {
        RefRange::WorktreeCompare { .. } => {
            anyhow::bail!(
                "{subcommand} does not support working tree mode — use a commit range (e.g., HEAD~1..HEAD)"
            )
        }
        RefRange::CommitRange { .. } => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn it_parses_range_with_double_dot() {
        let result = parse_range("main..HEAD");
        assert!(matches!(
            result,
            RefRange::CommitRange {
                base: "main",
                head: "HEAD"
            }
        ));
    }

    #[test]
    fn it_parses_bare_ref_as_worktree_compare() {
        let result = parse_range("abc1234");
        assert!(matches!(
            result,
            RefRange::WorktreeCompare { base: "abc1234" }
        ));
    }

    #[test]
    fn it_parses_head_as_worktree_compare() {
        let result = parse_range("HEAD");
        assert!(matches!(result, RefRange::WorktreeCompare { base: "HEAD" }));
    }

    #[test]
    fn it_parses_head_tilde_range() {
        let result = parse_range("HEAD~3..HEAD");
        assert!(matches!(
            result,
            RefRange::CommitRange {
                base: "HEAD~3",
                head: "HEAD"
            }
        ));
    }

    #[test]
    fn it_parses_three_dot_range() {
        let result = parse_range("main...HEAD");
        assert!(matches!(
            result,
            RefRange::CommitRange {
                base: "main",
                head: "HEAD"
            }
        ));
    }

    #[test]
    fn it_parses_three_dot_range_with_empty_head_as_head() {
        let result = parse_range("main...");
        assert!(matches!(
            result,
            RefRange::CommitRange {
                base: "main",
                head: "HEAD"
            }
        ));
    }

    #[test]
    fn it_rejects_worktree_mode_for_history_command() {
        let range = "HEAD";
        let ref_range = parse_range(range);
        let err = validate_commit_range(&ref_range, "history");
        assert!(err.is_err());
        let msg = err.unwrap_err().to_string();
        assert!(
            msg.contains("does not support working tree mode"),
            "expected 'does not support working tree mode' in: {msg}"
        );
    }

    #[test]
    fn it_accepts_commit_range_for_history_command() {
        let range = "HEAD~3..HEAD";
        let ref_range = parse_range(range);
        let result = validate_commit_range(&ref_range, "history");
        assert!(result.is_ok());
    }

    #[test]
    fn it_rejects_worktree_mode_for_snapshot_command() {
        let range = "HEAD";
        let ref_range = parse_range(range);
        let err = validate_commit_range(&ref_range, "snapshot");
        assert!(err.is_err());
        let msg = err.unwrap_err().to_string();
        assert!(
            msg.contains("does not support working tree mode"),
            "expected 'does not support working tree mode' in: {msg}"
        );
    }

    #[test]
    fn it_accepts_commit_range_for_snapshot_command() {
        let range = "HEAD~1..HEAD";
        let ref_range = parse_range(range);
        let result = validate_commit_range(&ref_range, "snapshot");
        assert!(result.is_ok());
    }
}
