"""Step definitions for import-scoped caller scenarios.

Fixtures create multi-file repos where some files import the changed module
and others don't (but happen to call a function with the same leaf name).
Assertion steps verify that only importing files appear as callers.
"""

from __future__ import annotations

from typing import Any

from behave import given, then
from behave.runner import Context

from json_steps import _ensure_json_parsed
from repo_setup_steps import _commit, _init_repo, _write_file


# ---------- Rust: one importer, one non-importer with same leaf name ----------

RUST_LIB_INITIAL = """\
pub fn compute(x: i32) -> i32 {
    x + 1
}
"""

RUST_LIB_MODIFIED = """\
pub fn compute(x: i32) -> i32 {
    x * 2 + 1
}
"""

RUST_IMPORTER = """\
use crate::lib::compute;

fn caller() {
    let result = compute(42);
}
"""

# This file has a function named `compute` call but does NOT import the changed module
RUST_NON_IMPORTER = """\
fn unrelated_compute() -> i32 {
    compute(99)
}

fn compute(x: i32) -> i32 {
    x - 1
}
"""


@given("a Rust repo where only one file imports the changed module")
def step_rust_import_scoped(context: Context) -> None:
    """Rust repo: lib.rs changed, importer.rs imports it, unrelated.rs doesn't."""
    repo_dir = _init_repo(context)
    _write_file(repo_dir, "src/lib.rs", RUST_LIB_INITIAL)
    _write_file(repo_dir, "src/importer.rs", RUST_IMPORTER)
    _write_file(repo_dir, "src/unrelated.rs", RUST_NON_IMPORTER)
    _commit(
        repo_dir, "initial",
        ["src/lib.rs", "src/importer.rs", "src/unrelated.rs"],
    )
    _write_file(repo_dir, "src/lib.rs", RUST_LIB_MODIFIED)
    _commit(repo_dir, "modify compute", ["src/lib.rs"])


# ---------- Python: one importer, one non-importer ----------

PY_LIB_INITIAL = """\
def compute(x):
    return x + 1
"""

PY_LIB_MODIFIED = """\
def compute(x):
    return x * 2 + 1
"""

PY_IMPORTER = """\
from lib import compute

def caller():
    result = compute(42)
"""

PY_NON_IMPORTER = """\
def compute(x):
    return x - 1

def other():
    result = compute(99)
"""


@given("a Python repo where only one file imports the changed module")
def step_python_import_scoped(context: Context) -> None:
    """Python repo: lib.py changed, importer.py imports it, unrelated.py doesn't."""
    repo_dir = _init_repo(context)
    _write_file(repo_dir, "lib.py", PY_LIB_INITIAL)
    _write_file(repo_dir, "importer.py", PY_IMPORTER)
    _write_file(repo_dir, "unrelated.py", PY_NON_IMPORTER)
    _commit(
        repo_dir, "initial",
        ["lib.py", "importer.py", "unrelated.py"],
    )
    _write_file(repo_dir, "lib.py", PY_LIB_MODIFIED)
    _commit(repo_dir, "modify compute", ["lib.py"])


# ---------- TypeScript: one importer, one non-importer ----------

TS_LIB_INITIAL = """\
export function compute(x: number): number {
    return x + 1;
}
"""

TS_LIB_MODIFIED = """\
export function compute(x: number): number {
    return x * 2 + 1;
}
"""

TS_IMPORTER = """\
import { compute } from './lib';

function caller() {
    const result = compute(42);
}
"""

TS_NON_IMPORTER = """\
function compute(x: number): number {
    return x - 1;
}

function other() {
    const result = compute(99);
}
"""


@given("a TypeScript repo where only one file imports the changed module")
def step_ts_import_scoped(context: Context) -> None:
    """TS repo: lib.ts changed, importer.ts imports it, unrelated.ts doesn't."""
    repo_dir = _init_repo(context)
    _write_file(repo_dir, "lib.ts", TS_LIB_INITIAL)
    _write_file(repo_dir, "importer.ts", TS_IMPORTER)
    _write_file(repo_dir, "unrelated.ts", TS_NON_IMPORTER)
    _commit(
        repo_dir, "initial",
        ["lib.ts", "importer.ts", "unrelated.ts"],
    )
    _write_file(repo_dir, "lib.ts", TS_LIB_MODIFIED)
    _commit(repo_dir, "modify compute", ["lib.ts"])


# ---------- Go: same-package caller (no explicit import needed) ----------

GO_MOD = """\
module example

go 1.21
"""

GO_LIB_INITIAL = """\
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

GO_SAME_PKG_CALLER = """\
package lib

func Caller() int {
    return Compute(42)
}
"""


@given("a Go repo where a same-package file calls the changed function")
def step_go_same_package(context: Context) -> None:
    """Go repo: lib.go changed, caller.go in same package calls it."""
    repo_dir = _init_repo(context)
    _write_file(repo_dir, "go.mod", GO_MOD)
    _write_file(repo_dir, "lib/lib.go", GO_LIB_INITIAL)
    _write_file(repo_dir, "lib/caller.go", GO_SAME_PKG_CALLER)
    _commit(
        repo_dir, "initial",
        ["go.mod", "lib/lib.go", "lib/caller.go"],
    )
    _write_file(repo_dir, "lib/lib.go", GO_LIB_MODIFIED)
    _commit(repo_dir, "modify Compute", ["lib/lib.go"])


# ---------- Ruby fallback fixture ----------

RUBY_LIB_INITIAL = """\
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

def caller_fn
  result = compute(42)
end
"""


@given("a Ruby repo with callers of a changed function")
def step_ruby_fallback(context: Context) -> None:
    """Ruby repo: lib.rb changed, caller.rb calls compute."""
    repo_dir = _init_repo(context)
    _write_file(repo_dir, "lib.rb", RUBY_LIB_INITIAL)
    _write_file(repo_dir, "caller.rb", RUBY_CALLER)
    _commit(repo_dir, "initial", ["lib.rb", "caller.rb"])
    _write_file(repo_dir, "lib.rb", RUBY_LIB_MODIFIED)
    _commit(repo_dir, "modify compute", ["lib.rb"])


# ---------- C fallback fixture ----------

C_LIB_INITIAL = """\
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
extern int compute(int x);

int main(void) {
    int result = compute(42);
    return 0;
}
"""


@given("a C repo with callers of a changed function")
def step_c_fallback(context: Context) -> None:
    """C repo: lib.c changed, main.c calls compute."""
    repo_dir = _init_repo(context)
    _write_file(repo_dir, "lib.c", C_LIB_INITIAL)
    _write_file(repo_dir, "main.c", C_CALLER)
    _commit(repo_dir, "initial", ["lib.c", "main.c"])
    _write_file(repo_dir, "lib.c", C_LIB_MODIFIED)
    _commit(repo_dir, "modify compute", ["lib.c"])


# ---------- Rust pub use re-export fixture ----------

RUST_REEXPORT_CARGO_TOML = """\
[package]
name = "reexport_test"
version = "0.1.0"
edition = "2021"
"""

RUST_REEXPORT_LIB = """\
pub mod inner;
pub mod caller;

pub use inner::compute;
"""

RUST_REEXPORT_INNER_INITIAL = """\
pub fn compute(x: i32) -> i32 {
    x + 1
}
"""

RUST_REEXPORT_INNER_MODIFIED = """\
pub fn compute(x: i32) -> i32 {
    x * 2 + 1
}
"""

# caller.rs uses the re-exported path via `pub use crate::inner::compute`
# to exercise both the pub use match AND the actual caller behavior.
RUST_REEXPORT_CALLER = """\
pub use crate::inner::compute;

pub fn run() {
    let result = compute(42);
}
"""


@given("a Rust repo where a file pub-re-exports the changed function")
def step_rust_pub_use(context: Context) -> None:
    """Rust repo: inner.rs's compute() is re-exported and called via pub use."""
    repo_dir = _init_repo(context)
    _write_file(repo_dir, "Cargo.toml", RUST_REEXPORT_CARGO_TOML)
    _write_file(repo_dir, "src/lib.rs", RUST_REEXPORT_LIB)
    _write_file(repo_dir, "src/inner.rs", RUST_REEXPORT_INNER_INITIAL)
    _write_file(repo_dir, "src/caller.rs", RUST_REEXPORT_CALLER)
    _commit(
        repo_dir,
        "initial",
        ["Cargo.toml", "src/lib.rs", "src/inner.rs", "src/caller.rs"],
    )
    _write_file(repo_dir, "src/inner.rs", RUST_REEXPORT_INNER_MODIFIED)
    _commit(repo_dir, "modify compute", ["src/inner.rs"])


# ---------- Rust integration test via extern crate name ----------

RUST_INTEG_CARGO_TOML = """\
[package]
name = "integ_test_crate"
version = "0.1.0"
edition = "2021"
"""

RUST_INTEG_LIB_INITIAL = """\
pub fn compute(x: i32) -> i32 {
    x + 1
}
"""

RUST_INTEG_LIB_MODIFIED = """\
pub fn compute(x: i32) -> i32 {
    x * 2 + 1
}
"""

RUST_INTEG_TEST = """\
use integ_test_crate::compute;

#[test]
fn it_computes() {
    let result = compute(1);
    assert_eq!(result, 3);
}
"""


@given("a Rust repo with an integration test that uses the extern crate name")
def step_rust_integration_test(context: Context) -> None:
    """Rust repo: tests/it.rs uses `use integ_test_crate::compute;`."""
    repo_dir = _init_repo(context)
    _write_file(repo_dir, "Cargo.toml", RUST_INTEG_CARGO_TOML)
    _write_file(repo_dir, "src/lib.rs", RUST_INTEG_LIB_INITIAL)
    _write_file(repo_dir, "tests/it.rs", RUST_INTEG_TEST)
    _commit(
        repo_dir,
        "initial",
        ["Cargo.toml", "src/lib.rs", "tests/it.rs"],
    )
    _write_file(repo_dir, "src/lib.rs", RUST_INTEG_LIB_MODIFIED)
    _commit(repo_dir, "modify compute", ["src/lib.rs"])


# ---------- Python relative import fixture ----------

PY_PKG_INIT = ""

PY_PKG_LIB_INITIAL = """\
def compute(x):
    return x + 1
"""

PY_PKG_LIB_MODIFIED = """\
def compute(x):
    return x * 2 + 1
"""

PY_PKG_SIBLING = """\
from . import lib

def caller():
    return lib.compute(42)
"""


@given("a Python repo where a sibling uses a relative import")
def step_python_relative_import(context: Context) -> None:
    """Python pkg/lib.py changed; pkg/sibling.py uses `from . import lib`."""
    repo_dir = _init_repo(context)
    _write_file(repo_dir, "pkg/__init__.py", PY_PKG_INIT)
    _write_file(repo_dir, "pkg/lib.py", PY_PKG_LIB_INITIAL)
    _write_file(repo_dir, "pkg/sibling.py", PY_PKG_SIBLING)
    _commit(
        repo_dir,
        "initial",
        ["pkg/__init__.py", "pkg/lib.py", "pkg/sibling.py"],
    )
    _write_file(repo_dir, "pkg/lib.py", PY_PKG_LIB_MODIFIED)
    _commit(repo_dir, "modify compute", ["pkg/lib.py"])


# ---------- Assertion steps ----------


def _get_function_callers(context: Context, func_name: str) -> list[dict[str, Any]]:
    """Get all callers (production + test) for a named function."""
    data = _ensure_json_parsed(context)
    functions = data.get("functions", [])
    assert functions is not None, "No functions in context output"
    for entry in functions:
        if entry.get("name") == func_name:
            callers = entry.get("callers", []) or []
            test_refs = entry.get("test_references", []) or []
            return callers + test_refs
    raise AssertionError(
        f"No context entry for function '{func_name}'. "
        f"Available: {[e.get('name') for e in functions]}"
    )


@then('the function "{func_name}" has only callers from importing files')
def step_only_importing_callers(context: Context, func_name: str) -> None:
    """Assert that no caller comes from a file named 'unrelated'."""
    callers = _get_function_callers(context, func_name)
    unrelated = [c for c in callers if "unrelated" in c.get("file", "")]
    assert not unrelated, (
        f"Expected no callers from non-importing files, but found callers "
        f"from: {[c['file'] for c in unrelated]}. "
        f"All callers: {[c.get('file') for c in callers]}"
    )


@then('the function "{func_name}" has at least {count:d} caller')
def step_has_n_callers(context: Context, func_name: str, count: int) -> None:
    """Assert the function has at least N callers."""
    callers = _get_function_callers(context, func_name)
    assert len(callers) >= count, (
        f"Expected at least {count} caller(s) for '{func_name}', "
        f"got {len(callers)}: {callers}"
    )


@then('the function "{func_name}" has scoping_mode "{mode}"')
def step_has_scoping_mode(context: Context, func_name: str, mode: str) -> None:
    """Assert the function entry has the expected scoping_mode value."""
    data = _ensure_json_parsed(context)
    functions = data.get("functions", [])
    for entry in functions:
        if entry.get("name") == func_name:
            actual = entry.get("scoping_mode")
            assert actual == mode, (
                f"Expected scoping_mode={mode!r} for '{func_name}', "
                f"got {actual!r}. Full entry: {entry}"
            )
            return
    raise AssertionError(
        f"No context entry for function '{func_name}'. "
        f"Available: {[e.get('name') for e in functions]}"
    )
