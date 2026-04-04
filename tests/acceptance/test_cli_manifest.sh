#!/usr/bin/env bash
# @ISSUE-15: git-prism manifest produces valid JSON with expected structure
#
# Scenario: User generates a change manifest for a commit range
#   Given a git repo with 2 commits (one adding a .rs file, one modifying it)
#   When I run git-prism manifest HEAD~1..HEAD --repo <path>
#   Then the exit code is 0
#   And the output is valid JSON
#   And the JSON has metadata, summary, and files keys
#   And summary.total_files_changed > 0
#   And at least one file has functions_changed (not null)

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "$SCRIPT_DIR/helpers.sh"

echo "test_cli_manifest.sh"

ensure_built

repo_path=$(create_test_repo)
trap cleanup_test_repos EXIT

output=$("$BINARY" manifest "HEAD~1..HEAD" --repo "$repo_path" 2>&1)
exit_code=$?

assert_exit_code 0 "$exit_code" "manifest exits with code 0"
assert_valid_json "$output" "manifest output is valid JSON"
assert_json_has_key "$output" "metadata" "JSON has 'metadata' key"
assert_json_has_key "$output" "summary" "JSON has 'summary' key"
assert_json_has_key "$output" "files" "JSON has 'files' key"
assert_json_value_gt "$output" "summary.total_files_changed" 0 "summary.total_files_changed > 0"
assert_json_value_not_null "$output" "files.0.functions_changed" "first file has functions_changed (not null)"

print_summary
exit_with_status
