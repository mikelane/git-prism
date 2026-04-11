# tree-sitter-kotlin (vendored)

This directory contains a vendored copy of the [tree-sitter-kotlin](https://github.com/fwcd/tree-sitter-kotlin) grammar, used by git-prism's Kotlin analyzer (`src/treesitter/kotlin.rs`).

## Provenance

| Field | Value |
|-------|-------|
| Upstream | https://github.com/fwcd/tree-sitter-kotlin |
| Version | 0.3.5 |
| Commit | `fa05943f392d30355988b1dfc5ede191dd1f5cb8` |
| License | MIT (see [LICENSE](LICENSE)) |
| Copyright | © 2019 fwcd |
| Date vendored | 2026-04-08 (git-prism commit `5e09f2c`, PR #96) |

## Why vendored

The tree-sitter-kotlin grammar is not published as a Rust crate at `crates.io`, so git-prism cannot depend on it directly via `Cargo.toml`. The C parser source and headers needed to compile the grammar are vendored here and built by `build.rs` using the `cc` crate. This keeps the build reproducible and eliminates a runtime dependency on network fetches during `cargo build`.

## Vendored files

Only the C parser sources and headers needed to compile the grammar are vendored:

- `src/parser.c` — generated parser
- `src/scanner.c` — custom scanner for layout-sensitive tokens
- `src/tree_sitter/*.h` — headers from the tree-sitter runtime
- `src/node-types.json` — node type metadata

## Updating

To bump the vendored version:

1. Clone `fwcd/tree-sitter-kotlin` at the desired tag
2. Copy the files listed above into this directory
3. Update the table above with the new version, commit SHA, and date
4. Fetch the upstream `LICENSE` and update the footer of `LICENSE` in this directory to match the new commit
5. Run `cargo clean && cargo build --release` and verify all Kotlin tests in `src/treesitter/kotlin.rs` still pass
