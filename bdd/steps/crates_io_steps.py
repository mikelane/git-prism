"""Step definitions for crates.io package verification."""

from __future__ import annotations

import json
import urllib.error
import urllib.request
from typing import Any

from behave import then, when
from behave.runner import Context


@when('I query crates.io for package "{name}"')
def step_query_crates_io(context: Context, name: str) -> None:
    """Query the crates.io API for a package by name.

    Stores the response, HTTP status, and any error on the context
    so that assertion steps can produce specific diagnostic messages.

    Args:
        context: The behave context, stores response data and HTTP status.
        name: The crate name to look up on crates.io.
    """
    url = f"https://crates.io/api/v1/crates/{name}"
    request = urllib.request.Request(url, headers={"User-Agent": "git-prism-bdd-test"})
    context.crates_io_package_name = name
    try:
        with urllib.request.urlopen(request, timeout=10) as resp:
            body: dict[str, Any] = json.loads(resp.read())
            context.crates_io_response = body
            context.crates_io_status = resp.status
            context.crates_io_error = None
    except urllib.error.HTTPError as e:
        context.crates_io_response = None
        context.crates_io_status = e.code
        context.crates_io_error = f"HTTP {e.code}: {e.reason}"
    except urllib.error.URLError as e:
        context.crates_io_response = None
        context.crates_io_status = None
        context.crates_io_error = f"Network error querying crates.io: {e.reason}"


@then("the package exists on crates.io")
def step_package_exists_on_crates_io(context: Context) -> None:
    """Assert that the queried package was found on crates.io (HTTP 200)."""
    name = getattr(context, "crates_io_package_name", "<unknown>")

    assert context.crates_io_error is None, (
        f"Failed to query crates.io for package '{name}': "
        f"{context.crates_io_error}"
    )
    assert context.crates_io_status == 200, (
        f"Package '{name}' not found on crates.io "
        f"(HTTP {context.crates_io_status})"
    )
    assert context.crates_io_response is not None, (
        f"No response body from crates.io for package '{name}'"
    )
    assert "crate" in context.crates_io_response, (
        f"Response for '{name}' missing 'crate' key. "
        f"Keys present: {list(context.crates_io_response.keys())}"
    )
