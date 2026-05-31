# ADR 0011 — Redirect Hook Removal

**Status:** Accepted  
**Date:** 2026-05-30  
**Supersedes:** ADR-0008 (redirect-hook-architecture)

## Context

The redirect hook (`hooks/bash_redirect_hook.py`, installed via `git-prism hooks install`) was
introduced in ADR-0008 as a way to intercept `git diff`, `git log`, and similar commands issued
by Claude Code's Bash tool and reroute them to git-prism's structured JSON output.

The hook has a fundamental structural limitation that ADR-0009 and ADR-0010 exposed explicitly:

**Claude Code's `PreToolUse` hook fires only on the top-level Bash command string.** It is NOT
a syscall interceptor. Any `git` call issued inside a subprocess — make, lefthook, cargo build
scripts, pre-commit hooks, any wrapper — never triggers the `PreToolUse` event. The hook is
therefore blind to a large fraction of real-world git invocations.

The PATH shim (ADR-0009 / ADR-0010), by contrast, intercepts at the `exec`/`PATH` layer. When
`~/.local/share/git-prism/bin` appears first on `PATH`, every process that resolves `git` from
PATH — including subprocesses, build tools, and scripts — goes through git-prism. The shim is a
**strict superset** of the redirect hook's coverage on Unix systems where PATH is configured
correctly.

Issues #322–#325 (Windows passthrough parity, `gh` CLI interception, first-class `shim`
subcommand, auto-PATH setup UX) established that the shim meets all the operational requirements
that originally motivated the redirect hook. With those issues merged, the redirect hook provides
no capability the shim does not already provide — and provides less.

## Decision

Hard-remove the redirect hook:

1. **Delete** `hooks/bash_redirect_hook.py`, `hooks/git-prism-redirect.sh`, and
   `hooks/test_bash_redirect_hook.py` from the repository.
2. **Remove** `install_redirect_hook`, the `include_str!` embeds
   (`REDIRECT_PY_CONTENT`, `REDIRECT_SH_CONTENT`, `REDIRECT_PY_NAME`, `REDIRECT_SCRIPT_NAME`,
   `SENTINEL_ID`), `execute_install`, `copy_bundled_scripts`, `canonical_entry`,
   `is_stale_path`, `InstallOutcome`, `InstallOptions`, and `other_scopes_with_sentinel` from
   `src/hooks.rs`.
3. **`git-prism hooks install`** (without `--path-shim`) exits **non-zero** with an informative
   message directing users to `git-prism shim install`. There is no deprecated-but-functional
   phase.
4. **Keep** `git-prism hooks uninstall` and `git-prism hooks status` for legacy cleanup — users
   who had the hook installed need a way to remove it.
5. **Keep** `git-prism hooks install --path-shim` as a deprecated alias for
   `git-prism shim install` for one release cycle (it already warns and still works).

This is a **breaking change** — the removal is intentional and permanent. The version bump to
v0.9.0 signals the break.

## Consequences

### Migration path for users of the old hook

Users who previously ran `git-prism hooks install --scope user` must:

```
git-prism hooks uninstall --scope user   # remove old settings.json entry
git-prism shim install                   # install the PATH shim
# then follow the shim install output to add the export PATH line to ~/.zshrc
```

### What is preserved

- `git-prism hooks uninstall --scope <user|project|local>` — removes any lingering hook entries
  from Claude Code's `settings.json`.
- `git-prism hooks status` — reports whether any hook entries remain, plus shim status.
- `git-prism hooks install --path-shim` — deprecated alias still works this release with a
  deprecation warning; use `git-prism shim install` going forward.

### Why no deprecation phase

A "deprecated but still functional" phase would require keeping `bash_redirect_hook.py` embedded
in the binary, which contradicts the epic's hard constraint: the v0.9.0 release is blocked until
the hook is removed. The shim has been production-ready since #325 merged. Users have the
migration path above. A clean hard-removal is operationally safer than shipping two overlapping
interception mechanisms.
