# Project: git-prism — Function Context Epic (#113)

You're picking up development on mikelane/git-prism, an agent-optimized git data MCP server in Rust. The project is at v0.5.0 with 445 tests, 13 language analyzers, cursor-based pagination, OpenTelemetry observability, content-aware function diffs, and mutation testing CI.

## Codebase orientation

Read CLAUDE.md first — it has build commands, conventions, module responsibilities, and the TDD mandate. Key files:

- `src/treesitter/mod.rs` — `LanguageAnalyzer` trait, `Function` struct (name, signature, start_line, end_line, body_hash), `body_hash_for_node()` helper, `sha256_hex()` helper, `analyzer_for_extension()` registry
- `src/treesitter/<lang>.rs` — 13 analyzer implementations (rust_lang, python, go, typescript, java, php, swift, kotlin, c_lang, cpp, csharp, ruby). Each implements `extract_functions()` and `extract_imports()`.
- `src/tools/manifest.rs` — `build_manifest()`, `build_worktree_manifest()`, `diff_functions()` (body-hash-based comparison with rename detection), `diff_imports()`
- `src/tools/types.rs` — `FunctionChange` (with `from_function()` constructor), `FunctionChangeType` enum (Added, Modified, Deleted, SignatureChanged, Renamed), `ManifestFileEntry`, all MCP arg/response types
- `src/tools/history.rs` — `build_history()` for per-commit manifests
- `src/tools/snapshots.rs` — `build_snapshots()` for file content
- `src/server.rs` — MCP server with `#[tool_router]` and `#[tool]` macros. Three tools: `get_change_manifest`, `get_file_snapshots`, `get_commit_history`
- `src/main.rs` — CLI wiring (clap). Subcommands: serve, manifest, snapshot, history, languages
- `src/pagination.rs` — cursor encoding, validation, clamping
- `src/telemetry.rs` — OTLP exporter, metrics, traces (opt-in via env vars)
- `src/metrics.rs` — OpenTelemetry instrument helpers
- `src/privacy.rs` — repo path hashing, ref pattern normalization
- `src/git/reader.rs` — `RepoReader` wrapping gix
- `src/git/diff.rs` — `FileChange`, `ChangeType`, `ChangeScope`
- `src/git/worktree.rs` — working tree diff
- `bdd/` — Behave (Python) BDD framework. 14 feature files, 51 passing scenarios, 268 steps
- `docs/specs/` — design specs (date-prefixed markdown)
- `docs/decisions/` — ADRs (numbered, 0001-0004 exist)

## The epic

Epic #113: Function context — callers, callees, and test references for changed functions.

New MCP tool `get_function_context` that returns who calls the changed functions, what they call, and which test files reference them. Uses tree-sitter to extract call sites from ASTs (same approach as existing function/import extraction).

### Child issues in dependency order

    #114 (spike: validate call extraction across grammars)
      └→ #115 (BDD bootstrap)
          └→ #116 (CallSite type + extract_calls() trait method)
              ├→ #117 (extract_calls: Rust, Python, Go, TS/JS)
              ├→ #118 (extract_calls: Java, C, C++, PHP)
              └→ #119 (extract_calls: C#, Ruby, Swift, Kotlin)
                  └→ #120 (get_function_context tool handler)
                      └→ #121 (MCP + CLI wiring)
                          ├→ #122 (telemetry)
                          └→ #123 (documentation)
                              └→ #124 (capstone demo + release v0.6.0)

Read each issue on GitHub before starting it. They have detailed scope and acceptance criteria.

## Epic workflow — strict ordering

### 1. Spike (#114)

Write throwaway prototype code to validate that tree-sitter call expression node traversal works across at least 4 languages (Rust, Python, Go, TypeScript). Answer the questions in the issue. The output is ADR `docs/decisions/0005-function-context-spike.md`. The spike code is throwaway — delete it after the ADR is written. Everything else in the epic is blocked on this.

### 2. BDD bootstrap (#115)

Take what you learned from the spike and write Gherkin scenarios in `bdd/features/function_context.feature`. Rules:

- **Black-box only.** Scenarios describe user-visible behavior. No mention of tree-sitter, AST nodes, call_expression, body_hash, or any implementation detail.
- **High level and user focused.** "When the agent requests function context" not "when extract_calls parses the AST."
- **Tag every scenario** with `@not_implemented` and `@ISSUE-NNN` (the issue number of whichever implementation issue will make it pass).
- **Write meaningfully failing step definitions.** Steps must execute and FAIL with assertion errors that describe what's wrong (e.g., "Expected callers list to be non-empty, got []"). NOT undefined steps.
- **Follow Gherkin best practices.** Use Rules to group related scenarios. Use Background for shared setup. Keep scenarios independent.

### 3. Implementation issues (#116-#123)

Each implementation issue owns a slice of the Gherkin scenarios. The workflow for each:

1. **Remove `@not_implemented`** from that issue's Gherkin scenarios
2. **Run the scenarios and confirm they FAIL** (RED) — this proves the scenarios were meaningful
3. **Implement with strict TDD**: Red → Green → Triangulate → Refactor. Write a failing unit test before writing production code. Every single time.
4. **Run the full gauntlet** before marking the issue done (see below)

### 4. Capstone demo (#124)

Blocked on everything else. Narrated walkthrough showing the full manifest → context → snapshots agent workflow. Includes version bump to v0.6.0 and release.

## Build and test commands

    cargo clippy -- -D warnings   # lint — warnings are errors
    cargo fmt --check              # format check
    cargo test                     # unit + integration tests (445 currently)
    cargo build --release          # release build (needed for BDD)
    python -m behave bdd/features/ --no-capture --tags="not @crates_io"  # BDD suite (51 scenarios)

## PR gauntlet review process

Every PR gets a full gauntlet review before merge. Never skip this. The gauntlet has three phases:

### Phase 1: Bug hunt

You are a Sr. Software Engineer. Your job is to find bugs that tests missed.

1. Read every line of the diff carefully. Think about edge cases, off-by-one errors, race conditions, non-deterministic behavior, error handling gaps.
2. For every suspected bug, **write a failing test that proves it**. If you can't write a failing test, you haven't proven the bug is real.
3. Fix every confirmed bug. Verify the failing test now passes.
4. Repeat until you can't find any more bugs.

### Phase 2: Code smell audit

Systematically check for:

- **Duplicate Code** — identical or near-identical blocks (the #1 priority)
- **Long Method** — functions too large to understand at a glance
- **Primitive Obsession** — String/int where a newtype would be clearer
- **Inconsistent style** — e.g., full-path std::collections::HashSet in one place, imported HashSet in another
- **Missing test assertions** — tests that pass but don't verify the important invariant
- **Misleading names** — test or function names that describe old behavior
- **Dead code** — unused imports, unreachable branches
- **Stale tags** — `@not_implemented` on passing scenarios

Fix what you find. Do a second pass after fixing to make sure you didn't introduce new smells.

### Phase 3: Church of Clean Code (Rust purist review)

Run the five specialist checks against the PR's production code:

1. **Panic Exorcist** — no `.unwrap()`, `.expect()`, `panic!()`, `todo!()` in non-test code. `.unwrap_or()`, `.ok()`, `?` operator are fine.
2. **Type Hierophant** — String where &str would suffice? Missing Debug derives on public types? God traits with too many methods?
3. **Borrow Inquisitor** — unnecessary `.clone()` calls? Ownership patterns that could use references?
4. **Undefined Behavior Sentinel** — any `unsafe` blocks? Do they have `// SAFETY:` comments with invariant proofs?
5. **Fearless Concurrency Apostle** — any blocking calls in async context? (Probably N/A — the tool handlers use `spawn_blocking`.)

### After all three phases

Run the full gauntlet one final time:

    cargo fmt --check
    cargo clippy -- -D warnings
    cargo test
    cargo build --release
    python -m behave bdd/features/ --no-capture --tags="not @crates_io"

Everything must be green. Then create the PR.

## Important rules

- **Never guess. Verify before claiming anything.** If you're not sure whether tree-sitter exposes a `call_expression` node for a language, write a test.
- **TDD is mandatory.** Red → Green → Triangulate → Refactor. No production code without a failing test.
- **Tests must be hermetic and deterministic.** No flaky tests. Integration tests build real git repos in temp dirs.
- **No AI slop in docs.** Write like a human engineer. No marketing language.
- **The sandbox has a git commit signing hook that breaks test repos.** Run `git config --global commit.gpgsign false` at the start of every session.
- **Production code uses gix only** — never shell out to `git` CLI. Test helpers may use `git` CLI for repo setup.
- **Tree-sitter nullability convention:** `functions_changed` is `null` (not empty array) when no grammar exists. Apply the same convention to call data — `null` means "no grammar", `[]` means "analyzed, nothing found."
- **All public types** derive `Serialize` and `schemars::JsonSchema` for MCP tool schemas.
- **Function diffing is content-aware.** `diff_functions()` compares by body hash (SHA-256), not line position. Moved-but-unchanged functions are suppressed. Renames detected by matching body hashes. Use `body_hash_for_node()` in analyzers, `FunctionChange::from_function()` to construct changes.

## Git workflow

- **Develop on feature branches.** Branch naming: `claude/<feature-slug>`
- **Commit messages:** Conventional commits (`feat:`, `fix:`, `test:`, `docs:`, `chore:`, `refactor:`)
- **Push with:** `git push -u origin <branch-name>`. Retry up to 4 times with exponential backoff on network failure.
- **Do NOT create PRs unless explicitly asked.** Do NOT merge PRs unless explicitly asked.
- **Pre-push hook (lefthook):** runs fmt, clippy, and test before every push. Never use `--no-verify`.

## Key dependencies

- **rmcp 1.3** — MCP SDK. `#[tool_router]` and `#[tool]` proc macros. Stdio transport.
- **gix 0.81** — Pure Rust git. Features: basic, blob-diff, sha1, status.
- **tree-sitter 0.26** — Native Rust AST parsing. 13 grammar crates.
- **sha2 0.10** — SHA-256 for body hashing and privacy.
- **clap 4** — CLI with derive API.
- **tracing + opentelemetry** — structured logging and OTLP export.
- **thiserror** — library error types. **anyhow** — app-level errors in main.rs.
- **insta** — snapshot tests. **tempfile** — test fixture temp dirs.

## What success looks like

When this epic is done, an agent reviewing a PR can make three calls:

1. `get_change_manifest` — "these 5 functions changed"
2. `get_function_context` — "function X is called by 3 production files and 2 test files; it calls functions Y and Z"
3. `get_file_snapshots` — deep dive into the highest-impact callers

The agent never has to grep through files to find callers or guess which tests to check. The blast radius is structured data.
