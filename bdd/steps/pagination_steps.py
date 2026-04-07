"""Step definitions for pagination feature.

These scenarios test cursor-based pagination of manifest and history
responses via the MCP JSON-RPC interface. All steps are black-box —
they interact with git-prism as an agent would, sending tool calls
over stdio and inspecting the JSON responses.

The production pagination module does not exist yet — these scenarios
are tagged @not_implemented and must FAIL with assertion errors until
the pagination implementation lands.
"""

from __future__ import annotations

import json
import subprocess
from pathlib import Path

from behave import given, then, when
from behave.runner import Context

from repo_setup_steps import _commit, _init_repo, _write_file


def _send_mcp_tool_call(
    context: Context,
    tool_name: str,
    arguments: dict,
    repo_path: str | None = None,
) -> dict:
    """Send a JSON-RPC tool call to git-prism serve and return the parsed response."""
    if repo_path:
        arguments["repo_path"] = repo_path

    request = {
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": "2024-11-05",
            "capabilities": {},
            "clientInfo": {"name": "bdd-pagination-tests", "version": "0.0.0"},
        },
    }
    tool_call = {
        "jsonrpc": "2.0",
        "id": 2,
        "method": "tools/call",
        "params": {"name": tool_name, "arguments": arguments},
    }

    input_data = json.dumps(request) + "\n" + json.dumps(tool_call) + "\n"

    proc = subprocess.run(
        [context.binary_path, "serve"],
        input=input_data,
        capture_output=True,
        text=True,
        timeout=30,
        cwd=repo_path,
    )

    # Parse the tool call response (second JSON-RPC response line)
    lines = [l for l in proc.stdout.strip().split("\n") if l.strip()]
    if len(lines) < 2:
        return {"error": f"Expected 2 response lines, got {len(lines)}: {proc.stdout}"}

    response = json.loads(lines[1])
    if "result" in response:
        content = response["result"].get("content", [])
        for item in content:
            if item.get("type") == "text":
                return json.loads(item["text"])
    if "error" in response:
        return {"error": response["error"]}
    return response


# ---------- Given steps ----------


def _disable_gpgsign(repo_dir: str) -> None:
    """Disable commit signing in a repo (needed in some CI environments)."""
    subprocess.run(
        ["git", "config", "commit.gpgsign", "false"],
        cwd=repo_dir, check=True, capture_output=True,
    )


@given("a git repository with {count:d} changed files")
def step_create_repo_with_n_files(context: Context, count: int) -> None:
    repo_dir = _init_repo(context)
    _disable_gpgsign(repo_dir)
    context.repo_path = repo_dir

    # Initial commit with a seed file
    _write_file(repo_dir, "seed.txt", "seed\n")
    _commit(repo_dir, "initial", ["seed.txt"])

    # Second commit with N changed files
    for i in range(count):
        _write_file(repo_dir, f"file_{i:04d}.txt", f"content {i}\n")
    subprocess.run(
        ["git", "add", "."], cwd=repo_dir, check=True, capture_output=True
    )
    subprocess.run(
        ["git", "commit", "-m", f"add {count} files"],
        cwd=repo_dir, check=True, capture_output=True,
    )


@given("a git repository with {count:d} sequential commits")
def step_create_repo_with_n_commits(context: Context, count: int) -> None:
    repo_dir = _init_repo(context)
    _disable_gpgsign(repo_dir)
    context.repo_path = repo_dir

    # Anchor commit
    _write_file(repo_dir, "anchor.txt", "anchor\n")
    _commit(repo_dir, "anchor", ["anchor.txt"])

    # N sequential commits
    for i in range(count):
        _write_file(repo_dir, f"file_{i:04d}.txt", f"commit {i}\n")
        _commit(repo_dir, f"commit {i}", [f"file_{i:04d}.txt"])


@given(
    "the agent has received the first page of a manifest with page size {size:d}"
)
def step_get_first_page(context: Context, size: int) -> None:
    result = _send_mcp_tool_call(
        context,
        "get_change_manifest",
        {"base_ref": "HEAD~1", "head_ref": "HEAD", "page_size": size},
        repo_path=context.repo_path,
    )
    context.first_page = result
    context.first_page_files = [f["path"] for f in result.get("files", [])]
    context.cursor = result.get("pagination", {}).get("next_cursor")
    assert context.cursor is not None, (
        f"Expected a pagination cursor on the first page but got none. "
        f"Response keys: {list(result.keys())}. "
        f"This fails because pagination is not yet implemented."
    )


@given("a git repository with {count:d} unstaged changed files")
def step_create_repo_with_unstaged_files(context: Context, count: int) -> None:
    repo_dir = _init_repo(context)
    _disable_gpgsign(repo_dir)
    context.repo_path = repo_dir

    # Initial commit with a seed file
    _write_file(repo_dir, "seed.txt", "seed\n")
    _commit(repo_dir, "initial", ["seed.txt"])

    # Create N unstaged files (not committed)
    for i in range(count):
        _write_file(repo_dir, f"file_{i:04d}.txt", f"content {i}\n")


@given("a new commit is added to the repository")
def step_add_commit(context: Context) -> None:
    _write_file(context.repo_path, "new_after_cursor.txt", "new content\n")
    _commit(context.repo_path, "post-cursor commit", ["new_after_cursor.txt"])


# ---------- When steps ----------


@when("the agent requests a change manifest with page size {size:d}")
def step_request_manifest_with_page_size(context: Context, size: int) -> None:
    context.result = _send_mcp_tool_call(
        context,
        "get_change_manifest",
        {"base_ref": "HEAD~1", "head_ref": "HEAD", "page_size": size},
        repo_path=context.repo_path,
    )


@when("the agent requests a working tree manifest with page size {size:d}")
def step_request_worktree_manifest(context: Context, size: int) -> None:
    context.result = _send_mcp_tool_call(
        context,
        "get_change_manifest",
        {"base_ref": "HEAD", "page_size": size},
        repo_path=context.repo_path,
    )


@when("the agent requests the next page using the cursor")
def step_request_next_page(context: Context) -> None:
    context.result = _send_mcp_tool_call(
        context,
        "get_change_manifest",
        {
            "base_ref": "HEAD~1",
            "head_ref": "HEAD",
            "cursor": context.cursor,
        },
        repo_path=context.repo_path,
    )


@when("the agent pages through the entire manifest with page size {size:d}")
def step_page_through_all(context: Context, size: int) -> None:
    all_files: list[str] = []
    pages: list[dict] = []
    cursor = None

    for page_num in range(100):  # safety limit
        args = {"base_ref": "HEAD~1", "head_ref": "HEAD", "page_size": size}
        if cursor:
            args["cursor"] = cursor

        result = _send_mcp_tool_call(
            context,
            "get_change_manifest",
            args,
            repo_path=context.repo_path,
        )
        pages.append(result)

        page_files = [f["path"] for f in result.get("files", [])]
        all_files.extend(page_files)

        pagination = result.get("pagination", {})
        cursor = pagination.get("next_cursor")
        if cursor is None:
            break

    context.all_collected_files = all_files
    context.all_pages = pages
    context.result = pages[-1] if pages else {}


@when("the agent requests a manifest with an invalid cursor")
def step_request_with_invalid_cursor(context: Context) -> None:
    context.result = _send_mcp_tool_call(
        context,
        "get_change_manifest",
        {
            "base_ref": "HEAD~1",
            "head_ref": "HEAD",
            "cursor": "this-is-not-a-valid-cursor!!!",
        },
        repo_path=context.repo_path,
    )


@when("the agent requests a change manifest without pagination parameters")
def step_request_without_pagination(context: Context) -> None:
    context.result = _send_mcp_tool_call(
        context,
        "get_change_manifest",
        {"base_ref": "HEAD~1", "head_ref": "HEAD"},
        repo_path=context.repo_path,
    )


@when("the agent requests commit history with page size {size:d}")
def step_request_history_with_page_size(context: Context, size: int) -> None:
    context.result = _send_mcp_tool_call(
        context,
        "get_commit_history",
        {
            "base_ref": "HEAD~20",
            "head_ref": "HEAD",
            "page_size": size,
        },
        repo_path=context.repo_path,
    )


@when("the user runs the manifest CLI command")
def step_run_cli_manifest(context: Context) -> None:
    proc = subprocess.run(
        [context.binary_path, "manifest", "HEAD~1..HEAD", "--repo", context.repo_path],
        capture_output=True,
        text=True,
        timeout=30,
    )
    assert proc.returncode == 0, (
        f"CLI exited with code {proc.returncode}: {proc.stderr}"
    )
    context.cli_result = json.loads(proc.stdout)


# ---------- Then steps ----------


@then("the response contains exactly {count:d} files")
def step_response_has_exact_files(context: Context, count: int) -> None:
    files = context.result.get("files", [])
    assert len(files) == count, (
        f"Expected {count} files, got {len(files)}. "
        f"Response keys: {list(context.result.keys())}. "
        f"This likely fails because pagination is not yet implemented — "
        f"the response returns all files in one page."
    )


@then("the response contains at most {count:d} files")
def step_response_has_at_most_files(context: Context, count: int) -> None:
    files = context.result.get("files", [])
    assert len(files) <= count, (
        f"Expected at most {count} files, got {len(files)}."
    )


@then("the response includes a pagination cursor")
def step_response_has_cursor(context: Context) -> None:
    pagination = context.result.get("pagination", {})
    cursor = pagination.get("next_cursor")
    assert cursor is not None, (
        f"Expected a 'pagination.next_cursor' in the response. "
        f"Response keys: {list(context.result.keys())}. "
        f"Pagination key present: {'pagination' in context.result}. "
        f"This fails because pagination is not yet implemented."
    )


@then("the response has no pagination cursor")
def step_response_has_no_cursor(context: Context) -> None:
    pagination = context.result.get("pagination", {})
    cursor = pagination.get("next_cursor")
    assert cursor is None, (
        f"Expected no pagination cursor (last page), but got: {cursor}"
    )


@then("the pagination shows {count:d} total files")
def step_pagination_shows_total(context: Context, count: int) -> None:
    pagination = context.result.get("pagination", {})
    total = pagination.get("total_files")
    assert total == count, (
        f"Expected pagination.total_files={count}, got {total}. "
        f"Pagination object: {pagination}"
    )


@then("none of the files overlap with the first page")
def step_no_overlap(context: Context) -> None:
    first_paths = set(context.first_page_files)
    current_paths = {f["path"] for f in context.result.get("files", [])}
    overlap = first_paths & current_paths
    assert not overlap, (
        f"Found {len(overlap)} overlapping files between pages: {overlap}"
    )


@then("all {count:d} changed files are collected across {pages:d} pages")
def step_all_files_collected(context: Context, count: int, pages: int) -> None:
    assert len(context.all_collected_files) == count, (
        f"Expected {count} total files across all pages, "
        f"got {len(context.all_collected_files)}."
    )
    assert len(context.all_pages) == pages, (
        f"Expected {pages} pages, got {len(context.all_pages)}."
    )


@then("no files are duplicated")
def step_no_duplicates(context: Context) -> None:
    seen = set()
    dupes = []
    for f in context.all_collected_files:
        if f in seen:
            dupes.append(f)
        seen.add(f)
    assert not dupes, f"Found duplicate files across pages: {dupes}"


@then("the summary reports {count:d} total files changed")
def step_summary_total(context: Context, count: int) -> None:
    summary = context.result.get("summary", {})
    total = summary.get("total_files_changed")
    assert total == count, (
        f"Expected summary.total_files_changed={count}, got {total}."
    )


@then("the summary is identical on every page")
def step_summary_identical_across_pages(context: Context) -> None:
    # Uses context.all_pages populated by "pages through the entire manifest"
    pages = getattr(context, "all_pages", [])
    assert len(pages) >= 2, (
        f"Expected multiple pages to compare summaries, got {len(pages)}. "
        f"This fails because pagination is not yet implemented."
    )
    summaries = [p.get("summary") for p in pages]
    for i, s in enumerate(summaries[1:], start=2):
        assert s == summaries[0], (
            f"Summary on page {i} differs from page 1: {s} != {summaries[0]}"
        )


@then("the response is an error")
def step_response_is_error(context: Context) -> None:
    assert "error" in context.result, (
        f"Expected an error response, got: {list(context.result.keys())}"
    )


@then("the error message mentions the cursor")
def step_error_mentions_cursor(context: Context) -> None:
    error = context.result.get("error", {})
    msg = str(error).lower()
    assert "cursor" in msg, (
        f"Expected error to mention 'cursor', got: {error}"
    )


@then("the error message indicates the repository has changed")
def step_error_repo_changed(context: Context) -> None:
    error = context.result.get("error", {})
    msg = str(error).lower()
    assert any(word in msg for word in ("changed", "stale", "mismatch", "inconsistent")), (
        f"Expected error about repo change, got: {error}"
    )


@then("the last page has no pagination cursor")
def step_last_page_no_cursor(context: Context) -> None:
    last_page = context.all_pages[-1]
    pagination = last_page.get("pagination", {})
    cursor = pagination.get("next_cursor")
    assert cursor is None, (
        f"Expected no cursor on last page, got: {cursor}"
    )


@then("the response contains exactly {count:d} commits")
def step_response_has_exact_commits(context: Context, count: int) -> None:
    commits = context.result.get("commits", [])
    assert len(commits) == count, (
        f"Expected {count} commits, got {len(commits)}. "
        f"This likely fails because history pagination is not yet implemented."
    )


@then("the output contains all {count:d} files in a single JSON response")
def step_cli_all_files(context: Context, count: int) -> None:
    files = context.cli_result.get("files", [])
    assert len(files) == count, (
        f"Expected CLI to output all {count} files, got {len(files)}."
    )
