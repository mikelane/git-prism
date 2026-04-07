# ADR 0003: Pagination Spike — Cursor-Based Pagination Feasibility

- **Status**: Accepted
- **Date**: 2026-04-07
- **Spike issue**: #71
- **Design spec**: `docs/specs/2026-04-07-cursor-pagination-design.md`

## Context

The cursor-based pagination design proposes adding optional `cursor` and `page_size` parameters to `get_change_manifest` and `get_commit_history`. Before implementing, we needed to validate four technical assumptions that the design depends on.

## Questions and Findings

### 1. Can we add optional params to existing rmcp `#[tool]` args?

**Validated.** Added `cursor: Option<String>` and `page_size: usize` (with `#[serde(default = "default_page_size")]`) to `ManifestArgs`. The existing test `manifest_args_deserializes_with_defaults` continued to pass — callers that omit the new fields get `cursor: None` and `page_size: 100`.

This works because rmcp's `#[tool]` macro generates a JSON Schema from the `JsonSchema` derive, and serde's `default` attribute handles missing fields during deserialization. No breaking change to existing callers.

### 2. Does a base64 cursor round-trip through JSON-RPC?

**Validated.** A cursor struct (`{v, offset, base_sha, head_sha}`) serialized to JSON, base64-encoded, wrapped in a JSON-RPC string value, unwrapped, base64-decoded, and deserialized back to the original struct. No escaping issues — base64's alphabet (`A-Za-z0-9+/=`) is JSON-safe.

### 3. Can we re-resolve SHAs stored in cursors?

**Validated.** `RepoReader::resolve_commit()` already accepts full SHA strings (40 hex chars). The existing test `it_resolves_full_sha` confirms this. Cursors will store resolved SHAs from the first call; subsequent calls re-resolve and compare to detect repo changes between pages.

### 4. Is gix diff fast enough to repeat per page?

**Validated.** Benchmarked on a synthetic repo with 500 changed files:

| Run | Time |
|-----|------|
| Cold (first compile) | 6.3s |
| Warm (subsequent) | 0.24s |
| Warm (third) | 0.25s |

**~250ms for 500 files** is negligible. The design's Option A (recompute full diff on each page) is confirmed viable. No need for the more complex Option B (cache summary in cursor).

## Decision

Proceed with the design as specified. No changes needed based on spike findings:

- Optional params with serde defaults are backward compatible.
- Base64 cursors are JSON-RPC safe.
- SHA re-resolution works for cursor consistency checks.
- Per-page diff recomputation is fast enough — Option A (simple) over Option B (cached).

## Dependencies

Add `base64 = "0.22"` as a direct dependency (currently only transitive via opentelemetry).

## Consequences

- The implementation can proceed as designed without architectural changes.
- The spike branch (`spike/pagination-prototype`) contains throwaway code and should be deleted.
- Tree-sitter analysis per page (not upfront) remains the correct approach — the diff is cheap, the analysis is the expensive part.
