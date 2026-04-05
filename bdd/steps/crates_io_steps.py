"""Step definitions for crates.io package verification."""

import json
import urllib.request

from behave import then, when


@when('I query crates.io for package "{name}"')
def step_query_crates_io(context, name):
    url = f"https://crates.io/api/v1/crates/{name}"
    req = urllib.request.Request(url, headers={"User-Agent": "git-prism-bdd-test"})
    try:
        with urllib.request.urlopen(req, timeout=10) as resp:
            context.crates_io_response = json.loads(resp.read())
            context.crates_io_status = resp.status
    except urllib.error.HTTPError as e:
        context.crates_io_response = None
        context.crates_io_status = e.code


@then("the package exists on crates.io")
def step_package_exists_on_crates_io(context):
    assert context.crates_io_status == 200, (
        f"Package not found on crates.io (HTTP {context.crates_io_status})"
    )
    assert context.crates_io_response is not None, "No response from crates.io"
    assert "crate" in context.crates_io_response, (
        f"Unexpected response: {context.crates_io_response}"
    )
