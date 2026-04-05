"""Step definitions for CLI invocation and output assertions."""

from __future__ import annotations

import re
import subprocess
from pathlib import Path

from behave import given, then, use_step_matcher, when
from behave.runner import Context

from repo_setup_steps import _commit, _init_repo, _write_file

RUST_INITIAL = """\
fn greet(name: &str) -> String {
    format!("Hello, {}!", name)
}

fn main() {
    println!("{}", greet("world"));
}
"""

RUST_MODIFIED = """\
fn greet(name: &str) -> String {
    format!("Hello, {}!", name)
}

fn farewell(name: &str) -> String {
    format!("Goodbye, {}!", name)
}

fn main() {
    println!("{}", greet("world"));
    println!("{}", farewell("world"));
}
"""


@given("a git repository with two commits")
def step_create_test_repo(context: Context) -> None:
    """Create a temporary git repo with two commits modifying a Rust file."""
    repo_dir = _init_repo(context)

    _write_file(repo_dir, "main.rs", RUST_INITIAL)
    _commit(repo_dir, "initial commit", ["main.rs"])

    _write_file(repo_dir, "main.rs", RUST_MODIFIED)
    _commit(repo_dir, "add farewell function", ["main.rs"])


# Use regex matcher to disambiguate the two "I run" step patterns
use_step_matcher("re")


@when(r'I run "(?P<command>[^"]+)" in "(?P<directory>[^"]+)"')
def step_run_command_in_dir(context: Context, command: str, directory: str) -> None:
    """Run a CLI command in a specific directory."""
    parts = command.split()
    if parts[0] == "git-prism":
        parts[0] = context.binary_path

    # For commands with explicit directory, use --repo to point there
    if _command_accepts_repo(parts):
        parts.extend(["--repo", directory])

    context.result = subprocess.run(
        parts, capture_output=True, text=True, cwd=directory
    )


@when(r'I run "(?P<command>[^"]+)"')
def step_run_command(context: Context, command: str) -> None:
    """Run a CLI command, defaulting to the test repo directory."""
    parts = command.split()
    if parts[0] == "git-prism":
        parts[0] = context.binary_path

    run_dir: str = getattr(context, "repo_path", context.project_root)

    # For commands that accept --repo, inject it when running against a test repo
    repo_path: str | None = getattr(context, "repo_path", None)
    if repo_path and _command_accepts_repo(parts):
        parts.extend(["--repo", repo_path])

    context.result = subprocess.run(parts, capture_output=True, text=True, cwd=run_dir)


# Restore default matcher for remaining steps
use_step_matcher("parse")


@then("the exit code is {code:d}")
def step_check_exit_code(context: Context, code: int) -> None:
    """Assert that the process exited with the expected code."""
    assert context.result.returncode == code, (
        f"Expected exit code {code}, got {context.result.returncode}\n"
        f"stdout: {context.result.stdout}\n"
        f"stderr: {context.result.stderr}"
    )


@then("the exit code is not {code:d}")
def step_check_exit_code_not(context: Context, code: int) -> None:
    """Assert that the process did NOT exit with the given code."""
    assert context.result.returncode != code, (
        f"Expected exit code other than {code}, but got {code}\n"
        f"stdout: {context.result.stdout}\n"
        f"stderr: {context.result.stderr}"
    )


@then('the output contains "{text}"')
def step_output_contains(context: Context, text: str) -> None:
    """Assert that stdout+stderr contains the expected text."""
    full_output = context.result.stdout + context.result.stderr
    assert text in full_output, (
        f"'{text}' not found in output:\n{full_output}"
    )


@then('the output does not contain "{text}"')
def step_output_not_contains(context: Context, text: str) -> None:
    """Assert that stdout+stderr does NOT contain the given text."""
    full_output = context.result.stdout + context.result.stderr
    assert text not in full_output, (
        f"'{text}' found in output but should not be:\n{full_output}"
    )


@then("the stderr is not empty")
def step_stderr_not_empty(context: Context) -> None:
    """Assert that stderr contains at least some output.

    Error scenarios must produce a diagnostic message on stderr.
    A silent failure (exit code != 0 with empty stderr) is not
    a helpful error -- it leaves the user with no information.
    """
    assert context.result.stderr.strip(), (
        f"stderr is empty -- the command failed silently with exit code "
        f"{context.result.returncode}. Error scenarios must produce a "
        f"diagnostic message.\nstdout: {context.result.stdout}"
    )


def _command_accepts_repo(parts: list[str]) -> bool:
    """Check if the command is a subcommand that accepts --repo.

    Args:
        parts: The split command-line tokens, where parts[0] is the binary.

    Returns:
        True if the command is a git-prism subcommand that accepts --repo
        and --repo is not already present.
    """
    binary_name = Path(parts[0]).name
    if binary_name != "git-prism" and not parts[0].endswith("git-prism"):
        return False
    if "--repo" in parts:
        return False
    subcommands_with_repo = {"manifest", "snapshot", "history"}
    return len(parts) > 1 and parts[1] in subcommands_with_repo


@then('the languages list includes "{language}"')
def step_languages_list_includes(context: Context, language: str) -> None:
    """Assert that a language appears in the languages command output.

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
