"""Behave hooks for git-prism BDD tests.

Builds the release binary once before all tests and cleans up
temporary git repositories after each scenario.
"""

import os
import shutil
import subprocess
import sys


BINARY_PATH = None


def before_all(context):
    global BINARY_PATH
    project_root = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
    context.project_root = project_root

    # behave's step loader exec's step files directly, so imports between
    # step modules (and from environment.py) need steps/ on sys.path.
    steps_dir = os.path.join(os.path.dirname(os.path.abspath(__file__)), "steps")
    if steps_dir not in sys.path:
        sys.path.insert(0, steps_dir)

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
    context.server_procs = []


def after_scenario(context, scenario):
    # Telemetry scenarios spawn an MCP server and a mock OTLP collector;
    # tear both down before removing the temp repo directory so file
    # handles can't keep the repo alive on shutdown. The helper no-ops
    # when no collector/procs were registered, so call unconditionally.
    from telemetry_steps import telemetry_after_scenario
    telemetry_after_scenario(context)

    for path in getattr(context, "cleanup_dirs", []):
        if os.path.isdir(path):
            shutil.rmtree(path, ignore_errors=True)
