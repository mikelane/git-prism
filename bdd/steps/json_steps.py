"""Step definitions for JSON validation and inspection."""

import json

from behave import then


@then("the output is valid JSON")
def step_valid_json(context):
    try:
        context.json_data = json.loads(context.result.stdout)
    except json.JSONDecodeError as e:
        raise AssertionError(
            f"Output is not valid JSON: {e}\n"
            f"stdout: {context.result.stdout[:500]}"
        ) from e


@then('the JSON has key "{key}"')
def step_json_has_key(context, key):
    data = _ensure_json_parsed(context)
    _navigate_dotted_path(data, key)


@then('the JSON value "{path}" is greater than {value:d}')
def step_json_value_gt(context, path, value):
    data = _ensure_json_parsed(context)
    actual = _navigate_dotted_path(data, path)
    assert actual > value, (
        f"Expected JSON path '{path}' > {value}, got {actual}"
    )


@then('at least one file has "{key}" that is not null')
def step_at_least_one_file_has_non_null(context, key):
    data = _ensure_json_parsed(context)
    files = data.get("files", [])
    assert files, "No 'files' array in JSON output"
    found = any(f.get(key) is not None for f in files)
    assert found, (
        f"No file has non-null '{key}'. "
        f"Values: {[f.get(key) for f in files]}"
    )


def _ensure_json_parsed(context):
    """Return parsed JSON, parsing from stdout if not already cached."""
    if not hasattr(context, "json_data") or context.json_data is None:
        context.json_data = json.loads(context.result.stdout)
    return context.json_data


def _navigate_dotted_path(data, path: str):
    """Navigate a dotted path like 'summary.total_files_changed' into nested data."""
    parts = path.split(".")
    current = data
    for part in parts:
        if part.isdigit():
            index = int(part)
            assert isinstance(current, list), (
                f"Expected list at '{part}' in path '{path}', got {type(current).__name__}"
            )
            assert index < len(current), (
                f"Index {index} out of range (length {len(current)}) in path '{path}'"
            )
            current = current[index]
        else:
            assert isinstance(current, dict), (
                f"Expected dict at '{part}' in path '{path}', got {type(current).__name__}"
            )
            assert part in current, (
                f"Key '{part}' not found in path '{path}'. Available keys: {list(current.keys())}"
            )
            current = current[part]
    return current
