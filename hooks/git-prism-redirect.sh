#!/bin/bash
# git-prism redirect hook — bundled wrapper.
#
# Invoked by Claude Code as a PreToolUse hook. Reads a JSON payload on
# stdin, defers parsing/decision logic to the sibling Python module, and
# returns the Python script's exit code unmodified.
#
# Per ADR-0008 Decision 6: if `python3` is missing on PATH the wrapper
# announces the gap on stderr and exits 0 (fail open) — a broken hook
# must never block a working git command.
#
# The shebang is intentionally absolute (`#!/bin/bash`, not `/usr/bin/env
# bash`) so the env-form's PATH lookup cannot fail before the wrapper
# even runs. The "missing python3" scenario tests precisely this: PATH
# is set to a nonexistent directory, and the hook must still execute.

set -u

if ! command -v python3 >/dev/null 2>&1; then
    printf 'git-prism-redirect: python3 not found on PATH; skipping redirect\n' >&2
    exit 0
fi

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
exec python3 "${SCRIPT_DIR}/bash_redirect_hook.py" "$@"
