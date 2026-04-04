# git-prism

Agent-optimized git data MCP server. Provides structured change manifests and full file snapshots instead of human-oriented diffs.

## Architecture

- **`src/main.rs`** — CLI entry point (clap). Subcommands: `serve`, `manifest`, `snapshot`, `languages`, `version`.
- **`src/server.rs`** — MCP server setup (rmcp, stdio transport).
- **`src/tools/`** — MCP tool implementations. `manifest.rs` (get_change_manifest), `snapshots.rs` (get_file_snapshots), `types.rs` (shared response structs).
- **`src/git/`** — Git data access layer (gix). `reader.rs` (open repo, resolve refs), `diff.rs` (structured file-level diffs), `depfiles.rs` (parse Cargo.toml/package.json/go.mod/pyproject.toml), `generated.rs` (heuristic detection of generated files).
- **`src/treesitter/`** — Function-level analysis. `mod.rs` (LanguageAnalyzer trait + registry), per-language analyzers (go.rs, python.rs, typescript.rs, rust_lang.rs).

## Conventions

- TDD is mandatory. Red-green-refactor cycle.
- Use `thiserror` for library errors, `anyhow` for application errors in main.
- Snapshot tests via `insta` for tool output schemas.
- Integration tests build real git repos with `gix` in temp dirs — no mocking git.
- Tree-sitter `functions_changed` is `null` (not empty array) when no grammar exists for a language.

## Build & Test

```bash
cargo clippy -- -D warnings
cargo fmt --check
cargo test
cargo build --release
```

## MCP Registration

```bash
claude mcp add git-prism -- git-prism serve
```

## Design Doc

Full JSON schemas for both tools: `/Users/mikelane/dev/git-prism-architecture.md`
