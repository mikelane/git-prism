# ADR 0009: PATH Shim Architecture

- **Status**: Implemented
- **Date**: 2026-05-29
- **Context**: Epic #284 — a PATH shim that intercepts agent `git` calls outside Claude Code's hook pipeline

## Context

The bundled redirect hook (see [ADR 0008](0008-redirect-hook-spike.md)) only fires
inside Claude Code's `PreToolUse` pipeline. It catches `git diff/log/show/blame`
that an agent issues through the Bash tool, but it is blind to git invoked by any
other path: a subprocess the agent spawns, a shell wrapper, a build script, an
MCP server of its own. Those calls reach the real git untouched and hand the agent
porcelain output — exactly the token-wasteful diff text git-prism exists to replace.

A PATH shim closes that gap at the process level. If `git-prism` sits on `PATH`
under the name `git`, ahead of the real git, then *every* `git` invocation in that
shell flows through git-prism first, regardless of who spawned it. git-prism then
decides per command whether to intercept (return structured JSON) or pass through
(exec the real git unchanged).

This ADR records the architectural decisions for that work. The agent-detection
foundation it depends on landed earlier in [PR #280](https://github.com/mikelane/git-prism/pull/280);
the shim itself is the body of epic [#284](https://github.com/mikelane/git-prism/issues/284).

## Decision

### 1. `argv[0]`-aware dispatch in the same binary, not a separate binary

The shim is the same `git-prism` binary. At startup, `main` inspects `argv[0]`: if
the program was invoked under a name ending in `git` (the symlink), it enters shim
mode; otherwise it dispatches the normal clap CLI (`serve`, `manifest`, `hooks`, …).

**Rationale.** One binary means one build, one release artifact, one version to keep
in sync. A separate `git-prism-git-shim` binary would double the cargo-dist matrix,
require a second crates.io entry or a bundled-binary install step, and invite version
skew between the shim and the tool whose JSON path it reuses. `argv[0]` dispatch is
the classic Unix multi-call pattern (busybox, coreutils' `[`); it costs one string
check at startup.

**Consequences.**

- (+) Single artifact; the shim always matches the installed git-prism exactly.
- (+) The shim reuses the same git + tree-sitter code paths in-process — no IPC, no
  second binary to locate.
- (−) A trivial amount of shim-mode branching lives in the otherwise-CLI `main`.
  Mitigated by keeping the shim logic in `src/shim/` and having `main` only dispatch.
- (−) The install symlink must point at a real `git-prism`; a moved or deleted binary
  breaks the shim. Acceptable — the same is true of any installed tool.

### 2. PATH shim, not (only) a Claude Code hook rewrite

The redirect hook stays; the shim is additive, not a replacement.

**Rationale.** The hook and the shim cover disjoint blind spots. The hook sees the
agent's *intent* (the literal Bash command text, before execution) and can soft-warn
or rewrite it with full Claude Code context. The shim sees *every* git process, even
ones the hook never observes, but only at exec time with no conversational context.
Neither subsumes the other. A hook-only solution misses non-Bash-tool git; a
shim-only solution loses the ability to advise the agent in-band.

**Consequences.**

- (+) Defense in depth: two independent interception points.
- (+) The shim works for agents that aren't Claude Code at all, since detection is
  env-var-based (see #280), not Claude-specific.
- (−) Two mechanisms to document and reason about. Mitigated by this ADR and the
  README sections drawing the boundary explicitly.

### 3. `GIT_PRISM_INSIDE_SHIM` sentinel for loop-break, not an in-process guard

The shim sets `GIT_PRISM_INSIDE_SHIM=1` in the environment of every child process it
spawns. On entry, if that variable is already set, the shim immediately passes through
to the real git.

**Rationale.** Recursion here is *cross-process*, not in-process. When the shim execs
the real git, that git may itself shell out (hooks, `lefthook`, `cargo`'s embedded
git, a build script) — and because the shim is still first on `PATH`, those nested
calls re-enter git-prism. An in-process recursion counter cannot see across that
`exec`/`fork` boundary; only an inherited environment variable survives it. The
sentinel is the standard wrapper-script technique (cf. `ccache`, `sccache`, editor
`$EDITOR` re-entrancy guards).

**Consequences.**

- (+) Robust against arbitrarily deep nesting through unrelated tools.
- (+) Doubles as a user-facing escape hatch: `GIT_PRISM_INSIDE_SHIM=1 git …` forces
  vanilla git for one command.
- (−) Relies on env inheritance; a child that scrubs its environment could, in theory,
  loop. In practice nothing in the git toolchain scrubs `GIT_PRISM_*`, and the cost of
  a missed sentinel is a loop the user would notice immediately, not silent corruption.

### 4. Exact port of the Python classifier, not new ref-range logic

The watch-list / ref-range classifier is a faithful port of the existing Python hook
classifier into `src/shim/classify.rs`, not a fresh Rust design.

**Rationale.** The two interception points must agree on what counts as an
interceptable command, or an agent gets inconsistent behavior depending on which path
its git call took. Porting the already-tested Python logic — same watch list, same
ref-range detection — guarantees that agreement and lets us reuse the existing corpus
of classification cases as Rust test fixtures. Inventing parallel logic would risk
subtle divergence (a flag one side intercepts and the other passes through).

**Consequences.**

- (+) Behavioral parity between hook and shim by construction.
- (+) Existing classification edge cases transfer directly into Rust unit tests.
- (−) The two implementations must be kept in sync by hand; a change to one needs a
  matching change to the other. Mitigated by shared test cases and a note in both
  modules.

### 5. Passthrough for ref-range-less forms, not intercepting all `git diff`

The shim only intercepts watch-list subcommands when a **ref range** is present
(`git diff main..HEAD`, `git log A..B`). A bare `git diff`, `git diff --staged`, or
`git status` passes straight through.

**Rationale.** git-prism's structured JSON answers "what changed between two refs."
A bare `git diff` (working tree vs index) and interactive forms have no two-ref shape
to map onto the manifest tools, and intercepting them would break the muscle-memory
commands humans and agents rely on for quick local inspection. Restricting
interception to ref-range forms targets exactly the PR-review / blast-radius use case
the JSON path serves, and leaves everything else as plain git.

**Consequences.**

- (+) Zero surprise for the overwhelming majority of git usage — only the explicit
  range forms change behavior.
- (+) Matches the MCP tools' own contract, which is ref-range oriented.
- (−) An agent that wants structured working-tree data via the shim doesn't get it
  (it must use the MCP tool's working-tree mode instead). Accepted — the shim's job is
  to catch the range-diff calls that leak past the hook, not to cover every mode.

### 6. Keep the MCP server alongside the shim, not deprecate it

The shim is an additional front door to the same data; the MCP server (`git-prism serve`)
remains the primary, fully supported interface.

**Rationale.** The MCP tools are richer than the shim can be: pagination, explicit
budgets, `function_names` filtering, working-tree mode, the `review_change`
orchestration. The shim necessarily speaks the fixed vocabulary of git's CLI surface
and can only return what a given `git diff`/`log` shape maps onto. The shim is a
convenience for git calls that escape the MCP path, not a superset of it. Deprecating
MCP would lose capability and break every existing Claude Code registration.

**Consequences.**

- (+) Agents get the full MCP tool surface when they call git-prism directly, and a
  graceful fallback when they shell out to git.
- (+) No migration burden on existing users.
- (−) Two surfaces returning overlapping-but-not-identical JSON shapes. Mitigated by
  the shim reusing the same underlying manifest code, so the data is consistent even
  where the envelope differs. The shim is labeled **experimental** in the README to
  set expectations while its surface settles.

## Status of dependent work

Implementation landed across #280 (agent detection), #286 (shim core), #287 (`argv[0]`
dispatch), #288 (`hooks install --path-shim`), #289 (telemetry counters), #296 (exit
codes 126/127), #299 (`GIT_PRISM_DEBUG_RESOLVER`), #302 (`Created symlink:` line), and
#314 (Windows `cfg` guards). The capstone demo (#291) records the end-to-end flow.
