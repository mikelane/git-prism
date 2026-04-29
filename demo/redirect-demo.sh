#!/usr/bin/env bash
set -euo pipefail

# Capstone demo for git-prism v0.7.0 redirect-hook epic (issue #242).
#
# Three segments:
#   1. The training problem — Claude's git muscle memory overrides MCP registration.
#   2. How the hook works — drive the tokenizer over stdin/stdout, then install.
#   3. The payoff — structured manifest + context output replacing raw diffs.
#
# Sleep values are calibrated from demo/recordings/v0.7.0/redirect-epic_timing.json
# so each segment stays in sync with the narration audio track.

# Colors (mirrors demo/demo.sh).
BLUE='\033[1;34m'
GREEN='\033[1;32m'
CYAN='\033[1;36m'
RED='\033[1;31m'
YELLOW='\033[1;33m'
DIM='\033[2m'
RESET='\033[0m'

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
BINARY="$REPO_ROOT/target/release/git-prism"
HOOK_PYTHON="$REPO_ROOT/hooks/bash_redirect_hook.py"

type_cmd() {
    echo -ne "${GREEN}\$ ${RESET}"
    if command -v pv >/dev/null 2>&1; then
        echo -n "$1" | pv -qL 40
    else
        echo -n "$1"
    fi
    echo ""
    sleep 0.3
}

# Binary must be pre-built before recording.  Build with: cargo build --release
if [[ ! -x "$BINARY" ]]; then
    echo -e "${RED}✗ Binary not found at $BINARY — run: cargo build --release${RESET}" >&2
    exit 1
fi
if [[ ! -f "$HOOK_PYTHON" ]]; then
    echo -e "${RED}✗ Hook not found at $HOOK_PYTHON — are you on the right branch?${RESET}" >&2
    exit 1
fi
echo -e "${GREEN}✓ $("$BINARY" --version 2>/dev/null)${RESET}"
echo ""

# === SEGMENT 1: THE TRAINING PROBLEM ===
echo -e "${BLUE}╔══════════════════════════════════════════╗${RESET}"
echo -e "${BLUE}║${RESET}  ${CYAN}Segment 1: The training problem${RESET}        ${BLUE}║${RESET}"
echo -e "${BLUE}╚══════════════════════════════════════════╝${RESET}\n"

echo -e "${DIM}# git-prism is registered as an MCP server. Five structured tools.${RESET}"
echo -e "${DIM}# But Claude was trained on millions of git commands.${RESET}"
echo -e "${DIM}# That muscle memory does not go away just because an MCP server is installed.${RESET}\n"
sleep 16.4  # hook_intro: 18.7s - 2.3s display

echo -e "${DIM}# Claude reaches for git diff — exactly as its training taught it.${RESET}"
type_cmd "git diff HEAD~1..HEAD"
(cd "$REPO_ROOT" && git --no-pager diff HEAD~1..HEAD | head -25) || true  # repo may have only one commit in a fresh checkout
echo -e "${DIM}  ... (truncated for demo) ...${RESET}"
echo -e "\n${DIM}# Hunk headers, plus and minus prefixes — for humans, not agents.${RESET}"
echo -e "${DIM}# Five structured tools, one tool call away. Unused.${RESET}\n"
sleep 16.9  # git_diff_problem: 17.9s - 1s display

# === SEGMENT 2: HOW THE HOOK WORKS ===
echo -e "${BLUE}╔══════════════════════════════════════════╗${RESET}"
echo -e "${BLUE}║${RESET}  ${CYAN}Segment 2: How the hook works${RESET}          ${BLUE}║${RESET}"
echo -e "${BLUE}╚══════════════════════════════════════════╝${RESET}\n"

echo -e "${DIM}# The hook is a Claude Code PreToolUse program. It reads a JSON${RESET}"
echo -e "${DIM}# payload on stdin and decides via exit code: 0 allow, 0+JSON${RESET}"
echo -e "${DIM}# advisory, 2 block.${RESET}\n"

echo -e "${YELLOW}# Block: gh pr diff returns raw text — exit 2 stops it cold.${RESET}"
type_cmd 'echo {...gh pr diff 123...} | python3 hooks/bash_redirect_hook.py'
set +e
echo '{"tool_name":"Bash","tool_input":{"command":"gh pr diff 123"}}' \
    | python3 "$HOOK_PYTHON"
BLOCK_EXIT=$?
set -e
echo -e "${RED}exit code: $BLOCK_EXIT${RESET}\n"
sleep 9.2  # hook_block: 12.2s - 3s display

echo -e "${YELLOW}# Advisory: git diff gets a nudge toward get_change_manifest — exit 0.${RESET}"
type_cmd 'echo {...git diff main..HEAD...} | python3 hooks/bash_redirect_hook.py'
set +e
echo '{"tool_name":"Bash","tool_input":{"command":"git diff main..HEAD"}}' \
    | python3 "$HOOK_PYTHON"
ADVISORY_EXIT=$?
set -e
echo -e "${GREEN}exit code: $ADVISORY_EXIT${RESET}\n"
sleep 10.8  # hook_advisory: 13.8s - 3s display

echo -e "${YELLOW}# Silent: a benign command — no output, exit 0.${RESET}"
type_cmd 'echo {...echo hello world...} | python3 hooks/bash_redirect_hook.py'
set +e
echo '{"tool_name":"Bash","tool_input":{"command":"echo hello world"}}' \
    | python3 "$HOOK_PYTHON"
SILENT_EXIT=$?
set -e
echo -e "${GREEN}exit code: $SILENT_EXIT (no stdout/stderr)${RESET}\n"
sleep 9.3  # hook_silent: 12.3s - 3s display

echo -e "${BLUE}--- Install ceremony ---${RESET}\n"
echo -e "${DIM}# git-prism ships the install command — one shot, idempotent.${RESET}"
type_cmd "git-prism hooks status"
"$BINARY" hooks status || true  # non-zero expected when hook is not yet installed; demo continues
echo ""
sleep 1.5

type_cmd "git-prism hooks install --scope user --dry-run  # filter to git-prism entries"
echo -e "${DIM}# Dry-run prints the full merged settings.json. Filtering down to${RESET}"
echo -e "${DIM}# just the git-prism redirect entries the install would add.${RESET}"
"$BINARY" hooks install --scope user --dry-run \
    | python3 -c '
import json, sys
data = json.load(sys.stdin)
matchers = data.get("hooks", {}).get("PreToolUse", [])
prism_hooks = []
for entry in matchers:
    matcher = entry.get("matcher")
    for h in entry.get("hooks", []):
        if h.get("command", "").endswith("git-prism-redirect.sh"):
            prism_hooks.append({"matcher": matcher, **h})
print(json.dumps(prism_hooks, indent=2))
'
echo ""
sleep 2.5

echo -e "${DIM}# Drop --dry-run to write ~/.claude/settings.json + copy hook scripts${RESET}"
echo -e "${DIM}# into ~/.claude/hooks/. Default scope is user (subagent compatibility).${RESET}\n"
sleep 8.6  # hook_install: 17.2s - 8.6s display

# === SEGMENT 3: STRUCTURED PAYOFF ===
echo -e "${BLUE}╔══════════════════════════════════════════╗${RESET}"
echo -e "${BLUE}║${RESET}  ${CYAN}Segment 3: The structured payoff${RESET}       ${BLUE}║${RESET}"
echo -e "${BLUE}╚══════════════════════════════════════════╝${RESET}\n"

echo -e "${DIM}# Same change range. Instead of raw diff, structured per-file metadata.${RESET}"
type_cmd "git-prism manifest HEAD~1..HEAD"
(cd "$REPO_ROOT" && "$BINARY" manifest HEAD~1..HEAD 2>/dev/null \
    | python3 -m json.tool | head -30) || true  # tolerate single-commit repos in demo environments
echo -e "${DIM}  ... (truncated for demo) ...${RESET}\n"
sleep 17.7  # review_change: 20.7s - 3s display

echo -e "${DIM}# git-prism context adds callers, callees, and blast-radius risk per function.${RESET}"
echo -e "${DIM}# The MCP tool review_change combines manifest + context in one call.${RESET}"
type_cmd "git-prism context HEAD~1..HEAD"
(cd "$REPO_ROOT" && "$BINARY" context HEAD~1..HEAD 2>/dev/null \
    | python3 -m json.tool | head -25) || true  # tolerate single-commit repos in demo environments
echo -e "${DIM}  ... (truncated for demo) ...${RESET}\n"
sleep 4.0  # closing first portion: context output reading time

echo -e "${BLUE}╔══════════════════════════════════════════╗${RESET}"
echo -e "${BLUE}║${RESET}  ${GREEN}git-prism v0.7.0${RESET} — bundled redirect hook ${BLUE}║${RESET}"
echo -e "${BLUE}║${RESET}  ${CYAN}github.com/mikelane/git-prism${RESET}            ${BLUE}║${RESET}"
echo -e "${BLUE}╚══════════════════════════════════════════╝${RESET}\n"
sleep 11.0  # closing: 16.9s - 5.8s display + 4s buffer
