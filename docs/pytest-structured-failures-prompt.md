# Project: pytest-structured-failures

You're building a pytest plugin that emits structured, agent-consumable failure
reports. This is the "what broke" complement to git-prism (the "what changed"
tool). Together they close the agent coding loop: git-prism tells an agent which
functions changed, this plugin tells it which tests broke and why — without the
agent burning tokens parsing tracebacks.

## The problem

Every agent coding loop does the same thing: run tests, get a wall of text,
parse the traceback with general reasoning, lose context tokens on formatting
noise. A test run that fails 3 out of 200 tests produces kilobytes of output
where 95% is passing-test noise and the remaining 5% is human-formatted
stack traces the agent has to reconstruct into actionable information.

The agent needs to know:
- Which tests failed (not which passed)
- What was asserted vs. what was actual
- The local variables at the failure point
- Which source lines are involved
- How to link the failure back to a recent change

It does NOT need: ANSI color codes, progress dots, section headers, passing
test names, or prose explanations of what pytest is doing.

## What to build

A pytest plugin (`pytest-structured-failures`) that:

1. **Hooks `pytest_runtest_makereport`** to capture failure details as
   structured data during the test run.
2. **Emits a JSON report** (via `--structured-failures=<path>` CLI flag)
   containing only the failures, with:
   - Test node ID, file, line number
   - Exception type and message
   - Assertion introspection (left value, right value, operator)
   - Local variables at the failure point (serialized safely — no
     repr bombs on large objects)
   - The failing source line and surrounding context (±5 lines)
   - Duration of the failed test
3. **Optionally emits to stdout** (via `--sf-stdout`) for piping directly
   into an agent's context.
4. **Stays out of the way** — zero impact on passing tests, no output
   unless failures occur, no dependency beyond pytest itself.

## Design principles

- **Structured over narrative.** Every field is typed and named. No prose.
  An agent should be able to act on a failure object without interpreting
  English sentences.
- **Failures only.** Passing tests are noise. The report contains zero
  information about tests that passed.
- **Safe serialization.** Local variables are serialized with truncation
  and type fallbacks. A 10MB DataFrame in a local should become
  `{"type": "DataFrame", "shape": [10000, 50], "repr_truncated": "...first 200 chars..."}`,
  not a 10MB JSON string that blows the context window.
- **Minimal footprint.** No dependencies beyond pytest. No config files
  required. One CLI flag to turn it on.

## Example output

    {
      "version": "0.1.0",
      "summary": {
        "total": 200,
        "passed": 197,
        "failed": 3,
        "errors": 0,
        "duration_seconds": 12.4
      },
      "failures": [
        {
          "node_id": "tests/test_math.py::test_divide_by_zero",
          "file": "tests/test_math.py",
          "line": 42,
          "duration_seconds": 0.003,
          "exception": {
            "type": "AssertionError",
            "message": "assert divide(10, 0) == 'infinity'"
          },
          "assertion": {
            "left": "ZeroDivisionError('division by zero')",
            "right": "'infinity'",
            "operator": "=="
          },
          "locals": {
            "numerator": 10,
            "denominator": 0
          },
          "source_context": {
            "start_line": 37,
            "end_line": 47,
            "lines": [
              "def test_divide_by_zero():",
              "    numerator = 10",
              "    denominator = 0",
              "    result = divide(numerator, denominator)",
              "    assert result == 'infinity'  # <-- line 42",
              ""
            ],
            "failing_line_index": 4
          }
        }
      ]
    }

## Technical starting points

- **pytest plugin entry point:** Use `setuptools` entry point
  `pytest11 = pytest_structured_failures.plugin`. This is how pytest
  discovers plugins installed via pip.
- **Key hooks:**
  - `pytest_addoption` — register `--structured-failures` and `--sf-stdout`
  - `pytest_runtest_makereport` — capture failure data per test phase
    (setup, call, teardown)
  - `pytest_sessionfinish` — write the JSON report
- **Assertion introspection:** pytest already rewrites assertions. The
  `report.longrepr` object contains the rewritten assertion info.
  `report.longrepr.reprcrash` has file/line/message.
  `report.longrepr.reprtraceback` has the frame chain.
  For assertion details, look at `report.longrepr.reprcrash.message`
  which contains the `assert X == Y` rewrite.
- **Local variable capture:** Use `report.longrepr.reprtraceback.reprentries[-1].reprlocals`
  if available, or walk `report.longrepr.chain` for the innermost frame.
  Alternatively, capture from `sys.last_traceback` or the `ExceptionInfo`
  object.
- **Safe serialization:** Build a `safe_repr(obj, max_length=200)` helper
  that handles: primitives (pass through), strings (truncate), collections
  (truncate + show length), objects with `__len__`/`shape` (show metadata),
  everything else (type name + truncated repr).

## Project conventions

- **Python 3.10+**, `pyproject.toml` only (no setup.py, no setup.cfg)
- **TDD is mandatory.** Write a failing test before production code. Every
  feature starts with a test that demonstrates the current behavior is wrong.
- **Test with `pytester`** — pytest's built-in fixture for testing plugins.
  It runs pytest in a subprocess and lets you assert on the result. This is
  the standard way to test pytest plugins.
- **No AI slop in docs.** Write like a human engineer. No marketing language.
- **Commit messages:** Conventional commits (`feat:`, `fix:`, `test:`, etc.)
- **Linting:** `ruff check` and `ruff format` (not black, not flake8)
- **Type hints everywhere.** Use `mypy --strict` or at minimum check with
  `pyright`.

## Workflow

1. **Scaffold the project** — pyproject.toml, src layout, pytester-based
   test skeleton
2. **Minimal viable plugin** — capture failures, emit JSON with node_id +
   exception type + message. Get a pytester test green that runs a failing
   test and asserts the JSON output has the right structure.
3. **Assertion introspection** — extract left/right/operator from pytest's
   assertion rewriting
4. **Local variable capture** — safely serialize locals at the failure point
5. **Source context** — extract the failing line ±5 lines of context
6. **stdout mode** — `--sf-stdout` for direct piping
7. **Edge cases** — errors vs failures, setup/teardown failures, parametrized
   tests, xfail, fixtures that raise

## What NOT to do

- Don't capture passing tests. Ever. They're noise.
- Don't add dependencies beyond pytest. No click, no rich, no pydantic.
  Keep the dependency footprint at zero.
- Don't try to determine the *cause* of the failure or suggest fixes.
  That's the agent's job. This plugin provides the raw structured data.
- Don't format anything for human consumption. No colors, no boxes, no
  progress bars. If a human wants to read the output, they can pipe it
  through `jq`.
- Don't try to integrate with git-prism in v0. The connection between
  "what changed" and "what broke" is a future capability. Build this
  plugin to stand alone first.

## Future vision (don't build yet, just know the direction)

The eventual goal is a `get_test_impact` tool that cross-references
git-prism's `functions_changed` with this plugin's failure data and
test coverage info to answer: "which of my changes broke which tests?"
That's the tool that closes the full agent coding loop. But it requires
both git-prism and this plugin to exist independently first.
