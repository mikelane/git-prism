//! argv-based git subcommand classifier.
//!
//! Ports `_classify_git_command` from `hooks/bash_redirect_hook.py` to Rust.
//! All logic is pure — no I/O, no env reads.

/// The result of classifying a `git …` argv slice.
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
    /// Anything else — pass through to real git.
    Passthrough,
}

/// Classify a `git …` argv slice (argv[0] is the binary name, argv[1] is the
/// git subcommand).  Returns `Passthrough` when the subcommand is not on the
/// watch list or does not carry a ref range.
pub(crate) fn classify<'a>(argv: &'a [&'a str]) -> Classification<'a> {
    // Need at least ["git", "<subcommand>"]
    if argv.len() < 2 {
        return Classification::Passthrough;
    }
    let subcommand = argv[1];
    let rest = &argv[2..];

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

/// Return the pickaxe search term if the argv slice contains `-S<term>`,
/// `-S <term>`, `-G<term>`, or `-G <term>`.
fn pickaxe_term<'a>(tokens: &[&'a str]) -> Option<&'a str> {
    let mut iter = tokens.iter().copied().peekable();
    while let Some(tok) = iter.next() {
        if tok == "-S" || tok == "-G" {
            // Separate-token form: `-S term`
            if let Some(&next) = iter.peek() {
                return Some(next);
            }
        } else if tok.starts_with("-S") || tok.starts_with("-G") {
            // Concatenated form: `-Sterm`
            if tok.len() > 2 {
                return Some(&tok[2..]);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- Passthrough cases ---

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
}
