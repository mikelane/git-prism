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
| `GIT_PRISM_OTLP_HEADERS` | Comma-separated `key=value` pairs sent as gRPC metadata (e.g. `signoz-access-token=xxx`). | unset |
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

- `tool`: `get_change_manifest`, `get_file_snapshots`, `get_commit_history`
- `status`: `success`, `error`
- `pattern`: `worktree`, `single_commit`, `range_double_dot`, `range_triple_dot`, `branch`, `sha`
- `scope`: `staged`, `unstaged`, `committed`
- `language`: `rust`, `go`, `python`, `typescript`, `javascript`, `java`, `c`, `cpp`

### Performance Metrics

| Metric | Kind | Labels | Description |
|--------|------|--------|-------------|
| `git_prism.tool.duration_ms` | histogram | `tool` | End-to-end tool execution time. |
| `git_prism.gix.operation_ms` | histogram | `operation` | Time spent in gix operations. |
| `git_prism.treesitter.parse_ms` | histogram | `language` | Tree-sitter parse + extraction time. |
| `git_prism.errors.total` | counter | `tool`, `error_kind` | Errors by tool and kind. |

**Label values:**

- `operation`: `open_repo`, `resolve_ref`, `diff_commits`, `diff_worktree`, `read_blob`, `walk_commits`
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

**Label values:**

- `reason`: `max_files`, `max_snapshot_files`, `file_size_limit`

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

### Root Span Attributes

Every root `mcp.tool.*` span carries these attributes:

| Attribute | Description |
|-----------|-------------|
| `tool.name` | Tool that was invoked. |
| `repo.path_hash` | SHA-256 hash of the repository path (never the raw path). |
| `ref.base` | Normalized ref pattern (e.g. `branch`, `sha`, `worktree`). |
| `ref.head` | Normalized ref pattern for the head ref. |
| `response.files_count` | Number of files in the response. |
| `response.tokens_estimated` | Estimated token count. |
| `response.bytes` | Response size in bytes. |
| `response.truncated` | Whether the response was truncated (`true`/`false`). |

---

## Privacy Guarantees

git-prism is designed to be safe for use with cloud-hosted telemetry backends.
The following data **never leaves the process** in any metric, trace, or log:

- **Raw repository paths** -- hashed with SHA-256 before export.
- **File contents** -- never attached to spans or metrics.
- **Commit messages** -- not included in any telemetry data.
- **Author names and email addresses** -- not exported.
- **Literal branch, tag, or ref names** -- normalized to a bounded enum (`branch`, `sha`, `worktree`, etc.) before export.
- **Commit SHAs** — included only as span attributes for trace correlation, never as metric labels. Not human-readable in dashboard views unless explicitly queried.

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
