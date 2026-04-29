<!-- Capstone narration for git-prism v0.7.0 redirect-hook epic (issue #242).
     Segment timings are estimates against the rendered demo cast. Format
     mirrors demo/narration.md. -->

## Segment 1: Before — the problem (estimated 0:30)

Bash redirections in agentic sessions can silently overwrite tracked files. Here's a notes file under version control with research I want to keep. An agent runs `echo "overwritten content" > notes.md` and the original is gone. No diff, no prompt, nothing for review to catch.

## Segment 2: How the hook works (estimated 0:30)

git-prism v0.7.0 ships a Claude Code PreToolUse hook. It reads each Bash tool call as JSON on stdin and answers with an exit code. Two blocks. Zero with a JSON body lets it through with a redirect nudge. Zero by itself is silent. So `gh pr diff` blocks, `git diff main..HEAD` nudges toward `get_change_manifest`, and `echo hello world` passes through.

Parsing is Python's `shlex`, so compound commands, subshells, pipelines, and variable expansion all tokenize structurally instead of by regex.

`git-prism hooks install --scope user` copies the hook scripts into `~/.claude/hooks/` and writes a PreToolUse entry into Claude Code's `~/.claude/settings.json`. User scope is the default because Claude Code issue 13898 keeps subagents from calling project-scoped MCP servers correctly.

## Segment 3: review_change vs git diff (estimated 0:45)

`git diff HEAD~1..HEAD` returns hunk headers, plus-and-minus prefixes, and whitespace context. It was built for humans reading patches, and agents pay for every line of it.

`git-prism manifest` returns the same change as structured JSON: per-file change types, line counts, language, function-level diffs, import deltas, dependency updates. `git-prism context` adds callers, callees, test references, and a risk score per changed function. The MCP tool `review_change` runs both in one call and replaces `git diff <ref>..<ref>` for PR review and refactor audits.

Install via `brew install git-prism` or `cargo install git-prism`. Source at github.com/mikelane/git-prism.
