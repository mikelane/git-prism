#!/usr/bin/env bash
# @ISSUE-17: README.md exists with required sections for distribution
#
# Scenario: A user discovers the project on GitHub
#   Then README.md exists in the repo root
#   And it contains "Installation" or "Install" (case insensitive)
#   And it contains "get_change_manifest"
#   And it contains "get_file_snapshots"
#   And it contains "claude mcp add"

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "$SCRIPT_DIR/helpers.sh"

echo "test_readme.sh"

readme="$PROJECT_ROOT/README.md"

assert_file_exists "$readme" "README.md exists in repo root"

if [[ -f "$readme" ]]; then
  content=$(cat "$readme")
  assert_contains "$content" "install" "README contains 'Installation' or 'Install'"
  assert_contains "$content" "get_change_manifest" "README contains 'get_change_manifest'"
  assert_contains "$content" "get_file_snapshots" "README contains 'get_file_snapshots'"
  assert_contains "$content" "claude mcp add" "README contains 'claude mcp add'"
else
  fail "README contains 'Installation' or 'Install' (file missing)"
  fail "README contains 'get_change_manifest' (file missing)"
  fail "README contains 'get_file_snapshots' (file missing)"
  fail "README contains 'claude mcp add' (file missing)"
fi

print_summary
exit_with_status
