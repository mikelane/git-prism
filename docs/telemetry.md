# Telemetry Reference

git-prism includes optional OpenTelemetry instrumentation that exports metrics
and traces to any OTLP-compatible backend (SigNoz, Grafana, Datadog, etc.).
Telemetry is **disabled by default** and produces zero overhead when not
configured.

## Configuration

Set these environment variables before starting the server:

| Variable | Purpose | Default |
|----------|---------|---------|
| `GIT_PRISM_OTLP_ENDPOINT` | OTLP gRPC endpoint URL. When unset, telemetry is disabled entirely. | unset (disabled) |
| `GIT_PRISM_OTLP_HEADERS` | **Planned — not yet wired (see [#43](https://github.com/mikelane/git-prism/issues/43)).** Intended for comma-separated `key=value` pairs sent as gRPC metadata (e.g. `signoz-access-token=xxx`). Setting this variable today has no effect; the exporter sends requests without custom headers. Authenticating against managed OTLP backends that require a header currently requires a local collector proxy. | unset |
| `GIT_PRISM_SERVICE_NAME` | `service.name` resource attribute reported to the backend. | `git-prism` |
| `GIT_PRISM_SERVICE_VERSION` | `service.version` resource attribute reported to the backend. | crate version from `Cargo.toml` |

Example:

```bash
export GIT_PRISM_OTLP_ENDPOINT=http://localhost:4317
export GIT_PRISM_SERVICE_NAME=git-prism-staging
git-prism serve
```

---

## Metrics

All metric names are prefixed with `git_prism.`. Counters are monotonic.
Histograms use explicit bucket boundaries listed below.

### Usage Metrics

| Metric | Kind | Labels | Description |
|--------|------|--------|-------------|
| `git_prism.sessions.started` | counter | -- | Total MCP sessions initialized. |
| `git_prism.requests.total` | counter | `tool`, `status` | Total tool invocations. |
| `git_prism.manifest.ref_pattern` | counter | `pattern` | How refs are specified. |
| `git_prism.change_scope.seen` | counter | `scope` | Change scopes encountered. |
| `git_prism.languages.analyzed` | counter | `language` | Files analyzed per language. |

**Label values:**

- `tool`: `get_change_manifest`, `get_file_snapshots`, `get_commit_history`, `get_function_context`
- `status`: `success`, `error`
- `pattern`: `worktree`, `single_commit`, `range_double_dot`, `range_triple_dot`, `branch`, `sha`
- `scope`: `staged`, `unstaged`, `committed`
- `language`: `rust`, `go`, `python`, `typescript`, `javascript`, `java`, `c`, `cpp`, `csharp`, `ruby`, `swift`, `kotlin`, `php` (13 total; matches the registered analyzers in `src/treesitter/mod.rs`. Files whose extension resolves to `unknown` are filtered before emission.)

### Performance Metrics

| Metric | Kind | Labels | Description |
|--------|------|--------|-------------|
| `git_prism.tool.duration_ms` | histogram | `tool` | End-to-end tool execution time. |
| `git_prism.gix.operation_ms` | histogram | `operation` | **Planned — not yet wired.** The instrument is registered so dashboards can refer to it, but no call site currently records durations. Tracked in [#189](https://github.com/mikelane/git-prism/issues/189). |
| `git_prism.treesitter.parse_ms` | histogram | `language` | **Planned — not yet wired.** Same status as `gix.operation_ms`; the `record_treesitter_parse` helper is defined but unused. Tracked in [#189](https://github.com/mikelane/git-prism/issues/189). |
| `git_prism.errors.total` | counter | `tool`, `error_kind` | Errors by tool and kind. |

**Label values:**

- `operation`: `open_repo`, `resolve_ref`, `diff_commits`, `diff_worktree`, `read_blob`, `walk_commits` (these are the `git.*` span names in `src/git/`; the label enum is defined for when `gix.operation_ms` is wired up per [#189](https://github.com/mikelane/git-prism/issues/189))
- `error_kind`: `ref_not_found`, `repo_not_found`, `diff_failed`, `parse_failed`, `io_error`, `unknown`

**Duration histogram buckets (ms):** 1, 2, 5, 10, 25, 50, 100, 250, 500, 1000, 2500, 5000, 10000, 30000

### Token-Efficiency Metrics

| Metric | Kind | Labels | Description |
|--------|------|--------|-------------|
| `git_prism.response.tokens_estimated` | histogram | `tool` | Estimated token count of the response. |
| `git_prism.response.bytes` | histogram | `tool` | Response size in bytes. |
| `git_prism.manifest.files_returned` | histogram | -- | Number of files in a manifest response. |
| `git_prism.manifest.functions_changed` | histogram | `language` | Number of changed functions per file. |
| `git_prism.response.truncated` | counter | `tool`, `reason` | Responses that were truncated. |
| `git_prism.pagination.pages_requested` | counter | `tool` | Paginated requests (cursor-bearing). |

**Label values:**

- `reason`: `paginated` (emitted from `get_change_manifest` when the response carries a non-null `next_cursor`), `max_file_size` (emitted from `get_file_snapshots` when a file body exceeds `max_file_size_bytes` and is truncated byte-wise). Silent truncation in `get_file_snapshots` when the `paths` array exceeds its effective limit is not currently reported through this counter; that gap is tracked in [#189](https://github.com/mikelane/git-prism/issues/189).

**Token/byte histogram buckets:** 100, 500, 1000, 5000, 10000, 50000, 100000, 500000, 1000000

---

## Traces

Each tool invocation produces a trace tree rooted at an `mcp.tool.*` span.
All spans use millisecond-precision timing.

### `get_change_manifest`

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

### `get_file_snapshots`

```
mcp.tool.get_file_snapshots
├── git.open_repo
├── git.resolve_ref              [one per ref]
└── git.read_blob                [one per requested file per ref]
```

### `get_commit_history`

```
mcp.tool.get_commit_history
├── git.open_repo
├── git.walk_commits
└── [manifest sub-tree]           [one per commit in range]
```

### `get_function_context`

```
mcp.tool.get_function_context
└── context.build
    ├── context.get_manifest              [delegates into the manifest sub-tree]
    ├── context.scan_files                [walks head tree, records file_count]
    ├── context.match_callers             [leaf-name match against changed functions]
    └── context.extract_callees           [tree-sitter walk per changed function]
```

`context.build` is the entry span for `build_function_context` in `src/tools/context.rs`. It records three attributes when the build completes: `functions_changed`, `files_scanned`, and `total_callers_found`. The sub-spans are sequential (not parallel). Naming is subject to change while the context subsystem is still iterating; treat these spans as internal rather than part of a stable API.

### Root Span Attributes

Every root `mcp.tool.*` span carries these attributes. Names below use the exact identifiers recorded in `src/server.rs` (underscored, not dotted); this is what SigNoz, Grafana, or Datadog will see when querying traces.

| Attribute | Description |
|-----------|-------------|
| `tool_name` | Tool that was invoked. |
| `repo_path_hash` | SHA-256 hash of the repository path (never the raw path). |
| `ref_base` | Normalized ref pattern (e.g. `branch`, `sha`, `worktree`). |
| `ref_head` | Normalized ref pattern for the head ref. `worktree` when the caller omits `head_ref`. |
| `response_files_count` | Number of files in the response. |
| `response_bytes` | Response size in bytes. |
| `response_truncated` | Whether the response carries a non-null `next_cursor` (`true`/`false`). |
| `page_number` | Page number (0-indexed), derived from cursor offset. |
| `page_size` | Requested page size after clamping to the `1..=500` range. |

Token estimates are emitted as the `git_prism.response.tokens_estimated` metric (see the Token-Efficiency Metrics table above) and are not recorded on any span.

---

## Privacy Guarantees

git-prism is designed to be safe for use with cloud-hosted telemetry backends.
The following data **never leaves the process** in any metric, trace, or log:

- **Raw repository paths** -- hashed with SHA-256 before export.
- **File contents** -- never attached to spans or metrics.
- **Commit messages** -- not included in any telemetry data.
- **Author names and email addresses** -- not exported.
- **Literal branch, tag, or ref names** -- normalized to a bounded enum (`branch`, `sha`, `worktree`, etc.) before export.
- **Commit SHAs** -- not exported on any span or metric. `ref_base` and `ref_head` span attributes carry the normalized ref pattern (`branch`, `sha`, `worktree`, ...), not the raw SHA or ref name, so a dashboard viewer cannot reconstruct which commits a caller touched.

Only structural metadata is exported: counts, durations, file extensions
(via language labels), and normalized ref patterns.

---

## Example Queries

These examples use PromQL-style syntax compatible with SigNoz and Grafana.

### Are my agents using git-prism?

Total requests grouped by tool over the last 24 hours:

```promql
sum by (tool) (increase(git_prism_requests_total[24h]))
```

This tells you which tools agents are calling and how often.

### p95 tool duration

95th percentile latency per tool:

```promql
histogram_quantile(0.95, sum by (le, tool) (rate(git_prism_tool_duration_ms_bucket[5m])))
```

Use this to identify slow tool calls and set alerting thresholds.

### Token savings estimate

Distribution of estimated tokens per response, useful for understanding how
compact git-prism responses are compared to raw diffs:

```promql
histogram_quantile(0.50, sum by (le, tool) (rate(git_prism_response_tokens_estimated_bucket[1h])))
```

Compare the median token count across tools to understand typical response sizes
and track whether changes to filtering or truncation settings reduce token usage.
