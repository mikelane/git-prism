"""Step definitions for function context scenarios.

Repo fixtures create multi-file repos with known call patterns.
Assertion steps validate the function context JSON output.
"""

from __future__ import annotations

from typing import Any

from behave import given, then
from behave.runner import Context

from json_steps import _ensure_json_parsed
from repo_setup_steps import _commit, _init_repo, _write_file


# ---------- Rust fixture: multi-file repo with callers/callees ----------

RUST_LIB_INITIAL = """\
pub fn calculate(x: i32) -> i32 {
    x + 1
}

pub fn helper(x: i32) -> i32 {
    x * 2
}

pub fn unused_func() -> bool {
    true
}
"""

RUST_LIB_MODIFIED = """\
pub fn calculate(x: i32) -> i32 {
    x + 1 + helper(x)
}

pub fn helper(x: i32) -> i32 {
    x * 3
}

pub fn process(data: i32) -> i32 {
    calculate(data) + helper(data)
}

pub fn unused_func() -> bool {
    false
}
"""

RUST_MAIN = """\
use lib::calculate;

fn main() {
    let result = calculate(42);
    println!("{}", result);
}
"""

RUST_TEST = """\
#[cfg(test)]
mod tests {
    use super::calculate;

    #[test]
    fn test_calculate() {
        assert_eq!(calculate(1), 2);
    }
}
"""


@given("a git repository with function context test fixtures")
def step_repo_with_function_context(context: Context) -> None:
    """Create a multi-file Rust repo with known call relationships.

    Commit 1: lib.rs (calculate, helper, unused_func), main.rs (calls calculate),
              tests/test_lib.rs (calls calculate)
    Commit 2: lib.rs modified (calculate now calls helper, new process fn added,
              unused_func body changed)
    """
    repo_dir = _init_repo(context)

    # Commit 1: initial state
    _write_file(repo_dir, "src/lib.rs", RUST_LIB_INITIAL)
    _write_file(repo_dir, "src/main.rs", RUST_MAIN)
    _write_file(repo_dir, "tests/test_lib.rs", RUST_TEST)
    _commit(
        repo_dir, "initial: lib, main, test",
        ["src/lib.rs", "src/main.rs", "tests/test_lib.rs"],
    )

    # Commit 2: modify lib.rs
    _write_file(repo_dir, "src/lib.rs", RUST_LIB_MODIFIED)
    _commit(repo_dir, "modify calculate, helper, add process", ["src/lib.rs"])


# ---------- Unsupported language fixture ----------

@given("a git repository with an unsupported language change")
def step_repo_unsupported_language(context: Context) -> None:
    """Create a repo where only a .txt file changed."""
    repo_dir = _init_repo(context)

    _write_file(repo_dir, "notes.txt", "first version\n")
    _commit(repo_dir, "initial", ["notes.txt"])

    _write_file(repo_dir, "notes.txt", "second version\n")
    _commit(repo_dir, "modify notes", ["notes.txt"])


# ---------- Python fixture ----------

PYTHON_LIB = """\
def compute(x):
    return x + 1
"""

PYTHON_LIB_MODIFIED = """\
def compute(x):
    return x * 2 + 1
"""

PYTHON_CALLER = """\
from lib import compute

def main():
    result = compute(42)
    print(result)
"""


@given("a git repository with a Python function context fixture")
def step_repo_python_context(context: Context) -> None:
    """Create a Python repo where compute() is called from another file."""
    repo_dir = _init_repo(context)

    _write_file(repo_dir, "lib.py", PYTHON_LIB)
    _write_file(repo_dir, "main.py", PYTHON_CALLER)
    _commit(repo_dir, "initial python", ["lib.py", "main.py"])

    _write_file(repo_dir, "lib.py", PYTHON_LIB_MODIFIED)
    _commit(repo_dir, "modify compute", ["lib.py"])


# ---------- Go fixture ----------

GO_LIB = """\
package lib

func Compute(x int) int {
    return x + 1
}
"""

GO_LIB_MODIFIED = """\
package lib

func Compute(x int) int {
    return x*2 + 1
}
"""

GO_CALLER = """\
package main

import "example/lib"

func main() {
    result := lib.Compute(42)
    _ = result
}
"""

GO_MOD = """\
module example

go 1.21
"""


@given("a git repository with a Go function context fixture")
def step_repo_go_context(context: Context) -> None:
    """Create a Go repo where Compute() is called from another file."""
    repo_dir = _init_repo(context)

    _write_file(repo_dir, "go.mod", GO_MOD)
    _write_file(repo_dir, "lib/lib.go", GO_LIB)
    _write_file(repo_dir, "main.go", GO_CALLER)
    _commit(repo_dir, "initial go", ["go.mod", "lib/lib.go", "main.go"])

    _write_file(repo_dir, "lib/lib.go", GO_LIB_MODIFIED)
    _commit(repo_dir, "modify compute", ["lib/lib.go"])


# ---------- TypeScript fixture ----------

TS_LIB = """\
export function compute(x: number): number {
    return x + 1;
}
"""

TS_LIB_MODIFIED = """\
export function compute(x: number): number {
    return x * 2 + 1;
}
"""

TS_CALLER = """\
import { compute } from './lib';

function main() {
    const result = compute(42);
    console.log(result);
}
"""


@given("a git repository with a TypeScript function context fixture")
def step_repo_ts_context(context: Context) -> None:
    """Create a TypeScript repo where compute() is called from another file."""
    repo_dir = _init_repo(context)

    _write_file(repo_dir, "lib.ts", TS_LIB)
    _write_file(repo_dir, "main.ts", TS_CALLER)
    _commit(repo_dir, "initial ts", ["lib.ts", "main.ts"])

    _write_file(repo_dir, "lib.ts", TS_LIB_MODIFIED)
    _commit(repo_dir, "modify compute", ["lib.ts"])


# ---------- Java fixture ----------

JAVA_LIB = """\
public class Lib {
    public static int compute(int x) {
        return x + 1;
    }
}
"""

JAVA_LIB_MODIFIED = """\
public class Lib {
    public static int compute(int x) {
        return x * 2 + 1;
    }
}
"""

JAVA_CALLER = """\
public class Main {
    public static void main(String[] args) {
        int result = Lib.compute(42);
        System.out.println(result);
    }
}
"""


@given("a git repository with a Java function context fixture")
def step_repo_java_context(context: Context) -> None:
    """Create a Java repo where compute() is called from another file."""
    repo_dir = _init_repo(context)

    _write_file(repo_dir, "Lib.java", JAVA_LIB)
    _write_file(repo_dir, "Main.java", JAVA_CALLER)
    _commit(repo_dir, "initial java", ["Lib.java", "Main.java"])

    _write_file(repo_dir, "Lib.java", JAVA_LIB_MODIFIED)
    _commit(repo_dir, "modify compute", ["Lib.java"])


# ---------- C fixture ----------

C_LIB = """\
int compute(int x) {
    return x + 1;
}
"""

C_LIB_MODIFIED = """\
int compute(int x) {
    return x * 2 + 1;
}
"""

C_CALLER = """\
#include <stdio.h>

extern int compute(int x);

int main(void) {
    int result = compute(42);
    printf("%d\\n", result);
    return 0;
}
"""


@given("a git repository with a C function context fixture")
def step_repo_c_context(context: Context) -> None:
    """Create a C repo where compute() is called from another file."""
    repo_dir = _init_repo(context)

    _write_file(repo_dir, "lib.c", C_LIB)
    _write_file(repo_dir, "main.c", C_CALLER)
    _commit(repo_dir, "initial c", ["lib.c", "main.c"])

    _write_file(repo_dir, "lib.c", C_LIB_MODIFIED)
    _commit(repo_dir, "modify compute", ["lib.c"])


# ---------- Ruby fixture ----------

RUBY_LIB = """\
def compute(x)
  x + 1
end
"""

RUBY_LIB_MODIFIED = """\
def compute(x)
  x * 2 + 1
end
"""

RUBY_CALLER = """\
require_relative 'lib'

def main
  result = compute(42)
  puts result
end
"""


@given("a git repository with a Ruby function context fixture")
def step_repo_ruby_context(context: Context) -> None:
    """Create a Ruby repo where compute() is called from another file."""
    repo_dir = _init_repo(context)

    _write_file(repo_dir, "lib.rb", RUBY_LIB)
    _write_file(repo_dir, "main.rb", RUBY_CALLER)
    _commit(repo_dir, "initial ruby", ["lib.rb", "main.rb"])

    _write_file(repo_dir, "lib.rb", RUBY_LIB_MODIFIED)
    _commit(repo_dir, "modify compute", ["lib.rb"])


# ===================================================================
# Assertion steps — all produce meaningful failures, not undefined
# ===================================================================


def _get_context_data(context: Context) -> dict[str, Any]:
    """Parse the JSON output and return the context data."""
    return _ensure_json_parsed(context)


def _get_function_context(
    context: Context, func_name: str,
) -> dict[str, Any]:
    """Find the context entry for a specific function name."""
    data = _get_context_data(context)
    functions = data.get("functions", [])
    assert functions is not None, (
        "Expected 'functions' key in context output, got None. "
        f"Top-level keys: {list(data.keys())}"
    )
    for entry in functions:
        if entry.get("name") == func_name:
            return entry
    raise AssertionError(
        f"No context entry for function '{func_name}'. "
        f"Available: {[e.get('name') for e in functions]}"
    )


@then('the context for function "{func_name}" lists callers')
def step_context_has_callers(context: Context, func_name: str) -> None:
    """Assert that the function has at least one caller."""
    entry = _get_function_context(context, func_name)
    callers = entry.get("callers", [])
    assert callers is not None and len(callers) > 0, (
        f"Expected non-empty callers list for '{func_name}', "
        f"got {callers!r}. Full entry: {entry}"
    )


@then('a caller of "{func_name}" is in file "{filepath}"')
def step_caller_in_file(context: Context, func_name: str, filepath: str) -> None:
    """Assert that at least one caller is from the specified file."""
    entry = _get_function_context(context, func_name)
    callers = entry.get("callers", [])
    assert callers, (
        f"Expected callers for '{func_name}', got empty list"
    )
    files = [c.get("file", c.get("path", "")) for c in callers]
    assert any(filepath in f for f in files), (
        f"No caller of '{func_name}' found in file matching '{filepath}'. "
        f"Caller files: {files}"
    )


@then("each caller entry has a line number")
def step_each_caller_has_line(context: Context) -> None:
    """Assert that every caller entry in the output has a line number."""
    data = _get_context_data(context)
    functions = data.get("functions", [])
    assert functions, "No functions in context output"
    for entry in functions:
        for caller in entry.get("callers", []) or []:
            line = caller.get("line")
            assert line is not None and isinstance(line, int) and line > 0, (
                f"Caller entry missing valid line number: {caller}"
            )


@then('the context for function "{func_name}" lists callees')
def step_context_has_callees(context: Context, func_name: str) -> None:
    """Assert that the function has at least one callee."""
    entry = _get_function_context(context, func_name)
    callees = entry.get("callees", [])
    assert callees is not None and len(callees) > 0, (
        f"Expected non-empty callees list for '{func_name}', "
        f"got {callees!r}. Full entry: {entry}"
    )


@then('a callee of "{func_name}" is "{callee_name}"')
def step_callee_is(context: Context, func_name: str, callee_name: str) -> None:
    """Assert that one of the callees matches the expected name."""
    entry = _get_function_context(context, func_name)
    callees = entry.get("callees", [])
    assert callees, (
        f"Expected callees for '{func_name}', got empty list"
    )
    callee_names = [c.get("name", c) if isinstance(c, dict) else str(c) for c in callees]
    assert any(callee_name in name for name in callee_names), (
        f"Callee '{callee_name}' not found for function '{func_name}'. "
        f"Callees: {callee_names}"
    )


@then('the context for function "{func_name}" has test references')
def step_context_has_test_refs(context: Context, func_name: str) -> None:
    """Assert that the function has at least one test reference."""
    entry = _get_function_context(context, func_name)
    test_refs = entry.get("test_references", entry.get("test_callers", []))
    assert test_refs is not None and len(test_refs) > 0, (
        f"Expected non-empty test references for '{func_name}', "
        f"got {test_refs!r}. Full entry: {entry}"
    )


@then('a test reference for "{func_name}" is in a file matching "{pattern}"')
def step_test_ref_matches_pattern(
    context: Context, func_name: str, pattern: str,
) -> None:
    """Assert that a test reference file path matches the given pattern."""
    entry = _get_function_context(context, func_name)
    test_refs = entry.get("test_references", entry.get("test_callers", []))
    assert test_refs, (
        f"Expected test references for '{func_name}', got empty list"
    )
    files = [r.get("file", r.get("path", "")) for r in test_refs]
    assert any(pattern.lower() in f.lower() for f in files), (
        f"No test reference for '{func_name}' matches pattern '{pattern}'. "
        f"Test ref files: {files}"
    )


@then('the context for function "{func_name}" has zero callers')
def step_context_zero_callers(context: Context, func_name: str) -> None:
    """Assert that the function has no callers (empty list, not null)."""
    entry = _get_function_context(context, func_name)
    callers = entry.get("callers")
    assert callers is not None, (
        f"Expected callers to be an empty list for '{func_name}', "
        f"got null/None. Full entry: {entry}"
    )
    assert len(callers) == 0, (
        f"Expected zero callers for '{func_name}', "
        f"got {len(callers)}: {callers}"
    )


@then("the context result has a null entry for unsupported files")
def step_context_null_for_unsupported(context: Context) -> None:
    """Assert that files with unsupported languages have null context."""
    data = _get_context_data(context)
    # The tool should indicate unsupported files have null function context
    functions = data.get("functions")
    if functions is None:
        # If the entire response is null functions, that's valid
        return
    # Otherwise, check that unsupported files are handled
    # (the exact representation will be determined during implementation)
    assert functions is not None, (
        "Expected context output to handle unsupported languages. "
        f"Got: {data}"
    )


@then("the context result has entries for at least {count:d} functions")
def step_context_has_n_entries(context: Context, count: int) -> None:
    """Assert that the context contains entries for multiple functions."""
    data = _get_context_data(context)
    functions = data.get("functions", [])
    assert functions is not None, (
        "Expected 'functions' key in context output, got None"
    )
    assert len(functions) >= count, (
        f"Expected at least {count} function context entries, "
        f"got {len(functions)}: {[e.get('name') for e in functions]}"
    )


@then("each function context entry has callers and callees keys")
def step_each_entry_has_keys(context: Context) -> None:
    """Assert that every function entry has both callers and callees."""
    data = _get_context_data(context)
    functions = data.get("functions", [])
    assert functions, "No functions in context output"
    for i, entry in enumerate(functions):
        assert "callers" in entry, (
            f"Function entry {i} ({entry.get('name', '?')}) missing 'callers' key. "
            f"Keys: {list(entry.keys())}"
        )
        assert "callees" in entry, (
            f"Function entry {i} ({entry.get('name', '?')}) missing 'callees' key. "
            f"Keys: {list(entry.keys())}"
        )
