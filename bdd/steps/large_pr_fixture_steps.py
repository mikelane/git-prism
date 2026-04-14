"""Fixture builder for large-PR scenarios in the response-size guardrails feature.

Builds a deterministic Rust repository with `file_count` source files, each
containing `fns_per_file` functions with stable names (`function_01`,
`function_02`, ...). Two commits are created: an initial commit with all
functions returning `1`, and a modified commit where every function body is
changed to return `2`. The git ref range between the two commits drives the
When-steps for the ISSUE-212 scenarios.

Deterministic naming is required so the function-name-filter scenario can
assert on specific names without having to know how git-prism orders its
output.
"""

from __future__ import annotations

from behave import given
from behave.runner import Context

from repo_setup_steps import _commit, _init_repo, _write_file


def _rust_function(index: int, body_value: int) -> str:
    """Render one Rust function definition with a deterministic name.

    Function names are zero-padded so lexicographic sort matches numeric sort
    up to `function_99`. Bodies are just integer literals — tree-sitter does
    not care about semantics.
    """
    name = f"function_{index:02d}"
    return f"pub fn {name}() -> i32 {{ {body_value} }}\n"


def _rust_source_file(start_index: int, fns_per_file: int, body_value: int) -> str:
    """Render the full content of a single Rust source file.

    Functions in the file are numbered `function_{start_index:02d}` through
    `function_{start_index + fns_per_file - 1:02d}`, each with a body of
    `body_value`. Distinct body values across commits produce real function
    diffs that git-prism's content-aware differ will surface.
    """
    return "".join(
        _rust_function(start_index + i, body_value) for i in range(fns_per_file)
    )


def _build_large_pr_fixture(
    context: Context, file_count: int, fns_per_file: int,
) -> None:
    """Build a deterministic multi-file Rust repo with two commits.

    The INITIAL commit populates `file_count` files under `src/`, each named
    `src_NN.rs` and containing `fns_per_file` functions. The MODIFIED commit
    rewrites every file so every function body changes, producing a dense
    change set that exercises response-size budgets.

    The helper appends the tempdir path to `context.cleanup_dirs` so
    `bdd/environment.py::after_scenario` cleans it up, and sets
    `context.repo_path` so existing CLI steps that auto-inject `--repo` work
    out of the box. It is called twice by this module: once from the feature
    Background (20 files, ~50 functions) and once from the capstone Rule for
    the extreme-change stress fixture (200 files, 1000 functions).
    """
    repo_dir = _init_repo(context)

    initial_filenames: list[str] = []
    for file_index in range(file_count):
        start = file_index * fns_per_file + 1
        filename = f"src/src_{file_index + 1:03d}.rs"
        _write_file(repo_dir, filename, _rust_source_file(start, fns_per_file, 1))
        initial_filenames.append(filename)
    _commit(repo_dir, "initial: seed functions", initial_filenames)

    modified_filenames: list[str] = []
    for file_index in range(file_count):
        start = file_index * fns_per_file + 1
        filename = f"src/src_{file_index + 1:03d}.rs"
        _write_file(repo_dir, filename, _rust_source_file(start, fns_per_file, 2))
        modified_filenames.append(filename)
    _commit(repo_dir, "modified: bump every function body", modified_filenames)


@given(
    "a git repository with a change affecting {file_count:d} files "
    "and {fn_count:d} modified functions",
)
def step_impl_large_pr_fixture(
    context: Context, file_count: int, fn_count: int,
) -> None:
    """Create the deterministic large-PR fixture for the ISSUE-212 scenarios.

    Both the Background (20 files / 50 functions) and the capstone Rule
    (200 files / 1000 functions) hit this single step via parameter
    extraction. We pick `fns_per_file` so that total function count is at
    least the requested number — the Gherkin language says "affecting X
    files and Y modified functions" not "exactly Y", so rounding up is fine.
    """
    fns_per_file = max(1, fn_count // file_count)
    if fns_per_file * file_count < fn_count:
        fns_per_file += 1
    _build_large_pr_fixture(context, file_count, fns_per_file)
