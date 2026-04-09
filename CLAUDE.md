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

- **TDD is mandatory.** Red-green-refactor. Write a failing test before writing production code.
- **Error handling:** `thiserror` for library error types in modules, `anyhow` for application-level errors in `main.rs`.
- **Snapshot tests:** Use `insta` crate. Snapshot files live next to the source in `snapshots/` directories. Update with `cargo insta review`.
- **Integration tests:** Build real git repos in temp dirs. Test helpers may use `git` CLI for repo setup (gix's write API is impractical for test fixtures). Production code must use `gix` only — never shell out to `git` CLI in non-test code.
- **Tree-sitter nullability:** `functions_changed` is `null` (not empty array) when no grammar exists for a language. `None` in Rust → `null` in JSON. The distinction matters.
- **Function diffing is content-aware.** `diff_functions()` compares functions by body hash (SHA-256), not line position. Moved-but-unchanged functions are suppressed. Renames detected by matching unmatched deleted/added pairs with identical body hashes. Use `body_hash_for_node()` in analyzers, `FunctionChange::from_function()` to construct changes.
- **Call extraction:** `extract_calls()` on `LanguageAnalyzer` returns `Vec<CallSite>` with callee name, line number, method-call flag, and optional receiver. Each language has its own call node kinds (Rust: `call_expression`+`macro_invocation`, Python: `call`, Go/TS/C/C++: `call_expression`, Java: `method_invocation`, PHP: `function_call_expression`+`member_call_expression`, C#: `invocation_expression`, Ruby: `call`, Swift/Kotlin: `call_expression`+`navigation_expression`). Caller matching uses leaf name comparison.
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

## Git Hooks (lefthook)

A pre-push hook runs `fmt --check`, `clippy`, and `test` before every push. Managed by [lefthook](https://github.com/evilmartians/lefthook) via `lefthook.yml`. After cloning:
```bash
lefthook install
```
Never use `--no-verify` to skip hooks.
