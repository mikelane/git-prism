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
/// `opentelemetry-otlp`'s HTTP exporter does not auto-append the
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

    /// Best-effort flush with a wall-clock deadline.
    ///
    /// Runs `force_flush` on a background thread and waits at most `timeout`
    /// for it to complete.  If the collector is unreachable and the flush
    /// blocks (up to `EXPORT_TIMEOUT = 5 s`), this method returns after
    /// `timeout` without blocking the caller.
    ///
    /// Use this on the shim passthrough path where every intercepted git
    /// command pays the flush cost: a DOWN OTLP endpoint must not stall the
    /// developer's shell for 5 s per invocation.
    ///
    /// Safe to call on a no-op guard: completes immediately with no overhead.
    pub fn force_flush_bounded(&mut self, timeout: Duration) {
        if self.meter_provider.is_none() && self.tracer_provider.is_none() {
            return;
        }
        // Clone the Arc-backed provider handles so they can be moved into the thread.
        let mp = self.meter_provider.clone();
        let tp = self.tracer_provider.clone();
        let (tx, rx) = std::sync::mpsc::channel::<()>();
        std::thread::spawn(move || {
            if let Some(mp) = mp
                && let Err(e) = mp.force_flush()
            {
                eprintln!("git-prism: failed to force-flush metrics: {e}");
            }
            if let Some(tp) = tp
                && let Err(e) = tp.force_flush()
            {
                eprintln!("git-prism: failed to force-flush traces: {e}");
            }
            let _ = tx.send(());
        });
        // Wait up to `timeout`; if the flush thread is still blocked, abandon it.
        // The thread will be torn down with the process on execvp or normal exit.
        let _ = rx.recv_timeout(timeout);
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
    init_with_attacher(attach_tracing_subscriber_default, true)
}

/// Like [`init`], but suppresses the one-line "telemetry initialized" success
/// banner on stderr.
///
/// The shim runs once per intercepted `git`/`gh` invocation, so emitting the
/// success line every time would flood the developer's shell with one stderr
/// line per git command. Failure paths still log (a misconfigured endpoint must
/// remain visible) and the endpoint is still recorded in the exported resource;
/// only the per-invocation success banner is silenced.
pub fn init_quiet() -> TelemetryGuard {
    init_with_attacher(attach_tracing_subscriber_default, false)
}

/// Read the service name from the environment, falling back to the default
/// when the variable is absent **or empty**.
fn resolve_service_name() -> String {
    match std::env::var(ENV_SERVICE_NAME) {
        Ok(v) if !v.is_empty() => v,
        _ => DEFAULT_SERVICE_NAME.to_string(),
    }
}

/// Read the service version from the environment, falling back to the crate
/// version when the variable is absent **or empty**.
fn resolve_service_version() -> String {
    match std::env::var(ENV_SERVICE_VERSION) {
        Ok(v) if !v.is_empty() => v,
        _ => env!("CARGO_PKG_VERSION").to_string(),
    }
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
///
/// `emit_success_banner` controls only the one-line success message printed
/// when initialization fully succeeds. Failure paths log unconditionally so a
/// misconfigured endpoint is never silently swallowed regardless of caller.
fn init_with_attacher<F>(attach_subscriber: F, emit_success_banner: bool) -> TelemetryGuard
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

    let service_name = resolve_service_name();
    let service_version = resolve_service_version();

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

    if emit_success_banner {
        eprintln!("git-prism: telemetry initialized (HTTP/protobuf, endpoint={base})");
    }

    TelemetryGuard {
        tracer_provider: Some(tracer_provider),
        meter_provider: Some(meter_provider),
    }
}

/// Test-only constructors for `TelemetryGuard`.
///
/// Placed in a `#[cfg(test)]` impl block so the dead-code lint does not fire
/// in non-test builds — the methods are invisible outside test compilation.
#[cfg(test)]
impl TelemetryGuard {
    /// Construct a no-op guard with no providers attached.
    ///
    /// Used in unit tests that need to pass a `TelemetryGuard` to functions
    /// under test without initializing real OTLP providers.  `force_flush`,
    /// `force_flush_bounded`, and `Drop` on this guard are zero-cost no-ops.
    pub(crate) fn noop() -> Self {
        Self {
            tracer_provider: None,
            meter_provider: None,
        }
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

    #[test]
    fn test_init_quiet_without_env_returns_noop() {
        let _lock = ENV_MUTEX.lock().unwrap();
        // SAFETY: ENV_MUTEX is held — no concurrent env mutation.
        unsafe {
            clear_telemetry_env();
        }
        let guard = init_quiet();
        assert!(
            !guard.is_active(),
            "init_quiet must produce a no-op guard when no endpoint is set, same as init"
        );
    }

    #[tokio::test]
    async fn test_init_quiet_with_endpoint_creates_providers() {
        let _lock = ENV_MUTEX.lock().unwrap();
        // SAFETY: ENV_MUTEX is held — no concurrent env mutation.
        unsafe {
            clear_telemetry_env();
            std::env::set_var(ENV_OTLP_ENDPOINT, "http://localhost:4318");
        }
        let guard = init_quiet();
        assert!(
            guard.is_active(),
            "init_quiet must still create active providers when an endpoint is set; \
             suppressing the banner must not suppress telemetry itself"
        );
        // SAFETY: cleanup
        unsafe {
            std::env::remove_var(ENV_OTLP_ENDPOINT);
        }
        drop(guard);
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

    /// Verify that an empty `GIT_PRISM_SERVICE_VERSION` falls back to the crate
    /// version rather than passing an empty string as the service.version attribute.
    ///
    /// The production code path under test is in `init_with_attacher`:
    ///   `std::env::var(ENV_SERVICE_VERSION).unwrap_or_else(|_| env!(...).to_string())`
    /// Without the `is_empty()` guard, `Ok("")` is returned and the empty string
    /// is used as the service version.  This test calls `resolve_service_version`
    /// which is the extracted helper that applies the guard.
    #[test]
    fn it_uses_crate_version_when_service_version_env_is_empty_string() {
        let _lock = ENV_MUTEX.lock().unwrap();
        // SAFETY: ENV_MUTEX is held — no concurrent env mutation.
        unsafe {
            clear_telemetry_env();
            std::env::set_var(ENV_SERVICE_VERSION, "");
        }
        let version = resolve_service_version();
        unsafe {
            std::env::remove_var(ENV_SERVICE_VERSION);
        }
        assert_eq!(
            version,
            env!("CARGO_PKG_VERSION"),
            "empty GIT_PRISM_SERVICE_VERSION must fall back to crate version"
        );
    }

    /// Verify a non-empty `GIT_PRISM_SERVICE_NAME` is returned verbatim — the
    /// resolver must actually read the env var, not always return the default.
    /// Kills the mutation where the custom-value branch is dropped.
    #[test]
    fn it_uses_custom_service_name_when_service_name_env_is_set() {
        let _lock = ENV_MUTEX.lock().unwrap();
        // SAFETY: ENV_MUTEX is held — no concurrent env mutation.
        unsafe {
            clear_telemetry_env();
            std::env::set_var(ENV_SERVICE_NAME, "custom-prism");
        }
        let name = resolve_service_name();
        unsafe {
            std::env::remove_var(ENV_SERVICE_NAME);
        }
        assert_eq!(
            name, "custom-prism",
            "a non-empty GIT_PRISM_SERVICE_NAME must be used verbatim, not replaced by the default"
        );
        assert_ne!(
            name, DEFAULT_SERVICE_NAME,
            "the custom name must not collapse to the default"
        );
    }

    /// Verify a non-empty `GIT_PRISM_SERVICE_VERSION` is returned verbatim
    /// rather than the crate-version fallback. Kills the mutation where the
    /// custom-value branch is dropped in favor of always returning the fallback.
    #[test]
    fn it_uses_custom_service_version_when_service_version_env_is_set() {
        let _lock = ENV_MUTEX.lock().unwrap();
        // SAFETY: ENV_MUTEX is held — no concurrent env mutation.
        unsafe {
            clear_telemetry_env();
            std::env::set_var(ENV_SERVICE_VERSION, "9.9.9-custom");
        }
        let version = resolve_service_version();
        unsafe {
            std::env::remove_var(ENV_SERVICE_VERSION);
        }
        assert_eq!(
            version, "9.9.9-custom",
            "a non-empty GIT_PRISM_SERVICE_VERSION must be used verbatim"
        );
        assert_ne!(
            version,
            env!("CARGO_PKG_VERSION"),
            "the custom version must not collapse to the crate-version fallback"
        );
    }

    /// Verify that an empty `GIT_PRISM_SERVICE_NAME` falls back to `"git-prism"`.
    #[test]
    fn it_uses_default_service_name_when_service_name_env_is_empty_string() {
        let _lock = ENV_MUTEX.lock().unwrap();
        // SAFETY: ENV_MUTEX is held — no concurrent env mutation.
        unsafe {
            clear_telemetry_env();
            std::env::set_var(ENV_SERVICE_NAME, "");
        }
        let name = resolve_service_name();
        unsafe {
            std::env::remove_var(ENV_SERVICE_NAME);
        }
        assert_eq!(
            name, DEFAULT_SERVICE_NAME,
            "empty GIT_PRISM_SERVICE_NAME must fall back to default 'git-prism'"
        );
    }

    #[test]
    fn it_uses_crate_version_when_service_version_env_is_unset() {
        let _lock = ENV_MUTEX.lock().unwrap();
        // SAFETY: ENV_MUTEX is held — no concurrent env mutation.
        unsafe {
            clear_telemetry_env();
        }
        // With GIT_PRISM_SERVICE_VERSION unset, the resolver must return exactly
        // the compiled-in crate version — not an empty string, not a stale
        // hardcoded constant. Asserting equality against CARGO_PKG_VERSION binds
        // the test to the live package version, so a regression to a literal
        // string (the original bug this guards) fails immediately.
        let version = resolve_service_version();
        assert_eq!(
            version,
            env!("CARGO_PKG_VERSION"),
            "unset GIT_PRISM_SERVICE_VERSION must fall back to the crate version"
        );
        assert!(
            !version.is_empty(),
            "the crate-version fallback must never be empty"
        );
    }

    /// `init_quiet` must produce the same guard semantics as `init` — it only
    /// suppresses the success banner, not provider construction. An active
    /// endpoint must still yield an active guard.
    #[tokio::test]
    async fn it_quiet_init_still_activates_providers_when_endpoint_is_set() {
        let _lock = ENV_MUTEX.lock().unwrap();
        // SAFETY: ENV_MUTEX is held — no concurrent env mutation.
        unsafe {
            clear_telemetry_env();
            std::env::set_var(ENV_OTLP_ENDPOINT, "http://localhost:4318");
        }
        let guard = init_quiet();
        // SAFETY: cleanup
        unsafe {
            std::env::remove_var(ENV_OTLP_ENDPOINT);
        }
        assert!(
            guard.is_active(),
            "init_quiet must still create providers when an endpoint is configured; \
             it only suppresses the stderr success banner"
        );
        drop(guard);
    }

    /// With no endpoint, `init_quiet` degrades to a no-op guard just like `init`.
    #[test]
    fn it_quiet_init_returns_noop_without_endpoint() {
        let _lock = ENV_MUTEX.lock().unwrap();
        // SAFETY: ENV_MUTEX is held — no concurrent env mutation.
        unsafe {
            clear_telemetry_env();
        }
        let guard = init_quiet();
        assert!(
            !guard.is_active(),
            "init_quiet must be a no-op guard when no endpoint is set"
        );
    }

    #[test]
    fn it_force_flushes_bounded_noop_guard_without_panic() {
        let mut guard = TelemetryGuard::noop();
        // force_flush_bounded on a no-op guard must complete immediately with no panic.
        guard.force_flush_bounded(Duration::from_millis(200));
    }

    #[test]
    fn it_force_flushes_noop_guard_without_panic() {
        let mut guard = TelemetryGuard {
            tracer_provider: None,
            meter_provider: None,
        };
        // force_flush_bounded on a no-op guard must be a zero-cost no-op.
        guard.force_flush_bounded(Duration::from_millis(500));
    }

    #[tokio::test]
    async fn it_force_flushes_active_guard_without_panic() {
        let _lock = ENV_MUTEX.lock().unwrap();
        // SAFETY: ENV_MUTEX is held — no concurrent env mutation.
        unsafe {
            clear_telemetry_env();
            std::env::set_var(ENV_OTLP_ENDPOINT, "http://localhost:4318");
        }
        let mut guard = init();
        assert!(
            guard.is_active(),
            "guard must be active for this test to exercise flush"
        );
        // force_flush_bounded on an active guard must not panic even when no exporter is reachable.
        guard.force_flush_bounded(Duration::from_millis(500));
        // SAFETY: cleanup
        unsafe {
            std::env::remove_var(ENV_OTLP_ENDPOINT);
        }
        drop(guard);
    }

    /// Double-flush must be idempotent: `force_flush_bounded` borrows the providers
    /// (`&mut self`) rather than taking them, so a second call — and a
    /// subsequent `Drop` — must still succeed without panic or double-free.
    /// This is the real shim sequence: flush before exec, then Drop on exit.
    #[tokio::test]
    async fn it_tolerates_double_force_flush_then_drop_on_active_guard() {
        let _lock = ENV_MUTEX.lock().unwrap();
        // SAFETY: ENV_MUTEX is held — no concurrent env mutation.
        unsafe {
            clear_telemetry_env();
            std::env::set_var(ENV_OTLP_ENDPOINT, "http://localhost:4318");
        }
        let mut guard = init();
        assert!(
            guard.is_active(),
            "guard must be active for this test to exercise repeated flush"
        );
        guard.force_flush_bounded(Duration::from_millis(500));
        // Second flush must remain a no-panic no-op: providers are still owned.
        guard.force_flush_bounded(Duration::from_millis(500));
        // Guard must still be active after repeated flushes — flush does not
        // consume the providers the way Drop does.
        assert!(
            guard.is_active(),
            "force_flush_bounded must not consume the providers; guard stays active"
        );
        // SAFETY: cleanup
        unsafe {
            std::env::remove_var(ENV_OTLP_ENDPOINT);
        }
        // Drop after two flushes must not double-free or panic.
        drop(guard);
    }

    /// Double-flush on a no-op guard must remain a zero-cost no-op across
    /// repeated calls and a final Drop.
    #[test]
    fn it_tolerates_double_force_flush_on_noop_guard() {
        let mut guard = TelemetryGuard::noop();
        guard.force_flush_bounded(Duration::from_millis(500));
        guard.force_flush_bounded(Duration::from_millis(500));
        assert!(
            !guard.is_active(),
            "a no-op guard must stay inactive across repeated force_flush_bounded calls"
        );
        drop(guard);
    }

    /// `force_flush_bounded` on an active guard must return within the deadline
    /// even when the configured OTLP endpoint is unreachable. The 5 s exporter
    /// timeout must NOT leak through: we cap the wait at 200 ms and assert the
    /// call returns in well under the exporter timeout. This is the property the
    /// shim relies on so a DOWN collector never stalls the developer's shell.
    #[tokio::test]
    async fn it_force_flushes_bounded_active_guard_within_deadline() {
        let _lock = ENV_MUTEX.lock().unwrap();
        // SAFETY: ENV_MUTEX is held — no concurrent env mutation.
        unsafe {
            clear_telemetry_env();
            // Unroutable TEST-NET-1 address (RFC 5737): connections hang/fail,
            // exercising the bounded-wait path rather than a fast local refusal.
            std::env::set_var(ENV_OTLP_ENDPOINT, "http://192.0.2.1:4318");
        }
        let mut guard = init();
        assert!(
            guard.is_active(),
            "guard must be active to exercise the bounded-flush wait path"
        );
        let start = std::time::Instant::now();
        guard.force_flush_bounded(Duration::from_millis(200));
        let elapsed = start.elapsed();
        // SAFETY: cleanup
        unsafe {
            std::env::remove_var(ENV_OTLP_ENDPOINT);
        }
        drop(guard);
        assert!(
            elapsed < Duration::from_secs(1),
            "bounded flush must abandon a stalled exporter well before the 1s budget \
             (200ms cap × 5 margin); took {elapsed:?}"
        );
    }

    /// QA #361: the WHOLE structured-path teardown — bounded flush AND the
    /// guard's `Drop` — must stay bounded against an unreachable collector.
    ///
    /// `main.rs` runs exactly this sequence on the structured `git diff` path:
    ///
    /// ```text
    /// let exit_code = run_shim(...);                       // returns ExitCode
    /// guard.force_flush_bounded(STRUCTURED_FLUSH_TIMEOUT); // 500 ms cap
    /// return exit_code;                                    // guard dropped HERE
    /// ```
    ///
    /// The #361 fix bounds `force_flush_bounded`, so the agent's teardown no
    /// longer stalls for multiple seconds even when the collector is unreachable.
    /// This test verifies that the *entire* sequence — bounded flush followed by
    /// the guard's `Drop` (which runs `tp.shutdown()` + `mp.shutdown()`) — stays
    /// bounded against a black-hole endpoint. This regression guard prevents #360
    /// from recurring via either the force_flush path or the guard-drop path.
    #[tokio::test]
    async fn qa_structured_path_total_teardown_is_bounded_on_black_hole_endpoint() {
        use crate::shim::STRUCTURED_FLUSH_TIMEOUT;
        let _lock = ENV_MUTEX.lock().unwrap();
        // SAFETY: ENV_MUTEX is held — no concurrent env mutation.
        unsafe {
            clear_telemetry_env();
            // Unroutable TEST-NET-1 (RFC 5737): connections hang, exercising the
            // export-timeout path rather than a fast local connection refusal.
            std::env::set_var(ENV_OTLP_ENDPOINT, "http://192.0.2.1:4318");
        }
        let mut guard = init();
        assert!(
            guard.is_active(),
            "guard must be active to exercise the structured-path teardown"
        );
        // SAFETY: cleanup before timing so a slow remove_var doesn't pollute the measurement.
        unsafe {
            std::env::remove_var(ENV_OTLP_ENDPOINT);
        }

        let start = std::time::Instant::now();
        // Exact main.rs structured-path sequence:
        guard.force_flush_bounded(STRUCTURED_FLUSH_TIMEOUT); // 500 ms cap
        drop(guard); // <-- main.rs `return exit_code` drops the guard here
        let elapsed = start.elapsed();

        // The whole teardown must stay well under a single 5 s EXPORT_TIMEOUT.
        // A generous 2 s budget (4x the 500 ms flush cap) still fails loudly if
        // Drop reintroduces the unbounded multi-second stall.
        assert!(
            elapsed < Duration::from_secs(2),
            "structured-path teardown (bounded flush + guard Drop) must stay bounded; \
             a black-hole collector must not stall the agent's git diff on teardown. \
             took {elapsed:?} (Drop::shutdown is unbounded — #361 stall reintroduced)"
        );
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
        let guard = init_with_attacher(
            |_tp| Err("subscriber already registered (simulated)".to_string()),
            true,
        );
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
