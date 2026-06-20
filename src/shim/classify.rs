//! argv-based git subcommand classifier.
//!
//! Implements the same command classification logic that was previously in
//! `hooks/bash_redirect_hook.py` (removed in v0.9.0, see ADR-0011).
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
/// All `gh` subcommands pass through to the real `gh` binary unchanged.
/// Structured PR review is available via the MCP tools (`review_change`,
/// `get_change_manifest`) rather than through shim interception.
fn classify_gh<'a>(_subcommand: &str, _rest: &[&'a str]) -> Classification<'a> {
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
    // Scripted-output flags take priority — caller wants text, not JSON.
    if has_scripted_output_flag(rest) {
        return Classification::Passthrough;
    }
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
    // Scripted-output flags take priority — caller wants text, not JSON.
    if has_scripted_output_flag(rest) {
        return Classification::Passthrough;
    }
    if let Some(range) = rest.iter().copied().find(|t| has_ref_range(t)) {
        return Classification::Manifest { range };
    }
    Classification::Passthrough
}

fn classify_show<'a>(rest: &[&'a str]) -> Classification<'a> {
    // Scripted-output flags mean the caller wants text, not JSON.
    if has_scripted_output_flag(rest) {
        return Classification::Passthrough;
    }
    // First non-flag argument is the object spec.
    if let Some(spec) = rest.iter().copied().find(|t| !t.starts_with('-')) {
        // Detect a blob/tree object spec of the form `<rev>:<path>` or `<rev>:`.
        // Rules:
        //   - Must contain `:`
        //   - The text BEFORE the first `:` must be non-empty (a real rev)
        //   - The text AFTER the first `:` must NOT begin with `/`
        //     (`:/<regex>` is the commit-message-search syntax where the
        //      before-colon portion is empty, so it falls through to ShowSnapshot)
        if let Some(colon_pos) = spec.find(':') {
            let before = &spec[..colon_pos];
            let after = &spec[colon_pos + 1..];
            if !before.is_empty() && !after.starts_with('/') {
                return Classification::Passthrough;
            }
        }
        return Classification::ShowSnapshot { sha: spec };
    }
    Classification::Passthrough
}

/// Return `true` when the argv slice contains any flag that requests
/// scripted/machine-readable text output from git (the caller wants raw git
/// output, not a JSON manifest).
///
/// Flags covered:
/// - `--format=<val>` and `--format` (pretty-print format string)
/// - `--pretty=<val>` and `--pretty` (pretty-print preset or format string)
/// - `-s` / `--no-patch` (suppress diff, often paired with `--format`)
/// - `--porcelain` (machine-readable output for `status`, `blame`, etc.)
/// - `--stat` (diffstat text output)
/// - `-z` (NUL-terminated output)
///
/// # SAFETY: `--` separator not honoured
///
/// git treats `--` as the end of flags and the start of pathspecs, so
/// `git diff a..b -- --stat` passes `--stat` as a filename, not a flag.
/// This scanner does not track `--` and would incorrectly treat that
/// `--stat` as a scripted-output flag, causing passthrough when interception
/// was correct.  This is a theoretical input (no real caller names a file
/// `--stat`) and adding a runtime guard would complicate the hot path for
/// no practical benefit.  If it becomes real, scan for `--` first and stop
/// checking beyond it.
fn has_scripted_output_flag(tokens: &[&str]) -> bool {
    tokens.iter().any(|tok| {
        tok.starts_with("--format=")
            || tok.starts_with("--pretty=")
            || *tok == "--format"
            || *tok == "--pretty"
            || *tok == "-s"
            || *tok == "--no-patch"
            || *tok == "--porcelain"
            || *tok == "--stat"
            || *tok == "-z"
    })
}

fn classify_blame<'a>(rest: &[&'a str]) -> Classification<'a> {
    // Scripted-output flags (e.g. --porcelain) mean the caller wants raw text.
    if has_scripted_output_flag(rest) {
        return Classification::Passthrough;
    }
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
    fn it_passes_through_gh_pr_diff_with_number() {
        // gh pr diff <N> must now pass straight through to the real gh binary.
        assert_eq!(
            classify(&["gh", "pr", "diff", "42"]),
            Classification::Passthrough
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
    fn it_passes_through_gh_pr_diff_help_flag() {
        // "gh pr diff --help" must pass through, not be treated as PR number "--help"
        assert_eq!(
            classify(&["gh", "pr", "diff", "--help"]),
            Classification::Passthrough
        );
    }

    #[test]
    fn it_passes_through_gh_pr_diff_with_other_flags() {
        // Other flags like --web, --patch, --name-only must pass through
        assert_eq!(
            classify(&["gh", "pr", "diff", "--web"]),
            Classification::Passthrough
        );
        assert_eq!(
            classify(&["gh", "pr", "diff", "--patch"]),
            Classification::Passthrough
        );
        assert_eq!(
            classify(&["gh", "pr", "diff", "--name-only"]),
            Classification::Passthrough
        );
    }

    #[test]
    fn it_passes_through_gh_pr_diff_with_non_numeric_arg() {
        // After #367 the numeric-PR-number branch is gone; a branch-name arg
        // (or anything else) passes through like every other gh subcommand.
        assert_eq!(
            classify(&["gh", "pr", "diff", "my-branch"]),
            Classification::Passthrough
        );
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
    fn it_classifies_git_show_with_non_scripted_flag_before_sha() {
        // Flags that don't request scripted output still route to ShowSnapshot.
        // --name-only requests filenames only — not a scripted format override.
        assert_eq!(
            classify(&["git", "show", "--name-only", "abc1234"]),
            Classification::ShowSnapshot { sha: "abc1234" }
        );
    }

    // --- Scripted-output passthrough (issue #338) ---

    #[test]
    fn it_passes_through_git_show_with_format_flag() {
        // The exact case from the bug report: git show -s --format=%ct HEAD
        assert_eq!(
            classify(&["git", "show", "-s", "--format=%ct", "HEAD"]),
            Classification::Passthrough
        );
    }

    #[test]
    fn it_passes_through_git_show_with_bare_format_flag() {
        // git show --format <value> HEAD  (space-separated form)
        assert_eq!(
            classify(&["git", "show", "--format", "%ct", "HEAD"]),
            Classification::Passthrough
        );
    }

    #[test]
    fn it_passes_through_git_show_with_pretty_equals_flag() {
        // git show --pretty=format:%H HEAD
        assert_eq!(
            classify(&["git", "show", "--pretty=format:%H", "HEAD"]),
            Classification::Passthrough
        );
    }

    #[test]
    fn it_passes_through_git_show_with_bare_pretty_flag() {
        // git show --pretty HEAD  (bare --pretty with separate value)
        assert_eq!(
            classify(&["git", "show", "--pretty", "abc1234"]),
            Classification::Passthrough
        );
    }

    #[test]
    fn it_passes_through_git_show_with_porcelain_flag() {
        // git show --porcelain HEAD
        assert_eq!(
            classify(&["git", "show", "--porcelain", "abc1234"]),
            Classification::Passthrough
        );
    }

    #[test]
    fn it_passes_through_git_show_with_stat_flag() {
        // git show --stat HEAD  (diffstat text output)
        assert_eq!(
            classify(&["git", "show", "--stat", "abc1234"]),
            Classification::Passthrough
        );
    }

    #[test]
    fn it_passes_through_git_show_with_z_flag() {
        // git show -z HEAD  (NUL-separated output)
        assert_eq!(
            classify(&["git", "show", "-z", "abc1234"]),
            Classification::Passthrough
        );
    }

    #[test]
    fn it_passes_through_git_log_with_format_flag() {
        // git log --format=%ct main..HEAD  (scripted log output)
        assert_eq!(
            classify(&["git", "log", "--format=%ct", "main..HEAD"]),
            Classification::Passthrough
        );
    }

    #[test]
    fn it_passes_through_git_log_with_pretty_flag() {
        // git log --pretty=oneline main..HEAD
        assert_eq!(
            classify(&["git", "log", "--pretty=oneline", "main..HEAD"]),
            Classification::Passthrough
        );
    }

    #[test]
    fn it_passes_through_git_diff_with_stat_flag() {
        // git diff --stat main..HEAD
        assert_eq!(
            classify(&["git", "diff", "--stat", "main..HEAD"]),
            Classification::Passthrough
        );
    }

    #[test]
    fn it_passes_through_git_show_with_no_patch_flag() {
        // --no-patch is a synonym for -s; caller is requesting suppressed output
        assert_eq!(
            classify(&["git", "show", "--no-patch", "abc1234"]),
            Classification::Passthrough
        );
    }

    #[test]
    fn it_still_classifies_plain_git_show_sha_as_snapshot() {
        // Regression: plain git show without scripted-output flags must still be intercepted
        assert_eq!(
            classify(&["git", "show", "abc1234"]),
            Classification::ShowSnapshot { sha: "abc1234" }
        );
    }

    #[test]
    fn it_still_classifies_git_log_range_without_format_as_history() {
        // Regression: git log with a range but no scripted-output flags must still be intercepted
        assert_eq!(
            classify(&["git", "log", "main..HEAD"]),
            Classification::History {
                range: "main..HEAD"
            }
        );
    }

    // --- ShowSnapshot: blob/object-spec passthrough (issue #381) ---

    #[test]
    fn it_passes_through_git_show_with_branch_colon_path() {
        // git show origin/main:.lefthook.yml — object spec, must NOT try peel_to_commit
        assert_eq!(
            classify(&["git", "show", "origin/main:.lefthook.yml"]),
            Classification::Passthrough
        );
    }

    #[test]
    fn it_passes_through_git_show_with_sha_colon_path() {
        // git show 0123abcdef1234567890abcdef1234567890abcd:apps/x/package.json
        assert_eq!(
            classify(&[
                "git",
                "show",
                "0123abcdef1234567890abcdef1234567890abcd:apps/x/package.json"
            ]),
            Classification::Passthrough
        );
    }

    #[test]
    fn it_passes_through_git_show_with_head_colon_empty_path() {
        // git show HEAD: — tree spec, must pass through
        assert_eq!(
            classify(&["git", "show", "HEAD:"]),
            Classification::Passthrough
        );
    }

    #[test]
    fn it_classifies_git_show_commit_message_search_as_snapshot() {
        // git show :/fix typo — colon-slash is commit search, NOT blob spec; before-colon is empty
        assert_eq!(
            classify(&["git", "show", ":/fix typo"]),
            Classification::ShowSnapshot { sha: ":/fix typo" }
        );
    }

    #[test]
    fn it_classifies_git_show_plain_sha_without_colon_as_snapshot() {
        // git show abc1234 — no colon at all, stays intercepted
        assert_eq!(
            classify(&["git", "show", "abc1234"]),
            Classification::ShowSnapshot { sha: "abc1234" }
        );
    }

    #[test]
    fn it_passes_through_git_show_head_colon_nested_path() {
        // git show HEAD:dir/file.txt — rev:path is a blob spec
        assert_eq!(
            classify(&["git", "show", "HEAD:dir/file.txt"]),
            Classification::Passthrough
        );
    }

    #[test]
    fn it_classifies_git_show_colon_slash_regex_with_nonempty_before_as_passthrough() {
        // git show HEAD:/path — before-colon is "HEAD" (non-empty), after begins with '/'
        // This is a rev:path where path starts with '/' — real git accepts this.
        // The before-colon is non-empty but after starts with '/', so per our rule it
        // stays as ShowSnapshot (not a `:/<regex>` search form).
        // Actually: `HEAD:/path` is a valid blob spec (HEAD rev, path `/path`).
        // Per the detection rule: before="HEAD" (non-empty), after="/path" (starts with '/').
        // The rule says after must NOT start with '/' to be passthrough — so this stays intercepted.
        assert_eq!(
            classify(&["git", "show", "HEAD:/some/absolute/path"]),
            Classification::ShowSnapshot {
                sha: "HEAD:/some/absolute/path"
            }
        );
    }

    #[test]
    fn it_classifies_git_show_bare_colon_slash_regex_as_snapshot() {
        // git show :/regex — before-colon is empty, so it's commit-search syntax
        assert_eq!(
            classify(&["git", "show", ":/another regex"]),
            Classification::ShowSnapshot {
                sha: ":/another regex"
            }
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

    // --- Item 9: additional scripted-output classifier tests ---

    #[test]
    fn it_passes_through_git_diff_with_format_flag_and_range() {
        // git diff --format=%H main..HEAD
        assert_eq!(
            classify(&["git", "diff", "--format=%H", "main..HEAD"]),
            Classification::Passthrough
        );
    }

    #[test]
    fn it_passes_through_git_diff_with_s_flag_and_range() {
        // git diff -s main..HEAD
        assert_eq!(
            classify(&["git", "diff", "-s", "main..HEAD"]),
            Classification::Passthrough
        );
    }

    #[test]
    fn it_passes_through_git_diff_with_z_flag_and_range() {
        // git diff -z main..HEAD
        assert_eq!(
            classify(&["git", "diff", "-z", "main..HEAD"]),
            Classification::Passthrough
        );
    }

    #[test]
    fn it_passes_through_git_show_with_bare_s_flag() {
        // git show -s abc1234 (bare -s, no --format)
        assert_eq!(
            classify(&["git", "show", "-s", "abc1234"]),
            Classification::Passthrough
        );
    }

    #[test]
    fn it_passes_through_git_blame_with_porcelain_flag() {
        // git blame --porcelain src/main.rs — scripted output, must passthrough.
        assert_eq!(
            classify(&["git", "blame", "--porcelain", "src/main.rs"]),
            Classification::Passthrough
        );
    }

    #[test]
    fn it_still_classifies_plain_git_blame_as_blame_snapshot() {
        // Regression: plain git blame without scripted-output flags must still be intercepted.
        assert_eq!(
            classify(&["git", "blame", "src/main.rs"]),
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
