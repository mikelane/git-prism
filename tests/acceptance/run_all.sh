#!/usr/bin/env bash
# Runner for all git-prism acceptance tests.
# Executes each test script and collects pass/fail results.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

TOTAL_PASS=0
TOTAL_FAIL=0
SUITE_FAILURES=()

echo "=== git-prism Acceptance Tests ==="
echo ""

for test_script in "$SCRIPT_DIR"/test_*.sh; do
  test_name=$(basename "$test_script")

  # Run the test and capture its output
  test_output=$("$test_script" 2>&1) || true

  # Extract the summary line (last line of output: "N passed, M failed")
  summary_line=$(echo "$test_output" | tail -1)

  # Parse pass/fail counts from the summary
  passed=$(echo "$summary_line" | grep -oE '^[0-9]+' || echo "0")
  failed=$(echo "$summary_line" | grep -oE '[0-9]+ failed' | grep -oE '[0-9]+' || echo "0")

  TOTAL_PASS=$((TOTAL_PASS + passed))
  TOTAL_FAIL=$((TOTAL_FAIL + failed))

  if [[ "$failed" -gt 0 ]]; then
    SUITE_FAILURES+=("$test_name")
  fi

  # Print each test's full output
  echo "$test_output"
  echo ""
done

echo "=== Summary: $TOTAL_PASS passed, $TOTAL_FAIL failed ==="

if [[ ${#SUITE_FAILURES[@]} -gt 0 ]]; then
  echo ""
  echo "Failing suites:"
  for s in "${SUITE_FAILURES[@]}"; do
    echo "  - $s"
  done
  exit 1
fi

exit 0
