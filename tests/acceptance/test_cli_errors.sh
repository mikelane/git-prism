#!/usr/bin/env bash
# @ISSUE-18: error messages are clear, helpful, and never expose Rust panics
#
# Scenario: User provides an invalid ref range
#   Given a git repo
#   When I run git-prism manifest nonexistent-branch..HEAD --repo <path>
#   Then the exit code is nonzero
#   And stderr does not contain "panicked"
#   And stderr does not contain "RUST_BACKTRACE"
#   And stderr contains a human-readable message
#
# Scenario: User points to a path that is not a git repository
#   When I run git-prism manifest HEAD~1..HEAD --repo /tmp/not-a-repo
#   Then the exit code is nonzero
#   And stderr mentions "repository" or "repo"

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "$SCRIPT_DIR/helpers.sh"

echo "test_cli_errors.sh"

ensure_built

repo_path=$(create_test_repo)
trap cleanup_test_repos EXIT

# --- invalid ref range ---

stderr_file=$(mktemp)
exit_code=0
"$BINARY" manifest "nonexistent-branch..HEAD" --repo "$repo_path" > /dev/null 2>"$stderr_file" || exit_code=$?
stderr_output=$(cat "$stderr_file")
rm -f "$stderr_file"

# The exit code must be nonzero for a bad ref
assert_exit_code_nonzero "$exit_code" "invalid ref: exit code is nonzero"
assert_not_contains "$stderr_output" "panicked" "invalid ref: no panic in output"
assert_not_contains "$stderr_output" "RUST_BACKTRACE" "invalid ref: no RUST_BACKTRACE in output"
# stderr should have some text (a human-readable message)
if [[ -n "$stderr_output" ]]; then
  pass "invalid ref: stderr contains a message"
else
  fail "invalid ref: stderr contains a message (stderr was empty)"
fi

# --- not a git repo ---

not_a_repo=$(mktemp -d)
stderr_file=$(mktemp)
exit_code=0
"$BINARY" manifest "HEAD~1..HEAD" --repo "$not_a_repo" > /dev/null 2>"$stderr_file" || exit_code=$?
stderr_output=$(cat "$stderr_file")
rm -f "$stderr_file"
rm -rf "$not_a_repo"

assert_exit_code_nonzero "$exit_code" "not-a-repo: exit code is nonzero"
# The error should mention repository/repo in some form
if echo "$stderr_output" | grep -qiE 'repositor|repo'; then
  pass "not-a-repo: error mentions repository"
else
  fail "not-a-repo: error mentions repository (got: $stderr_output)"
fi

print_summary
exit_with_status
