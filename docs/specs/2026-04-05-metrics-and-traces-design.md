# Metrics and Traces Design

- **Epic**: #43
- **Status**: Draft
- **Date**: 2026-04-05

## Context

git-prism ships as an MCP server that agents call during their sessions. Today there is no visibility into how that service is used. The developer cannot answer basic questions like "are my agents calling git-prism at all?", "which tools do they call most?", "how much token budget do my manifests actually save?", or "which operations are slow?". Adding OpenTelemetry metrics and traces closes this gap and produces the data a SigNoz (or any OTLP backend) dashboard needs.

## Goals

1. **Usage visibility** — confirm agents use git-prism and see what they ask for.
2. **Performance observability** — golden signals (request rate, latency, errors) plus per-operation breakdowns (gix, tree-sitter).
3. **Token-efficiency evidence** — quantify the value proposition (structured data consumes fewer tokens than raw diffs).

## Non-Goals

- Instrumentation of the `git-prism` CLI subcommands. Only the `serve` command (MCP server) emits telemetry.
- Dashboards, alerting rules, or SigNoz-specific configuration. Users build their own.
- Long-term log retention or log aggregation.
- Any feature that runs without explicit opt-in. Telemetry is off unless the user sets the endpoint env var.

## Configuration

Telemetry is controlled entirely by environment variables with the `GIT_PRISM_` prefix. When the endpoint variable is unset, the telemetry subsystem is not initialized and all instrumentation points compile to zero-cost no-ops.

| Variable | Purpose | Default |
|----------|---------|---------|
| `GIT_PRISM_OTLP_ENDPOINT` | OTLP gRPC endpoint URL. When unset, telemetry is disabled. | unset (disabled) |
| `GIT_PRISM_OTLP_HEADERS` | Comma-separated `key=value` pairs for auth headers. | unset |
| `GIT_PRISM_SERVICE_NAME` | Service name reported to the backend. | `git-prism` |
| `GIT_PRISM_SERVICE_VERSION` | Service version reported to the backend. | crate version at compile time |

## Architecture

### New Module: `src/telemetry.rs`

Owns the full telemetry lifecycle.

**`init() -> TelemetryGuard`** — reads env vars, configures the OTLP exporter (traces and metrics), installs the global tracer + meter providers, and returns a guard. If `GIT_PRISM_OTLP_ENDPOINT` is unset, returns a no-op guard and does nothing else. The guard's `Drop` impl flushes any pending spans/metrics on shutdown.

**Metric and tracer initialization** — uses the `opentelemetry-otlp` exporter with the gRPC transport. Meters and tracers are created once at startup and cached for reuse.

### Instrumentation Sites

- `src/server.rs` — calls `telemetry::init()` at MCP server startup and holds the guard for the lifetime of the process.
- `src/server.rs::call_tool` handler — wraps each tool invocation in a root span and records the top-level metrics (request count, duration, token estimate, response bytes).
- `src/git/reader.rs`, `src/git/diff.rs`, `src/git/worktree.rs` — sub-spans for gix operations.
- `src/treesitter/*.rs` — sub-spans for parser setup and extraction calls.
- `src/tools/manifest.rs`, `src/tools/snapshots.rs`, `src/tools/history.rs` — sub-spans for analysis steps.

Instrumentation uses `#[tracing::instrument]` attribute macros plus a handful of manual `tracing::info_span!` calls where attribute recording needs finer control.

### Shutdown and Flushing

The MCP server runs until its stdio connection closes. The `TelemetryGuard` returned from `init()` lives for the server's lifetime. When it drops (normal shutdown or panic unwind), its `Drop` impl calls `shutdown_tracer_provider()` and `shutdown_meter_provider()` to flush pending telemetry. A small timeout (5 seconds) bounds the shutdown wait.

## Metrics Catalog

All metrics use OTLP instrument conventions. Durations are milliseconds. Byte counts are bytes. Token estimates are unitless integers.

### Usage Metrics

| Metric | Kind | Labels |
|--------|------|--------|
| `git_prism.sessions.started` | counter | — |
| `git_prism.requests.total` | counter | `tool`, `status` |
| `git_prism.manifest.ref_pattern` | counter | `pattern` |
| `git_prism.change_scope.seen` | counter | `scope` |
| `git_prism.languages.analyzed` | counter | `language` |

`ref_pattern` values: `worktree`, `single_commit`, `range_double_dot`, `range_triple_dot`, `branch`, `sha`.
`scope` values: `staged`, `unstaged`, `committed`.
`status` values: `success`, `error`.

### Performance Metrics

| Metric | Kind | Labels |
|--------|------|--------|
| `git_prism.tool.duration_ms` | histogram | `tool` |
| `git_prism.gix.operation_ms` | histogram | `operation` |
| `git_prism.treesitter.parse_ms` | histogram | `language` |
| `git_prism.errors.total` | counter | `tool`, `error_kind` |

`operation` values: `open_repo`, `resolve_ref`, `diff_commits`, `diff_worktree`, `read_blob`, `walk_commits`.

Histogram buckets for `duration_ms` (and variants): exponential, `1, 2, 5, 10, 25, 50, 100, 250, 500, 1000, 2500, 5000, 10000, 30000`.

### Token-Efficiency Metrics

| Metric | Kind | Labels |
|--------|------|--------|
| `git_prism.response.tokens_estimated` | histogram | `tool` |
| `git_prism.response.bytes` | histogram | `tool` |
| `git_prism.manifest.files_returned` | histogram | — |
| `git_prism.manifest.functions_changed` | histogram | `language` |
| `git_prism.response.truncated` | counter | `tool`, `reason` |

Histogram buckets for `tokens_estimated` and `bytes`: exponential, `100, 500, 1000, 5000, 10000, 50000, 100000, 500000, 1000000`.
`reason` values: `max_files`, `max_snapshot_files`, `file_size_limit`.

## Trace Structure

### Root Span

One root span per MCP tool invocation. Name: `mcp.tool.<tool_name>`.

**Attributes:**
- `tool.name`
- `repo.path_hash` — SHA-256 of the canonicalized repo path.
- `ref.base` — normalized pattern, not the literal ref name.
- `ref.head` — normalized pattern.
- `response.files_count`
- `response.tokens_estimated`
- `response.bytes`
- `response.truncated` — boolean.

### Sub-Span Tree

```
mcp.tool.get_change_manifest
├── git.open_repo
├── git.resolve_ref              [one per ref]
├── git.diff_commits OR git.diff_worktree
├── manifest.detect_generated
├── manifest.analyze_file         [one per analyzed file]
│   ├── treesitter.parse
│   ├── treesitter.extract_functions
│   └── treesitter.extract_imports
└── manifest.diff_dependencies
```

For `get_file_snapshots`:

```
mcp.tool.get_file_snapshots
├── git.open_repo
├── git.resolve_ref               [one per ref]
└── git.read_blob                 [one per file]
```

For `get_commit_history`:

```
mcp.tool.get_commit_history
├── git.open_repo
├── git.walk_commits
└── [manifest sub-tree]           [one per commit in range]
```

### Error Recording

Errors are recorded on the owning span via `span.record_error`. A corresponding `git_prism.errors.total` counter fires with the `error_kind` label set to a small enum we control (not the raw error message).

## Cardinality and Privacy

### High-Cardinality Hazards Avoided

- Raw repo paths are hashed before export. The hash is a span attribute, never a metric label.
- Commit SHAs are span attributes, never metric labels.
- Ref names are normalized to a small set of pattern values (see `ref_pattern` label values above). Literal branch and tag names are never exported.
- File paths are counted, never labeled.
- Function names are counted, never labeled.

### Privacy-Safe Exports

The following never leave the process:
- Raw repository paths.
- File contents.
- Commit messages.
- Author names and email addresses.
- Literal branch, tag, or ref names.

This matters even for single-user telemetry because OTLP endpoints are often shared (SigNoz Cloud, team instances) and exports should be safe to share in screenshots.

## Testing

### Unit Tests

- `telemetry::init()` with no env vars — returns no-op guard, no initialization performed.
- `telemetry::init()` with endpoint set — initializes providers, returns real guard.
- Ref pattern normalization — `HEAD` → `single_commit`, `HEAD~3..HEAD` → `range_double_dot`, etc.
- Repo path hashing is deterministic and collision-resistant.
- Error kind classification — `GitError` variants map to a bounded `error_kind` enum.

### Integration Tests

- MCP tool invocation with telemetry enabled produces a span with expected attributes.
- MCP tool invocation with telemetry disabled incurs no span-creation overhead (benchmark comparison).
- Shutdown flushes pending metrics within the configured timeout.

### BDD Scenarios

- Running `git-prism serve` with no telemetry env vars does not attempt any network connections.
- Running `git-prism serve` with a valid `GIT_PRISM_OTLP_ENDPOINT` connects to the configured endpoint and exports a baseline `sessions.started` counter within five seconds.
- Invoking a tool produces the expected metric labels (`tool`, `status`, `scope`, etc.).

## Dependencies

New crates added to `Cargo.toml`:

- `tracing` — structured logging and spans.
- `tracing-subscriber` — subscriber implementation.
- `tracing-opentelemetry` — bridge from `tracing` spans to OpenTelemetry traces.
- `opentelemetry` — metrics and tracer APIs.
- `opentelemetry_sdk` — SDK providers and processors.
- `opentelemetry-otlp` — OTLP gRPC exporter (with `tonic` feature).
- `sha2` — SHA-256 hashing for repo paths.

## Rollout

After the spec is approved, the epic proceeds as a sequence of PRs following the v0.2.0 pattern: BDD bootstrap (Gherkin scenarios), `telemetry.rs` module and OTLP exporter setup, instrumentation of MCP handlers and gix/tree-sitter call sites, privacy-safe attribute serialization, documentation updates, and a narrated capstone demo that walks through a local SigNoz dashboard receiving traffic from a live agent session. Each PR goes through the standard gauntlet and follows the BDD RED-GREEN workflow. When all child issues close, v0.3.0 ships to crates.io and the Homebrew tap.
