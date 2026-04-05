"""Step definitions for CLI invocation and output assertions."""

import os
import subprocess
import tempfile

from behave import given, then, when, use_step_matcher


@given("a git repository with two commits")
def step_create_test_repo(context):
    tmp = tempfile.mkdtemp()
    context.cleanup_dirs.append(tmp)

    rust_initial = """\
fn greet(name: &str) -> String {
    format!("Hello, {}!", name)
}

fn main() {
    println!("{}", greet("world"));
}
"""
    rust_modified = """\
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
    subprocess.run(["git", "init"], cwd=tmp, check=True, capture_output=True)
    subprocess.run(
        ["git", "config", "user.email", "test@test.com"],
        cwd=tmp,
        check=True,
        capture_output=True,
    )
    subprocess.run(
        ["git", "config", "user.name", "Test"],
        cwd=tmp,
        check=True,
        capture_output=True,
    )

    main_rs = os.path.join(tmp, "main.rs")

    with open(main_rs, "w") as f:
        f.write(rust_initial)
    subprocess.run(["git", "add", "main.rs"], cwd=tmp, check=True, capture_output=True)
    subprocess.run(
        ["git", "commit", "-m", "initial commit"],
        cwd=tmp,
        check=True,
        capture_output=True,
    )

    with open(main_rs, "w") as f:
        f.write(rust_modified)
    subprocess.run(["git", "add", "main.rs"], cwd=tmp, check=True, capture_output=True)
    subprocess.run(
        ["git", "commit", "-m", "add farewell function"],
        cwd=tmp,
        check=True,
        capture_output=True,
    )

    context.repo_path = tmp


# Use regex matcher to disambiguate the two "I run" step patterns
use_step_matcher("re")


@when(r'I run "(?P<command>[^"]+)" in "(?P<directory>[^"]+)"')
def step_run_command_in_dir(context, command, directory):
    parts = command.split()
    if parts[0] == "git-prism":
        parts[0] = context.binary_path

    # For commands with explicit directory, use --repo to point there
    if _command_accepts_repo(parts):
        parts.extend(["--repo", directory])

    result = subprocess.run(
        parts, capture_output=True, text=True, cwd=directory
    )
    context.result = result


@when(r'I run "(?P<command>[^"]+)"')
def step_run_command(context, command):
    parts = command.split()
    if parts[0] == "git-prism":
        parts[0] = context.binary_path

    run_dir = getattr(context, "repo_path", context.project_root)

    # For commands that accept --repo, inject it when running against a test repo
    repo_path = getattr(context, "repo_path", None)
    if repo_path and _command_accepts_repo(parts):
        parts.extend(["--repo", repo_path])

    result = subprocess.run(parts, capture_output=True, text=True, cwd=run_dir)
    context.result = result


# Restore default matcher for remaining steps
use_step_matcher("parse")


@then("the exit code is {code:d}")
def step_check_exit_code(context, code):
    assert context.result.returncode == code, (
        f"Expected exit code {code}, got {context.result.returncode}\n"
        f"stdout: {context.result.stdout}\n"
        f"stderr: {context.result.stderr}"
    )


@then("the exit code is not {code:d}")
def step_check_exit_code_not(context, code):
    assert context.result.returncode != code, (
        f"Expected exit code other than {code}, but got {code}\n"
        f"stdout: {context.result.stdout}\n"
        f"stderr: {context.result.stderr}"
    )


@then('the output contains "{text}"')
def step_output_contains(context, text):
    full_output = context.result.stdout + context.result.stderr
    assert text in full_output, (
        f"'{text}' not found in output:\n{full_output}"
    )


@then('the output does not contain "{text}"')
def step_output_not_contains(context, text):
    full_output = context.result.stdout + context.result.stderr
    assert text not in full_output, (
        f"'{text}' found in output but should not be:\n{full_output}"
    )


def _command_accepts_repo(parts: list[str]) -> bool:
    """Check if the command is a subcommand that accepts --repo."""
    binary_basename = os.path.basename(parts[0])
    if binary_basename != "git-prism" and not parts[0].endswith("git-prism"):
        return False
    if "--repo" in parts:
        return False
    subcommands_with_repo = {"manifest", "snapshot", "history"}
    return len(parts) > 1 and parts[1] in subcommands_with_repo
