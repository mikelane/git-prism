# ADR 0002: Mutation Testing Baseline

- **Status**: Accepted
- **Date**: 2026-04-06
- **Epic**: #44 (Mutation testing with 90% gate)

## Context

We want to quantify how effectively git-prism's test suite catches regressions, not just whether tests pass. [cargo-mutants](https://mutants.rs/) systematically modifies production code and checks whether the test suite detects each change. A high mutation score means the tests actually verify behavior, not just exercise code paths.

## Decision

Adopt cargo-mutants with a 90% mutation score gate on non-excluded code.

### Configuration

Config lives at `.cargo/mutants.toml`.

**Scope**: `src/**/*.rs` (production code only).

**Excluded files** (48 mutants, ~9% of total):

| File | Mutants | Reason |
|------|---------|--------|
| `src/main.rs` | 3 | CLI entry point — requires process-level E2E testing |
| `src/server.rs` | 25 | MCP transport wiring + observability plumbing — business logic tested via `tools/` |
| `src/telemetry.rs` | 5 | OTel initialization — tested via BDD with mock OTLP collector |
| `src/metrics.rs` | 15 | OTel instrument helpers — no-op without live exporter |

**In scope**: 460 mutants across git operations, tree-sitter analyzers, tool logic, dependency parsing, privacy helpers, and type utilities.

### Baseline Score

**To be filled after first CI run.**

| Metric | Value |
|--------|-------|
| Total mutants generated | 460 |
| Mutants killed (caught) | TBD |
| Mutants survived (missed) | TBD |
| Timeouts | TBD |
| Unviable | TBD |
| **Mutation score** | **TBD%** |

### Top Unkilled-Mutant Categories (TBD)

Will be populated after the first full run. Expected high-survivor areas based on code analysis:

1. **Tree-sitter analyzers** (~193 mutants) — string matching on AST node types; tests use known snippets but may not cover all edge mutations
2. **Dependency file parsing** (72 mutants) — TOML/JSON parsing with many branch points
3. **Privacy helpers** (25 mutants) — simple heuristic functions with edge cases

### Plan to Reach 90%

1. Run baseline in CI (#54)
2. Categorize surviving mutants into:
   - **Real gaps** — write tests to kill them
   - **Genuinely untestable** — add `#[mutants::skip]` with a justifying comment
   - **Low-value cosmetic** — exclude in config with justification
3. Iterate until score ≥ 90% (#53)

## Consequences

- Every PR must maintain ≥90% mutation score on changed files (incremental mode)
- Full suite runs on main merges and weekly
- `#[mutants::skip]` annotations require a comment explaining why
- Developers should run `cargo mutants --in-diff HEAD~1` locally before pushing
