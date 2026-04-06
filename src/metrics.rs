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

    // Performance histograms
    tool_duration_ms: Histogram<f64>,

    // Token-efficiency histograms
    response_tokens_estimated: Histogram<f64>,
    response_bytes: Histogram<f64>,
    manifest_files_returned: Histogram<f64>,
    manifest_functions_changed: Histogram<f64>,
}

impl Metrics {
    /// Create all instruments from the global meter provider.
    pub fn new() -> Self {
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
            .with_boundaries(SIZE_BUCKETS.to_vec())
            .build();

        let manifest_functions_changed = meter
            .f64_histogram("git_prism.manifest.functions_changed")
            .with_description("Per-file function change count")
            .with_boundaries(SIZE_BUCKETS.to_vec())
            .build();

        Self {
            sessions_started,
            requests_total,
            ref_pattern,
            change_scope_seen,
            languages_analyzed,
            errors_total,
            response_truncated,
            tool_duration_ms,
            response_tokens_estimated,
            response_bytes,
            manifest_files_returned,
            manifest_functions_changed,
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
        self.response_truncated.add(
            1,
            &[
                KeyValue::new("tool", tool.to_string()),
                KeyValue::new("reason", reason.to_string()),
            ],
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
}
