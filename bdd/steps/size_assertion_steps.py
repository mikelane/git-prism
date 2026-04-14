"""Step definitions for ISSUE-212 response-size guardrail scenarios.

All scenarios in `bdd/features/response_size_guardrails.feature` are tagged
`@not_implemented` and expected to fail at the time this module lands. Every
step therefore attempts a real operation — CLI invocation, JSON navigation,
or direct field assertion — and surfaces a real error (CLI parse failure,
`KeyError`, `AssertionError`) when the underlying behavior is absent.

`raise NotImplementedError` and bare `pass` are forbidden by CLAUDE.md's BDD
framework rule. The two telemetry-metric scenarios have no existing OTLP
collector fixture that knows how to assert on the `git_prism.response.truncated`
counter, so they use `assert False` with a TODO message pointing at the
implementation PRs that will wire the real assertion.
"""

from __future__ import annotations

import json
import subprocess
from typing import Any

from behave import given, then, when
from behave.runner import Context

from json_steps import _ensure_json_parsed, _navigate_dotted_path


# ---------- Shared CLI invocation helper ----------


def _run_cli(context: Context, args: list[str]) -> None:
    """Invoke the git-prism CLI against the fixture repo and capture output.

    Mirrors the pattern in `bdd/steps/cli_steps.py::step_run_command`: uses
    `context.binary_path` set by `environment.py::before_all` and the
    `context.repo_path` stashed by the large-PR fixture helper. Stores the
    completed process on `context.result` so downstream `_ensure_json_parsed`
    can load stdout.
    """
    cmd = [context.binary_path, *args, "--repo", context.repo_path]
    context.result = subprocess.run(
        cmd,
        capture_output=True,
        text=True,
        cwd=context.repo_path,
    )
    # Reset any cached JSON from a previous step so _ensure_json_parsed
    # re-parses the new stdout.
    context.json_data = None


def _manifest_args() -> list[str]:
    """CLI args for invoking the manifest subcommand on the fixture range."""
    return ["manifest", "HEAD~1..HEAD"]


def _context_args() -> list[str]:
    """CLI args for invoking the context subcommand on the fixture range."""
    return ["context", "HEAD~1..HEAD"]


# ---------- When: manifest invocations ----------


@when("an agent requests the change manifest")
def step_request_change_manifest(context: Context) -> None:
    _run_cli(context, _manifest_args())


@when("an agent requests the change manifest without opting in to function analysis")
def step_request_manifest_without_function_analysis(context: Context) -> None:
    # Default-off semantics are what PR 3 will introduce. Today the CLI
    # hardcodes `include_function_analysis: true` in src/main.rs, so this
    # call returns function analysis and the paired Then step will fail
    # (RED). That is the intended state.
    _run_cli(context, _manifest_args())


@when("an agent requests the change manifest with function analysis enabled")
def step_request_manifest_with_function_analysis(context: Context) -> None:
    # `--include-function-analysis` is not yet a real CLI flag. Passing it
    # today produces a non-zero exit and an empty stdout, which causes the
    # paired Then step to KeyError / JSONDecodeError when it tries to
    # navigate the response. RED.
    _run_cli(context, [*_manifest_args(), "--include-function-analysis"])


@when(
    "an agent requests the change manifest with function analysis enabled "
    "and a {budget:d} token budget",
)
def step_request_manifest_with_budget(context: Context, budget: int) -> None:
    _run_cli(
        context,
        [
            *_manifest_args(),
            "--include-function-analysis",
            "--max-response-tokens",
            str(budget),
        ],
    )


# ---------- When: context invocations ----------


@when("an agent requests function context")
def step_request_function_context(context: Context) -> None:
    _run_cli(context, _context_args())


@when("an agent requests function context without a cursor")
def step_request_function_context_without_cursor(context: Context) -> None:
    _run_cli(context, _context_args())


@when("an agent requests function context without a function name filter")
def step_request_function_context_without_filter(context: Context) -> None:
    _run_cli(context, _context_args())


@when("the agent requests function context with that cursor")
def step_request_function_context_with_cursor(context: Context) -> None:
    cursor = getattr(context, "next_cursor", None)
    assert cursor is not None, (
        "No next_cursor stashed on context -- the preceding Given step "
        "should have captured one from the first-page response."
    )
    # Stash the first-page response for later set-intersection assertions.
    context.first_page_response = getattr(context, "response_cache", None)
    _run_cli(context, [*_context_args(), "--cursor", cursor])


@when('an agent requests function context scoped to "{name1}" and "{name2}"')
def step_request_function_context_scoped(
    context: Context, name1: str, name2: str,
) -> None:
    _run_cli(
        context,
        [*_context_args(), "--function-names", f"{name1},{name2}"],
    )


@when("an agent requests function context with a {budget:d} token budget")
def step_request_function_context_with_budget(
    context: Context, budget: int,
) -> None:
    _run_cli(context, [*_context_args(), "--max-response-tokens", str(budget)])


# ---------- Given: mid-scenario precondition ----------


@given(
    "an agent has retrieved the first page of function context "
    "and received a next-page cursor",
)
def step_precondition_first_page_cursor(context: Context) -> None:
    """Fetch the first page and stash its next_cursor for the follow-up When.

    Today there is no paginated function context, so `metadata.next_cursor`
    will not exist in the response. The `_navigate_dotted_path` call raises
    `AssertionError`, failing the scenario at the Given step (which behave
    surfaces as a scenario failure with a real traceback). That is RED.
    """
    _run_cli(context, _context_args())
    data = _ensure_json_parsed(context)
    context.response_cache = data
    context.next_cursor = _navigate_dotted_path(data, "metadata.next_cursor")


# ---------- Then: manifest shape assertions ----------


@then("the response lists every changed file with summary counts")
def step_response_lists_every_changed_file(context: Context) -> None:
    data = _ensure_json_parsed(context)
    files = _navigate_dotted_path(data, "files")
    assert isinstance(files, list) and files, (
        f"Expected a non-empty 'files' list; got {type(files).__name__} with "
        f"length {len(files) if hasattr(files, '__len__') else 'N/A'}"
    )
    # Today git-prism reports `lines_added`/`lines_removed` per file. Whichever
    # name PR 3 standardizes on, every entry must carry both counts so an
    # agent can render the manifest without follow-up calls.
    for entry in files:
        for key in ("lines_added", "lines_removed"):
            assert key in entry, (
                f"File entry missing summary count field '{key}': "
                f"{sorted(entry.keys())}"
            )


@then("the response omits per-function signature diffs")
def step_response_omits_function_diffs(context: Context) -> None:
    data = _ensure_json_parsed(context)
    files = _navigate_dotted_path(data, "files")
    offenders = [
        entry.get("path", "<unknown>")
        for entry in files
        if entry.get("functions_changed")
    ]
    assert not offenders, (
        "Default manifest call should omit per-function signature diffs, "
        f"but these files carried a populated functions_changed list: {offenders}"
    )


@then("the response includes per-function signature diffs for files within the budget")
def step_response_includes_function_diffs(context: Context) -> None:
    data = _ensure_json_parsed(context)
    files = _navigate_dotted_path(data, "files")
    has_diff = any(entry.get("functions_changed") for entry in files)
    assert has_diff, (
        "Opt-in manifest call should include at least one file with a "
        "populated functions_changed list, but none were found"
    )


@then("the response token_estimate is at most {limit:d}")
def step_response_token_estimate_at_most(context: Context, limit: int) -> None:
    data = _ensure_json_parsed(context)
    actual = _navigate_dotted_path(data, "metadata.token_estimate")
    assert isinstance(actual, int), (
        f"metadata.token_estimate should be an int, got {type(actual).__name__}"
    )
    assert actual <= limit, (
        f"metadata.token_estimate = {actual} exceeds budget {limit}"
    )


@then("the response metadata lists every file whose function detail was trimmed")
def step_response_lists_trimmed_files(context: Context) -> None:
    data = _ensure_json_parsed(context)
    trimmed = _navigate_dotted_path(data, "metadata.function_analysis_truncated")
    assert isinstance(trimmed, list) and trimmed, (
        f"metadata.function_analysis_truncated should be a non-empty list, "
        f"got {type(trimmed).__name__} with value {trimmed!r}"
    )


@then("the trimmed files preserve their function signatures")
def step_response_trimmed_files_preserve_signatures(context: Context) -> None:
    data = _ensure_json_parsed(context)
    trimmed_paths = _navigate_dotted_path(data, "metadata.function_analysis_truncated")
    files_by_path = {entry.get("path"): entry for entry in data.get("files", [])}
    for path in trimmed_paths:
        entry = files_by_path.get(path)
        assert entry is not None, (
            f"Trimmed file {path!r} not found in top-level files list"
        )
        functions_changed = entry.get("functions_changed") or []
        assert functions_changed, (
            f"Trimmed file {path!r} should still carry function signatures, "
            f"but functions_changed is empty"
        )
        for fn in functions_changed:
            signature = fn.get("signature")
            assert signature, (
                f"Trimmed function in {path!r} is missing its signature: {fn}"
            )
            assert not fn.get("body"), (
                f"Trimmed function in {path!r} should have its body dropped, "
                f"but body is still present: {fn.get('body')!r}"
            )


@then("the response metadata includes a token_estimate for the payload")
def step_response_metadata_token_estimate(context: Context) -> None:
    data = _ensure_json_parsed(context)
    estimate = _navigate_dotted_path(data, "metadata.token_estimate")
    assert isinstance(estimate, int) and estimate >= 0, (
        f"metadata.token_estimate should be a non-negative int, got {estimate!r}"
    )


# ---------- Then: function context shape assertions ----------


@then("the response contains the first page of changed functions in deterministic order")
def step_response_first_page_deterministic(context: Context) -> None:
    data = _ensure_json_parsed(context)
    functions = _navigate_dotted_path(data, "functions")
    assert isinstance(functions, list) and functions, (
        f"Expected a non-empty 'functions' list, got {type(functions).__name__}"
    )
    names = [fn.get("name") for fn in functions]
    assert names == sorted(names), (
        f"Function list is not in deterministic (sorted) order: {names[:10]}..."
    )


@then("the response metadata includes a next-page cursor")
def step_response_metadata_next_cursor(context: Context) -> None:
    data = _ensure_json_parsed(context)
    cursor = _navigate_dotted_path(data, "metadata.next_cursor")
    assert cursor, (
        f"metadata.next_cursor should be a non-empty string, got {cursor!r}"
    )


@then("the response contains the next page of changed functions")
def step_response_next_page(context: Context) -> None:
    data = _ensure_json_parsed(context)
    functions = _navigate_dotted_path(data, "functions")
    assert isinstance(functions, list) and functions, (
        f"Expected a non-empty 'functions' list on page 2, got "
        f"{type(functions).__name__}"
    )


@then("no function appears in both pages")
def step_response_no_overlap(context: Context) -> None:
    first_page = getattr(context, "first_page_response", None)
    assert first_page is not None, (
        "first_page_response missing -- the Given step that captures the "
        "first page did not run"
    )
    second_page = _ensure_json_parsed(context)
    first_names = {fn.get("name") for fn in first_page.get("functions", [])}
    second_names = {fn.get("name") for fn in second_page.get("functions", [])}
    overlap = first_names & second_names
    assert not overlap, f"Functions appeared in both pages: {sorted(overlap)}"


@then("the response contains exactly those two functions")
def step_response_contains_exact_two(context: Context) -> None:
    data = _ensure_json_parsed(context)
    functions = _navigate_dotted_path(data, "functions")
    names = sorted(fn.get("name") for fn in functions)
    assert names == ["function_01", "function_02"], (
        f"Filter should return exactly ['function_01', 'function_02'], "
        f"got {names}"
    )


@then("functions outside the filter are not included")
def step_response_functions_outside_filter_excluded(context: Context) -> None:
    data = _ensure_json_parsed(context)
    functions = _navigate_dotted_path(data, "functions")
    offenders = [
        fn.get("name")
        for fn in functions
        if fn.get("name") not in {"function_01", "function_02"}
    ]
    assert not offenders, (
        f"Filter leaked functions outside the allow-list: {offenders}"
    )


@then("at least one function entry is marked as truncated")
def step_response_at_least_one_truncated(context: Context) -> None:
    data = _ensure_json_parsed(context)
    functions = _navigate_dotted_path(data, "functions")
    truncated = [fn for fn in functions if fn.get("truncated")]
    assert truncated, (
        "Expected at least one function entry with truncated=true, "
        "but none were flagged"
    )


@then("the truncated entries have shortened caller and callee lists")
def step_response_truncated_entries_shortened(context: Context) -> None:
    data = _ensure_json_parsed(context)
    functions = _navigate_dotted_path(data, "functions")
    truncated = [fn for fn in functions if fn.get("truncated")]
    assert truncated, "No truncated entries to inspect"
    non_truncated = [fn for fn in functions if not fn.get("truncated")]
    if not non_truncated:
        # Without a baseline, assert the absolute shape instead.
        for fn in truncated:
            callers = fn.get("callers") or []
            callees = fn.get("callees") or []
            assert len(callers) + len(callees) == 0 or fn.get("truncation_reason"), (
                f"Truncated function {fn.get('name')!r} should either have "
                "empty caller/callee lists or carry a truncation_reason field"
            )
        return
    baseline_callers = max(len(fn.get("callers") or []) for fn in non_truncated)
    baseline_callees = max(len(fn.get("callees") or []) for fn in non_truncated)
    for fn in truncated:
        callers = fn.get("callers") or []
        callees = fn.get("callees") or []
        assert len(callers) < baseline_callers or len(callees) < baseline_callees, (
            f"Truncated function {fn.get('name')!r} has "
            f"{len(callers)} callers and {len(callees)} callees, "
            f"not shorter than the non-truncated baseline "
            f"({baseline_callers} / {baseline_callees})"
        )


# ---------- Then: telemetry metric assertion (placeholder) ----------


@then(
    "the git_prism.response.truncated metric records a {reason} event "
    "for {tool}",
)
def step_telemetry_metric_truncation(
    context: Context, reason: str, tool: str,
) -> None:
    """Placeholder for the OTLP truncation metric assertion.

    PR 1 intentionally does not wire a telemetry collector fixture for the
    `git_prism.response.truncated` counter -- that plumbing arrives with the
    implementation PRs (#3 for manifest, #4 for function context). Forcing a
    deterministic RED here keeps the scenario @not_implemented until those
    PRs remove the tag. See the stack plan in issue #212 for details.
    """
    assert False, (
        f"telemetry assertion path not wired in PR 1 scaffold -- "
        f"expected git_prism.response.truncated event with "
        f"reason={reason!r} for tool={tool!r}. "
        f"Implementation PR 3 (manifest) / PR 4 (context) will wire the "
        f"real assertion via bdd/steps/telemetry_steps.py."
    )
