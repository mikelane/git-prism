# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Fixed

- **`git-prism shim install` now creates a stable symlink target on Homebrew installs.** Previously `canonicalize()` resolved the full symlink chain, baking the version-pinned Cellar path (`/opt/homebrew/Cellar/git-prism/<version>/bin/git-prism`) into the shim. Every `brew upgrade` moved the binary to a new Cellar directory and GC'd the old one, leaving the shim pointing at a deleted file. The install logic now detects a Homebrew Cellar layout by looking for a `Cellar` component in the canonical exe path and maps it to `<prefix>/bin/git-prism` — the stable symlink Homebrew maintains across upgrades. Non-Homebrew installs (cargo, source builds) are unaffected. `git-prism shim status` now includes an advisory warning when the current shim target contains a `Cellar` path component, prompting the user to re-run `git-prism shim install`. (#343)

## [0.9.1] — 2026-06-01

### Fixed

- **Annotated tags now peel to their target commit during ref resolution.** `get_change_manifest`, `get_commit_history`, `get_file_snapshots`, and the shim's `git diff` / `git log` / `git show` interception previously failed on annotated-tag refs with the error `was supposed to be of kind commit, but was kind tag`. Ref resolution now peels tag objects to their target commit, so diffing or showing tagged releases (e.g. `v0.8.0..v0.9.0`) works. (#337)
- **Shim now passes through scripted-output git invocations.** When `git show`, `git log`, or `git diff` is called with flags that request formatted text output (`--format=`, `--pretty=`, `--pretty`, `-s`, `--no-patch`, `--porcelain`, `--stat`, `-z`), the shim forwards the call to real git instead of returning a JSON manifest. This fixes `git show -s --format=%ct HEAD` silently returning a change-manifest JSON payload instead of the expected epoch integer. (#338)

### Added

- **Enriched `git show <ref>` response.** The shim's `git show <sha>` handler now returns a `ShowManifestResponse` with structured commit metadata (`commit.sha`, `commit.short_sha`, `commit.parents`, `commit.author`, `commit.committer`, `commit.subject`, `commit.body`) and a top-level `diffstat` object (`files_changed`, `insertions`, `deletions`). JSON-aware callers no longer need to parse git text output to get per-commit author, timestamp, or diffstat data. (#338)

## [0.9.0] — 2026-05-31

### Breaking Changes

- **`git-prism hooks install` (without `--path-shim`) now exits non-zero.** The bundled redirect hook (`bash_redirect_hook.py` / `git-prism-redirect.sh`) was removed. Running `git-prism hooks install` without `--path-shim` prints a migration message to stderr and exits 1.

  **Migration:** use the PATH shim instead:

  ```sh
  # Remove any previously installed redirect hook entry first:
  git-prism hooks uninstall --scope user   # or --scope project / local

  # Install the PATH shim:
  git-prism shim install
  ```

  The PATH shim intercepts git at the PATH layer and is a strict superset of the redirect hook's coverage. See `docs/decisions/0011-redirect-hook-removal.md` for rationale. (#326)

### Added

- **`git-prism shim` subcommand (first-class PATH shim management).** `git-prism shim install`, `git-prism shim uninstall`, and `git-prism shim status` are now first-class top-level subcommands, replacing the `--path-shim` flag under `git-prism hooks`. The shim install logic is unchanged — the symlink goes to `~/.local/share/git-prism/bin/git` — but the entry point is now semantically correct: the shim is a PATH-layer interceptor, not a Claude Code hook. (#324)
- **`git-prism hooks install --path-shim` deprecated alias.** The flag still works with a warning (`warning: --path-shim is deprecated; use \`git-prism shim install\` instead`). Use `git-prism shim install` for new installs. (#324)
- **`gh pr diff` interception.** With a `gh` symlink pointing at git-prism ahead of the real `gh` on `PATH`, the shim recognizes `argv[0] == "gh"` and routes `gh pr diff <number>` through git-prism's structured manifest pipeline — resolving the PR's base..head range via `gh pr view` and returning the same JSON the MCP tools produce. Every other `gh` subcommand passes through to the real `gh` unchanged. (#323)
- **Auto-PATH setup during `git-prism shim install`.** When the shim directory is not already on `PATH`, install prompts for consent and, if accepted, idempotently appends the `export PATH` line to your shell rc (`.zshrc` or `.bashrc`, chosen from `$SHELL`) and reminds you to restart Claude Code so its frozen shell snapshot picks up the new `PATH` (see `docs/decisions/0010-shim-direct-call-interception.md`). Declining prints manual instructions and modifies no files. The append is line-wise idempotent (re-running install does not duplicate the export). (#325)
- **Windows shim passthrough.** On non-Unix platforms the shim now passes git through via spawn-and-wait (forwarding the exit code, inheriting stdio) instead of the Unix `execvp`, so the shim functions on Windows rather than bailing. The `windows-latest` CI job exercises it. The PATH-shim *install* remains Unix-only. (#322)
- **PATH shim (Unix-only install).** Installed as a `git` binary ahead of the real git on `PATH`, git-prism intercepts watch-list subcommands (`diff`/`log`/`show`/`blame`/pickaxe) that carry a ref range when an AI agent is detected, routing them to structured JSON; humans, CI, non-agents, and ref-range-less commands pass through to vanilla git untouched. Ships the Python classifier ported to `src/shim/classify.rs`, `argv[0]`-aware dispatch in `main` (#287), the shim core of `classify`/`real_git`/handlers (#286), and a `git-prism hooks install --path-shim` flag (plus matching `uninstall`/`status`) that creates the `~/.local/share/git-prism/bin/git` symlink and prints the `Created symlink:` line and PATH-export instructions (#288, #302). Agent detection reuses the env-var logic from #280. `GIT_PRISM_INSIDE_SHIM=1` forces passthrough and is set in child processes to break recursion through nested git calls. `GIT_PRISM_DEBUG_RESOLVER=1` prints the resolved real-git path to stderr (#299). (#284)
- **Shim telemetry counters.** `shim_invocations_total` and `shim_classification_total` record how often the shim runs and how each command is classified (intercept vs passthrough). (#289)
- **`git-prism agent-detect` subcommand.** Prints a JSON object indicating whether the current process is running on behalf of an AI coding agent, detected via environment variables only. Detection checks `AI_AGENT` (Vercel cross-tool convention), `AGENT` with an allowlisted value (`goose`, `amp`), and eight tool-specific markers (`CLAUDECODE`, `CURSOR_AGENT`, `GEMINI_CLI`, `CODEX_SANDBOX`, `CLINE_ACTIVE`, `AUGMENT_AGENT`, `OPENCODE_CLIENT`, `TRAE_AI_SHELL_ID`). `CI=true` is a hard override that always returns `{"agent": null, "signal": null}`. Not exposed as an MCP tool — diagnostics/ops use only. (#278)

### Fixed

- **Shim passthrough distinguishes exit 126 from 127.** When delegating to the real git, the shim now returns `126` if the resolved binary exists but is not executable, and `127` if no git binary is found on `PATH`, matching POSIX shell conventions instead of collapsing both into one code. (#296)
- **Windows builds compile cleanly.** The shim's `execvp`-based exec path is now gated behind `#[cfg(unix)]`, so the crate compiles as a no-op on Windows and `hooks install --path-shim` bails with "not supported on non-Unix platforms" rather than failing the build. (#314)

## [0.8.0] — 2026-05-13

### Added

- **Structured diff hunk metadata in `get_file_snapshots`.** Pass `include_diff_hunks: true` to get unified-diff hunk boundaries (`old_start`, `old_lines`, `new_start`, `new_lines`) for each modified file. Enables agents to compute diff-relative line positions for GitHub inline review comments without parsing raw unified diff output. Only emitted for modified files where both before and after content exists. (#271)
- **Actionable resolution field for missing remote branches.** When `get_change_manifest` is called with a branch name that exists as a remote tracking ref (`refs/remotes/origin/{branch}`) but not as a local branch, the `ResolveRef` error now includes a JSON payload with a `resolution` field suggesting `git fetch origin {branch}`. Plain text errors are preserved for SHAs, qualified refs, and branches that do not exist anywhere. (#263)

### Fixed

- **Gitlink (submodule) entries filtered from change manifest output.** Tree-diff handlers for additions, deletions, modifications, and rewrites now check for gitlink mode (`entry_mode.is_commit()`) and skip those entries. Mode transitions (file ↔ submodule at the same path) are handled correctly in both committed and worktree diffs. (#264)
- **Redirect hook now surfaces `review_change` in advice messages.** The `BLOCK_GH_PR_DIFF` and `ADVICE_GET_CHANGE_MANIFEST` constants now include `review_change` as the preferred alternative to `git diff` for PR review. Unicode arrows and em-dashes replaced with ASCII equivalents (`-->`, `--`) for terminal safety. Escaped newlines (`\<newline>`) in bash commands are now stripped before tokenization. (#265)
- **Redirect hook: heredoc false positive for `gh pr diff`.** The bash command tokenizer now applies a regex-based pre-pass to strip heredoc bodies from raw text before the line-by-line shlex tokenizer runs. This prevents false-positive hard-blocks when `gh pr diff` text appears verbatim inside a heredoc body that shlex cannot tokenize (e.g., inside a multi-line double-quoted string). (#270)
- **Redirect hook: `gh api .../contents/...?ref=<sha>` bypass interception.** The hook now detects `gh api repos/<owner>/<repo>/contents/<path>?ref=<sha>` calls and redirects to `get_file_snapshots` with advisory JSON on stdout, closing a gap where agents could fetch raw file content via the GitHub API and bypass git-prism entirely. (#270)

### Dependencies

- `tokio` bumped from 1.51.1 to 1.52.1 (#261)
- `rmcp` bumped from 1.5.0 to 1.6.0 (#262)
- `tree-sitter-c-sharp` bumped from 0.23.1 to 0.23.5 (#260)
- `clap` bumped from 4.6.0 to 4.6.1 (#259)

## [0.7.0] — 2026-04-29

### Breaking Changes

- **`get_change_manifest` default for `include_function_analysis` flipped to `false`.** Function-level diffs are now opt-in, aligning the tool's default with its "cheap first-resort" contract. Pass `include_function_analysis: true` to restore the previous behavior. The CLI adds an `--include-function-analysis` flag with the same effect.
- **`get_change_manifest` enforces a token budget (default 8192).** When the response would exceed the budget, function/import analysis is progressively stripped per file via a three-tier algorithm (full → signatures-only → bare). Trimmed files that preserved their function signatures are listed in `metadata.function_analysis_truncated`. Pass `max_response_tokens: 0` (or the CLI `--max-response-tokens 0`) to disable enforcement. Internal callers (e.g. `get_function_context`) bypass enforcement via `ManifestOptions.max_response_tokens = None`.
- **`record_truncated` metric now carries a `reason` label.** New `reason="token_budget"` events are emitted whenever the manifest budget trims any file detail. Cardinality is bounded via `classify_truncation_reason` in `src/privacy.rs`.

### Changed

- **MCP tool descriptions rewritten with comparative framing vs raw git.** All four tool doc comments in `src/server.rs` now name the raw git command they replace, so agents reading `tools/list` see when to reach for git-prism instead of falling back to `git diff` / `git log` / `git show` / `git log -S`. `get_commit_history` was promoted from a one-line `description = "..."` argument to a full `///` doc block; `get_function_context` gained its first proper doc comment (it previously had none). Snapshot tests under `src/snapshots/` lock the prose against silent drift, and `it_publishes_comparative_framing_for_every_tool` mirrors the `@ISSUE-237` BDD scenario as an in-binary regression net.

### Added

- **`review_change` MCP tool.** New orchestration tool that returns a combined `{ manifest, function_context }` payload for the same ref range in a single call, splitting `max_response_tokens` 40/60 (manifest / function_context). Designed to compete head-to-head with `git diff <ref>..<ref>`: the doc comment leads with "Use this instead of `git diff`" so agents reach for it during PR review and refactor audits. Pure orchestration — internally calls the existing `get_change_manifest` and `get_function_context` handlers, no diff or analysis logic is duplicated. Pagination uses two independent opaque cursors (`manifest_cursor` / `function_context_cursor`) so each half can advance without re-paginating the other. Each sub-response stamps `metadata.budget_tokens` with its share for downstream observability. (#240)
- **`get_function_context` gains pagination, a name filter, and a response-size budget.** Four new `ContextArgs` fields — `cursor`, `page_size` (1–500, default 25), `function_names`, `max_response_tokens` (default 8192, `0` disables) — mirror the manifest tool's guardrails so the second-resort read tool can no longer exceed MCP context limits. The CLI exposes the same knobs: `--cursor`, `--page-size`, `--function-names=a,b`, `--max-response-tokens`.
- **Per-entry `truncated` flag on `FunctionContextEntry`.** When the budget clamps an entry's caller / callee / test-reference lists (top 5 callers, top 5 callees, top 3 test references are kept), the entry's `truncated` flag is set and its name lands in `metadata.function_analysis_truncated`. The flag is also set on the last kept entry when the response was cut short by the budget or page-size, so `function_analysis_truncated` is never empty on a truncated response.
- **`function_names` as the escape hatch for re-querying clamped entries.** Agents that need the full caller / callee list for an entry that was clamped on a prior paginated call should re-request with `function_names: ["name"]` — the filtered response fits comfortably within the budget.
- **Metadata mirrors pagination cursor.** `ContextMetadata.next_cursor` duplicates `pagination.next_cursor` for agents reading only the metadata block.
- **Bounded-cardinality truncation metric.** `get_function_context` now emits `record_truncated(tool, reason)` with `reason="paginated"` when a next-page cursor is returned and `reason="token_budget"` when any entry was clamped, matching the manifest tool's signalling contract.
- **Python bash tokenizer and bundled redirect hook script.** `hooks/bash_redirect_hook.py` uses `shlex.shlex(posix=True, punctuation_chars=True)` to structurally parse bash commands, covering compound (`&&`), subshell `(...)`, pipeline `|`, variable expansion, and heredoc-body suppression. The bundled `hooks/git-prism-redirect.sh` wraps it and implements the three-state exit-code protocol (0 = allow, 0+JSON = advisory, 2 = hard block). Stdlib-only; no third-party parser dependency. Includes a pytest suite under `hooks/tests/`. (#248)
- **`git-prism hooks install/uninstall/status` subcommand group.** New CLI subcommand (`src/hooks.rs`) copies the bundled redirect hook into the target scope's hooks directory and writes a `PreToolUse` entry with a stable sentinel `id: "git-prism-bash-redirect-v1"` into Claude Code's `settings.json`. Default scope is `user` (`~/.claude/settings.json`) to avoid Claude Code issue anthropics/claude-code#13898, which prevents custom subagents from calling project-scoped MCP servers correctly. Re-install is idempotent; `--force` overwrites user-edited entries. `hooks status` prints a table of which scopes have the hook installed and at what version. (#249)
- **ADR 0008: redirect hook architecture.** Documents all six architectural decisions for the redirect-hook epic: bash tokenizer choice (`shlex` over `bashlex` and handwritten parsers), `--scope` semantics mirroring `claude mcp add`, idempotency via sentinel `id` field, BDD testability contract, `--scope user` default to dodge the project-scope subagent MCP bug (#13898), and fail-open behavior when `python3` is missing. (#243)
- **`.claudeignore` to reduce LLM scan noise.** Excludes build artifacts (`target/`), vendored grammars, BDD fixtures, and demo recordings from LLM context windows. (#251)

### Dependencies

- `tree-sitter-c` bumped from 0.23.4 to 0.24.2 (#233)
- `gix` bumped from 0.81.0 to 0.83.0 (0.82.0 was yanked) (#232)
- `sha2` bumped from 0.10.9 to 0.11.0 (#199)

## [0.6.0] — 2026-04-09

> Released 2026-04-10 (retroactively tagged; see ADR 0007 for history).

### Added

- **`get_function_context` tool.** New MCP tool and CLI subcommand (`git-prism context HEAD~1..HEAD`) that returns callers, callees, and test references for each changed function. Agents no longer need to grep through the codebase to find who calls a modified function or which tests cover it.
- **Call extraction across all 13 languages.** New `extract_calls()` method on `LanguageAnalyzer` with language-specific node kinds: Rust (`call_expression` + `macro_invocation`), Python (`call`), Go/TS/JS/C/C++ (`call_expression`), Java (`method_invocation`), PHP (`function_call_expression` + `member_call_expression`), C# (`invocation_expression`), Ruby (`call`), Swift/Kotlin (`call_expression` + `navigation_expression`).
- `CallSite` struct with callee name, line number, method-call flag, and optional receiver.
- `RepoReader::list_files_at_ref()` for walking git trees to discover caller files.
- Test file detection via path conventions (e.g., `/tests/`, `_test.go`, `.test.ts`).
- Tracing spans for context operations: `context.build`, `context.get_manifest`, `context.scan_files`, `context.match_callers`, `context.extract_callees`.
- 15 BDD scenarios for function context (callers, callees, test references, unsupported languages, multi-language extraction, CLI validation).
- ADR 0005: call-site extraction spike findings.
- 51 new unit tests (496 total).

### Changed

- Agent Workflow in README updated from two-step (manifest -> snapshots) to three-step (manifest -> context -> snapshots).
- CLAUDE.md documents call extraction conventions, `extract_calls()` pattern, and `context` subcommand.

## [0.5.0] — 2026-04-09

> Released 2026-04-10 (retroactively tagged; see ADR 0007).

### Added

- **Content-aware function diffs.** `diff_functions()` now compares functions by SHA-256 body hash instead of line position. Three improvements over v0.4:
  - **Reorder suppression** — functions that moved but didn't change no longer produce false `modified` entries.
  - **Body-only detection** — functions whose implementation changed (but signature didn't) are now detected as `modified`, even when line numbers are stable.
  - **Rename detection** — when a deleted function and an added function share the same body hash, they're reported as a single `renamed` entry with `old_name` populated instead of separate `deleted` + `added`.
- New `renamed` variant in `functions_changed[].change_type`.
- New `old_name` field on function change entries (null for non-renames).
- `body_hash_for_node()` helper for tree-sitter analyzers.
- `FunctionChange::from_function()` constructor for building change entries.
- 4 integration tests with real git repos covering reorder, body change, rename, and rename+modify scenarios.
- 5 BDD scenarios for content-aware diffs.

### Changed

- `modified` in `functions_changed` now means the function body changed (was: line positions changed). This is a semantic change — fewer false positives, more true positives.
- All 13 language analyzers compute body hashes during tree-sitter extraction.
- CLAUDE.md and README.md updated with content-aware diffing documentation.

## [0.4.0] — 2026-04-08

### Added

- **PHP** tree-sitter analyzer (`.php`) — functions, class methods, `use` declarations
- **C#** tree-sitter analyzer (`.cs`) — methods, constructors, `using` directives
- **Kotlin** tree-sitter analyzer (`.kt`, `.kts`) — functions, methods, extension functions, imports
- **Ruby** tree-sitter analyzer (`.rb`) — methods, singleton methods, `require`/`require_relative`
- **Swift** tree-sitter analyzer (`.swift`) — functions, methods, init declarations, imports

Function-level analysis now covers 13 languages (was 8). The selection targets widely-used languages on GitHub as of early 2026 (Rust, Go, Python, JavaScript, TypeScript, Java, C, C++, C#, Ruby, Swift, Kotlin, PHP); language priority was chosen from informal GitHub usage signals rather than a formal Octoverse citation.

### Changed

- README languages table updated with all 13 languages
- CLI `languages` command lists all 13 languages

## [0.3.1] — 2026-04-08

### Fixed

- Mutation testing CI: incremental PR check is now informational only (reports score, never blocks). The 90% threshold applies only to the full suite on main. Previously, equivalent mutants in small diffs caused false failures.

### Added

- Python and TypeScript tree-sitter tests for class/method line number accuracy.

## [0.3.0] — 2026-04-08

### Added

- **Cursor-based pagination** for `get_change_manifest` and `get_commit_history`. Large diffs are no longer silently truncated — agents page through results using opaque cursors. New parameters: `cursor` (continuation token) and `page_size` (1-500, default 100).
- **OpenTelemetry observability** — opt-in metrics and traces via `GIT_PRISM_OTLP_ENDPOINT`. 14 metrics (request counts, duration histograms, token estimates, error rates) and per-tool trace trees with sub-spans for gix and tree-sitter operations.
- **Mutation testing CI** — cargo-mutants runs on every PR (incremental) and weekly (full suite) with sharded execution and nextest for faster feedback.
- **CLI auto-pagination** — `manifest` and `history` commands loop through all pages internally and output complete results. New `--page-size` flag for tuning.
- Privacy-safe telemetry attributes: repo paths SHA-256 hashed, ref names normalized to bounded enum, commit SHAs restricted to span attributes.
- Pagination telemetry: `pages_requested` counter, `page_number` and `page_size` span attributes.

### Changed

- **Breaking:** `ManifestResponse` replaces `truncated`/`truncation_info` with `pagination` object (`total_items`, `page_start`, `page_size`, `next_cursor`).
- **Breaking:** `HistoryResponse` gains `pagination` object.
- Default manifest page size is 100 files (was 200 hard truncation limit). Agents can request up to 500 per page.
- Summary always reflects all files regardless of which page is returned.
- Tree-sitter analysis runs only on the current page's files (performance improvement for large diffs).

### Technical

- New modules: `src/telemetry.rs`, `src/metrics.rs`, `src/privacy.rs`, `src/pagination.rs`
- `base64` added as direct dependency for cursor encoding
- OpenTelemetry stack: `tracing`, `tracing-opentelemetry`, `opentelemetry-otlp` (gRPC/tonic)
- ADRs: mutation testing baseline (ADR 0002), pagination spike (ADR 0003)
- Test count: 246 → 366 (120 new tests including mutation-testing gap closers)
- CI: mutation testing workflow with 4-shard parallelism, nextest, copy-target caching

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

[0.8.0]: https://github.com/mikelane/git-prism/compare/v0.7.0...v0.8.0
[0.7.0]: https://github.com/mikelane/git-prism/compare/v0.6.0...v0.7.0
[0.6.0]: https://github.com/mikelane/git-prism/compare/v0.5.0...v0.6.0
[0.5.0]: https://github.com/mikelane/git-prism/compare/v0.4.0...v0.5.0
[0.4.0]: https://github.com/mikelane/git-prism/compare/v0.3.1...v0.4.0
[0.3.1]: https://github.com/mikelane/git-prism/compare/v0.3.0...v0.3.1
[0.3.0]: https://github.com/mikelane/git-prism/compare/v0.2.0...v0.3.0
[0.2.0]: https://github.com/mikelane/git-prism/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/mikelane/git-prism/releases/tag/v0.1.0
