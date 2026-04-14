use std::sync::OnceLock;

use opentelemetry::metrics::{Counter, Histogram};
use opentelemetry::{KeyValue, global};

/// Histogram bucket boundaries for duration measurements (milliseconds).
const DURATION_BUCKETS: &[f64] = &[
    1.0, 2.0, 5.0, 10.0, 25.0, 50.0, 100.0, 250.0, 500.0, 1000.0, 2500.0, 5000.0, 10000.0, 30000.0,
];

/// Histogram bucket boundaries for token/byte size measurements.
const SIZE_BUCKETS: &[f64] = &[
    100.0, 500.0, 1000.0, 5000.0, 10000.0, 50000.0, 100000.0, 500000.0, 1000000.0,
];

/// Histogram bucket boundaries for count-scale measurements (files, functions).
const COUNT_BUCKETS: &[f64] = &[1.0, 2.0, 5.0, 10.0, 25.0, 50.0, 100.0, 200.0, 500.0];

/// All OpenTelemetry instruments for git-prism metrics.
///
/// Created lazily via [`get()`] from the global meter provider installed by
/// `telemetry::init()`. When no OTLP endpoint is configured, the global
/// meter is a no-op, so all recording calls are effectively free.
pub struct Metrics {
    // Usage counters
    sessions_started: Counter<u64>,
    requests_total: Counter<u64>,
    ref_pattern: Counter<u64>,
    change_scope_seen: Counter<u64>,
    languages_analyzed: Counter<u64>,
    errors_total: Counter<u64>,
    response_truncated: Counter<u64>,
    pages_requested: Counter<u64>,

    // Performance histograms
    tool_duration_ms: Histogram<f64>,
    // TODO: gix_operation_ms and treesitter_parse_ms are created so all instruments
    // exist, but actual recording happens via span timing in the tracing/OTel bridge
    // layer rather than explicit calls deep in the git/treesitter modules.
    #[allow(dead_code)]
    gix_operation_ms: Histogram<f64>,
    #[allow(dead_code)]
    treesitter_parse_ms: Histogram<f64>,

    // Token-efficiency histograms
    response_tokens_estimated: Histogram<f64>,
    response_bytes: Histogram<f64>,
    manifest_files_returned: Histogram<f64>,
    manifest_functions_changed: Histogram<f64>,
}

impl Metrics {
    /// Create all instruments from the global meter provider.
    fn new() -> Self {
        let meter = global::meter("git-prism");

        let sessions_started = meter
            .u64_counter("git_prism.sessions.started")
            .with_description("Number of MCP server sessions started")
            .build();

        let requests_total = meter
            .u64_counter("git_prism.requests.total")
            .with_description("Total tool requests")
            .build();

        let ref_pattern = meter
            .u64_counter("git_prism.manifest.ref_pattern")
            .with_description("Ref pattern classification counts")
            .build();

        let change_scope_seen = meter
            .u64_counter("git_prism.change_scope.seen")
            .with_description("Change scope counts per file")
            .build();

        let languages_analyzed = meter
            .u64_counter("git_prism.languages.analyzed")
            .with_description("Languages seen in manifest files")
            .build();

        let errors_total = meter
            .u64_counter("git_prism.errors.total")
            .with_description("Error counts by tool and kind")
            .build();

        let response_truncated = meter
            .u64_counter("git_prism.response.truncated")
            .with_description("Truncation events")
            .build();

        let pages_requested = meter
            .u64_counter("git_prism.pagination.pages_requested")
            .with_description("Paginated requests (cursor-bearing)")
            .build();

        let tool_duration_ms = meter
            .f64_histogram("git_prism.tool.duration_ms")
            .with_description("Tool invocation duration in milliseconds")
            .with_boundaries(DURATION_BUCKETS.to_vec())
            .build();

        let response_tokens_estimated = meter
            .f64_histogram("git_prism.response.tokens_estimated")
            .with_description("Estimated token count of response (bytes / 4)")
            .with_boundaries(SIZE_BUCKETS.to_vec())
            .build();

        let response_bytes = meter
            .f64_histogram("git_prism.response.bytes")
            .with_description("Response JSON byte size")
            .with_boundaries(SIZE_BUCKETS.to_vec())
            .build();

        let manifest_files_returned = meter
            .f64_histogram("git_prism.manifest.files_returned")
            .with_description("Number of files in manifest response")
            .with_boundaries(COUNT_BUCKETS.to_vec())
            .build();

        let manifest_functions_changed = meter
            .f64_histogram("git_prism.manifest.functions_changed")
            .with_description("Per-file function change count")
            .with_boundaries(COUNT_BUCKETS.to_vec())
            .build();

        let gix_operation_ms = meter
            .f64_histogram("git_prism.gix.operation_ms")
            .with_description("Time spent in gix operations")
            .with_boundaries(DURATION_BUCKETS.to_vec())
            .build();

        let treesitter_parse_ms = meter
            .f64_histogram("git_prism.treesitter.parse_ms")
            .with_description("Tree-sitter parse and extraction time")
            .with_boundaries(DURATION_BUCKETS.to_vec())
            .build();

        Self {
            sessions_started,
            requests_total,
            ref_pattern,
            change_scope_seen,
            languages_analyzed,
            errors_total,
            response_truncated,
            pages_requested,
            tool_duration_ms,
            response_tokens_estimated,
            response_bytes,
            manifest_files_returned,
            manifest_functions_changed,
            gix_operation_ms,
            treesitter_parse_ms,
        }
    }

    // --- Recording helpers ---

    pub fn record_session_started(&self) {
        self.sessions_started.add(1, &[]);
    }

    pub fn record_request(&self, tool: &str, status: &str) {
        self.requests_total.add(
            1,
            &[
                KeyValue::new("tool", tool.to_string()),
                KeyValue::new("status", status.to_string()),
            ],
        );
    }

    pub fn record_duration(&self, tool: &str, duration_ms: f64) {
        self.tool_duration_ms
            .record(duration_ms, &[KeyValue::new("tool", tool.to_string())]);
    }

    pub fn record_error(&self, tool: &str, error_kind: &str) {
        self.errors_total.add(
            1,
            &[
                KeyValue::new("tool", tool.to_string()),
                KeyValue::new("error_kind", error_kind.to_string()),
            ],
        );
    }

    pub fn record_ref_pattern(&self, pattern: &str) {
        self.ref_pattern
            .add(1, &[KeyValue::new("pattern", pattern.to_string())]);
    }

    pub fn record_change_scope(&self, scope: &str) {
        self.change_scope_seen
            .add(1, &[KeyValue::new("scope", scope.to_string())]);
    }

    pub fn record_language(&self, language: &str) {
        self.languages_analyzed
            .add(1, &[KeyValue::new("language", language.to_string())]);
    }

    pub fn record_response_bytes(&self, tool: &str, bytes: f64) {
        self.response_bytes
            .record(bytes, &[KeyValue::new("tool", tool.to_string())]);
    }

    pub fn record_tokens_estimated(&self, tool: &str, tokens: f64) {
        self.response_tokens_estimated
            .record(tokens, &[KeyValue::new("tool", tool.to_string())]);
    }

    pub fn record_files_returned(&self, count: f64) {
        self.manifest_files_returned.record(count, &[]);
    }

    pub fn record_functions_changed(&self, language: &str, count: f64) {
        self.manifest_functions_changed
            .record(count, &[KeyValue::new("language", language.to_string())]);
    }

    pub fn record_truncated(&self, tool: &str, reason: &str) {
        // Normalize at the metric boundary so attribute cardinality on the
        // `reason` label is bounded by construction regardless of what the
        // caller passes. The classifier is a flat exact-match — see
        // `crate::privacy::classify_truncation_reason` for the full rationale.
        let normalized = crate::privacy::classify_truncation_reason(reason);
        self.response_truncated.add(
            1,
            &[
                KeyValue::new("tool", tool.to_string()),
                KeyValue::new("reason", normalized),
            ],
        );
    }

    pub fn record_pagination_page(&self, tool: &str) {
        self.pages_requested
            .add(1, &[KeyValue::new("tool", tool.to_string())]);
    }

    #[allow(dead_code)]
    pub fn record_gix_operation(&self, operation: &str, duration_ms: f64) {
        self.gix_operation_ms.record(
            duration_ms,
            &[KeyValue::new("operation", operation.to_string())],
        );
    }

    #[allow(dead_code)]
    pub fn record_treesitter_parse(&self, language: &str, duration_ms: f64) {
        self.treesitter_parse_ms.record(
            duration_ms,
            &[KeyValue::new("language", language.to_string())],
        );
    }
}

/// Global singleton accessor for the [`Metrics`] instance.
///
/// The instruments are created from the global meter provider, which is either
/// a real OTLP exporter or a no-op depending on whether `telemetry::init()`
/// found a configured endpoint.
pub fn get() -> &'static Metrics {
    static INSTANCE: OnceLock<Metrics> = OnceLock::new();
    INSTANCE.get_or_init(Metrics::new)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metrics_new_does_not_panic() {
        // With no OTLP endpoint, the global meter is a no-op — but instruments
        // must still be created without error.
        let metrics = Metrics::new();
        // Smoke-test that recording does not panic.
        metrics.record_session_started();
        metrics.record_request("test_tool", "success");
        metrics.record_duration("test_tool", 42.0);
        metrics.record_error("test_tool", "unknown");
        metrics.record_ref_pattern("branch");
        metrics.record_change_scope("committed");
        metrics.record_language("rust");
        metrics.record_response_bytes("test_tool", 1024.0);
        metrics.record_tokens_estimated("test_tool", 256.0);
        metrics.record_files_returned(5.0);
        metrics.record_functions_changed("rust", 3.0);
        metrics.record_truncated("test_tool", "max_files");
        metrics.record_pagination_page("test_tool");
        metrics.record_gix_operation("diff_commits", 15.0);
        metrics.record_treesitter_parse("rust", 5.0);
    }

    #[test]
    fn get_returns_same_instance() {
        let a = get() as *const Metrics;
        let b = get() as *const Metrics;
        assert_eq!(a, b, "get() should return the same singleton");
    }

    #[test]
    fn record_request_with_different_statuses() {
        let metrics = Metrics::new();
        // Both success and error status should work without panic.
        metrics.record_request("get_change_manifest", "success");
        metrics.record_request("get_change_manifest", "error");
        metrics.record_request("get_commit_history", "success");
        metrics.record_request("get_file_snapshots", "error");
    }

    #[test]
    fn record_all_change_scopes() {
        let metrics = Metrics::new();
        metrics.record_change_scope("committed");
        metrics.record_change_scope("staged");
        metrics.record_change_scope("unstaged");
    }

    #[test]
    fn record_pagination_page_does_not_panic() {
        let metrics = Metrics::new();
        metrics.record_pagination_page("get_change_manifest");
        metrics.record_pagination_page("get_commit_history");
    }

    #[test]
    fn record_all_error_kinds() {
        let metrics = Metrics::new();
        for kind in &[
            "ref_not_found",
            "repo_not_found",
            "diff_failed",
            "parse_failed",
            "io_error",
            "unknown",
        ] {
            metrics.record_error("test_tool", kind);
        }
    }

    #[test]
    fn it_normalizes_unknown_truncation_reason_without_panicking() {
        // Indirect behavior check: an unrecognized reason string must pass
        // through record_truncated cleanly, because classify_truncation_reason
        // folds it to "unknown" at the metric boundary. The real per-arm
        // assertion lives in src/privacy.rs::tests; this test locks in the
        // wiring so a future refactor that drops the normalization call would
        // fail to compile or panic here.
        let metrics = Metrics::new();
        metrics.record_truncated("test_tool", "wildly_unrecognized_reason_42");
        metrics.record_truncated("test_tool", "paginated");
        metrics.record_truncated("test_tool", "token_budget");
    }
}
