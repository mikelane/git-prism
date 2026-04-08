# Epic: Content-Aware Function Diffs

> Draft for GitHub issue creation. Delete after issues are created.

## Epic Issue

**Title:** Content-aware function diffs — body hashing, rename detection, moved-function suppression

**Labels:** `epic`, `enhancement`

**Body:**

### Problem

`diff_functions()` compares functions by name and line position, producing three classes of wrong results:

1. **False positives on reorder** — moved-but-unchanged functions reported as `Modified`
2. **Missed body-only changes** — function body changes not detected when line count is stable
3. **Lossy renames** — renamed functions appear as `Deleted` + `Added` instead of `Renamed`

### Solution

Add `body_hash` (SHA-256 of raw body bytes) to the `Function` struct. Rewrite `diff_functions()` to compare by body hash instead of line position. Detect renames by matching unmatched deleted/added functions with identical body hashes.

### Design spec

`docs/specs/2026-04-08-smarter-function-diffs.md`

### ADR

`docs/decisions/0004-content-aware-function-diff-spike.md`

### Child issues

1. [ ] #TBD — Add `body_hash` field to `Function` struct and `sha256_hex` helper
2. [ ] #TBD — Update all 13 language analyzers to compute `body_hash`
3. [ ] #TBD — Add `Renamed` variant to `FunctionChangeType` and `old_name` to `FunctionChange`
4. [ ] #TBD — Rewrite `diff_functions()` with body-hash-based algorithm
5. [ ] #TBD — Integration tests (real git repos: reorder, rename, body-only change)
6. [ ] #TBD — BDD scenarios for content-aware diffs
7. [ ] #TBD — Documentation and CLAUDE.md updates
8. [ ] #TBD — Release v0.5.0

---

## Child Issue 1: Add `body_hash` to `Function` struct

**Title:** feat: add `body_hash` field to `Function` and `sha256_hex` helper

**Labels:** `enhancement`

**Body:**

Add a `body_hash: String` field to `src/treesitter/mod.rs::Function`. The field holds a hex-encoded SHA-256 hash of the function body bytes. Mark it `#[serde(skip)]` — it's internal plumbing, not JSON output.

Add `pub fn sha256_hex(bytes: &[u8]) -> String` helper to `src/treesitter/mod.rs`.

### Acceptance criteria

- `Function` struct has `body_hash: String` field
- `sha256_hex` returns 64-char lowercase hex string
- `body_hash` is excluded from JSON serialization
- Existing tests still pass (body_hash populated with empty string or default for now)

---

## Child Issue 2: Update all 13 analyzers to compute `body_hash`

**Title:** feat: compute `body_hash` in all 13 language analyzers

**Labels:** `enhancement`

**Body:**

Each analyzer's function extraction code must compute `body_hash` using the tree-sitter body node byte range:

```rust
let body_node = child.child_by_field_name("body").unwrap_or(child);
let body_bytes = &source[body_node.start_byte()..body_node.end_byte()];
let body_hash = sha256_hex(body_bytes);
```

Kotlin needs its existing fallback (search for `function_body` by kind).

### Acceptance criteria

- All 13 analyzers populate `body_hash`
- Each analyzer has a `body_hash_is_deterministic` test
- Each analyzer has a `body_hash_changes_when_body_changes` test

---

## Child Issue 3: Add `Renamed` variant and `old_name` field

**Title:** feat: add `FunctionChangeType::Renamed` and `FunctionChange.old_name`

**Labels:** `enhancement`

**Body:**

Update `src/tools/types.rs`:

- Add `Renamed` to `FunctionChangeType` enum (serializes as `"renamed"`)
- Add `pub old_name: Option<String>` to `FunctionChange` (null for non-renames)

### Acceptance criteria

- `Renamed` variant serializes as `"renamed"` in JSON
- `old_name` is `null` when change_type is not `Renamed`
- Serialization round-trip tests pass

---

## Child Issue 4: Rewrite `diff_functions()` with body-hash algorithm

**Title:** feat: rewrite `diff_functions()` to use body hashes for comparison and rename detection

**Labels:** `enhancement`

**Body:**

Replace the line-position comparison in `diff_functions()` with body-hash comparison. Implement rename detection by matching unmatched deleted/added functions with identical body hashes.

See design spec algorithm (steps 1-6).

### Acceptance criteria

- Moved-but-unchanged functions produce no `FunctionChange`
- Body-only changes produce `Modified`
- Renamed functions produce `Renamed` with `old_name` populated
- Rename + body change produces `Deleted` + `Added`
- Swapped functions produce no changes
- Multiple renames detected correctly
- All scenarios from the design spec test table pass

---

## Child Issue 5: Integration tests

**Title:** test: integration tests for content-aware function diffs

**Labels:** `test`

**Body:**

Build real git repos in temp dirs that exercise:

1. A commit that reorders functions — manifest shows zero function changes
2. A commit that renames a function — manifest shows one `Renamed` change
3. A commit that modifies only a function body — manifest shows one `Modified` change
4. A commit that renames and modifies — manifest shows `Deleted` + `Added`

---

## Child Issue 6: BDD scenarios

**Title:** test: BDD scenarios for content-aware diffs

**Labels:** `test`, `bdd`

**Body:**

New feature file `bdd/features/content_aware_diffs.feature` with scenarios covering reorder, rename, and body-only change from the CLI perspective. Initially tagged `@not_implemented`.

---

## Child Issue 7: Documentation

**Title:** docs: update CLAUDE.md and README for content-aware diffs

**Labels:** `documentation`

---

## Child Issue 8: Release v0.5.0

**Title:** chore: release v0.5.0

**Labels:** `release`
