#!/usr/bin/env bash
set -euo pipefail

# Capstone demo for git-prism v0.7.0 redirect-hook epic (issue #242).
#
# Three segments:
#   1. Before — a direct shell redirect silently clobbers a tracked file.
#   2. How the hook works — drive the bundled tokenizer over its stdin/stdout
#      protocol to show advisory + block decisions, then run the install
#      ceremony.
#   3. review_change vs git diff — contrast raw porcelain text with the
#      structured manifest + context output git-prism produces for agents.
#
# The script is idempotent: running it twice from a fresh checkout produces
# the same output. Sandbox repos are created in $TMPDIR via mktemp -d and
# cleaned up via trap.
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

# Resolve the repo root from this script's location so the demo runs from
# any working directory — the existing demo/demo.sh hard-codes the path,
# but locating the repo dynamically keeps this script idempotent across
# checkouts.
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

# === SEGMENT 1: BEFORE — THE PROBLEM ===
echo -e "${BLUE}╔══════════════════════════════════════════╗${RESET}"
echo -e "${BLUE}║${RESET}  ${CYAN}Segment 1: Before — the problem${RESET}        ${BLUE}║${RESET}"
echo -e "${BLUE}╚══════════════════════════════════════════╝${RESET}\n"

SANDBOX="$(mktemp -d)"
trap 'rm -rf "$SANDBOX"' EXIT

(
    cd "$SANDBOX"
    git init -q
    git config user.email "demo@example.com"
    git config user.name "Demo"
    cat > notes.md <<'EOF'
# Project notes

Important research findings the agent should NOT lose.
EOF
    git add notes.md
    git commit -q -m "add notes"
)

echo -e "${DIM}# A tracked file with content the agent must preserve.${RESET}"
type_cmd "cat notes.md"
cat "$SANDBOX/notes.md"
echo ""
sleep 12.0  # problem_intro: 12.4s

echo -e "${DIM}# A bash redirect issued in an agentic session — no warning, no diff.${RESET}"
type_cmd 'echo "overwritten content" > notes.md'
(cd "$SANDBOX" && echo "overwritten content" > notes.md)
sleep 0.5

echo -e "${DIM}# The original content is gone.${RESET}"
type_cmd "cat notes.md"
cat "$SANDBOX/notes.md"
echo -e "\n${RED}✗ Tracked file silently clobbered. No prompt, no recovery path.${RESET}\n"
sleep 15.0  # problem_clobber: 16.2s

# === SEGMENT 2: HOW THE HOOK WORKS ===
echo -e "${BLUE}╔══════════════════════════════════════════╗${RESET}"
echo -e "${BLUE}║${RESET}  ${CYAN}Segment 2: How the hook works${RESET}          ${BLUE}║${RESET}"
echo -e "${BLUE}╚══════════════════════════════════════════╝${RESET}\n"

echo -e "${DIM}# The hook is a Claude Code PreToolUse program. It reads a JSON${RESET}"
echo -e "${DIM}# payload on stdin and decides via exit code: 0 allow, 0+JSON${RESET}"
echo -e "${DIM}# advisory, 2 block. Drive it directly to see each state.${RESET}\n"
sleep 14.3  # hook_intro: 14.6s

echo -e "${YELLOW}# Block: gh pr diff returns raw text an agent can't use — exit 2 stops it cold.${RESET}"
type_cmd 'echo {...gh pr diff 123...} | python3 hooks/bash_redirect_hook.py'
set +e
echo '{"tool_name":"Bash","tool_input":{"command":"gh pr diff 123"}}' \
    | python3 "$HOOK_PYTHON"
BLOCK_EXIT=$?
set -e
echo -e "${RED}exit code: $BLOCK_EXIT${RESET}\n"
sleep 12.0  # hook_block: 12.2s

echo -e "${YELLOW}# Advisory: git diff redirected to get_change_manifest — exit 0, no interruption.${RESET}"
type_cmd 'echo {...git diff main..HEAD...} | python3 hooks/bash_redirect_hook.py'
set +e
echo '{"tool_name":"Bash","tool_input":{"command":"git diff main..HEAD"}}' \
    | python3 "$HOOK_PYTHON"
ADVISORY_EXIT=$?
set -e
echo -e "${GREEN}exit code: $ADVISORY_EXIT${RESET}\n"
sleep 12.5  # hook_advisory: 12.8s

echo -e "${YELLOW}# Silent: a benign command — no output, exit 0.${RESET}"
type_cmd 'echo {...echo hello world...} | python3 hooks/bash_redirect_hook.py'
set +e
echo '{"tool_name":"Bash","tool_input":{"command":"echo hello world"}}' \
    | python3 "$HOOK_PYTHON"
SILENT_EXIT=$?
set -e
echo -e "${GREEN}exit code: $SILENT_EXIT (no stdout/stderr)${RESET}\n"
sleep 12.0  # hook_silent: 12.3s

echo -e "${BLUE}--- Install ceremony ---${RESET}\n"
echo -e "${DIM}# git-prism ships the install command itself — one shot, idempotent.${RESET}"
type_cmd "git-prism hooks status"
"$BINARY" hooks status || true  # non-zero expected when hook is not yet installed; demo continues
echo ""
sleep 1.5  # intentional pause after status output

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
sleep 2.5  # intentional pause: let viewer read the JSON

echo -e "${DIM}# Drop --dry-run to write ~/.claude/settings.json + copy hook scripts${RESET}"
echo -e "${DIM}# into ~/.claude/hooks/. Default scope is user (subagent compatibility).${RESET}\n"
sleep 8.0  # hook_install remainder: 16.9s total

# === SEGMENT 3: review_change vs git diff ===
echo -e "${BLUE}╔══════════════════════════════════════════╗${RESET}"
echo -e "${BLUE}║${RESET}  ${CYAN}Segment 3: review_change vs git diff${RESET}   ${BLUE}║${RESET}"
echo -e "${BLUE}╚══════════════════════════════════════════╝${RESET}\n"

echo -e "${DIM}# Inside git-prism's own repo. First the porcelain agents reach for.${RESET}"
type_cmd "git diff HEAD~1..HEAD"
(cd "$REPO_ROOT" && git --no-pager diff HEAD~1..HEAD | head -25) || true  # repo may have only one commit in a fresh checkout
echo -e "${DIM}  ... (truncated for demo) ...${RESET}\n"
sleep 15.0  # git_diff_problem: 16.4s

echo -e "${DIM}# Same change, structured per-file metadata — no @@ hunks, no +/- noise.${RESET}"
type_cmd "git-prism manifest HEAD~1..HEAD"
(cd "$REPO_ROOT" && "$BINARY" manifest HEAD~1..HEAD 2>/dev/null \
    | python3 -m json.tool | head -30) || true  # tolerate single-commit repos in demo environments
echo -e "${DIM}  ... (truncated for demo) ...${RESET}\n"
sleep 17.0  # review_change: 18.8s

echo -e "${DIM}# Function context: callers, callees, blast radius. The MCP tool${RESET}"
echo -e "${DIM}# review_change combines manifest + context in one call.${RESET}"
type_cmd "git-prism context HEAD~1..HEAD"
(cd "$REPO_ROOT" && "$BINARY" context HEAD~1..HEAD 2>/dev/null \
    | python3 -m json.tool | head -25) || true  # tolerate single-commit repos in demo environments
echo -e "${DIM}  ... (truncated for demo) ...${RESET}\n"
sleep 7.0  # closing first portion

echo -e "${BLUE}╔══════════════════════════════════════════╗${RESET}"
echo -e "${BLUE}║${RESET}  ${GREEN}git-prism v0.7.0${RESET} — bundled redirect hook ${BLUE}║${RESET}"
echo -e "${BLUE}║${RESET}  ${CYAN}github.com/mikelane/git-prism${RESET}            ${BLUE}║${RESET}"
echo -e "${BLUE}╚══════════════════════════════════════════╝${RESET}\n"
sleep 10.0  # closing: 14.6s — extra buffer ensures video outlasts 147.3s audio
