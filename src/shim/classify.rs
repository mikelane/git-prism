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
    /// `git show <sha>` → `get_file_snapshots` (show variant)
    ShowSnapshot { sha: &'a str },
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
    // argv[0] may be an absolute path (e.g. /tmp/bin/gh); extract only the
    // filename component for dispatch.
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
        _ => Classification::Passthrough,
    }
}

/// Options that only change how git prints a result the handler already
/// reports in full, so intercepting them cannot change the answer.
const PRESENTATION_FLAGS: [&str; 2] = ["--no-color", "--color"];
const LOG_FLAGS: [&str; 3] = ["--oneline", "--decorate", "--no-decorate"];
const DIFF_FLAGS: [&str; 3] = ["--name-only", "--name-status", "--no-ext-diff"];
const SHOW_FLAGS: [&str; 2] = ["--name-only", "--name-status"];

/// Collect the positional arguments of `tokens`, or `None` when any option is
/// not one the handler honours.
///
/// A handler returns the whole answer for one range or object. Any option or
/// argument that would narrow, reorder, or reinterpret that answer (`--`
/// pathspecs, `-n`, `-R`, `--no-index`, a typo) must reach real git, which
/// either honours it or rejects it with its own exit status. `allowed_flags`
/// extends the presentation-only flags every command accepts. Pickaxe
/// searches (`-S`, `-G`, `--pickaxe-*`) are unknown options here: the handlers
/// ignore the term, so they must reach real git.
fn honoured_positionals<'a>(tokens: &[&'a str], allowed_flags: &[&str]) -> Option<Vec<&'a str>> {
    let mut positionals = Vec::new();
    for token in tokens.iter().copied() {
        if !token.starts_with('-') || token == "-" {
            positionals.push(token);
        } else if !PRESENTATION_FLAGS.contains(&token) && !allowed_flags.contains(&token) {
            return None;
        }
    }
    Some(positionals)
}

fn classify_log<'a>(rest: &[&'a str]) -> Classification<'a> {
    match honoured_positionals(rest, &LOG_FLAGS).as_deref() {
        Some([range]) if has_ref_range(range) && !is_three_dot_range(range) => {
            Classification::History { range }
        }
        _ => Classification::Passthrough,
    }
}

fn classify_diff<'a>(rest: &[&'a str]) -> Classification<'a> {
    match honoured_positionals(rest, &DIFF_FLAGS).as_deref() {
        Some([range]) if has_ref_range(range) && !is_three_dot_range(range) => {
            Classification::Manifest { range }
        }
        _ => Classification::Passthrough,
    }
}

fn classify_show<'a>(rest: &[&'a str]) -> Classification<'a> {
    let Some(positionals) = honoured_positionals(rest, &SHOW_FLAGS) else {
        return Classification::Passthrough;
    };
    // Exactly one object spec; several (or a `--` pathspec) are real git's job.
    let [spec] = positionals.as_slice() else {
        return Classification::Passthrough;
    };
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
    // `<rev>^{tree}`, `<rev>^{}` etc. peel to a non-commit object.
    if spec.contains("^{") {
        return Classification::Passthrough;
    }
    Classification::ShowSnapshot { sha: spec }
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

/// Return `true` for `a...b`. Real git resolves it against the merge base
/// (`diff`) or as a symmetric difference (`log`); the handlers implement
/// neither, so the shim leaves it to real git.
fn is_three_dot_range(token: &str) -> bool {
    token.contains("...")
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- Passthrough cases ---

    #[test]
    fn it_passes_through_diff_quiet() {
        assert_eq!(
            classify(&["git", "diff", "--quiet", "a..b"]),
            Classification::Passthrough
        );
    }

    #[test]
    fn it_passes_through_diff_exit_code() {
        assert_eq!(
            classify(&["git", "diff", "--exit-code", "a..b"]),
            Classification::Passthrough
        );
    }

    #[test]
    fn it_passes_through_show_tree_peel_spec() {
        assert_eq!(
            classify(&["git", "show", "HEAD^{tree}"]),
            Classification::Passthrough
        );
    }

    #[test]
    fn it_passes_through_show_empty_peel_spec() {
        assert_eq!(
            classify(&["git", "show", "v1^{}"]),
            Classification::Passthrough
        );
    }

    #[test]
    fn it_still_snapshots_plain_show_head() {
        assert_eq!(
            classify(&["git", "show", "HEAD"]),
            Classification::ShowSnapshot { sha: "HEAD" }
        );
    }

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
    fn it_passes_through_git_diff_three_dot_range() {
        assert_eq!(
            classify(&["git", "diff", "main...HEAD"]),
            Classification::Passthrough
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
    fn it_passes_through_git_log_three_dot_range() {
        assert_eq!(
            classify(&["git", "log", "main...HEAD"]),
            Classification::Passthrough
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

    // --- Pickaxe (git log -S/-G) passes through: the handlers ignore the term ---

    #[test]
    fn it_passes_through_git_log_pickaxe_in_every_form() {
        let cases: [&[&str]; 8] = [
            &["git", "log", "-S", "myfunction"],
            &["git", "log", "-Smyfunction"],
            &["git", "log", "-G", "pattern"],
            &["git", "log", "-Gpattern"],
            &["git", "log", "-S", "term", "main..HEAD"],
            &["git", "log", "-Sfoo", "main..HEAD"],
            &["git", "log", "-S", "main..HEAD"],
            &["git", "log", "--pickaxe-regex", "-S", "x", "main..HEAD"],
        ];
        let classified = cases.map(|argv| (argv, classify(argv)));
        let expected = cases.map(|argv| (argv, Classification::Passthrough));
        assert_eq!(classified, expected);
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
    fn it_classifies_git_show_bare_colon_slash_regex_as_snapshot() {
        // git show :/regex — before-colon is empty, so it's commit-search syntax
        assert_eq!(
            classify(&["git", "show", ":/another regex"]),
            Classification::ShowSnapshot {
                sha: ":/another regex"
            }
        );
    }

    // --- git blame always passes through (#387; structured blame tracked in #388) ---

    #[test]
    fn it_passes_through_every_git_blame_form() {
        let cases: [&[&str]; 4] = [
            &["git", "blame", "src/main.rs"],
            &["git", "blame", "--no-color", "src/main.rs"],
            &["git", "blame", "-w", "src/main.rs"],
            &["git", "blame", "--porcelain", "src/main.rs"],
        ];
        let classified = cases.map(|argv| (argv, classify(argv)));
        let expected = cases.map(|argv| (argv, Classification::Passthrough));
        assert_eq!(classified, expected);
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

    // --- Bug fixes: index/stage blob specs and rev:abs-path must passthrough ---

    #[test]
    fn it_passes_through_git_show_stage0_blob_spec() {
        // git show :staged.txt — stage-0 index blob spec; empty before-colon is valid
        assert_eq!(
            classify(&["git", "show", ":staged.txt"]),
            Classification::Passthrough
        );
    }

    #[test]
    fn it_passes_through_git_show_explicit_stage0_blob_spec() {
        // git show :0:staged.txt — explicit stage-0 index blob spec
        assert_eq!(
            classify(&["git", "show", ":0:staged.txt"]),
            Classification::Passthrough
        );
    }

    #[test]
    fn it_passes_through_git_show_explicit_stage1_blob_spec() {
        // git show :1:file — merge stage 1 (common ancestor)
        assert_eq!(
            classify(&["git", "show", ":1:file.txt"]),
            Classification::Passthrough
        );
    }

    #[test]
    fn it_passes_through_git_show_explicit_stage3_blob_spec() {
        // git show :3:file — merge stage 3 (theirs)
        assert_eq!(
            classify(&["git", "show", ":3:conflict.txt"]),
            Classification::Passthrough
        );
    }

    #[test]
    fn it_passes_through_git_show_rev_colon_absolute_path() {
        // git show HEAD:/some/absolute/path — rev:abs-path is a valid blob lookup
        assert_eq!(
            classify(&["git", "show", "HEAD:/some/absolute/path"]),
            Classification::Passthrough
        );
    }

    #[test]
    fn it_classifies_git_show_commit_search_as_snapshot() {
        // git show :/fix typo — :/text is commit-message-search, NOT a blob spec
        assert_eq!(
            classify(&["git", "show", ":/fix typo"]),
            Classification::ShowSnapshot { sha: ":/fix typo" }
        );
    }

    #[test]
    fn it_classifies_git_show_bare_colon_slash_as_snapshot() {
        // git show :/x — the shortest commit-search form
        assert_eq!(
            classify(&["git", "show", ":/x"]),
            Classification::ShowSnapshot { sha: ":/x" }
        );
    }

    // --- Unhonoured options must reach real git ---

    #[test]
    fn it_passes_through_options_and_arguments_the_handlers_do_not_honour() {
        let cases: [&[&str]; 14] = [
            &["git", "diff", "--bogus", "a..b"],
            &["git", "diff", "--no-index", "a..b"],
            &["git", "diff", "-R", "a..b"],
            &["git", "diff", "a..b", "--", "path"],
            &["git", "diff", "a..b", "other"],
            &["git", "log", "--bogus", "a..b"],
            &["git", "log", "-n", "0", "a..b"],
            &["git", "log", "-n0", "a..b"],
            &["git", "log", "--max-count=1", "a..b"],
            &["git", "log", "a..b", "--", "path"],
            &["git", "log", "-S", "foo", "main"],
            &["git", "show", "--bogus", "abc1234"],
            &["git", "show", "abc1234", "def5678"],
            &["git", "blame", "src/main.rs", "extra-arg"],
        ];
        let classified = cases.map(|argv| (argv, classify(argv)));
        let expected = cases.map(|argv| (argv, Classification::Passthrough));
        assert_eq!(classified, expected);
    }

    #[test]
    fn it_passes_through_show_with_a_pathspec() {
        assert_eq!(
            classify(&["git", "show", "abc1234", "--", "f.txt"]),
            Classification::Passthrough
        );
    }

    #[test]
    fn it_still_intercepts_a_diff_with_a_presentation_flag() {
        assert_eq!(
            classify(&["git", "diff", "--no-color", "--name-only", "a..b"]),
            Classification::Manifest { range: "a..b" }
        );
    }
}
