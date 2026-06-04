"""Step definitions for the shim parity BDD scenarios (epic #328).

Covers:
  @ISSUE-322 -- Windows passthrough parity (spawn+wait exit code + I/O)
  @ISSUE-323 -- gh CLI interception via shim symlink
  @ISSUE-324 -- git-prism shim install|uninstall|status subcommand
  @ISSUE-325 -- auto-PATH setup UX during shim install
  @ISSUE-326 -- redirect hook deprecation and removal

All steps shell out to the real compiled git-prism binary or to symlinks in
per-scenario tempdirs. No Rust internals are imported. Steps fail with
assertion errors because the features do not exist yet — that is the point.

Platform note for @ISSUE-322:
  These steps are technically executable on unix, but the scenarios are tagged
  @windows because the Windows code path (spawn+wait, no execvp) is what is
  under test. On unix the spawn+wait path also works, so the scenarios will
  PASS locally. They are NOT genuinely RED on unix — the Windows CI job
  (windows-latest) is the authoritative RED gate.
"""

from __future__ import annotations

import os
import re
import shutil
import stat
import subprocess
import tempfile
from pathlib import Path

from behave import given, then, when
from behave.runner import Context

from shim_wrapper_steps import (
    _SHIM_TIMEOUT_SECONDS,
    _build_shim_dir,
    _clean_env,
    _find_binary,
)

# ---------------------------------------------------------------------------
# Shared helpers
# ---------------------------------------------------------------------------

_SHIM_DIR_RELATIVE = ".local/share/git-prism/bin"
_SHIM_EXPORT_FRAGMENT = ".local/share/git-prism/bin"

_SHA_PATTERN = re.compile(r"^[0-9a-f]{40}$")


def _install_stub_gh(context: Context, script: str) -> Path:
    """Write a stub gh shell script into a fresh tempdir/bin and make it executable.

    Returns the path to the stub binary.  Registers the tempdir for cleanup and
    sets context.stub_gh_dir / context.stub_gh_path.
    """
    tmpdir = tempfile.mkdtemp()
    context.cleanup_dirs.append(tmpdir)
    stub_dir = Path(tmpdir) / "bin"
    stub_dir.mkdir()
    stub_path = stub_dir / "gh"
    stub_path.write_text(script)
    stub_path.chmod(stub_path.stat().st_mode | stat.S_IXUSR | stat.S_IXGRP | stat.S_IXOTH)
    context.stub_gh_dir = stub_dir
    context.stub_gh_path = stub_path
    return stub_path


def _run_binary(
    context: Context,
    args: list[str],
    *,
    env: dict[str, str] | None = None,
    cwd: str | None = None,
) -> subprocess.CompletedProcess[str]:
    """Run the git-prism binary with given args, storing result in context."""
    binary = _find_binary(context)
    run_env = os.environ.copy() if env is None else env
    result = subprocess.run(  # noqa: S603
        [str(binary), *args],
        capture_output=True,
        text=True,
        env=run_env,
        cwd=cwd,
        check=False,
        timeout=_SHIM_TIMEOUT_SECONDS,
    )
    context.result = result
    return result


def _isolated_env(context: Context) -> dict[str, str]:
    """Return env dict with HOME overridden to context.isolated_home."""
    env = os.environ.copy()
    env["HOME"] = str(context.isolated_home)
    return env


def _shim_dir_path(context: Context) -> Path:
    """Absolute path to the shim bin directory inside isolated_home."""
    return context.isolated_home / _SHIM_DIR_RELATIVE


# ---------------------------------------------------------------------------
# Given: shared setup steps (reuse isolated_home from shim_wrapper_steps
# where the step text matches — those are already registered there)
# ---------------------------------------------------------------------------


# NOTE: "a fixture git repository with two commits" is already defined in
# shim_wrapper_steps.py. The @ISSUE-322 scenarios reuse that step — do NOT
# redefine it here. behave loads all step files, so it is available.


@given("a shell rc file exists at \"{rel_path}\" under HOME")
def step_create_rc_file(context: Context, rel_path: str) -> None:
    """Create a minimal shell rc file in the isolated HOME."""
    rc_path = context.isolated_home / rel_path
    rc_path.parent.mkdir(parents=True, exist_ok=True)
    if not rc_path.exists():
        rc_path.write_text("# shell rc\n")
    context.rc_path = rc_path
    context.rc_path_initial_content = rc_path.read_text()


@given("the shim directory is not in PATH")
def step_shim_dir_not_in_path(context: Context) -> None:
    """Assert that the shim dir is absent from PATH (it always is in a fresh isolated HOME)."""
    shim_dir = str(_shim_dir_path(context))
    current_path = os.environ.get("PATH", "")
    assert shim_dir not in current_path, (
        f"Shim directory {shim_dir!r} is already in PATH. "
        "This step requires a fresh isolated HOME where the shim is not installed."
    )


@given("the shim directory is already in PATH")
def step_shim_dir_already_in_path(context: Context) -> None:
    """Inject the shim dir into PATH for this scenario.

    Stores the modified PATH in context so the When step can inject it.
    """
    shim_dir = str(_shim_dir_path(context))
    shim_dir_path = _shim_dir_path(context)
    shim_dir_path.mkdir(parents=True, exist_ok=True)
    context.injected_path = f"{shim_dir}:{os.environ.get('PATH', '')}"


@given("the shim is installed with a \"gh\" symlink")
def step_shim_installed_with_gh_symlink(context: Context) -> None:
    """Create a tempdir/bin/gh symlink pointing at the git-prism binary."""
    binary = _find_binary(context)
    tmpdir = tempfile.mkdtemp()
    context.cleanup_dirs.append(tmpdir)
    shim_dir = Path(tmpdir) / "bin"
    shim_dir.mkdir()
    gh_link = shim_dir / "gh"
    gh_link.symlink_to(binary)
    context.gh_shim_dir = shim_dir
    context.gh_shim_link = gh_link


@given("a stub gh binary is installed that knows the fixture PR SHAs")
def step_stub_gh_with_pr_shas(context: Context) -> None:
    """Create a stub gh script that returns fixture repo commit SHAs for pr view.

    The stub lives in its own tempdir/bin directory (separate from the shim dir)
    so PATH ordering controls which 'gh' wins: stub_gh_dir must come before
    gh_shim_dir so that when the shim (argv0=gh) calls Command::new("gh") to
    resolve the PR range, it hits this stub — not a recursive shim invocation.

    The stub handles:
      pr view <N> --json baseRefOid,headRefOid  -> fixture base..head SHAs
      anything else                             -> exit 0, empty output
    """
    repo_path = context.repo_path
    # Capture the two commit SHAs from the fixture repo (base=HEAD~1, head=HEAD).
    base_sha = subprocess.check_output(
        ["git", "rev-parse", "HEAD~1"], cwd=repo_path, text=True
    ).strip()
    head_sha = subprocess.check_output(
        ["git", "rev-parse", "HEAD"], cwd=repo_path, text=True
    ).strip()
    context.fixture_base_sha = base_sha
    context.fixture_head_sha = head_sha

    assert _SHA_PATTERN.fullmatch(base_sha), (
        f"base_sha has unexpected shape (want 40 hex chars): {base_sha!r}"
    )
    assert _SHA_PATTERN.fullmatch(head_sha), (
        f"head_sha has unexpected shape (want 40 hex chars): {head_sha!r}"
    )

    stub_script = f"""\
#!/bin/sh
# Stub gh — hermetic, no network, no auth required.
if [ "$1" = "pr" ] && [ "$2" = "view" ] && [ "$4" = "--json" ]; then
    printf '%s' '{{"baseRefOid":"{base_sha}","headRefOid":"{head_sha}"}}'
    exit 0
fi
echo "stub gh: unhandled args: $*" >&2
exit 64
"""
    _install_stub_gh(context, stub_script)


@given("a stub gh binary is installed for passthrough comparison")
def step_stub_gh_for_passthrough(context: Context) -> None:
    """Create a stub gh script with canned deterministic output for passthrough scenarios.

    The stub handles the specific subcommands used by passthrough scenarios:
      repo view --json name     -> {"name":"stub-repo"}
      issue list --limit 1     -> []
      anything else            -> exit 0, empty output

    This stub is placed in stub_gh_dir and also recorded as stub_gh_path so
    the 'the stub gh binary runs ... directly' When step can invoke it directly.
    """
    stub_script = """\
#!/bin/sh
# Stub gh for passthrough parity — canned output, no network.
if [ "$1" = "repo" ] && [ "$2" = "view" ]; then
    printf '%s' '{"name":"stub-repo"}'
    exit 0
fi
if [ "$1" = "issue" ] && [ "$2" = "list" ]; then
    printf '%s' '[]'
    exit 0
fi
echo "stub gh: unhandled args: $*" >&2
exit 64
"""
    _install_stub_gh(context, stub_script)


@given("a real gh binary is available on PATH")
def step_real_gh_available(context: Context) -> None:
    """Assert the real gh binary is on PATH; skip with an informative error if not."""
    real_gh = shutil.which("gh")
    assert real_gh is not None, (
        "The real 'gh' binary is not on PATH. "
        "@ISSUE-323 scenarios require a real gh installation. "
        "Install gh (https://cli.github.com/) to run these scenarios."
    )
    context.real_gh_path = real_gh


# NOTE: "an isolated HOME with an empty .claude directory" is already defined
# in redirect_hook_steps.py. The @ISSUE-326 scenarios reuse that step.


@given("the redirect hook files have been removed from the installation")
def step_redirect_hook_files_removed(context: Context) -> None:
    """Simulate post-removal state by recording that hooks files are absent.

    This step does NOT modify the real installation. Instead it sets a
    context flag that the When step uses to run git-prism from a fake
    installation directory where bash_redirect_hook.py has been deleted.
    The implementation will be exercised through a real binary invocation.

    # TODO(#326): fake_install_root is built here but the When step runs the
    # real binary without wiring it in. When #326 lands, the implementer must
    # arrange for the binary invocation to exercise a post-removal code path
    # (e.g. by building a stripped binary or using an env var that skips the
    # hook file lookup) so this scenario tests the real removed state rather
    # than just asserting that the real project hooks/ dir lacks the file.
    """
    # Build a fake installation root that looks like a post-removal layout:
    # the hooks/ directory exists but bash_redirect_hook.py is absent.
    tmpdir = tempfile.mkdtemp()
    context.cleanup_dirs.append(tmpdir)
    fake_install = Path(tmpdir) / "fake-install"
    fake_install.mkdir()
    hooks_dir = fake_install / "hooks"
    hooks_dir.mkdir()
    # bash_redirect_hook.py intentionally NOT created — that is the scenario.
    context.fake_install_root = fake_install
    context.hooks_dir_path = hooks_dir

    # Also set up an isolated_home for the When step.
    if not hasattr(context, "isolated_home") or context.isolated_home is None:
        home_tmp = tempfile.mkdtemp()
        context.cleanup_dirs.append(home_tmp)
        context.isolated_home = Path(home_tmp)


# ---------------------------------------------------------------------------
# When: invocations
# ---------------------------------------------------------------------------


@when("the shim runs \"{command}\" without agent env vars")
def step_shim_runs_without_agent(context: Context, command: str) -> None:
    """Invoke the shim (as 'git') with a clean env — no agent markers.

    Builds a shim dir for the current scenario and invokes 'git <args>'.
    Stores result in context.shim_result for parity assertions.
    """
    parts = command.split()
    if parts and parts[0] == "git":
        parts = parts[1:]

    shim_dir = _build_shim_dir(context)
    env = _clean_env()
    env["PATH"] = f"{shim_dir}:{env.get('PATH', '')}"

    result = subprocess.run(  # noqa: S603
        ["git", *parts],
        capture_output=True,
        text=True,
        env=env,
        cwd=context.repo_path,
        check=False,
        timeout=_SHIM_TIMEOUT_SECONDS,
    )
    context.shim_result = result
    context.result = result


@when("the real git binary runs \"{command}\" in the same repository")
def step_real_git_runs(context: Context, command: str) -> None:
    """Run the same command with the real git binary (not the shim).

    Uses env -i to strip PATH manipulation so we get the real system git.
    Stores result in context.real_result for parity assertions.
    """
    parts = command.split()
    if parts and parts[0] == "git":
        parts = parts[1:]

    # Find real git by skipping the shim dir
    shim_dir = str(getattr(context, "shim_dir", ""))
    path_entries = os.environ.get("PATH", "").split(":")
    path_without_shim = ":".join(p for p in path_entries if p != shim_dir)

    env = os.environ.copy()
    env["PATH"] = path_without_shim

    real_git = shutil.which("git", path=path_without_shim)
    assert real_git is not None, (
        f"Could not find real git binary in PATH (excluding shim dir {shim_dir!r}).\n"
        f"PATH searched: {path_without_shim}"
    )

    result = subprocess.run(  # noqa: S603
        [real_git, *parts],
        capture_output=True,
        text=True,
        env=env,
        cwd=context.repo_path,
        check=False,
        timeout=_SHIM_TIMEOUT_SECONDS,
    )
    context.real_result = result


@when("an agent runs \"{command}\" via the shim with CLAUDECODE=1")
def step_agent_runs_via_gh_shim(context: Context, command: str) -> None:
    """Invoke the gh shim (context.gh_shim_dir/gh) with CLAUDECODE=1.

    The gh shim symlink is set up by the Given step. We prepend its
    directory to PATH so 'gh' resolves to the shim.
    """
    parts = command.split()
    assert parts and parts[0] == "gh", (
        f"Expected command starting with 'gh', got: {command!r}"
    )

    env = _clean_env()
    # Put stub_gh_dir FIRST on PATH so both the shim's internal Command::new("gh")
    # (used to resolve PR SHAs) and any subsequent gh call resolve to the stub,
    # not to the shim symlink itself (which would cause infinite recursion).
    # We invoke the shim directly by absolute path (context.gh_shim_link) rather
    # than via PATH, so the agent's command still routes through the shim.
    stub_prefix = f"{context.stub_gh_dir}:" if hasattr(context, "stub_gh_dir") else ""
    env["PATH"] = f"{stub_prefix}{env.get('PATH', '')}"
    env["CLAUDECODE"] = "1"

    # Invoke the shim binary directly (not via PATH) to avoid ambiguity with
    # the stub on PATH.  argv[0] must be the shim link path so the binary sees
    # "gh" as its own name and routes to handle_gh_pr_diff.
    shim_bin = str(context.gh_shim_link)
    result = subprocess.run(  # noqa: S603
        [shim_bin, *parts[1:]],
        capture_output=True,
        text=True,
        env=env,
        cwd=getattr(context, "repo_path", None),
        check=False,
        timeout=_SHIM_TIMEOUT_SECONDS,
    )
    context.gh_shim_result = result
    context.result = result


@when("the stub gh binary runs \"{command}\" directly")
def step_stub_gh_runs_directly(context: Context, command: str) -> None:
    """Run the same gh command with the stub binary directly for parity comparison.

    Invokes context.stub_gh_path directly (not via PATH resolution) so the
    comparison is against the stub's own output — no real gh required.
    """
    parts = command.split()
    assert parts and parts[0] == "gh", (
        f"Expected command starting with 'gh', got: {command!r}"
    )

    result = subprocess.run(  # noqa: S603
        [str(context.stub_gh_path), *parts[1:]],
        capture_output=True,
        text=True,
        check=False,
        timeout=_SHIM_TIMEOUT_SECONDS,
    )
    context.real_gh_result = result


@when("the real gh binary runs \"{command}\" directly")
def step_real_gh_runs_directly(context: Context, command: str) -> None:
    """Run the same gh command with the real binary for parity comparison."""
    parts = command.split()
    assert parts and parts[0] == "gh", (
        f"Expected command starting with 'gh', got: {command!r}"
    )

    real_gh = context.real_gh_path
    result = subprocess.run(  # noqa: S603
        [real_gh, *parts[1:]],
        capture_output=True,
        text=True,
        check=False,
        timeout=_SHIM_TIMEOUT_SECONDS,
    )
    context.real_gh_result = result


@when("I run \"git-prism shim install\" with the isolated HOME")
def step_shim_install(context: Context) -> None:
    """Run git-prism shim install with the isolated HOME."""
    _run_binary(context, ["shim", "install"], env=_isolated_env(context))


@when("I run \"git-prism shim install\" with the isolated HOME again")
def step_shim_install_again(context: Context) -> None:
    """Run git-prism shim install a second time (idempotency check)."""
    _run_binary(context, ["shim", "install"], env=_isolated_env(context))


@when("I run \"git-prism shim status\" with the isolated HOME")
def step_shim_status(context: Context) -> None:
    """Run git-prism shim status with the isolated HOME."""
    _run_binary(context, ["shim", "status"], env=_isolated_env(context))


@when("I run \"git-prism shim uninstall\" with the isolated HOME")
def step_shim_uninstall(context: Context) -> None:
    """Run git-prism shim uninstall with the isolated HOME."""
    _run_binary(context, ["shim", "uninstall"], env=_isolated_env(context))


# NOTE: 'I run "git-prism hooks install --path-shim" with the isolated HOME'
# is already defined in shim_wrapper_steps.py. The @ISSUE-324 deprecated-alias
# scenario reuses that step — do NOT redefine it here.


@when("I run \"git-prism hooks install\" with an isolated HOME")
def step_hooks_install_no_flag_with_isolated_home(context: Context) -> None:
    """Run git-prism hooks install (no --path-shim) with an isolated HOME."""
    if not hasattr(context, "isolated_home") or context.isolated_home is None:
        tmpdir = tempfile.mkdtemp()
        context.cleanup_dirs.append(tmpdir)
        context.isolated_home = Path(tmpdir)
    _run_binary(context, ["hooks", "install"], env=_isolated_env(context))


@when("I run \"git-prism shim install\" and consent to PATH setup with the isolated HOME")
def step_shim_install_consent(context: Context) -> None:
    """Run git-prism shim install, answering 'y' to the PATH setup prompt."""
    binary = _find_binary(context)
    env = _isolated_env(context)
    # Pin SHELL to zsh so detect_rc_file always picks .zprofile, regardless of
    # the CI runner's ambient $SHELL (which is /bin/bash on GitHub Actions).
    env["SHELL"] = "/bin/zsh"
    # If the shim dir was injected into PATH by a prior Given step, use that.
    if hasattr(context, "injected_path"):
        env["PATH"] = context.injected_path

    result = subprocess.run(  # noqa: S603
        [str(binary), "shim", "install"],
        input="y\n",
        capture_output=True,
        text=True,
        env=env,
        check=False,
        timeout=_SHIM_TIMEOUT_SECONDS,
    )
    context.result = result


@when(
    "I run \"git-prism shim install\" and consent to PATH setup with the isolated HOME again"
)
def step_shim_install_consent_again(context: Context) -> None:
    """Run git-prism shim install a second time with 'y' consent (idempotency check)."""
    binary = _find_binary(context)
    env = _isolated_env(context)
    # Pin SHELL to zsh so detect_rc_file always picks .zprofile, regardless of
    # the CI runner's ambient $SHELL (which is /bin/bash on GitHub Actions).
    env["SHELL"] = "/bin/zsh"
    result = subprocess.run(  # noqa: S603
        [str(binary), "shim", "install"],
        input="y\n",
        capture_output=True,
        text=True,
        env=env,
        check=False,
        timeout=_SHIM_TIMEOUT_SECONDS,
    )
    context.result = result


@when("I run \"git-prism shim install\" and decline PATH setup with the isolated HOME")
def step_shim_install_decline(context: Context) -> None:
    """Run git-prism shim install, answering 'n' to the PATH setup prompt."""
    binary = _find_binary(context)
    env = _isolated_env(context)
    result = subprocess.run(  # noqa: S603
        [str(binary), "shim", "install"],
        input="n\n",
        capture_output=True,
        text=True,
        env=env,
        check=False,
        timeout=_SHIM_TIMEOUT_SECONDS,
    )
    context.result = result


@when("I inspect the git-prism hooks directory")
def step_inspect_hooks_directory(context: Context) -> None:
    """No-op marker: records the real project hooks dir path for Then assertions."""
    # Use the real project root so the assertion checks the actual shipped files.
    context.hooks_dir_path = Path(context.project_root) / "hooks"
    context.result = subprocess.CompletedProcess(
        args=[], returncode=0, stdout="", stderr=""
    )


# ---------------------------------------------------------------------------
# Then: assertions
# ---------------------------------------------------------------------------


@then("the shim exit code matches the real git exit code")
def step_shim_exit_matches_real_git(context: Context) -> None:
    """Assert shim and real git exited with the same code."""
    shim_rc = context.shim_result.returncode
    real_rc = context.real_result.returncode
    assert shim_rc == real_rc, (
        f"Exit code mismatch: shim={shim_rc}, real git={real_rc}.\n"
        f"shim stderr: {context.shim_result.stderr}\n"
        f"real stderr: {context.real_result.stderr}"
    )


@then("the shim stdout matches the real git stdout")
def step_shim_stdout_matches_real_git(context: Context) -> None:
    """Assert shim and real git produced identical stdout."""
    shim_out = context.shim_result.stdout
    real_out = context.real_result.stdout
    assert shim_out == real_out, (
        f"stdout mismatch.\nshim: {shim_out!r}\nreal: {real_out!r}"
    )


@then("the shim stderr matches the real git stderr")
def step_shim_stderr_matches_real_git(context: Context) -> None:
    """Assert shim and real git produced identical stderr."""
    shim_err = context.shim_result.stderr
    real_err = context.real_result.stderr
    assert shim_err == real_err, (
        f"stderr mismatch.\nshim: {shim_err!r}\nreal: {real_err!r}"
    )


@then("the shim exit code matches the real gh exit code")
def step_shim_exit_matches_real_gh(context: Context) -> None:
    """Assert gh shim and real gh exited with the same code."""
    shim_rc = context.gh_shim_result.returncode
    real_rc = context.real_gh_result.returncode
    assert shim_rc == real_rc, (
        f"Exit code mismatch: shim={shim_rc}, real gh={real_rc}.\n"
        f"shim stderr: {context.gh_shim_result.stderr}\n"
        f"real stderr: {context.real_gh_result.stderr}"
    )


@then("the shim stdout matches the real gh stdout")
def step_shim_stdout_matches_real_gh(context: Context) -> None:
    """Assert gh shim and real gh produced identical stdout."""
    shim_out = context.gh_shim_result.stdout
    real_out = context.real_gh_result.stdout
    assert shim_out == real_out, (
        f"stdout mismatch.\nshim: {shim_out!r}\nreal: {real_out!r}"
    )


@then("the path \"{rel_path}\" is a symlink under HOME")
def step_path_is_symlink_under_home(context: Context, rel_path: str) -> None:
    """Assert that HOME/<rel_path> is a symlink (using context.isolated_home)."""
    target = context.isolated_home / rel_path
    assert target.is_symlink(), (
        f"Expected {target} to be a symlink, but it does not exist or is not a symlink.\n"
        f"stdout: {context.result.stdout}\n"
        f"stderr: {context.result.stderr}"
    )


@then("the path \"{rel_path}\" does not exist under HOME")
def step_path_does_not_exist_under_home(context: Context, rel_path: str) -> None:
    """Assert that HOME/<rel_path> does not exist."""
    target = context.isolated_home / rel_path
    assert not target.exists(), (
        f"Expected {target} to NOT exist, but it does.\n"
        f"stdout: {context.result.stdout}\n"
        f"stderr: {context.result.stderr}"
    )


@then("the stderr contains \"{phrase}\"")
def step_stderr_contains(context: Context, phrase: str) -> None:
    """Assert that stderr contains the given phrase."""
    err = context.result.stderr
    assert phrase in err, (
        f"Expected stderr to contain {phrase!r}.\nstderr: {err!r}\n"
        f"stdout: {context.result.stdout!r}"
    )


@then("the rc file \"{rel_path}\" under HOME contains the shim directory export line")
def step_rc_contains_export(context: Context, rel_path: str) -> None:
    """Assert the rc file has been updated with the shim PATH export."""
    rc_path = context.isolated_home / rel_path
    assert rc_path.exists(), f"RC file not found: {rc_path}"
    content = rc_path.read_text()
    assert _SHIM_EXPORT_FRAGMENT in content, (
        f"RC file {rc_path} does not contain the shim export line.\n"
        f"Expected fragment: {_SHIM_EXPORT_FRAGMENT!r}\n"
        f"File contents:\n{content}"
    )


_SHIM_BLOCK_START_MARKER = "# >>> git-prism shim >>>"


@then("the rc file \"{rel_path}\" under HOME contains the shim export line exactly once")
def step_rc_contains_export_exactly_once(context: Context, rel_path: str) -> None:
    """Assert the managed PATH block appears exactly once (idempotency).

    The new marker-delimited block contains the shim path fragment twice (once
    in each branch of the case statement), so we count block start-markers
    rather than raw fragment occurrences.
    """
    rc_path = context.isolated_home / rel_path
    assert rc_path.exists(), f"RC file not found: {rc_path}"
    content = rc_path.read_text()
    occurrences = content.count(_SHIM_BLOCK_START_MARKER)
    assert occurrences == 1, (
        f"Expected the managed PATH block to appear exactly once in {rc_path}, "
        f"but found {occurrences} occurrences of the start marker.\n"
        f"Marker: {_SHIM_BLOCK_START_MARKER!r}\n"
        f"File contents:\n{content}"
    )


@then("the rc file \"{rel_path}\" under HOME is unchanged")
def step_rc_unchanged(context: Context, rel_path: str) -> None:
    """Assert the rc file was not modified (user declined PATH setup)."""
    rc_path = context.isolated_home / rel_path
    assert rc_path.exists(), f"RC file not found: {rc_path}"
    current = rc_path.read_text()
    initial = getattr(context, "rc_path_initial_content", None)
    assert initial is not None, (
        "context.rc_path_initial_content not set — did the Given step run?"
    )
    assert current == initial, (
        f"RC file {rc_path} was modified despite the user declining PATH setup.\n"
        f"Before:\n{initial}\nAfter:\n{current}"
    )


@then("\"{rel_path}\" does not exist in the installation")
def step_file_does_not_exist_in_installation(context: Context, rel_path: str) -> None:
    """Assert that the named path does not exist under the real project root.

    Checks the real project tree so that when bash_redirect_hook.py is still
    present (before #326 removes it) this step FAILS with an AssertionError —
    proving the scenario is genuinely RED before implementation.
    """
    target = Path(context.project_root) / rel_path
    assert not target.exists(), (
        f"Expected {target} to NOT exist after redirect hook removal, "
        f"but it still exists. This is the expected RED state before #326 lands."
    )


@then('each entry in the "files" array has a "change_scope" field')
def step_files_entries_have_change_scope(context: Context) -> None:
    """Assert every entry in the top-level files array carries a change_scope field.

    This is a git-prism manifest-specific field that raw gh output cannot
    produce, so a passing assertion here proves the shim routed the call
    through git-prism rather than forwarding raw gh output.

    Fails RED now because the gh shim does not exist yet — the real gh
    binary returns plain text or its own JSON, neither of which has a
    change_scope field on file entries.
    """
    import json as _json
    stdout = (context.result.stdout or "").strip()
    try:
        data = _json.loads(stdout)
    except _json.JSONDecodeError as exc:
        raise AssertionError(
            f"Output is not valid JSON: {exc}\n"
            f"stdout: {stdout[:500]}\n"
            f"stderr: {context.result.stderr[:500]}"
        ) from exc
    files = data.get("files") if isinstance(data, dict) else None
    assert isinstance(files, list) and files, (
        f"Expected a non-empty 'files' array in JSON, got: {str(data)[:500]}"
    )
    missing = [
        i for i, entry in enumerate(files)
        if not (isinstance(entry, dict) and "change_scope" in entry)
    ]
    assert not missing, (
        f"Files entries at indices {missing} are missing 'change_scope' field. "
        f"This field is emitted only by git-prism manifest output — raw gh "
        f"output cannot satisfy this assertion.\n"
        f"files[0]: {files[0] if files else 'empty'}"
    )
