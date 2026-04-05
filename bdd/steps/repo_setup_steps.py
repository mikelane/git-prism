"""Step definitions for creating specific git repository fixtures.

These steps create temporary git repos with specific file contents
and commit histories for testing git-prism's various features.
"""

import os
import subprocess
import tempfile

from behave import given


def _init_repo(context):
    """Create and initialize a temporary git repository."""
    tmp = tempfile.mkdtemp()
    context.cleanup_dirs.append(tmp)
    subprocess.run(["git", "init"], cwd=tmp, check=True, capture_output=True)
    subprocess.run(
        ["git", "config", "user.email", "test@test.com"],
        cwd=tmp, check=True, capture_output=True,
    )
    subprocess.run(
        ["git", "config", "user.name", "Test"],
        cwd=tmp, check=True, capture_output=True,
    )
    context.repo_path = tmp
    return tmp


def _write_file(repo_path, filename, content):
    """Write content to a file in the repo, creating directories as needed."""
    filepath = os.path.join(repo_path, filename)
    os.makedirs(os.path.dirname(filepath), exist_ok=True)
    with open(filepath, "w") as f:
        f.write(content)
    return filepath


def _commit(repo_path, message, files=None):
    """Stage files and create a commit."""
    if files:
        for f in files:
            subprocess.run(
                ["git", "add", f], cwd=repo_path,
                check=True, capture_output=True,
            )
    subprocess.run(
        ["git", "commit", "-m", message],
        cwd=repo_path, check=True, capture_output=True,
    )


# ---------- Working tree status fixtures ----------


@given("a git repository with one commit")
def step_repo_with_one_commit(context):
    tmp = _init_repo(context)
    _write_file(tmp, "README.md", "# Test\n")
    _commit(tmp, "initial commit", ["README.md"])


@given('a new file "{filename}" is staged with content')
def step_stage_new_file(context, filename):
    repo = context.repo_path
    _write_file(repo, filename, context.text)
    subprocess.run(
        ["git", "add", filename], cwd=repo,
        check=True, capture_output=True,
    )


@given('a git repository with a committed file "{filename}" containing')
def step_repo_with_committed_file(context, filename):
    tmp = _init_repo(context)
    _write_file(tmp, filename, context.text)
    _commit(tmp, "initial commit", [filename])


@given('the file "{filename}" is modified on disk to')
def step_modify_file_on_disk(context, filename):
    _write_file(context.repo_path, filename, context.text)


@given('the file "{filename}" is modified and staged with content')
def step_modify_and_stage_file(context, filename):
    repo = context.repo_path
    _write_file(repo, filename, context.text)
    subprocess.run(
        ["git", "add", filename], cwd=repo,
        check=True, capture_output=True,
    )


@given('the file "{filename}" is further modified on disk to')
def step_further_modify_on_disk(context, filename):
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
def step_repo_with_java_commit(context):
    tmp = _init_repo(context)

    _write_file(tmp, "Calculator.java", JAVA_INITIAL)
    _commit(tmp, "initial java", ["Calculator.java"])

    _write_file(tmp, "Calculator.java", JAVA_MODIFIED)
    _commit(tmp, "add multiply method and Map import", ["Calculator.java"])


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
def step_repo_with_c_commit(context):
    tmp = _init_repo(context)

    _write_file(tmp, "main.c", C_INITIAL)
    _commit(tmp, "initial c", ["main.c"])

    _write_file(tmp, "main.c", C_MODIFIED)
    _commit(tmp, "add farewell function", ["main.c"])


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
def step_repo_with_cpp_commit(context):
    tmp = _init_repo(context)

    _write_file(tmp, "calculator.cpp", CPP_INITIAL)
    _commit(tmp, "initial cpp", ["calculator.cpp"])

    _write_file(tmp, "calculator.cpp", CPP_MODIFIED)
    _commit(tmp, "add multiply method", ["calculator.cpp"])


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
def step_repo_with_header_commit(context):
    tmp = _init_repo(context)

    _write_file(tmp, "utils.h", HEADER_INITIAL)
    _commit(tmp, "initial header", ["utils.h"])

    _write_file(tmp, "utils.h", HEADER_MODIFIED)
    _commit(tmp, "add multiply and greet declarations", ["utils.h"])


# ---------- Per-commit history fixtures ----------


@given("a git repository with three sequential commits")
def step_repo_with_three_commits(context):
    tmp = _init_repo(context)

    _write_file(tmp, "file_a.txt", "first version\n")
    _commit(tmp, "commit one", ["file_a.txt"])

    _write_file(tmp, "file_b.txt", "second file\n")
    _commit(tmp, "commit two", ["file_b.txt"])

    _write_file(tmp, "file_a.txt", "updated version\n")
    _commit(tmp, "commit three", ["file_a.txt"])
