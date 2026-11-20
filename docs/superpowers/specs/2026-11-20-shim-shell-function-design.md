# Design: git-prism shim as a shell function (agent-typed git only)

Date: 2026-11-20. Status: approved approach (A); doc pending approval.

## Intent

git-prism should give an agent dense, structured output **whenever the LLM itself will read git's output**, and get out of the way **whenever a program reads it**. Scripts spend no tokens and are written against real git's output, so they must get byte-for-byte git.

Trigger: under the 0.10.0 PATH shim, `~/.claude/hooks/lib/classify_pr.py` ran `git diff -M --no-color --no-ext-diff A..B` and received a JSON manifest with exit 0. A T3 PR (CI workflow, 1235-line patch) was classified T1. Root cause: agent detection uses inherited env vars (`AI_AGENT`, `CLAUDECODE`), so every process an agent launches looks like the agent. The flag heuristic (`has_scripted_output_flag`) cannot close that gap.

## Goals

1. Commands the agent types into its shell (`git diff main..HEAD`, `git diff | jq`, `$(git log a..b)`) keep getting git-prism output.
2. Any program that execs git (Python `subprocess`, `make`, `bash script.sh`, `#!/bin/zsh` scripts, `xargs git`, `sh -c`) gets real git: same stdout bytes, stderr, exit code.
3. Anything the shim cannot serve (blob/tree/index revs, unknown forms, handler errors) execs real git instead of erroring or printing something else.
4. Intercepted JSON carries a one-line hint telling the agent how to get raw git (`command git …`).

## Non-goals

- Opt-in for wrapper scripts the agent reads (deferred; purely additive later).
- Process-ancestry detection.
- Fixing `classify_pr.py`'s T1 fallback (separate, in `~/.claude`).

## Anti-goals

- Any path where a program receives JSON when it asked git for text. Wrong-cost is asymmetric: missed interception costs tokens; wrong interception produces silently wrong results. When unsure, pass through.

## Constraints

- Breaking change (0.x minor bump, `feat(shim)!`). CHANGELOG + ADR superseding ADR-0009/0010 positioning ("shim catches make/sh -c" is no longer a goal).
- Production code uses `gix` only; real git is exec'd via existing `RealGitExec`.
- Install edits users' rc files — format must be right first time; uninstall must remove it cleanly; migration from the 0.10.0 PATH block + symlinks must be automatic.
- The function must live in the **interactive** rc only (`.zshrc` / `.bashrc`), never `.zshenv` / `.bash_profile`-for-non-interactive, because `.zshenv` is read by every zsh including scripts.
- Mechanism depends on Claude Code's shell snapshot capturing rc functions. Verified for zsh (276 functions in the current snapshot). Bash: unverified — builder verifies early.

## Approaches considered

- **A. Shell function in interactive rc (chosen).** `git() { command git-prism shim-run git "$@"; }`. Functions are not inherited by child processes, so only agent-typed commands hit it. Correct by construction; failure mode is "not intercepted" (safe). Cost: only works where the agent shell snapshots/sources the interactive rc.
- **B. Keep PATH shim, check parent process.** Intercept only when git's parent is the agent's shell. Per-platform/per-agent code; `$(...)`, pipelines and `xargs` change the tree; wrong guesses still produce silently wrong output. Rejected.
- **Dropped:** expanding the flag heuristic (already failed), opt-in env var (agents won't use it), removing interception (gives up the value).

## Design

1. **New entry point `git-prism shim-run <git|…> [args…]`** (hidden subcommand). Runs today's `run_shim` decision tree unchanged (opt-out → loop-break → agent detection → classify → handler), with `argv[0] = "git"`. Real-git resolution must skip the function and any legacy shim dir (existing `RealGitExec` already skips the shim dir; confirm it doesn't recurse into git-prism).
2. **Managed rc block** (same `# >>> git-prism shim >>>` / `# <<< git-prism shim <<<` markers and `strip_all_marker_blocks` logic) written to the interactive rc only, containing the `git` function. `shim install` migration: strip old blocks from `.zshenv`/`.zshrc` (or bash pair), write new block to `.zshrc`/`.bashrc`, remove `~/.local/share/git-prism/bin/{git,gh}` symlinks and the dir if empty. `shim uninstall` removes block + any legacy artifacts. `shim status` reports function block present / legacy PATH shim present.
3. **Passthrough on handler failure.** `handlers::handle` currently prints `git-prism shim: handler error: …`. Output is buffered, so on error discard the buffer and `exec.passthrough(argv)`. Keep the #384 colon rule for `show`; add `^{…}` peel syntax (`A^{tree}`, `A^{blob}`) to passthrough.
4. **Redirect-to-file passthrough (user decision).** In `shim-run`, if stdout is a regular file (`fstat` on fd 1 → `S_ISREG`, e.g. `git diff A..B > x.patch`), exec real git. Pipes and the Bash tool's own pipe are still intercepted; `git diff | git apply` is accepted as intercepted, and the hint points the agent at `command git`. Tests: `> file` yields bytes identical to real git; `| cat` yields JSON.
5. **Hint.** Intercepted JSON responses gain a top-level field (e.g. `"_raw_git": "this is git-prism output; for raw git run: command git <args>"`). Exact field name is the builder's call; keep it one line.
6. **Legacy argv[0]=git mode** in `main.rs`: if the binary is still invoked as `git` (stale symlink), behave as pure passthrough to real git. Prevents a half-migrated machine from reproducing the bug.

## Tests (must not be vacuous)

- Child env sets an agent marker (`AI_AGENT=…`) and unsets `CI`, `GIT_PRISM_PASSTHROUGH`, `GIT_PRISM_DISABLE`; otherwise detection short-circuits and tests pass without exercising the bug.
- Use the worktree-built binary, never the installed one. Compare against the same real git the shim resolves to.
- Parity (parametrized, real temp repo): a program exec'ing git through PATH gets output identical to real git for `diff -M --no-color --no-ext-diff A..B`, `show A:path`, `show A:`, `show :path`, `show A^{tree}`, `log A..B`; exit codes match including bad ref and missing path.
- Function path: `zsh -ic` (or sourcing the generated block) with `git diff A..B` → JSON with hint; same shell running `python3 -c 'subprocess…git diff…'` → real git.
- Install/uninstall/migration unit tests on rc content (old PATH block in both files → new function block in interactive rc only; idempotent re-run; uninstall leaves no markers).
- BDD: update `shim_wrapper.feature` / `shim_parity.feature` first (RED), add scenarios for "program-launched git is byte-for-byte" and "handler failure execs real git".

- Existing shim BDD/unit tests that build a `bin/git` symlink and expect interception are rewritten to call `git-prism shim-run git …`; symlink scenarios remain only to prove pure passthrough (root's call).
- `gh`: no function, no `shim-run gh` interception; a stale `gh` symlink is pure passthrough (root's call — gh has been passthrough since #376).

## Docs

README shim section, CLAUDE.md, CHANGELOG `[Unreleased]` breaking entry, new ADR (00NN) superseding the PATH-layer positioning of 0009/0010. No `Cargo.toml` version bump in this PR — releases go through a separate `chore: release` PR (root's call, per repo convention and `integrity.yml`).

## Left to the builder

- Hint field name and wording.
- Whether `shim-run` is hidden from `--help`.
- Bash verification result; if Claude Code's bash snapshot does not capture functions, document zsh-only support and make install on bash print that.
- Whether `gh` needs a function at all (it is pure passthrough since #376 — likely drop it).

## Out of scope / report only

- `classify_pr.py` falls back to T1 on unparseable diff; should be T3.
- PRs classified locally under the 0.10.0 shim may be under-tiered; audit separately.
- The fix is inert locally until released and `brew upgrade`d + `git-prism shim install` re-run.
