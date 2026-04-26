# ADR 0008: Redirect Hook Architecture

- **Status**: Accepted
- **Date**: 2026-04-26
- **Context**: Spike for Epic #234 — bundled redirect hooks for git-prism

## Context

Agents reach for raw `git diff/log/show/blame` instead of git-prism MCP tools because of pretraining bias. The current local hook (`~/.claude/hooks/git-prism-redirect.sh`) backstops this with regex pattern matching against `tool_input.command`, but the regex approach misses common shapes:

- compound commands: `cd /tmp && git diff main..HEAD`
- subshells: `(git log main..HEAD)`
- variable expansion: `git diff $BASE..HEAD`
- pipelines: `git diff main..HEAD | grep foo`
- heredocs that quote git commands inside the body
- command substitution: `cd $(git rev-parse --show-toplevel) && git diff ...`

The hook also lives in user dotfiles, so installing git-prism gets the binary but none of the redirect leverage. Epic #234 ships a `git-prism install-hooks` subcommand and a bundled hook that calls a stdlib-only Python tokenizer to parse bash structurally.

This ADR records the architectural decisions for that work. No production code lands from this spike — only this file.

## Decision

### 1. Bash command parser: Python stdlib `shlex.shlex` with `punctuation_chars=True`

Use `shlex.shlex(posix=True, punctuation_chars=True, whitespace_split=True)` as the primary tokenizer, with two small wrappers around it. No third-party parser dependency.

**What `shlex` handles natively** (verified against representative inputs in this spike's prototype):

- compound operators `&&`, `||`, `|`, `;` are emitted as standalone tokens (with `punctuation_chars=True`)
- subshell parens `(`, `)` are standalone tokens
- variable forms `$X`, `$X..HEAD`, `${X}` are preserved as literal tokens (we do not evaluate)
- `$(...)` decomposes into `$`, `(`, ..., `)` tokens — usable as command-substitution delimiters
- single- and double-quoted strings are unwrapped to their content
- escaped whitespace (`git\ diff`) joins tokens correctly
- newlines collapse to whitespace, so multiline bash works

**What we wrap around `shlex`:**

1. **Heredoc body skipping.** `shlex` emits `<<EOF`, then every body word as separate tokens, then the closing `EOF`. A `git diff a..b` literally appearing inside a heredoc body would be a false positive. Solve by walking the token stream once: when we see `<<TAG` (or `<<-TAG`, `<<"TAG"`, `<<'TAG'`), drop tokens until we re-encounter `TAG` at the start of a logical line.

2. **Backtick stripping.** Backticks (`` `cmd` ``) attach to adjacent tokens (`` `git `` → single token). Replace stray backticks with whitespace before feeding the input to `shlex`. This treats backticks as command-substitution delimiters identically to `$(...)`.

After tokenization, walk the flat token list left-to-right, splitting at any of `&&`, `||`, `|`, `;`, `(`, `)`, `$`, `<<`. Each resulting sub-list is a candidate command. Apply the redirect-pattern matchers (e.g., "starts with `git diff` and contains `..`") against each sub-list independently.

`bashlex` and a handwritten parser are both rejected; details under Alternatives Considered.

### 2. `--scope` semantics: mirror `claude mcp add --scope` exactly

`git-prism install-hooks --scope <local|user|project>` matches the three scopes Claude Code's own CLI uses (`claude mcp add --help` confirmed). Default is `user`.

**Why `user` is the default (not `local` like `claude mcp add`):**

Local scope writes to `<repo>/.claude/settings.local.json`, which is per-checkout and gitignored. Most users want the redirect hook to apply across every git-prism-aware repo on the machine; user scope (`~/.claude/settings.json`) gives that with one install. If a user wants per-repo override they pass `--scope project` (committed) or `--scope local` (gitignored). See decision 5 below for the second reason user-scope wins.

**Paths each scope writes to:**

| Scope | Settings file | Hook script location | Committed? |
|---|---|---|---|
| `user` (default) | `~/.claude/settings.json` | `~/.claude/hooks/git-prism-redirect.sh` (copied) | n/a — outside repo |
| `project` | `<repo>/.claude/settings.json` | `<repo>/.claude/hooks/git-prism-redirect.sh` (copied) | yes |
| `local` | `<repo>/.claude/settings.local.json` | `<repo>/.claude/hooks/git-prism-redirect.sh` (copied) | no (gitignored) |

The Python tokenizer (`parse_git_invocations.py`) is copied alongside the shell script in each case. This is intentional duplication — keeps each scope self-contained and avoids cross-scope path resolution at hook execution time.

**Precedence when entries exist in multiple scopes:**

Claude Code merges hook entries from all three scopes and runs them all. There is no "scope wins over scope" — they additively concatenate. If a user installs at `user` scope and again at `project` scope, the same redirect runs twice on every Bash call. The installer detects this on re-install (decision 3) and surfaces it as a warning: "git-prism redirect hook already installed at user scope; installing at project scope will cause duplicate redirects. Continue? [y/N]".

**Missing target directory:**

If `<repo>/.claude/` does not exist for `--scope project` or `--scope local`, the installer creates it (`mkdir -p`). For `--scope user`, `~/.claude/` always exists if Claude Code is running. We never create `~/.claude/` ourselves — if it is missing, we error with "Claude Code does not appear to be installed".

**Discovery (`install-hooks --status`):**

The installer's `--status` flag (no positional args) reads all three settings files and prints a table of where git-prism entries are installed and which version of the hook script each one points at. This makes "is it installed?" answerable without `cat ~/.claude/settings.json | jq`.

### 3. Idempotency: sentinel field `id: "git-prism-redirect-vN"`

Each PreToolUse entry the installer writes carries an explicit `id` field with the value `git-prism-redirect-vN` where `N` is the hook schema version (start at `1`). Re-install is detect-and-replace based on exact `id` match.

**Algorithm:**

1. Read the target settings file (create empty `{}` if missing).
2. Locate `hooks.PreToolUse` array (create if missing).
3. For each entry git-prism would write, search the array for an existing entry with a matching `id` field.
4. If found:
   - If the hook script path matches what we would write: no-op, log "already installed".
   - If the path differs (user moved the script, or version bump): replace the entry, log "updated".
5. If not found: append the entry.

**User-edited entries:**

If we find an entry with our `id` whose `command`, `matcher`, or other fields have been hand-edited away from what we would write, the default is **skip with a warning**: "Existing git-prism-redirect entry has been modified locally. Skipping. Pass `--force` to overwrite, or `--uninstall` first." This protects the user's customizations.

`--force` overwrites unconditionally. We do not attempt three-way merge — too clever, fails silently, and the user can always `--uninstall && install-hooks` to reset.

**Why a sentinel `id` field over command-string match:**

- Command-string match breaks the moment we change the script path or rename the file.
- Comment-marker match (`// managed by git-prism`) is fragile because JSON does not support comments — we would need to invent a convention (e.g., a `_managed_by` sibling field), which is just an uglier sentinel.
- The `id` field is already a first-class field in the Claude Code hook schema (used for ordering and conflict detection by the harness itself). Reusing it costs nothing and is the most idiomatic option.

The version suffix (`-v1`) lets future schema changes detect old entries cleanly: when we ship `v2`, the installer replaces all `git-prism-redirect-v1` entries with `git-prism-redirect-v2` versions. Old uninstalls remain possible via `--uninstall --version v1` for users who want to roll back.

### 4. BDD testability: subprocess shell-out with mock JSON on stdin

Step definitions in `bdd/steps/` shell out to the bundled `hooks/git-prism-redirect.sh` (with `parse_git_invocations.py` next to it on `PYTHONPATH`) using `subprocess.run`. The Gherkin scenarios feed mock JSON on stdin, then assert on:

- exit code: `0` (allow), `2` (block)
- stderr text: must contain a redirect message
- stdout JSON: when emitted (advisory mode), must validate against the Claude Code hookSpecificOutput schema

**Hermetic constraints:**

- Tests must NOT mutate `~/.claude/settings.json`. Use a `tempfile.TemporaryDirectory()` per scenario, set `HOME` to it, run the install/uninstall commands inside that sandbox, then assert on the resulting tempdir contents. Tear down by letting the tempdir context manager exit.
- Tests must NOT spawn a real Claude Code process. The hook script reads stdin and exits — that's the entire contract. The behave tests exercise that contract directly.
- Test fixtures live under `bdd/fixtures/hook_inputs/` as JSON files, named after the scenario they support (e.g., `bash_compound_diff.json`, `bash_status_only.json`).

**Coverage matrix the BDD scenarios must cover:**

| Input shape | Expected behavior |
|---|---|
| `git diff main..HEAD` (bare) | advisory redirect emitted |
| `cd /tmp && git diff main..HEAD` (compound) | advisory |
| `(git log main..HEAD)` (subshell) | advisory |
| `git diff $BASE..HEAD` (var expansion) | advisory |
| `git diff main..HEAD \| grep foo` (pipeline) | advisory |
| `cat <<EOF\\ngit diff a..b\\nEOF\\ngit status` (heredoc with git inside body) | NO advisory (only `git status` is real) |
| `git status` | no advisory |
| `git add file.txt` | no advisory |
| `gh pr diff 123` | hard block (exit 2) |
| `mcp__github__get_commit` tool input | hard block |
| Empty stdin | exit 0, no output |
| Malformed JSON | exit 0 (fail open), warning on stderr |

**Why not unit-test the Python tokenizer in isolation:**

We will, for the parser logic specifically (Pythonic unit tests under `hooks/tests/test_parse_git_invocations.py`, run by `pytest`). But the behavioral contract — "this exit code, this stderr, given this stdin JSON" — is what Claude Code actually exercises, and that contract is the load-bearing surface. The BDD layer tests it end-to-end. Unit tests catch regressions in the parser internals; BDD catches regressions in the wire protocol. We need both.

### 5. Subagent MCP scope bug: real, relevant, default to `--scope user`

Verified both issues exist (queried `gh api repos/anthropics/claude-code/issues/...`):

- **#13605** ([link](https://github.com/anthropics/claude-code/issues/13605), closed 2026-03-25): "Custom plugin subagents cannot access MCP tools (built-in agents can)." Workaround documented: use `general-purpose` built-in agent. Marked as resolved by reporter, but the resolution is "use the workaround" — the underlying behavior is unchanged.
- **#13898** ([link](https://github.com/anthropics/claude-code/issues/13898), still **open** as of 2026-04-21): "Custom Subagents Cannot Access Project-Scoped MCP Servers (Hallucinate Instead)." Custom subagents in `.claude/agents/` cannot call tools from `.mcp.json` (project scope) — they hallucinate plausible-but-incorrect responses. **Globally configured MCP servers (`~/.claude/mcp.json`) work correctly in the same subagents.** The reporter's test matrix is unambiguous.

**Relevance to git-prism:**

git-prism is an MCP server. Users who run subagents (via the Task tool in Claude Code) and want those subagents to call git-prism tools (`get_change_manifest`, etc.) hit #13898 if git-prism is registered in project-scoped `.mcp.json`. The subagent silently hallucinates structured-looking output. This is exactly the failure mode the redirect-hook epic is trying to prevent — agents not actually using git-prism — and it would silently defeat the entire epic if we recommended project scope.

**Decision:**

- **Default `git-prism install-hooks --scope user`.** Document the reason in the CLI help text: "User scope is the default because Claude Code issue anthropics/claude-code#13898 prevents custom subagents from calling project-scoped MCP servers correctly."
- **Document the same caveat in the README install-hooks section**, with a link to the upstream issue.
- **Do not block `--scope project` or `--scope local`** — power users may have their own reasons (e.g., team-shared config in a monorepo, no subagent usage). Just don't make it the easy path.
- **Re-evaluate when #13898 closes.** Add a TODO comment in `install-hooks` source that links the issue. When it closes, revisit whether `--scope project` should become the default for the install-hooks command (consistent with how MCP server registration in `.mcp.json` works).

This is the same workaround the upstream issues converged on. It is the right default until upstream is fixed.

## Consequences

- **Single-file Python tokenizer.** `hooks/parse_git_invocations.py` is stdlib-only and can be vendored alongside the hook script with no install ceremony. CI runs the tokenizer's pytest suite directly with the system Python.
- **Two wrapper functions added to the parser.** Heredoc skipping and backtick normalization. Both are <30 lines, both are unit-testable in isolation. The complexity budget for "things `shlex` doesn't handle" is bounded.
- **One new CLI subcommand.** `git-prism install-hooks` with flags `--scope`, `--uninstall`, `--force`, `--status`. Wired in `src/main.rs` like `serve` / `manifest`.
- **Hook entries gain a stable `id`.** Future schema changes can migrate cleanly. The version suffix is part of the contract.
- **`--scope user` is the documented default.** The README and `--help` text both explain why. When upstream #13898 closes, this default becomes a candidate for revisit.
- **BDD tests use subprocess shell-out, not in-process import.** Step definitions stay in Python (cross-language to Rust production code, per project BDD policy). Hermeticity comes from `tempfile`-scoped HOME, not from mocking the script's contents.
- **No third-party Python parser dependency.** The hook works on a stock `python3` install.

## Prevention

- The `id` sentinel must be enforced in the writer and tested in BDD: a re-install scenario with an existing entry must produce no duplicates. If the sentinel is dropped or renamed, idempotency breaks silently — the BDD scenario catches it.
- The shlex-edge-case list (heredoc body, backticks) must have explicit BDD scenarios, not just unit tests. If someone "improves" the parser later and accidentally regresses one of these, the BDD layer flags it.
- The `--scope user` default must be documented in three places (CLI `--help`, README, this ADR). Drift on the default risks reverting a deliberate decision. When #13898 closes, the revisit must update all three together.
- `parse_git_invocations.py` must be importable in tests as `from hooks.parse_git_invocations import tokenize_command`. The hook shell script must invoke it via `python3 -m parse_git_invocations` so the test path and the runtime path use the same module-loading pattern.

## Alternatives Considered

1. **`bashlex` (third-party AST parser).** Rejected: GPLv3 license, bootstrap cost (venv or vendoring), and the missed cases relative to `shlex` are not git-invocation patterns. The cost outweighs the benefit for this use case.

2. **Handwritten recursive-descent bash parser.** Rejected: more code, higher bug surface, and unverifiable without a substantial test corpus that we'd have to author from scratch. `shlex` has 30+ years of stdlib hardening for free.

3. **Tree-sitter bash grammar called from Rust.** Rejected: tree-sitter parsing happens in the Rust binary, but the hook runs in the Claude Code harness as a separate subprocess that reads JSON on stdin. Calling back into the Rust binary just to parse a string would require either a long-running daemon or per-call binary spawn (~100ms cold start each). The Python option is simpler and avoids round-tripping.

4. **Local scope (`--scope local`) as the default.** Rejected: per-checkout, gitignored, and useless for "I just installed git-prism, redirect everything." User scope is what most users want; project/local stay available for the minority who need them.

5. **Project scope (`--scope project`) as the default.** Rejected: would directly trip Claude Code issue #13898 for subagent users. The whole point of the epic is to make agents reach for git-prism — silently breaking subagent calls would be a self-inflicted regression.

6. **Comment-marker idempotency (`// managed by git-prism`).** Rejected: JSON does not support comments. Inventing a `_managed_by` field is functionally identical to using the existing `id` field but uglier.

7. **Three-way merge for user-edited entries.** Rejected: too clever, silent failures when the merge gets it wrong, and `--uninstall && install-hooks --force` is a perfectly serviceable manual reset. Keep the surface area small.

8. **Unit-test only (skip BDD for hook scripts).** Rejected: the Claude Code wire contract — exit code, stderr, stdout JSON shape, given stdin JSON — is the load-bearing surface. Unit tests on the parser internals do not exercise that contract. Both layers earn their keep.

9. **Wait for upstream #13898 to fix project-scope subagent MCP.** Rejected: issue has been open since 2025-12-13 with no fix in sight; we cannot block the epic on Anthropic's release schedule. Default to user scope, document the constraint, revisit when upstream closes.

## References

- Spike issue: https://github.com/mikelane/git-prism/issues/235
- Parent epic: https://github.com/mikelane/git-prism/issues/234
- Existing local hook prototype: `~/.claude/hooks/git-prism-redirect.sh`
- ADR template: `docs/decisions/0007-pr-125-squash-merge-post-mortem.md`
- Python `shlex` docs: https://docs.python.org/3/library/shlex.html (specifically `shlex.shlex` with `punctuation_chars=True` and `posix=True`)
- `bashlex` (rejected alternative): https://github.com/idank/bashlex
- Claude Code MCP scope bug (closed): https://github.com/anthropics/claude-code/issues/13605
- Claude Code project-scope subagent MCP bug (open): https://github.com/anthropics/claude-code/issues/13898
- `claude mcp add --scope` reference (for parallel design): output of `claude mcp add --help` (verified 2026-04-26)
