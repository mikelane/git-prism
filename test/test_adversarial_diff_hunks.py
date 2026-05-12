#!/usr/bin/env python3
"""Adversarial tests for diff_hunks feature in get_file_snapshots.

Tests the `include_diff_hunks` parameter of the MCP `get_file_snapshots` tool.
Runs the MCP server via subprocess and communicates over JSON-RPC over stdio.
"""
import json
import os
import subprocess
import sys
import tempfile
import time
import threading
import queue


def run_git(repo_path, *args):
    """Run a git command in the given repo."""
    result = subprocess.run(
        ["git"] + list(args),
        cwd=repo_path,
        capture_output=True,
        text=True,
    )
    if result.returncode != 0:
        raise RuntimeError(f"git {' '.join(args)} failed: {result.stderr}")
    return result.stdout


def create_test_repo():
    """Create a temp git repo with an initial commit and a second modifying commit."""
    tmp = tempfile.mkdtemp(prefix="git-prism-adversarial-")
    run_git(tmp, "init", "--initial-branch=main")
    run_git(tmp, "config", "user.email", "test@test.com")
    run_git(tmp, "config", "user.name", "Test")

    # Initial commit: a text file with 3 lines
    with open(os.path.join(tmp, "hello.txt"), "w") as f:
        f.write("line1\nline2\nline3\n")
    run_git(tmp, "add", "hello.txt")
    run_git(tmp, "commit", "-m", "initial commit")

    # Second commit: modify line 2
    with open(os.path.join(tmp, "hello.txt"), "w") as f:
        f.write("line1\nmodified\nline3\n")
    run_git(tmp, "add", "hello.txt")
    run_git(tmp, "commit", "-m", "modify line 2")

    return tmp


def create_emptied_file_repo():
    """Create a temp repo where a file is emptied (all lines deleted)."""
    tmp = tempfile.mkdtemp(prefix="git-prism-emptied-")
    run_git(tmp, "init", "--initial-branch=main")
    run_git(tmp, "config", "user.email", "test@test.com")
    run_git(tmp, "config", "user.name", "Test")

    # Initial commit: a file with content
    with open(os.path.join(tmp, "data.txt"), "w") as f:
        f.write("line1\nline2\nline3\n")
    run_git(tmp, "add", "data.txt")
    run_git(tmp, "commit", "-m", "add data file")

    # Second commit: empty the file entirely
    with open(os.path.join(tmp, "data.txt"), "w") as f:
        f.write("")
    run_git(tmp, "add", "data.txt")
    run_git(tmp, "commit", "-m", "empty data file")

    return tmp


def create_populated_empty_file_repo():
    """Create a temp repo where an empty file gains content."""
    tmp = tempfile.mkdtemp(prefix="git-prism-populated-")
    run_git(tmp, "init", "--initial-branch=main")
    run_git(tmp, "config", "user.email", "test@test.com")
    run_git(tmp, "config", "user.name", "Test")

    # Initial commit: a truly empty file
    with open(os.path.join(tmp, "empty.txt"), "w") as f:
        f.write("")
    run_git(tmp, "add", "empty.txt")
    run_git(tmp, "commit", "-m", "add empty file")

    # Second commit: populate with content
    with open(os.path.join(tmp, "empty.txt"), "w") as f:
        f.write("line1\nline2\n")
    run_git(tmp, "add", "empty.txt")
    run_git(tmp, "commit", "-m", "populate empty file")

    return tmp


def create_binary_file_repo():
    """Create a temp repo where a text file becomes binary (non-UTF-8 bytes)."""
    tmp = tempfile.mkdtemp(prefix="git-prism-binary-")
    run_git(tmp, "init", "--initial-branch=main")
    run_git(tmp, "config", "user.email", "test@test.com")
    run_git(tmp, "config", "user.name", "Test")

    # Initial commit: a text file
    with open(os.path.join(tmp, "image.bin"), "w") as f:
        f.write("placeholder\n")
    run_git(tmp, "add", "image.bin")
    run_git(tmp, "commit", "-m", "add placeholder")

    # Second commit: binary content (PNG header bytes - non-UTF8)
    with open(os.path.join(tmp, "image.bin"), "wb") as f:
        f.write(bytes([0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A]))
    run_git(tmp, "add", "image.bin")
    run_git(tmp, "commit", "-m", "make binary")

    return tmp


def create_trailing_newline_repo():
    """Create a temp repo where trailing newline is added."""
    tmp = tempfile.mkdtemp(prefix="git-prism-newline-")
    run_git(tmp, "init", "--initial-branch=main")
    run_git(tmp, "config", "user.email", "test@test.com")
    run_git(tmp, "config", "user.name", "Test")

    # Initial commit: no trailing newline
    with open(os.path.join(tmp, "text.txt"), "wb") as f:
        f.write(b"hello")
    run_git(tmp, "add", "text.txt")
    run_git(tmp, "commit", "-m", "add without trailing newline")

    # Second commit: add trailing newline
    with open(os.path.join(tmp, "text.txt"), "w") as f:
        f.write("hello\n")
    run_git(tmp, "add", "text.txt")
    run_git(tmp, "commit", "-m", "add trailing newline")

    return tmp


class McpServer:
    """Manages the git-prism MCP server subprocess."""

    def __init__(self, repo_path):
        exe = os.path.join(
            os.path.dirname(os.path.dirname(os.path.abspath(__file__))),
            "target", "debug", "git-prism",
        )
        # Try release build if debug doesn't exist
        if not os.path.exists(exe):
            exe = os.path.join(
                os.path.dirname(os.path.dirname(os.path.abspath(__file__))),
                "target", "release", "git-prism",
            )

        self.proc = subprocess.Popen(
            [exe, "serve"],
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            cwd=repo_path,
        )
        self._id = 0
        self._lock = threading.Lock()

        # Initialize the MCP session
        self._send_json({
            "jsonrpc": "2.0",
            "id": self._next_id(),
            "method": "initialize",
            "params": {
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": {"name": "adversarial-qa", "version": "0.1.0"},
            },
        })
        init_resp = self._recv_json()
        if "error" in init_resp:
            raise RuntimeError(f"MCP initialize failed: {init_resp['error']}")

        # Send initialized notification
        self._send_json({
            "jsonrpc": "2.0",
            "method": "notifications/initialized",
        })

    def _next_id(self):
        with self._lock:
            self._id += 1
            return self._id

    def _send_json(self, data):
        msg = json.dumps(data) + "\n"
        self.proc.stdin.write(msg.encode())
        self.proc.stdin.flush()

    def _recv_json(self):
        line = self.proc.stdout.readline()
        if not line:
            raise RuntimeError("MCP server closed stdout")
        return json.loads(line.decode())

    def call_tool(self, tool_name, arguments):
        """Call an MCP tool and return the result content."""
        self._send_json({
            "jsonrpc": "2.0",
            "id": self._next_id(),
            "method": "tools/call",
            "params": {
                "name": tool_name,
                "arguments": arguments,
            },
        })
        resp = self._recv_json()
        if "error" in resp:
            raise RuntimeError(f"MCP tool call failed: {resp['error']}")
        return resp["result"]["content"][0]["text"]

    def close(self):
        self.proc.stdin.close()
        self.proc.stdout.close()
        self.proc.stderr.close()
        self.proc.wait(timeout=5)


def test_diff_hunks_for_modified_file():
    """Verify diff_hunks is present for a modified file when requested."""
    print("TEST: it_returns_diff_hunks_for_modified_file_when_requested")
    repo = create_test_repo()
    server = McpServer(repo)

    result = json.loads(server.call_tool("get_file_snapshots", {
        "base_ref": "HEAD~1",
        "head_ref": "HEAD",
        "paths": ["hello.txt"],
        "include_diff_hunks": True,
    }))

    file_entry = result["files"][0]
    assert file_entry["diff_hunks"] is not None, (
        f"diff_hunks should not be None for modified file. Got: {file_entry['diff_hunks']}"
    )
    hunks = file_entry["diff_hunks"]
    assert len(hunks) > 0, f"Should have at least one hunk. Got {len(hunks)}"
    h = hunks[0]
    assert h["old_start"] == 2, f"old_start should be 2, got {h['old_start']}"
    assert h["old_lines"] == 1, f"old_lines should be 1, got {h['old_lines']}"
    assert h["new_start"] == 2, f"new_start should be 2, got {h['new_start']}"
    assert h["new_lines"] == 1, f"new_lines should be 1, got {h['new_lines']}"

    server.close()
    print("  PASS")


def test_diff_hunks_for_emptied_file():
    """BUG: Verify diff_hunks is present when a file is emptied of all content.

    When a file has all content removed (non-empty -> 0 bytes), git's unified
    diff still produces a hunk like `@@ -1,N +0,0 @@`. The current
    implementation may return None instead, which prevents agents from
    computing diff-relative positions for such files.
    """
    print("TEST: it_returns_diff_hunks_for_emptied_file")
    repo = create_emptied_file_repo()
    server = McpServer(repo)

    result = json.loads(server.call_tool("get_file_snapshots", {
        "base_ref": "HEAD~1",
        "head_ref": "HEAD",
        "paths": ["data.txt"],
        "include_diff_hunks": True,
    }))

    file_entry = result["files"][0]
    # The file was modified (content removed) - both before and after exist
    assert file_entry["before"] is not None, "before should exist"
    # After might be None if read_file_at_ref returns Err for empty...
    # OR after exists with empty content
    print(f"  before: {json.dumps(file_entry['before'])}")
    print(f"  after: {json.dumps(file_entry['after'])}")
    print(f"  diff_hunks: {file_entry['diff_hunks']}")
    print(f"  is_binary: {file_entry['is_binary']}")

    if file_entry["after"] is not None:
        # Both before and after exist - should have hunks for the deletion
        assert file_entry["diff_hunks"] is not None, (
            "BUG: diff_hunks is None for emptied file. Expected at least one deletion hunk."
        )
        print("  diff_hunks present (correct)")
    else:
        print("  after is None (empty file not read by read_file_at_ref)")

    server.close()
    print("  DONE")


def test_diff_hunks_for_populated_file():
    """BUG: Verify diff_hunks when empty file gains content.

    When a 0-byte file gains content, unified diff produces
    `@@ -0,0 +1,N @@`. The current implementation may return None.
    """
    print("TEST: it_returns_diff_hunks_for_populated_empty_file")
    repo = create_populated_empty_file_repo()
    server = McpServer(repo)

    result = json.loads(server.call_tool("get_file_snapshots", {
        "base_ref": "HEAD~1",
        "head_ref": "HEAD",
        "paths": ["empty.txt"],
        "include_diff_hunks": True,
    }))

    file_entry = result["files"][0]
    print(f"  before: {json.dumps(file_entry['before'])}")
    print(f"  after: {json.dumps(file_entry['after'])}")
    print(f"  diff_hunks: {file_entry['diff_hunks']}")
    print(f"  is_binary: {file_entry['is_binary']}")

    if file_entry["before"] is not None and file_entry["after"] is not None:
        assert file_entry["diff_hunks"] is not None, (
            "BUG: diff_hunks is None for file populated from empty. "
            "Expected at least one addition hunk."
        )
        print("  diff_hunks present (correct)")
    else:
        print("  One side is None")

    server.close()
    print("  DONE")


def test_diff_hunks_default_is_false():
    """Verify diff_hunks is None when include_diff_hunks is not set (default false)."""
    print("TEST: it_omits_diff_hunks_by_default")
    repo = create_test_repo()
    server = McpServer(repo)

    result = json.loads(server.call_tool("get_file_snapshots", {
        "base_ref": "HEAD~1",
        "head_ref": "HEAD",
        "paths": ["hello.txt"],
    }))

    file_entry = result["files"][0]
    assert file_entry.get("diff_hunks") is None, (
        f"diff_hunks should be None when include_diff_hunks is not passed. Got: {file_entry.get('diff_hunks')}"
    )

    server.close()
    print("  PASS")


def test_diff_hunks_for_added_file():
    """Verify diff_hunks is None for added files."""
    print("TEST: it_omits_diff_hunks_for_added_file")
    repo = create_test_repo()

    # Add a new file at HEAD
    with open(os.path.join(repo, "new.txt"), "w") as f:
        f.write("brand new\n")
    run_git(repo, "add", "new.txt")
    run_git(repo, "commit", "-m", "add new file")

    server = McpServer(repo)

    result = json.loads(server.call_tool("get_file_snapshots", {
        "base_ref": "HEAD~1",
        "head_ref": "HEAD",
        "paths": ["new.txt"],
        "include_diff_hunks": True,
    }))

    file_entry = result["files"][0]
    assert file_entry["before"] is None, "added file should have no before"
    assert file_entry.get("diff_hunks") is None, (
        f"diff_hunks should be None for added files. Got: {file_entry.get('diff_hunks')}"
    )

    server.close()
    print("  PASS")


def test_diff_hunks_for_binary_file():
    """Check behavior when a file becomes binary (non-UTF-8).

    read_file_at_ref uses std::str::from_utf8 (not from_utf8_lossy),
    so non-UTF-8 blob data causes an error. This should result in
    before being None and diff_hunks being None.
    """
    print("TEST: it_handles_non_utf8_binary_file")
    repo = create_binary_file_repo()
    server = McpServer(repo)

    result = json.loads(server.call_tool("get_file_snapshots", {
        "base_ref": "HEAD~1",
        "head_ref": "HEAD",
        "paths": ["image.bin"],
        "include_diff_hunks": True,
    }))

    file_entry = result["files"][0]
    print(f"  is_binary: {file_entry['is_binary']}")
    print(f"  before: {json.dumps(file_entry['before'])}")
    print(f"  after: {json.dumps(file_entry['after'])}")
    print(f"  diff_hunks: {file_entry.get('diff_hunks')}")

    # The file IS binary (non-UTF-8 content at HEAD)
    # is_binary should be true
    if not file_entry["is_binary"]:
        print("  BUG: is_binary is false for non-UTF-8 binary file!")
        print("  This means the is_binary flag is unreliable for files with")
        print("  non-UTF-8 bytes (as opposed to files with null bytes).")

    server.close()
    print("  DONE")


def test_diff_hunks_for_trailing_newline():
    """Check hunk behavior when trailing newline is added."""
    print("TEST: it_handles_trailing_newline_addition")
    repo = create_trailing_newline_repo()
    server = McpServer(repo)

    result = json.loads(server.call_tool("get_file_snapshots", {
        "base_ref": "HEAD~1",
        "head_ref": "HEAD",
        "paths": ["text.txt"],
        "include_diff_hunks": True,
    }))

    file_entry = result["files"][0]
    print(f"  is_binary: {file_entry['is_binary']}")
    print(f"  before: {json.dumps(file_entry['before'])}")
    print(f"  after: {json.dumps(file_entry['after'])}")
    print(f"  diff_hunks: {file_entry['diff_hunks']}")

    hunks = file_entry.get("diff_hunks")
    if hunks:
        for i, h in enumerate(hunks):
            print(f"  hunk[{i}]: old_start={h['old_start']}, old_lines={h['old_lines']}, "
                  f"new_start={h['new_start']}, new_lines={h['new_lines']}")

    server.close()
    print("  DONE")


def test_cli_snapshot_has_include_diff_hunks_flag():
    """Verify that the CLI snapshot command supports --include-diff-hunks."""
    print("TEST: it_supports_include_diff_hunks_cli_flag")
    repo = create_test_repo()

    exe = os.path.join(
        os.path.dirname(os.path.dirname(os.path.abspath(__file__))),
        "target", "debug", "git-prism",
    )
    if not os.path.exists(exe):
        exe = os.path.join(
            os.path.dirname(os.path.dirname(os.path.abspath(__file__))),
            "target", "release", "git-prism",
        )

    # Run the CLI snapshot command with --include-diff-hunks
    result = subprocess.run(
        [exe, "snapshot", "HEAD~1..HEAD", "--paths", "hello.txt", "--repo", repo, "--include-diff-hunks"],
        capture_output=True, text=True,
    )

    if result.returncode != 0:
        raise RuntimeError(f"CLI error: {result.stderr}")

    data = json.loads(result.stdout)
    file_entry = data["files"][0]
    assert file_entry["diff_hunks"] is not None, (
        "CLI snapshot with --include-diff-hunks should include diff_hunks"
    )
    assert len(file_entry["diff_hunks"]) > 0, (
        "CLI snapshot with --include-diff-hunks should have at least one hunk"
    )
    print("  PASS")


def main():
    print("=" * 60)
    print("Adversarial QA: diff_hunks feature tests")
    print("=" * 60)

    failures = []

    # Ensure the binary is built
    worktree = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
    print(f"Building git-prism in {worktree}...")
    build_result = subprocess.run(
        ["cargo", "build", "--bin", "git-prism"],
        cwd=worktree,
        capture_output=True, text=True,
    )
    if build_result.returncode != 0:
        print(f"Build failed: {build_result.stderr}")
        sys.exit(1)
    print("Build succeeded.\n")

    tests = [
        test_diff_hunks_default_is_false,
        test_diff_hunks_for_modified_file,
        test_diff_hunks_for_added_file,
        test_diff_hunks_for_binary_file,
        test_diff_hunks_for_trailing_newline,
        test_diff_hunks_for_emptied_file,
        test_diff_hunks_for_populated_file,
        test_cli_snapshot_has_include_diff_hunks_flag,
    ]

    for test_fn in tests:
        try:
            test_fn()
        except Exception as e:
            print(f"  FAIL: {e}")
            failures.append((test_fn.__name__, str(e)))
        print()

    print("=" * 60)
    if failures:
        print(f"FAILURES: {len(failures)}")
        for name, err in failures:
            print(f"  - {name}: {err}")
        sys.exit(1)
    else:
        print("ALL TESTS PASSED")
        sys.exit(0)


if __name__ == "__main__":
    main()
