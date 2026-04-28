"""Bash command tokenizer + redirect decision logic.

Part of the bundled redirect hook (Epic #234, ADR 0008). The shell hook
script (`git-prism-redirect.sh`, shipped by #239) reads a Claude Code
PreToolUse JSON payload on stdin and shells out to this module via
`python3 -m bash_redirect_hook`. We expose two public functions:

- `tokenize_command(s: str) -> list[Invocation]` — return a structured
  list of `git` invocations found in the bash command string.
- `decide_redirect(invocations: list[Invocation]) -> RedirectDecision` —
  classify the invocations as allow / advisory / block.

Stdlib only (`shlex`, `dataclasses`). No third-party deps.
"""

from __future__ import annotations

import json
import shlex
import sys
from dataclasses import asdict, dataclass, field

# Tokens treated as command boundaries when walking the flat shlex
# output. Hitting any of these starts a new candidate command.
_COMMAND_BOUNDARIES = frozenset({"&&", "||", "|", ";", "(", ")"})

# Sentinel token inserted at every newline position before lexing. It
# carries the same semantics as a top-level command separator AND is
# what the heredoc body-skipper looks for to recognize the closing
# tag's "start of a logical line" position.
_NEWLINE_TOKEN = "\x00NL\x00"

# Map each watch-list git subcommand to the git-prism MCP tool that
# replaces it. Anything not in this map is allowed without advice
# (write-side commands, plumbing, etc).
_REDIRECT_TARGETS = {
    "diff": "get_change_manifest",
    "log": "get_commit_history",
    "show": "get_file_snapshots",
    "blame": "get_file_snapshots",
}


@dataclass(frozen=True)
class RedirectDecision:
    """Result of classifying a list of `git` invocations.

    Attributes:
        action: `"allow"` (no redirect, command runs unaltered),
            `"advisory"` (command runs but the agent gets a nudge to
            use the MCP tool instead), or `"block"` (hard refusal —
            reserved for `gh pr diff` / `mcp__github__*` shapes that
            the bundled hook script handles outside this module).
        advice: User-facing text describing the recommended MCP tool.
            Empty for `"allow"` decisions.
    """

    action: str
    advice: str


@dataclass(frozen=True)
class Invocation:
    """A single `git` command extracted from a bash string.

    Attributes:
        subcommand: The git subcommand (e.g. `"diff"`, `"log"`).
        args: All tokens following the subcommand, with shell quoting
            already removed by `shlex` but variable references (e.g.
            `$BASE`) preserved as literal text.
        position: Character offset in the original input where the
            invocation starts. Used by the hook script when reporting
            advice that quotes the original command.
        raw: The original substring that produced this invocation,
            useful for verbatim quoting in advisory messages.
    """

    subcommand: str
    args: list[str] = field(default_factory=list)
    position: int = 0
    raw: str = ""


def tokenize_command(command: str) -> list[Invocation]:
    """Extract every `git` invocation from a bash command string.

    Walks the input with `shlex` configured to recognize bash
    punctuation (`&&`, `||`, `|`, `;`, `(`, `)`). Backticks are
    normalized to whitespace before lexing because they delimit command
    substitution identically to `$(...)` for our purposes. Newlines
    are pre-replaced with a sentinel token so heredoc termination ("tag
    appears at the start of a line") survives `shlex`'s default
    whitespace handling.

    Returns invocations in left-to-right order.
    """
    if not command:
        return []

    normalized = _normalize_substitution_chars(command)
    tokens = _lex_tokens(normalized)
    if not tokens:
        return []

    tokens = _strip_heredoc_bodies(tokens)
    return _collect_git_invocations(tokens, command)


def _normalize_substitution_chars(command: str) -> str:
    """Pre-process the command for `shlex`.

    - Backticks become a `$(` substitution boundary so the inner
      command starts a fresh segment, matching `$(...)` semantics from
      ADR Decision 1. Without this, the inner `git rev-parse` would
      become a sibling argument of the outer `cd` segment and be
      suppressed (correctly, per ADR), but the closing backtick would
      collapse the boundary and the outer `git diff` would fuse into
      the `cd` segment too.
    - Newlines become a sentinel token so heredoc termination ("tag
      appears at the start of a line") survives `shlex`'s default
      whitespace handling.
    """
    return (
        command.replace("`", " $( ")
        .replace("\n", f" {_NEWLINE_TOKEN} ")
    )


def _lex_tokens(command: str) -> list[str]:
    """Tokenize `command` into bash-aware tokens."""
    lex = shlex.shlex(command, posix=True, punctuation_chars=True)
    lex.whitespace_split = True
    return list(lex)


def _strip_heredoc_bodies(tokens: list[str]) -> list[str]:
    """Remove tokens inside heredoc bodies.

    A `<< TAG` (or `<<- TAG`, `<< "TAG"`, `<< 'TAG'`) opens a heredoc
    whose body ends when the tag appears as a standalone token
    immediately after a newline sentinel. With newlines preserved as
    sentinel tokens, the closing tag is the sequence
    `<sentinel>, <TAG>` followed by another sentinel or end-of-stream.
    """
    output: list[str] = []
    i = 0
    while i < len(tokens):
        token = tokens[i]
        if token == "<<":
            tag = _heredoc_tag_at(tokens, i + 1)
            if tag is None:
                output.append(token)
                i += 1
                continue
            i = _skip_heredoc_body(tokens, i + 2, tag)
            continue
        output.append(token)
        i += 1
    return output


def _heredoc_tag_at(tokens: list[str], index: int) -> str | None:
    """Return the heredoc tag at `index`, stripping `-` and quotes.

    Bash accepts `<<EOF`, `<<-EOF`, `<<"EOF"`, and `<<'EOF'`. shlex
    splits the operator from the tag (after our newline normalization,
    `<<EOF` lexes as `['<<', 'EOF']`), so we just grab the next token.
    """
    if index >= len(tokens):
        return None
    candidate = tokens[index]
    if candidate.startswith("-"):
        candidate = candidate[1:]
    return candidate.strip("'\"") or None


def _skip_heredoc_body(
    tokens: list[str], start: int, tag: str
) -> int:
    """Advance past the heredoc body, returning the index after the tag.

    The closing tag must appear immediately after a newline sentinel
    (mirroring bash's "start of a logical line" rule).
    """
    seen_newline = False
    i = start
    while i < len(tokens):
        token = tokens[i]
        if token == _NEWLINE_TOKEN:
            seen_newline = True
            i += 1
            continue
        if seen_newline and token == tag:
            return i + 1
        seen_newline = False
        i += 1
    return i


def _collect_git_invocations(
    tokens: list[str], original: str
) -> list[Invocation]:
    """Walk `tokens` left-to-right and emit every `git`-headed segment.

    The walker splits at every command boundary (`&&`, `||`, `|`, `;`,
    `(`, `)`, newline sentinel) and at any token starting with `$(`
    (handles `$(...)` substitution boundaries). When the head of a
    segment is `git`, we record an `Invocation`.
    """
    invocations: list[Invocation] = []
    segment: list[str] = []
    cursor = 0

    def flush() -> None:
        nonlocal cursor
        invocation = _build_invocation(segment, original, cursor)
        if invocation is not None:
            cursor = invocation.position + len(invocation.raw)
            invocations.append(invocation)

    for token in tokens:
        if _is_segment_terminator(token):
            flush()
            segment = []
            continue
        segment.append(token)

    flush()
    return invocations


def _is_segment_terminator(token: str) -> bool:
    """Return True if `token` ends the current candidate command.

    `$` is treated as a boundary because `shlex` with
    `punctuation_chars=True` splits `$(` into two tokens (`$` then
    `(`); the bare `$` would otherwise leak into the previous segment
    as an argument.
    """
    if token in _COMMAND_BOUNDARIES:
        return True
    if token == _NEWLINE_TOKEN:
        return True
    return token == "$" or token.startswith("$(")


def _build_invocation(
    segment: list[str], original: str, search_from: int
) -> Invocation | None:
    """Construct an `Invocation` from a `git`-headed segment.

    Returns `None` when the segment is empty or doesn't begin with the
    bare word `git`. A subcommand-less `git` (segment of length 1) is
    still returned with empty args — `git status` shapes need that.
    """
    if not segment or segment[0] != "git":
        return None
    subcommand = segment[1] if len(segment) > 1 else ""
    args = segment[2:]
    position = original.find("git", search_from)
    if position == -1:
        position = search_from
    raw = " ".join(segment)
    return Invocation(
        subcommand=subcommand, args=args, position=position, raw=raw
    )


def decide_redirect(invocations: list[Invocation]) -> RedirectDecision:
    """Classify a list of invocations as allow / advisory / block.

    Walks the invocations in order and emits advisory text for every
    watch-list subcommand (`diff`, `log`, `show`, `blame`). Returns
    `"allow"` when nothing on the list matches.

    Hard-block targets (`gh pr diff`, `mcp__github__*`) live OUTSIDE
    the tokenizer because they don't start with `git`; the bundled
    hook script (#239) detects those at the JSON-payload level before
    it ever calls this module.
    """
    advisory_lines: list[str] = [
        line
        for inv in invocations
        if (line := _redirect_line_for(inv)) is not None
    ]
    if not advisory_lines:
        return RedirectDecision(action="allow", advice="")
    return RedirectDecision(
        action="advisory", advice="\n".join(advisory_lines)
    )


def _redirect_line_for(invocation: Invocation) -> str | None:
    """Format the advisory message for a single watch-list invocation."""
    tool = _REDIRECT_TARGETS.get(invocation.subcommand)
    if tool is None:
        return None
    return (
        f"git-prism: prefer `{tool}` over `{invocation.raw}` "
        f"for structured output."
    )


def _command_from_payload(raw_stdin: str) -> str:
    """Extract `tool_input.command` from a Claude Code PreToolUse payload.

    Returns an empty string for empty stdin, payloads with no `Bash`
    tool_name, or malformed JSON. The bundled hook script is the
    authoritative parser for malformed-payload diagnostics; this
    module just emits an empty invocation list and exits 0.
    """
    if not raw_stdin.strip():
        return ""
    try:
        payload = json.loads(raw_stdin)
    except json.JSONDecodeError:
        return ""
    if not isinstance(payload, dict):
        return ""
    tool_input = payload.get("tool_input")
    if not isinstance(tool_input, dict):
        return ""
    command = tool_input.get("command")
    return command if isinstance(command, str) else ""


def _emit_invocations_json(command: str) -> str:
    """Tokenize `command` and serialize the invocation list to JSON."""
    return json.dumps([asdict(inv) for inv in tokenize_command(command)])


def _main() -> int:
    raw = sys.stdin.read()
    command = _command_from_payload(raw)
    sys.stdout.write(_emit_invocations_json(command))
    return 0


if __name__ == "__main__":
    raise SystemExit(_main())
