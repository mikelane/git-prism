# ADR 0012: Shim as an Interactive-Shell Function

- **Status**: Accepted
- **Date**: 2026-11-21
- **Context**: Issue [#387](https://github.com/mikelane/git-prism/issues/387)
- **Supersedes**: the PATH-layer interception decided in [ADR 0009](0009-path-shim-architecture.md) §1–§2 and the "shim ⊇ hook" conclusion of [ADR 0010](0010-shim-direct-call-interception.md)

## Context

Through 0.10.0 the shim was a PATH entry. `git-prism shim install` created
`~/.local/share/git-prism/bin/{git,gh}` symlinks to the git-prism binary and a
PATH-prepend block in the shell startup files. Every process that resolved `git`
from `PATH` ran git-prism first. ADR 0010 counted this as an advantage: `sh -c`,
`make`, and nested scripts were all intercepted, which the redirect hook could
never do.

That coverage caused a real failure. `~/.claude/hooks/lib/classify_pr.py`, a
Python program launched from an agent session, ran:

```bash
git diff -M --no-color --no-ext-diff A..B
```

It received a JSON change manifest with exit status 0 instead of a unified diff.
The classifier could not parse it and fell back to its lowest tier. A T3 PR (a CI
workflow change with a 1235-line patch) was classified T1 and received a much
lighter review than its risk required.

The root cause is agent detection. `detect_calling_agent` reads environment
variables (`AI_AGENT`, `CLAUDECODE`, and the other markers in
`src/agent_detection.rs`). Child processes inherit those variables, so every
program an agent launches looks like the agent. The scripted-output flag
heuristic (`has_scripted_output_flag` in `src/shim/classify.rs`) can only
recognize callers that pass a flag it knows about. `-M --no-color --no-ext-diff`
is not on that list, and no finite list covers every script.

The cost of a mistake is asymmetric. If git-prism fails to intercept, the agent
spends extra tokens. If it intercepts wrongly, a program gets output in a format
it did not ask for, often with exit status 0, and produces a wrong result without
any error.

## Decision

### Rule: intercept only when the LLM reads git's output

git-prism replaces git output only when the language model will read that output
itself. When a program reads the output, the program gets real git: the same
stdout bytes, stderr, and exit status.

### Mechanism: a `git` function in the interactive rc file

`git-prism shim install` writes a marker-delimited block to the interactive rc
file: `~/.zshrc`, or `~/.bashrc` when the basename of `$SHELL` is `bash`. It
never writes to `~/.zshenv` or `~/.bash_profile`. zsh reads `.zshenv` for every
shell, scripts included, so a function there would reach scripts again.

```sh
# >>> git-prism shim >>>
function git {
  if command -v git-prism >/dev/null 2>&1; then
    command git-prism shim-run git "$@"
  else
    command git "$@"
  fi
}
# <<< git-prism shim <<<
```

- The block uses `function git { … }` rather than `git() { … }`. With the second
  form, a user's `alias git=…` would alias-expand the definition into a parse
  error.
- If `git-prism` is not on `PATH` (uninstalled, or `PATH` changed), the function
  runs real git. Removing the binary does not break `git`.
- `git-prism shim-run git <args>` runs the existing `run_shim` decision tree:
  opt-out, then the loop-break sentinel, then agent detection, then
  classification, then a handler.

Shell functions are not inherited by child processes. A program that execs `git`
(Python `subprocess`, `make`, `sh -c`, `xargs git`, a `#!/bin/sh` script) finds
the real binary on `PATH` and never reaches git-prism. Only `git` typed into the
agent's own shell runs the function, and this holds without any heuristic.

The function reaches the agent through Claude Code's shell snapshot. ADR 0010
recorded that Claude Code sources the user's rc files once per launch, writes the
result to `~/.claude/shell-snapshots/snapshot-<shell>-<id>.sh`, and sources that
snapshot before every Bash tool call. The snapshot includes rc-defined functions.
This was verified for zsh: the snapshot in use while this design was written
contained 276 functions. **It is unverified for bash.** We have not observed a
`snapshot-bash-*` file that contains a function from `~/.bashrc`. Because the
snapshot is captured at launch, the agent session must be restarted after
`shim install`. The install output says so.

### Passthrough rules inside `shim-run`

Even for a typed command, `shim-run` execs real git in these cases:

| Condition | Why |
|---|---|
| `GIT_PRISM_PASSTHROUGH` or `GIT_PRISM_DISABLE` is `1`/`true` | Explicit opt-out. Checked before telemetry starts. |
| `GIT_PRISM_INSIDE_SHIM` is set | Manual loop-break/escape hatch. The shim no longer sets it for real git; the resolver skipping git-prism binaries prevents recursion. |
| No agent detected (or `CI=true`) | A human typing `git` gets git. |
| The classifier returns `Passthrough` | The subcommand or form is not on the watch list, or it carries an option or argument the handler does not honour (scripted-output flags, `-n`, `-R`, `--` pathspecs, unknown options, extra positionals). The classifier intercepts only an allowlist of presentation flags plus one range, object, or path. `--quiet` and `--exit-code` are now on that list because the caller wants the exit status, not stdout. `git show <rev>^{…}` peel specs now pass through as well. Pickaxe searches (`-S`, `-G`, `--pickaxe-*`) pass through because the function-context handler ignored the search term. `git blame <path>` passes through when the path is staged or modified, because real git blames the working tree. `git show` of a merge commit passes through: real git prints a combined diff and the handler would diff against the first parent only. Three-dot ranges (`a...b`) pass through too: real git resolves them against the merge base (`diff`) or as a symmetric difference (`log`), and the handlers do neither. |
| stdout is a regular file | `git diff A..B > x.patch` is a capture for a program, so it gets real bytes. Detected with `fstat` on fd 1. |
| The handler returns an error | The buffered partial output is discarded and real git runs. The caller sees git's own stdout, stderr, and exit status instead of a git-prism error message. |

### Raw-git hint in intercepted output

Every JSON object the shim prints gets a top-level `git_prism_hint` field:

```json
"git_prism_hint": "git-prism structured output. For raw git output run: command git diff HEAD~1..HEAD"
```

An agent that needs porcelain (for example, to pipe into `git apply`) learns the
bypass from the output itself. `command git` skips shell functions, so it runs
real git. The hint is added only on the shim path. MCP and CLI output do not
change.

### Stale symlinks are pure passthrough

If the binary is invoked with a basename of `git` or `gh`, which is what a
leftover 0.10.0 symlink does, `main` execs the real binary immediately. A
half-migrated machine therefore cannot reproduce the bug. `shim install` and
`shim uninstall` remove the legacy PATH blocks from all four startup files and
the `git`/`gh` symlinks. They remove the directory only if it is then empty, and
they leave regular files a user placed there alone. `shim status` reports any
legacy artifacts it finds.

### Real-binary resolution skips git-prism

Resolving the real `git`/`gh` now skips any `PATH` candidate that canonicalizes
to the running executable or to a file whose name starts with `git-prism`.
Previously, two shim directories on `PATH` could exec each other forever (a
`gh` → `gh` loop).

### `gh` is not intercepted

There is no `gh` function. `gh` has passed through unchanged since #376, so it
gains nothing from interception.

## Alternatives rejected

- **Keep the PATH shim and check the parent process.** git-prism would intercept
  only when git's parent is the agent's shell. This needs per-platform and
  per-agent code. `$(…)`, pipelines, and `xargs` all change the process tree. A
  wrong guess still gives a program JSON without any error.
- **Expand the scripted-output flag heuristic.** This is the approach that just
  failed. Scripts are not required to pass any particular flag, so no finite
  list is complete.
- **Opt-in environment variable.** Interception would happen only when the agent
  sets a variable. Agents do not reliably set variables they were not told about,
  so the feature would mostly be off.

## Consequences

- (+) A program that execs git gets real git, independent of agent detection.
  The `classify_pr.py` failure cannot recur through a program-launched `git`.
- (+) Failures are safe. When the function is missing (non-snapshotting agent,
  bash where the snapshot behavior is unverified, a new shell that did not
  source the rc), the caller gets real git and loses only token savings.
- (−) `make`, `sh -c '…'`, and wrapper scripts are no longer intercepted. ADR
  0010 listed these as the shim's advantage over the redirect hook. That
  advantage is deliberately given up. Coverage is now about the same as the
  removed hook: commands the agent types.
- (−) A pipe from a typed command into a program is still intercepted.
  `git diff A..B | git apply` sends JSON to `git apply`. The shell runs the
  function on the left of the pipe, and stdout is a pipe, not a file. The
  `git_prism_hint` field points the agent at `command git diff A..B | git apply`.
- (−) A program that starts an interactive shell (`zsh -i -c 'git …'`) sources
  the rc file and gets the function, so it is intercepted when agent variables
  are present.
- (−) The function is also defined in a human's interactive shells. Agent
  detection and `CI=true` still decide, so a human without agent variables gets
  real git. The only cost is one extra `git-prism` process per `git` call.
- (−) Users must re-run `git-prism shim install` and restart their agent session
  after upgrading. An existing PATH install becomes inert, not harmful, because
  stale symlinks pass through.
- (−) The design depends on Claude Code's snapshot capturing rc functions. This
  is an implementation detail of Claude Code, not a documented contract.

## Status of earlier ADRs

- **ADR 0009**: superseded in part. §1 (`argv[0]` dispatch as the interception
  entry point) and §2 (PATH shim) are replaced by `shim-run` and the rc function.
  §3 (the `GIT_PRISM_INSIDE_SHIM` sentinel), §4 (the classifier port), §5
  (passthrough for forms without a ref range), and §6 (keep the MCP server) still
  apply.
- **ADR 0010**: superseded in part. Its observations about the shell snapshot
  (sourced from rc at launch, captures functions, frozen until restart) are the
  evidence this ADR relies on. Its conclusion that PATH interception of nested
  calls is desirable is reversed.
- **ADR 0011**: the redirect hook's removal stands. Its rationale that the shim
  is a "strict superset" of the hook no longer holds, since the function covers
  roughly the same typed commands the hook did. It still covers them without a
  `PreToolUse` dependency and with one implementation shared with the classifier.
