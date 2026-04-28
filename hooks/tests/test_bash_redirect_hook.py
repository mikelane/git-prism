"""Unit tests for the bash command tokenizer.

Per ADR 0008 Decision 1 the parser is `shlex.shlex(posix=True,
punctuation_chars=True)` plus two wrappers (heredoc body skip + backtick
normalization). These tests exercise each shape from the ADR's coverage
matrix.
"""

from __future__ import annotations

import json
import subprocess
import sys
from pathlib import Path

import pytest

from bash_redirect_hook import Invocation, decide_redirect, tokenize_command

HOOKS_DIR = Path(__file__).resolve().parent.parent


def test_tokenize_plain_git_diff() -> None:
    invocations = tokenize_command("git diff main..HEAD")

    assert invocations == [
        Invocation(
            subcommand="diff",
            args=["main..HEAD"],
            position=0,
            raw="git diff main..HEAD",
        )
    ]


def test_tokenize_plain_git_log_with_oneline() -> None:
    """Triangulates against a hardcoded `diff main..HEAD` return: a
    different subcommand AND different args force the parser to actually
    read the input.
    """
    invocations = tokenize_command("git log --oneline")

    assert len(invocations) == 1
    assert invocations[0].subcommand == "log"
    assert invocations[0].args == ["--oneline"]


def test_tokenize_returns_empty_list_when_no_git_invocation() -> None:
    """No `git` token in the input means no invocations to redirect."""
    assert tokenize_command("ls -la") == []


def test_tokenize_empty_string_returns_empty_list() -> None:
    assert tokenize_command("") == []


# ---------------------------------------------------------------------------
# ADR 0008 Decision 4 coverage matrix — every accepted shape from the
# decision table appears here as a parametrized table-driven case so
# regressions are easy to localize.
# ---------------------------------------------------------------------------


@pytest.mark.parametrize(
    ("command", "expected"),
    [
        pytest.param(
            "cd /tmp && git diff main..HEAD",
            [("diff", ["main..HEAD"])],
            id="compound-and",
        ),
        pytest.param(
            "git diff main..HEAD; ls",
            [("diff", ["main..HEAD"])],
            id="semicolon-separator",
        ),
        pytest.param(
            "false || git diff main..HEAD",
            [("diff", ["main..HEAD"])],
            id="compound-or",
        ),
        pytest.param(
            "(git log main..HEAD)",
            [("log", ["main..HEAD"])],
            id="subshell-parens",
        ),
        pytest.param(
            "git diff main..HEAD | grep foo",
            [("diff", ["main..HEAD"])],
            id="pipeline",
        ),
        pytest.param(
            "git diff $BASE..HEAD",
            [("diff", ["$BASE..HEAD"])],
            id="var-expansion-preserved-literally",
        ),
        pytest.param(
            "cd $(git rev-parse --show-toplevel) && git diff main..HEAD",
            [
                ("rev-parse", ["--show-toplevel"]),
                ("diff", ["main..HEAD"]),
            ],
            id="command-substitution-dollar-paren",
        ),
        pytest.param(
            "cd `git rev-parse --show-toplevel` && git diff main..HEAD",
            [
                ("rev-parse", ["--show-toplevel"]),
                ("diff", ["main..HEAD"]),
            ],
            id="backtick-substitution-normalized",
        ),
        pytest.param(
            'git diff "main..HEAD"',
            [("diff", ["main..HEAD"])],
            id="quoted-arg-stripped-by-shlex",
        ),
        pytest.param(
            "git blame src/server.rs",
            [("blame", ["src/server.rs"])],
            id="git-blame",
        ),
        pytest.param(
            "git status",
            [("status", [])],
            id="status-no-args-still-recognized",
        ),
        pytest.param(
            "git add file.txt",
            [("add", ["file.txt"])],
            id="add-no-redirect-target",
        ),
    ],
)
def test_tokenize_recognized_shapes(
    command: str, expected: list[tuple[str, list[str]]]
) -> None:
    invocations = tokenize_command(command)
    actual = [(inv.subcommand, inv.args) for inv in invocations]
    assert actual == expected


@pytest.mark.parametrize(
    "command",
    [
        pytest.param("ls -la", id="no-git-token"),
        pytest.param('echo "git diff main..HEAD"', id="git-as-string-literal"),
        pytest.param("git_helper foo", id="function-name-with-git-prefix"),
        pytest.param("# git diff main..HEAD", id="leading-comment"),
    ],
)
def test_tokenize_does_not_false_positive(command: str) -> None:
    """Strings that mention git but don't invoke it must not be flagged."""
    assert tokenize_command(command) == []


# ---------------------------------------------------------------------------
# Heredoc coverage — the parser must skip the body of every heredoc form
# (default, dash, single-quoted) and resume tokenizing after the closing
# tag.
# ---------------------------------------------------------------------------


def test_tokenize_skips_default_heredoc_body() -> None:
    """`<<EOF` body must be opaque — bait `git log a..b` inside is data."""
    command = "cat <<EOF\ngit diff a..b\ngit log main..HEAD\nEOF\ngit status"

    invocations = tokenize_command(command)

    assert [(inv.subcommand, inv.args) for inv in invocations] == [
        ("status", []),
    ]


def test_tokenize_skips_tab_stripped_heredoc_body() -> None:
    """`<<-EOF` strips leading tabs from the body but is still skipped."""
    command = "cat <<-EOF\n\tgit diff a..b\n\tEOF\ngit status"

    invocations = tokenize_command(command)

    assert [(inv.subcommand, inv.args) for inv in invocations] == [
        ("status", []),
    ]


def test_tokenize_skips_quoted_heredoc_body() -> None:
    """`<<'EOF'` quoted form disables expansion; body still opaque."""
    command = "cat <<'EOF'\ngit diff a..b\nEOF\ngit status"

    invocations = tokenize_command(command)

    assert [(inv.subcommand, inv.args) for inv in invocations] == [
        ("status", []),
    ]


def test_tokenize_resumes_after_heredoc_terminator() -> None:
    """Bait inside body must not survive; post-EOF git command must be seen."""
    command = "cat <<EOF\ngit log a..b\nEOF\ngit diff main..HEAD"

    invocations = tokenize_command(command)

    assert [(inv.subcommand, inv.args) for inv in invocations] == [
        ("diff", ["main..HEAD"]),
    ]


# ---------------------------------------------------------------------------
# decide_redirect() — classification logic over a tokenized invocation list
# ---------------------------------------------------------------------------


def test_decide_redirect_no_invocations_is_allow() -> None:
    """Empty invocation list means no git command — allow with no advice."""
    decision = decide_redirect([])

    assert decision.action == "allow"
    assert decision.advice == ""


def test_decide_redirect_advises_on_git_diff() -> None:
    """`git diff` is on the watch list and routes to get_change_manifest."""
    invocations = tokenize_command("git diff main..HEAD")

    decision = decide_redirect(invocations)

    assert decision.action == "advisory"
    assert "get_change_manifest" in decision.advice


def test_decide_redirect_advises_on_git_log() -> None:
    invocations = tokenize_command("git log --oneline main..HEAD")

    decision = decide_redirect(invocations)

    assert decision.action == "advisory"
    assert "get_commit_history" in decision.advice


def test_decide_redirect_advises_on_git_show() -> None:
    invocations = tokenize_command("git show HEAD")

    decision = decide_redirect(invocations)

    assert decision.action == "advisory"
    assert "get_file_snapshots" in decision.advice


def test_decide_redirect_advises_on_git_blame() -> None:
    invocations = tokenize_command("git blame src/server.rs")

    decision = decide_redirect(invocations)

    assert decision.action == "advisory"
    assert "get_file_snapshots" in decision.advice


@pytest.mark.parametrize(
    "command",
    [
        "git status",
        "git add file.txt",
        "git commit -m hi",
        "git push origin",
        "git fetch origin",
    ],
)
def test_decide_redirect_allows_write_side_commands(command: str) -> None:
    """Write-side commands and `git status` must not trigger advice."""
    invocations = tokenize_command(command)

    decision = decide_redirect(invocations)

    assert decision.action == "allow"
    assert decision.advice == ""


def test_decide_redirect_skips_inner_git_rev_parse() -> None:
    """`git rev-parse` is not on the watch list, only the outer `git diff`."""
    invocations = tokenize_command(
        "cd $(git rev-parse --show-toplevel) && git diff main..HEAD"
    )

    decision = decide_redirect(invocations)

    assert decision.action == "advisory"
    assert "get_change_manifest" in decision.advice
    assert "rev-parse" not in decision.advice


def test_decide_redirect_preserves_var_reference_in_advice() -> None:
    """The advice text should quote the literal `$BASE`, never an expansion."""
    invocations = tokenize_command("git diff $BASE..HEAD")

    decision = decide_redirect(invocations)

    assert "$BASE..HEAD" in decision.advice


# ---------------------------------------------------------------------------
# CLI entry point — `python3 -m bash_redirect_hook` reads a Claude Code
# PreToolUse JSON payload on stdin and writes the tokenized invocation
# list as JSON on stdout. The bundled shell script (#239) parses that.
# ---------------------------------------------------------------------------


def _run_module(stdin: str) -> tuple[int, str, str]:
    """Run `python3 -m bash_redirect_hook` with `stdin` and capture output."""
    proc = subprocess.run(
        [sys.executable, "-m", "bash_redirect_hook"],
        input=stdin,
        capture_output=True,
        text=True,
        cwd=str(HOOKS_DIR),
    )
    return proc.returncode, proc.stdout, proc.stderr


def test_module_emits_json_invocations_for_recognized_command() -> None:
    """`python3 -m bash_redirect_hook` consumes the Claude Code payload."""
    payload = json.dumps(
        {
            "tool_name": "Bash",
            "tool_input": {"command": "git diff main..HEAD"},
            "hook_event_name": "PreToolUse",
        }
    )

    code, stdout, _ = _run_module(payload)

    assert code == 0
    parsed = json.loads(stdout)
    assert parsed == [
        {
            "subcommand": "diff",
            "args": ["main..HEAD"],
            "position": 0,
            "raw": "git diff main..HEAD",
        }
    ]


def test_module_emits_empty_list_for_no_git_command() -> None:
    payload = json.dumps(
        {"tool_name": "Bash", "tool_input": {"command": "ls -la"}}
    )

    code, stdout, _ = _run_module(payload)

    assert code == 0
    assert json.loads(stdout) == []


def test_module_handles_empty_stdin_as_empty_list() -> None:
    code, stdout, _ = _run_module("")

    assert code == 0
    assert json.loads(stdout) == []
