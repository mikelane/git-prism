#!/bin/bash
set -euo pipefail

# Colors
BLUE='\033[1;34m'
GREEN='\033[1;32m'
CYAN='\033[1;36m'
DIM='\033[2m'
RESET='\033[0m'

DEMO_REPO="/Users/mikelane/dev/git-prism"
BINARY="$DEMO_REPO/target/release/git-prism"

type_cmd() {
    echo -ne "${GREEN}\$ ${RESET}"
    echo -n "$1" | pv -qL 40
    echo ""
    sleep 0.3
}

# -- intro (8.1s) --
echo -e "\n${BLUE}╔══════════════════════════════════════════╗${RESET}"
echo -e "${BLUE}║${RESET}    ${CYAN}git-prism${RESET} v0.1.0 — Demo              ${BLUE}║${RESET}"
echo -e "${BLUE}║${RESET}    Agent-optimized git data MCP server   ${BLUE}║${RESET}"
echo -e "${BLUE}╚══════════════════════════════════════════╝${RESET}\n"
sleep 7.5

# -- languages (10.3s) --
echo -e "${BLUE}--- Supported Languages ---${RESET}\n"
type_cmd "git-prism languages"
$BINARY languages
sleep 8.5

# -- manifest (13.1s) --
echo -e "\n${BLUE}--- Change Manifest ---${RESET}\n"
type_cmd "git-prism manifest HEAD~1..HEAD --repo $DEMO_REPO"
$BINARY manifest HEAD~1..HEAD --repo "$DEMO_REPO" 2>/dev/null | python3 -m json.tool | head -30
echo -e "${DIM}  ... (truncated for demo)${RESET}"
sleep 10.5

# -- manifest_detail (13.2s) --
echo ""
$BINARY manifest HEAD~1..HEAD --repo "$DEMO_REPO" 2>/dev/null | python3 -c "
import json, sys
d = json.load(sys.stdin)
s = d['summary']
print(f\"  Files changed: {s['total_files_changed']}\")
print(f\"  Lines added:   {s['total_lines_added']}\")
print(f\"  Lines removed: {s['total_lines_removed']}\")
print(f\"  Languages:     {', '.join(s['languages_affected'])}\")
if d['files']:
    f = d['files'][0]
    print(f\"\n  First file: {f['path']}\")
    print(f\"    Change type:  {f['change_type']}\")
    print(f\"    Generated:    {f['is_generated']}\")
    fc = f.get('functions_changed')
    if fc is not None:
        print(f\"    Functions changed: {len(fc)}\")
    ic = f.get('imports_changed')
    if ic is not None:
        print(f\"    Imports added: {len(ic.get('added', []))}\")
"
sleep 11.5

# -- snapshot (13.0s) --
echo -e "\n${BLUE}--- File Snapshots ---${RESET}\n"
type_cmd "git-prism snapshot HEAD~1..HEAD --paths src/main.rs --repo $DEMO_REPO"
$BINARY snapshot HEAD~1..HEAD --paths src/main.rs --repo "$DEMO_REPO" 2>/dev/null | python3 -c "
import json, sys
d = json.load(sys.stdin)
f = d['files'][0]
print(f\"  Path: {f['path']}\")
print(f\"  Language: {f['language']}\")
if f.get('before'):
    print(f\"  Before: {f['before']['line_count']} lines, {f['before']['size_bytes']} bytes\")
if f.get('after'):
    print(f\"  After:  {f['after']['line_count']} lines, {f['after']['size_bytes']} bytes\")
print(f\"  Token estimate: {d['token_estimate']}\")
"
sleep 11.0

# -- snapshot_detail (8.4s) --
echo -e "\n${DIM}  Complete file content available (not just diffs)${RESET}"
echo -e "${DIM}  Token estimate helps agents budget context windows${RESET}"
sleep 7.5

# -- error (10.8s) --
echo -e "\n${BLUE}--- Error Handling ---${RESET}\n"
type_cmd "git-prism manifest nonexistent-branch..HEAD --repo $DEMO_REPO"
$BINARY manifest nonexistent-branch..HEAD --repo "$DEMO_REPO" 2>&1 || true
sleep 8.5

# -- mcp_register (11.2s) --
echo -e "\n${BLUE}--- MCP Registration ---${RESET}\n"
type_cmd "claude mcp add git-prism -- git-prism serve"
echo -e "${GREEN}✓ git-prism registered as MCP server${RESET}"
echo -e "${DIM}  Available in all Claude Code sessions${RESET}"
sleep 9.0

# -- closing (11.4s + 4s buffer) --
echo -e "\n${BLUE}╔══════════════════════════════════════════╗${RESET}"
echo -e "${BLUE}║${RESET}  ${CYAN}github.com/mikelane/git-prism${RESET}            ${BLUE}║${RESET}"
echo -e "${BLUE}║${RESET}  ${GREEN}brew tap mikelane/tap${RESET}                   ${BLUE}║${RESET}"
echo -e "${BLUE}║${RESET}  ${GREEN}brew install git-prism${RESET}                  ${BLUE}║${RESET}"
echo -e "${BLUE}╚══════════════════════════════════════════╝${RESET}\n"
sleep 15.0
