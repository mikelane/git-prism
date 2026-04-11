#!/usr/bin/env bash
# Reject commit messages that reference (#N) where N does not resolve to a real PR or issue.
#
# History: PR #125 was squash-merged with commit messages referencing PRs
# #114-#123 that never existed, creating a phantom audit trail. This hook
# was introduced as part of issue #202 to make that class of phantom
# impossible to commit.
set -euo pipefail

COMMIT_MSG_FILE="${1:-}"
if [ -z "$COMMIT_MSG_FILE" ] || [ ! -f "$COMMIT_MSG_FILE" ]; then
  echo "usage: $0 <commit-msg-file>" >&2
  exit 2
fi

# Extract all (#N) references from the commit message.
REFS=$(grep -oE '\(#[0-9]+\)' "$COMMIT_MSG_FILE" | grep -oE '[0-9]+' | sort -u || true)
if [ -z "$REFS" ]; then
  exit 0
fi

FAILED=""
for N in $REFS; do
  # A real PR or issue must exist. Try PR first, then issue.
  # NB: use `--json state` rather than `--json number` — gh returns the
  # echoed number for any numeric input without actually resolving it,
  # so checking a server-side field like `state` forces real validation.
  if gh pr view "$N" --json state --jq .state >/dev/null 2>&1; then
    continue
  fi
  if gh issue view "$N" --json state --jq .state >/dev/null 2>&1; then
    continue
  fi
  FAILED="$FAILED #$N"
done

if [ -n "$FAILED" ]; then
  echo "error: commit message references PR/issue numbers that do not exist:$FAILED" >&2
  echo "" >&2
  echo "This check exists because PR #125 was once merged with commit messages" >&2
  echo "referencing PRs #114-#123 that never existed, creating a phantom audit trail." >&2
  echo "" >&2
  echo "If you meant to use placeholder numbers, use a different format (e.g. TBD-114)." >&2
  exit 1
fi

exit 0
