"""Step definitions for content-aware function diff scenarios.

Repo fixtures create specific function change patterns (reorder, body change,
rename) and assertion steps validate the manifest output.
"""

from __future__ import annotations

from typing import Any

from behave import given, then
from behave.runner import Context

from json_steps import _ensure_json_parsed
from repo_setup_steps import _commit, _init_repo, _write_file

# --- Rust source fixtures ---

RUST_TWO_FUNCTIONS = """\
fn greet(name: &str) -> String {
    format!("Hello, {}!", name)
}

fn farewell(name: &str) -> String {
    format!("Goodbye, {}!", name)
}
"""

RUST_TWO_FUNCTIONS_SWAPPED = """\
fn farewell(name: &str) -> String {
    format!("Goodbye, {}!", name)
}

fn greet(name: &str) -> String {
    format!("Hello, {}!", name)
}
"""

RUST_NEW_FUNCTION_ABOVE = """\
fn new_function() -> i32 {
    42
}

fn greet(name: &str) -> String {
    format!("Hello, {}!", name)
}

fn farewell(name: &str) -> String {
    format!("Goodbye, {}!", name)
}
"""

RUST_BODY_BEFORE = """\
fn compute(x: i32) -> i32 {
    x + 1
}
"""

RUST_BODY_AFTER = """\
fn compute(x: i32) -> i32 {
    x * 2 + 1
}
"""

RUST_RENAME_BEFORE = """\
fn old_name(x: i32) -> i32 {
    x + 1
}
"""

RUST_RENAME_AFTER = """\
fn new_name(x: i32) -> i32 {
    x + 1
}
"""

RUST_RENAME_AND_MODIFY_BEFORE = """\
fn old_name(x: i32) -> i32 {
    x + 1
}
"""

RUST_RENAME_AND_MODIFY_AFTER = """\
fn new_name(x: i32) -> i32 {
    x * 2 + 1
}
"""

# --- Repo setup steps ---


@given("a git repository where functions are reordered between commits")
def step_repo_reorder(context: Context) -> None:
    """Two commits: second swaps the order of two functions."""
    repo_dir = _init_repo(context)
    _write_file(repo_dir, "lib.rs", RUST_TWO_FUNCTIONS)
    _commit(repo_dir, "initial two functions", ["lib.rs"])
    _write_file(repo_dir, "lib.rs", RUST_TWO_FUNCTIONS_SWAPPED)
    _commit(repo_dir, "swap function order", ["lib.rs"])


@given("a git repository where a new function is added above an existing one")
def step_repo_add_above(context: Context) -> None:
    """Two commits: second adds a new function before the existing ones."""
    repo_dir = _init_repo(context)
    _write_file(repo_dir, "lib.rs", RUST_TWO_FUNCTIONS)
    _commit(repo_dir, "initial two functions", ["lib.rs"])
    _write_file(repo_dir, "lib.rs", RUST_NEW_FUNCTION_ABOVE)
    _commit(repo_dir, "add new function above existing", ["lib.rs"])


@given("a git repository where a function body is modified")
def step_repo_body_change(context: Context) -> None:
    """Two commits: second changes the function body without changing signature."""
    repo_dir = _init_repo(context)
    _write_file(repo_dir, "lib.rs", RUST_BODY_BEFORE)
    _commit(repo_dir, "initial compute function", ["lib.rs"])
    _write_file(repo_dir, "lib.rs", RUST_BODY_AFTER)
    _commit(repo_dir, "change compute body", ["lib.rs"])


@given("a git repository where a function is renamed")
def step_repo_rename(context: Context) -> None:
    """Two commits: second renames a function without changing its body."""
    repo_dir = _init_repo(context)
    _write_file(repo_dir, "lib.rs", RUST_RENAME_BEFORE)
    _commit(repo_dir, "initial function", ["lib.rs"])
    _write_file(repo_dir, "lib.rs", RUST_RENAME_AFTER)
    _commit(repo_dir, "rename function", ["lib.rs"])


@given("a git repository where a function is renamed and modified")
def step_repo_rename_and_modify(context: Context) -> None:
    """Two commits: second renames AND changes the body of a function."""
    repo_dir = _init_repo(context)
    _write_file(repo_dir, "lib.rs", RUST_RENAME_AND_MODIFY_BEFORE)
    _commit(repo_dir, "initial function", ["lib.rs"])
    _write_file(repo_dir, "lib.rs", RUST_RENAME_AND_MODIFY_AFTER)
    _commit(repo_dir, "rename and modify function", ["lib.rs"])


# --- Assertion steps ---


def _get_file_functions(context: Context, filename: str) -> list[dict[str, Any]]:
    """Extract the functions_changed list for a specific file from manifest JSON."""
    manifest = _ensure_json_parsed(context)
    files: list[dict[str, Any]] = manifest.get("files", [])
    for file_entry in files:
        if file_entry.get("path") == filename:
            return file_entry.get("functions_changed") or []
    raise AssertionError(
        f"File '{filename}' not found in manifest. "
        f"Available: {[f.get('path') for f in files]}"
    )


@then('the file "{filename}" has zero function changes')
def step_file_zero_function_changes(context: Context, filename: str) -> None:
    """Assert that a file has an empty functions_changed list."""
    functions = _get_file_functions(context, filename)
    assert len(functions) == 0, (
        f"Expected zero function changes for '{filename}', "
        f"got {len(functions)}: {functions}"
    )


@then('the only function change for "{filename}" is added "{func_name}"')
def step_only_function_change_is_added(
    context: Context, filename: str, func_name: str,
) -> None:
    """Assert exactly one function change: an 'added' with the given name."""
    functions = _get_file_functions(context, filename)
    assert len(functions) == 1, (
        f"Expected exactly 1 function change for '{filename}', "
        f"got {len(functions)}: {functions}"
    )
    assert functions[0]["name"] == func_name, (
        f"Expected function name '{func_name}', got '{functions[0]['name']}'"
    )
    assert functions[0]["change_type"] == "added", (
        f"Expected change_type 'added', got '{functions[0]['change_type']}'"
    )


@then('the file "{filename}" has a function change "{func_name}" with type "{change_type}"')
def step_file_has_function_change(
    context: Context, filename: str, func_name: str, change_type: str,
) -> None:
    """Assert that a specific function change exists with the given type."""
    functions = _get_file_functions(context, filename)
    for fn in functions:
        if fn["name"] == func_name and fn["change_type"] == change_type:
            return
    raise AssertionError(
        f"No function change with name='{func_name}' and type='{change_type}' "
        f"in '{filename}'. Got: {functions}"
    )


@then('the renamed function "{func_name}" has old_name "{old_name}"')
def step_renamed_function_has_old_name(
    context: Context, func_name: str, old_name: str,
) -> None:
    """Assert that a renamed function has the expected old_name."""
    manifest = _ensure_json_parsed(context)
    files: list[dict[str, Any]] = manifest.get("files", [])
    for file_entry in files:
        for fn in file_entry.get("functions_changed") or []:
            if fn["name"] == func_name and fn["change_type"] == "renamed":
                actual_old = fn.get("old_name")
                assert actual_old == old_name, (
                    f"Expected old_name='{old_name}' for renamed function "
                    f"'{func_name}', got '{actual_old}'"
                )
                return
    raise AssertionError(
        f"No renamed function '{func_name}' found in any file."
    )
