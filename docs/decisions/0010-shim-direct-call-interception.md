# ADR 0010: Shim Direct-Call Interception via Claude Code's Shell Snapshot

- **Status**: Accepted (spike outcome)
- **Date**: 2026-05-30
- **Context**: Epic #328 — sunset the redirect hook; spike #320
- **Supersedes assumptions in**: the epic premise that direct-call interception was an open question

## Context

Epic #328 wants to remove the bundled redirect hook (`hooks/bash_redirect_hook.py`)
and rely on the PATH shim alone. That is only safe if the shim is a true **superset**
of the hook's coverage — including the hook's one job: intercepting `git` that an agent
issues directly through Claude Code's Bash tool.

The redirect hook is a Claude Code `PreToolUse` hook. It fires on the literal Bash
command *string* an agent emits, before execution. The PATH shim intercepts at a
different layer entirely: it puts a `git` symlink (→ `git-prism`) on `PATH` ahead of
the real git, so every `git` *exec* in that process tree flows through git-prism. These
two mechanisms are orthogonal (see [ADR 0009](0009-path-shim-architecture.md) §2).

The load-bearing unknown for the whole epic: **does the shim's `PATH` entry actually
reach the subshell that Claude Code's Bash tool spawns?** The worry was that Claude Code
runs each Bash command in a fresh non-login subshell that doesn't re-source `~/.zshrc`,
so a `PATH` export added to rc might never be seen.

This spike answered that empirically, from inside a live Claude Code session — the exact
process under investigation.

## How Claude Code's Bash tool actually runs commands

Observed on macOS, Claude Code launched from an interactive zsh under Warp:

1. Each Bash tool call is run by **`/bin/zsh -c`** (not `bash -c` as the epic assumed),
   and the command is wrapped so it **first sources a shell snapshot**:

   ```
   /bin/zsh -c 'source ~/.claude/shell-snapshots/snapshot-zsh-<id>.sh 2>/dev/null || true && … && eval <command>'
   ```

2. That snapshot is **generated once per `claude` launch** (one file per launch; 14
   were present). Its mtime trailed the `claude` process start by ~47s — it is written
   at startup, not per command.

3. The snapshot **hardcodes `PATH`** as a single resolved string:

   ```sh
   export PATH='/Users/.../.cargo/bin:/opt/homebrew/bin:…'
   ```

   It does **not** re-evaluate rc files at command time. `PATH` for every Bash tool
   call is whatever string was frozen into the snapshot at launch.

4. The snapshot also captures 45 rc-defined **aliases** (`cat=bat`, `aws=sso-aws`, …)
   and the user's shell **functions**. Their presence proves snapshot generation
   **sources the user's rc files** — even though the launching shell was `zsh -g
   --no_rcs` (rc *not* sourced by the launcher itself). Claude Code sources rc when it
   builds the snapshot.

The net model: **`PATH` inside the Bash tool = the rc-derived `PATH` of the environment
in which `claude` was launched, frozen at launch into a per-session snapshot.**

## What we tested

A throwaway lab (`/tmp/shim-spike`) placed a fake `git` on `PATH` ahead of the real one
and invoked git five ways. Results:

| Invocation | Intercepted? | What it proves |
|---|---|---|
| A. Direct `git status` | ✅ | direct Bash-tool calls resolve through the shim |
| B. `sh -c 'git log'` | ✅ | child processes inherit `PATH` |
| C. `git` inside a Makefile target | ✅ | build tools inherit `PATH` |
| D. script → script → `git` (2 levels) | ✅ | transitive inheritance holds arbitrarily deep |
| E. `env -i PATH=/usr/bin:/bin git` | ❌ (real git) | dependency is *purely* `PATH` — strip it and the shim is gone |

Cases B–D are precisely the calls the redirect hook is **structurally blind to**: the
hook only sees the top-level command string (`make build`), never the nested `git`. The
shim catches all of them. This is the empirical proof of the "shim ⊇ hook" premise that
the epic depends on.

## Decision

**Direct-call interception works, and the shim is a strict superset of the redirect
hook — *conditional on the shim directory being in `PATH` at the moment `claude` is
launched.*** That condition is satisfiable and is exactly what the auto-PATH-setup
feature (#325) must guarantee.

### Answers to the spike questions

1. **Does `git` resolve through the shim for a direct Bash-tool `git status`?**
   Yes — provided the shim dir is in the launch-time snapshot `PATH`.

2. **Does it resolve through the shim for `git` inside make / lefthook / build scripts?**
   Yes. `PATH` is inherited by every descendant process (cases B/C/D). This is the
   shim's decisive advantage over the hook.

3. **Exact install requirements?**
   - The shim dir (`~/.local/share/git-prism/bin`) must be on `PATH` in the rc that
     Claude Code sources when it builds its snapshot (`~/.zshrc` / `~/.zshenv` /
     `~/.bashrc`, per the user's shell).
   - **Claude Code must be (re)started after the rc edit.** The running session's
     snapshot is frozen; rc edits do not reach it. A *new* `claude` launch regenerates
     the snapshot and picks up the new `PATH`. A terminal restart is **not** required —
     only that the shell launching `claude` has the updated rc in effect.

4. **Is there a race / timing window?**
   Yes, exactly one: the snapshot is captured at launch. Any `PATH` change after launch
   (including the shim's own install) is invisible to the current session until the next
   `claude` launch. This is a one-time "install → restart Claude Code" step, not an
   ongoing race.

5. **macOS vs Linux?**
   The mechanism is POSIX-universal: snapshot sources rc, exports a resolved `PATH`,
   children inherit it. Directly observed on macOS/zsh only. Linux and bash are
   *expected-equivalent* (the snapshot file is named `snapshot-bash-*` there) but were
   **not** directly observed in this spike — see Limitations.

## Consequences

- (+) The epic's core premise holds: the redirect hook can be removed without losing
  direct-call coverage, **and** the shim additionally covers the nested calls (B/C/D)
  the hook never could.
- (+) #325 (auto-PATH-setup) is well-defined: append the `export PATH` line to the
  user's rc, then instruct the user to **restart Claude Code**. Because snapshot
  generation sources rc, the next launch picks it up — no per-command magic needed.
- (+) The shim is launcher-agnostic in principle: any agent whose subprocess `PATH`
  includes the shim dir is covered, not just Claude Code.
- (−) The "restart Claude Code after install" step is unavoidable and must be loud in
  the install output and the README. Users who install mid-session and see no
  interception are hitting the frozen-snapshot window, not a bug. #325's install
  message must say this explicitly.
- (−) Anything that resets `PATH` (`env -i`, `sudo` with `secure_path`, a container with
  a scrubbed environment) defeats the shim for those calls. This is inherent to any
  PATH-based interceptor and is acceptable — the redirect hook covered none of those
  cases either.
- (−) The snapshot behavior is a Claude Code implementation detail, not a contract. If
  Claude Code stops snapshotting `PATH` (e.g. switches to fully fresh subshells with
  inherited-but-not-frozen env), direct interception still works **as long as the parent
  `claude` process `PATH` includes the shim dir** — which the same rc edit guarantees.
  The dependency is on `PATH` inheritance, which is stable; the snapshot is just the
  concrete delivery mechanism we observed.

## Limitations (honesty about evidence boundaries)

- Observed only on **macOS / zsh / Warp**. Not directly verified on Linux, bash, the
  VS Code extension, plain Terminal.app, or tmux. The mechanism is general, but the
  BDD scenarios (#321) should include at least one non-zsh path, and #322's
  `windows-latest` job is the Windows evidence.
- The fake-`git` lab proves `PATH`-layer resolution and inheritance, not git-prism's own
  classifier. That behavior is covered by existing `src/shim/` unit tests and ADR 0009.
- Spike prototype code lives in `/tmp/shim-spike` and on the disposable
  `spike/shim-direct-call` branch (if pushed for the record). It is **evidence, not
  foundation** — only this ADR is merged to main.

## Follow-ups this ADR unblocks

- #321 BDD bootstrap — scenarios may now assume the install + restart model documented
  above. Read this ADR before writing them.
- #325 auto-PATH-setup — design confirmed: rc append + "restart Claude Code" message.
- #326 redirect-hook removal — the superset claim is proven; removal is safe once
  #322/#323/#324/#325 land.
