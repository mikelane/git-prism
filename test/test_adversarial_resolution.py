"""Adversarial QA tests for remote ref resolution (ISSUE-263).

Each test creates real git repos and invokes the git-prism binary.
Tests that find bugs are labelled BUG and must FAIL to prove them.
"""

from __future__ import annotations

import json
import os
import shutil
import subprocess
import sys
import tempfile


BINARY = os.path.join(
    os.path.dirname(os.path.dirname(os.path.abspath(__file__))),
    "target", "release", "git-prism",
)


def _git(args, cwd):
    return subprocess.run(["git"] + list(args), cwd=cwd,
                          capture_output=True, text=True, check=False)


def _create_local_repo():
    path = tempfile.mkdtemp()
    _git(["init", "--initial-branch=main"], cwd=path)
    _git(["-c", "user.email=test@test.com", "-c", "user.name=Test",
          "-c", "commit.gpgsign=false", "commit",
          "--allow-empty", "-m", "initial"], cwd=path)
    return path


def _create_bare_remote():
    path = tempfile.mkdtemp()
    _git(["init", "--bare"], cwd=path)
    return path


def _git_prism(args, cwd):
    return subprocess.run([BINARY] + list(args), cwd=cwd,
                          capture_output=True, text=True, check=False)


def _extract_json(output):
    start = output.find("{")
    if start == -1:
        return None
    count = 0
    end = start
    for i, ch in enumerate(output[start:], start):
        if ch == "{":
            count += 1
        elif ch == "}":
            count -= 1
            if count == 0:
                end = i + 1
                break
    try:
        return json.loads(output[start:end])
    except json.JSONDecodeError:
        return None


def _push_and_fetch_branch(local, remote, branch, remote_name="origin"):
    """Push a local branch to the bare remote, fetch it, then go back
    to main and delete the local branch. The remote tracking ref remains."""
    _git(["remote", "add", remote_name, remote], cwd=local)
    _git(["checkout", "-b", branch], cwd=local)
    with open(os.path.join(local, "feat.txt"), "w") as fh:
        fh.write(f"branch {branch}\n")
    _git(["add", "feat.txt"], cwd=local)
    _git(["-c", "commit.gpgsign=false", "commit",
          "-m", f"add {branch}"], cwd=local)
    _git(["push", "-u", remote_name, branch], cwd=local)
    _git(["fetch", remote_name], cwd=local)
    _git(["checkout", "main"], cwd=local)
    _git(["branch", "-D", branch], cwd=local)


# ── BUG tests ──────────────────────────────────────────────────────────────

def test_BUG_1_resolution_says_checkout_not_fetch():
    """BUG: Resolution says 'git checkout feature/foo' but the BDD
    acceptance criteria (remote_ref_resolution.feature:12) expect
    'git fetch origin feature/foo'.

    Confirmed by behave run:
      ASSERT FAILED: Expected 'resolution' = 'git fetch origin feature/foo',
      got 'git checkout feature/foo'

    This test asserts the BDD-expected value, which FAILS because the
    code produces a different value.
    """
    bdd_expected = "git fetch origin feature/foo"
    local = _create_local_repo()
    remote = _create_bare_remote()
    try:
        _push_and_fetch_branch(local, remote, "feature/foo")
        result = _git_prism(["manifest", "main..feature/foo",
                             "--repo", local], cwd=local)
        assert result.returncode != 0, f"exit: {result.returncode}"
        combined = result.stdout + result.stderr
        jerr = _extract_json(combined)
        assert jerr is not None, f"No JSON in:\n{combined}"
        assert "resolution" in jerr, f"No resolution in:\n{jerr}"
        actual = jerr["resolution"]
        # This FAILS because code produces "git checkout feature/foo"
        assert actual == bdd_expected, (
            f"BDD mismatch: expected '{bdd_expected}' got '{actual}'")
    finally:
        shutil.rmtree(local, ignore_errors=True)
        shutil.rmtree(remote, ignore_errors=True)


def test_BUG_2_hardcoded_origin_ignores_upstream():
    """BUG: Resolution only checks refs/remotes/origin/<branch>.
    A branch tracked under 'upstream' gets no resolution even though
    'git checkout <branch>' would work.
    """
    local = _create_local_repo()
    remote = _create_bare_remote()
    try:
        _push_and_fetch_branch(local, remote, "feature/foo", "upstream")
        result = _git_prism(["manifest", "main..feature/foo",
                             "--repo", local], cwd=local)
        assert result.returncode != 0, f"exit: {result.returncode}"
        combined = result.stdout + result.stderr
        jerr = _extract_json(combined)

        # refs/remotes/upstream/feature/foo exists but
        # resolve_ref_with_fallback only checks refs/remotes/origin/*.
        # ASSERT FAILS to prove the gap.
        assert jerr is not None and "resolution" in jerr, (
            "No resolution for branch tracked on 'upstream' remote.\n"
            f"Output:\n{combined}"
        )
    finally:
        shutil.rmtree(local, ignore_errors=True)
        shutil.rmtree(remote, ignore_errors=True)


# ── REGRESSION / EDGE tests ─────────────────────────────────────────────────

def test_branch_with_slash_gets_resolution():
    """Verify slashed branch names work."""
    local = _create_local_repo()
    remote = _create_bare_remote()
    try:
        _push_and_fetch_branch(local, remote, "feature/sub/branch")
        result = _git_prism(["manifest", "main..feature/sub/branch",
                             "--repo", local], cwd=local)
        assert result.returncode != 0
        jerr = _extract_json(result.stdout + result.stderr)
        assert jerr is not None
        assert "resolution" in jerr
        assert "fetch" in jerr["resolution"]
    finally:
        shutil.rmtree(local, ignore_errors=True)
        shutil.rmtree(remote, ignore_errors=True)


def test_branch_with_at_gets_resolution():
    """Regression: branches with @ (but not @{) must get a resolution."""
    local = _create_local_repo()
    remote = _create_bare_remote()
    try:
        _push_and_fetch_branch(local, remote, "feature@team")
        result = _git_prism(["manifest", "main..feature@team",
                             "--repo", local], cwd=local)
        assert result.returncode != 0
        jerr = _extract_json(result.stdout + result.stderr)
        assert jerr is not None
        assert "resolution" in jerr
    finally:
        shutil.rmtree(local, ignore_errors=True)
        shutil.rmtree(remote, ignore_errors=True)


def test_no_resolution_for_unknown_branch():
    """Totally unknown branch -> plain text error, no JSON."""
    local = _create_local_repo()
    try:
        result = _git_prism(["manifest", "main..totally-unknown",
                             "--repo", local], cwd=local)
        assert result.returncode != 0
        combined = result.stdout + result.stderr
        jerr = _extract_json(combined)
        assert jerr is None or "resolution" not in jerr
        assert "Could not find ref" in combined
    finally:
        shutil.rmtree(local, ignore_errors=True)


def test_no_resolution_for_bare_sha():
    """Bare SHA -> plain error, no JSON."""
    local = _create_local_repo()
    sha = "deadbeef1234567890abcdef1234567890abcdef12"
    try:
        result = _git_prism(["manifest", f"main..{sha}",
                             "--repo", local], cwd=local)
        assert result.returncode != 0
        combined = result.stdout + result.stderr
        jerr = _extract_json(combined)
        assert jerr is None or "resolution" not in jerr
    finally:
        shutil.rmtree(local, ignore_errors=True)


def test_no_resolution_for_qualified_ref():
    """refs/heads/nonexistent -> plain error, no resolution."""
    local = _create_local_repo()
    try:
        result = _git_prism(["manifest", "main..refs/heads/x",
                             "--repo", local], cwd=local)
        assert result.returncode != 0
        combined = result.stdout + result.stderr
        jerr = _extract_json(combined)
        assert jerr is None or "resolution" not in jerr
    finally:
        shutil.rmtree(local, ignore_errors=True)


def test_no_resolution_for_at_curly_reflog():
    """HEAD@{1} is reflog syntax -> plain error, no resolution."""
    local = _create_local_repo()
    try:
        result = _git_prism(["manifest", "main..HEAD@{1}",
                             "--repo", local], cwd=local)
        assert result.returncode != 0
        combined = result.stdout + result.stderr
        jerr = _extract_json(combined)
        assert jerr is None or "resolution" not in jerr
    finally:
        shutil.rmtree(local, ignore_errors=True)


if __name__ == "__main__":
    failures = []
    for name, func in sorted(globals().items()):
        if name.startswith("test_") and callable(func):
            try:
                func()
                print(f"  PASS: {name}")
            except AssertionError as e:
                print(f"  FAIL: {name} -- {e}")
                failures.append(name)
            except Exception as e:
                print(f"  ERROR: {name} -- {type(e).__name__}: {e}")
                failures.append(name)
    print(f"\n{len(failures)} test(s) failed.")
    if failures:
        print("Failures:", ", ".join(failures))
        sys.exit(1)
    else:
        sys.exit(0)
