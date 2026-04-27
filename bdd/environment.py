"""Behave hooks for git-prism BDD tests.

Builds the release binary once before all tests and cleans up
temporary git repositories after each scenario.
"""

import os
import shutil
import subprocess
import sys

from behave.model import Scenario
from behave.runner import Context


def before_all(context: Context) -> None:
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

    context.binary_path = binary


def before_scenario(context: Context, scenario: Scenario) -> None:
    context.cleanup_dirs = []
    context.json_data = None
    context.server_procs = []
    # Reset redirect-hook scenario state so values from a previous scenario
    # cannot leak in (e.g., a stale `fake_home` would skip the isolated-HOME
    # setup and let the test read the developer's real ~/.claude/...).
    context.fake_home = None
    context.hook_payload = None
    context.hook_command = None
    context.hook_extra_env = {}
    context.review_change_payload = None
    context.captured_sha256 = None
    context.captured_pretooluse_length = None
    context.user_settings_path = None
    context.user_hooks_dir = None
    context.project_repo_path = None
    context.project_settings_path = None
    context.project_hooks_dir = None


def after_scenario(context: Context, scenario: Scenario) -> None:
    # Telemetry scenarios spawn an MCP server and a mock OTLP collector;
    # tear both down before removing the temp repo directory so file
    # handles can't keep the repo alive on shutdown. The helper no-ops
    # when no collector/procs were registered, so call unconditionally.
    from telemetry_steps import telemetry_after_scenario
    telemetry_after_scenario(context)

    for path in getattr(context, "cleanup_dirs", []):
        if os.path.isdir(path):
            shutil.rmtree(path, ignore_errors=True)
