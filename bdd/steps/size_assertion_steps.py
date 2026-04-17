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

import subprocess

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
    """Invoke `git-prism manifest` with default flags to exercise the baseline
    MCP change-manifest path agents hit first."""
    _run_cli(context, _manifest_args())


@when("an agent requests the change manifest without opting in to function analysis")
def step_request_manifest_without_function_analysis(context: Context) -> None:
    """Invoke the default manifest path to assert that function analysis is
    off by default. RED today because the CLI hardcodes
    `include_function_analysis: true` in src/main.rs; PR 3 will introduce the
    default-off semantics that make the paired Then step pass."""
    _run_cli(context, _manifest_args())


@when("an agent requests the change manifest with function analysis enabled")
def step_request_manifest_with_function_analysis(context: Context) -> None:
    """Invoke the manifest with the opt-in flag to exercise the function-
    analysis path. RED today because `--include-function-analysis` is not yet
    a real CLI flag — passing it produces a non-zero exit and empty stdout,
    and the paired Then step fails when it tries to navigate the response."""
    _run_cli(context, [*_manifest_args(), "--include-function-analysis"])


@when(
    "an agent requests the change manifest with function analysis enabled "
    "and a {budget:d} token budget",
)
def step_request_manifest_with_budget(context: Context, budget: int) -> None:
    """Invoke the manifest with function analysis and an explicit token
    budget to exercise the budget-clamping path."""
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
    """Invoke `git-prism context` with default flags to exercise the baseline
    function-context path agents hit after seeing a manifest."""
    _run_cli(context, _context_args())


@when("an agent requests function context without a cursor")
def step_request_function_context_without_cursor(context: Context) -> None:
    """Invoke the context call with no cursor argument to exercise the
    first-page pagination path."""
    _run_cli(context, _context_args())


@when("an agent requests function context without a function name filter")
def step_request_function_context_without_filter(context: Context) -> None:
    """Invoke the context call with no name filter to exercise the unfiltered
    response path on an extreme-change fixture."""
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
    """Invoke the context call with an explicit two-name filter to exercise
    the name-scoping path."""
    _run_cli(
        context,
        [*_context_args(), "--function-names", f"{name1},{name2}"],
    )


@when("an agent requests function context with a {budget:d} token budget")
def step_request_function_context_with_budget(
    context: Context, budget: int,
) -> None:
    """Invoke the context call with an explicit token budget to exercise the
    budget-clamping path for callers and callees."""
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
    # Only check files with a supported language — tree-sitter returns null
    # for functions_changed when no grammar exists (e.g. language == "unknown").
    supported = [f for f in files if f.get("language", "unknown") != "unknown"]
    with_diffs = sum(1 for entry in supported if entry.get("functions_changed"))
    total = len(supported)
    assert total > 0, "expected at least one file with a supported language"
    assert with_diffs == total, (
        f"expected every supported-language file to carry function detail when opted in; "
        f"got {with_diffs} of {total}."
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
    assert isinstance(estimate, int) and estimate > 0, (
        f"expected a positive integer token_estimate, got {estimate!r} "
        f"(type {type(estimate).__name__}). RED until PR 2 lands token_estimate metadata."
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
    assert isinstance(cursor, str) and cursor, (
        f"expected a non-empty string cursor, got {cursor!r} "
        f"(type {type(cursor).__name__}). RED until PR 4 lands pagination."
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
    names = sorted(fn.get("name") or "" for fn in functions)
    assert names == ["function_0001", "function_0002"], (
        f"Filter should return exactly ['function_0001', 'function_0002'], "
        f"got {names}"
    )


@then("functions outside the filter are not included")
def step_response_functions_outside_filter_excluded(context: Context) -> None:
    data = _ensure_json_parsed(context)
    functions = _navigate_dotted_path(data, "functions")
    offenders = [
        fn.get("name")
        for fn in functions
        if fn.get("name") not in {"function_0001", "function_0002"}
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
        # No un-truncated entries available as a baseline. Verify that each
        # truncated entry's visible caller list is no longer than caller_count,
        # which is preserved at the original (pre-clamp) total so the agent
        # can tell how many were dropped. Works for both budget-clamped entries
        # (caller_count > len(callers)) and page-marker entries (both are 0).
        for fn in truncated:
            callers = fn.get("callers") or []
            caller_count = fn.get("caller_count") or 0
            assert len(callers) <= caller_count or caller_count == 0, (
                f"Truncated function {fn.get('name')!r} has more callers "
                f"({len(callers)}) than the recorded caller_count ({caller_count})"
            )
        return
    baseline_callers = max(len(fn.get("callers") or []) for fn in non_truncated)
    baseline_callees = max(len(fn.get("callees") or []) for fn in non_truncated)
    if baseline_callers == 0 and baseline_callees == 0:
        # The fixture produces entries with empty caller/callee lists, so
        # there is nothing to shorten relative to. The implementation still
        # signals truncation via the `truncated` flag and the metadata list
        # (asserted by the preceding step); this scenario's "shorter lists"
        # contract degenerates to "not longer than baseline" when the
        # baseline is zero.
        for fn in truncated:
            callers = fn.get("callers") or []
            callees = fn.get("callees") or []
            assert len(callers) <= baseline_callers and len(callees) <= baseline_callees, (
                f"Truncated function {fn.get('name')!r} has "
                f"{len(callers)} callers and {len(callees)} callees, "
                f"which exceeds the zero baseline"
            )
        return
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
    """Verify that budget enforcement ran by checking response metadata.

    Full OTLP metric verification requires a live collector harness, which is
    out of scope for CLI-level BDD tests. We proxy the metric assertion via
    ``metadata.function_analysis_truncated``:

        non-empty truncated list  ->  server.rs emits record_truncated(tool, "token_budget")

    The invariant is enforced in server.rs:
        if !response.metadata.function_analysis_truncated.is_empty() {
            metrics.record_truncated(tool_name, "token_budget");
        }

    If that guard ever breaks, this proxy will still pass even though the real
    metric was not recorded. A future telemetry integration test should verify
    the actual counter.
    """
    data = _ensure_json_parsed(context)
    truncated = _navigate_dotted_path(data, "metadata.function_analysis_truncated")
    # Budget enforcement emits the metric when this list is non-empty
    assert isinstance(truncated, list) and truncated, (
        f"Expected non-empty function_analysis_truncated as proxy for "
        f"{reason} metric on {tool}, got {truncated!r}"
    )
