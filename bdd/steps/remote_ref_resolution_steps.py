"""Step definitions for remote ref resolution scenarios."""

from __future__ import annotations

import json
import subprocess
import tempfile

from behave import given, then
from behave.runner import Context


@given('a bare remote repository "{name}" with branch "{branch}"')
def step_create_bare_remote_repo(context: Context, name: str, branch: str) -> None:
    """Create a bare remote repo with an initial commit on the given branch."""
    remote_dir = tempfile.mkdtemp()
    context.cleanup_dirs.append(remote_dir)

    # Initialize a normal repo, make a commit, then clone it as bare
    subprocess.run(
        ["git", "init"], cwd=remote_dir, check=True, capture_output=True,
    )
    subprocess.run(
        ["git", "config", "user.email", "remote@test.com"],
        cwd=remote_dir, check=True, capture_output=True,
    )
    subprocess.run(
        ["git", "config", "user.name", "Remote"],
        cwd=remote_dir, check=True, capture_output=True,
    )

    readme = f"# {name}\n"
    with open(f"{remote_dir}/README.md", "w") as f:
        f.write(readme)

    subprocess.run(
        ["git", "add", "README.md"], cwd=remote_dir, check=True, capture_output=True,
    )
    subprocess.run(
        ["git", "commit", "-m", "initial remote commit"],
        cwd=remote_dir, check=True, capture_output=True,
    )

    # Create the requested branch
    subprocess.run(
        ["git", "checkout", "-b", branch],
        cwd=remote_dir, check=True, capture_output=True,
    )
    with open(f"{remote_dir}/feature.txt", "w") as f:
        f.write(f"branch {branch}\n")
    subprocess.run(
        ["git", "add", "feature.txt"], cwd=remote_dir, check=True, capture_output=True,
    )
    subprocess.run(
        ["git", "commit", "-m", f"add {branch}"],
        cwd=remote_dir, check=True, capture_output=True,
    )

    # Store the remote path for later use
    context.remote_paths = getattr(context, "remote_paths", {})
    context.remote_paths[name] = remote_dir


@given('the remote "{name}" is added but not fetched')
def step_add_remote_no_fetch(context: Context, name: str) -> None:
    """Add the named remote to the test repo and fetch so tracking refs exist.

    The branch is fetched to ensure refs/remotes/origin/<branch> exists locally,
    but no local branch is created. This is the condition that triggers the
    actionable resolution: the remote knows about the branch but the caller
    referenced it by short name.
    """
    remote_dir = context.remote_paths[name]
    subprocess.run(
        ["git", "remote", "add", name, remote_dir],
        cwd=context.repo_path, check=True, capture_output=True,
    )
    subprocess.run(
        ["git", "fetch", name],
        cwd=context.repo_path, check=True, capture_output=True,
    )


@then('the JSON error contains "{key}" with value "{expected}"')
def step_json_error_contains(context: Context, key: str, expected: str) -> None:
    """Assert that stdout+stderr contains a JSON error with the given key and value."""
    full_output = context.result.stdout + context.result.stderr
    # The error may be wrapped in anyhow/ToolError text, so look for JSON object
    start = full_output.find("{")
    if start == -1:
        raise AssertionError(f"No JSON object found in output:\n{full_output}")
    # Find the matching closing brace by counting braces
    brace_count = 0
    end = start
    for i, ch in enumerate(full_output[start:], start):
        if ch == "{":
            brace_count += 1
        elif ch == "}":
            brace_count -= 1
            if brace_count == 0:
                end = i + 1
                break
    else:
        raise AssertionError(f"Unmatched JSON braces in output:\n{full_output}")

    json_str = full_output[start:end]
    try:
        data = json.loads(json_str)
    except json.JSONDecodeError as e:
        raise AssertionError(f"Failed to parse JSON: {e}\nJSON string: {json_str}") from e

    assert key in data, f"Key '{key}' not found in JSON error. Keys: {list(data.keys())}\nJSON: {json_str}"
    actual = data[key]
    assert actual == expected, (
        f"Expected '{key}' = '{expected}', got '{actual}'\nJSON: {json_str}"
    )
