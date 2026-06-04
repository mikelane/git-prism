//! argv-based git subcommand classifier.
//!
//! Implements the same command classification logic that was previously in
//! `hooks/bash_redirect_hook.py` (removed in v0.9.0, see ADR-0011).
//! All logic is pure — no I/O, no env reads.

/// The full result of classifying an argv slice.
///
/// `classification` is the routing decision; `repo_override` carries the path
/// from a leading `-C <path>` git global option so the caller can build the
/// manifest against the correct repository instead of cwd.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct ClassifyResult<'a> {
    pub(crate) classification: Classification<'a>,
    /// Path from `-C <path>` global option, if present (last one wins).
    pub(crate) repo_override: Option<&'a str>,
}

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
/// `argv[0]` is the binary name (`"git"` or `"gh"`); the subcommand follows
/// after any leading git global options.  Returns a `ClassifyResult` whose
/// `classification` is `Passthrough` when the subcommand is not on the watch
/// list or the argv is too short.
///
/// For `git` invocations, leading global options (e.g. `-C <path>`,
/// `-c <name=value>`, `--git-dir <path>`) are skipped to locate the real
/// subcommand.  The last `-C <path>` value is surfaced in `repo_override`.
pub(crate) fn classify<'a>(argv: &'a [&'a str]) -> ClassifyResult<'a> {
    if argv.len() < 2 {
        return ClassifyResult {
            classification: Classification::Passthrough,
            repo_override: None,
        };
    }
    // argv[0] may be an absolute path (e.g. /tmp/bin/gh) when invoked via a
    // symlink; extract only the filename component for dispatch, matching what
    // main.rs does for the shim-mode gate.
    let binary_basename = std::path::Path::new(argv[0])
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or(argv[0]);

    match binary_basename {
        "gh" => {
            let subcommand = argv[1];
            let rest = &argv[2..];
            ClassifyResult {
                classification: classify_gh(subcommand, rest),
                repo_override: None,
            }
        }
        _ => {
            let (subcommand, rest, repo_override) = skip_git_global_options(&argv[1..]);
            let subcommand = match subcommand {
                Some(s) => s,
                None => {
                    return ClassifyResult {
                        classification: Classification::Passthrough,
                        repo_override: None,
                    };
                }
            };
            ClassifyResult {
                classification: classify_git(subcommand, rest),
                repo_override,
            }
        }
    }
}

/// Skip leading git global options and return `(subcommand, rest, repo_override)`.
///
/// `tokens` is `argv[1..]` (everything after the binary name).  Returns the
/// first non-option token as the subcommand, the remaining tokens as `rest`,
/// and the last `-C <path>` value seen as `repo_override`.
///
/// If an unrecognised leading `-`-prefixed token is encountered the safe
/// default is to return `(None, &[], None)` so the caller produces Passthrough.
fn skip_git_global_options<'a>(
    tokens: &'a [&'a str],
) -> (Option<&'a str>, &'a [&'a str], Option<&'a str>) {
    let mut i = 0;
    let mut repo_override: Option<&'a str> = None;

    while i < tokens.len() {
        let tok = tokens[i];

        // Short value-taking options (two-token form only).
        if tok == "-C" {
            repo_override = tokens.get(i + 1).copied();
            i += 2;
            continue;
        }
        if tok == "-c" {
            i += 2;
            continue;
        }

        // Long value-taking options in `--opt value` form (two tokens).
        if matches!(
            tok,
            "--git-dir" | "--work-tree" | "--namespace" | "--super-prefix" | "--config-env"
        ) {
            i += 2;
            continue;
        }

        // Long value-taking options in `--opt=value` form (one token).
        if tok.starts_with("--git-dir=")
            || tok.starts_with("--work-tree=")
            || tok.starts_with("--namespace=")
            || tok.starts_with("--super-prefix=")
            || tok.starts_with("--config-env=")
        {
            i += 1;
            continue;
        }

        // `--exec-path=<value>` (one token) or bare `--exec-path` (valueless).
        if tok.starts_with("--exec-path=") || tok == "--exec-path" {
            i += 1;
            continue;
        }

        // Valueless flags.
        if matches!(
            tok,
            "-p" | "--paginate"
                | "-P"
                | "--no-pager"
                | "--bare"
                | "--no-replace-objects"
                | "--literal-pathspecs"
                | "--glob-pathspecs"
                | "--noglob-pathspecs"
                | "--icase-pathspecs"
                | "--no-optional-locks"
        ) {
            i += 1;
            continue;
        }

        // Unrecognised leading option → safe default: Passthrough.
        if tok.starts_with('-') {
            return (None, &[], None);
        }

        // First non-option token is the subcommand.
        return (Some(tok), &tokens[i + 1..], repo_override);
    }

    (None, &[], None)
}

/// Classify `gh <subcommand> …` argv.
///
/// Only `gh pr diff <N>` is intercepted; everything else passes through to
/// the real `gh` binary.
fn classify_gh<'a>(subcommand: &str, rest: &[&'a str]) -> Classification<'a> {
    // gh pr diff <N>  →  intercept only if N is a numeric PR number.
    // Flags (starting with -) and non-numeric tokens pass through.
    if subcommand == "pr"
        && rest.first() == Some(&"diff")
        && let Some(pr_number) = rest.get(1)
        && !pr_number.starts_with('-')
        && pr_number.chars().all(|c| c.is_ascii_digit())
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
    // First non-flag argument is the sha.
    if let Some(sha) = rest.iter().copied().find(|t| !t.starts_with('-')) {
        return Classification::ShowSnapshot { sha };
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

    /// Convenience: extract just the `Classification` from a `ClassifyResult`.
    /// Used by existing tests that only care about routing, not repo_override.
    fn classification<'a>(argv: &'a [&'a str]) -> Classification<'a> {
        classify(argv).classification
    }

    // --- Passthrough cases ---

    // --- gh classification ---

    #[test]
    fn it_classifies_gh_pr_diff_as_gh_pr_diff() {
        assert_eq!(
            classification(&["gh", "pr", "diff", "42"]),
            Classification::GhPrDiff { pr_number: "42" }
        );
    }

    #[test]
    fn it_passes_through_gh_repo_view() {
        assert_eq!(
            classification(&["gh", "repo", "view", "--json", "name"]),
            Classification::Passthrough
        );
    }

    #[test]
    fn it_passes_through_gh_issue_list() {
        assert_eq!(
            classification(&["gh", "issue", "list", "--limit", "1"]),
            Classification::Passthrough
        );
    }

    #[test]
    fn it_passes_through_gh_pr_diff_without_number() {
        // "gh pr diff" with no number is ambiguous — pass through.
        assert_eq!(
            classification(&["gh", "pr", "diff"]),
            Classification::Passthrough
        );
    }

    #[test]
    fn it_passes_through_gh_pr_diff_help_flag() {
        // "gh pr diff --help" must pass through, not be treated as PR number "--help"
        assert_eq!(
            classification(&["gh", "pr", "diff", "--help"]),
            Classification::Passthrough
        );
    }

    #[test]
    fn it_passes_through_gh_pr_diff_with_other_flags() {
        // Other flags like --web, --patch, --name-only must pass through
        assert_eq!(
            classification(&["gh", "pr", "diff", "--web"]),
            Classification::Passthrough
        );
        assert_eq!(
            classification(&["gh", "pr", "diff", "--patch"]),
            Classification::Passthrough
        );
        assert_eq!(
            classification(&["gh", "pr", "diff", "--name-only"]),
            Classification::Passthrough
        );
    }

    #[test]
    fn it_passes_through_gh_pr_list() {
        assert_eq!(
            classification(&["gh", "pr", "list"]),
            Classification::Passthrough
        );
    }

    // --- git classification (existing) ---

    #[test]
    fn it_passes_through_git_status() {
        assert_eq!(
            classification(&["git", "status"]),
            Classification::Passthrough
        );
    }

    #[test]
    fn it_passes_through_git_add() {
        assert_eq!(
            classification(&["git", "add", "file.rs"]),
            Classification::Passthrough
        );
    }

    #[test]
    fn it_passes_through_git_commit() {
        assert_eq!(
            classification(&["git", "commit", "-m", "msg"]),
            Classification::Passthrough
        );
    }

    #[test]
    fn it_passes_through_git_push() {
        assert_eq!(
            classification(&["git", "push"]),
            Classification::Passthrough
        );
    }

    #[test]
    fn it_passes_through_git_fetch() {
        assert_eq!(
            classification(&["git", "fetch"]),
            Classification::Passthrough
        );
    }

    #[test]
    fn it_passes_through_git_pull() {
        assert_eq!(
            classification(&["git", "pull"]),
            Classification::Passthrough
        );
    }

    #[test]
    fn it_passes_through_git_log_no_range() {
        assert_eq!(classification(&["git", "log"]), Classification::Passthrough);
    }

    #[test]
    fn it_passes_through_git_log_oneline() {
        assert_eq!(
            classification(&["git", "log", "--oneline"]),
            Classification::Passthrough
        );
    }

    #[test]
    fn it_passes_through_git_diff_no_range() {
        assert_eq!(
            classification(&["git", "diff"]),
            Classification::Passthrough
        );
    }

    #[test]
    fn it_passes_through_git_diff_single_ref() {
        // HEAD alone has no `..` — not a ref range
        assert_eq!(
            classification(&["git", "diff", "HEAD"]),
            Classification::Passthrough
        );
    }

    #[test]
    fn it_passes_through_bare_dotdot_token() {
        // `..` alone is the parent-dir shorthand, not a ref range
        assert_eq!(
            classification(&["git", "diff", ".."]),
            Classification::Passthrough
        );
    }

    // --- git global options (issue #356) ---

    #[test]
    fn it_classifies_git_dash_c_diff_as_manifest_with_repo_override() {
        // git -C /path diff HEAD~1..HEAD  →  Manifest, repo_override = "/path"
        let result = classify(&["git", "-C", "/path", "diff", "HEAD~1..HEAD"]);
        assert_eq!(
            result,
            ClassifyResult {
                classification: Classification::Manifest {
                    range: "HEAD~1..HEAD"
                },
                repo_override: Some("/path"),
            }
        );
    }

    #[test]
    fn it_classifies_git_lowercase_c_log_as_history() {
        // git -c user.x=y log A..B  →  History, no repo_override
        let result = classify(&["git", "-c", "user.x=y", "log", "A..B"]);
        assert_eq!(
            result,
            ClassifyResult {
                classification: Classification::History { range: "A..B" },
                repo_override: None,
            }
        );
    }

    #[test]
    fn it_classifies_git_git_dir_show_as_show_snapshot() {
        // git --git-dir=/p/.git show abc1234  →  ShowSnapshot, no repo_override
        let result = classify(&["git", "--git-dir=/p/.git", "show", "abc1234"]);
        assert_eq!(
            result,
            ClassifyResult {
                classification: Classification::ShowSnapshot { sha: "abc1234" },
                repo_override: None,
            }
        );
    }

    #[test]
    fn it_classifies_combined_globals_dash_c_and_lowercase_c() {
        // git -C /repo -c k=v diff A..B  →  Manifest, repo_override = "/repo"
        let result = classify(&["git", "-C", "/repo", "-c", "k=v", "diff", "A..B"]);
        assert_eq!(
            result,
            ClassifyResult {
                classification: Classification::Manifest { range: "A..B" },
                repo_override: Some("/repo"),
            }
        );
    }

    #[test]
    fn it_classifies_valueless_global_no_pager_diff() {
        // git --no-pager diff A..B  →  Manifest, no repo_override
        let result = classify(&["git", "--no-pager", "diff", "A..B"]);
        assert_eq!(
            result,
            ClassifyResult {
                classification: Classification::Manifest { range: "A..B" },
                repo_override: None,
            }
        );
    }

    #[test]
    fn it_passes_through_unrecognized_leading_flag() {
        // git --unknown-flag diff A..B  →  Passthrough (safe default)
        let result = classify(&["git", "--unknown-flag", "diff", "A..B"]);
        assert_eq!(
            result,
            ClassifyResult {
                classification: Classification::Passthrough,
                repo_override: None,
            }
        );
    }

    #[test]
    fn it_uses_last_dash_c_path_when_multiple_present() {
        // git -C /first -C /last diff A..B  →  Manifest, repo_override = "/last"
        let result = classify(&["git", "-C", "/first", "-C", "/last", "diff", "A..B"]);
        assert_eq!(
            result,
            ClassifyResult {
                classification: Classification::Manifest { range: "A..B" },
                repo_override: Some("/last"),
            }
        );
    }

    #[test]
    fn it_passes_through_bare_triple_dot_token() {
        assert_eq!(
            classification(&["git", "diff", "..."]),
            Classification::Passthrough
        );
    }

    #[test]
    fn it_passes_through_git_show_no_args() {
        assert_eq!(
            classification(&["git", "show"]),
            Classification::Passthrough
        );
    }

    #[test]
    fn it_passes_through_git_blame_no_args() {
        assert_eq!(
            classification(&["git", "blame"]),
            Classification::Passthrough
        );
    }

    #[test]
    fn it_passes_through_too_short_argv() {
        assert_eq!(classification(&["git"]), Classification::Passthrough);
        assert_eq!(classification(&[]), Classification::Passthrough);
    }

    // --- Manifest (git diff <ref>..<ref>) ---

    #[test]
    fn it_classifies_git_diff_two_dot_range_as_manifest() {
        assert_eq!(
            classification(&["git", "diff", "main..HEAD"]),
            Classification::Manifest {
                range: "main..HEAD"
            }
        );
    }

    #[test]
    fn it_classifies_git_diff_three_dot_range_as_manifest() {
        assert_eq!(
            classification(&["git", "diff", "main...HEAD"]),
            Classification::Manifest {
                range: "main...HEAD"
            }
        );
    }

    #[test]
    fn it_classifies_git_diff_sha_range_as_manifest() {
        assert_eq!(
            classification(&["git", "diff", "abc123..def456"]),
            Classification::Manifest {
                range: "abc123..def456"
            }
        );
    }

    // --- History (git log <ref>..<ref>) ---

    #[test]
    fn it_classifies_git_log_two_dot_range_as_history() {
        assert_eq!(
            classification(&["git", "log", "main..HEAD"]),
            Classification::History {
                range: "main..HEAD"
            }
        );
    }

    #[test]
    fn it_classifies_git_log_three_dot_range_as_history() {
        assert_eq!(
            classification(&["git", "log", "main...HEAD"]),
            Classification::History {
                range: "main...HEAD"
            }
        );
    }

    #[test]
    fn it_classifies_git_log_with_flags_and_range_as_history() {
        assert_eq!(
            classification(&["git", "log", "--oneline", "HEAD~3..HEAD"]),
            Classification::History {
                range: "HEAD~3..HEAD"
            }
        );
    }

    // --- FunctionContext (git log -S/-G) ---

    #[test]
    fn it_classifies_git_log_pickaxe_s_separate_token() {
        assert_eq!(
            classification(&["git", "log", "-S", "myfunction"]),
            Classification::FunctionContext {
                range: None,
                pickaxe_term: "myfunction",
            }
        );
    }

    #[test]
    fn it_classifies_git_log_pickaxe_s_concatenated() {
        assert_eq!(
            classification(&["git", "log", "-Smyfunction"]),
            Classification::FunctionContext {
                range: None,
                pickaxe_term: "myfunction",
            }
        );
    }

    #[test]
    fn it_classifies_git_log_pickaxe_g_separate_token() {
        assert_eq!(
            classification(&["git", "log", "-G", "pattern"]),
            Classification::FunctionContext {
                range: None,
                pickaxe_term: "pattern",
            }
        );
    }

    #[test]
    fn it_classifies_git_log_pickaxe_g_concatenated() {
        assert_eq!(
            classification(&["git", "log", "-Gpattern"]),
            Classification::FunctionContext {
                range: None,
                pickaxe_term: "pattern",
            }
        );
    }

    #[test]
    fn it_classifies_pickaxe_before_ref_range() {
        // Pickaxe check must win over the ref-range check
        assert_eq!(
            classification(&["git", "log", "-S", "term", "main..HEAD"]),
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
            classification(&["git", "show", "abc1234"]),
            Classification::ShowSnapshot { sha: "abc1234" }
        );
    }

    #[test]
    fn it_classifies_git_show_with_non_scripted_flag_before_sha() {
        // Flags that don't request scripted output still route to ShowSnapshot.
        // --name-only requests filenames only — not a scripted format override.
        assert_eq!(
            classification(&["git", "show", "--name-only", "abc1234"]),
            Classification::ShowSnapshot { sha: "abc1234" }
        );
    }

    // --- Scripted-output passthrough (issue #338) ---

    #[test]
    fn it_passes_through_git_show_with_format_flag() {
        // The exact case from the bug report: git show -s --format=%ct HEAD
        assert_eq!(
            classification(&["git", "show", "-s", "--format=%ct", "HEAD"]),
            Classification::Passthrough
        );
    }

    #[test]
    fn it_passes_through_git_show_with_bare_format_flag() {
        // git show --format <value> HEAD  (space-separated form)
        assert_eq!(
            classification(&["git", "show", "--format", "%ct", "HEAD"]),
            Classification::Passthrough
        );
    }

    #[test]
    fn it_passes_through_git_show_with_pretty_equals_flag() {
        // git show --pretty=format:%H HEAD
        assert_eq!(
            classification(&["git", "show", "--pretty=format:%H", "HEAD"]),
            Classification::Passthrough
        );
    }

    #[test]
    fn it_passes_through_git_show_with_bare_pretty_flag() {
        // git show --pretty HEAD  (bare --pretty with separate value)
        assert_eq!(
            classification(&["git", "show", "--pretty", "abc1234"]),
            Classification::Passthrough
        );
    }

    #[test]
    fn it_passes_through_git_show_with_porcelain_flag() {
        // git show --porcelain HEAD
        assert_eq!(
            classification(&["git", "show", "--porcelain", "abc1234"]),
            Classification::Passthrough
        );
    }

    #[test]
    fn it_passes_through_git_show_with_stat_flag() {
        // git show --stat HEAD  (diffstat text output)
        assert_eq!(
            classification(&["git", "show", "--stat", "abc1234"]),
            Classification::Passthrough
        );
    }

    #[test]
    fn it_passes_through_git_show_with_z_flag() {
        // git show -z HEAD  (NUL-separated output)
        assert_eq!(
            classification(&["git", "show", "-z", "abc1234"]),
            Classification::Passthrough
        );
    }

    #[test]
    fn it_passes_through_git_log_with_format_flag() {
        // git log --format=%ct main..HEAD  (scripted log output)
        assert_eq!(
            classification(&["git", "log", "--format=%ct", "main..HEAD"]),
            Classification::Passthrough
        );
    }

    #[test]
    fn it_passes_through_git_log_with_pretty_flag() {
        // git log --pretty=oneline main..HEAD
        assert_eq!(
            classification(&["git", "log", "--pretty=oneline", "main..HEAD"]),
            Classification::Passthrough
        );
    }

    #[test]
    fn it_passes_through_git_diff_with_stat_flag() {
        // git diff --stat main..HEAD
        assert_eq!(
            classification(&["git", "diff", "--stat", "main..HEAD"]),
            Classification::Passthrough
        );
    }

    #[test]
    fn it_passes_through_git_show_with_no_patch_flag() {
        // --no-patch is a synonym for -s; caller is requesting suppressed output
        assert_eq!(
            classification(&["git", "show", "--no-patch", "abc1234"]),
            Classification::Passthrough
        );
    }

    #[test]
    fn it_still_classifies_plain_git_show_sha_as_snapshot() {
        // Regression: plain git show without scripted-output flags must still be intercepted
        assert_eq!(
            classification(&["git", "show", "abc1234"]),
            Classification::ShowSnapshot { sha: "abc1234" }
        );
    }

    #[test]
    fn it_still_classifies_git_log_range_without_format_as_history() {
        // Regression: git log with a range but no scripted-output flags must still be intercepted
        assert_eq!(
            classification(&["git", "log", "main..HEAD"]),
            Classification::History {
                range: "main..HEAD"
            }
        );
    }

    // --- BlameSnapshot (git blame <path>) ---

    #[test]
    fn it_classifies_git_blame_path_as_blame_snapshot() {
        assert_eq!(
            classification(&["git", "blame", "src/main.rs"]),
            Classification::BlameSnapshot {
                path: "src/main.rs"
            }
        );
    }

    #[test]
    fn it_classifies_git_blame_with_flags_before_path() {
        assert_eq!(
            classification(&["git", "blame", "-w", "src/main.rs"]),
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
            classification(&["git", "diff", "--format=%H", "main..HEAD"]),
            Classification::Passthrough
        );
    }

    #[test]
    fn it_passes_through_git_diff_with_s_flag_and_range() {
        // git diff -s main..HEAD
        assert_eq!(
            classification(&["git", "diff", "-s", "main..HEAD"]),
            Classification::Passthrough
        );
    }

    #[test]
    fn it_passes_through_git_diff_with_z_flag_and_range() {
        // git diff -z main..HEAD
        assert_eq!(
            classification(&["git", "diff", "-z", "main..HEAD"]),
            Classification::Passthrough
        );
    }

    #[test]
    fn it_passes_through_git_show_with_bare_s_flag() {
        // git show -s abc1234 (bare -s, no --format)
        assert_eq!(
            classification(&["git", "show", "-s", "abc1234"]),
            Classification::Passthrough
        );
    }

    #[test]
    fn it_passes_through_git_blame_with_porcelain_flag() {
        // git blame --porcelain src/main.rs — scripted output, must passthrough.
        assert_eq!(
            classification(&["git", "blame", "--porcelain", "src/main.rs"]),
            Classification::Passthrough
        );
    }

    #[test]
    fn it_still_classifies_plain_git_blame_as_blame_snapshot() {
        // Regression: plain git blame without scripted-output flags must still be intercepted.
        assert_eq!(
            classification(&["git", "blame", "src/main.rs"]),
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
            classification(&["git", "log", "-S", "foo"]),
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
            classification(&["git", "log", "-Sfoo"]),
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
            classification(&["git", "log", "-S", "foo", "main..HEAD"]),
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
            classification(&["git", "log", "-S", "main..HEAD"]),
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
            classification(&["git", "log", "-S"]),
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
            classification(&["git", "log", "-Sfoo", "main..HEAD"]),
            Classification::FunctionContext {
                range: Some("main..HEAD"),
                pickaxe_term: "foo",
            }
        );
    }
}
