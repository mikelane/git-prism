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

/// Guard that owns the telemetry providers. When dropped, it flushes
/// pending spans and metrics with a bounded timeout.
pub struct TelemetryGuard {
    tracer_provider: Option<SdkTracerProvider>,
    meter_provider: Option<SdkMeterProvider>,
}

impl TelemetryGuard {
    /// Returns `true` if telemetry is active (providers are initialized).
    #[cfg(test)]
    fn is_active(&self) -> bool {
        self.tracer_provider.is_some()
    }
}

/// The design spec targets a 5s flush on shutdown. The SDK's `.shutdown()` handles
/// its own timing internally, so we rely on its defaults here rather than passing
/// an explicit timeout.
impl Drop for TelemetryGuard {
    fn drop(&mut self) {
        if let Some(tp) = self.tracer_provider.take() {
            let _ = tp.shutdown();
        }
        if let Some(mp) = self.meter_provider.take() {
            let _ = mp.shutdown();
        }
    }
}

/// Read telemetry configuration from environment variables and initialize
/// OpenTelemetry providers if an OTLP endpoint is configured.
///
/// When `GIT_PRISM_OTLP_ENDPOINT` is not set, this returns a no-op guard
/// with zero overhead.
pub fn init() -> TelemetryGuard {
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

    let service_name =
        std::env::var(ENV_SERVICE_NAME).unwrap_or_else(|_| DEFAULT_SERVICE_NAME.to_string());
    let service_version = std::env::var(ENV_SERVICE_VERSION)
        .unwrap_or_else(|_| env!("CARGO_PKG_VERSION").to_string());

    // Build the OTLP trace exporter.
    let trace_exporter = match opentelemetry_otlp::SpanExporter::builder()
        .with_http()
        .with_endpoint(&endpoint)
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
        .with_endpoint(&endpoint)
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

    // Install global meter provider and tracing subscriber.
    // In test builds both are skipped: the global subscriber is managed by
    // #[traced_test] (its Once would be poisoned by a competing registration),
    // and set_meter_provider triggers OpenTelemetry's internal tracing
    // diagnostics which also install a global subscriber as a side effect.
    #[cfg(not(test))]
    opentelemetry::global::set_meter_provider(meter_provider.clone());

    // Initialize the tracing subscriber with the OTel layer.
    #[cfg(not(test))]
    {
        let tracer = tracer_provider.tracer("git-prism");
        let otel_layer = tracing_opentelemetry::layer().with_tracer(tracer);
        if let Err(e) = Registry::default().with(otel_layer).try_init() {
            eprintln!("git-prism: failed to initialize tracing subscriber: {e}");
        }
    }

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

    // Helper to remove telemetry env vars for test isolation.
    // SAFETY: Caller must hold ENV_MUTEX. `set_var`/`remove_var` are unsafe
    // because they mutate shared process state; the mutex serializes access
    // so no concurrent mutation occurs.
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
            std::env::set_var(ENV_OTLP_ENDPOINT, "http://localhost:4317");
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
            std::env::set_var(ENV_OTLP_ENDPOINT, "http://localhost:4317");
        }
        let active_guard = init();
        // SAFETY: cleanup
        unsafe {
            std::env::remove_var(ENV_OTLP_ENDPOINT);
        }
        drop(active_guard);
        // If we reach here without panicking, the test passes.
    }

    #[tokio::test]
    async fn test_init_with_custom_service_name_succeeds() {
        let _lock = ENV_MUTEX.lock().unwrap();
        // SAFETY: ENV_MUTEX is held — no concurrent env mutation.
        unsafe {
            clear_telemetry_env();
            std::env::set_var(ENV_OTLP_ENDPOINT, "http://localhost:4317");
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
}
