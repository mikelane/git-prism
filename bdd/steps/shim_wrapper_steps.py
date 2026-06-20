"""Step definitions for the PATH-shim wrapper BDD scenarios.

The shim is invoked by creating a symlink tmpdir/bin/git -> git-prism binary
and pointing PATH at tmpdir/bin.  No Rust internals are imported — all
assertions are made by shelling out to the real compiled binary.
"""

from __future__ import annotations

import json
import os
import stat
import subprocess
import tempfile
from pathlib import Path
from typing import Any

from behave import given, then, when
from behave.runner import Context

from repo_setup_steps import _commit, _init_repo, _write_file

# Timeout guard — shim invocations should return almost immediately.
_SHIM_TIMEOUT_SECONDS: float = 30.0

# Agent-related environment variables that must be stripped before each
# scenario so the host shell's own agent environment cannot bleed in.
# CI is included because detect_calling_agent() treats any non-empty CI value
# as a global override that suppresses agent detection (returns None). GitHub
# Actions sets CI=true on every job, which would make every shim scenario fall
# through to passthrough instead of routing to structured JSON output.
_AGENT_ENV_VARS: tuple[str, ...] = (
    "CI",
    "CLAUDECODE",
    "AI_AGENT",
    "AGENT",
    "CURSOR_AGENT",
    "GEMINI_CLI",
    "CODEX_SANDBOX",
    "CLINE_ACTIVE",
    "AUGMENT_AGENT",
    "OPENCODE_CLIENT",
    "TRAE_AI_SHELL_ID",
    "GIT_PRISM_INSIDE_SHIM",
)


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------


def _find_binary(context: Context) -> Path:
    """Locate the git-prism binary, preferring release over debug.

    Raises AssertionError with a clear message when neither build exists so
    that the failure is actionable rather than a cryptic missing-symlink error.
    """
    release = Path(context.project_root) / "target" / "release" / "git-prism"
    debug = Path(context.project_root) / "target" / "debug" / "git-prism"
    if release.is_file():
        return release
    if debug.is_file():
        return debug
    raise AssertionError(
        f"git-prism binary not found at {release} or {debug}. "
        "Run `cargo build --release` (or `cargo build`) before running BDD tests."
    )


def _build_shim_dir(context: Context) -> Path:
    """Create a tempdir/bin/git symlink pointing at the git-prism binary.

    Returns the directory that contains the 'git' symlink (tmpdir/bin).
    The tempdir is registered in context.cleanup_dirs.
    """
    binary = _find_binary(context)
    tmpdir = tempfile.mkdtemp()
    context.cleanup_dirs.append(tmpdir)
    shim_dir = Path(tmpdir) / "bin"
    shim_dir.mkdir()
    shim_link = shim_dir / "git"
    shim_link.symlink_to(binary)
    context.shim_dir = shim_dir
    return shim_dir


def _clean_env() -> dict[str, str]:
    """Return a copy of os.environ with agent env vars stripped."""
    env = os.environ.copy()
    for var in _AGENT_ENV_VARS:
        env.pop(var, None)
    return env


def _run_shim(
    context: Context,
    args: list[str],
    extra_env: dict[str, str] | None = None,
) -> None:
    """Invoke the shim symlink (as 'git') with a controlled environment.

    Prepends the shim directory to PATH so that 'git' resolves to the shim.
    Strips all known agent env vars first, then applies extra_env on top.
    """
    shim_dir = _build_shim_dir(context)
    env = _clean_env()
    env["PATH"] = f"{shim_dir}:{env.get('PATH', '')}"
    if extra_env:
        env.update(extra_env)

    context.result = subprocess.run(  # noqa: S603
        ["git"] + args,
        capture_output=True,
        text=True,
        env=env,
        cwd=context.repo_path,
        check=False,
        timeout=_SHIM_TIMEOUT_SECONDS,
    )


# ---------------------------------------------------------------------------
# Given: fixture builders
# ---------------------------------------------------------------------------


@given("a fixture git repository with two commits")
def step_fixture_repo_two_commits(context: Context) -> None:
    """Create a temporary repo with two commits on distinct files."""
    repo_dir = _init_repo(context)
    _write_file(repo_dir, "hello.txt", "hello\n")
    _commit(repo_dir, "first commit", ["hello.txt"])
    _write_file(repo_dir, "world.txt", "world\n")
    _commit(repo_dir, "second commit", ["world.txt"])


@given("a fixture git repository with two commits and a tracked file")
def step_fixture_repo_with_tracked_file(context: Context) -> None:
    """Create a repo whose second commit contains README.md for blame to target."""
    repo_dir = _init_repo(context)
    _write_file(repo_dir, "hello.txt", "hello\n")
    _commit(repo_dir, "first commit", ["hello.txt"])
    _write_file(repo_dir, "README.md", "# Readme\nLine two.\n")
    _commit(repo_dir, "add readme", ["README.md"])


@given("a fixture git repository with two commits and a staged file")
def step_fixture_repo_with_staged_file(context: Context) -> None:
    """Create a repo with two commits plus a new file staged but not committed."""
    repo_dir = _init_repo(context)
    _write_file(repo_dir, "hello.txt", "hello\n")
    _commit(repo_dir, "first commit", ["hello.txt"])
    _write_file(repo_dir, "world.txt", "world\n")
    _commit(repo_dir, "second commit", ["world.txt"])
    # Stage a new file without committing
    _write_file(repo_dir, "staged.txt", "staged content\n")
    subprocess.run(  # noqa: S603
        ["git", "add", "staged.txt"],
        cwd=repo_dir,
        check=True,
        capture_output=True,
    )


@given("a fixture git repository with an unstaged modification")
def step_fixture_repo_unstaged_modification(context: Context) -> None:
    """Create a repo with one commit then modify a tracked file without staging."""
    repo_dir = _init_repo(context)
    _write_file(repo_dir, "tracked.txt", "original\n")
    _commit(repo_dir, "initial commit", ["tracked.txt"])
    # Modify without staging
    (Path(repo_dir) / "tracked.txt").write_text("modified\n")


@given("an env-dumper script that captures the child environment")
def step_env_dumper_script(context: Context) -> None:
    """Write a shell script that dumps env to a file then execs real git.

    The script is placed at context.dumper_output_file.  The shim symlink
    points at git-prism; the env-dumper is wired in via the When step.
    """
    tmpdir = tempfile.mkdtemp()
    context.cleanup_dirs.append(tmpdir)

    dumper_output = Path(tmpdir) / "env_dump.txt"
    context.dumper_output_file = str(dumper_output)

    # Write the env-dumper script. After dumping env it execs the real
    # git via a fallback chain that matches the eventual Rust resolver
    # (#286). Avoids hardcoding /usr/bin/git, which is wrong on Alpine,
    # Nix, and Homebrew-only macOS runners.
    dumper_script = Path(tmpdir) / "git"
    dumper_script.write_text(
        f'#!/bin/sh\n'
        f'env > "{dumper_output}"\n'
        f'for candidate in /usr/bin/git /usr/local/bin/git /opt/homebrew/bin/git; do\n'
        f'    [ -x "$candidate" ] && exec "$candidate" "$@"\n'
        f'done\n'
        f'echo "env-dumper: no git found in standard locations" >&2\n'
        f'exit 127\n'
    )
    dumper_script.chmod(dumper_script.stat().st_mode | stat.S_IEXEC)
    context.env_dumper_dir = Path(tmpdir)


@given("an isolated HOME directory")
def step_isolated_home(context: Context) -> None:
    """Create a tempdir to use as an isolated HOME for hooks install/uninstall."""
    tmpdir = tempfile.mkdtemp()
    context.cleanup_dirs.append(tmpdir)
    context.isolated_home = Path(tmpdir)


# ---------------------------------------------------------------------------
# When: invocations
# ---------------------------------------------------------------------------


def _parse_shim_command(command_str: str) -> list[str]:
    """Parse a quoted command string like 'git diff HEAD~1..HEAD' into args.

    Strips the leading 'git' token since the shim is invoked as 'git' and
    we pass only the subcommand + flags.
    """
    parts = command_str.split()
    # Drop the leading 'git' token — the shim IS git in the PATH
    if parts and parts[0] == "git":
        parts = parts[1:]
    return parts


@when('I run the shim as "{command}" with CLAUDECODE=1')
def step_run_shim_with_claudecode(context: Context, command: str) -> None:
    """Invoke the shim symlink as 'git' with CLAUDECODE=1 set."""
    _run_shim(context, _parse_shim_command(command), extra_env={"CLAUDECODE": "1"})


@when('I run the shim as "{command}" with CLAUDECODE=1 and GIT_PRISM_INSIDE_SHIM=1')
def step_run_shim_with_both_flags(context: Context, command: str) -> None:
    """Invoke the shim with both CLAUDECODE=1 and GIT_PRISM_INSIDE_SHIM=1."""
    _run_shim(
        context,
        _parse_shim_command(command),
        extra_env={"CLAUDECODE": "1", "GIT_PRISM_INSIDE_SHIM": "1"},
    )


@when('I run the shim as "{command}" without any agent env vars')
def step_run_shim_no_agent_vars(context: Context, command: str) -> None:
    """Invoke the shim with no agent env vars set (non-agent invocation).

    Always sets GIT_PRISM_DEBUG_RESOLVER=1 so the shim emits the resolved
    real-git path to stderr, enabling the resolver-path assertions.
    """
    _run_shim(
        context,
        _parse_shim_command(command),
        extra_env={"GIT_PRISM_DEBUG_RESOLVER": "1"},
    )


@when('I run the shim as "git" pointing at the env-dumper with CLAUDECODE=1')
def step_run_shim_with_env_dumper(context: Context) -> None:
    """Invoke the shim, with the env-dumper wired in as the real-git target.

    The shim directory contains the git-prism symlink (as 'git').
    The env-dumper directory also contains a 'git' script.  We put the shim
    dir first in PATH so the shim runs, then the shim's real-git resolver
    must skip its own dir and find the env-dumper's git next.
    """
    shim_dir = _build_shim_dir(context)
    dumper_dir = context.env_dumper_dir
    env = _clean_env()
    env["PATH"] = f"{shim_dir}:{dumper_dir}:{env.get('PATH', '')}"
    env["CLAUDECODE"] = "1"

    context.result = subprocess.run(  # noqa: S603
        ["git"],
        capture_output=True,
        text=True,
        env=env,
        cwd=context.repo_path,
        check=False,
        timeout=_SHIM_TIMEOUT_SECONDS,
    )


@when('I run "git-prism hooks install --path-shim" with the isolated HOME')
def step_hooks_install_path_shim(context: Context) -> None:
    """Run hooks install --path-shim with the isolated HOME directory."""
    binary = _find_binary(context)
    env = os.environ.copy()
    env["HOME"] = str(context.isolated_home)

    context.result = subprocess.run(  # noqa: S603
        [str(binary), "hooks", "install", "--path-shim"],
        capture_output=True,
        text=True,
        env=env,
        check=False,
        timeout=_SHIM_TIMEOUT_SECONDS,
    )


@when('I run "git-prism hooks install --path-shim" with the isolated HOME again')
def step_hooks_install_path_shim_again(context: Context) -> None:
    """Run hooks install --path-shim a second time (idempotency check)."""
    binary = _find_binary(context)
    env = os.environ.copy()
    env["HOME"] = str(context.isolated_home)

    context.result = subprocess.run(  # noqa: S603
        [str(binary), "hooks", "install", "--path-shim"],
        capture_output=True,
        text=True,
        env=env,
        check=False,
        timeout=_SHIM_TIMEOUT_SECONDS,
    )


@when('I run "git-prism hooks uninstall --path-shim" with the isolated HOME')
def step_hooks_uninstall_path_shim(context: Context) -> None:
    """Run hooks uninstall --path-shim with the isolated HOME directory."""
    binary = _find_binary(context)
    env = os.environ.copy()
    env["HOME"] = str(context.isolated_home)

    context.result = subprocess.run(  # noqa: S603
        [str(binary), "hooks", "uninstall", "--path-shim"],
        capture_output=True,
        text=True,
        env=env,
        check=False,
        timeout=_SHIM_TIMEOUT_SECONDS,
    )


@when('I run "git-prism hooks status" with the isolated HOME')
def step_hooks_status_with_isolated_home(context: Context) -> None:
    """Run hooks status with the isolated HOME directory."""
    binary = _find_binary(context)
    env = os.environ.copy()
    env["HOME"] = str(context.isolated_home)

    context.result = subprocess.run(  # noqa: S603
        [str(binary), "hooks", "status"],
        capture_output=True,
        text=True,
        env=env,
        check=False,
        timeout=_SHIM_TIMEOUT_SECONDS,
    )


# ---------------------------------------------------------------------------
# Then: assertions
# ---------------------------------------------------------------------------


def _parse_result_json(context: Context) -> Any:
    """Parse stdout as JSON, raising AssertionError with context on failure."""
    try:
        return json.loads(context.result.stdout)
    except json.JSONDecodeError as exc:
        raise AssertionError(
            f"Output is not valid JSON: {exc}\n"
            f"stdout: {context.result.stdout[:500]}\n"
            f"stderr: {context.result.stderr[:500]}"
        ) from exc


# NOTE: the following steps are already defined in other step files and must
# not be redefined here:
#   "the exit code is {code:d}"     → cli_steps.py
#   'the output contains "{text}"'  → cli_steps.py
#   "the output is valid JSON"      → json_steps.py


@then("the output is not JSON")
def step_output_is_not_json(context: Context) -> None:
    """Assert that stdout is NOT valid JSON (plain git output).

    Any successful ``json.loads`` fails the assertion — including scalar
    JSON values like ``null``, ``42``, or ``true`` that would otherwise
    slip past a dict/list-only check.
    """
    stdout = (context.result.stdout or "").strip()
    try:
        json.loads(stdout)
    except json.JSONDecodeError:
        return  # Expected — output is not JSON, scenario passes
    raise AssertionError(
        f"Expected non-JSON output (passthrough from real git) but stdout "
        f"parsed as JSON: {stdout[:500]!r}"
    )


@then('the JSON output has a "files" array')
def step_json_has_files_array(context: Context) -> None:
    """Assert the JSON output contains a top-level 'files' key holding a list."""
    data = _parse_result_json(context)
    assert isinstance(data, dict) and isinstance(data.get("files"), list), (
        f"Expected JSON with 'files' array, got: {str(data)[:500]}"
    )


@then('the JSON output has a "commits" array')
def step_json_has_commits_array(context: Context) -> None:
    """Assert the JSON output contains a top-level 'commits' key holding a list."""
    data = _parse_result_json(context)
    assert isinstance(data, dict) and isinstance(data.get("commits"), list), (
        f"Expected JSON with 'commits' array, got: {str(data)[:500]}"
    )


@then('the JSON output has a "function_context" shape')
def step_json_has_function_context(context: Context) -> None:
    """Assert the JSON output has the function-context wire shape.

    build_function_context_with_options returns {"functions": [...], ...}.
    The Gherkin step says "function_context shape" as a semantic label;
    the actual top-level key in the JSON response is "functions".
    """
    data = _parse_result_json(context)
    assert isinstance(data, dict) and isinstance(data.get("functions"), list), (
        f"Expected JSON with 'functions' list (function_context shape), got: {str(data)[:500]}"
    )


@then('the JSON output has a "snapshots" key')
def step_json_has_snapshots_key(context: Context) -> None:
    """Assert the JSON output has the snapshot wire shape.

    build_snapshots returns {"files": [...], ...}.
    The Gherkin step says "snapshots key" as a semantic label;
    the actual top-level key in the JSON response is "files".
    """
    data = _parse_result_json(context)
    assert isinstance(data, dict) and isinstance(data.get("files"), list), (
        f"Expected JSON with 'files' list (snapshot shape), got: {str(data)[:500]}"
    )


# NOTE: 'the output contains "{text}"' is already defined in cli_steps.py.


@then("the new commit exists in the repository")
def step_new_commit_exists(context: Context) -> None:
    """Assert that git log shows at least one more commit than before the When step.

    We simply check that git log runs cleanly and produces output — the
    shim test creates a commit from a repo that already had two, so three
    commits must exist.
    """
    result = subprocess.run(  # noqa: S603
        ["git", "log", "--oneline"],
        capture_output=True,
        text=True,
        cwd=context.repo_path,
        check=False,
    )
    lines = [ln for ln in result.stdout.strip().splitlines() if ln]
    assert len(lines) >= 3, (  # two fixture commits + one from the shim
        f"Expected at least 3 commits after shim git commit, got {len(lines)}:\n"
        f"{result.stdout}"
    )


@then('the env-dumper output contains "GIT_PRISM_INSIDE_SHIM=1"')
def step_env_dumper_contains_sentinel(context: Context) -> None:
    """Assert the env-dumper output file records GIT_PRISM_INSIDE_SHIM=1."""
    dumper_file = Path(context.dumper_output_file)
    assert dumper_file.exists(), (
        f"Env-dumper output file does not exist: {dumper_file}\n"
        f"shim stdout: {context.result.stdout}\n"
        f"shim stderr: {context.result.stderr}"
    )
    contents = dumper_file.read_text()
    assert "GIT_PRISM_INSIDE_SHIM=1" in contents, (
        f"GIT_PRISM_INSIDE_SHIM=1 not found in env-dumper output:\n{contents}"
    )


@then("the resolved real-git binary path does not live in the shim directory")
def step_real_git_not_in_shim_dir(context: Context) -> None:
    """Assert the shim resolved real git to a path outside the shim directory.

    Reads the 'resolved real git to <path>' line emitted by GIT_PRISM_DEBUG_RESOLVER
    and asserts the resolved path is not inside context.shim_dir.
    """
    stderr = context.result.stderr
    prefix = "git-prism shim: resolved real git to "
    resolved_path = None
    for line in stderr.splitlines():
        if line.startswith(prefix):
            resolved_path = Path(line[len(prefix):].strip())
            break

    assert resolved_path is not None, (
        f"Expected 'resolved real git to <path>' in shim stderr but found none.\n"
        f"stderr: {stderr}"
    )

    shim_dir = context.shim_dir
    assert not str(resolved_path).startswith(str(shim_dir)), (
        f"Resolved real git '{resolved_path}' lives inside the shim directory '{shim_dir}'. "
        "The resolver must skip the shim dir and find git elsewhere."
    )


@then('the path "$HOME/{rel_path}" is a symlink')
def step_path_is_symlink(context: Context, rel_path: str) -> None:
    """Assert that $HOME/<rel_path> is a symlink (using the isolated HOME)."""
    target = context.isolated_home / rel_path
    assert target.is_symlink(), (
        f"Expected {target} to be a symlink, but it does not exist or is not a symlink.\n"
        f"stdout: {context.result.stdout}\n"
        f"stderr: {context.result.stderr}"
    )


@then('the path "$HOME/{rel_path}" does not exist')
def step_path_does_not_exist(context: Context, rel_path: str) -> None:
    """Assert that $HOME/<rel_path> does not exist."""
    target = context.isolated_home / rel_path
    assert not target.exists(), (
        f"Expected {target} to not exist, but it does.\n"
        f"stdout: {context.result.stdout}\n"
        f"stderr: {context.result.stderr}"
    )


@then('the directory "$HOME/{rel_path}" does not exist')
def step_directory_does_not_exist(context: Context, rel_path: str) -> None:
    """Assert that $HOME/<rel_path> directory does not exist."""
    target = context.isolated_home / rel_path
    assert not target.exists(), (
        f"Expected directory {target} to not exist, but it does.\n"
        f"stdout: {context.result.stdout}\n"
        f"stderr: {context.result.stderr}"
    )


@then('the hooks status output mentions "path-shim"')
def step_hooks_status_mentions_path_shim(context: Context) -> None:
    """Assert the hooks status output contains the text 'path-shim'."""
    full_output = context.result.stdout + context.result.stderr
    assert "path-shim" in full_output, (
        f"'path-shim' not found in hooks status output:\n{full_output}"
    )


@then("the output does not contain the handler error marker")
def step_output_no_handler_error(context: Context) -> None:
    """Assert that stderr contains no 'git-prism shim: handler error' message.

    This string appears when the shim intercepts a command but its handler
    fails (e.g. peel-to-commit on a blob spec).  Its absence confirms the
    shim either passed through cleanly or handled the command without error.
    """
    error_marker = "git-prism shim: handler error"
    stderr = context.result.stderr or ""
    assert error_marker not in stderr, (
        f"Unexpected handler error in shim stderr:\n{stderr}"
    )


@then("the hooks status output indicates path-shim is not installed")
def step_hooks_status_not_installed(context: Context) -> None:
    """Assert the hooks status output reports path-shim as not installed.

    Requires both ``path-shim`` and a not-installed phrase on the same
    line so the existing bash-redirect-hook's ``not installed`` message
    can't trigger a false GREEN before the implementation lands.
    """
    full_output = context.result.stdout + context.result.stderr
    path_shim_lines = [
        line for line in full_output.splitlines() if "path-shim" in line.lower()
    ]
    assert path_shim_lines, (
        f"Expected a line mentioning 'path-shim' in hooks status output, "
        f"got:\n{full_output}"
    )
    assert any(
        "not installed" in line.lower() or "absent" in line.lower()
        for line in path_shim_lines
    ), (
        f"Expected a 'path-shim' line indicating it is not installed; "
        f"matching lines were: {path_shim_lines!r}"
    )
