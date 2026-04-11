"""Step definitions for blast radius scoring scenarios.

Fixtures create repos with known caller/test patterns.
Assertion steps validate the blast_radius object in function context output.
"""

from __future__ import annotations

from typing import Any

from behave import given, then
from behave.runner import Context

from json_steps import _ensure_json_parsed
from repo_setup_steps import _commit, _init_repo, _write_file


# ---------- Fixture: function with callers but NO tests ----------

RUST_NO_TESTS_LIB_INITIAL = """\
pub fn process_data(x: i32) -> i32 {
    x + 1
}
"""

RUST_NO_TESTS_LIB_MODIFIED = """\
pub fn process_data(x: i32) -> i32 {
    x * 2 + 1
}
"""

RUST_NO_TESTS_CALLER_A = """\
use crate::lib::process_data;

fn handle_request() {
    let result = process_data(42);
}
"""

RUST_NO_TESTS_CALLER_B = """\
use crate::lib::process_data;

fn batch_run() {
    let result = process_data(100);
}
"""


@given("a git repository with a blast radius no-tests fixture")
def step_repo_blast_radius_no_tests(context: Context) -> None:
    """Create a repo where process_data has callers but no test files."""
    repo_dir = _init_repo(context)

    _write_file(repo_dir, "src/lib.rs", RUST_NO_TESTS_LIB_INITIAL)
    _write_file(repo_dir, "src/handler.rs", RUST_NO_TESTS_CALLER_A)
    _write_file(repo_dir, "src/batch.rs", RUST_NO_TESTS_CALLER_B)
    _commit(
        repo_dir, "initial: lib + callers, no tests",
        ["src/lib.rs", "src/handler.rs", "src/batch.rs"],
    )

    _write_file(repo_dir, "src/lib.rs", RUST_NO_TESTS_LIB_MODIFIED)
    _commit(repo_dir, "modify process_data", ["src/lib.rs"])


# ---------- Assertion steps ----------


def _get_function_blast_radius(
    context: Context, func_name: str,
) -> dict[str, Any]:
    """Find the blast_radius for a specific function in context output."""
    data = _ensure_json_parsed(context)
    functions = data.get("functions", [])
    assert functions is not None, (
        "Expected 'functions' key in context output, got None. "
        f"Top-level keys: {list(data.keys())}"
    )
    for entry in functions:
        if entry.get("name") == func_name:
            blast_radius = entry.get("blast_radius")
            assert blast_radius is not None, (
                f"Function '{func_name}' has no 'blast_radius' field. "
                f"Entry keys: {list(entry.keys())}"
            )
            return blast_radius
    raise AssertionError(
        f"No context entry for function '{func_name}'. "
        f"Available: {[e.get('name') for e in functions]}"
    )


@then('each function context entry has a "blast_radius" object')
def step_each_entry_has_blast_radius(context: Context) -> None:
    """Assert that every function context entry has a blast_radius field."""
    data = _ensure_json_parsed(context)
    functions = data.get("functions", [])
    assert functions, "No functions in context output"
    for i, entry in enumerate(functions):
        assert "blast_radius" in entry, (
            f"Function entry {i} ({entry.get('name', '?')}) missing "
            f"'blast_radius' key. Keys: {list(entry.keys())}"
        )
        br = entry["blast_radius"]
        assert isinstance(br, dict), (
            f"blast_radius for '{entry.get('name', '?')}' is not an object: {br!r}"
        )


@then('each blast_radius has fields "{f1}" and "{f2}" and "{f3}" and "{f4}"')
def step_blast_radius_has_fields(
    context: Context, f1: str, f2: str, f3: str, f4: str,
) -> None:
    """Assert that every blast_radius object has the four required fields."""
    data = _ensure_json_parsed(context)
    functions = data.get("functions", [])
    assert functions, "No functions in context output"
    required = {f1, f2, f3, f4}
    for entry in functions:
        br = entry.get("blast_radius")
        assert br is not None, (
            f"Function '{entry.get('name', '?')}' missing blast_radius"
        )
        missing = required - set(br.keys())
        assert not missing, (
            f"blast_radius for '{entry.get('name', '?')}' missing fields: {missing}. "
            f"Got: {list(br.keys())}"
        )


@then('the function "{func_name}" has blast_radius risk "{risk}"')
def step_function_has_risk(context: Context, func_name: str, risk: str) -> None:
    """Assert that a function's blast_radius has the expected risk level."""
    br = _get_function_blast_radius(context, func_name)
    actual_risk = br.get("risk")
    assert actual_risk == risk, (
        f"Expected risk '{risk}' for '{func_name}', "
        f"got '{actual_risk}'. Full blast_radius: {br}"
    )


@then('the function "{func_name}" has blast_radius with has_tests {value}')
def step_function_has_tests(context: Context, func_name: str, value: str) -> None:
    """Assert that a function's blast_radius has the expected has_tests value."""
    br = _get_function_blast_radius(context, func_name)
    expected = value.lower() == "true"
    actual = br.get("has_tests")
    assert actual == expected, (
        f"Expected has_tests={expected} for '{func_name}', "
        f"got {actual}. Full blast_radius: {br}"
    )


@then('the function "{func_name}" has blast_radius risk not "{risk}"')
def step_function_risk_not(context: Context, func_name: str, risk: str) -> None:
    """Assert that a function's blast_radius risk is NOT the given value."""
    br = _get_function_blast_radius(context, func_name)
    actual_risk = br.get("risk")
    assert actual_risk != risk, (
        f"Expected risk NOT '{risk}' for '{func_name}', "
        f"but got exactly '{actual_risk}'. Full blast_radius: {br}"
    )
