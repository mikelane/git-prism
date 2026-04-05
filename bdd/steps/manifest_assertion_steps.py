"""Step definitions for manifest-specific JSON assertions.

These steps validate structural properties of manifest JSON output
that go beyond the generic JSON assertions in json_steps.py.
"""

import json
import re
import urllib.request

from behave import then, when


def _ensure_json_parsed(context):
    """Return parsed JSON, parsing from stdout if not already cached."""
    if not hasattr(context, "json_data") or context.json_data is None:
        context.json_data = json.loads(context.result.stdout)
    return context.json_data


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
    scopes = set()
    for f in files:
        scope = f.get("change_scope")
        if scope:
            scopes.add(scope)

    assert "staged" in scopes or "Staged" in scopes, (
        f"No staged changes found. Scopes present: {scopes}"
    )
    assert "unstaged" in scopes or "Unstaged" in scopes, (
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


@when('I query crates.io for package "{name}"')
def step_query_crates_io(context, name):
    url = f"https://crates.io/api/v1/crates/{name}"
    req = urllib.request.Request(url, headers={"User-Agent": "git-prism-bdd-test"})
    try:
        with urllib.request.urlopen(req, timeout=10) as resp:
            context.crates_io_response = json.loads(resp.read())
            context.crates_io_status = resp.status
    except urllib.error.HTTPError as e:
        context.crates_io_response = None
        context.crates_io_status = e.code


@then("the package exists on crates.io")
def step_package_exists_on_crates_io(context):
    assert context.crates_io_status == 200, (
        f"Package not found on crates.io (HTTP {context.crates_io_status})"
    )
    assert context.crates_io_response is not None, "No response from crates.io"
    assert "crate" in context.crates_io_response, (
        f"Unexpected response: {context.crates_io_response}"
    )


@then('the languages list includes "{language}"')
def step_languages_list_includes(context, language):
    """Match a language name at the start of a line in the languages output.

    The languages command outputs lines like:
      go         (.go)
      python     (.py)
    We match the language name as a whole word at the start (after whitespace).
    """
    output = context.result.stdout
    pattern = rf"^\s+{re.escape(language)}\s"
    assert re.search(pattern, output, re.MULTILINE), (
        f"Language '{language}' not found in languages output:\n{output}"
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
