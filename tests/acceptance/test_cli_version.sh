#!/usr/bin/env bash
# @ISSUE-19: git-prism --version outputs version string
#
# Scenario: User checks the installed version
#   Given git-prism is built
#   When I run git-prism --version
#   Then the output contains "git-prism"
#   And the output contains a semver version number

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "$SCRIPT_DIR/helpers.sh"

echo "test_cli_version.sh"

ensure_built

output=$("$BINARY" --version 2>&1) || true

assert_contains "$output" "git-prism" "output contains 'git-prism'"
# semver pattern: digits.digits.digits
if echo "$output" | grep -qE '[0-9]+\.[0-9]+\.[0-9]+'; then
  pass "output contains semver version number"
else
  fail "output contains semver version number"
fi

print_summary
exit_with_status
