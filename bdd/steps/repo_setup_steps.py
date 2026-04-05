"""Step definitions for creating specific git repository fixtures.

These steps create temporary git repos with specific file contents
and commit histories for testing git-prism's various features.
"""

from __future__ import annotations

import subprocess
import tempfile
from pathlib import Path

from behave import given
from behave.runner import Context


def _init_repo(context: Context) -> str:
    """Create and initialize a temporary git repository.

    Args:
        context: The behave context, used to register cleanup dirs and store repo_path.

    Returns:
        The absolute path to the temporary repository directory.
    """
    repo_dir = tempfile.mkdtemp()
    context.cleanup_dirs.append(repo_dir)
    subprocess.run(["git", "init"], cwd=repo_dir, check=True, capture_output=True)
    subprocess.run(
        ["git", "config", "user.email", "test@test.com"],
        cwd=repo_dir, check=True, capture_output=True,
    )
    subprocess.run(
        ["git", "config", "user.name", "Test"],
        cwd=repo_dir, check=True, capture_output=True,
    )
    context.repo_path = repo_dir
    return repo_dir


def _write_file(repo_path: str, filename: str, content: str) -> Path:
    """Write content to a file in the repo, creating directories as needed.

    Args:
        repo_path: The root directory of the git repository.
        filename: The relative path within the repo (may include subdirectories).
        content: The text content to write.

    Returns:
        The full path to the written file.
    """
    filepath = Path(repo_path) / filename
    filepath.parent.mkdir(parents=True, exist_ok=True)
    filepath.write_text(content)
    return filepath


def _commit(
    repo_path: str,
    message: str,
    files: list[str],
) -> None:
    """Stage specified files and create a commit.

    Args:
        repo_path: The root directory of the git repository.
        message: The commit message.
        files: Filenames to stage before committing. Must not be empty --
            accidental empty commits produce opaque git errors that waste
            debugging time in test fixtures.

    Raises:
        ValueError: If files is empty, indicating a bug in the test fixture.
    """
    if not files:
        msg = (
            f"_commit() called with no files to stage for message: '{message}'. "
            f"This is almost certainly a bug in the test fixture."
        )
        raise ValueError(msg)
    for filename in files:
        subprocess.run(
            ["git", "add", filename], cwd=repo_path,
            check=True, capture_output=True,
        )
    subprocess.run(
        ["git", "commit", "-m", message],
        cwd=repo_path, check=True, capture_output=True,
    )


# ---------- Working tree status fixtures ----------


@given("a git repository with one commit")
def step_repo_with_one_commit(context: Context) -> None:
    """Create a temporary repo with a single initial commit."""
    repo_dir = _init_repo(context)
    _write_file(repo_dir, "README.md", "# Test\n")
    _commit(repo_dir, "initial commit", ["README.md"])


@given('a new file "{filename}" is staged with content')
def step_stage_new_file(context: Context, filename: str) -> None:
    """Write a file and stage it in the test repo."""
    repo = context.repo_path
    _write_file(repo, filename, context.text)
    subprocess.run(
        ["git", "add", filename], cwd=repo,
        check=True, capture_output=True,
    )


@given('a git repository with a committed file "{filename}" containing')
def step_repo_with_committed_file(context: Context, filename: str) -> None:
    """Create a temporary repo with a single committed file."""
    repo_dir = _init_repo(context)
    _write_file(repo_dir, filename, context.text)
    _commit(repo_dir, "initial commit", [filename])


@given('the file "{filename}" is modified on disk to')
def step_modify_file_on_disk(context: Context, filename: str) -> None:
    """Overwrite a file in the test repo without staging."""
    _write_file(context.repo_path, filename, context.text)


@given('the file "{filename}" is modified and staged with content')
def step_modify_and_stage_file(context: Context, filename: str) -> None:
    """Overwrite and stage a file in the test repo."""
    repo = context.repo_path
    _write_file(repo, filename, context.text)
    subprocess.run(
        ["git", "add", filename], cwd=repo,
        check=True, capture_output=True,
    )


@given('the file "{filename}" is further modified on disk to')
def step_further_modify_on_disk(context: Context, filename: str) -> None:
    """Overwrite a file again without staging (creating unstaged changes)."""
    _write_file(context.repo_path, filename, context.text)


# ---------- Java analyzer fixtures ----------


JAVA_INITIAL = """\
package com.example;

import java.util.List;

public class Calculator {
    public int add(int a, int b) {
        return a + b;
    }
}
"""

JAVA_MODIFIED = """\
package com.example;

import java.util.List;
import java.util.Map;

public class Calculator {
    public int add(int a, int b) {
        return a + b;
    }

    public int multiply(int a, int b) {
        return a * b;
    }
}
"""


@given("a git repository with a Java commit")
def step_repo_with_java_commit(context: Context) -> None:
    """Create a repo with two Java commits: initial class and added method."""
    repo_dir = _init_repo(context)

    _write_file(repo_dir, "Calculator.java", JAVA_INITIAL)
    _commit(repo_dir, "initial java", ["Calculator.java"])

    _write_file(repo_dir, "Calculator.java", JAVA_MODIFIED)
    _commit(repo_dir, "add multiply method and Map import", ["Calculator.java"])


# ---------- C analyzer fixtures ----------


C_INITIAL = """\
#include <stdio.h>

void greet(const char* name) {
    printf("Hello, %s!\\n", name);
}

int main(void) {
    greet("world");
    return 0;
}
"""

C_MODIFIED = """\
#include <stdio.h>
#include <stdlib.h>

void greet(const char* name) {
    printf("Hello, %s!\\n", name);
}

void farewell(const char* name) {
    printf("Goodbye, %s!\\n", name);
}

int main(void) {
    greet("world");
    farewell("world");
    return 0;
}
"""


@given("a git repository with a C commit")
def step_repo_with_c_commit(context: Context) -> None:
    """Create a repo with two C commits: initial and added farewell function."""
    repo_dir = _init_repo(context)

    _write_file(repo_dir, "main.c", C_INITIAL)
    _commit(repo_dir, "initial c", ["main.c"])

    _write_file(repo_dir, "main.c", C_MODIFIED)
    _commit(repo_dir, "add farewell function", ["main.c"])


CPP_INITIAL = """\
#include <iostream>
#include <string>

namespace math {

class Calculator {
public:
    int add(int a, int b) {
        return a + b;
    }
};

}  // namespace math
"""

CPP_MODIFIED = """\
#include <iostream>
#include <string>
#include <vector>

namespace math {

class Calculator {
public:
    int add(int a, int b) {
        return a + b;
    }

    int multiply(int a, int b) {
        return a * b;
    }
};

}  // namespace math
"""


@given("a git repository with a C++ commit")
def step_repo_with_cpp_commit(context: Context) -> None:
    """Create a repo with two C++ commits: initial class and added method."""
    repo_dir = _init_repo(context)

    _write_file(repo_dir, "calculator.cpp", CPP_INITIAL)
    _commit(repo_dir, "initial cpp", ["calculator.cpp"])

    _write_file(repo_dir, "calculator.cpp", CPP_MODIFIED)
    _commit(repo_dir, "add multiply method", ["calculator.cpp"])


HEADER_INITIAL = """\
#ifndef UTILS_H
#define UTILS_H

int add(int a, int b);

#endif
"""

HEADER_MODIFIED = """\
#ifndef UTILS_H
#define UTILS_H

int add(int a, int b);
int multiply(int a, int b);
void greet(const char* name);

#endif
"""


@given("a git repository with a C header commit")
def step_repo_with_header_commit(context: Context) -> None:
    """Create a repo with two header commits: initial and added declarations."""
    repo_dir = _init_repo(context)

    _write_file(repo_dir, "utils.h", HEADER_INITIAL)
    _commit(repo_dir, "initial header", ["utils.h"])

    _write_file(repo_dir, "utils.h", HEADER_MODIFIED)
    _commit(repo_dir, "add multiply and greet declarations", ["utils.h"])


# ---------- Per-commit history fixtures ----------


@given("a git repository with three sequential commits")
def step_repo_with_three_commits(context: Context) -> None:
    """Create a repo with three sequential commits across two files."""
    repo_dir = _init_repo(context)

    _write_file(repo_dir, "file_a.txt", "first version\n")
    _commit(repo_dir, "commit one", ["file_a.txt"])

    _write_file(repo_dir, "file_b.txt", "second file\n")
    _commit(repo_dir, "commit two", ["file_b.txt"])

    _write_file(repo_dir, "file_a.txt", "updated version\n")
    _commit(repo_dir, "commit three", ["file_a.txt"])
