#!/usr/bin/env bash
# Shared test utilities for git-prism acceptance tests.
# Source this file from each test script.

set -euo pipefail

# ---------- state ----------
FAILURES=0
PASSES=0
TEST_REPOS=()

# ---------- project paths ----------
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
BINARY="$PROJECT_ROOT/target/release/git-prism"

# ---------- colors ----------
if [[ -t 1 ]]; then
  GREEN=$'\033[0;32m'
  RED=$'\033[0;31m'
  RESET=$'\033[0m'
else
  GREEN=""
  RED=""
  RESET=""
fi

pass() {
  PASSES=$((PASSES + 1))
  echo "  ${GREEN}pass${RESET} $1"
}

fail() {
  FAILURES=$((FAILURES + 1))
  echo "  ${RED}FAIL${RESET} $1"
}

# ---------- assertions ----------

assert_exit_code() {
  local expected="$1"
  local actual="$2"
  local label="${3:-exit code is $expected}"
  if [[ "$actual" -eq "$expected" ]]; then
    pass "$label"
  else
    fail "$label (expected $expected, got $actual)"
  fi
}

assert_exit_code_nonzero() {
  local actual="$1"
  local label="${2:-exit code is nonzero}"
  if [[ "$actual" -ne 0 ]]; then
    pass "$label"
  else
    fail "$label (expected nonzero, got 0)"
  fi
}

assert_contains() {
  local haystack="$1"
  local needle="$2"
  local label="${3:-output contains '$needle'}"
  if echo "$haystack" | grep -qi "$needle"; then
    pass "$label"
  else
    fail "$label"
  fi
}

assert_not_contains() {
  local haystack="$1"
  local needle="$2"
  local label="${3:-output does not contain '$needle'}"
  if echo "$haystack" | grep -qi "$needle"; then
    fail "$label"
  else
    pass "$label"
  fi
}

assert_valid_json() {
  local text="$1"
  local label="${2:-output is valid JSON}"
  if echo "$text" | python3 -m json.tool > /dev/null 2>&1; then
    pass "$label"
  else
    fail "$label"
  fi
}

assert_json_has_key() {
  local json="$1"
  local key="$2"
  local label="${3:-JSON has key '$key'}"
  if echo "$json" | python3 -c "
import sys, json
data = json.load(sys.stdin)
parts = '$key'.split('.')
obj = data
for p in parts:
    if p.isdigit():
        obj = obj[int(p)]
    else:
        obj = obj[p]
" 2>/dev/null; then
    pass "$label"
  else
    fail "$label"
  fi
}

assert_json_value_gt() {
  local json="$1"
  local key="$2"
  local threshold="$3"
  local label="${4:-JSON '$key' > $threshold}"
  local val
  val=$(echo "$json" | python3 -c "
import sys, json
data = json.load(sys.stdin)
parts = '$key'.split('.')
obj = data
for p in parts:
    if p.isdigit():
        obj = obj[int(p)]
    else:
        obj = obj[p]
print(obj)
" 2>/dev/null) || true
  if [[ -n "$val" ]] && python3 -c "exit(0 if $val > $threshold else 1)" 2>/dev/null; then
    pass "$label"
  else
    fail "$label (got ${val:-<missing>})"
  fi
}

assert_json_value_not_null() {
  local json="$1"
  local key="$2"
  local label="${3:-JSON '$key' is not null}"
  local val
  val=$(echo "$json" | python3 -c "
import sys, json
data = json.load(sys.stdin)
parts = '$key'.split('.')
obj = data
for p in parts:
    if p.isdigit():
        obj = obj[int(p)]
    else:
        obj = obj[p]
print('NULL' if obj is None else 'OK')
" 2>/dev/null) || true
  if [[ "$val" == "OK" ]]; then
    pass "$label"
  else
    fail "$label"
  fi
}

assert_file_exists() {
  local path="$1"
  local label="${2:-file exists: $path}"
  if [[ -f "$path" ]]; then
    pass "$label"
  else
    fail "$label"
  fi
}

# ---------- test repo setup ----------

create_test_repo() {
  local tmp
  tmp=$(mktemp -d)
  TEST_REPOS+=("$tmp")

  git -C "$tmp" init --quiet
  git -C "$tmp" config user.email "test@test.com"
  git -C "$tmp" config user.name "Test"

  # First commit: add a Rust file
  cat > "$tmp/main.rs" <<'RUST'
fn greet(name: &str) -> String {
    format!("Hello, {}!", name)
}

fn main() {
    println!("{}", greet("world"));
}
RUST
  git -C "$tmp" add main.rs
  git -C "$tmp" commit --quiet -m "initial commit"

  # Second commit: modify the Rust file
  cat > "$tmp/main.rs" <<'RUST'
fn greet(name: &str) -> String {
    format!("Hello, {}!", name)
}

fn farewell(name: &str) -> String {
    format!("Goodbye, {}!", name)
}

fn main() {
    println!("{}", greet("world"));
    println!("{}", farewell("world"));
}
RUST
  git -C "$tmp" add main.rs
  git -C "$tmp" commit --quiet -m "add farewell function"

  echo "$tmp"
}

cleanup_test_repos() {
  for repo in "${TEST_REPOS[@]}"; do
    rm -rf "$repo"
  done
  TEST_REPOS=()
}

# ---------- build ----------

ensure_built() {
  if [[ ! -x "$BINARY" ]]; then
    echo "  Building git-prism (release)..."
    cargo build --release --manifest-path "$PROJECT_ROOT/Cargo.toml" 2>&1
  fi
}

# ---------- summary ----------

print_summary() {
  echo "$PASSES passed, $FAILURES failed"
}

exit_with_status() {
  if [[ "$FAILURES" -gt 0 ]]; then
    exit 1
  fi
  exit 0
}
