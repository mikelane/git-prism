#!/usr/bin/env bash
# @ISSUE-15: git-prism languages lists all supported languages
#
# Scenario: User checks which languages support function-level analysis
#   When I run git-prism languages
#   Then the output contains "go"
#   And the output contains "python"
#   And the output contains "typescript"
#   And the output contains "javascript"
#   And the output contains "rust"

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "$SCRIPT_DIR/helpers.sh"

echo "test_cli_languages.sh"

ensure_built

output=$("$BINARY" languages 2>&1)
exit_code=$?

assert_exit_code 0 "$exit_code" "languages exits with code 0"
assert_contains "$output" "go" "output contains 'go'"
assert_contains "$output" "python" "output contains 'python'"
assert_contains "$output" "typescript" "output contains 'typescript'"
assert_contains "$output" "javascript" "output contains 'javascript'"
assert_contains "$output" "rust" "output contains 'rust'"

print_summary
exit_with_status
