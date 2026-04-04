#!/usr/bin/env bash
# @ISSUE-15: git-prism snapshot produces valid JSON with before/after content
#
# Scenario: User retrieves file snapshots for a commit range
#   Given a git repo with 2 commits modifying main.rs
#   When I run git-prism snapshot HEAD~1..HEAD --paths main.rs --repo <path>
#   Then the exit code is 0
#   And the output is valid JSON
#   And files[0].before exists
#   And files[0].after exists
#   And token_estimate > 0

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "$SCRIPT_DIR/helpers.sh"

echo "test_cli_snapshot.sh"

ensure_built

repo_path=$(create_test_repo)
trap cleanup_test_repos EXIT

output=$("$BINARY" snapshot "HEAD~1..HEAD" --paths main.rs --repo "$repo_path" 2>&1)
exit_code=$?

assert_exit_code 0 "$exit_code" "snapshot exits with code 0"
assert_valid_json "$output" "snapshot output is valid JSON"
assert_json_has_key "$output" "files.0.before" "files[0].before exists"
assert_json_has_key "$output" "files.0.after" "files[0].after exists"
assert_json_value_gt "$output" "token_estimate" 0 "token_estimate > 0"

print_summary
exit_with_status
