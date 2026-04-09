# Content-Aware Function Diffs

- **Epic**: #100
- **Status**: Draft
- **Date**: 2026-04-08

## Context

`diff_functions()` in `src/tools/manifest.rs` compares functions extracted by tree-sitter between two versions of a file. Today it matches functions by name (HashMap keyed on `Function.name`), then compares signatures and line positions. This produces three classes of wrong results:

1. **False positives on reorder.** A function that moves in a file (reordered, or pushed down by a new function above it) gets flagged as `Modified` because `start_line`/`end_line` changed, even though the function's body is byte-for-byte identical.
2. **Missed body-only changes.** If a function's body changes but its line count stays the same and it doesn't move, the diff reports nothing. The current algorithm has no way to detect body changes — it never looks at the body.
3. **Lossy rename representation.** A renamed function shows as one `Deleted` + one `Added`. The consuming agent has no signal that these two are the same function under a new name.

All three stem from the same root cause: `Function` carries `{name, signature, start_line, end_line}` but discards the function body during tree-sitter extraction. The fix is to carry a hash of the body bytes through the pipeline and use it for equality comparison and rename detection.

## Goals

1. **Eliminate false positives from reordering.** Functions that moved but didn't change should produce no `FunctionChange` entry.
2. **Detect body-only changes.** Functions whose implementation changed (but signature didn't) should be reported as `Modified`.
3. **Detect renames.** When an unmatched deleted function and an unmatched added function have identical body hashes, report a single `Renamed` change instead of separate `Deleted` + `Added`.
4. **Backward-compatible JSON shape.** The `functions_changed` array keeps the same structure. The only additions are a new `old_name` field on `FunctionChange` (null when not a rename) and a new `renamed` variant in `FunctionChangeType`.

## Non-Goals

- Fuzzy rename detection (name changed AND body changed). If both changed, we can't reliably link them; `Deleted` + `Added` is the honest answer.
- Whitespace normalization or comment stripping before hashing. Formatting changes are real changes.
- A new `Moved` change type. Moved-but-unchanged functions are simply filtered out. If there's demand later, we can add it.
- Function body content in the manifest response. The manifest is metadata; agents use `get_file_snapshots` for content.

## Design

### `Function` struct change

Add a `body_hash` field computed during tree-sitter extraction:

```rust
pub struct Function {
    pub name: String,
    pub signature: String,
    pub start_line: usize,
    pub end_line: usize,
    pub body_hash: String,  // hex-encoded SHA-256 of raw body bytes
}
```

The hash covers the full byte range of the function body node as reported by tree-sitter (`body.start_byte()..body.end_byte()`). For functions without an explicit body node (e.g., abstract methods, declarations), the hash covers the entire function node range. Raw bytes, no normalization.

`body_hash` is `#[serde(skip)]` — it's internal plumbing, not part of the JSON output. It doesn't need `JsonSchema`.

### Analyzer changes (all 13 languages)

Each analyzer's `extract_functions_from_node()` (or equivalent) already has access to the source bytes and the tree-sitter node. The change is mechanical: after computing `start_line` and `end_line`, also compute:

```rust
let body_node = child.child_by_field_name("body").unwrap_or(child);
let body_bytes = &source[body_node.start_byte()..body_node.end_byte()];
let body_hash = sha256_hex(body_bytes);
```

A shared helper `sha256_hex(bytes: &[u8]) -> String` lives in `src/treesitter/mod.rs` to avoid duplicating the hashing logic across 13 files.

### `FunctionChangeType` enum update

```rust
pub enum FunctionChangeType {
    Added,
    Modified,        // same name, same signature, different body_hash
    Deleted,
    SignatureChanged, // same name, different signature (body may or may not differ)
    Renamed,         // different name, same body_hash
}
```

### `FunctionChange` struct update

```rust
pub struct FunctionChange {
    pub name: String,
    pub old_name: Option<String>,  // populated only for Renamed
    pub change_type: FunctionChangeType,
    pub start_line: usize,
    pub end_line: usize,
    pub signature: String,
}
```

`old_name` serializes as `null` for non-rename changes.

### Rewritten `diff_functions()` algorithm

```
Input: base_fns: &[Function], head_fns: &[Function]
Output: Vec<FunctionChange>

1. Build base_map: HashMap<&str, &Function>  (keyed by name)
   Build head_map: HashMap<&str, &Function>  (keyed by name)

2. For each name in head_map that also exists in base_map:
   a. If signatures differ → SignatureChanged
   b. Else if body_hashes differ → Modified
   c. Else → no change (skip, even if lines differ — this is the "moved" case)

3. Collect unmatched_added:  names in head_map not in base_map
   Collect unmatched_deleted: names in base_map not in head_map

4. Rename detection:
   Build deleted_by_hash: HashMap<&str, Vec<&Function>>  (body_hash → deleted fns)
   For each added function:
     If deleted_by_hash contains its body_hash:
       Pop one match → emit Renamed { name: added.name, old_name: deleted.name }
       Remove the deleted function from unmatched_deleted
     Else:
       Emit Added

5. Remaining unmatched_deleted → Deleted

6. Sort by name, return.
```

Step 4 handles the case where multiple functions share the same body hash (unlikely but possible — e.g., stub implementations). It matches greedily; the first unmatched deleted function with a matching hash wins.

### Dependency: `sha2` crate

`sha2` is already a transitive dependency (used in `src/privacy.rs` for repo path hashing). Promote it to a direct dependency if it isn't one already, or reuse the existing import path.

### Telemetry

No new metrics or spans. The existing `treesitter.extract_functions` span covers the extraction path. The hashing overhead is negligible (SHA-256 of a few hundred bytes per function).

### Breaking changes

This is a semantic change to the manifest JSON output:

- Functions that merely moved will **stop appearing** in `functions_changed`. Consumers that relied on line-position changes to detect moves will see fewer results.
- Functions with body-only changes will **start appearing** as `Modified`. This is a fix, not a regression — these were previously missed.
- The `renamed` variant is new. Consumers that don't recognize it will see it as an unknown enum value. Since the field is `snake_case` serialized and the struct includes `name` + `signature`, well-behaved consumers should handle it gracefully.

The version bump should be a minor (v0.5.0) since no existing fields are removed and the JSON shape is additive.

## Testing

### Unit tests (`src/tools/manifest.rs`)

Existing `diff_functions` tests will be updated. New cases:

| Scenario | base_fns | head_fns | Expected |
|----------|----------|----------|----------|
| Moved, unchanged body | `[foo@L1-5, hash=A]` | `[foo@L10-14, hash=A]` | No changes |
| Body-only change | `[foo@L1-5, hash=A]` | `[foo@L1-5, hash=B]` | `Modified(foo)` |
| Rename | `[old_name, hash=A]` | `[new_name, hash=A]` | `Renamed(new_name, old_name=old_name)` |
| Rename + body change | `[old_name, hash=A]` | `[new_name, hash=B]` | `Deleted(old_name) + Added(new_name)` |
| Swap two functions | `[foo@L1, bar@L10]` | `[bar@L1, foo@L10]` | No changes (both moved, bodies same) |
| Signature change | `[foo(i32), hash=A]` | `[foo(i64), hash=B]` | `SignatureChanged(foo)` |
| Multiple renames | `[a, hash=X], [b, hash=Y]` | `[c, hash=X], [d, hash=Y]` | `Renamed(c, old=a), Renamed(d, old=b)` |
| Duplicate body hashes | `[a, hash=X], [b, hash=X]` | `[c, hash=X]` | Greedy: `Renamed(c, old=a), Deleted(b)` or `Renamed(c, old=b), Deleted(a)` |

### Unit tests (`src/treesitter/*.rs`)

Each analyzer gets a test verifying that `body_hash` is populated and deterministic:

```rust
#[test]
fn body_hash_is_deterministic() {
    let source = b"fn hello() { println!(\"hi\"); }";
    let fns1 = RustAnalyzer.extract_functions(source).unwrap();
    let fns2 = RustAnalyzer.extract_functions(source).unwrap();
    assert_eq!(fns1[0].body_hash, fns2[0].body_hash);
    assert!(!fns1[0].body_hash.is_empty());
}

#[test]
fn body_hash_changes_when_body_changes() {
    let v1 = b"fn hello() { println!(\"hi\"); }";
    let v2 = b"fn hello() { println!(\"bye\"); }";
    let fns1 = RustAnalyzer.extract_functions(v1).unwrap();
    let fns2 = RustAnalyzer.extract_functions(v2).unwrap();
    assert_ne!(fns1[0].body_hash, fns2[0].body_hash);
}
```

### Integration tests

Build a real git repo with:
- A commit that reorders functions → manifest should show zero function changes.
- A commit that renames a function → manifest should show one `Renamed` change.
- A commit that modifies only function body → manifest should show one `Modified` change.

### BDD scenarios

New feature file `bdd/features/content_aware_diffs.feature` covering the three headline scenarios (reorder, rename, body change) from the CLI perspective.

## Rollout

1. **Spike PR** — validate that tree-sitter body node ranges are reliable across all 13 languages. Produce ADR 0004.
2. **BDD bootstrap** — Gherkin scenarios with `@not_implemented` tags.
3. **`Function` struct + `sha256_hex` helper** — add `body_hash` field and hashing utility. Update all 13 analyzers. Unit tests for determinism.
4. **Rewrite `diff_functions()`** — new algorithm using body hashes. Unit tests for all scenarios in the table above.
5. **`FunctionChangeType::Renamed` + `FunctionChange.old_name`** — type changes, serialization tests.
6. **Integration tests** — real git repos exercising the three headline scenarios.
7. **BDD green** — remove `@not_implemented` tags, verify scenarios pass.
8. **Documentation + CLAUDE.md update.**
9. **Release v0.5.0.**
