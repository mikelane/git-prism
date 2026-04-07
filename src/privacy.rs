use std::path::Path;

use schemars::JsonSchema;
use serde::Serialize;
use sha2::{Digest, Sha256};

/// SHA-256 hash of the canonicalized absolute path, hex-encoded.
/// Deterministic: same path always produces the same hash.
///
/// If canonicalization fails (e.g., the path doesn't exist on disk), the path
/// is hashed as-is. This means two different string representations of the same
/// filesystem path (e.g., with and without trailing slash, or relative vs absolute)
/// may produce different hashes when the path is not resolvable.
pub fn hash_repo_path(path: &Path) -> String {
    let canonical = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    let mut hasher = Sha256::new();
    hasher.update(canonical.to_string_lossy().as_bytes());
    format!("{:x}", hasher.finalize())
}

/// Bounded enum representing the pattern of a git ref string.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RefPattern {
    Worktree,
    SingleCommit,
    RangeDoubleDot,
    RangeTripleDot,
    Branch,
    Sha,
}

impl RefPattern {
    pub fn as_str(&self) -> &'static str {
        match self {
            RefPattern::Worktree => "worktree",
            RefPattern::SingleCommit => "single_commit",
            RefPattern::RangeDoubleDot => "range_double_dot",
            RefPattern::RangeTripleDot => "range_triple_dot",
            RefPattern::Branch => "branch",
            RefPattern::Sha => "sha",
        }
    }
}

/// Classify a raw ref string into a bounded `RefPattern`.
///
/// Note: `RefPattern::Worktree` is never returned by this function — the caller
/// sets it based on context (e.g., when there is no head_ref).
pub fn normalize_ref_pattern(ref_str: &str) -> RefPattern {
    if ref_str.contains("...") {
        return RefPattern::RangeTripleDot;
    }
    if ref_str.contains("..") {
        return RefPattern::RangeDoubleDot;
    }
    if is_hex_sha(ref_str) {
        return RefPattern::Sha;
    }
    if ref_str.contains('~') || ref_str.contains('^') {
        return RefPattern::SingleCommit;
    }
    if ref_str == "HEAD" {
        return RefPattern::SingleCommit;
    }
    RefPattern::Branch
}

/// Best-effort heuristic to detect hex SHA strings.
///
/// Accepts exactly 40 characters (full SHA-1) or >= 12 all-hex characters as a
/// short SHA. The 12-char minimum reduces false positives from branch names that
/// happen to be valid hex (e.g., `deadbeef`, `cafebabe`). Git's default
/// abbreviation is 7 characters, but real-world short SHAs passed to tools are
/// typically 12+. There is inherent ambiguity — a 12+ hex-char branch name would
/// still be misclassified — but this is acceptable for telemetry categorization.
fn is_hex_sha(s: &str) -> bool {
    let len = s.len();
    (len == 40 || len >= 12) && s.chars().all(|c| c.is_ascii_hexdigit())
}

/// Classify the ref pattern for metrics based on split ref arguments.
///
/// MCP tool args arrive pre-split (no `..` syntax), so we can't distinguish
/// `..` from `...`. When `head_ref` is `None`, the mode is `"worktree"`.
/// When both refs are present, we classify `base_ref` using
/// `normalize_ref_pattern` to get a spec-compliant label (`branch`, `sha`,
/// `single_commit`, etc.).
pub fn classify_ref_mode(base_ref: &str, head_ref: Option<&str>) -> &'static str {
    match head_ref {
        None => "worktree",
        Some(_) => normalize_ref_pattern(base_ref).as_str(),
    }
}

/// Maps error description strings to a bounded label set for metrics.
pub fn classify_error_kind(err: &str) -> &'static str {
    let lower = err.to_lowercase();
    if lower.contains("resolve") || lower.contains("ref not found") || lower.contains("invalid ref")
    {
        "ref_not_found"
    } else if lower.contains("repository")
        || lower.contains("repo not found")
        || lower.contains("not a git")
        || lower.contains("open repo")
    {
        "repo_not_found"
    } else if lower.contains("diff") {
        "diff_failed"
    } else if lower.contains("parse") || lower.contains("tree-sitter") {
        "parse_failed"
    } else if lower.contains("i/o") || lower.contains("io error") || lower.contains("permission") {
        "io_error"
    } else {
        "unknown"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_hash_repo_path_deterministic() {
        let path = Path::new("/tmp/some-repo");
        let hash1 = hash_repo_path(path);
        let hash2 = hash_repo_path(path);
        assert_eq!(hash1, hash2);
        // SHA-256 hex is 64 chars
        assert_eq!(hash1.len(), 64);
    }

    #[test]
    fn test_hash_repo_path_different_paths_differ() {
        let hash1 = hash_repo_path(Path::new("/tmp/repo-a"));
        let hash2 = hash_repo_path(Path::new("/tmp/repo-b"));
        assert_ne!(hash1, hash2);
    }

    #[test]
    fn test_normalize_double_dot_range() {
        assert_eq!(
            normalize_ref_pattern("main..HEAD"),
            RefPattern::RangeDoubleDot
        );
    }

    #[test]
    fn test_normalize_triple_dot_range() {
        assert_eq!(
            normalize_ref_pattern("main...HEAD"),
            RefPattern::RangeTripleDot
        );
    }

    #[test]
    fn test_normalize_sha() {
        let sha40 = "a".repeat(40);
        assert_eq!(normalize_ref_pattern(&sha40), RefPattern::Sha);
    }

    #[test]
    fn test_normalize_short_sha() {
        // 12+ hex chars are classified as short SHAs
        assert_eq!(normalize_ref_pattern("abc1234def56"), RefPattern::Sha);
        // 7-char and 8-char hex strings are now Branch (below 12-char threshold)
        assert_eq!(normalize_ref_pattern("abc1234"), RefPattern::Branch);
        assert_eq!(normalize_ref_pattern("deadbeef"), RefPattern::Branch);
    }

    #[test]
    fn test_normalize_head() {
        assert_eq!(normalize_ref_pattern("HEAD"), RefPattern::SingleCommit);
    }

    #[test]
    fn test_normalize_head_tilde() {
        assert_eq!(normalize_ref_pattern("HEAD~3"), RefPattern::SingleCommit);
    }

    #[test]
    fn test_normalize_head_caret() {
        assert_eq!(normalize_ref_pattern("HEAD^2"), RefPattern::SingleCommit);
    }

    #[test]
    fn test_normalize_branch() {
        assert_eq!(normalize_ref_pattern("main"), RefPattern::Branch);
        assert_eq!(normalize_ref_pattern("feature/foo"), RefPattern::Branch);
    }

    #[test]
    fn test_classify_error_ref_not_found() {
        assert_eq!(
            classify_error_kind("could not resolve ref"),
            "ref_not_found"
        );
        assert_eq!(classify_error_kind("ref not found"), "ref_not_found");
    }

    #[test]
    fn test_classify_error_unknown() {
        assert_eq!(classify_error_kind("something went wrong"), "unknown");
        assert_eq!(classify_error_kind("totally random"), "unknown");
    }

    #[test]
    fn test_normalize_ref_pattern_edge_cases() {
        // Empty string falls through to Branch
        assert_eq!(normalize_ref_pattern(""), RefPattern::Branch);
        // Full ref paths are Branch
        assert_eq!(normalize_ref_pattern("refs/heads/main"), RefPattern::Branch);
        // Remote-tracking refs are Branch
        assert_eq!(normalize_ref_pattern("origin/main"), RefPattern::Branch);
        // Tags are Branch (no special tag variant)
        assert_eq!(normalize_ref_pattern("v1.0.0"), RefPattern::Branch);
    }

    #[test]
    fn test_classify_error_no_false_positives() {
        // "refactoring" should NOT match "ref not found" patterns
        assert_eq!(classify_error_kind("refactoring in progress"), "unknown");
        // "reproduce" should NOT match "repo" patterns
        assert_eq!(classify_error_kind("could not reproduce"), "unknown");
    }

    // Tests to kill || -> && mutants in classify_error_kind.
    // Each test exercises exactly ONE alternative in each || chain.

    // Line 99: resolve || ref not found || invalid ref
    #[test]
    fn it_classifies_resolve_alone_as_ref_not_found() {
        assert_eq!(classify_error_kind("could not resolve"), "ref_not_found");
    }

    #[test]
    fn it_classifies_ref_not_found_alone() {
        assert_eq!(
            classify_error_kind("ref not found in repo"),
            "ref_not_found"
        );
    }

    #[test]
    fn it_classifies_invalid_ref_alone() {
        assert_eq!(
            classify_error_kind("invalid ref specified"),
            "ref_not_found"
        );
    }

    // Lines 102-105: repository || repo not found || not a git || open repo
    #[test]
    fn it_classifies_repository_alone_as_repo_not_found() {
        assert_eq!(classify_error_kind("bad repository path"), "repo_not_found");
    }

    #[test]
    fn it_classifies_repo_not_found_alone() {
        assert_eq!(classify_error_kind("repo not found here"), "repo_not_found");
    }

    #[test]
    fn it_classifies_not_a_git_alone() {
        assert_eq!(classify_error_kind("not a git directory"), "repo_not_found");
    }

    #[test]
    fn it_classifies_open_repo_alone() {
        assert_eq!(classify_error_kind("failed to open repo"), "repo_not_found");
    }

    // Line 110: parse || tree-sitter
    #[test]
    fn it_classifies_parse_alone_as_parse_failed() {
        assert_eq!(classify_error_kind("failed to parse file"), "parse_failed");
    }

    #[test]
    fn it_classifies_tree_sitter_alone_as_parse_failed() {
        assert_eq!(
            classify_error_kind("tree-sitter error occurred"),
            "parse_failed"
        );
    }

    // Line 112: i/o || io error || permission
    #[test]
    fn it_classifies_io_slash_alone_as_io_error() {
        assert_eq!(classify_error_kind("an i/o failure"), "io_error");
    }

    #[test]
    fn it_classifies_io_error_alone() {
        assert_eq!(classify_error_kind("io error on read"), "io_error");
    }

    #[test]
    fn it_classifies_permission_alone_as_io_error() {
        assert_eq!(classify_error_kind("permission denied"), "io_error");
    }

    #[test]
    fn test_ref_pattern_serializes_to_expected_strings() {
        let all_variants = [
            RefPattern::Worktree,
            RefPattern::SingleCommit,
            RefPattern::RangeDoubleDot,
            RefPattern::RangeTripleDot,
            RefPattern::Branch,
            RefPattern::Sha,
        ];
        let expected = [
            "worktree",
            "single_commit",
            "range_double_dot",
            "range_triple_dot",
            "branch",
            "sha",
        ];
        for (variant, exp) in all_variants.iter().zip(expected.iter()) {
            let json = serde_json::to_string(variant).unwrap();
            assert_eq!(json, format!("\"{}\"", exp));
            assert_eq!(variant.as_str(), *exp);
        }
    }

    #[test]
    fn test_classify_ref_mode_worktree() {
        assert_eq!(classify_ref_mode("HEAD", None), "worktree");
    }

    #[test]
    fn test_classify_ref_mode_with_head_ref() {
        // When both refs provided, classifies base_ref via normalize_ref_pattern.
        assert_eq!(classify_ref_mode("main", Some("HEAD")), "branch");
        assert_eq!(classify_ref_mode("HEAD~3", Some("HEAD")), "single_commit");
        assert_eq!(classify_ref_mode(&"a".repeat(40), Some("HEAD")), "sha");
    }
}
