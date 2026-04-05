#!/bin/bash
set -euo pipefail

# Colors
BLUE='\033[1;34m'
GREEN='\033[1;32m'
CYAN='\033[1;36m'
DIM='\033[2m'
RESET='\033[0m'

WORKTREE_ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
BINARY="$WORKTREE_ROOT/target/release/git-prism"
TMPDIR_BASE=$(mktemp -d)

cleanup() { rm -rf "$TMPDIR_BASE"; }
trap cleanup EXIT

type_cmd() {
    echo -ne "${GREEN}\$ ${RESET}"
    echo -n "$1" | pv -qL 40
    echo ""
    sleep 0.3
}

# Create a Java test repo
JAVA_REPO="$TMPDIR_BASE/java-project"
mkdir -p "$JAVA_REPO"
(
    cd "$JAVA_REPO"
    git init -q
    git config user.email "demo@test.com"
    git config user.name "Demo"

    cat > Calculator.java <<'JAVA'
package com.example;

import java.util.List;

public class Calculator {
    public int add(int a, int b) {
        return a + b;
    }
}
JAVA
    git add Calculator.java
    git commit -q -m "initial Calculator class"

    cat > Calculator.java <<'JAVA'
package com.example;

import java.util.List;
import java.util.Map;

public class Calculator {
    public int add(int a, int b) {
        return a + b;
    }

    public int multiply(int a, int b) {
        return a * b;
    }
}
JAVA
    git add Calculator.java
    git commit -q -m "add multiply method and Map import"
)

# Create a C++ test repo
CPP_REPO="$TMPDIR_BASE/cpp-project"
mkdir -p "$CPP_REPO"
(
    cd "$CPP_REPO"
    git init -q
    git config user.email "demo@test.com"
    git config user.name "Demo"

    cat > calculator.cpp <<'CPP'
#include <iostream>

namespace math {

class Calculator {
public:
    int add(int a, int b) {
        return a + b;
    }
};

}  // namespace math
CPP
    git add calculator.cpp
    git commit -q -m "initial Calculator class"

    cat > calculator.cpp <<'CPP'
#include <iostream>
#include <vector>

namespace math {

class Calculator {
public:
    int add(int a, int b) {
        return a + b;
    }

    int multiply(int a, int b) {
        return a * b;
    }
};

}  // namespace math
CPP
    git add calculator.cpp
    git commit -q -m "add multiply method"
)

# Create a working tree test repo
WT_REPO="$TMPDIR_BASE/working-tree"
mkdir -p "$WT_REPO"
(
    cd "$WT_REPO"
    git init -q
    git config user.email "demo@test.com"
    git config user.name "Demo"

    echo "version = 1.0" > config.txt
    echo 'def main(): print("hello")' > app.py
    git add config.txt app.py
    git commit -q -m "initial files"

    # Staged change
    echo "version = 2.0" > config.txt
    git add config.txt

    # Unstaged change
    echo 'def main(): print("goodbye")' > app.py
)

# Create a multi-commit repo for history (4 commits so HEAD~3..HEAD covers 3)
HIST_REPO="$TMPDIR_BASE/history-project"
mkdir -p "$HIST_REPO"
(
    cd "$HIST_REPO"
    git init -q
    git config user.email "demo@test.com"
    git config user.name "Demo"

    echo "# Project" > README.md
    git add README.md
    git commit -q -m "initial readme"

    echo 'fn main() { println!("hello"); }' > main.rs
    git add main.rs
    git commit -q -m "add main entry point"

    cat > lib.rs <<'RUST'
pub fn greet(name: &str) -> String {
    format!("Hello, {}!", name)
}
RUST
    git add lib.rs
    git commit -q -m "add greeting library"

    echo 'mod lib;' >> main.rs
    git add main.rs
    git commit -q -m "wire lib into main"
)

# ============================================================
# DEMO START
# ============================================================

# -- intro (7.4s) --
echo -e "\n${BLUE}╔══════════════════════════════════════════╗${RESET}"
echo -e "${BLUE}║${RESET}    ${CYAN}git-prism${RESET} v0.2.0 — Demo              ${BLUE}║${RESET}"
echo -e "${BLUE}║${RESET}    Broader coverage, working tree,       ${BLUE}║${RESET}"
echo -e "${BLUE}║${RESET}    per-commit history                    ${BLUE}║${RESET}"
echo -e "${BLUE}╚══════════════════════════════════════════╝${RESET}\n"
sleep 6.8

# -- languages (8.8s) --
echo -e "${BLUE}--- Supported Languages ---${RESET}\n"
type_cmd "git-prism languages"
$BINARY languages
sleep 7.0

# -- java_analysis (14.9s) --
echo -e "\n${BLUE}--- Java Analysis ---${RESET}\n"
type_cmd "git-prism manifest HEAD~1..HEAD --repo $JAVA_REPO"
$BINARY manifest HEAD~1..HEAD --repo "$JAVA_REPO" 2>/dev/null | python3 -c "
import json, sys
d = json.load(sys.stdin)
f = d['files'][0]
print(f\"  File: {f['path']}\")
print(f\"  Language: {f['language']}\")
fc = f.get('functions_changed', [])
if fc:
    print(f\"  Functions changed:\")
    for fn in fc:
        print(f\"    {fn['change_type']:10s} {fn['name']}  ({fn['signature']})\")
ic = f.get('imports_changed')
if ic:
    if ic.get('added'):
        print(f\"  Imports added: {', '.join(ic['added'])}\")
"
sleep 11.5

# -- cpp_analysis (13.6s) --
echo -e "\n${BLUE}--- C++ Analysis ---${RESET}\n"
type_cmd "git-prism manifest HEAD~1..HEAD --repo $CPP_REPO"
$BINARY manifest HEAD~1..HEAD --repo "$CPP_REPO" 2>/dev/null | python3 -c "
import json, sys
d = json.load(sys.stdin)
f = d['files'][0]
print(f\"  File: {f['path']}\")
print(f\"  Language: {f['language']}\")
fc = f.get('functions_changed', [])
if fc:
    print(f\"  Functions changed:\")
    for fn in fc:
        print(f\"    {fn['change_type']:10s} {fn['name']}\")
ic = f.get('imports_changed')
if ic and ic.get('added'):
    print(f\"  Includes added: {', '.join(ic['added'])}\")
"
sleep 10.5

# -- working_tree (13.9s) --
echo -e "\n${BLUE}--- Working Tree Status ---${RESET}\n"
type_cmd "git-prism manifest HEAD --repo $WT_REPO"
$BINARY manifest HEAD --repo "$WT_REPO" 2>/dev/null | python3 -m json.tool | head -35
echo -e "${DIM}  ... (truncated)${RESET}"
sleep 10.5

# -- working_tree_detail (13.6s) --
echo ""
$BINARY manifest HEAD --repo "$WT_REPO" 2>/dev/null | python3 -c "
import json, sys
d = json.load(sys.stdin)
print(f\"  Files in working tree manifest: {len(d['files'])}\")
for f in d['files']:
    scope = f.get('change_scope', 'committed')
    print(f\"    {f['path']:20s} scope={scope:10s} type={f['change_type']}\")
"
sleep 11.5

# -- history (11.2s) --
echo -e "\n${BLUE}--- Per-Commit History ---${RESET}\n"
type_cmd "git-prism history HEAD~3..HEAD --repo $HIST_REPO"
$BINARY history HEAD~3..HEAD --repo "$HIST_REPO" 2>/dev/null | python3 -c "
import json, sys
d = json.load(sys.stdin)
print(f\"  Commits in range: {len(d['commits'])}\")
for i, c in enumerate(d['commits']):
    m = c['metadata']
    print(f\"    {i+1}. {m['sha'][:7]} {m['message']}\")
    print(f\"       Files: {c['summary']['total_files_changed']}, +{c['summary']['total_lines_added']}/-{c['summary']['total_lines_removed']}\")
"
sleep 7.5

# -- error (8.4s) --
echo -e "\n${BLUE}--- Error Handling ---${RESET}\n"
type_cmd "git-prism snapshot HEAD --repo $WT_REPO"
$BINARY snapshot HEAD --repo "$WT_REPO" 2>&1 || true
sleep 6.0

# -- install (9.5s) --
echo -e "\n${BLUE}--- Installation ---${RESET}\n"
echo -e "  ${GREEN}cargo install git-prism${RESET}          ${DIM}# from crates.io${RESET}"
echo -e "  ${GREEN}brew tap mikelane/tap${RESET}             ${DIM}# Homebrew${RESET}"
echo -e "  ${GREEN}brew install git-prism${RESET}"
echo -e "  ${DIM}# or download from GitHub Releases${RESET}"
sleep 8.0

# -- closing (11.7s + 4s buffer) --
echo -e "\n${BLUE}╔══════════════════════════════════════════╗${RESET}"
echo -e "${BLUE}║${RESET}  ${CYAN}github.com/mikelane/git-prism${RESET}            ${BLUE}║${RESET}"
echo -e "${BLUE}║${RESET}  8 languages, working tree, history      ${BLUE}║${RESET}"
echo -e "${BLUE}║${RESET}  ${GREEN}cargo install git-prism${RESET}                 ${BLUE}║${RESET}"
echo -e "${BLUE}╚══════════════════════════════════════════╝${RESET}\n"
sleep 22.0
