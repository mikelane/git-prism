# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What This Is

Agent-optimized git data MCP server. Four tools: `get_change_manifest` (structured metadata about what changed), `get_file_snapshots` (complete before/after file content), `get_commit_history` (per-commit manifests for a range), and `get_function_context` (callers, callees, and test references for changed functions). Replaces human-oriented diffs with structured JSON for LLM agents.

Supports both commit-to-commit comparison (`main..HEAD`) and working tree comparison (`HEAD` alone), which shows staged and unstaged changes vs a base ref.

## Build & Test

```bash
cargo clippy -- -D warnings   # lint — warnings are errors
cargo fmt --check              # format check
cargo test                     # unit + integration tests
cargo build --release          # release build
```

## Conventions

- **TDD is mandatory.** See the TDD section below for the full rules.
- **Error handling:** `thiserror` for library error types in modules, `anyhow` for application-level errors in `main.rs`.
- **Snapshot tests:** Use `insta` crate. Snapshot files live next to the source in `snapshots/` directories. Update with `cargo insta review`.
- **Integration tests:** Build real git repos in temp dirs. Test helpers may use `git` CLI for repo setup (gix's write API is impractical for test fixtures). Production code must use `gix` only — never shell out to `git` CLI in non-test code.
- **Tree-sitter nullability:** `functions_changed` is `null` (not empty array) when no grammar exists for a language. `None` in Rust → `null` in JSON. The distinction matters.
- **Function diffing is content-aware.** `diff_functions()` compares functions by body hash (SHA-256), not line position. Moved-but-unchanged functions are suppressed. Renames detected by matching unmatched deleted/added pairs with identical body hashes. Use `body_hash_for_node()` in analyzers, `FunctionChange::from_function()` to construct changes.
- **Call extraction:** `extract_calls()` on `LanguageAnalyzer` returns `Vec<CallSite>` with callee name, line number, method-call flag, and optional receiver. Each language has its own call node kinds (Rust: `call_expression`+`macro_invocation`, Python: `call`, Go/TS/C/C++: `call_expression`, Java: `method_invocation`, PHP: `function_call_expression`+`member_call_expression`, C#: `invocation_expression`, Ruby: `call`, Swift/Kotlin: `call_expression`+`navigation_expression`). Caller matching uses leaf name comparison.
- **Import-scoped caller scanning.** `get_function_context` uses import data to scope the caller scan for Rust, Python, Go, and TypeScript/JavaScript. Only files that import the changed module (or are in the same directory for Go) are fully parsed. Unsupported languages fall back to full scan. Logic lives in `src/tools/import_scope.rs`. Changed files are always scanned. Use `infer_module_path()` and `imports_reference_module()` to check relationships.
- **Wrapper-pattern extraction.** Some tree-sitter nodes wrap function declarations (`export_statement` in TS/JS, `decorated_definition` in Python, `linkage_specification` in C++). Analyzers recurse into these wrapper nodes to find the inner function/class definitions. Same pattern used by Rust analyzer for `impl_item` and C/C++ for preprocessor conditionals.
- **Blast radius scoring.** Every `FunctionContextEntry` includes a `blast_radius` object with `production_callers`, `test_callers`, `has_tests`, and `risk` (enum: `none`/`low`/`medium`/`high`). Risk classification keys on **production callers only**: 0 production callers → `none` (regardless of test callers), 1–2 with tests → `low`, 1–2 without tests → `medium`, 3+ with tests → `medium`, 3+ without tests → `high`. Use `BlastRadius::compute(production, test)` to construct.
- **All public types** derive `Serialize` and relevant `schemars::JsonSchema` for MCP tool schemas.

## Key Dependencies

- **`rmcp` 1.3** — MCP SDK. Tools defined with `#[tool_router]` and `#[tool]` proc macros. Stdio transport.
- **`gix` 0.81** — Pure Rust git. Use minimal feature flags (`basic`, `blob-diff`, `sha1`). Do not use `git2` or shell out to `git`.
- **`tree-sitter` 0.26** — Native Rust. Grammar crates: `tree-sitter-c`, `tree-sitter-cpp`, `tree-sitter-go`, `tree-sitter-python`, `tree-sitter-typescript`, `tree-sitter-javascript`, `tree-sitter-rust`.
- **`sha2` 0.10** — SHA-256 hashing for function body content (content-aware diffs) and repo path privacy.
- **`clap` 4** — CLI with derive API. Subcommands: `serve`, `manifest`, `snapshot`, `history`, `context`, `languages`.

## Working Tree Mode

`git-prism manifest HEAD` (a single ref, no `..`) compares that ref against the working tree instead of diffing two commits. Each file entry includes a `change_scope` field: `"staged"` (index vs HEAD) or `"unstaged"` (disk vs index). The same file can appear twice if it has both staged and unstaged changes.

For the MCP tool, omit `head_ref` to trigger working tree mode: `get_change_manifest(base_ref="HEAD")`.

## Module Responsibilities

- `src/git/` — Git data access. Wraps `gix`. Returns structured Rust types, never strings.
- `src/treesitter/` — Function/import extraction. Each language is a self-contained file implementing `LanguageAnalyzer` trait.
- `src/tools/` — MCP tool handlers. Orchestrate git + treesitter modules into JSON responses. `context.rs` handles function context (callers/callees/test references).
- `src/pagination.rs` — Cursor encoding, pagination types, validation.
- `src/server.rs` — MCP server lifecycle (rmcp, stdio).
- `src/main.rs` — CLI wiring only (clap).

## Adding a New Language Analyzer

1. Add grammar crate to `Cargo.toml`
2. Create `src/treesitter/<lang>.rs` implementing `LanguageAnalyzer`
3. Register extension in `src/treesitter/mod.rs` registry
4. Add table-driven tests with known source snippets

## TDD

### The Three Laws

1. You may not write any production code unless it is to make a failing test pass.
2. You may not write any more test code than is required to make a test fail (and compilation errors count as test failures).
3. You may only write as much production code as necessary to make a failing test pass.

### The Cycle

**Red → Green → Triangulate → Refactor.**

- **Red:** Write one test that fails. Stop as soon as it fails (including compile failures).
- **Green:** Write the minimum production code to make that test pass. Hardcoded returns are fine at this stage.
- **Triangulate:** Write a second (or third) test case with different inputs that breaks a hardcoded or overly specific implementation, forcing you to generalize toward the real algorithm. Don't generalize on the first green — let the tests push you there.
- **Refactor:** Clean up duplication in both test and production code while all tests stay green. This is the only step where you change code without changing behavior.

### Unit Test Rules

- **Hermetic and deterministic.** No network, no filesystem, no shared mutable state, no system clock. Every run produces the same result regardless of environment or execution order.
- **Test behavior via public methods only.** Do not test private functions, internal state, or implementation details. If you feel the need to test a private method, that's a signal to extract it into its own type with a public API.
- **Let tests drive abstractions.** If testing is hard, the design needs to change — not the test. Use test difficulty as feedback on your API surface.
- **No flaky tests.** A test that fails intermittently is worse than no test. If you see flakiness, fix the root cause immediately — do not retry, do not ignore.

## Epic SDLC

Epic structure is a strict pipeline with enforced gates. Every epic follows this sequence — no skipping steps.

### Epic Issue Structure

Every epic issue must contain these sections in order:

1. **Goal** — one paragraph stating what and why
2. **Context** — the problem being solved and why it matters now
3. **Scope** — what's included, plus explicit "Out of scope" subsection when relevant
4. **Design Documents** — links to specs/ADRs (when they exist)
5. **Acceptance Criteria** — checkbox list of verifiable conditions
6. **Child Issues** — ordered checklist of implementation issues
7. **Dependency Order** — ASCII dependency graph showing the build sequence

Label the issue with `epic`.

### The Pipeline

```
Spike (optional) → BDD Bootstrap → Implementation Issues → Capstone Demo
```

Each arrow is a **real GitHub blocking dependency** set via the Dependencies API — not markdown text saying "blocked by." If you write "BLOCKED BY #42" in the issue body but don't call the API, it doesn't count.

### The Rules

1. **Spike first if the problem space is unknown.** The spike lives on a `spike/<topic>` branch that is never merged. Its only deliverable is an ADR in `docs/decisions/NNNN-short-title.md`. The prototype code is disposable — the ADR is the artifact. No TDD during spikes.

2. **BDD Bootstrap blocks everything.** Before any implementation begins, write ALL Gherkin scenarios for the epic using a real cucumber framework (`behave`, `cucumber-js`) in a **different language than production code**. Tag each scenario with `@ISSUE-XX` pointing to the implementation issue that will make it pass. Step definitions must attempt real operations and fail with assertion errors — not `raise NotImplementedError` or `pass`. The tests must run and fail (RED).

3. **Implementation issues reference their scenarios.** Each issue's body includes the specific `@ISSUE-XX` Gherkin scenarios it must make pass. First commit on the branch removes `@not_implemented` from those scenarios (proving RED). Then make them GREEN. Use TDD internally for unit tests.

4. **Capstone demo is mandatory.** It's a narrated `.mp4` video proving the epic works end-to-end. Not screenshots, not GIFs. It's blocked by ALL implementation issues. The epic isn't done without it.

### Child Issue Decomposition

Child issues follow this consistent ordering:

```
Spike (optional)
  └→ BDD Bootstrap
      └→ Core types / trait methods
          ├→ Implementation batch 1
          ├→ Implementation batch 2
          └→ Implementation batch 3
              └→ Tool handler / wiring
                  ├→ Telemetry
                  └→ Documentation
                      └→ Capstone demo + release
```

### Setting Up Dependencies

```shell
# Get the internal ID (not the issue number) of the blocker
BLOCKER_ID=$(gh api repos/OWNER/REPO/issues/100 --jq '.id')

# Set blocked-by relationship
echo "{\"issue_id\": $BLOCKER_ID}" | \
  gh api repos/OWNER/REPO/issues/101/dependencies/blocked_by \
  --method POST --input -
```

You must also add child issues to the epic as **sub-issues**. Sub-issues and blocked-by are separate concepts — you need both.

### What Agents Get Wrong

- Writing Gherkin after code (defeats the purpose — it must block implementation)
- Using the same language for BDD and production (you'll import internals and test implementation, not behavior)
- Setting up sub-issues but forgetting blocked-by relationships
- Claiming "tests pass" without the capstone demo
- Promoting spike code to production because "it mostly works"

### Verification

Before any implementation agent starts work on issue `#XX`, check that its blockers are actually closed:

```shell
gh api repos/OWNER/REPO/issues/XX/dependencies/blocked_by \
  --jq '.[].number' | while read b; do
    gh issue view "$b" --repo OWNER/REPO --json state -q .state
  done
```

Don't trust memory or issue titles. Check the API.

## Git Hooks (lefthook)

A pre-push hook runs `fmt --check`, `clippy`, and `test` before every push. Managed by [lefthook](https://github.com/evilmartians/lefthook) via `lefthook.yml`. After cloning:
```bash
lefthook install
```
Never use `--no-verify` to skip hooks.
