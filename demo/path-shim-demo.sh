#!/usr/bin/env bash
set -euo pipefail

# Capstone demo for the git-prism PATH-shim epic (issue #291, epic #284).
#
# Five scenes:
#   1. Install   — git-prism hooks install --path-shim, symlink + status
#   2. Intercept — agent shell (CLAUDECODE=1) → structured JSON manifest
#   3. Vanilla   — clean human shell → real `diff --git` output
#   4. Sentinel  — GIT_PRISM_INSIDE_SHIM + resolver skip prevent self-exec loops
#   5. Uninstall — git-prism hooks uninstall --path-shim, status
#
# Runs the REAL shim against an isolated temp HOME so it never touches the
# user's real ~/.local/share/git-prism. Sleep values are calibrated from
# demo/recordings/path-shim_timing.json to stay in sync with the narration.

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

# Isolated HOME so install/uninstall never touch the real environment.
DEMO_HOME="$(mktemp -d)"
SHIM_DIR="$DEMO_HOME/.local/share/git-prism/bin"
# A clean PATH that contains a real git but NOT the shim dir.
REAL_PATH="/opt/homebrew/bin:/usr/bin:/bin:/usr/sbin:/sbin"

cleanup() { rm -rf "$DEMO_HOME"; }
trap cleanup EXIT

type_cmd() {
    echo -ne "${GREEN}\$ ${RESET}"
    if command -v pv >/dev/null 2>&1; then echo -n "$1" | pv -qL 55; else echo -n "$1"; fi
    echo ""
    sleep 0.3
}

if [[ ! -x "$BINARY" ]]; then
    echo -e "${RED}✗ Binary not found at $BINARY — run: cargo build --release${RESET}" >&2
    exit 1
fi
echo -e "${GREEN}✓ $("$BINARY" --version 2>/dev/null)${RESET}"
echo ""

# === SCENE 0/title ===
echo -e "${BLUE}╔════════════════════════════════════════════════╗${RESET}"
echo -e "${BLUE}║${RESET}  ${CYAN}git-prism PATH shim — one git, two audiences${RESET}  ${BLUE}║${RESET}"
echo -e "${BLUE}╚════════════════════════════════════════════════╝${RESET}\n"
echo -e "${DIM}# The redirect hook nudges agents after the fact.${RESET}"
echo -e "${DIM}# The PATH shim wraps git itself: structured data for agents,${RESET}"
echo -e "${DIM}# vanilla git for humans — from the same command.${RESET}\n"
sleep 11.7  # title: 12.0s - ~1.5s display

# === SCENE 1: INSTALL ===
echo -e "${BLUE}--- Scene 1: Install ---${RESET}\n"
type_cmd "git-prism hooks install --path-shim"
HOME="$DEMO_HOME" "$BINARY" hooks install --path-shim
echo ""
type_cmd "ls -l ~/.local/share/git-prism/bin/git"
ls -l "$SHIM_DIR/git" | sed "s|$DEMO_HOME|~|g"
echo ""
type_cmd "git-prism hooks status   # (path-shim row)"
HOME="$DEMO_HOME" "$BINARY" hooks status 2>/dev/null | grep -i "shim\|symlink" || HOME="$DEMO_HOME" "$BINARY" hooks status 2>/dev/null | tail -3
echo ""
sleep 13.7  # install: 16.2s - ~6.7s display

# === SCENE 2: AGENT INTERCEPT ===
echo -e "${BLUE}--- Scene 2: Agent shell → structured JSON ---${RESET}\n"
echo -e "${DIM}# CLAUDECODE=1 marks an agent. The shim classifies the command${RESET}"
echo -e "${DIM}# and returns a manifest instead of a human diff.${RESET}"
type_cmd "CLAUDECODE=1 git diff HEAD~1..HEAD"
( cd "$REPO_ROOT" && env -u CI CLAUDECODE=1 PATH="$SHIM_DIR:$REAL_PATH" \
    "$SHIM_DIR/git" diff HEAD~1..HEAD 2>/dev/null | python3 -m json.tool 2>/dev/null | head -22 ) || true
echo -e "${DIM}  ... (truncated for demo) ...${RESET}\n"
sleep 15.0  # intercept: 17.0s - ~6s display

# === SCENE 3: VANILLA FOR HUMANS ===
echo -e "${BLUE}--- Scene 3: Clean human shell → vanilla git ---${RESET}\n"
echo -e "${DIM}# No agent markers. The shim steps aside and exec's real git.${RESET}"
type_cmd "git diff HEAD~1..HEAD   # (no agent env)"
( cd "$REPO_ROOT" && env -i HOME="$HOME" TERM="${TERM:-xterm}" PATH="$SHIM_DIR:$REAL_PATH" \
    "$SHIM_DIR/git" --no-pager diff HEAD~1..HEAD 2>/dev/null | head -16 ) || true
echo -e "${DIM}  ... (truncated for demo) ...${RESET}\n"
sleep 13.1  # vanilla: 14.6s - ~5.6s display

# === SCENE 4: SENTINEL + RESOLVER (no loops) ===
echo -e "${BLUE}--- Scene 4: No self-exec loops ---${RESET}\n"
echo -e "${YELLOW}# Sentinel: GIT_PRISM_INSIDE_SHIM=1 forces passthrough even for an agent.${RESET}"
type_cmd "GIT_PRISM_INSIDE_SHIM=1 CLAUDECODE=1 git diff HEAD~1..HEAD"
( cd "$REPO_ROOT" && env -u CI GIT_PRISM_INSIDE_SHIM=1 CLAUDECODE=1 PATH="$SHIM_DIR:$REAL_PATH" \
    "$SHIM_DIR/git" --no-pager diff HEAD~1..HEAD 2>/dev/null | head -5 ) || true
echo -e "${DIM}  → vanilla output, sentinel honored.${RESET}\n"
echo -e "${YELLOW}# Resolver: skips its own dir; GIT_PRISM_DEBUG_RESOLVER shows the real git.${RESET}"
type_cmd "GIT_PRISM_DEBUG_RESOLVER=1 git status   # (stderr)"
( cd "$REPO_ROOT" && env -i HOME="$HOME" TERM="${TERM:-xterm}" GIT_PRISM_DEBUG_RESOLVER=1 \
    PATH="$SHIM_DIR:$REAL_PATH" "$SHIM_DIR/git" status >/dev/null 2>/tmp/ps_resolver.txt ) || true
grep "resolved real git" /tmp/ps_resolver.txt | sed "s|$SHIM_DIR|<shim-dir>|g" || true
echo ""
sleep 16.1  # sentinel: 18.6s - ~7.1s display

# === SCENE 5: UNINSTALL ===
echo -e "${BLUE}--- Scene 5: Uninstall ---${RESET}\n"
type_cmd "git-prism hooks uninstall --path-shim"
HOME="$DEMO_HOME" "$BINARY" hooks uninstall --path-shim
echo ""
type_cmd "git-prism hooks status   # shim gone"
HOME="$DEMO_HOME" "$BINARY" hooks status 2>/dev/null | grep -i "shim\|symlink" || echo -e "${DIM}  (no path-shim installed)${RESET}"
echo ""
sleep 9.9  # uninstall: 11.9s - ~4.9s display

# === CLOSING ===
echo -e "${BLUE}╔════════════════════════════════════════════════╗${RESET}"
echo -e "${BLUE}║${RESET}  ${GREEN}git-prism PATH shim${RESET} — agents get JSON,         ${BLUE}║${RESET}"
echo -e "${BLUE}║${RESET}  humans get vanilla git, no loops.             ${BLUE}║${RESET}"
echo -e "${BLUE}║${RESET}  ${CYAN}github.com/mikelane/git-prism${RESET}                 ${BLUE}║${RESET}"
echo -e "${BLUE}╚════════════════════════════════════════════════╝${RESET}\n"
sleep 15.9  # closing: 11.9s display + ~4s buffer (video must outlast audio)
