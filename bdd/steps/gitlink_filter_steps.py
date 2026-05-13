
"""Step definitions for gitlink filtering scenarios.

These steps create temporary git repositories with submodule references
and assert that git-prism excludes them from the change manifest.
"""

from __future__ import annotations

import subprocess
from pathlib import Path

from behave import given, then
from behave.runner import Context

from repo_setup_steps import _commit, _init_repo, _write_file


def _create_bare_repo() -> str:
    """Create a temporary bare git repository to serve as a submodule origin."""
    import tempfile
    repo_dir = tempfile.mkdtemp()
    subprocess.run(
        ["git", "init", "--bare"], cwd=repo_dir, check=True, capture_output=True,
    )
    return repo_dir


@given('a submodule "{path}" is added and committed')
def step_add_submodule(context: Context, path: str) -> None:
    """Add a submodule at the given path and commit it."""
    repo = context.repo_path
    bare = _create_bare_repo()
    context.cleanup_dirs.append(bare)

    # Clone the bare repo, add a commit, and push it back so the
    # parent repo has a valid submodule target.
    clone_dir = Path(bare).parent / "submod-clone"
    subprocess.run(
        ["git", "clone", bare, str(clone_dir)],
        check=True, capture_output=True,
    )
    (clone_dir / "README.md").write_text("submodule content\n")
    subprocess.run(
        ["git", "add", "README.md"], cwd=str(clone_dir),
        check=True, capture_output=True,
    )
    subprocess.run(
        ["git", "commit", "-m", "initial submodule commit"],
        cwd=str(clone_dir), check=True, capture_output=True,
    )
    subprocess.run(
        ["git", "push", "origin", "HEAD"],
        cwd=str(clone_dir), check=True, capture_output=True,
    )

    # Add the submodule in the parent repo
    subprocess.run(
        ["git", "submodule", "add", bare, path],
        cwd=repo, check=True, capture_output=True,
    )
    subprocess.run(
        ["git", "commit", "-m", "add submodule"],
        cwd=repo, check=True, capture_output=True,
    )


@given('the submodule "{path}" commit is updated')
def step_update_submodule(context: Context, path: str) -> None:
    """Update the submodule commit by adding a new commit in the submodule."""
    repo = context.repo_path
    submod_path = Path(repo) / path

    (submod_path / "update.txt").write_text("updated\n")
    subprocess.run(
        ["git", "add", "update.txt"], cwd=str(submod_path),
        check=True, capture_output=True,
    )
    subprocess.run(
        ["git", "commit", "-m", "submodule update"],
        cwd=str(submod_path), check=True, capture_output=True,
    )
    # Stage the submodule change in the parent repo
    subprocess.run(
        ["git", "add", path], cwd=repo,
        check=True, capture_output=True,
    )


@given('the submodule "{path}" is removed and committed')
def step_remove_submodule(context: Context, path: str) -> None:
    """Remove the submodule and commit the deletion."""
    repo = context.repo_path
    subprocess.run(
        ["git", "rm", "-f", path], cwd=repo,
        check=True, capture_output=True,
    )
    # Also clean up the .gitmodules entry if it exists
    gitmodules = Path(repo) / ".gitmodules"
    if gitmodules.exists():
        subprocess.run(
            ["git", "add", ".gitmodules"], cwd=repo,
            check=True, capture_output=True,
        )
    subprocess.run(
        ["git", "commit", "-m", "remove submodule"],
        cwd=repo, check=True, capture_output=True,
    )


@given('a binary file "{path}" is added and committed')
def step_add_binary_file(context: Context, path: str) -> None:
    """Add a binary file with null bytes and commit it."""
    repo = context.repo_path
    filepath = Path(repo) / path
    filepath.parent.mkdir(parents=True, exist_ok=True)
    filepath.write_bytes(bytes([0x89, 0x50, 0x4E, 0x47, 0x00, 0x00]))
    subprocess.run(
        ["git", "add", path], cwd=repo,
        check=True, capture_output=True,
    )
    subprocess.run(
        ["git", "commit", "-m", "add binary file"],
        cwd=repo, check=True, capture_output=True,
    )


@then('no file has path "{path}"')
def step_no_file_has_path(context: Context, path: str) -> None:
    """Assert that no file entry has the given path."""
    from json_steps import _ensure_json_parsed
    manifest = _ensure_json_parsed(context)
    files: list[dict[str, object]] = manifest.get("files", [])
    has_matching_path = any(
        file_entry.get("path") == path for file_entry in files
    )
    assert not has_matching_path, (
        f"Found file with path '{path}' but expected none. "
        f"Paths: {[file_entry.get("path") for file_entry in files]}"
    )
