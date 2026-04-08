# ADR 0004: Content-Aware Function Diff Spike — Body Hash Feasibility

- **Status**: Accepted
- **Date**: 2026-04-08
- **Spike issue**: TBD
- **Design spec**: `docs/specs/2026-04-08-smarter-function-diffs.md`

## Context

The design spec proposes adding a `body_hash` field to `Function`, computed by SHA-256 hashing the raw bytes of each function's body node as reported by tree-sitter. Before implementing across all 13 analyzers, we needed to validate three assumptions:

1. Does `child_by_field_name("body")` return a usable node for function-like constructs across all supported grammars?
2. Are the body byte ranges deterministic — same source in, same range out?
3. Is the same function body at different file positions hash-equal (position independence)?

## Questions and Findings

### 1. Does `child_by_field_name("body")` work across all grammars?

**Validated for 11 of 13 languages.** Spike tests (`tests/spike_body_hash.rs`) parsed real function snippets and confirmed body node access for: Rust (`function_item`), Python (`function_definition`), Go (`function_declaration`, `method_declaration`), TypeScript (`function_declaration`), JavaScript (`function_declaration`), Java (`method_declaration`), C (`function_definition`), C++ (`function_definition`), PHP (`function_definition`), C# (`method_declaration`), Ruby (`method`).

**Kotlin and Swift** use vendored grammars that aren't directly accessible as `tree_sitter::Language` from integration tests. However, both analyzers already use `child_by_field_name("body")` in their `signature_text()` functions (Kotlin with a fallback to searching for `function_body` by kind), and existing unit tests prove the signature correctly excludes the body. This confirms the body node is accessible.

**C forward declarations** (`int add(int a, int b);`) correctly produce no body node — the node kind is `declaration`, not `function_definition`. Production code should fall back to hashing the full node range for bodyless constructs.

### 2. Are body byte ranges deterministic?

**Validated.** Parsing the same source twice produces identical body node byte ranges. SHA-256 of those ranges produces identical hex digests. This is expected — tree-sitter is a deterministic parser — but worth confirming since the hash becomes part of the diff algorithm's correctness.

### 3. Is the hash position-independent?

**Validated.** The function `fn foo() { println!("hello"); }` produces the same body hash whether it appears at line 1 or line 3 (pushed down by a preceding function). This is the core property that eliminates false "Modified" reports on reordered functions.

The body node bytes `{ println!("hello"); }` are identical regardless of where the function appears in the file. The byte content doesn't include any position information — it's just the raw source text of the body.

### 4. Kotlin's fallback mechanism

The Kotlin grammar doesn't expose a `body` field on `function_declaration` nodes. The existing analyzer already handles this by falling back to searching children for a `function_body` kind node. The same fallback applies when computing body hashes. No special case needed in the shared hashing helper — each analyzer is responsible for locating its body node.

## Decision

Proceed with the design as specified. The spike confirms all technical assumptions:

- `child_by_field_name("body")` is the right approach for 12 of 13 languages.
- Kotlin needs its existing fallback (search by kind for `function_body`), which already works.
- Bodyless constructs (declarations, abstract methods) return `None` for the body node. Production code should hash the full node range as a fallback, ensuring these functions still get a hash for comparison.
- SHA-256 of raw body bytes is deterministic and position-independent.
- No normalization needed — raw bytes are sufficient.

## Dependencies

`sha2 = "0.10"` is already a direct dependency (used in `src/privacy.rs`). No new dependencies needed.

## Consequences

- The implementation can proceed exactly as the design spec describes.
- Each analyzer's `extract_functions` implementation needs a mechanical change: compute body hash alongside the existing fields.
- A shared `sha256_hex(bytes: &[u8]) -> String` helper in `src/treesitter/mod.rs` avoids duplicating the hash logic.
- The spike test file (`tests/spike_body_hash.rs`) should be deleted once the real implementation is in place.
