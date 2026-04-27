"""Step definitions for the bundled redirect-hook epic (#234).

Every step shells out to the real `git-prism` binary or to the bundled
`hooks/git-prism-redirect.sh` script in the repo. None of them mock the
subprocess. Until the implementation issues land, the binary will not
ship the `hooks` subcommand and the `hooks/` directory will not exist —
the steps fail with explicit assertion errors that document the contract
under test.

Hermeticity: every scenario gets a per-scenario `tempfile.TemporaryDirectory()`
under `context.hook_tempdirs` (cleaned up in `after_scenario`). The
`@given("an isolated HOME ...")` step overrides `HOME` for that scenario
only — no test mutates the developer's real `~/.claude/settings.json`.
"""

from __future__ import annotations

import hashlib
import json
import os
import subprocess
import tempfile
from collections.abc import Mapping
from pathlib import Path
from typing import Any

from behave import given, then, when
from behave.runner import Context

from repo_setup_steps import _commit, _init_repo, _write_file


# A loose alias for "JSON-shaped object" — we accept any value type because
# these helpers shuttle Claude Code hook payloads, JSON-RPC envelopes, and
# MCP tool responses, all of which mix strings, ints, lists, and nested
# objects under the same `dict[str, Any]` shape.
JsonObject = dict[str, Any]


# ---------------------------------------------------------------------------
# Common helpers
# ---------------------------------------------------------------------------


def _project_root(context: Context) -> Path:
    """Return the absolute path of the git-prism repo root."""
    return Path(context.project_root)


def _bundled_hook_script(context: Context) -> Path:
    """Return the path to the bundled `hooks/git-prism-redirect.sh` script.

    This file does not exist until #239 lands. Asserting on its presence
    is part of the failing test's contract.
    """
    return _project_root(context) / "hooks" / "git-prism-redirect.sh"


def _scenario_tempdir(context: Context) -> Path:
    """Allocate a fresh tempdir for the current scenario.

    All allocated dirs are tracked on `context.hook_tempdirs` so
    `after_scenario` can tear them down deterministically.
    """
    tmp = tempfile.mkdtemp(prefix="git-prism-bdd-")
    context.cleanup_dirs.append(tmp)
    return Path(tmp)


def _hook_input_payload(command: str) -> JsonObject:
    """Build a Claude Code Bash-tool PreToolUse payload around `command`."""
    return {
        "tool_name": "Bash",
        "tool_input": {"command": command},
        "hook_event_name": "PreToolUse",
    }


def _run_hook_script(
    context: Context,
    script_path: Path,
    payload: JsonObject | None,
    extra_env: Mapping[str, str] | None = None,
    raw_stdin: str | None = None,
) -> subprocess.CompletedProcess[str]:
    """Invoke a hook script with the given JSON payload on stdin.

    Hermeticity: HOME is forced to the per-scenario `context.fake_home`
    when one has been allocated (matching `_run_git_prism`), so the hook
    cannot read the developer's real `~/.claude/...`. If the scenario
    never set up a fake HOME, fall back to a fresh tempdir so we still
    don't touch the real home directory.

    `payload` is JSON-encoded and sent as stdin. Pass `raw_stdin` instead
    to send arbitrary bytes (used for the malformed-JSON and empty-stdin
    fail-open scenarios).
    """
    env = os.environ.copy()
    fake_home = getattr(context, "fake_home", None)
    if fake_home is None:
        fake_home = Path(_scenario_tempdir(context))
    env["HOME"] = str(fake_home)
    if extra_env:
        env.update(extra_env)

    if raw_stdin is not None:
        stdin = raw_stdin
    elif payload is None:
        stdin = ""
    else:
        stdin = json.dumps(payload)

    proc = subprocess.run(
        [str(script_path)],
        input=stdin,
        capture_output=True,
        text=True,
        env=env,
    )
    context.result = proc
    return proc


def _sha256_of_file(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


# ---------------------------------------------------------------------------
# W2: tools/list assertions (#237)
# ---------------------------------------------------------------------------


def _send_jsonrpc_to_server(
    context: Context, method: str, params: JsonObject | None = None
) -> JsonObject:
    """Spawn `git-prism serve`, send one JSON-RPC request, return the response.

    The MCP server speaks line-delimited JSON-RPC over stdio. We send an
    `initialize` first (the rmcp framework requires it before `tools/list`
    will return anything) followed by the method under test, then close
    stdin and parse the responses.
    """
    binary = context.binary_path
    initialize_req = {
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": "2024-11-05",
            "capabilities": {},
            "clientInfo": {"name": "behave", "version": "0.0"},
        },
    }
    initialized_notif = {
        "jsonrpc": "2.0",
        "method": "notifications/initialized",
        "params": {},
    }
    target_req = {
        "jsonrpc": "2.0",
        "id": 2,
        "method": method,
        "params": params or {},
    }
    payload = (
        json.dumps(initialize_req)
        + "\n"
        + json.dumps(initialized_notif)
        + "\n"
        + json.dumps(target_req)
        + "\n"
    )

    proc = subprocess.run(
        [binary, "serve"],
        input=payload,
        capture_output=True,
        text=True,
        timeout=20,
    )
    context.result = proc
    # Find the response with id == 2
    for line in proc.stdout.splitlines():
        line = line.strip()
        if not line:
            continue
        try:
            obj = json.loads(line)
        except json.JSONDecodeError:
            continue
        if obj.get("id") == 2:
            return obj
    raise AssertionError(
        f"No JSON-RPC response with id=2 found.\n"
        f"stdout: {proc.stdout[:1000]}\n"
        f"stderr: {proc.stderr[:1000]}"
    )


@given("the git-prism MCP server is running over stdio")
def step_mcp_server_running(context: Context) -> None:
    """No-op marker; the server is spawned per-request below.

    The behave `Given` slot would normally set up long-running state, but
    spawning a real persistent MCP child here would leak state across
    scenarios. Instead, each `When` step that needs the server respawns
    it with the canonical `initialize` -> `notifications/initialized` ->
    `<target>` triple. We capture that intent here to keep the Gherkin
    readable.
    """
    context.mcp_server_marker = True


@when('I send a "tools/list" JSON-RPC request')
def step_send_tools_list(context: Context) -> None:
    response = _send_jsonrpc_to_server(context, "tools/list")
    context.tools_list_response = response
    assert "result" in response, (
        f"tools/list returned no 'result': {response}"
    )
    tools = response["result"].get("tools", [])
    assert tools, f"tools/list returned an empty tool list: {response}"
    context.tool_descriptions = {t.get("name"): t.get("description", "") for t in tools}


@then('the description for "{tool_name}" mentions "{phrase}"')
def step_description_mentions(
    context: Context, tool_name: str, phrase: str
) -> None:
    descriptions = getattr(context, "tool_descriptions", None)
    assert descriptions is not None, (
        "tool_descriptions not populated -- the 'tools/list' step did not run"
    )
    assert tool_name in descriptions, (
        f"Tool '{tool_name}' not found in tools/list response. "
        f"Got: {sorted(descriptions.keys())}"
    )
    desc = descriptions[tool_name]
    # Minimum-length check defends against keyword stuffing: an impl could
    # satisfy the substring check by jamming all four expected phrases into
    # a 30-character description. Tool descriptions are user-facing prose
    # and need enough context for an LLM agent to make a routing decision.
    minimum_description_chars = 80
    assert len(desc) >= minimum_description_chars, (
        f"Description for '{tool_name}' is too short to be meaningful "
        f"(got {len(desc)} chars, expected >= {minimum_description_chars}). "
        f"This guards against keyword stuffing.\n"
        f"Description was: {desc!r}"
    )
    assert phrase.lower() in desc.lower(), (
        f"Description for '{tool_name}' does not mention '{phrase}'.\n"
        f"Description was: {desc!r}"
    )


# ---------------------------------------------------------------------------
# W3: bash tokenizer scenarios (#238)
# ---------------------------------------------------------------------------


@given('a hook input with bash command "{command}"')
def step_hook_input_with_command(context: Context, command: str) -> None:
    context.hook_payload = _hook_input_payload(command)
    context.hook_command = command


@given('a hook input with the bash command from "{fixture}"')
def step_hook_input_from_fixture(context: Context, fixture: str) -> None:
    fixture_path = (
        _project_root(context) / "bdd" / "fixtures" / "hook_inputs" / fixture
    )
    assert fixture_path.is_file(), (
        f"Hook-input fixture not found: {fixture_path}"
    )
    command = fixture_path.read_text()
    context.hook_payload = _hook_input_payload(command)
    context.hook_command = command


@when("I run the bundled redirect hook with that input")
def step_run_bundled_hook(context: Context) -> None:
    script = _bundled_hook_script(context)
    assert script.is_file(), (
        f"Bundled hook script not found at {script}. "
        f"It must be shipped by the install-hooks subcommand work (#239)."
    )
    extra_env = getattr(context, "hook_extra_env", None)
    _run_hook_script(context, script, context.hook_payload, extra_env=extra_env)


@then("the hook exit code is {code:d}")
def step_hook_exit_code(context: Context, code: int) -> None:
    actual = context.result.returncode
    assert actual == code, (
        f"Expected hook exit code {code}, got {actual}.\n"
        f"command: {context.hook_command!r}\n"
        f"stdout: {context.result.stdout!r}\n"
        f"stderr: {context.result.stderr!r}"
    )


@then('the hook stdout is JSON containing redirect advice for "{tool_name}"')
def step_hook_stdout_redirect_advice(context: Context, tool_name: str) -> None:
    out = context.result.stdout.strip()
    assert out, (
        f"Hook stdout is empty -- expected redirect advice for {tool_name}.\n"
        f"command: {context.hook_command!r}\n"
        f"stderr: {context.result.stderr!r}"
    )
    try:
        payload = json.loads(out)
    except json.JSONDecodeError as e:
        raise AssertionError(
            f"Hook stdout is not valid JSON: {e}\nstdout: {out!r}"
        ) from e
    hook_specific = payload.get("hookSpecificOutput", {})
    advice = hook_specific.get("additionalContext", "")
    assert tool_name in advice, (
        f"Redirect advice does not mention '{tool_name}'.\n"
        f"hookSpecificOutput: {hook_specific!r}"
    )


@then('the hook does not attempt to expand "{var}"')
def step_hook_does_not_expand(context: Context, var: str) -> None:
    """The advice text must contain the literal `$VAR` rather than an
    expanded value. The shell expansion would be a security/privacy bug
    (the hook would leak env vars into stdout/telemetry).
    """
    out = context.result.stdout
    assert var in out, (
        f"Expected literal {var!r} to appear in hook stdout (proving no "
        f"expansion happened). stdout: {out!r}"
    )


@then("the hook stdout is empty")
def step_hook_stdout_empty(context: Context) -> None:
    out = context.result.stdout
    assert out.strip() == "", (
        f"Expected empty hook stdout for command "
        f"{getattr(context, 'hook_command', '?')!r}, got: {out!r}"
    )


@then("the hook stderr is empty")
def step_hook_stderr_empty(context: Context) -> None:
    err = context.result.stderr
    assert err.strip() == "", (
        f"Expected empty hook stderr for command "
        f"{getattr(context, 'hook_command', '?')!r}, got: {err!r}"
    )


@then('the hook stderr contains "{phrase}"')
def step_hook_stderr_contains(context: Context, phrase: str) -> None:
    err = context.result.stderr
    assert phrase in err, (
        f"Expected hook stderr to contain {phrase!r}.\nstderr: {err!r}"
    )


@then('the hook stdout matches "{phrase}"')
def step_hook_stdout_matches(context: Context, phrase: str) -> None:
    out = context.result.stdout
    assert phrase in out, (
        f"Expected hook stdout to contain {phrase!r}.\nstdout: {out!r}"
    )


# ---------------------------------------------------------------------------
# W4: install-hooks scenarios (#239)
# ---------------------------------------------------------------------------


def _isolated_home(context: Context) -> Path:
    """Allocate a fresh tempdir to use as $HOME and create ~/.claude in it.

    Mutates `context.fake_home` and `context.user_settings_path` so later
    steps can locate the settings file without re-deriving the path.
    """
    home = _scenario_tempdir(context)
    (home / ".claude").mkdir(parents=True, exist_ok=True)
    context.fake_home = home
    context.user_settings_path = home / ".claude" / "settings.json"
    context.user_hooks_dir = home / ".claude" / "hooks"
    return home


@given("an isolated HOME with an empty .claude directory")
def step_isolated_home(context: Context) -> None:
    _isolated_home(context)


@given("a temporary git repository as the working directory")
def step_temp_repo_as_cwd(context: Context) -> None:
    repo_dir = _init_repo(context)
    _write_file(repo_dir, "README.md", "# test\n")
    _commit(repo_dir, "initial commit", ["README.md"])
    context.project_repo_path = Path(repo_dir)
    context.project_settings_path = (
        Path(repo_dir) / ".claude" / "settings.json"
    )
    context.project_hooks_dir = Path(repo_dir) / ".claude" / "hooks"


@given(
    'the user settings file contains an unrelated PreToolUse entry with '
    'id "{entry_id}"'
)
def step_seed_user_setting(context: Context, entry_id: str) -> None:
    """Seed an existing entry in the user settings before install runs.

    This proves uninstall is surgical (only touches our own entries).
    """
    settings_path = context.user_settings_path
    settings_path.parent.mkdir(parents=True, exist_ok=True)
    existing = {
        "hooks": {
            "PreToolUse": [
                {"id": entry_id, "matcher": "Bash", "command": "echo unrelated"}
            ]
        }
    }
    settings_path.write_text(json.dumps(existing, indent=2))


def _run_git_prism(
    context: Context,
    args: list[str],
    cwd: Path | None = None,
    extra_env: Mapping[str, str] | None = None,
) -> subprocess.CompletedProcess[str]:
    """Run `git-prism <args>` with HOME overridden to the isolated tempdir."""
    env = os.environ.copy()
    fake_home = getattr(context, "fake_home", None)
    if fake_home is not None:
        env["HOME"] = str(fake_home)
    if extra_env:
        env.update(extra_env)
    proc = subprocess.run(
        [context.binary_path, *args],
        capture_output=True,
        text=True,
        cwd=str(cwd) if cwd else None,
        env=env,
    )
    context.result = proc
    return proc


@when("I install the redirect hook at user scope")
def step_install_user_scope(context: Context) -> None:
    _run_git_prism(context, ["hooks", "install", "--scope", "user"])


@when("I install the redirect hook at project scope in the repo")
def step_install_project_scope(context: Context) -> None:
    _run_git_prism(
        context,
        ["hooks", "install", "--scope", "project"],
        cwd=context.project_repo_path,
    )


@when("I uninstall the redirect hook at user scope")
def step_uninstall_user_scope(context: Context) -> None:
    _run_git_prism(context, ["hooks", "uninstall", "--scope", "user"])


@when("I capture the user settings file sha256")
def step_capture_user_settings_sha(context: Context) -> None:
    settings_path = context.user_settings_path
    assert settings_path.is_file(), (
        f"Expected settings file at {settings_path} after install"
    )
    context.captured_sha256 = _sha256_of_file(settings_path)


@then(
    'the user settings file contains a PreToolUse entry with id "{entry_id}"'
)
def step_user_settings_has_entry(context: Context, entry_id: str) -> None:
    settings_path = context.user_settings_path
    assert settings_path.is_file(), (
        f"Expected user settings file at {settings_path} -- "
        f"`git-prism hooks install` did not write it."
    )
    data = json.loads(settings_path.read_text())
    entries = data.get("hooks", {}).get("PreToolUse", [])
    ids = [e.get("id") for e in entries]
    assert entry_id in ids, (
        f"Expected PreToolUse entry with id {entry_id!r} in {settings_path}.\n"
        f"Found ids: {ids}"
    )


@then(
    'the user settings file does not contain a PreToolUse entry with id '
    '"{entry_id}"'
)
def step_user_settings_lacks_entry(context: Context, entry_id: str) -> None:
    settings_path = context.user_settings_path
    assert settings_path.is_file(), (
        f"Expected user settings file at {settings_path}"
    )
    data = json.loads(settings_path.read_text())
    entries = data.get("hooks", {}).get("PreToolUse", [])
    ids = [e.get("id") for e in entries]
    assert entry_id not in ids, (
        f"Expected PreToolUse id {entry_id!r} to be ABSENT from "
        f"{settings_path}, but found it. ids: {ids}"
    )


@then(
    'the project settings file contains a PreToolUse entry with id "{entry_id}"'
)
def step_project_settings_has_entry(context: Context, entry_id: str) -> None:
    settings_path = context.project_settings_path
    assert settings_path.is_file(), (
        f"Expected project settings file at {settings_path} -- "
        f"`git-prism hooks install --scope project` did not write it."
    )
    data = json.loads(settings_path.read_text())
    entries = data.get("hooks", {}).get("PreToolUse", [])
    ids = [e.get("id") for e in entries]
    assert entry_id in ids, (
        f"Expected PreToolUse entry with id {entry_id!r} in {settings_path}.\n"
        f"Found ids: {ids}"
    )


@then('the user hooks directory contains a "{filename}" script')
def step_user_hooks_dir_has_script(context: Context, filename: str) -> None:
    script = context.user_hooks_dir / filename
    assert script.is_file(), (
        f"Expected hook script at {script}. "
        f"`git-prism hooks install --scope user` did not copy the bundled hook."
    )
    # Sanity: must be executable so Claude Code can run it directly.
    mode = script.stat().st_mode
    assert mode & 0o111, (
        f"Hook script at {script} is not executable (mode={oct(mode)})."
    )


@then('the project hooks directory contains a "{filename}" script')
def step_project_hooks_dir_has_script(context: Context, filename: str) -> None:
    script = context.project_hooks_dir / filename
    assert script.is_file(), (
        f"Expected hook script at {script}. "
        f"`git-prism hooks install --scope project` did not copy the bundled hook."
    )
    mode = script.stat().st_mode
    assert mode & 0o111, (
        f"Hook script at {script} is not executable (mode={oct(mode)})."
    )


@then("the user settings file sha256 is unchanged")
def step_user_settings_sha_unchanged(context: Context) -> None:
    settings_path = context.user_settings_path
    captured = getattr(context, "captured_sha256", None)
    assert captured is not None, (
        "captured_sha256 not set -- did the 'I capture the user settings "
        "file sha256' step run?"
    )
    current = _sha256_of_file(settings_path)
    assert current == captured, (
        f"Settings file changed between installs (expected idempotency).\n"
        f"before: {captured}\nafter:  {current}\n"
        f"path: {settings_path}"
    )


@given("an isolated HOME with the bundled hook installed at user scope")
def step_isolated_home_with_install(context: Context) -> None:
    _isolated_home(context)
    proc = _run_git_prism(context, ["hooks", "install", "--scope", "user"])
    assert proc.returncode == 0, (
        f"`git-prism hooks install --scope user` failed in fixture setup.\n"
        f"stdout: {proc.stdout!r}\nstderr: {proc.stderr!r}"
    )


@when("I run the installed user-scope hook with that input")
def step_run_installed_user_hook(context: Context) -> None:
    script = context.user_hooks_dir / "git-prism-redirect.sh"
    assert script.is_file(), (
        f"Installed user-scope hook missing at {script}. "
        f"`git-prism hooks install --scope user` did not copy it."
    )
    _run_hook_script(context, script, context.hook_payload)


# ---------------------------------------------------------------------------
# W5: review_change MCP tool scenarios (#240)
# ---------------------------------------------------------------------------


@given("a git repository with many changed files")
def step_repo_with_many_changes(context: Context) -> None:
    """Create a repo with 12 changed files so a page_size of 5 forces
    pagination on at least one sub-response."""
    repo_dir = _init_repo(context)
    # Anchor commit so HEAD~1 resolves.
    _write_file(repo_dir, "anchor.txt", "anchor\n")
    _commit(repo_dir, "anchor", ["anchor.txt"])

    files: list[str] = []
    for i in range(12):
        name = f"file_{i:02d}.py"
        _write_file(
            repo_dir,
            name,
            f"def fn_{i}():\n    return {i}\n",
        )
        files.append(name)
    _commit(repo_dir, "add many files", files)


def _call_review_change(
    context: Context,
    base: str,
    head: str,
    *,
    max_response_tokens: int | None = None,
    page_size: int | None = None,
) -> JsonObject:
    """Call the `review_change` MCP tool over stdio and return the result."""
    args: JsonObject = {
        "repo_path": str(context.repo_path),
        "base_ref": base,
        "head_ref": head,
    }
    if max_response_tokens is not None:
        args["max_response_tokens"] = max_response_tokens
    if page_size is not None:
        args["page_size"] = page_size
    response = _send_jsonrpc_to_server(
        context,
        "tools/call",
        {"name": "review_change", "arguments": args},
    )
    assert "result" in response, (
        f"review_change returned no 'result': {response}"
    )
    # MCP tool responses wrap the JSON payload in `content[0].text` (or
    # `structuredContent`, depending on the rmcp version). Try both.
    result = response["result"]
    if "structuredContent" in result:
        return result["structuredContent"]
    content = result.get("content", [])
    assert content, f"review_change result has no content: {result}"
    text = content[0].get("text", "")
    try:
        return json.loads(text)
    except json.JSONDecodeError as e:
        raise AssertionError(
            f"review_change content[0].text is not valid JSON: {e}\ntext: {text!r}"
        ) from e


@when(
    'I call the "review_change" tool with base "{base}" and head "{head}"'
)
def step_call_review_change_simple(
    context: Context, base: str, head: str
) -> None:
    context.review_change_payload = _call_review_change(context, base, head)


@when(
    'I call the "review_change" tool with base "{base}", head "{head}", and '
    'max_response_tokens {tokens:d}'
)
def step_call_review_change_with_budget(
    context: Context, base: str, head: str, tokens: int
) -> None:
    context.review_change_payload = _call_review_change(
        context, base, head, max_response_tokens=tokens
    )


@when(
    'I call the "review_change" tool with base "{base}", head "{head}", and '
    'page_size {size:d}'
)
def step_call_review_change_with_page_size(
    context: Context, base: str, head: str, size: int
) -> None:
    context.review_change_payload = _call_review_change(
        context, base, head, page_size=size
    )


def _get_dotted_path(payload: JsonObject, path: str) -> Any:
    """Walk a dotted path through a JSON-shaped dict and return the leaf.

    Generic helper — not specific to `review_change`. Used by every step
    that asserts on a nested key in any MCP tool response.
    """
    current: Any = payload
    for part in path.split("."):
        assert isinstance(current, dict), (
            f"Expected dict at '{part}' in path '{path}', got {type(current).__name__}"
        )
        assert part in current, (
            f"Key '{part}' missing in path '{path}'. "
            f"Available: {sorted(current.keys())}"
        )
        current = current[part]
    return current


@then('the response has key "{key}"')
def step_review_response_has_key(context: Context, key: str) -> None:
    payload = getattr(context, "review_change_payload", None)
    assert payload is not None, (
        "review_change_payload not set -- did the When step run?"
    )
    _get_dotted_path(payload, key)


@then('the response value "{path}" is greater than {value:d}')
def step_review_response_value_gt(
    context: Context, path: str, value: int
) -> None:
    payload = context.review_change_payload
    actual = _get_dotted_path(payload, path)
    assert actual > value, (
        f"Expected {path} > {value}, got {actual}"
    )


@then('the response key "{path}" is {expected:d}')
def step_review_response_key_eq_int(
    context: Context, path: str, expected: int
) -> None:
    payload = context.review_change_payload
    actual = _get_dotted_path(payload, path)
    assert actual == expected, (
        f"Expected {path} == {expected}, got {actual!r}"
    )


@then('at least one sub-response in the result has a non-null "next_cursor"')
def step_at_least_one_subresponse_paginated(context: Context) -> None:
    payload = context.review_change_payload
    cursors: list = []
    for sub_key in ("manifest", "function_context"):
        sub = payload.get(sub_key, {})
        cursor = sub.get("pagination", {}).get("next_cursor")
        cursors.append((sub_key, cursor))
    paginated = [k for k, c in cursors if c]
    assert paginated, (
        f"No sub-response paginated. cursors={cursors}\n"
        f"Expected at least one non-null next_cursor when page_size is small."
    )


# ---------------------------------------------------------------------------
# Tokenizer triangulation: env var leakage, command-substitution boundaries
# ---------------------------------------------------------------------------


@given('the environment variable "{name}" is set to "{value}"')
def step_set_env_var(context: Context, name: str, value: str) -> None:
    """Stash an env var that `_run_hook_script` will inject via extra_env.

    Used to triangulate the "tokenizer does not expand variables" property:
    if the impl accidentally calls `os.path.expandvars` or shells out, the
    secret value would leak into stdout/stderr. The follow-up `does not
    leak` step asserts that doesn't happen.
    """
    context.hook_extra_env = getattr(context, "hook_extra_env", {})
    context.hook_extra_env[name] = value


@then('the hook output does not leak the value "{value}"')
def step_hook_output_no_leak(context: Context, value: str) -> None:
    out = context.result.stdout
    err = context.result.stderr
    combined = f"{out}\n{err}"
    assert value not in combined, (
        f"Expected secret value {value!r} to NOT appear in hook output, "
        f"but found it. This indicates the tokenizer expanded "
        f"environment variables (security/privacy bug).\n"
        f"stdout: {out!r}\nstderr: {err!r}"
    )


@then('the hook stdout does not contain redirect advice for "{phrase}"')
def step_hook_stdout_no_advice_for(context: Context, phrase: str) -> None:
    """Assert a watch-list-miss command did not trigger advice.

    Some payloads contain BOTH a watch-list match (e.g., outer `git diff`)
    and a non-match (`git rev-parse`); the advice payload must mention
    only the match. The assertion looks at the parsed `additionalContext`
    field rather than raw stdout to avoid false positives from JSON
    structure that happens to contain the phrase as a substring.
    """
    out = context.result.stdout.strip()
    if not out:
        return  # Nothing emitted means nothing to leak.
    try:
        payload = json.loads(out)
    except json.JSONDecodeError:
        # If stdout isn't JSON the test will fail elsewhere; nothing to
        # check here.
        return
    advice = payload.get("hookSpecificOutput", {}).get("additionalContext", "")
    assert phrase not in advice, (
        f"Hook advice unexpectedly mentions {phrase!r}.\n"
        f"advice: {advice!r}"
    )


# ---------------------------------------------------------------------------
# Hard-block / fail-open scenarios (#239)
# ---------------------------------------------------------------------------


@when("I run the bundled redirect hook with empty stdin")
def step_run_hook_empty_stdin(context: Context) -> None:
    script = _bundled_hook_script(context)
    assert script.is_file(), (
        f"Bundled hook script not found at {script}. "
        f"It must be shipped by the install-hooks subcommand work (#239)."
    )
    _run_hook_script(context, script, payload=None, raw_stdin="")


@when('I run the bundled redirect hook with stdin "{stdin}"')
def step_run_hook_raw_stdin(context: Context, stdin: str) -> None:
    script = _bundled_hook_script(context)
    assert script.is_file(), (
        f"Bundled hook script not found at {script}. "
        f"It must be shipped by the install-hooks subcommand work (#239)."
    )
    _run_hook_script(context, script, payload=None, raw_stdin=stdin)


@when('I run the bundled redirect hook with that input and PATH "{path}"')
def step_run_hook_with_path(context: Context, path: str) -> None:
    script = _bundled_hook_script(context)
    assert script.is_file(), (
        f"Bundled hook script not found at {script}. "
        f"It must be shipped by the install-hooks subcommand work (#239)."
    )
    _run_hook_script(
        context,
        script,
        context.hook_payload,
        extra_env={"PATH": path},
    )


@then("the hook stderr is at most {n:d} line")
@then("the hook stderr is at most {n:d} lines")
def step_hook_stderr_at_most_n_lines(context: Context, n: int) -> None:
    err = context.result.stderr.rstrip("\n")
    if not err:
        return
    line_count = len(err.split("\n"))
    assert line_count <= n, (
        f"Expected hook stderr to be at most {n} line(s), got {line_count}.\n"
        f"stderr: {err!r}"
    )


# ---------------------------------------------------------------------------
# Idempotency / install-path triangulation (#239)
# ---------------------------------------------------------------------------


def _seed_user_settings_with_redirect_entry(
    context: Context, entry_id: str, command: str
) -> None:
    """Write a single PreToolUse entry with the given id+command to user settings.

    Used to set up Path 3 (stale script path), Path 4a (user-edited
    command), Path 4b (--force overwrites), and the v2 downgrade-refusal
    scenarios.
    """
    settings_path = context.user_settings_path
    settings_path.parent.mkdir(parents=True, exist_ok=True)
    existing = {
        "hooks": {
            "PreToolUse": [
                {"id": entry_id, "matcher": "Bash", "command": command}
            ]
        }
    }
    settings_path.write_text(json.dumps(existing, indent=2))


@given(
    'the user settings file contains a "{entry_id}" entry pointing to "{path}"'
)
def step_seed_user_settings_with_path(
    context: Context, entry_id: str, path: str
) -> None:
    _seed_user_settings_with_redirect_entry(context, entry_id, path)


@given(
    'the user settings file contains a "{entry_id}" entry with command "{command}"'
)
def step_seed_user_settings_with_command(
    context: Context, entry_id: str, command: str
) -> None:
    _seed_user_settings_with_redirect_entry(context, entry_id, command)


@when("I capture the user settings PreToolUse length")
def step_capture_user_pretooluse_length(context: Context) -> None:
    settings_path = context.user_settings_path
    data = json.loads(settings_path.read_text())
    entries = data.get("hooks", {}).get("PreToolUse", [])
    context.captured_pretooluse_length = len(entries)


@when('I install the redirect hook at user scope with "{flag}"')
def step_install_user_with_flag(context: Context, flag: str) -> None:
    _run_git_prism(context, ["hooks", "install", "--scope", "user", flag])


@when("I install the redirect hook at local scope in the repo")
def step_install_local_scope(context: Context) -> None:
    _run_git_prism(
        context,
        ["hooks", "install", "--scope", "local"],
        cwd=context.project_repo_path,
    )


@given("the redirect hook is installed at user scope")
def step_redirect_hook_installed_at_user(context: Context) -> None:
    proc = _run_git_prism(context, ["hooks", "install", "--scope", "user"])
    assert proc.returncode == 0, (
        f"Setup failed: hooks install --scope user did not succeed.\n"
        f"stdout: {proc.stdout!r}\nstderr: {proc.stderr!r}"
    )


@when(
    'I run "hooks install --scope project" in the repo and answer "{answer}"'
)
def step_install_project_with_answer(context: Context, answer: str) -> None:
    """Drive the cross-scope confirmation prompt by piping `answer` to stdin.

    The installer must write the prompt to stderr (so a piped stdout still
    works for diff output) and read a single character from stdin.
    """
    env = os.environ.copy()
    fake_home = getattr(context, "fake_home", None)
    if fake_home is not None:
        env["HOME"] = str(fake_home)
    proc = subprocess.run(
        [context.binary_path, "hooks", "install", "--scope", "project"],
        input=f"{answer}\n",
        capture_output=True,
        text=True,
        cwd=str(context.project_repo_path),
        env=env,
    )
    context.result = proc


@given('the redirect hook install state is "{state}"')
def step_redirect_hook_state(context: Context, state: str) -> None:
    """Drive `hooks status` triangulation by setting up three different states.

    `none` leaves both settings files absent; `user-only` runs the user
    install; `user-and-project` runs both. The status command must
    distinguish all three.
    """
    if state == "none":
        return
    proc = _run_git_prism(context, ["hooks", "install", "--scope", "user"])
    assert proc.returncode == 0, (
        f"Setup failed for state={state!r}: user install failed. "
        f"stdout: {proc.stdout!r} stderr: {proc.stderr!r}"
    )
    if state == "user-only":
        return
    if state == "user-and-project":
        proc = _run_git_prism(
            context,
            ["hooks", "install", "--scope", "project", "--force"],
            cwd=context.project_repo_path,
        )
        assert proc.returncode == 0, (
            f"Setup failed for state={state!r}: project install failed. "
            f"stdout: {proc.stdout!r} stderr: {proc.stderr!r}"
        )
        return
    raise AssertionError(f"Unknown install state: {state!r}")


@when('I run "hooks status" in the repo')
def step_run_hooks_status(context: Context) -> None:
    _run_git_prism(
        context, ["hooks", "status"], cwd=context.project_repo_path
    )


@then("the user settings PreToolUse length is unchanged")
def step_user_settings_pretooluse_length_unchanged(context: Context) -> None:
    captured = getattr(context, "captured_pretooluse_length", None)
    assert captured is not None, (
        "captured_pretooluse_length not set -- did the capture step run?"
    )
    settings_path = context.user_settings_path
    data = json.loads(settings_path.read_text())
    entries = data.get("hooks", {}).get("PreToolUse", [])
    assert len(entries) == captured, (
        f"PreToolUse array length changed across idempotent installs: "
        f"before={captured}, after={len(entries)}.\n"
        f"This indicates a duplicate-entry bug masked by hash equality."
    )


@then(
    'the user settings file contains exactly one PreToolUse entry with id '
    '"{entry_id}"'
)
def step_user_settings_exactly_one_entry(
    context: Context, entry_id: str
) -> None:
    settings_path = context.user_settings_path
    assert settings_path.is_file(), (
        f"Expected user settings file at {settings_path}"
    )
    data = json.loads(settings_path.read_text())
    entries = data.get("hooks", {}).get("PreToolUse", [])
    matching = [e for e in entries if e.get("id") == entry_id]
    assert len(matching) == 1, (
        f"Expected exactly one PreToolUse entry with id {entry_id!r}, "
        f"got {len(matching)}.\nentries: {entries}"
    )


def _user_pretooluse_entry(context: Context, entry_id: str) -> JsonObject:
    settings_path = context.user_settings_path
    assert settings_path.is_file(), (
        f"Expected user settings file at {settings_path}"
    )
    data = json.loads(settings_path.read_text())
    entries = data.get("hooks", {}).get("PreToolUse", [])
    for entry in entries:
        if entry.get("id") == entry_id:
            return entry
    raise AssertionError(
        f"No PreToolUse entry with id {entry_id!r} in {settings_path}.\n"
        f"entries: {entries}"
    )


@then(
    'the user settings file PreToolUse entry "{entry_id}" command does not '
    'contain "{phrase}"'
)
def step_user_pretooluse_command_lacks(
    context: Context, entry_id: str, phrase: str
) -> None:
    entry = _user_pretooluse_entry(context, entry_id)
    command = entry.get("command", "")
    assert phrase not in command, (
        f"Expected entry {entry_id!r} command to NOT contain {phrase!r}, "
        f"but command was {command!r}."
    )


@then(
    'the user settings file PreToolUse entry "{entry_id}" command contains '
    '"{phrase}"'
)
def step_user_pretooluse_command_contains(
    context: Context, entry_id: str, phrase: str
) -> None:
    entry = _user_pretooluse_entry(context, entry_id)
    command = entry.get("command", "")
    assert phrase in command, (
        f"Expected entry {entry_id!r} command to contain {phrase!r}, "
        f"but command was {command!r}."
    )


@then(
    'the user settings file PreToolUse entry "{entry_id}" command equals '
    '"{expected}"'
)
def step_user_pretooluse_command_equals(
    context: Context, entry_id: str, expected: str
) -> None:
    entry = _user_pretooluse_entry(context, entry_id)
    command = entry.get("command", "")
    assert command == expected, (
        f"Expected entry {entry_id!r} command to equal {expected!r}, "
        f"but command was {command!r}."
    )


@then(
    'the user settings file PreToolUse entry "{entry_id}" command does not '
    'equal "{expected}"'
)
def step_user_pretooluse_command_not_equals(
    context: Context, entry_id: str, expected: str
) -> None:
    entry = _user_pretooluse_entry(context, entry_id)
    command = entry.get("command", "")
    assert command != expected, (
        f"Expected entry {entry_id!r} command to NOT equal {expected!r}, "
        f"but it does. The user-edited command was overwritten."
    )


@then('the install stdout or stderr mentions "{phrase}"')
def step_install_output_mentions(context: Context, phrase: str) -> None:
    combined = f"{context.result.stdout}\n{context.result.stderr}"
    assert phrase in combined, (
        f"Expected install output to mention {phrase!r}.\n"
        f"stdout: {context.result.stdout!r}\nstderr: {context.result.stderr!r}"
    )


@then("the hook exit code is not {expected:d}")
def step_hook_exit_code_not(context: Context, expected: int) -> None:
    actual = context.result.returncode
    assert actual != expected, (
        f"Expected exit code != {expected}, got {actual}.\n"
        f"stdout: {context.result.stdout!r}\nstderr: {context.result.stderr!r}"
    )


@then('the hook stdout contains both "{phrase_a}" and "{phrase_b}"')
def step_hook_stdout_contains_both(
    context: Context, phrase_a: str, phrase_b: str
) -> None:
    out = context.result.stdout
    missing = [p for p in (phrase_a, phrase_b) if p not in out]
    assert not missing, (
        f"Expected hook stdout to contain BOTH {phrase_a!r} and {phrase_b!r}, "
        f"but missing: {missing!r}.\nstdout: {out!r}"
    )


# ---------------------------------------------------------------------------
# Scope semantics (#239)
# ---------------------------------------------------------------------------


@then(
    'the project local settings file contains a PreToolUse entry with id '
    '"{entry_id}"'
)
def step_project_local_settings_has_entry(
    context: Context, entry_id: str
) -> None:
    settings_path = (
        Path(context.project_repo_path) / ".claude" / "settings.local.json"
    )
    assert settings_path.is_file(), (
        f"Expected project-local settings file at {settings_path} -- "
        f"`hooks install --scope local` did not write it."
    )
    data = json.loads(settings_path.read_text())
    entries = data.get("hooks", {}).get("PreToolUse", [])
    ids = [e.get("id") for e in entries]
    assert entry_id in ids, (
        f"Expected PreToolUse entry with id {entry_id!r} in {settings_path}.\n"
        f"Found ids: {ids}"
    )


@then("the project settings file does not exist")
def step_project_settings_does_not_exist(context: Context) -> None:
    settings_path = (
        Path(context.project_repo_path) / ".claude" / "settings.json"
    )
    assert not settings_path.exists(), (
        f"Expected project settings file at {settings_path} to NOT exist, "
        f"but it does. (Wrong scope wrote here.)"
    )


@then("the user settings file does not exist")
def step_user_settings_does_not_exist(context: Context) -> None:
    settings_path = context.user_settings_path
    assert not settings_path.exists(), (
        f"Expected user settings file at {settings_path} to NOT exist, "
        f"but it does. (--dry-run wrote when it should not have.)"
    )


@then('the hook stdout contains "{phrase}"')
def step_hook_stdout_contains(context: Context, phrase: str) -> None:
    out = context.result.stdout
    assert phrase in out, (
        f"Expected hook stdout to contain {phrase!r}.\nstdout: {out!r}"
    )


# ---------------------------------------------------------------------------
# review_change cursor walk + budget triangulation (#240)
# ---------------------------------------------------------------------------


def _files_in_manifest_page(payload: JsonObject) -> set[str]:
    manifest = payload.get("manifest", {})
    files = manifest.get("files", []) or manifest.get("file_changes", [])
    out: set[str] = set()
    for entry in files:
        if isinstance(entry, dict):
            path = entry.get("path") or entry.get("file_path")
            if path:
                out.add(path)
    return out


@then(
    'following the manifest "next_cursor" returns a different set of files than '
    'page 1'
)
def step_follow_cursor_returns_different_files(context: Context) -> None:
    """Page 1 was already captured; call review_change again with the cursor.

    Catches a hardcoded-cursor bug (e.g., the impl always emits the same
    opaque token but ignores it on the next call). If the second page
    has the same file set as the first, pagination is fake.
    """
    payload = context.review_change_payload
    page1_files = _files_in_manifest_page(payload)
    cursor = (
        payload.get("manifest", {})
        .get("pagination", {})
        .get("next_cursor")
    )
    assert cursor, (
        f"No manifest cursor to follow. payload keys: "
        f"{list(payload.get('manifest', {}).keys())}"
    )

    # Re-issue the call with the cursor. We assume the same base/head/page_size
    # the first call used; agents replay it via the manifest's own cursor.
    args: dict = {
        "repo_path": str(context.repo_path),
        "base_ref": "HEAD~1",
        "head_ref": "HEAD",
        "page_size": 5,
        "manifest_cursor": cursor,
    }
    response = _send_jsonrpc_to_server(
        context,
        "tools/call",
        {"name": "review_change", "arguments": args},
    )
    assert "result" in response, (
        f"Cursor walk returned no 'result': {response}"
    )
    result = response["result"]
    if "structuredContent" in result:
        page2 = result["structuredContent"]
    else:
        content = result.get("content", [])
        assert content, f"Cursor walk has no content: {result}"
        page2 = json.loads(content[0]["text"])

    page2_files = _files_in_manifest_page(page2)
    assert page1_files != page2_files, (
        f"Cursor walk returned the same files as page 1 — pagination is "
        f"hardcoded.\npage1: {sorted(page1_files)}\npage2: {sorted(page2_files)}"
    )
    assert page2_files, (
        f"Cursor walk returned an empty manifest page; expected the next "
        f"slice of files.\npage2: {page2}"
    )
