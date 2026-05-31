"""Step definitions for surviving redirect-hook epic scenarios (#234).

The redirect hook (bash_redirect_hook.py / git-prism-redirect.sh) was
removed in v0.9.0 (ADR-0011). This file retains only the steps used by
the surviving scenarios in redirect_hook.feature:

  @ISSUE-237  -- MCP tool descriptions (server.rs)
  @ISSUE-239  -- hooks uninstall and hooks status (legacy cleanup)
  @ISSUE-240  -- review_change MCP tool (server.rs)

Steps that drove the deleted hook script (W3 tokenizer scenarios, install
scenarios, fail-open scenarios, idempotency triangulation, scope semantics)
have been removed along with the deleted scenarios.

Hermeticity: every scenario gets a per-scenario `tempfile.TemporaryDirectory()`
under `context.cleanup_dirs` (cleaned up in `after_scenario`). The
`@given("an isolated HOME ...")` step overrides `HOME` for that scenario
only -- no test mutates the developer's real `~/.claude/settings.json`.
"""

from __future__ import annotations

import json
import os
import subprocess
import tempfile
from collections.abc import Mapping
from pathlib import Path
from typing import Any

from behave import given, then, when
from behave.runner import Context

from repo_setup_steps import (
    commit as _commit,
    init_repo as _init_repo,
    write_file as _write_file,
)


JsonObject = dict[str, Any]


# ---------------------------------------------------------------------------
# Common helpers
# ---------------------------------------------------------------------------


def _scenario_tempdir(context: Context) -> Path:
    tmp = tempfile.mkdtemp(prefix="git-prism-bdd-")
    context.cleanup_dirs.append(tmp)
    return Path(tmp)


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
    malformed_lines: list[str] = []
    for raw_line in proc.stdout.splitlines():
        line = raw_line.strip()
        if not line:
            continue
        try:
            obj = json.loads(line)
        except json.JSONDecodeError:
            malformed_lines.append(line)
            continue
        if obj.get("id") == 2:
            return obj
    raise AssertionError(
        f"No JSON-RPC response with id=2 found.\n"
        f"stdout: {proc.stdout[:2000]}\n"
        f"stderr: {proc.stderr[:1000]}\n"
        f"malformed lines ({len(malformed_lines)}): {malformed_lines}"
    )


@given("the git-prism MCP server is running over stdio")
def step_mcp_server_running(context: Context) -> None:
    """No-op marker; the server is spawned per-request in the When steps."""
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
# W4: hooks uninstall and status (#239)
# ---------------------------------------------------------------------------


def _isolated_home(context: Context) -> Path:
    """Allocate a fresh tempdir to use as $HOME and create ~/.claude in it."""
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
    """Seed an existing entry in the user settings before uninstall runs."""
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


@given(
    'the user settings file contains a "{entry_id}" entry pointing to "{path}"'
)
def step_seed_user_settings_with_path(
    context: Context, entry_id: str, path: str
) -> None:
    """Write a single PreToolUse entry with the given id+path to user settings."""
    settings_path = context.user_settings_path
    settings_path.parent.mkdir(parents=True, exist_ok=True)
    existing = {
        "hooks": {
            "PreToolUse": [
                {"id": entry_id, "matcher": "Bash", "command": path}
            ]
        }
    }
    settings_path.write_text(json.dumps(existing, indent=2))


def _write_redirect_entry(settings_path: Path, sentinel_id: str) -> None:
    """Write a legacy redirect-hook PreToolUse sentinel directly to settings_path.

    Used by `step_redirect_hook_state` to set up the legacy state that
    `hooks status` and `hooks uninstall` operate on, without calling
    `hooks install` (which now exits non-zero after v0.9.0 removal).
    """
    settings_path.parent.mkdir(parents=True, exist_ok=True)
    existing: JsonObject = {}
    if settings_path.exists():
        try:
            existing = json.loads(settings_path.read_text())
        except json.JSONDecodeError:
            existing = {}
    hooks = existing.setdefault("hooks", {})
    pretool = hooks.setdefault("PreToolUse", [])
    # Only add if not already present (idempotent setup).
    ids = [e.get("id") for e in pretool]
    if sentinel_id not in ids:
        pretool.append({
            "id": sentinel_id,
            "matcher": "Bash",
            "command": "/old/path/git-prism-redirect.sh",
        })
    settings_path.write_text(json.dumps(existing, indent=2))


@given('the redirect hook install state is "{state}"')
def step_redirect_hook_state(context: Context, state: str) -> None:
    """Set up legacy redirect-hook state for `hooks status` triangulation.

    Writes the sentinel entry directly into the appropriate settings files
    rather than calling `hooks install` (removed in v0.9.0).

    States:
      none            -- leave both settings files absent
      user-only       -- write sentinel to user settings only
      project-only    -- write sentinel to project settings only
      user-and-project -- write sentinel to both
    """
    sentinel = "git-prism-bash-redirect-v1"
    if state == "none":
        return
    if state in ("user-only", "user-and-project"):
        _write_redirect_entry(context.user_settings_path, sentinel)
    if state in ("project-only", "user-and-project"):
        _write_redirect_entry(context.project_settings_path, sentinel)
    if state not in ("none", "user-only", "project-only", "user-and-project"):
        raise AssertionError(f"Unknown install state: {state!r}")


@when("I uninstall the redirect hook at user scope")
def step_uninstall_user_scope(context: Context) -> None:
    _run_git_prism(context, ["hooks", "uninstall", "--scope", "user"])


@when('I run "hooks status" in the repo')
def step_run_hooks_status(context: Context) -> None:
    _run_git_prism(
        context, ["hooks", "status"], cwd=context.project_repo_path
    )


@then("the hook exit code is {code:d}")
def step_hook_exit_code(context: Context, code: int) -> None:
    actual = context.result.returncode
    assert actual == code, (
        f"Expected hook exit code {code}, got {actual}.\n"
        f"stdout: {context.result.stdout!r}\n"
        f"stderr: {context.result.stderr!r}"
    )


@then(
    'the user settings file contains a PreToolUse entry with id "{entry_id}"'
)
def step_user_settings_has_entry(context: Context, entry_id: str) -> None:
    settings_path = context.user_settings_path
    assert settings_path.is_file(), (
        f"Expected user settings file at {settings_path}"
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


@then('the hook stdout contains "{phrase}"')
def step_hook_stdout_contains(context: Context, phrase: str) -> None:
    out = context.result.stdout
    assert phrase in out, (
        f"Expected hook stdout to contain {phrase!r}.\nstdout: {out!r}"
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
# W5: review_change MCP tool scenarios (#240)
# ---------------------------------------------------------------------------


@given("a git repository with many changed files")
def step_repo_with_many_changes(context: Context) -> None:
    """Create a repo with 12 changed files so a page_size of 5 forces
    pagination on at least one sub-response."""
    repo_dir = _init_repo(context)
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
    cursors: list[tuple[str, str | None]] = []
    for sub_key in ("manifest", "function_context"):
        sub = payload.get(sub_key, {})
        cursor = sub.get("pagination", {}).get("next_cursor")
        cursors.append((sub_key, cursor))
    paginated = [k for k, c in cursors if c]
    assert paginated, (
        f"No sub-response paginated. cursors={cursors}\n"
        f"Expected at least one non-null next_cursor when page_size is small."
    )


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

    args: JsonObject = {
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
        f"Cursor walk returned the same files as page 1 -- pagination is "
        f"hardcoded.\npage1: {sorted(page1_files)}\npage2: {sorted(page2_files)}"
    )
    assert page2_files, (
        f"Cursor walk returned an empty manifest page; expected the next "
        f"slice of files.\npage2: {page2}"
    )
