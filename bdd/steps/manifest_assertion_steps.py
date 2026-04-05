"""Step definitions for manifest-specific JSON assertions.

These steps validate structural properties of manifest JSON output
that go beyond the generic JSON assertions in json_steps.py.
"""

from behave import then

from json_steps import _ensure_json_parsed


@then('at least one file has path "{path}"')
def step_file_has_path(context, path):
    data = _ensure_json_parsed(context)
    files = data.get("files", [])
    assert files, "No 'files' array in JSON output"
    found = any(f.get("path") == path for f in files)
    assert found, (
        f"No file with path '{path}' found. "
        f"Paths: {[f.get('path') for f in files]}"
    )


@then("the manifest contains both staged and unstaged changes")
def step_manifest_has_staged_and_unstaged(context):
    data = _ensure_json_parsed(context)
    files = data.get("files", [])
    assert files, "No 'files' array in JSON output"

    # The working tree manifest should distinguish staged from unstaged.
    # The exact field name will be determined during implementation,
    # but we assert both types are present.
    scopes = {f.get("change_scope", "").lower() for f in files}

    assert "staged" in scopes, (
        f"No staged changes found. Scopes present: {scopes}"
    )
    assert "unstaged" in scopes, (
        f"No unstaged changes found. Scopes present: {scopes}"
    )


@then("the functions list includes a class-qualified method name")
def step_functions_include_qualified_name(context):
    data = _ensure_json_parsed(context)
    files = data.get("files", [])
    assert files, "No 'files' array in JSON output"

    for f in files:
        functions = f.get("functions_changed")
        if functions is None:
            continue
        for func in functions:
            name = func if isinstance(func, str) else func.get("name", "")
            # A class-qualified name contains a dot or double-colon separator
            if "." in name or "::" in name:
                return

    assert False, (
        "No class-qualified method name found in functions_changed. "
        f"Functions: {[f.get('functions_changed') for f in files]}"
    )


@then('the JSON value "commits" has length {count:d}')
def step_json_commits_has_length(context, count):
    data = _ensure_json_parsed(context)
    commits = data.get("commits", [])
    assert len(commits) == count, (
        f"Expected {count} commits, got {len(commits)}"
    )


@then('each commit entry has keys "metadata" and "files" and "summary"')
def step_each_commit_has_required_keys(context):
    data = _ensure_json_parsed(context)
    commits = data.get("commits", [])
    assert commits, "No 'commits' array in JSON output"

    for i, commit in enumerate(commits):
        for key in ("metadata", "files", "summary"):
            assert key in commit, (
                f"Commit {i} missing key '{key}'. "
                f"Available keys: {list(commit.keys())}"
            )
