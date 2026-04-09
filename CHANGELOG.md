# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.5.0] — 2026-04-09

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

Function-level analysis now covers 13 languages (was 8). git-prism supports the top 13 most popular languages on GitHub.

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
- ADRs: mutation testing baseline (#0002), pagination spike (#0003)
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

[0.4.0]: https://github.com/mikelane/git-prism/compare/v0.3.1...v0.4.0
[0.3.1]: https://github.com/mikelane/git-prism/compare/v0.3.0...v0.3.1
[0.3.0]: https://github.com/mikelane/git-prism/compare/v0.2.0...v0.3.0
[0.2.0]: https://github.com/mikelane/git-prism/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/mikelane/git-prism/releases/tag/v0.1.0
