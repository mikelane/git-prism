"""Behave hooks for git-prism BDD tests.

Builds the release binary once before all tests and cleans up
temporary git repositories after each scenario.
"""

import os
import shutil
import subprocess


BINARY_PATH = None


def before_all(context):
    global BINARY_PATH
    project_root = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
    context.project_root = project_root

    binary = os.path.join(project_root, "target", "release", "git-prism")

    if not os.path.isfile(binary):
        subprocess.run(
            ["cargo", "build", "--release"],
            cwd=project_root,
            check=True,
        )

    BINARY_PATH = binary
    context.binary_path = BINARY_PATH


def before_scenario(context, scenario):
    context.cleanup_dirs = []
    context.json_data = None


def after_scenario(context, scenario):
    for path in getattr(context, "cleanup_dirs", []):
        if os.path.isdir(path):
            shutil.rmtree(path, ignore_errors=True)
