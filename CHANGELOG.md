# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Breaking Changes

- **`get_change_manifest` default for `include_function_analysis` flipped to `false`.** Function-level diffs are now opt-in, aligning the tool's default with its "cheap first-resort" contract. Pass `include_function_analysis: true` to restore the previous behavior. The CLI adds an `--include-function-analysis` flag with the same effect.
- **`get_change_manifest` enforces a token budget (default 8192).** When the response would exceed the budget, function/import analysis is progressively stripped per file via a three-tier algorithm (full → signatures-only → bare). Trimmed files that preserved their function signatures are listed in `metadata.function_analysis_truncated`. Pass `max_response_tokens: 0` (or the CLI `--max-response-tokens 0`) to disable enforcement. Internal callers (e.g. `get_function_context`) bypass enforcement via `ManifestOptions.max_response_tokens = None`.
- **`record_truncated` metric now carries a `reason` label.** New `reason="token_budget"` events are emitted whenever the manifest budget trims any file detail. Cardinality is bounded via `classify_truncation_reason` in `src/privacy.rs`.

### Added

- **`get_function_context` gains pagination, a name filter, and a response-size budget.** Four new `ContextArgs` fields — `cursor`, `page_size` (1–500, default 25), `function_names`, `max_response_tokens` (default 8192, `0` disables) — mirror the manifest tool's guardrails so the second-resort read tool can no longer exceed MCP context limits. The CLI exposes the same knobs: `--cursor`, `--page-size`, `--function-names=a,b`, `--max-response-tokens`.
- **Per-entry `truncated` flag on `FunctionContextEntry`.** When the budget clamps an entry's caller / callee / test-reference lists (top 5 callers, top 5 callees, top 3 test references are kept), the entry's `truncated` flag is set and its name lands in `metadata.function_analysis_truncated`. The flag is also set on the last kept entry when the response was cut short by the budget or page-size, so `function_analysis_truncated` is never empty on a truncated response.
- **`function_names` as the escape hatch for re-querying clamped entries.** Agents that need the full caller / callee list for an entry that was clamped on a prior paginated call should re-request with `function_names: ["name"]` — the filtered response fits comfortably within the budget.
- **Metadata mirrors pagination cursor.** `ContextMetadata.next_cursor` duplicates `pagination.next_cursor` for agents reading only the metadata block.
- **Bounded-cardinality truncation metric.** `get_function_context` now emits `record_truncated(tool, reason)` with `reason="paginated"` when a next-page cursor is returned and `reason="token_budget"` when any entry was clamped, matching the manifest tool's signalling contract.

## [0.6.0] — 2026-04-09

> Released 2026-04-10 (retroactively tagged; see ADR 0007 for history).

### Added

- **`get_function_context` tool.** New MCP tool and CLI subcommand (`git-prism context HEAD~1..HEAD`) that returns callers, callees, and test references for each changed function. Agents no longer need to grep through the codebase to find who calls a modified function or which tests cover it.
- **Call extraction across all 13 languages.** New `extract_calls()` method on `LanguageAnalyzer` with language-specific node kinds: Rust (`call_expression` + `macro_invocation`), Python (`call`), Go/TS/JS/C/C++ (`call_expression`), Java (`method_invocation`), PHP (`function_call_expression` + `member_call_expression`), C# (`invocation_expression`), Ruby (`call`), Swift/Kotlin (`call_expression` + `navigation_expression`).
- `CallSite` struct with callee name, line number, method-call flag, and optional receiver.
- `RepoReader::list_files_at_ref()` for walking git trees to discover caller files.
- Test file detection via path conventions (e.g., `/tests/`, `_test.go`, `.test.ts`).
- Tracing spans for context operations: `context.build`, `context.get_manifest`, `context.scan_files`, `context.match_callers`, `context.extract_callees`.
- 15 BDD scenarios for function context (callers, callees, test references, unsupported languages, multi-language extraction, CLI validation).
- ADR 0005: call-site extraction spike findings.
- 51 new unit tests (496 total).

### Changed

- Agent Workflow in README updated from two-step (manifest -> snapshots) to three-step (manifest -> context -> snapshots).
- CLAUDE.md documents call extraction conventions, `extract_calls()` pattern, and `context` subcommand.

## [0.5.0] — 2026-04-09

> Released 2026-04-10 (retroactively tagged; see ADR 0007).

### Added

- **Content-aware function diffs.** `diff_functions()` now compares functions by SHA-256 body hash instead of line position. Three improvements over v0.4:
  - **Reorder suppression** — functions that moved but didn't change no longer produce false `modified` entries.
  - **Body-only detection** — functions whose implementation changed (but signature didn't) are now detected as `modified`, even when line numbers are stable.
  - **Rename detection** — when a deleted function and an added function share the same body hash, they're reported as a single `renamed` entry with `old_name` populated instead of separate `deleted` + `added`.
- New `renamed` variant in `functions_changed[].change_type`.
- New `old_name` field on function change entries (null for non-renames).
- `body_hash_for_node()` helper for tree-sitter analyzers.
- `FunctionChange::from_function()` constructor for building change entries.
- 4 integration tests with real git repos covering reorder, body change, rename, and rename+modify scenarios.
- 5 BDD scenarios for content-aware diffs.

### Changed

- `modified` in `functions_changed` now means the function body changed (was: line positions changed). This is a semantic change — fewer false positives, more true positives.
- All 13 language analyzers compute body hashes during tree-sitter extraction.
- CLAUDE.md and README.md updated with content-aware diffing documentation.

## [0.4.0] — 2026-04-08

### Added

- **PHP** tree-sitter analyzer (`.php`) — functions, class methods, `use` declarations
- **C#** tree-sitter analyzer (`.cs`) — methods, constructors, `using` directives
- **Kotlin** tree-sitter analyzer (`.kt`, `.kts`) — functions, methods, extension functions, imports
- **Ruby** tree-sitter analyzer (`.rb`) — methods, singleton methods, `require`/`require_relative`
- **Swift** tree-sitter analyzer (`.swift`) — functions, methods, init declarations, imports

Function-level analysis now covers 13 languages (was 8). The selection targets widely-used languages on GitHub as of early 2026 (Rust, Go, Python, JavaScript, TypeScript, Java, C, C++, C#, Ruby, Swift, Kotlin, PHP); language priority was chosen from informal GitHub usage signals rather than a formal Octoverse citation.

### Changed

- README languages table updated with all 13 languages
- CLI `languages` command lists all 13 languages

## [0.3.1] — 2026-04-08

### Fixed

- Mutation testing CI: incremental PR check is now informational only (reports score, never blocks). The 90% threshold applies only to the full suite on main. Previously, equivalent mutants in small diffs caused false failures.

### Added

- Python and TypeScript tree-sitter tests for class/method line number accuracy.

## [0.3.0] — 2026-04-08

### Added

- **Cursor-based pagination** for `get_change_manifest` and `get_commit_history`. Large diffs are no longer silently truncated — agents page through results using opaque cursors. New parameters: `cursor` (continuation token) and `page_size` (1-500, default 100).
- **OpenTelemetry observability** — opt-in metrics and traces via `GIT_PRISM_OTLP_ENDPOINT`. 14 metrics (request counts, duration histograms, token estimates, error rates) and per-tool trace trees with sub-spans for gix and tree-sitter operations.
- **Mutation testing CI** — cargo-mutants runs on every PR (incremental) and weekly (full suite) with sharded execution and nextest for faster feedback.
- **CLI auto-pagination** — `manifest` and `history` commands loop through all pages internally and output complete results. New `--page-size` flag for tuning.
- Privacy-safe telemetry attributes: repo paths SHA-256 hashed, ref names normalized to bounded enum, commit SHAs restricted to span attributes.
- Pagination telemetry: `pages_requested` counter, `page_number` and `page_size` span attributes.

### Changed

- **Breaking:** `ManifestResponse` replaces `truncated`/`truncation_info` with `pagination` object (`total_items`, `page_start`, `page_size`, `next_cursor`).
- **Breaking:** `HistoryResponse` gains `pagination` object.
- Default manifest page size is 100 files (was 200 hard truncation limit). Agents can request up to 500 per page.
- Summary always reflects all files regardless of which page is returned.
- Tree-sitter analysis runs only on the current page's files (performance improvement for large diffs).

### Technical

- New modules: `src/telemetry.rs`, `src/metrics.rs`, `src/privacy.rs`, `src/pagination.rs`
- `base64` added as direct dependency for cursor encoding
- OpenTelemetry stack: `tracing`, `tracing-opentelemetry`, `opentelemetry-otlp` (gRPC/tonic)
- ADRs: mutation testing baseline (ADR 0002), pagination spike (ADR 0003)
- Test count: 246 → 366 (120 new tests including mutation-testing gap closers)
- CI: mutation testing workflow with 4-shard parallelism, nextest, copy-target caching

## [0.2.0] — 2026-04-05

### Added

- Java tree-sitter analyzer with class-qualified method extraction (`Calculator.add`) and import parsing
- C tree-sitter analyzer for `.c` and `.h` files with function extraction and `#include` directives
- C++ tree-sitter analyzer with namespace-qualified methods (`math::Calculator::add`) and preprocessor-block recursion, supporting `.cpp`, `.hpp`, `.cc`, `.cxx`, `.hh`, `.hxx` extensions
- Working tree status: `git-prism manifest HEAD` compares HEAD against the working tree and returns staged + unstaged changes with a `change_scope` field
- Per-commit history: `git-prism history HEAD~N..HEAD` returns one manifest per commit in the range, including commit SHA, author, message, and timestamp
- `get_commit_history` MCP tool for per-commit breakdowns
- Published to crates.io — install with `cargo install git-prism`

### Changed

- Language detection now covers 8 languages (added Java, C, C++)
- Snapshot command rejects working tree mode with a clear error message directing users to use a commit range

### Technical

- Added gix `status` feature flag for working tree diffs (per ADR 0001)
- New `src/git/worktree.rs` module wrapping the gix status API
- `FileChange` type now carries a `change_scope` field: `Staged`, `Unstaged`, or `Committed`
- BDD acceptance suite expanded with 14 new scenarios across 5 feature files

## [0.1.0] — 2026-04-04

### Added

- Initial release with two MCP tools: `get_change_manifest` and `get_file_snapshots`
- CLI subcommands: `serve`, `manifest`, `snapshot`, `languages`
- Tree-sitter analyzers for Go, Python, TypeScript, JavaScript, and Rust
- Function-level and import-level change detection
- Dependency file diffing for Cargo, npm, Poetry, uv, and Go modules
- Generated file detection (lockfiles, minified files, `node_modules`, etc.)
- Binary file detection and truncation handling
- Homebrew tap and cargo-dist cross-platform binary releases

[0.6.0]: https://github.com/mikelane/git-prism/compare/v0.5.0...v0.6.0
[0.5.0]: https://github.com/mikelane/git-prism/compare/v0.4.0...v0.5.0
[0.4.0]: https://github.com/mikelane/git-prism/compare/v0.3.1...v0.4.0
[0.3.1]: https://github.com/mikelane/git-prism/compare/v0.3.0...v0.3.1
[0.3.0]: https://github.com/mikelane/git-prism/compare/v0.2.0...v0.3.0
[0.2.0]: https://github.com/mikelane/git-prism/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/mikelane/git-prism/releases/tag/v0.1.0
