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
from pathlib import Path

from behave import given, then, when
from behave.runner import Context

from repo_setup_steps import _commit, _init_repo, _write_file


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


def _hook_input_payload(command: str) -> dict:
    """Build a Claude Code Bash-tool PreToolUse payload around `command`."""
    return {
        "tool_name": "Bash",
        "tool_input": {"command": command},
        "hook_event_name": "PreToolUse",
    }


def _run_hook_script(
    context: Context,
    script_path: Path,
    payload: dict,
    extra_env: dict | None = None,
) -> subprocess.CompletedProcess:
    """Invoke a hook script with the given JSON payload on stdin.

    The script's exit code, stdout, and stderr are stored on `context.result`
    using the same shape the rest of the BDD steps expect, so the existing
    `the exit code is N` / `the output contains X` assertions can layer on
    top of these without duplication.
    """
    env = os.environ.copy()
    if extra_env:
        env.update(extra_env)
    proc = subprocess.run(
        [str(script_path)],
        input=json.dumps(payload),
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
    context: Context, method: str, params: dict | None = None
) -> dict:
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
    _run_hook_script(context, script, context.hook_payload)


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
    extra_env: dict | None = None,
) -> subprocess.CompletedProcess:
    """Run `git-prism <args>` with HOME overridden to the isolated tempdir."""
    env = os.environ.copy()
    if hasattr(context, "fake_home"):
        env["HOME"] = str(context.fake_home)
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
    context.captured_sha = _sha256_of_file(settings_path)


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
    captured = getattr(context, "captured_sha", None)
    assert captured is not None, (
        "captured_sha not set -- did the 'I capture the user settings file "
        "sha256' step run?"
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
) -> dict:
    """Call the `review_change` MCP tool over stdio and return the result."""
    args: dict = {
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


def _navigate_review_change(payload: dict, path: str):
    current = payload
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
    _navigate_review_change(payload, key)


@then('the response value "{path}" is greater than {value:d}')
def step_review_response_value_gt(
    context: Context, path: str, value: int
) -> None:
    payload = context.review_change_payload
    actual = _navigate_review_change(payload, path)
    assert actual > value, (
        f"Expected {path} > {value}, got {actual}"
    )


@then('the response key "{path}" is {expected:d}')
def step_review_response_key_eq_int(
    context: Context, path: str, expected: int
) -> None:
    payload = context.review_change_payload
    actual = _navigate_review_change(payload, path)
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
