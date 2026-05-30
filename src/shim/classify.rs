//! argv-based git subcommand classifier.
//!
//! Ports `_classify_git_command` from `hooks/bash_redirect_hook.py` to Rust.
//! All logic is pure — no I/O, no env reads.

/// The result of classifying a `git …` or `gh …` argv slice.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Classification<'a> {
    /// `git diff <ref>..<ref>` → `get_change_manifest`
    Manifest { range: &'a str },
    /// `git log <ref>..<ref>` → `get_commit_history`
    History { range: &'a str },
    /// `git log -S/-G <term>` → `get_function_context`
    FunctionContext {
        range: Option<&'a str>,
        pickaxe_term: &'a str,
    },
    /// `git show <sha>` → `get_file_snapshots` (show variant)
    ShowSnapshot { sha: &'a str },
    /// `git blame <path>` → `get_file_snapshots` (blame variant)
    BlameSnapshot { path: &'a str },
    /// `gh pr diff <N>` → `get_change_manifest` via resolved PR base..head range
    GhPrDiff { pr_number: &'a str },
    /// Anything else — pass through to real git/gh.
    Passthrough,
}

/// Classify a `git …` or `gh …` argv slice.
///
/// `argv[0]` is the binary name (`"git"` or `"gh"`); `argv[1]` is the
/// subcommand.  Returns `Passthrough` when the subcommand is not on the
/// watch list or the argv is too short.
pub(crate) fn classify<'a>(argv: &'a [&'a str]) -> Classification<'a> {
    if argv.len() < 2 {
        return Classification::Passthrough;
    }
    // argv[0] may be an absolute path (e.g. /tmp/bin/gh) when invoked via a
    // symlink; extract only the filename component for dispatch, matching what
    // main.rs does for the shim-mode gate.
    let binary_basename = std::path::Path::new(argv[0])
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or(argv[0]);
    let subcommand = argv[1];
    let rest = &argv[2..];

    match binary_basename {
        "gh" => classify_gh(subcommand, rest),
        _ => classify_git(subcommand, rest),
    }
}

/// Classify `gh <subcommand> …` argv.
///
/// Only `gh pr diff <N>` is intercepted; everything else passes through to
/// the real `gh` binary.
fn classify_gh<'a>(subcommand: &str, rest: &[&'a str]) -> Classification<'a> {
    // gh pr diff <N>  →  intercept; everything else passes through.
    if subcommand == "pr"
        && rest.first() == Some(&"diff")
        && let Some(pr_number) = rest.get(1)
    {
        return Classification::GhPrDiff { pr_number };
    }
    Classification::Passthrough
}

/// Classify `git <subcommand> …` argv (existing logic, unchanged).
fn classify_git<'a>(subcommand: &str, rest: &[&'a str]) -> Classification<'a> {
    match subcommand {
        "log" => classify_log(rest),
        "diff" => classify_diff(rest),
        "show" => classify_show(rest),
        "blame" => classify_blame(rest),
        _ => Classification::Passthrough,
    }
}

fn classify_log<'a>(rest: &[&'a str]) -> Classification<'a> {
    // Pickaxe check BEFORE ref-range check (mirrors Python order).
    if let Some(term) = pickaxe_term(rest) {
        let range = rest.iter().copied().find(|t| has_ref_range(t));
        return Classification::FunctionContext {
            range,
            pickaxe_term: term,
        };
    }
    // Ref-range check.
    if let Some(range) = rest.iter().copied().find(|t| has_ref_range(t)) {
        return Classification::History { range };
    }
    Classification::Passthrough
}

fn classify_diff<'a>(rest: &[&'a str]) -> Classification<'a> {
    if let Some(range) = rest.iter().copied().find(|t| has_ref_range(t)) {
        return Classification::Manifest { range };
    }
    Classification::Passthrough
}

fn classify_show<'a>(rest: &[&'a str]) -> Classification<'a> {
    // First non-flag argument is the sha.
    if let Some(sha) = rest.iter().copied().find(|t| !t.starts_with('-')) {
        return Classification::ShowSnapshot { sha };
    }
    Classification::Passthrough
}

fn classify_blame<'a>(rest: &[&'a str]) -> Classification<'a> {
    // First non-flag argument is the path.
    if let Some(path) = rest.iter().copied().find(|t| !t.starts_with('-')) {
        return Classification::BlameSnapshot { path };
    }
    Classification::Passthrough
}

/// Return `true` if `token` contains `..` with at least one character on
/// each side — i.e. it looks like `a..b` or `a...b`.
///
/// A bare `..` or `...` token is excluded because those are the
/// parent-directory shorthand, not ref ranges.
fn has_ref_range(token: &str) -> bool {
    if token == ".." || token == "..." {
        return false;
    }
    token.contains("..")
}

/// Return the pickaxe search term if the argv slice contains a `-S` or `-G`
/// flag (with or without an attached term).
///
/// Returns `Some(term)` where `term` may be empty:
/// - `-S foo`       → `Some("foo")`   (separate non-range token)
/// - `-Sfoo`        → `Some("foo")`   (concatenated)
/// - `-S main..HEAD`→ `Some("")`      (next token is a ref range, not a term)
/// - `-S`           → `Some("")`      (bare flag, no following token)
///
/// Returns `None` when no `-S`/`-G` flag is present at all.
fn pickaxe_term<'a>(tokens: &[&'a str]) -> Option<&'a str> {
    let mut iter = tokens.iter().copied().peekable();
    while let Some(tok) = iter.next() {
        if tok == "-S" || tok == "-G" {
            // Separate-token form: the next token is the term ONLY if it does
            // not look like a ref range.  A ref range belongs to the range
            // field, not the pickaxe term.
            return match iter.peek() {
                Some(&next) if !has_ref_range(next) => Some(next),
                _ => Some(""),
            };
        } else if tok.starts_with("-S") || tok.starts_with("-G") {
            // Concatenated form: `-Sterm` or `-S` (tok.len() == 2 means empty term).
            return Some(&tok[2..]);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- Passthrough cases ---

    // --- gh classification ---

    #[test]
    fn it_classifies_gh_pr_diff_as_gh_pr_diff() {
        assert_eq!(
            classify(&["gh", "pr", "diff", "42"]),
            Classification::GhPrDiff { pr_number: "42" }
        );
    }

    #[test]
    fn it_passes_through_gh_repo_view() {
        assert_eq!(
            classify(&["gh", "repo", "view", "--json", "name"]),
            Classification::Passthrough
        );
    }

    #[test]
    fn it_passes_through_gh_issue_list() {
        assert_eq!(
            classify(&["gh", "issue", "list", "--limit", "1"]),
            Classification::Passthrough
        );
    }

    #[test]
    fn it_passes_through_gh_pr_diff_without_number() {
        // "gh pr diff" with no number is ambiguous — pass through.
        assert_eq!(classify(&["gh", "pr", "diff"]), Classification::Passthrough);
    }

    #[test]
    fn it_passes_through_gh_pr_list() {
        assert_eq!(classify(&["gh", "pr", "list"]), Classification::Passthrough);
    }

    // --- git classification (existing) ---

    #[test]
    fn it_passes_through_git_status() {
        assert_eq!(classify(&["git", "status"]), Classification::Passthrough);
    }

    #[test]
    fn it_passes_through_git_add() {
        assert_eq!(
            classify(&["git", "add", "file.rs"]),
            Classification::Passthrough
        );
    }

    #[test]
    fn it_passes_through_git_commit() {
        assert_eq!(
            classify(&["git", "commit", "-m", "msg"]),
            Classification::Passthrough
        );
    }

    #[test]
    fn it_passes_through_git_push() {
        assert_eq!(classify(&["git", "push"]), Classification::Passthrough);
    }

    #[test]
    fn it_passes_through_git_fetch() {
        assert_eq!(classify(&["git", "fetch"]), Classification::Passthrough);
    }

    #[test]
    fn it_passes_through_git_pull() {
        assert_eq!(classify(&["git", "pull"]), Classification::Passthrough);
    }

    #[test]
    fn it_passes_through_git_log_no_range() {
        assert_eq!(classify(&["git", "log"]), Classification::Passthrough);
    }

    #[test]
    fn it_passes_through_git_log_oneline() {
        assert_eq!(
            classify(&["git", "log", "--oneline"]),
            Classification::Passthrough
        );
    }

    #[test]
    fn it_passes_through_git_diff_no_range() {
        assert_eq!(classify(&["git", "diff"]), Classification::Passthrough);
    }

    #[test]
    fn it_passes_through_git_diff_single_ref() {
        // HEAD alone has no `..` — not a ref range
        assert_eq!(
            classify(&["git", "diff", "HEAD"]),
            Classification::Passthrough
        );
    }

    #[test]
    fn it_passes_through_bare_dotdot_token() {
        // `..` alone is the parent-dir shorthand, not a ref range
        assert_eq!(
            classify(&["git", "diff", ".."]),
            Classification::Passthrough
        );
    }

    #[test]
    fn it_passes_through_bare_triple_dot_token() {
        assert_eq!(
            classify(&["git", "diff", "..."]),
            Classification::Passthrough
        );
    }

    #[test]
    fn it_passes_through_git_show_no_args() {
        assert_eq!(classify(&["git", "show"]), Classification::Passthrough);
    }

    #[test]
    fn it_passes_through_git_blame_no_args() {
        assert_eq!(classify(&["git", "blame"]), Classification::Passthrough);
    }

    #[test]
    fn it_passes_through_too_short_argv() {
        assert_eq!(classify(&["git"]), Classification::Passthrough);
        assert_eq!(classify(&[]), Classification::Passthrough);
    }

    // --- Manifest (git diff <ref>..<ref>) ---

    #[test]
    fn it_classifies_git_diff_two_dot_range_as_manifest() {
        assert_eq!(
            classify(&["git", "diff", "main..HEAD"]),
            Classification::Manifest {
                range: "main..HEAD"
            }
        );
    }

    #[test]
    fn it_classifies_git_diff_three_dot_range_as_manifest() {
        assert_eq!(
            classify(&["git", "diff", "main...HEAD"]),
            Classification::Manifest {
                range: "main...HEAD"
            }
        );
    }

    #[test]
    fn it_classifies_git_diff_sha_range_as_manifest() {
        assert_eq!(
            classify(&["git", "diff", "abc123..def456"]),
            Classification::Manifest {
                range: "abc123..def456"
            }
        );
    }

    // --- History (git log <ref>..<ref>) ---

    #[test]
    fn it_classifies_git_log_two_dot_range_as_history() {
        assert_eq!(
            classify(&["git", "log", "main..HEAD"]),
            Classification::History {
                range: "main..HEAD"
            }
        );
    }

    #[test]
    fn it_classifies_git_log_three_dot_range_as_history() {
        assert_eq!(
            classify(&["git", "log", "main...HEAD"]),
            Classification::History {
                range: "main...HEAD"
            }
        );
    }

    #[test]
    fn it_classifies_git_log_with_flags_and_range_as_history() {
        assert_eq!(
            classify(&["git", "log", "--oneline", "HEAD~3..HEAD"]),
            Classification::History {
                range: "HEAD~3..HEAD"
            }
        );
    }

    // --- FunctionContext (git log -S/-G) ---

    #[test]
    fn it_classifies_git_log_pickaxe_s_separate_token() {
        assert_eq!(
            classify(&["git", "log", "-S", "myfunction"]),
            Classification::FunctionContext {
                range: None,
                pickaxe_term: "myfunction",
            }
        );
    }

    #[test]
    fn it_classifies_git_log_pickaxe_s_concatenated() {
        assert_eq!(
            classify(&["git", "log", "-Smyfunction"]),
            Classification::FunctionContext {
                range: None,
                pickaxe_term: "myfunction",
            }
        );
    }

    #[test]
    fn it_classifies_git_log_pickaxe_g_separate_token() {
        assert_eq!(
            classify(&["git", "log", "-G", "pattern"]),
            Classification::FunctionContext {
                range: None,
                pickaxe_term: "pattern",
            }
        );
    }

    #[test]
    fn it_classifies_git_log_pickaxe_g_concatenated() {
        assert_eq!(
            classify(&["git", "log", "-Gpattern"]),
            Classification::FunctionContext {
                range: None,
                pickaxe_term: "pattern",
            }
        );
    }

    #[test]
    fn it_classifies_pickaxe_before_ref_range() {
        // Pickaxe check must win over the ref-range check
        let result = classify(&["git", "log", "-S", "term", "main..HEAD"]);
        assert_eq!(
            result,
            Classification::FunctionContext {
                range: Some("main..HEAD"),
                pickaxe_term: "term",
            }
        );
    }

    // --- ShowSnapshot (git show <sha>) ---

    #[test]
    fn it_classifies_git_show_sha_as_show_snapshot() {
        assert_eq!(
            classify(&["git", "show", "abc1234"]),
            Classification::ShowSnapshot { sha: "abc1234" }
        );
    }

    #[test]
    fn it_classifies_git_show_with_flags_before_sha() {
        assert_eq!(
            classify(&["git", "show", "--stat", "abc1234"]),
            Classification::ShowSnapshot { sha: "abc1234" }
        );
    }

    // --- BlameSnapshot (git blame <path>) ---

    #[test]
    fn it_classifies_git_blame_path_as_blame_snapshot() {
        assert_eq!(
            classify(&["git", "blame", "src/main.rs"]),
            Classification::BlameSnapshot {
                path: "src/main.rs"
            }
        );
    }

    #[test]
    fn it_classifies_git_blame_with_flags_before_path() {
        assert_eq!(
            classify(&["git", "blame", "-w", "src/main.rs"]),
            Classification::BlameSnapshot {
                path: "src/main.rs"
            }
        );
    }

    // --- F1: pickaxe term must not steal a ref-range token ---

    #[test]
    fn it_classifies_pickaxe_with_range_lookalike_as_term_and_no_range() {
        // git log -S foo  → term="foo", range=None
        assert_eq!(
            classify(&["git", "log", "-S", "foo"]),
            Classification::FunctionContext {
                range: None,
                pickaxe_term: "foo",
            }
        );
    }

    #[test]
    fn it_classifies_concatenated_pickaxe_as_term_and_no_range() {
        // git log -Sfoo  → term="foo", range=None
        assert_eq!(
            classify(&["git", "log", "-Sfoo"]),
            Classification::FunctionContext {
                range: None,
                pickaxe_term: "foo",
            }
        );
    }

    #[test]
    fn it_classifies_pickaxe_term_with_separate_range() {
        // git log -S foo main..HEAD  → term="foo", range="main..HEAD"
        assert_eq!(
            classify(&["git", "log", "-S", "foo", "main..HEAD"]),
            Classification::FunctionContext {
                range: Some("main..HEAD"),
                pickaxe_term: "foo",
            }
        );
    }

    #[test]
    fn it_classifies_pickaxe_when_next_token_is_ref_range_as_empty_term() {
        // git log -S main..HEAD  → term="", range="main..HEAD"  (the bug case)
        assert_eq!(
            classify(&["git", "log", "-S", "main..HEAD"]),
            Classification::FunctionContext {
                range: Some("main..HEAD"),
                pickaxe_term: "",
            }
        );
    }

    #[test]
    fn it_classifies_bare_pickaxe_flag_as_empty_term_no_range() {
        // git log -S  → term="", range=None
        assert_eq!(
            classify(&["git", "log", "-S"]),
            Classification::FunctionContext {
                range: None,
                pickaxe_term: "",
            }
        );
    }

    #[test]
    fn it_classifies_concatenated_pickaxe_with_separate_range() {
        // git log -Sfoo main..HEAD  → term="foo", range="main..HEAD"
        assert_eq!(
            classify(&["git", "log", "-Sfoo", "main..HEAD"]),
            Classification::FunctionContext {
                range: Some("main..HEAD"),
                pickaxe_term: "foo",
            }
        );
    }
}
