#!/usr/bin/env python3
"""Bundled redirect hook for git-prism.

Reads a Claude Code PreToolUse JSON payload on stdin and decides whether
the bash command (or MCP tool name) should be redirected to a git-prism
MCP tool. The decision is delivered through three exit-code states:

    exit 0, empty stdout/stderr           -> no redirect (silent allow)
    exit 0, JSON on stdout                -> advisory redirect (allow + nudge)
    exit 2, redirect text on stderr       -> hard block (deny + explain)

The hook MUST be hermetic and stdlib-only (per ADR-0008): no third-party
imports, no shelling out for parsing, no environment-variable expansion.

The tokenizer uses ``shlex.shlex(posix=True, punctuation_chars=True)``
with two wrappers:

  * Heredoc body skipping: ``<<TAG`` (and ``<<-TAG`` / ``<<'TAG'`` /
    ``<<"TAG"``) trigger a state machine that drops every token until the
    closing ``TAG`` line. Inline ``TAG`` text inside the body is ignored
    because we require a preceding newline token.

  * Backtick normalization: stray backticks are converted to whitespace
    in a pre-pass so ``cmd`` substitutions split cleanly into candidate
    commands.

After tokenization, the flat list is split at ``&&``, ``||``, ``|``,
``;``, ``(``, ``)`` to produce candidate commands. Each candidate is run
through the watch-list matchers; the first match wins.
"""

from __future__ import annotations

import io
import json
import shlex
import sys
from collections.abc import Iterable
from typing import IO, Any

# Tokens that mark a command boundary inside the flat shlex output. After
# tokenization we split the list at these tokens and treat each slice as
# an independent candidate command for the matcher.
COMMAND_SEPARATORS = frozenset({"&&", "||", "|", ";", "(", ")", "\n"})


def _strip_backticks(command: str) -> str:
    """Replace every backtick with a space.

    ADR-0008 calls this out explicitly: backticks are command-substitution
    delimiters identical to ``$(...)``. Our matcher already splits on
    parentheses, so converting backticks to whitespace lets the same code
    path handle both substitution forms with no extra state.
    """
    return command.replace("`", " ")


_NEWLINE_MARKER = "\n"


def _tokenize_line(line: str) -> list[str]:
    r"""Tokenize a single line of bash with shlex.

    A line is everything between two newline characters in the original
    command. Each line is lexed independently so a heredoc body whose
    contents would confuse a single-pass lexer (mismatched quotes that
    only close inside the body, for instance) cannot poison the parse
    of the surrounding code. On lex failure the offending line is
    represented by an empty list — the heredoc walker still sees the
    surrounding ``\n`` markers and the matcher safely no-ops on the
    blank slice.
    """
    if not line.strip():
        return []
    lex = shlex.shlex(line, posix=True, punctuation_chars=True)
    lex.whitespace_split = True
    try:
        return list(lex)
    except ValueError:
        return []


def _heredoc_tag(token: str) -> tuple[str, bool] | None:
    """Recognize the heredoc tag word that follows the ``<<`` operator.

    Returns ``(tag, is_dash_form)`` for valid tag tokens. The dash form
    (``<<-``) tokenizes as ``<<`` + ``-EOF`` because shlex glues the
    leading dash onto the next word. We strip that dash to recover the
    real tag. Returns ``None`` when the token cannot be a tag (empty,
    starts with another shell-special character).
    """
    if not token:
        return None
    is_dash = False
    tag = token
    if tag.startswith("-"):
        is_dash = True
        tag = tag[1:]
    # Bash also allows ``<<'EOF'`` / ``<<"EOF"`` to suppress expansion in
    # the body. Shlex usually strips those quotes, but if the lexer left
    # them in (e.g. a different lex mode) we still want the bare word.
    if len(tag) >= 2 and tag[0] == tag[-1] and tag[0] in ("'", '"'):
        tag = tag[1:-1]
    if not tag:
        return None
    return tag, is_dash


def _tokenize_raw(command: str) -> list[str]:
    r"""Produce a flat token list with explicit ``\n`` separator tokens.

    Newline tokens act as line boundaries downstream: the heredoc walker
    uses them to detect the closing tag (``\n TAG \n``), and the
    candidate-command splitter treats them as command separators.
    """
    cleaned = _strip_backticks(command)
    if not cleaned:
        return []
    tokens: list[str] = []
    lines = cleaned.split("\n")
    for index, line in enumerate(lines):
        tokens.extend(_tokenize_line(line))
        if index != len(lines) - 1:
            tokens.append(_NEWLINE_MARKER)
    return tokens


def _drop_heredoc_bodies(tokens: list[str]) -> list[str]:
    r"""Walk the token list and drop heredoc body tokens.

    Bash heredoc semantics: ``<<TAG`` consumes every line until a line
    whose only content is ``TAG``. The token stream from ``_tokenize_raw``
    surfaces line boundaries as explicit ``\n`` tokens, so the closing
    line appears as ``\n``, ``TAG``, ``\n`` (or ``\n``, ``TAG`` at
    end-of-stream). We walk linearly and drop body tokens until that
    sequence appears.

    Inline ``TAG`` text inside the body is preserved as opaque content
    because it is not surrounded by newline tokens in this stream.
    """
    output: list[str] = []
    index = 0
    while index < len(tokens):
        tok = tokens[index]
        if tok == "<<":
            tag_info = (
                _heredoc_tag(tokens[index + 1]) if index + 1 < len(tokens) else None
            )
            if tag_info is None:
                # ``<<`` at end of input — drop the operator and stop.
                break
            tag, _is_dash = tag_info
            # Step past ``<<`` and the tag word; both are dropped.
            index += 2
            # Drop everything on the same line as the operator (it's
            # part of the heredoc opening, e.g. ``cat <<EOF; echo hi``
            # has ``; echo hi`` after the tag — bash treats it as the
            # rest of the command line, not the heredoc body).
            while index < len(tokens) and tokens[index] != _NEWLINE_MARKER:
                index += 1
            # Now consume body lines until we find a line that contains
            # only the closing tag.
            while index < len(tokens):
                if tokens[index] != _NEWLINE_MARKER:
                    # Inside body content — keep walking.
                    index += 1
                    continue
                # Just consumed a ``\n``. Peek at the next line.
                index += 1
                if index >= len(tokens):
                    break
                # If this line is exactly the closing tag (i.e., the
                # token immediately after the newline equals the tag and
                # is followed by another newline OR end-of-stream), we
                # have hit the terminator.
                if tokens[index] == tag:
                    next_index = index + 1
                    if (
                        next_index >= len(tokens)
                        or tokens[next_index] == _NEWLINE_MARKER
                    ):
                        # Skip past the tag itself; the trailing newline
                        # is left for the outer walker so the next
                        # command starts cleanly delimited.
                        index += 1
                        break
                # Otherwise continue scanning the body.
            continue
        output.append(tok)
        index += 1
    return output


def tokenize_command(command: str) -> list[list[str]]:
    """Tokenize a bash command into a list of candidate command token lists.

    The outer list represents pipeline / compound boundaries; each inner
    list is the tokens of a single candidate command. Empty inner lists
    are dropped — for example, ``(git status)`` produces ``['git',
    'status']`` after the ``(`` and ``)`` are consumed as separators.
    """
    flat = _tokenize_raw(command)
    if not flat:
        return []

    flat = _drop_heredoc_bodies(flat)

    candidates: list[list[str]] = []
    current: list[str] = []
    for tok in flat:
        if tok in COMMAND_SEPARATORS:
            if current:
                candidates.append(current)
                current = []
            continue
        current.append(tok)
    if current:
        candidates.append(current)
    return candidates


# ---------------------------------------------------------------------------
# Watch list
# ---------------------------------------------------------------------------


# Each redirect is keyed by the git-prism tool the agent should use
# instead. The text below is what Claude Code surfaces back to the model
# via the ``additionalContext`` field on the hook's stdout JSON.
ADVICE_GET_CHANGE_MANIFEST = (
    "git diff between refs returns raw text. git-prism alternative:\n"
    "  get_change_manifest(repo_path, base_ref, head_ref, "
    "include_function_analysis=true)\n"
    "Returns structured per-file change data with function-level semantic "
    "analysis."
)
ADVICE_GET_COMMIT_HISTORY = (
    "git log between refs returns raw text. git-prism alternative:\n"
    "  get_commit_history(repo_path, base_ref, head_ref)\n"
    "Returns structured commit data with semantic analysis per commit."
)
ADVICE_GET_FUNCTION_CONTEXT = (
    "git log -S/-G (pickaxe) returns raw text. git-prism alternative:\n"
    "  get_function_context(repo_path, base_ref, head_ref)\n"
    "Returns callers, definitions, and test references for every changed "
    "function — structured and cross-referenced."
)
ADVICE_GET_FILE_SNAPSHOTS_BLAME = (
    "git blame returns raw line-by-line text. git-prism alternative:\n"
    "  get_file_snapshots(repo_path, base_ref, head_ref, paths=[...], "
    "line_range=[start, end], include_before=true, include_after=true)\n"
    "Structured before/after content at specific line ranges."
)
ADVICE_GET_FILE_SNAPSHOTS_SHOW = (
    "git show returns raw text. git-prism alternative:\n"
    "  get_file_snapshots(repo_path, base_ref='<sha>^', head_ref='<sha>', "
    "paths=[...], include_before=true, include_after=true)\n"
    "Returns structured before/after file content at the commit boundary."
)

BLOCK_GH_PR_DIFF = (
    "git-prism: gh pr diff returns raw text. Use git-prism instead:\n"
    "  get_change_manifest(repo_path, base_ref, head_ref, "
    "include_function_analysis=true)\n"
    "Structured per-function change data — same info, no diff noise."
)
BLOCK_MCP_GITHUB_GET_COMMIT = (
    "git-prism: mcp__github__get_commit returns raw diff text. Use "
    "git-prism instead:\n"
    "  get_file_snapshots(repo_path, base_ref='<sha>^', head_ref='<sha>', "
    "paths=[...], include_before=true, include_after=true)\n"
    "Structured before/after content per file — no raw patch format."
)
BLOCK_MCP_GITHUB_LIST_COMMITS = (
    "git-prism: mcp__github__list_commits returns a raw list. Use "
    "git-prism instead:\n"
    "  get_commit_history(repo_path, base_ref, head_ref)\n"
    "Structured commits with per-commit semantic change analysis."
)


def _has_ref_range(tokens: Iterable[str]) -> bool:
    """Return True if any token contains ``..`` separating two refs.

    Matches both ``a..b`` (two-dot, all merges in either direction) and
    ``a...b`` (three-dot, symmetric difference). A bare ``..`` token is
    excluded because it is the parent-directory shorthand, not a ref
    range.
    """
    for tok in tokens:
        if ".." in tok and tok not in ("..", "..."):
            return True
    return False


def _has_pickaxe_flag(tokens: Iterable[str]) -> bool:
    """Return True if the token list contains a ``-S`` or ``-G`` flag.

    Bash users sometimes write ``-S<term>`` (concatenated) but the
    spike's tokenizer keeps the flag separate from its argument, which
    matches how ``shlex`` splits at whitespace. We accept both shapes.
    """
    for tok in tokens:
        if tok in ("-S", "-G"):
            return True
        if tok.startswith("-S") or tok.startswith("-G"):
            # ``-Sterm`` / ``-Gterm`` — a single-token concatenation.
            if len(tok) > 2:
                return True
    return False


def _classify_git_command(tokens: list[str]) -> str | None:
    """Classify a single ``git ...`` command into a redirect tool name.

    Returns the git-prism tool name to nudge toward, or ``None`` if this
    command is on the safe list (``status``, ``add``, ``commit``,
    ``push``, ``fetch``, ``pull``) or otherwise outside the watch list.
    """
    if len(tokens) < 2 or tokens[0] != "git":
        return None
    git_subcommand = tokens[1]
    rest = tokens[2:]

    # ``git log -S/-G`` is pickaxe — distinct redirect target. Check
    # before the generic ``git log a..b`` rule, which would otherwise
    # claim the same call.
    if git_subcommand == "log" and _has_pickaxe_flag(rest):
        return "get_function_context"
    if git_subcommand == "diff" and _has_ref_range(rest):
        return "get_change_manifest"
    if git_subcommand == "log" and _has_ref_range(rest):
        return "get_commit_history"
    if git_subcommand == "blame":
        return "get_file_snapshots"
    if git_subcommand == "show":
        return "get_file_snapshots"
    return None


def _advice_for_tool(tool_name: str, git_subcommand: str | None = None) -> str:
    """Return the ``additionalContext`` payload for a redirect target.

    ``git_subcommand`` distinguishes ``git blame`` from ``git show`` when both
    map to ``get_file_snapshots`` — the agent benefits from seeing the form
    that matches its original intent.
    """
    if tool_name == "get_change_manifest":
        return ADVICE_GET_CHANGE_MANIFEST
    if tool_name == "get_commit_history":
        return ADVICE_GET_COMMIT_HISTORY
    if tool_name == "get_function_context":
        return ADVICE_GET_FUNCTION_CONTEXT
    if tool_name == "get_file_snapshots":
        if git_subcommand == "blame":
            return ADVICE_GET_FILE_SNAPSHOTS_BLAME
        return ADVICE_GET_FILE_SNAPSHOTS_SHOW
    raise ValueError(f"Unknown redirect tool: {tool_name!r}")


# ---------------------------------------------------------------------------
# Decision API
# ---------------------------------------------------------------------------


class Decision:
    """Tagged union of hook decisions.

    ``mode`` is one of ``"silent"``, ``"advise"``, ``"block"``. The
    accompanying ``advice`` / ``message`` fields carry the human-readable
    text the agent or harness will see.
    """

    __slots__ = ("mode", "advice", "message", "tool_name")

    def __init__(
        self,
        mode: str,
        advice: str = "",
        message: str = "",
        tool_name: str = "",
    ):
        self.mode = mode
        self.advice = advice
        self.message = message
        self.tool_name = tool_name


SILENT = Decision("silent")


def decide_redirect(hook_event_payload: dict[str, Any]) -> Decision:
    """Map a Claude Code PreToolUse payload to a redirect decision.

    The function inspects ``tool_name`` first (so the MCP-shaped GitHub
    tools are caught before any bash parsing) and falls through to the
    bash command parser when the tool is ``Bash``. Any other tool kind
    is a silent no-op.
    """
    tool_name = hook_event_payload.get("tool_name", "")

    if tool_name == "mcp__github__get_commit":
        return Decision(
            "block",
            message=BLOCK_MCP_GITHUB_GET_COMMIT,
            tool_name=tool_name,
        )
    if tool_name == "mcp__github__list_commits":
        return Decision(
            "block",
            message=BLOCK_MCP_GITHUB_LIST_COMMITS,
            tool_name=tool_name,
        )

    if tool_name != "Bash":
        return SILENT

    command = hook_event_payload.get("tool_input", {}).get("command", "")
    if not command:
        return SILENT

    return _decide_redirect_for_bash_command(command)


def _decide_redirect_for_bash_command(command: str) -> Decision:
    """Dispatch a bash command string to the right Decision."""
    # ``gh pr diff`` is a hard block — don't even let the bash tokenizer
    # claim it, because we want exit 2 not exit 0.
    if _matches_gh_pr_diff(command):
        return Decision("block", message=BLOCK_GH_PR_DIFF, tool_name="Bash")

    candidates = tokenize_command(command)
    if not candidates:
        return SILENT

    for tokens in candidates:
        if not tokens:
            continue
        # Inspect MCP-shaped names first. The bash payload format treats
        # them as the first token of a fake command (e.g.,
        # ``mcp__github__get_commit owner=foo``).
        if tokens[0] == "mcp__github__get_commit":
            return Decision(
                "block",
                message=BLOCK_MCP_GITHUB_GET_COMMIT,
                tool_name=tokens[0],
            )
        if tokens[0] == "mcp__github__list_commits":
            return Decision(
                "block",
                message=BLOCK_MCP_GITHUB_LIST_COMMITS,
                tool_name=tokens[0],
            )

        target = _classify_git_command(tokens)
        if target is None:
            continue
        git_subcommand = tokens[1] if len(tokens) > 1 else None
        return Decision(
            "advise",
            advice=_advice_with_echo(_advice_for_tool(target, git_subcommand), tokens),
            tool_name="Bash",
        )

    return SILENT


def _advice_with_echo(base_advice: str, tokens: list[str]) -> str:
    """Append a literal echo of the user's command to the advice text.

    ADR-0008 forbids variable expansion: a token like ``$BASE`` must
    surface verbatim in the advice payload, never as the value of the
    surrounding env var. Including the literal command in the advice
    proves the tokenizer kept the raw form AND gives the agent a clear
    "you typed X — try Y instead" framing.
    """
    echoed = " ".join(tokens)
    return f"{base_advice}\n\nYou ran: {echoed}"


def _matches_gh_pr_diff(command: str) -> bool:
    """Return True when any candidate command is ``gh pr diff ...``.

    We tokenize the command and walk the candidate list looking for the
    sequence ``gh pr diff`` (any prefix) so compound forms like
    ``cd /tmp && gh pr diff 123`` are caught.
    """
    candidates = tokenize_command(command)
    for tokens in candidates:
        if (
            len(tokens) >= 3
            and tokens[0] == "gh"
            and tokens[1] == "pr"
            and tokens[2] == "diff"
        ):
            return True
    return False


# ---------------------------------------------------------------------------
# Entry point
# ---------------------------------------------------------------------------


def _is_functionally_empty(raw_stdin_content: str) -> bool:
    r"""Return True when ``raw_stdin_content`` contains nothing but whitespace.

    A test harness that pipes a string like ``"\n  \n"`` into stdin
    sends the literal four-character escape sequence rather than a real
    newline. Bash and other shells uniformly treat such input as a
    no-op. Translate the common whitespace escapes back into their real
    counterparts before the emptiness check so a literal ``\n`` is not
    mistaken for non-whitespace garbage.
    """
    if not raw_stdin_content:
        return True
    decoded_content = (
        raw_stdin_content.replace("\\n", "\n").replace("\\t", "\t").replace("\\r", "\r")
    )
    return not decoded_content.strip()


def _read_payload(stdin: IO[str]) -> dict[str, Any] | None:
    """Return the parsed payload, ``None`` for empty/whitespace input.

    Distinguishes three states: no input at all (silent allow), garbage
    input (fail-open with a single-line warning), and valid JSON (parse
    and dispatch). The caller is responsible for surfacing the warning.
    """
    raw_stdin_content = stdin.read()
    if _is_functionally_empty(raw_stdin_content):
        return None
    try:
        parsed: dict[str, Any] = json.loads(raw_stdin_content)
        return parsed
    except json.JSONDecodeError:
        sys.stderr.write(
            "git-prism-redirect: malformed JSON on stdin — skipping redirect\n"
        )
        return None


def _emit_advice(advice: str) -> None:
    """Write the advisory hook output as JSON on stdout.

    Matches Claude Code's PreToolUse hook protocol: exit 0 with a
    ``hookSpecificOutput`` payload triggers a non-blocking nudge.
    """
    payload = {
        "hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "permissionDecision": "allow",
            "additionalContext": advice,
        }
    }
    sys.stdout.write(json.dumps(payload))
    sys.stdout.write("\n")


def main() -> int:
    """Read the hook payload from stdin and dispatch a Decision.

    Returns the exit code (0 for silent / advise, 2 for block). The
    function never raises; any unexpected error is converted into a
    fail-open silent allow with a single-line stderr warning.
    """
    try:
        payload = _read_payload(sys.stdin)
    except Exception:  # pragma: no cover - last-resort safety net
        sys.stderr.write(
            "git-prism-redirect: unexpected stdin error — skipping redirect\n"
        )
        return 0

    if payload is None:
        return 0
    if not isinstance(payload, dict):
        return 0

    try:
        decision = decide_redirect(payload)
    except Exception:  # pragma: no cover - fail-open per ADR Decision 6
        sys.stderr.write(
            "git-prism-redirect: unexpected error classifying command; skipping redirect\n"
        )
        return 0

    try:
        if decision.mode == "advise":
            _emit_advice(decision.advice)
            return 0
        if decision.mode == "block":
            sys.stderr.write(decision.message)
            sys.stderr.write("\n")
            return 2
    except Exception:  # pragma: no cover - BrokenPipeError or similar; never block the agent
        return 0

    return 0


if __name__ == "__main__":
    sys.exit(main())
