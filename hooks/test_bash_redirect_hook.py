"""Unit tests for hooks/bash_redirect_hook.py.

These tests exercise the public API of the hook module directly —
no subprocesses, no filesystem. The BDD scenarios cover the end-to-end
subprocess contract; these tests cover the internal logic with fast,
hermetic unit assertions.

Run with:
    python3 -m unittest test_bash_redirect_hook   (stdlib, no install)
    python3 -m pytest   test_bash_redirect_hook.py  (if pytest available)
"""

from __future__ import annotations

import sys
import unittest
from pathlib import Path

# Allow running from any cwd: add the hooks/ directory to the import path.
sys.path.insert(0, str(Path(__file__).parent))

from bash_redirect_hook import (
    _advice_with_echo,
    _classify_git_command,
    _drop_heredoc_bodies,
    _has_pickaxe_flag,
    _has_ref_range,
    _is_functionally_empty,
    _matches_gh_pr_diff,
    decide_redirect,
    tokenize_command,
)


# ---------------------------------------------------------------------------
# _is_functionally_empty
# ---------------------------------------------------------------------------


class TestIsFunctionallyEmpty(unittest.TestCase):
    def test_empty_string_is_empty(self):
        self.assertTrue(_is_functionally_empty(""))

    def test_whitespace_only_is_empty(self):
        self.assertTrue(_is_functionally_empty("   \t  "))

    def test_newline_only_is_empty(self):
        self.assertTrue(_is_functionally_empty("\n"))

    def test_escaped_newline_sequence_is_empty(self):
        # The BDD scenario pipes the literal four-character string "\\n  \\n".
        # _is_functionally_empty must translate escape sequences before checking.
        self.assertTrue(_is_functionally_empty("\\n  \\n"))

    def test_mixed_escape_sequences_are_empty(self):
        self.assertTrue(_is_functionally_empty("\\n\\t\\r"))

    def test_non_whitespace_is_not_empty(self):
        self.assertFalse(_is_functionally_empty("git diff"))

    def test_json_is_not_empty(self):
        self.assertFalse(_is_functionally_empty('{"tool_name": "Bash"}'))


# ---------------------------------------------------------------------------
# _has_ref_range
# ---------------------------------------------------------------------------


class TestHasRefRange(unittest.TestCase):
    def test_double_dot_range_is_detected(self):
        self.assertTrue(_has_ref_range(["main..HEAD"]))

    def test_triple_dot_range_is_detected(self):
        self.assertTrue(_has_ref_range(["main...HEAD"]))

    def test_bare_double_dot_is_excluded(self):
        # ".." is the parent-directory shorthand, not a ref range.
        self.assertFalse(_has_ref_range([".."]));

    def test_bare_triple_dot_is_excluded(self):
        self.assertFalse(_has_ref_range(["..."]))

    def test_no_range_returns_false(self):
        self.assertFalse(_has_ref_range(["git", "diff", "--stat"]))

    def test_range_anywhere_in_list_is_detected(self):
        self.assertTrue(_has_ref_range(["git", "diff", "feature..main"]))


# ---------------------------------------------------------------------------
# _has_pickaxe_flag
# ---------------------------------------------------------------------------


class TestHasPickaxeFlag(unittest.TestCase):
    def test_standalone_dash_s(self):
        self.assertTrue(_has_pickaxe_flag(["-S"]))

    def test_standalone_dash_g(self):
        self.assertTrue(_has_pickaxe_flag(["-G"]))

    def test_concatenated_dash_s_term(self):
        # -Sfoo is a single token when the user writes it without a space.
        self.assertTrue(_has_pickaxe_flag(["-Sfoo"]))

    def test_concatenated_dash_g_term(self):
        self.assertTrue(_has_pickaxe_flag(["-Gbar"]))

    def test_other_flags_not_matched(self):
        self.assertFalse(_has_pickaxe_flag(["-p", "--oneline", "-n"]))

    def test_empty_list_returns_false(self):
        self.assertFalse(_has_pickaxe_flag([]))


# ---------------------------------------------------------------------------
# _classify_git_command
# ---------------------------------------------------------------------------


class TestClassifyGitCommand(unittest.TestCase):
    def test_git_diff_with_ref_range_returns_change_manifest(self):
        self.assertEqual(
            _classify_git_command(["git", "diff", "main..HEAD"]),
            "get_change_manifest",
        )

    def test_git_log_with_ref_range_returns_commit_history(self):
        self.assertEqual(
            _classify_git_command(["git", "log", "main..HEAD"]),
            "get_commit_history",
        )

    def test_git_log_with_pickaxe_returns_function_context(self):
        # Pickaxe check must take priority over the ref-range check for git log.
        self.assertEqual(
            _classify_git_command(["git", "log", "-S", "foo"]),
            "get_function_context",
        )

    def test_git_log_pickaxe_priority_over_range(self):
        # Both pickaxe AND range present — pickaxe wins.
        self.assertEqual(
            _classify_git_command(["git", "log", "-S", "foo", "main..HEAD"]),
            "get_function_context",
        )

    def test_git_blame_returns_file_snapshots(self):
        self.assertEqual(
            _classify_git_command(["git", "blame", "src/main.rs"]),
            "get_file_snapshots",
        )

    def test_git_show_returns_file_snapshots(self):
        self.assertEqual(
            _classify_git_command(["git", "show", "abc123:src/main.rs"]),
            "get_file_snapshots",
        )

    def test_git_status_returns_none(self):
        self.assertIsNone(_classify_git_command(["git", "status"]))

    def test_git_add_returns_none(self):
        self.assertIsNone(_classify_git_command(["git", "add", "file.txt"]))

    def test_git_commit_returns_none(self):
        self.assertIsNone(_classify_git_command(["git", "commit", "-m", "msg"]))

    def test_git_push_returns_none(self):
        self.assertIsNone(_classify_git_command(["git", "push", "origin"]))

    def test_git_fetch_returns_none(self):
        self.assertIsNone(_classify_git_command(["git", "fetch", "origin"]))

    def test_non_git_command_returns_none(self):
        self.assertIsNone(_classify_git_command(["ls", "-la"]))

    def test_empty_list_returns_none(self):
        self.assertIsNone(_classify_git_command([]))

    def test_git_diff_without_range_returns_none(self):
        # Plain "git diff" (working-tree diff) — no range token, no redirect.
        self.assertIsNone(_classify_git_command(["git", "diff"]))


# ---------------------------------------------------------------------------
# tokenize_command
# ---------------------------------------------------------------------------


class TestTokenizeCommand(unittest.TestCase):
    def test_simple_git_diff(self):
        result = tokenize_command("git diff main..HEAD")
        self.assertEqual(result, [["git", "diff", "main..HEAD"]])

    def test_compound_and_command(self):
        result = tokenize_command("cd /tmp && git diff main..HEAD")
        self.assertIn(["git", "diff", "main..HEAD"], result)

    def test_subshell_parentheses(self):
        result = tokenize_command("(git log main..HEAD)")
        self.assertIn(["git", "log", "main..HEAD"], result)

    def test_pipeline(self):
        result = tokenize_command("git diff main..HEAD | grep foo")
        self.assertIn(["git", "diff", "main..HEAD"], result)

    def test_backtick_normalization(self):
        # Backticks are converted to spaces; outer git diff is still found.
        result = tokenize_command(
            "cd `git rev-parse --show-toplevel` && git diff main..HEAD"
        )
        git_diff = [
            c for c in result if c and c[0] == "git" and len(c) > 1 and c[1] == "diff"
        ]
        self.assertTrue(git_diff, f"git diff candidate missing from {result}")

    def test_variable_not_expanded(self):
        # $BASE must appear verbatim — never as the env-var's value.
        result = tokenize_command("git diff $BASE..HEAD")
        flat_tokens = [tok for cand in result for tok in cand]
        self.assertTrue(
            any("$BASE" in tok for tok in flat_tokens),
            f"Expected literal '$BASE' in tokens, got: {flat_tokens}",
        )

    def test_empty_command_returns_empty_list(self):
        self.assertEqual(tokenize_command(""), [])

    def test_heredoc_body_is_skipped(self):
        # git log inside a heredoc body must NOT produce a candidate.
        command = "cat <<EOF\ngit log a..b\nEOF\n"
        result = tokenize_command(command)
        git_log = [c for c in result if c and c[0] == "git"]
        self.assertFalse(git_log, f"git command inside heredoc body leaked: {result}")

    def test_tokenizer_resumes_after_heredoc_terminator(self):
        # After the closing tag, git diff on the next line must be detected.
        command = "cat <<EOF\ngit log a..b\nEOF\ngit diff main..HEAD"
        result = tokenize_command(command)
        git_diff = [
            c for c in result if c and c[0] == "git" and len(c) > 1 and c[1] == "diff"
        ]
        self.assertTrue(git_diff, f"git diff after heredoc not found in: {result}")
        git_log = [
            c for c in result if c and c[0] == "git" and len(c) > 1 and c[1] == "log"
        ]
        self.assertFalse(
            git_log,
            f"git log inside heredoc body leaked into candidates: {result}",
        )


# ---------------------------------------------------------------------------
# _drop_heredoc_bodies
# ---------------------------------------------------------------------------


class TestDropHeredocBodies(unittest.TestCase):
    def test_simple_heredoc_body_is_dropped(self):
        tokens = ["cat", "<<", "EOF", "\n", "git", "\n", "EOF", "\n", "echo", "done"]
        result = _drop_heredoc_bodies(tokens)
        self.assertNotIn("git", result)
        self.assertIn("echo", result)
        self.assertIn("done", result)

    def test_dash_form_heredoc_body_is_dropped(self):
        # shlex glues the "-" onto the tag word: << then -EOF
        tokens = ["cat", "<<", "-EOF", "\n", "git", "\n", "EOF", "\n", "echo", "done"]
        result = _drop_heredoc_bodies(tokens)
        self.assertNotIn("git", result)
        self.assertIn("echo", result)

    def test_content_before_heredoc_is_preserved(self):
        tokens = ["echo", "hi", "<<", "EOF", "\n", "body", "\n", "EOF"]
        result = _drop_heredoc_bodies(tokens)
        self.assertIn("echo", result)
        self.assertIn("hi", result)
        self.assertNotIn("body", result)

    def test_empty_token_list_returns_empty(self):
        self.assertEqual(_drop_heredoc_bodies([]), [])

    def test_no_heredoc_passes_through_unchanged(self):
        tokens = ["git", "diff", "main..HEAD"]
        self.assertEqual(_drop_heredoc_bodies(tokens), tokens)


# ---------------------------------------------------------------------------
# _advice_with_echo
# ---------------------------------------------------------------------------


class TestAdviceWithEcho(unittest.TestCase):
    def test_echo_appends_verbatim_tokens(self):
        tokens = ["git", "diff", "main..HEAD"]
        result = _advice_with_echo("base advice", tokens)
        self.assertIn("You ran: git diff main..HEAD", result)

    def test_variable_not_expanded_in_echo(self):
        # The token list contains the literal string "$BASE". The echo must
        # reproduce it verbatim — proving no os.path.expandvars or shell
        # expansion happened anywhere in the call chain.
        tokens = ["git", "diff", "$BASE..HEAD"]
        result = _advice_with_echo("base advice", tokens)
        self.assertIn(
            "$BASE..HEAD",
            result,
            f"Expected literal '$BASE..HEAD' in advice, got: {result!r}",
        )

    def test_base_advice_is_included(self):
        tokens = ["git", "log", "main..HEAD"]
        result = _advice_with_echo("USE GET_COMMIT_HISTORY", tokens)
        self.assertIn("USE GET_COMMIT_HISTORY", result)


# ---------------------------------------------------------------------------
# _matches_gh_pr_diff
# ---------------------------------------------------------------------------


class TestMatchesGhPrDiff(unittest.TestCase):
    def test_plain_gh_pr_diff_is_matched(self):
        self.assertTrue(_matches_gh_pr_diff("gh pr diff 123"))

    def test_compound_gh_pr_diff_is_matched(self):
        self.assertTrue(_matches_gh_pr_diff("cd /tmp && gh pr diff 123"))

    def test_gh_pr_view_is_not_matched(self):
        self.assertFalse(_matches_gh_pr_diff("gh pr view 123"))

    def test_git_diff_is_not_matched(self):
        self.assertFalse(_matches_gh_pr_diff("git diff main..HEAD"))

    def test_empty_command_is_not_matched(self):
        self.assertFalse(_matches_gh_pr_diff(""))


# ---------------------------------------------------------------------------
# decide_redirect
# ---------------------------------------------------------------------------


class TestDecideRedirect(unittest.TestCase):
    def _bash_payload(self, command: str) -> dict:
        return {
            "tool_name": "Bash",
            "tool_input": {"command": command},
            "hook_event_name": "PreToolUse",
        }

    def test_git_diff_with_range_returns_advise(self):
        decision = decide_redirect(self._bash_payload("git diff main..HEAD"))
        self.assertEqual(decision.mode, "advise")
        self.assertIn("get_change_manifest", decision.advice)

    def test_git_log_with_range_returns_advise(self):
        decision = decide_redirect(self._bash_payload("git log main..HEAD"))
        self.assertEqual(decision.mode, "advise")
        self.assertIn("get_commit_history", decision.advice)

    def test_git_log_pickaxe_returns_advise_for_function_context(self):
        decision = decide_redirect(self._bash_payload("git log -S foo"))
        self.assertEqual(decision.mode, "advise")
        self.assertIn("get_function_context", decision.advice)

    def test_git_blame_returns_advise_for_file_snapshots(self):
        decision = decide_redirect(self._bash_payload("git blame src/main.rs"))
        self.assertEqual(decision.mode, "advise")
        self.assertIn("get_file_snapshots", decision.advice)

    def test_git_status_returns_silent(self):
        decision = decide_redirect(self._bash_payload("git status"))
        self.assertEqual(decision.mode, "silent")

    def test_gh_pr_diff_returns_block(self):
        decision = decide_redirect(self._bash_payload("gh pr diff 123"))
        self.assertEqual(decision.mode, "block")
        self.assertIn("get_change_manifest", decision.message)

    def test_mcp_github_get_commit_tool_name_returns_block(self):
        payload = {
            "tool_name": "mcp__github__get_commit",
            "tool_input": {},
            "hook_event_name": "PreToolUse",
        }
        decision = decide_redirect(payload)
        self.assertEqual(decision.mode, "block")
        self.assertIn("git-prism", decision.message)

    def test_mcp_github_get_commit_as_bash_command_returns_block(self):
        decision = decide_redirect(
            self._bash_payload("mcp__github__get_commit owner=foo repo=bar sha=abc")
        )
        self.assertEqual(decision.mode, "block")

    def test_non_bash_tool_is_silent(self):
        payload = {
            "tool_name": "Read",
            "tool_input": {"file_path": "/tmp/file"},
            "hook_event_name": "PreToolUse",
        }
        decision = decide_redirect(payload)
        self.assertEqual(decision.mode, "silent")

    def test_empty_command_is_silent(self):
        decision = decide_redirect(self._bash_payload(""))
        self.assertEqual(decision.mode, "silent")

    def test_missing_tool_name_is_silent(self):
        decision = decide_redirect({})
        self.assertEqual(decision.mode, "silent")

    def test_variable_in_command_does_not_expand(self):
        # The advice text must contain the literal "$BASE", not an expanded value.
        decision = decide_redirect(self._bash_payload("git diff $BASE..HEAD"))
        self.assertEqual(decision.mode, "advise")
        self.assertIn(
            "$BASE",
            decision.advice,
            f"Expected literal '$BASE' in advice, got: {decision.advice!r}",
        )

    def test_heredoc_body_git_command_is_not_advised(self):
        # git log inside a heredoc body must NOT trigger advice.
        command = "cat <<EOF\ngit log a..b\nEOF\n"
        decision = decide_redirect(self._bash_payload(command))
        self.assertEqual(
            decision.mode,
            "silent",
            f"Expected silent for heredoc-body git, got mode={decision.mode!r}",
        )

    def test_git_diff_after_heredoc_is_advised(self):
        # After the heredoc closes, git diff must still be detected.
        command = "cat <<EOF\ngit log a..b\nEOF\ngit diff main..HEAD"
        decision = decide_redirect(self._bash_payload(command))
        self.assertEqual(decision.mode, "advise")
        self.assertIn("get_change_manifest", decision.advice)


if __name__ == "__main__":
    unittest.main()
