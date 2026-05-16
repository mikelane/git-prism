"""Step definitions for agent detection CLI tests.

Shells out to the real `git-prism agent-detect` binary with a controlled
environment (using env -i plus specific vars under test). Does NOT import
git-prism internals — Python-on-Rust cross-language isolation is intentional.
"""

from __future__ import annotations

import json
import os
import subprocess
from typing import Any

from behave import given, then, when
from behave.runner import Context

# Defensive upper bound — agent-detect should return in milliseconds.
# A hang here would deadlock CI.
_AGENT_DETECT_TIMEOUT_SECONDS: float = 30.0


@given("the git-prism binary is available")
def step_binary_available(context: Context) -> None:
    """Assert the release binary exists (built by environment.py before_all)."""
    assert os.path.isfile(context.binary_path), (
        f"git-prism binary not found at {context.binary_path}. "
        "Run `cargo build --release` first."
    )


@when('I run agent-detect with env vars "{env_spec}"')
def step_run_agent_detect_with_env(context: Context, env_spec: str) -> None:
    """Run agent-detect with a controlled environment containing only the given vars.

    env_spec is a comma-separated list of KEY=VALUE pairs.
    The binary runs with an empty base environment plus only these vars.

    Note: values containing commas are not supported by this split strategy;
    current Gherkin scenarios never use such values.
    """
    env: dict[str, str] = {}
    for raw_pair in env_spec.split(","):
        pair = raw_pair.strip()
        key, _, value = pair.partition("=")
        env[key.strip()] = value.strip()

    context.result = subprocess.run(  # noqa: S603 - args list, no shell
        [context.binary_path, "agent-detect"],
        capture_output=True,
        text=True,
        env=env,
        check=False,
        timeout=_AGENT_DETECT_TIMEOUT_SECONDS,
    )


@when("I run agent-detect with empty environment")
def step_run_agent_detect_empty_env(context: Context) -> None:
    """Run agent-detect with a completely empty environment."""
    context.result = subprocess.run(  # noqa: S603 - args list, no shell
        [context.binary_path, "agent-detect"],
        capture_output=True,
        text=True,
        env={},
        check=False,
        timeout=_AGENT_DETECT_TIMEOUT_SECONDS,
    )


@then('the JSON field "{field}" is "{expected}"')
def step_json_field_is_string(context: Context, field: str, expected: str) -> None:
    """Assert a top-level JSON field equals the expected string value."""
    data = _parse_result_json(context)
    actual = data.get(field)
    assert actual == expected, (
        f"Expected JSON field '{field}' = {expected!r}, got {actual!r}\n"
        f"Full output: {context.result.stdout}"
    )


@then('the JSON field "{field}" is null')
def step_json_field_is_null(context: Context, field: str) -> None:
    """Assert a top-level JSON field is null (Python None)."""
    data = _parse_result_json(context)
    assert field in data, (
        f"JSON field '{field}' not present in output.\n"
        f"Full output: {context.result.stdout}"
    )
    actual = data[field]
    assert actual is None, (
        f"Expected JSON field '{field}' = null, got {actual!r}\n"
        f"Full output: {context.result.stdout}"
    )


def _parse_result_json(context: Context) -> dict[str, Any]:
    """Parse stdout as JSON, caching on the context."""
    if not hasattr(context, "json_data") or context.json_data is None:
        try:
            context.json_data = json.loads(context.result.stdout)
        except json.JSONDecodeError as exc:
            raise AssertionError(
                f"Output is not valid JSON: {exc}\n"
                f"stdout: {context.result.stdout[:500]}\n"
                f"stderr: {context.result.stderr[:500]}"
            ) from exc
    return context.json_data
