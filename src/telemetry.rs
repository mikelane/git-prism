use std::time::Duration;

#[cfg(not(test))]
use opentelemetry::trace::TracerProvider as _;
use opentelemetry_otlp::WithExportConfig;
use opentelemetry_sdk::{metrics::SdkMeterProvider, trace::SdkTracerProvider};
#[cfg(not(test))]
use tracing_subscriber::{Registry, layer::SubscriberExt, util::SubscriberInitExt};

const DEFAULT_SERVICE_NAME: &str = "git-prism";
const EXPORT_TIMEOUT: Duration = Duration::from_secs(5);

/// Environment variable names for telemetry configuration.
const ENV_OTLP_ENDPOINT: &str = "GIT_PRISM_OTLP_ENDPOINT";
// TODO: wire up GIT_PRISM_OTLP_HEADERS (#43)
const ENV_SERVICE_NAME: &str = "GIT_PRISM_SERVICE_NAME";
const ENV_SERVICE_VERSION: &str = "GIT_PRISM_SERVICE_VERSION";

/// Compute the per-signal OTLP HTTP endpoints from a base URL.
///
/// `opentelemetry-otlp` 0.28's HTTP exporter does not auto-append the
/// per-signal path (`/v1/traces`, `/v1/metrics`) when the endpoint is
/// supplied via `with_endpoint()` — only the env-var-driven path
/// (`OTEL_EXPORTER_OTLP_ENDPOINT`) triggers that behavior. We construct
/// the full signal URLs explicitly so a user-supplied
/// `GIT_PRISM_OTLP_ENDPOINT=http://collector:4318` reaches the canonical
/// signal paths that real OTLP backends expect.
fn signal_endpoints(base: &str) -> (String, String) {
    let trimmed = base.trim_end_matches('/');
    (
        format!("{trimmed}/v1/traces"),
        format!("{trimmed}/v1/metrics"),
    )
}

/// Guard that owns the telemetry providers. When dropped, it flushes
/// pending spans and metrics with a bounded timeout.
pub struct TelemetryGuard {
    tracer_provider: Option<SdkTracerProvider>,
    meter_provider: Option<SdkMeterProvider>,
}

impl TelemetryGuard {
    /// Returns `true` if telemetry is active (providers are initialized).
    ///
    /// A `false` return means `init()` either saw no endpoint configured or
    /// failed at some stage of provider/subscriber setup and degraded to a
    /// zero-cost no-op. Production callers (e.g. `run_server`) can use this
    /// to emit a user-visible warning that telemetry is inactive even though
    /// `GIT_PRISM_OTLP_ENDPOINT` is set.
    pub fn is_active(&self) -> bool {
        self.tracer_provider.is_some()
    }
}

/// The design spec targets a 5s flush on shutdown. The SDK's `.shutdown()` handles
/// its own timing internally, so we rely on its defaults here rather than passing
/// an explicit timeout.
impl Drop for TelemetryGuard {
    fn drop(&mut self) {
        if let Some(tp) = self.tracer_provider.take()
            && let Err(e) = tp.shutdown()
        {
            eprintln!("git-prism: failed to flush traces on shutdown: {e}");
        }
        if let Some(mp) = self.meter_provider.take()
            && let Err(e) = mp.shutdown()
        {
            eprintln!("git-prism: failed to flush metrics on shutdown: {e}");
        }
    }
}

/// Attach the OTel tracing layer to the global `tracing` subscriber.
///
/// Returns `Err` if another subscriber is already registered globally — the
/// most common cause is a competing library (e.g. rmcp's stdio logger) having
/// installed its own subscriber before git-prism gets the chance. When this
/// happens, `init()` must degrade to a no-op guard rather than silently
/// dropping every span into an unattached OTel layer (see B1 regression test).
#[cfg(not(test))]
fn attach_tracing_subscriber_default(tracer_provider: &SdkTracerProvider) -> Result<(), String> {
    let tracer = tracer_provider.tracer("git-prism");
    let otel_layer = tracing_opentelemetry::layer().with_tracer(tracer);
    Registry::default()
        .with(otel_layer)
        .try_init()
        .map_err(|e| e.to_string())
}

/// In test builds the global tracing subscriber is managed by `#[traced_test]`
/// (or is left unset) and must not be touched by `init()` — installing a
/// competing subscriber would poison `Once` state shared across tests. The
/// regression test for B1 exercises the failure path via `init_with_attacher`
/// directly, so the default attacher can safely be a no-op in test builds.
#[cfg(test)]
fn attach_tracing_subscriber_default(_tracer_provider: &SdkTracerProvider) -> Result<(), String> {
    Ok(())
}

/// Read telemetry configuration from environment variables and initialize
/// OpenTelemetry providers if an OTLP endpoint is configured.
///
/// When `GIT_PRISM_OTLP_ENDPOINT` is not set, this returns a no-op guard
/// with zero overhead.
pub fn init() -> TelemetryGuard {
    init_with_attacher(attach_tracing_subscriber_default)
}

/// Core telemetry initialization body, parameterized by a subscriber-attach
/// function so tests can inject a failure without touching tracing's
/// process-global state.
///
/// This function is the single source of truth for the ordering of exporter
/// construction, provider installation, subscriber attachment, and the
/// user-visible "telemetry initialized" message. Every failure path must
/// return a no-op `TelemetryGuard` AND suppress the success message —
/// otherwise operators see "initialized" while spans silently disappear.
fn init_with_attacher<F>(attach_subscriber: F) -> TelemetryGuard
where
    F: FnOnce(&SdkTracerProvider) -> Result<(), String>,
{
    let endpoint = match std::env::var(ENV_OTLP_ENDPOINT) {
        Ok(ep) if !ep.is_empty() => ep,
        _ => {
            // No endpoint configured — return no-op guard, zero cost.
            return TelemetryGuard {
                tracer_provider: None,
                meter_provider: None,
            };
        }
    };

    let base = endpoint.trim_end_matches('/');
    let (traces_endpoint, metrics_endpoint) = signal_endpoints(base);

    let service_name =
        std::env::var(ENV_SERVICE_NAME).unwrap_or_else(|_| DEFAULT_SERVICE_NAME.to_string());
    let service_version = std::env::var(ENV_SERVICE_VERSION)
        .unwrap_or_else(|_| env!("CARGO_PKG_VERSION").to_string());

    // Build the OTLP trace exporter.
    let trace_exporter = match opentelemetry_otlp::SpanExporter::builder()
        .with_http()
        .with_endpoint(&traces_endpoint)
        .with_timeout(EXPORT_TIMEOUT)
        .build()
    {
        Ok(exp) => exp,
        Err(e) => {
            eprintln!("git-prism: failed to initialize trace exporter: {e}");
            return TelemetryGuard {
                tracer_provider: None,
                meter_provider: None,
            };
        }
    };

    // Build the OTLP metrics exporter.
    let metrics_exporter = match opentelemetry_otlp::MetricExporter::builder()
        .with_http()
        .with_endpoint(&metrics_endpoint)
        .with_timeout(EXPORT_TIMEOUT)
        .build()
    {
        Ok(exp) => exp,
        Err(e) => {
            eprintln!("git-prism: failed to initialize metrics exporter: {e}");
            return TelemetryGuard {
                tracer_provider: None,
                meter_provider: None,
            };
        }
    };

    let resource = opentelemetry_sdk::Resource::builder()
        .with_service_name(service_name)
        .with_attribute(opentelemetry::KeyValue::new(
            "service.version",
            service_version,
        ))
        .build();

    // Tracer provider
    let tracer_provider = SdkTracerProvider::builder()
        .with_batch_exporter(trace_exporter)
        .with_resource(resource.clone())
        .build();

    // Meter provider
    let reader = opentelemetry_sdk::metrics::PeriodicReader::builder(metrics_exporter).build();

    let meter_provider = SdkMeterProvider::builder()
        .with_reader(reader)
        .with_resource(resource)
        .build();

    // Install global meter provider. In test builds this is skipped:
    // `set_meter_provider` triggers OpenTelemetry's internal tracing
    // diagnostics which install a global subscriber as a side effect,
    // and tests manage their own subscriber state via `#[traced_test]`.
    #[cfg(not(test))]
    opentelemetry::global::set_meter_provider(meter_provider.clone());

    // Attach the OTel layer to the tracing subscriber. If this fails
    // (most commonly because another subscriber was already registered
    // globally — e.g. rmcp's stdio logger), degrade to a no-op guard
    // consistent with the exporter-build failure paths above. The
    // success message below MUST NOT fire in that case, or operators
    // will see "initialized" while spans silently disappear into an
    // unattached OTel layer.
    if let Err(e) = attach_subscriber(&tracer_provider) {
        eprintln!("git-prism: failed to initialize tracing subscriber: {e}");
        return TelemetryGuard {
            tracer_provider: None,
            meter_provider: None,
        };
    }

    eprintln!("git-prism: telemetry initialized (HTTP/protobuf, endpoint={base})");

    TelemetryGuard {
        tracer_provider: Some(tracer_provider),
        meter_provider: Some(meter_provider),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// Mutex to serialize tests that mutate process-global environment variables.
    /// `std::env::set_var` / `remove_var` are not thread-safe; concurrent mutation
    /// is undefined behavior. Every test that touches env vars MUST hold this lock
    /// for the duration of the test (setup, exercise, and cleanup).
    static ENV_MUTEX: Mutex<()> = Mutex::new(());

    /// Helper to remove telemetry env vars for test isolation.
    ///
    /// # Safety
    ///
    /// Caller must hold `ENV_MUTEX` for the duration of the call and any
    /// subsequent env-var reads in the same test. `set_var`/`remove_var` are
    /// unsafe because they mutate shared process state without synchronization;
    /// holding the mutex serializes all access so no concurrent mutation occurs.
    unsafe fn clear_telemetry_env() {
        unsafe {
            std::env::remove_var(ENV_OTLP_ENDPOINT);
            std::env::remove_var(ENV_SERVICE_NAME);
            std::env::remove_var(ENV_SERVICE_VERSION);
        }
    }

    #[test]
    fn test_init_without_env_returns_noop() {
        let _lock = ENV_MUTEX.lock().unwrap();
        // SAFETY: ENV_MUTEX is held — no concurrent env mutation.
        unsafe {
            clear_telemetry_env();
        }
        let guard = init();
        assert!(
            !guard.is_active(),
            "guard should be no-op when no endpoint is set"
        );
    }

    #[test]
    fn test_init_with_empty_endpoint_returns_noop() {
        let _lock = ENV_MUTEX.lock().unwrap();
        // SAFETY: ENV_MUTEX is held — no concurrent env mutation.
        unsafe {
            clear_telemetry_env();
            std::env::set_var(ENV_OTLP_ENDPOINT, "");
        }
        let guard = init();
        assert!(
            !guard.is_active(),
            "guard should be no-op when endpoint is empty"
        );
        // SAFETY: cleanup
        unsafe {
            std::env::remove_var(ENV_OTLP_ENDPOINT);
        }
    }

    #[tokio::test]
    async fn test_init_with_endpoint_creates_providers() {
        let _lock = ENV_MUTEX.lock().unwrap();
        // SAFETY: ENV_MUTEX is held — no concurrent env mutation.
        unsafe {
            clear_telemetry_env();
            // Use a dummy endpoint — the exporter won't connect but providers
            // should still be created.
            std::env::set_var(ENV_OTLP_ENDPOINT, "http://localhost:4318");
        }
        let guard = init();
        assert!(
            guard.is_active(),
            "guard should be active when endpoint is set"
        );
        // SAFETY: cleanup
        unsafe {
            std::env::remove_var(ENV_OTLP_ENDPOINT);
        }
        drop(guard);
    }

    #[tokio::test]
    async fn test_guard_drop_does_not_panic() {
        let _lock = ENV_MUTEX.lock().unwrap();
        // No-op guard
        let noop_guard = TelemetryGuard {
            tracer_provider: None,
            meter_provider: None,
        };
        drop(noop_guard);

        // Active guard (with real providers)
        // SAFETY: ENV_MUTEX is held — no concurrent env mutation.
        unsafe {
            clear_telemetry_env();
            std::env::set_var(ENV_OTLP_ENDPOINT, "http://localhost:4318");
        }
        let active_guard = init();
        // SAFETY: cleanup
        unsafe {
            std::env::remove_var(ENV_OTLP_ENDPOINT);
        }
        drop(active_guard);
        // If we reach here without panicking, the test passes.
    }

    #[test]
    fn it_trims_trailing_slash_when_computing_signal_paths() {
        let (traces, metrics) = signal_endpoints("http://localhost:4318/");
        assert_eq!(traces, "http://localhost:4318/v1/traces");
        assert_eq!(metrics, "http://localhost:4318/v1/metrics");
    }

    #[test]
    fn it_appends_signal_paths_to_a_bare_base() {
        let (traces, metrics) = signal_endpoints("http://localhost:4318");
        assert_eq!(traces, "http://localhost:4318/v1/traces");
        assert_eq!(metrics, "http://localhost:4318/v1/metrics");
    }

    #[tokio::test]
    async fn test_init_with_custom_service_name_succeeds() {
        let _lock = ENV_MUTEX.lock().unwrap();
        // SAFETY: ENV_MUTEX is held — no concurrent env mutation.
        unsafe {
            clear_telemetry_env();
            std::env::set_var(ENV_OTLP_ENDPOINT, "http://localhost:4318");
            std::env::set_var(ENV_SERVICE_NAME, "custom-prism");
        }
        let guard = init();
        assert!(guard.is_active());
        // SAFETY: cleanup
        unsafe {
            std::env::remove_var(ENV_OTLP_ENDPOINT);
            std::env::remove_var(ENV_SERVICE_NAME);
        }
        drop(guard);
    }

    /// Regression test for PR #210 blocker B1.
    ///
    /// Before the fix: when `Registry::default().with(otel_layer).try_init()`
    /// failed (most commonly because another subscriber was already registered
    /// globally, e.g. rmcp's stdio logger), `init()` logged the error via
    /// `eprintln!` and then continued on to print the success message and
    /// return an active `TelemetryGuard`. Traces silently disappeared into an
    /// OTel layer that was never attached to a subscriber while the operator
    /// saw "telemetry initialized" on stderr.
    ///
    /// This test injects a failing subscriber-attacher and asserts the guard
    /// degrades to no-op — matching how the other exporter-build failure paths
    /// behave. The injection point avoids depending on tracing's process-global
    /// subscriber state (which would make this test order-dependent with the
    /// other happy-path tests in this module).
    #[tokio::test]
    async fn it_returns_noop_guard_when_tracing_subscriber_init_fails() {
        let _lock = ENV_MUTEX.lock().unwrap();
        // SAFETY: ENV_MUTEX is held — no concurrent env mutation.
        unsafe {
            clear_telemetry_env();
            std::env::set_var(ENV_OTLP_ENDPOINT, "http://localhost:4318");
        }
        let guard =
            init_with_attacher(|_tp| Err("subscriber already registered (simulated)".to_string()));
        // SAFETY: cleanup
        unsafe {
            std::env::remove_var(ENV_OTLP_ENDPOINT);
        }
        assert!(
            !guard.is_active(),
            "guard must degrade to no-op when the tracing subscriber cannot be attached; \
             returning an active guard with no attached subscriber silently drops every span"
        );
    }
}
