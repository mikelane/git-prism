"""Step definitions for manifest-specific JSON assertions.

These steps validate structural properties of manifest JSON output
that go beyond the generic JSON assertions in json_steps.py.
"""

from __future__ import annotations

from typing import Any

from behave import then

from json_steps import _ensure_json_parsed


@then('at least one file has path "{path}"')
def step_file_has_path(context: Any, path: str) -> None:
    """Assert that at least one file entry has the given path."""
    manifest = _ensure_json_parsed(context)
    files: list[dict[str, Any]] = manifest.get("files", [])
    assert files, "No 'files' array in JSON output"
    has_matching_path = any(
        file_entry.get("path") == path for file_entry in files
    )
    assert has_matching_path, (
        f"No file with path '{path}' found. "
        f"Paths: {[file_entry.get('path') for file_entry in files]}"
    )


@then("the manifest contains both staged and unstaged changes")
def step_manifest_has_staged_and_unstaged(context: Any) -> None:
    """Assert that the manifest includes both staged and unstaged change scopes."""
    manifest = _ensure_json_parsed(context)
    files: list[dict[str, Any]] = manifest.get("files", [])
    assert files, "No 'files' array in JSON output"

    # The working tree manifest should distinguish staged from unstaged.
    # The exact field name will be determined during implementation,
    # but we assert both types are present.
    scopes = {file_entry.get("change_scope", "").lower() for file_entry in files}

    assert "staged" in scopes, (
        f"No staged changes found. Scopes present: {scopes}"
    )
    assert "unstaged" in scopes, (
        f"No unstaged changes found. Scopes present: {scopes}"
    )


@then("the functions list includes a class-qualified method name")
def step_functions_include_qualified_name(context: Any) -> None:
    """Assert that at least one function name is class-qualified (e.g. contains '.' or '::')."""
    manifest = _ensure_json_parsed(context)
    files: list[dict[str, Any]] = manifest.get("files", [])
    assert files, "No 'files' array in JSON output"

    for file_entry in files:
        functions: list[str | dict[str, str]] | None = file_entry.get("functions_changed")
        if functions is None:
            continue
        for function_record in functions:
            name = function_record if isinstance(function_record, str) else function_record.get("name", "")
            # A class-qualified name contains a dot or double-colon separator
            if "." in name or "::" in name:
                return

    assert False, (
        "No class-qualified method name found in functions_changed. "
        f"Functions: {[file_entry.get('functions_changed') for file_entry in files]}"
    )


@then('the JSON value "commits" has length {count:d}')
def step_json_commits_has_length(context: Any, count: int) -> None:
    """Assert that the commits array has exactly the expected number of entries."""
    manifest = _ensure_json_parsed(context)
    commits: list[dict[str, Any]] = manifest.get("commits", [])
    assert len(commits) == count, (
        f"Expected {count} commits, got {len(commits)}"
    )


@then('each commit entry has keys "metadata" and "files" and "summary"')
def step_each_commit_has_required_keys(context: Any) -> None:
    """Assert that every commit entry contains metadata, files, and summary keys."""
    manifest = _ensure_json_parsed(context)
    commits: list[dict[str, Any]] = manifest.get("commits", [])
    assert commits, "No 'commits' array in JSON output"

    for i, commit in enumerate(commits):
        for key in ("metadata", "files", "summary"):
            assert key in commit, (
                f"Commit {i} missing key '{key}'. "
                f"Available keys: {list(commit.keys())}"
            )
