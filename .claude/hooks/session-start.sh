#!/bin/bash
set -euo pipefail

# Only run in remote (web) environments
if [ "${CLAUDE_CODE_REMOTE:-}" != "true" ]; then
  exit 0
fi

# Install BDD Python dependencies (behave, grpcio, opentelemetry-proto)
pip install -r "$CLAUDE_PROJECT_DIR/bdd/requirements.txt" --quiet 2>/dev/null

# Install lefthook for git hooks
if ! command -v lefthook &>/dev/null; then
  curl -fsSL https://get.lefthook.com | sh -s -- -b /usr/local/bin 2>/dev/null
fi

# Install lefthook git hooks in the repo
cd "$CLAUDE_PROJECT_DIR"
lefthook install 2>/dev/null || true

# Build release binary (needed for BDD tests)
cargo build --release --quiet 2>/dev/null
