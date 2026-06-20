//! argv-based git subcommand classifier.
//!
//! Implements the same command classification logic that was previously in
//! `hooks/bash_redirect_hook.py` (removed in v0.9.0, see ADR-0011).
//! All logic is pure — no I/O, no env reads.

/// The full result of classifying an argv slice.
///
/// `classification` is the routing decision; `repo_dir_overrides` carries every
/// `-C <path>` value in argv order so the caller can apply git's `cd`-semantics
/// fold: `git -C a -C b` means `cd a; cd b`, not "use `b` against cwd".
///
/// An empty `Vec` means no `-C` was present.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct ClassifyResult<'a> {
    pub(crate) classification: Classification<'a>,
    /// All `-C <path>` values in argv order (empty when none).
    pub(crate) repo_dir_overrides: Vec<&'a str>,
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
/// subcommand.  All `-C <path>` values are collected in order into
/// `repo_dir_overrides`.
pub(crate) fn classify<'a>(argv: &'a [&'a str]) -> ClassifyResult<'a> {
    // cargo-mutants: skip -- equivalent mutant: `argv.len() < 2` vs `<= 2` is
    // indistinguishable. No git/gh argv of length 2 ever classifies to anything
    // but Passthrough — every real subcommand needs a range / sha / path / PR
    // argument (length >= 3) — so both guards produce identical results.
    if argv.len() < 2 {
        return ClassifyResult {
            classification: Classification::Passthrough,
            repo_dir_overrides: vec![],
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
                repo_dir_overrides: vec![],
            }
        }
        _ => {
            let (subcommand, rest, repo_dir_overrides) = skip_git_global_options(&argv[1..]);
            let subcommand = match subcommand {
                Some(s) => s,
                None => {
                    return ClassifyResult {
                        classification: Classification::Passthrough,
                        repo_dir_overrides: vec![],
                    };
                }
            };
            ClassifyResult {
                classification: classify_git(subcommand, rest),
                repo_dir_overrides,
            }
        }
    }
}

/// Skip leading git global options and return
/// `(subcommand, rest, repo_dir_overrides)`.
///
/// `tokens` is `argv[1..]` (everything after the binary name).  Returns the
/// first non-option token as the subcommand, the remaining tokens as `rest`,
/// and all `-C <path>` values in argv order as `repo_dir_overrides`.
///
/// Callers fold `repo_dir_overrides` with `cd`-semantics to resolve the repo
/// path: `git -C a -C b` → `cd a; cd b` → repo at `a/b` when `b` is relative.
///
/// If an unrecognised leading `-`-prefixed token is encountered the safe
/// default is to return `(None, &[], vec![])` so the caller produces Passthrough.
fn skip_git_global_options<'a>(
    tokens: &'a [&'a str],
) -> (Option<&'a str>, &'a [&'a str], Vec<&'a str>) {
    let mut i = 0;
    let mut repo_dir_overrides: Vec<&'a str> = vec![];

    while i < tokens.len() {
        let tok = tokens[i];

        // Short value-taking options (two-token form only).
        if tok == "-C" {
            if let Some(val) = tokens.get(i + 1).copied() {
                repo_dir_overrides.push(val);
            }
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
            return (None, &[], vec![]);
        }

        // First non-option token is the subcommand.
        return (Some(tok), &tokens[i + 1..], repo_dir_overrides);
    }

    (None, &[], vec![])
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
        // Any spec containing `:` is a blob/tree/index object spec that real git
        // resolves without peeling to a commit — pass it through unchanged.
        //
        // The ONLY colon-bearing form that is NOT a blob spec is `:/text`, the
        // commit-message-search syntax.  Every other colon form is an object spec:
        //   `<rev>:<path>`       — rev-qualified blob
        //   `<rev>:`             — rev-qualified tree root
        //   `:<path>`            — stage-0 index blob
        //   `:N:<path>`          — explicit-stage index blob (N = 0, 1, 2, 3)
        //   `<rev>:/abs/path`    — rev-qualified absolute-path blob
        //
        // Refs cannot contain `:`, so this single rule is sufficient.
        if spec.contains(':') && !spec.starts_with(":/") {
            return Classification::Passthrough;
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

    /// Convenience: extract just the `Classification` from a `ClassifyResult`.
    /// Used by existing tests that only care about routing, not dash_c.
    fn classification<'a>(argv: &'a [&'a str]) -> Classification<'a> {
        classify(argv).classification
    }

    // --- Passthrough cases ---

    // --- gh classification ---

    #[test]
    fn it_passes_through_gh_pr_diff_with_number() {
        // gh pr diff <N> must now pass straight through to the real gh binary.
        assert_eq!(
            classification(&["gh", "pr", "diff", "42"]),
            Classification::Passthrough
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
    fn it_passes_through_gh_pr_diff_with_non_numeric_arg() {
        // After #367 the numeric-PR-number branch is gone; a branch-name arg
        // (or anything else) passes through like every other gh subcommand.
        assert_eq!(
            classification(&["gh", "pr", "diff", "my-branch"]),
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
        // git -C /path diff HEAD~1..HEAD  →  Manifest, dash_c = ["/path"]
        let result = classify(&["git", "-C", "/path", "diff", "HEAD~1..HEAD"]);
        assert_eq!(
            result,
            ClassifyResult {
                classification: Classification::Manifest {
                    range: "HEAD~1..HEAD"
                },
                repo_dir_overrides: vec!["/path"],
            }
        );
    }

    #[test]
    fn it_classifies_git_lowercase_c_log_as_history() {
        // git -c user.x=y log A..B  →  History, no dash_c
        let result = classify(&["git", "-c", "user.x=y", "log", "A..B"]);
        assert_eq!(
            result,
            ClassifyResult {
                classification: Classification::History { range: "A..B" },
                repo_dir_overrides: vec![],
            }
        );
    }

    #[test]
    fn it_classifies_git_git_dir_show_as_show_snapshot() {
        // git --git-dir=/p/.git show abc1234  →  ShowSnapshot, no dash_c
        let result = classify(&["git", "--git-dir=/p/.git", "show", "abc1234"]);
        assert_eq!(
            result,
            ClassifyResult {
                classification: Classification::ShowSnapshot { sha: "abc1234" },
                repo_dir_overrides: vec![],
            }
        );
    }

    #[test]
    fn it_classifies_combined_globals_dash_c_and_lowercase_c() {
        // git -C /repo -c k=v diff A..B  →  Manifest, dash_c = ["/repo"]
        let result = classify(&["git", "-C", "/repo", "-c", "k=v", "diff", "A..B"]);
        assert_eq!(
            result,
            ClassifyResult {
                classification: Classification::Manifest { range: "A..B" },
                repo_dir_overrides: vec!["/repo"],
            }
        );
    }

    #[test]
    fn it_classifies_valueless_global_no_pager_diff() {
        // git --no-pager diff A..B  →  Manifest, no dash_c
        let result = classify(&["git", "--no-pager", "diff", "A..B"]);
        assert_eq!(
            result,
            ClassifyResult {
                classification: Classification::Manifest { range: "A..B" },
                repo_dir_overrides: vec![],
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
                repo_dir_overrides: vec![],
            }
        );
    }

    #[test]
    fn it_collects_all_dash_c_paths_in_order_when_multiple_present() {
        // git -C /first -C /last diff A..B  →  Manifest, dash_c = ["/first", "/last"]
        // resolve_repo_path folds these: /first then /last (absolute) → /last
        let result = classify(&["git", "-C", "/first", "-C", "/last", "diff", "A..B"]);
        assert_eq!(
            result,
            ClassifyResult {
                classification: Classification::Manifest { range: "A..B" },
                repo_dir_overrides: vec!["/first", "/last"],
            }
        );
    }

    #[test]
    fn it_preserves_relative_stacked_dash_c_order_for_cd_semantics() {
        // git -C a -C b diff A..B  →  repo_dir_overrides = ["a", "b"] in argv
        // order. The fold in resolve_repo_path turns this into cwd/a/b
        // (cd a; cd b); the classifier's only job is to preserve order
        // faithfully, NOT to collapse to the last value. A bug that kept only
        // the last token would yield ["b"] and silently target the wrong repo
        // (issue #356).
        let result = classify(&["git", "-C", "a", "-C", "b", "diff", "A..B"]);
        assert_eq!(
            result,
            ClassifyResult {
                classification: Classification::Manifest { range: "A..B" },
                repo_dir_overrides: vec!["a", "b"],
            }
        );
    }

    #[test]
    fn it_skips_dash_c_value_that_looks_like_a_subcommand() {
        // git -C log diff A..B  →  "log" is the -C PATH, not the subcommand.
        // The real subcommand is "diff". A naive scanner that treated the token
        // after -C as a subcommand would misclassify this as History.
        let result = classify(&["git", "-C", "log", "diff", "A..B"]);
        assert_eq!(
            result,
            ClassifyResult {
                classification: Classification::Manifest { range: "A..B" },
                repo_dir_overrides: vec!["log"],
            }
        );
    }

    #[test]
    fn it_skips_dash_lowercase_c_value_that_looks_like_a_subcommand() {
        // git -c diff log A..B  →  "diff" is the -c CONFIG value, not the
        // subcommand. The real subcommand is "log". -c carries no path, so
        // repo_dir_overrides stays empty. A scanner that mistook "diff" for the
        // subcommand would classify Manifest instead of History.
        let result = classify(&["git", "-c", "diff", "log", "A..B"]);
        assert_eq!(
            result,
            ClassifyResult {
                classification: Classification::History { range: "A..B" },
                repo_dir_overrides: vec![],
            }
        );
    }

    #[test]
    fn it_skips_long_global_in_space_separated_form() {
        // git --git-dir /p/.git diff A..B  →  the value "/p/.git" is the
        // --git-dir argument (two-token form), NOT the subcommand. The `=`
        // form is covered elsewhere; this guards the space-separated branch.
        let result = classify(&["git", "--git-dir", "/p/.git", "diff", "A..B"]);
        assert_eq!(
            result,
            ClassifyResult {
                classification: Classification::Manifest { range: "A..B" },
                repo_dir_overrides: vec![],
            }
        );
    }

    #[test]
    fn it_skips_work_tree_global_in_space_separated_form() {
        // git --work-tree /wt log A..B  →  History; "/wt" is consumed as the
        // --work-tree value, "log" is the subcommand.
        let result = classify(&["git", "--work-tree", "/wt", "log", "A..B"]);
        assert_eq!(
            result,
            ClassifyResult {
                classification: Classification::History { range: "A..B" },
                repo_dir_overrides: vec![],
            }
        );
    }

    #[test]
    fn it_skips_work_tree_global_in_equals_form() {
        // git --work-tree=/wt diff A..B  →  Manifest (one-token --opt=value form).
        let result = classify(&["git", "--work-tree=/wt", "diff", "A..B"]);
        assert_eq!(
            result,
            ClassifyResult {
                classification: Classification::Manifest { range: "A..B" },
                repo_dir_overrides: vec![],
            }
        );
    }

    #[test]
    fn it_skips_exec_path_value_form_and_bare_form() {
        // --exec-path has two shapes: `--exec-path=<dir>` (one token, valued)
        // and bare `--exec-path` (valueless query). Both must be skipped to
        // reach the subcommand. The bare form does NOT consume the next token.
        let valued = classify(&["git", "--exec-path=/usr/libexec", "diff", "A..B"]);
        assert_eq!(
            valued,
            ClassifyResult {
                classification: Classification::Manifest { range: "A..B" },
                repo_dir_overrides: vec![],
            }
        );
        let bare = classify(&["git", "--exec-path", "diff", "A..B"]);
        assert_eq!(
            bare,
            ClassifyResult {
                classification: Classification::Manifest { range: "A..B" },
                repo_dir_overrides: vec![],
            }
        );
    }

    #[test]
    fn it_skips_bare_paginate_valueless_global() {
        // git -p diff A..B  →  Manifest; -p is a valueless pager flag and must
        // NOT consume "diff" as a value.
        let result = classify(&["git", "-p", "diff", "A..B"]);
        assert_eq!(
            result,
            ClassifyResult {
                classification: Classification::Manifest { range: "A..B" },
                repo_dir_overrides: vec![],
            }
        );
    }

    #[test]
    fn it_handles_interleaved_globals_around_dash_c() {
        // git -c k=v -C /repo --no-pager -c j=w diff A..B
        // repo_dir_overrides collects only the -C value; the three
        // -c/--no-pager globals are skipped without disturbing collection or
        // subcommand location.
        let result = classify(&[
            "git",
            "-c",
            "k=v",
            "-C",
            "/repo",
            "--no-pager",
            "-c",
            "j=w",
            "diff",
            "A..B",
        ]);
        assert_eq!(
            result,
            ClassifyResult {
                classification: Classification::Manifest { range: "A..B" },
                repo_dir_overrides: vec!["/repo"],
            }
        );
    }

    #[test]
    fn it_interleaves_multiple_dash_c_among_other_globals_preserving_order() {
        // git -C a -c k=v -C b diff A..B  →  repo_dir_overrides = ["a", "b"] in
        // order, with the -c global skipped in between. Proves collection order
        // is driven by -C position, not by adjacency.
        let result = classify(&["git", "-C", "a", "-c", "k=v", "-C", "b", "diff", "A..B"]);
        assert_eq!(
            result,
            ClassifyResult {
                classification: Classification::Manifest { range: "A..B" },
                repo_dir_overrides: vec!["a", "b"],
            }
        );
    }

    #[test]
    fn it_passes_through_when_dash_c_is_last_token_with_no_value() {
        // git -C  (value-taking global as the final token, no value present)
        // The scanner must not panic on the missing value and must fall to the
        // safe Passthrough default rather than inventing a subcommand.
        let result = classify(&["git", "-C"]);
        assert_eq!(
            result,
            ClassifyResult {
                classification: Classification::Passthrough,
                repo_dir_overrides: vec![],
            }
        );
    }

    #[test]
    fn it_passes_through_when_long_value_global_is_last_token_with_no_value() {
        // git --git-dir  (two-token global with its value missing)  →  Passthrough.
        let result = classify(&["git", "--git-dir"]);
        assert_eq!(
            result,
            ClassifyResult {
                classification: Classification::Passthrough,
                repo_dir_overrides: vec![],
            }
        );
    }

    #[test]
    fn it_passes_through_when_only_globals_present_with_no_subcommand() {
        // git -c k=v --no-pager  (all globals, no subcommand at all)  →  Passthrough.
        let result = classify(&["git", "-c", "k=v", "--no-pager"]);
        assert_eq!(
            result,
            ClassifyResult {
                classification: Classification::Passthrough,
                repo_dir_overrides: vec![],
            }
        );
    }

    #[test]
    fn it_passes_through_unknown_flag_after_valid_globals_discarding_overrides() {
        // git -C /repo --bogus diff A..B  →  the unknown flag forces the safe
        // default, and repo_dir_overrides is reset to empty so a stale -C cannot
        // leak into a Passthrough result and mis-route a later handler.
        let result = classify(&["git", "-C", "/repo", "--bogus", "diff", "A..B"]);
        assert_eq!(
            result,
            ClassifyResult {
                classification: Classification::Passthrough,
                repo_dir_overrides: vec![],
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

    // --- ShowSnapshot: blob/object-spec passthrough (issue #381) ---

    #[test]
    fn it_passes_through_git_show_with_branch_colon_path() {
        // git show origin/main:.lefthook.yml — object spec, must NOT try peel_to_commit
        assert_eq!(
            classification(&["git", "show", "origin/main:.lefthook.yml"]),
            Classification::Passthrough
        );
    }

    #[test]
    fn it_passes_through_git_show_with_sha_colon_path() {
        // git show 0123abcdef1234567890abcdef1234567890abcd:apps/x/package.json
        assert_eq!(
            classification(&[
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
            classification(&["git", "show", "HEAD:"]),
            Classification::Passthrough
        );
    }

    #[test]
    fn it_classifies_git_show_commit_message_search_as_snapshot() {
        // git show :/fix typo — colon-slash is commit search, NOT blob spec; before-colon is empty
        assert_eq!(
            classification(&["git", "show", ":/fix typo"]),
            Classification::ShowSnapshot { sha: ":/fix typo" }
        );
    }

    #[test]
    fn it_classifies_git_show_plain_sha_without_colon_as_snapshot() {
        // git show abc1234 — no colon at all, stays intercepted
        assert_eq!(
            classification(&["git", "show", "abc1234"]),
            Classification::ShowSnapshot { sha: "abc1234" }
        );
    }

    #[test]
    fn it_passes_through_git_show_head_colon_nested_path() {
        // git show HEAD:dir/file.txt — rev:path is a blob spec
        assert_eq!(
            classification(&["git", "show", "HEAD:dir/file.txt"]),
            Classification::Passthrough
        );
    }

    #[test]
    fn it_classifies_git_show_bare_colon_slash_regex_as_snapshot() {
        // git show :/regex — before-colon is empty, so it's commit-search syntax
        assert_eq!(
            classification(&["git", "show", ":/another regex"]),
            Classification::ShowSnapshot {
                sha: ":/another regex"
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

    // --- Bug fixes: index/stage blob specs and rev:abs-path must passthrough ---

    #[test]
    fn it_passes_through_git_show_stage0_blob_spec() {
        // git show :staged.txt — stage-0 index blob spec; empty before-colon is valid
        assert_eq!(
            classification(&["git", "show", ":staged.txt"]),
            Classification::Passthrough
        );
    }

    #[test]
    fn it_passes_through_git_show_explicit_stage0_blob_spec() {
        // git show :0:staged.txt — explicit stage-0 index blob spec
        assert_eq!(
            classification(&["git", "show", ":0:staged.txt"]),
            Classification::Passthrough
        );
    }

    #[test]
    fn it_passes_through_git_show_explicit_stage1_blob_spec() {
        // git show :1:file — merge stage 1 (common ancestor)
        assert_eq!(
            classification(&["git", "show", ":1:file.txt"]),
            Classification::Passthrough
        );
    }

    #[test]
    fn it_passes_through_git_show_explicit_stage3_blob_spec() {
        // git show :3:file — merge stage 3 (theirs)
        assert_eq!(
            classification(&["git", "show", ":3:conflict.txt"]),
            Classification::Passthrough
        );
    }

    #[test]
    fn it_passes_through_git_show_rev_colon_absolute_path() {
        // git show HEAD:/some/absolute/path — rev:abs-path is a valid blob lookup
        assert_eq!(
            classification(&["git", "show", "HEAD:/some/absolute/path"]),
            Classification::Passthrough
        );
    }

    #[test]
    fn it_classifies_git_show_commit_search_as_snapshot() {
        // git show :/fix typo — :/text is commit-message-search, NOT a blob spec
        assert_eq!(
            classification(&["git", "show", ":/fix typo"]),
            Classification::ShowSnapshot { sha: ":/fix typo" }
        );
    }

    #[test]
    fn it_classifies_git_show_bare_colon_slash_as_snapshot() {
        // git show :/x — the shortest commit-search form
        assert_eq!(
            classification(&["git", "show", ":/x"]),
            Classification::ShowSnapshot { sha: ":/x" }
        );
    }
}
