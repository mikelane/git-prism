# Improving Agent Data Quality and Decision Support

- **Status**: Draft
- **Date**: 2026-04-09

## Context

v0.6.0 shipped the function context epic (#113): `get_function_context` returns callers, callees, and test references for changed functions across 13 languages. The three-step agent workflow (manifest → context → snapshots) is functional.

Three gaps remain that reduce the tool's value in real-world repos:

1. **Silent data loss.** Several common language patterns cause tree-sitter extraction to miss functions entirely, producing `functions_changed: null` for files that have analyzable functions. The agent gets no data instead of wrong data — the worst failure mode.

2. **Noisy caller matching.** `get_function_context` scans every file in the repo and matches callers by leaf name. In a repo with 500 files, a changed function named `validate()` matches every file that calls anything named `validate`, regardless of module boundaries. This noise scales with repo size.

3. **No prioritization signal.** The context tool returns raw lists of callers and callees. The agent must count them, check for test gaps, and decide what to review first. A computed blast radius score would eliminate this busywork.

## Proposal 1: Fix wrapper-pattern extraction (data quality)

### Problem

Tree-sitter represents certain language patterns as wrapper nodes around function declarations. The current analyzers walk top-level children looking for function nodes, but miss functions inside wrapper nodes:

| Language | Pattern | Wrapper node | Inner node |
|----------|---------|--------------|------------|
| TypeScript/JS | `export function foo() {}` | `export_statement` | `function_declaration` |
| TypeScript/JS | `export default function() {}` | `export_statement` | `function_declaration` |
| Python | `@decorator\ndef foo(): ...` | `decorated_definition` | `function_definition` |
| TypeScript/JS | `export class Foo {}` | `export_statement` | `class_declaration` |

These are not edge cases. `export function` is the default way to write public functions in TypeScript. `@decorator` is ubiquitous in Python (Flask, FastAPI, pytest, dataclasses). Every missed function means missing change detection, missing caller/callee data, and missing test references.

### Proposed fix

For each affected analyzer, add the wrapper node kind to the match statement in `extract_functions_from_node()` and recurse into its children. Same approach the Rust analyzer uses for `impl_item` → `function_item` and the C/C++ analyzers use for preprocessor conditionals.

The `extract_calls()` implementations already use full-tree DFS traversal (stack-based walk), so they're not affected — calls inside exported/decorated functions are already captured.

### Scope

- TypeScript/JavaScript: handle `export_statement` in `extract_functions_from_node()`
- Python: handle `decorated_definition` in `extract_functions_from_node()`
- Verify C++ `extern "C"` blocks (likely same pattern)
- Update existing tests; add test cases for each wrapper pattern
- BDD fixture for TypeScript can revert to using `export function` (currently works around the bug)

### Acceptance criteria

- `export function foo() {}` in TypeScript produces a `FunctionChange` entry
- `@app.route("/") def index(): ...` in Python produces a `FunctionChange` entry
- Existing tests unaffected
- No new `#[allow(dead_code)]` introduced

## Proposal 2: Blast radius scoring (agent decision support)

### Problem

After calling `get_function_context`, an agent receives raw caller/callee lists per function. To decide review priority, it must:

1. Count production callers vs test callers
2. Notice whether any tests exist at all
3. Compare across functions to find the highest-impact change
4. Decide whether a function with 0 callers is safe or just poorly covered

This is mechanical work that burns tokens and introduces judgment errors. The tool should provide this analysis directly.

### Proposed design

Add a `blast_radius` object to each `FunctionContextEntry`:

```json
{
  "name": "validate_input",
  "file": "src/validation.rs",
  "change_type": "modified",
  "blast_radius": {
    "production_callers": 5,
    "test_callers": 1,
    "has_tests": true,
    "risk": "high"
  },
  "callers": [...],
  "callees": [...],
  "test_references": [...]
}
```

Risk levels (based on production caller count and test coverage):

| Production callers | Has tests? | Risk |
|-------------------|------------|------|
| 0 | any | `"none"` (no downstream impact) |
| 1-2 | yes | `"low"` |
| 1-2 | no | `"medium"` (callers exist but untested) |
| 3+ | yes | `"medium"` |
| 3+ | no | `"high"` (wide blast radius, no test safety net) |

The `risk` field is a bounded enum, not a numeric score. Agents can filter/sort on it without interpreting arbitrary numbers.

### Scope

- Add `BlastRadius` struct to `src/tools/types.rs`
- Compute it in `build_function_context()` after caller matching
- Add `blast_radius` field to `FunctionContextEntry`
- Unit tests for risk classification logic
- Update README example output

### Acceptance criteria

- Every `FunctionContextEntry` has a `blast_radius` object
- Risk classification matches the table above
- BDD scenario: function with callers and tests → appropriate risk level
- BDD scenario: function with callers but no tests → higher risk level

## Proposal 3: Import-aware caller scoping (precision + performance)

### Problem

`get_function_context` currently does a brute-force scan: list every file in the repo at `head_ref`, parse each one, extract calls, and match by leaf function name. This has two problems:

1. **False positives.** If `utils.rs` has a changed function `parse()`, every file in the repo that calls any function named `parse()` — including `serde_json::from_str` chained with `.parse()`, `str::parse()`, `url.parse()` — shows up as a caller. Leaf-name matching can't distinguish them.

2. **Performance.** Parsing every file in a 500-file repo takes ~3-4 seconds (at ~7ms per file). For a 5000-file repo, that's 30+ seconds. The tool should be sub-second for typical use.

### Proposed design

Use import data to scope the scan. The manifest already extracts imports for every changed file. The approach:

1. For each changed file, identify its module path (e.g., `src/validation.rs` → `validation` or `crate::validation`)
2. Scan all files' imports (extractable lazily, without full function parsing) for references to that module
3. Only parse files that import from the changed module for full call extraction
4. Files with no import relationship are excluded from caller results

This doesn't eliminate all false positives (two modules could both have a function named `validate`), but it eliminates the cross-module noise that dominates in practice.

### Scope

- Add module-path inference from file paths (language-specific conventions)
- Add import-target extraction (reuse existing `extract_imports()`)
- Filter the file scan in `build_function_context()` to import-related files
- Fall back to full scan when import resolution is ambiguous
- Performance benchmark: before/after on a 100+ file repo

### Risks

- Module path conventions vary by language (Rust: `mod` hierarchy; Python: package directories; Go: package declarations; JS/TS: relative paths). Supporting all 13 languages correctly is significant work.
- Some languages have implicit imports (Ruby `require` with load path, Python relative imports). False negatives are possible.
- This changes the semantics of the caller list — previously "all files containing this name", now "files that plausibly import this module." Need to decide whether to surface the unscoped results as a fallback.

### Acceptance criteria

- Caller results for a changed function exclude files with no import relationship
- Performance improvement measurable on a 100+ file test repo
- No false negatives for direct import patterns in the top 4 languages (Rust, Python, Go, TypeScript)
- Fallback to full scan when import analysis is inconclusive

## Recommended sequencing

1. **Proposal 1 (wrapper extraction)** — bug fix, high confidence, directly improves all downstream tools. Ship as a patch (v0.6.1) or fold into the next minor.
2. **Proposal 2 (blast radius)** — small addition to existing types, high agent value, no architectural risk. Can ship alongside or immediately after proposal 1.
3. **Proposal 3 (import scoping)** — larger effort with language-specific complexity. Spike first to validate import resolution across top 4 languages. Ship as its own minor version.
