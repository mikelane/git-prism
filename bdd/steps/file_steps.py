"""Step definitions for file existence and content checks."""

import os

from behave import given, then


@given("the project root directory")
def step_project_root(context):
    # project_root is set in environment.py before_all
    assert os.path.isdir(context.project_root), (
        f"Project root does not exist: {context.project_root}"
    )


@then('the file "{filename}" exists')
def step_file_exists(context, filename):
    path = os.path.join(context.project_root, filename)
    assert os.path.isfile(path), f"File does not exist: {path}"


@then('the file "{filename}" contains "{text}" (case insensitive)')
def step_file_contains_ci(context, filename, text):
    path = os.path.join(context.project_root, filename)
    assert os.path.isfile(path), f"File does not exist: {path}"
    with open(path) as f:
        content = f.read()
    assert text.lower() in content.lower(), (
        f"'{text}' (case insensitive) not found in {filename}"
    )


@then('the file "{filename}" contains "{text}"')
def step_file_contains(context, filename, text):
    path = os.path.join(context.project_root, filename)
    assert os.path.isfile(path), f"File does not exist: {path}"
    with open(path) as f:
        content = f.read()
    assert text in content, f"'{text}' not found in {filename}"
