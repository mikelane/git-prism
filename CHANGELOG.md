# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

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

[0.2.0]: https://github.com/mikelane/git-prism/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/mikelane/git-prism/releases/tag/v0.1.0
