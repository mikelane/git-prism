"""Step definitions for the OpenTelemetry (metrics + traces) feature.

This file defines a minimal in-memory OTLP HTTP/protobuf collector. It
accepts POSTs to `/v1/traces` and `/v1/metrics`, parses the binary
protobuf bodies into `ExportTraceServiceRequest` /
`ExportMetricsServiceRequest` messages, and records every received
request so step definitions can assert against the resulting state.
"""

from __future__ import annotations

import json
import os
import subprocess
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from threading import Lock
from typing import Any

from behave import given, then, when
from behave.runner import Context
from opentelemetry.proto.collector.metrics.v1 import metrics_service_pb2
from opentelemetry.proto.collector.trace.v1 import trace_service_pb2

VALID_REF_PATTERNS = {
    "worktree",
    "single_commit",
    "range_double_dot",
    "range_triple_dot",
    "branch",
    "sha",
}


class MockOtlpCollector:
    """In-memory OTLP HTTP/protobuf collector for assertion use in BDD tests.

    Runs a tiny ThreadingHTTPServer that accepts POSTs to `/v1/traces`
    and `/v1/metrics`, parses the binary protobuf bodies into the
    standard `Export*ServiceRequest` messages, and stores every received
    request so step definitions can assert on the state the server emitted.
    """

    def __init__(self) -> None:
        self._lock = Lock()
        self._trace_requests: list[Any] = []
        self._metric_requests: list[Any] = []
        self._server: ThreadingHTTPServer | None = None
        self._thread: threading.Thread | None = None
        self.port: int | None = None

    def start(self) -> None:
        """Bind a ThreadingHTTPServer to a free port and serve in the background."""
        collector = self
        handler_cls = _make_handler(collector)
        self._server = ThreadingHTTPServer(("localhost", 0), handler_cls)
        self.port = self._server.server_address[1]
        self._thread = threading.Thread(
            target=self._server.serve_forever,
            daemon=True,
        )
        self._thread.start()

    def stop(self) -> None:
        if self._server is not None:
            self._server.shutdown()
            self._server.server_close()
            self._server = None
        if self._thread is not None:
            self._thread.join(timeout=5)
            self._thread = None

    def record_trace(self, request: Any) -> None:
        with self._lock:
            self._trace_requests.append(request)

    def record_metrics(self, request: Any) -> None:
        with self._lock:
            self._metric_requests.append(request)

    def get_trace_requests(self) -> list[Any]:
        with self._lock:
            return list(self._trace_requests)

    def get_metric_requests(self) -> list[Any]:
        with self._lock:
            return list(self._metric_requests)

    def clear(self) -> None:
        with self._lock:
            self._trace_requests.clear()
            self._metric_requests.clear()

    def all_spans(self) -> list[Any]:
        """Flatten every received ResourceSpans -> ScopeSpans -> Span."""
        out: list[Any] = []
        for req in self.get_trace_requests():
            for resource_spans in req.resource_spans:
                for scope_spans in resource_spans.scope_spans:
                    out.extend(scope_spans.spans)
        return out

    def all_metrics(self) -> list[Any]:
        """Flatten every received ResourceMetrics -> ScopeMetrics -> Metric."""
        out: list[Any] = []
        for req in self.get_metric_requests():
            for resource_metrics in req.resource_metrics:
                for scope_metrics in resource_metrics.scope_metrics:
                    out.extend(scope_metrics.metrics)
        return out


def _make_handler(collector: MockOtlpCollector) -> type[BaseHTTPRequestHandler]:
    """Build a request handler class bound to a specific collector instance."""

    class _OtlpHttpHandler(BaseHTTPRequestHandler):
        def do_POST(self) -> None:  # noqa: N802 -- BaseHTTPRequestHandler protocol
            content_length = int(self.headers.get("Content-Length", "0") or "0")
            body = self.rfile.read(content_length) if content_length > 0 else b""

            if self.path == "/v1/traces":
                request = trace_service_pb2.ExportTraceServiceRequest()
                request.ParseFromString(body)
                collector.record_trace(request)
                response = trace_service_pb2.ExportTraceServiceResponse()
                self._send_protobuf(response.SerializeToString())
                return

            if self.path == "/v1/metrics":
                request = metrics_service_pb2.ExportMetricsServiceRequest()
                request.ParseFromString(body)
                collector.record_metrics(request)
                response = metrics_service_pb2.ExportMetricsServiceResponse()
                self._send_protobuf(response.SerializeToString())
                return

            self.send_response(404)
            self.end_headers()

        def _send_protobuf(self, body: bytes) -> None:
            self.send_response(200)
            self.send_header("Content-Type", "application/x-protobuf")
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)

        def log_message(self, format: str, *args: Any) -> None:  # noqa: A002 -- BaseHTTPRequestHandler protocol
            # Silence default stderr access logging during tests.
            return

    return _OtlpHttpHandler


# ---------- helpers used by the steps ----------


def _get_collector(context: Context) -> MockOtlpCollector:
    collector = getattr(context, "otlp_collector", None)
    if collector is None:
        msg = (
            "Mock OTLP collector was not started. "
            "The Background step 'a mock OTLP collector is running' must run first."
        )
        raise AssertionError(msg)
    return collector


def _assert_not_jsonrpc_error(line: str, context: str) -> None:
    """Assert that a JSON-RPC response line is not an error response.

    Empty lines and non-JSON output are tolerated (the server may still be
    initializing, or a stray log line may have been written). Only explicit
    JSON-RPC error responses raise.
    """
    if not line.strip():
        return
    try:
        response = json.loads(line)
    except json.JSONDecodeError:
        return
    if isinstance(response, dict) and "error" in response:
        msg = f"Server returned JSON-RPC error in {context}: {response['error']}"
        raise AssertionError(msg)


def _wait_for_export(collector: MockOtlpCollector, timeout: float = 5.0) -> None:
    """Poll the mock collector until at least one trace or metric request arrives.

    Replaces the previous load-bearing ``time.sleep(0.5)``. Does not raise on
    timeout — downstream assertions produce clearer failure messages pointing
    at the missing telemetry data.
    """
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if collector.get_trace_requests() or collector.get_metric_requests():
            return
        time.sleep(0.05)


def _send_mcp_initialize(proc: subprocess.Popen[str]) -> None:
    """Send the MCP initialize handshake (request + notification) over stdin."""
    request = {
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": "2024-11-05",
            "capabilities": {},
            "clientInfo": {"name": "bdd-telemetry-tests", "version": "0.0.0"},
        },
    }
    initialized_notification = {
        "jsonrpc": "2.0",
        "method": "notifications/initialized",
    }
    assert proc.stdin is not None
    proc.stdin.write(json.dumps(request) + "\n")
    proc.stdin.write(json.dumps(initialized_notification) + "\n")
    proc.stdin.flush()


def _send_tool_call(
    context: Context,
    proc: subprocess.Popen[str],
    tool_name: str,
    arguments: dict[str, Any],
    call_id: int = 2,
) -> None:
    """Send a JSON-RPC tools/call request, wait for the response, then shut down.

    Closes stdin after reading the response to trigger a clean server shutdown
    (which flushes the TelemetryGuard). Drains stdout and stderr via
    ``proc.communicate()`` to avoid the canonical pipe-buffer deadlock that
    bit this harness during PR #210 review. After shutdown, polls the mock
    collector for OTLP delivery instead of sleeping.
    """
    request = {
        "jsonrpc": "2.0",
        "id": call_id,
        "method": "tools/call",
        "params": {"name": tool_name, "arguments": arguments},
    }
    assert proc.stdin is not None
    assert proc.stdout is not None
    proc.stdin.write(json.dumps(request) + "\n")
    proc.stdin.flush()

    # Read the two JSON-RPC responses. readline() is used intentionally —
    # MCP over stdio writes one JSON-RPC message per line.
    init_line = proc.stdout.readline()
    _assert_not_jsonrpc_error(init_line, "initialize")

    tool_line = proc.stdout.readline()
    _assert_not_jsonrpc_error(tool_line, "tools/call")

    # Close stdin to signal EOF → server shuts down → TelemetryGuard flushes.
    try:
        proc.stdin.close()
    except (BrokenPipeError, ValueError):
        pass  # stdin may already be closed if the server exited

    # Drain remaining stdout + stderr with a timeout to avoid the canonical
    # pipe-buffer deadlock that bit this test harness during PR #210 review.
    try:
        proc.communicate(timeout=10)
    except subprocess.TimeoutExpired:
        proc.kill()
        proc.communicate(timeout=5)

    # Poll the collector for OTLP delivery instead of sleeping — avoids
    # flaky CI on slow runners while keeping a bounded upper wait.
    _wait_for_export(_get_collector(context))


def _spawn_server(
    context: Context,
    *,
    endpoint: str | None,
    cwd: str | None = None,
) -> subprocess.Popen[str]:
    env = os.environ.copy()
    env.pop("GIT_PRISM_OTLP_ENDPOINT", None)
    env.pop("GIT_PRISM_OTLP_HEADERS", None)
    env.pop("GIT_PRISM_SERVICE_NAME", None)
    env.pop("GIT_PRISM_SERVICE_VERSION", None)
    # Scrub any inherited OpenTelemetry env vars from the parent shell
    # (e.g. local SigNoz/OrbStack collector config). The opentelemetry-otlp
    # SDK gives `OTEL_EXPORTER_OTLP_*` env vars higher precedence than the
    # `with_endpoint()` builder call, so a stale parent env can silently
    # redirect exports away from the mock collector.
    for key in list(env):
        if key.startswith("OTEL_"):
            env.pop(key, None)
    if endpoint is not None:
        env["GIT_PRISM_OTLP_ENDPOINT"] = endpoint
    proc = subprocess.Popen(  # noqa: S603 -- test harness, inputs are fixed strings
        [context.binary_path, "serve"],
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
        env=env,
        cwd=cwd,
    )
    context.server_procs = getattr(context, "server_procs", [])
    context.server_procs.append(proc)
    return proc


def _span_has_attribute(span: Any, key: str, value: str) -> bool:
    for attr in span.attributes:
        if attr.key == key:
            return attr.value.string_value == value
    return False


def _attribute_values(span: Any, key: str) -> list[str]:
    return [
        attr.value.string_value
        for attr in span.attributes
        if attr.key == key
    ]


def _all_attribute_strings(collector: MockOtlpCollector) -> list[str]:
    """Return every string-valued attribute across all spans and metrics."""
    strings: list[str] = []
    for span in collector.all_spans():
        for attr in span.attributes:
            if attr.value.string_value:
                strings.append(attr.value.string_value)
    for req in collector.get_trace_requests():
        for resource_spans in req.resource_spans:
            for attr in resource_spans.resource.attributes:
                if attr.value.string_value:
                    strings.append(attr.value.string_value)
    for req in collector.get_metric_requests():
        for resource_metrics in req.resource_metrics:
            for attr in resource_metrics.resource.attributes:
                if attr.value.string_value:
                    strings.append(attr.value.string_value)
            for scope_metrics in resource_metrics.scope_metrics:
                for metric in scope_metrics.metrics:
                    for dp in _metric_data_points(metric):
                        for attr in dp.attributes:
                            if attr.value.string_value:
                                strings.append(attr.value.string_value)
    return strings


def _metric_data_points(metric: Any) -> list[Any]:
    which = metric.WhichOneof("data")
    if which is None:
        return []
    data = getattr(metric, which)
    return list(getattr(data, "data_points", []))


def _collect_repo_shas(repo_path: str) -> list[str]:
    result = subprocess.run(  # noqa: S603,S607 -- test harness
        ["git", "rev-list", "--all"],
        cwd=repo_path,
        capture_output=True,
        text=True,
        check=True,
    )
    return [line.strip() for line in result.stdout.splitlines() if line.strip()]


# ---------- Background and env setup ----------


@given("a mock OTLP collector is running")
def step_mock_collector_running(context: Context) -> None:
    collector = MockOtlpCollector()
    collector.start()
    context.otlp_collector = collector


@given("no telemetry environment variables are set")
def step_no_telemetry_env(context: Context) -> None:
    context.telemetry_endpoint = None


@given("GIT_PRISM_OTLP_ENDPOINT points at the mock collector")
def step_endpoint_set(context: Context) -> None:
    collector = _get_collector(context)
    context.telemetry_endpoint = f"http://localhost:{collector.port}"


# The "a git repository with two commits" step lives in cli_steps.py and
# is reused here — it creates exactly the Rust fixture these scenarios need.


# ---------- Server lifecycle steps ----------


@when('I start "git-prism serve" and send an MCP initialize request')
def step_start_server_init_only(context: Context) -> None:
    endpoint = getattr(context, "telemetry_endpoint", None)
    proc = _spawn_server(context, endpoint=endpoint)
    _send_mcp_initialize(proc)
    # Read the initialize response so it doesn't block the pipe.
    assert proc.stdout is not None
    proc.stdout.readline()


@when(
    'I start "git-prism serve" against that repo and call "{tool_name}"',
)
def step_start_server_and_call_tool(context: Context, tool_name: str) -> None:
    endpoint = getattr(context, "telemetry_endpoint", None)
    repo_path = context.repo_path
    proc = _spawn_server(context, endpoint=endpoint, cwd=repo_path)
    _send_mcp_initialize(proc)
    _send_tool_call(
        context,
        proc,
        tool_name,
        {"base_ref": "HEAD~1", "head_ref": "HEAD", "repo_path": repo_path},
    )


@when(
    'I start "git-prism serve" against that repo and call "{tool_name}" '
    "with an invalid ref",
)
def step_start_server_and_call_tool_invalid_ref(
    context: Context, tool_name: str,
) -> None:
    endpoint = getattr(context, "telemetry_endpoint", None)
    repo_path = context.repo_path
    proc = _spawn_server(context, endpoint=endpoint, cwd=repo_path)
    _send_mcp_initialize(proc)
    _send_tool_call(
        context,
        proc,
        tool_name,
        {
            "base_ref": "does-not-exist-ref-xyzzy",
            "head_ref": "HEAD",
            "repo_path": repo_path,
        },
    )


@when("I wait {seconds:d} seconds for any exports")
def step_wait(context: Context, seconds: int) -> None:
    # Close stdin on all server procs to trigger shutdown → telemetry flush,
    # then drain stdout + stderr via communicate() so we don't deadlock on
    # a full pipe buffer (the canonical proc.wait() hazard).
    for proc in getattr(context, "server_procs", []):
        if proc.stdin and not proc.stdin.closed:
            try:
                proc.stdin.close()
            except (BrokenPipeError, ValueError):
                pass
        try:
            proc.communicate(timeout=seconds)
        except subprocess.TimeoutExpired:
            proc.kill()
            try:
                proc.communicate(timeout=2)
            except subprocess.TimeoutExpired:
                pass


# ---------- Assertions: metrics ----------


@then("the mock collector received zero trace exports")
def step_zero_traces(context: Context) -> None:
    collector = _get_collector(context)
    requests = collector.get_trace_requests()
    assert len(requests) == 0, (
        f"Expected zero trace exports, got {len(requests)} "
        f"(spans: {len(collector.all_spans())}). "
        f"Telemetry should be off by default."
    )


@then("the mock collector received zero metric exports")
def step_zero_metrics(context: Context) -> None:
    collector = _get_collector(context)
    requests = collector.get_metric_requests()
    assert len(requests) == 0, (
        f"Expected zero metric exports, got {len(requests)} "
        f"(metrics: {len(collector.all_metrics())}). "
        f"Telemetry should be off by default."
    )


@then('the mock collector received a metric named "{metric_name}"')
def step_metric_received(context: Context, metric_name: str) -> None:
    collector = _get_collector(context)
    metrics = collector.all_metrics()
    names = [m.name for m in metrics]
    assert metric_name in names, (
        f"Expected metric '{metric_name}' in collector. "
        f"Received metrics: {names!r}"
    )


@then('the "{metric_name}" counter value is at least {minimum:d}')
def step_counter_at_least(
    context: Context, metric_name: str, minimum: int,
) -> None:
    collector = _get_collector(context)
    total = 0
    for metric in collector.all_metrics():
        if metric.name != metric_name:
            continue
        for dp in _metric_data_points(metric):
            total += int(getattr(dp, "as_int", 0) or 0)
    assert total >= minimum, (
        f"Expected '{metric_name}' counter >= {minimum}, got {total}."
    )


@then(
    'that metric has a data point with label "{key}" equal to "{value}"',
)
def step_metric_has_label(context: Context, key: str, value: str) -> None:
    collector = _get_collector(context)
    for metric in collector.all_metrics():
        for dp in _metric_data_points(metric):
            for attr in dp.attributes:
                if attr.key == key and attr.value.string_value == value:
                    return
    all_labels = [
        (metric.name, attr.key, attr.value.string_value)
        for metric in collector.all_metrics()
        for dp in _metric_data_points(metric)
        for attr in dp.attributes
    ]
    msg = (
        f"No metric data point found with label {key}={value!r}. "
        f"All (metric, key, value) labels: {all_labels!r}"
    )
    raise AssertionError(msg)


# ---------- Assertions: spans ----------


@then('the mock collector received a span named "{span_name}"')
def step_span_received(context: Context, span_name: str) -> None:
    collector = _get_collector(context)
    spans = collector.all_spans()
    names = [s.name for s in spans]
    assert span_name in names, (
        f"Expected span '{span_name}' in collector. "
        f"Received spans: {names!r}"
    )


@then('that span has a child span named "{child_name}"')
def step_span_has_child(context: Context, child_name: str) -> None:
    collector = _get_collector(context)
    spans = collector.all_spans()
    # The most recently referenced parent in our scenarios is the root
    # mcp.tool.* span; but we accept any span of that name as the parent.
    # Find candidate parents and confirm at least one child with name=child_name.
    parent_span_ids = {
        s.span_id for s in spans if s.name.startswith("mcp.tool.")
    }
    for span in spans:
        if span.name == child_name and span.parent_span_id in parent_span_ids:
            return
    names = [(s.name, s.span_id.hex(), s.parent_span_id.hex()) for s in spans]
    msg = (
        f"No child span named '{child_name}' found under an mcp.tool.* parent. "
        f"All (name, span_id, parent_span_id): {names!r}"
    )
    raise AssertionError(msg)


@then('that span has status "{status}"')
def step_span_has_status(context: Context, status: str) -> None:
    collector = _get_collector(context)
    # Status code mapping in OTLP: 0=UNSET, 1=OK, 2=ERROR.
    want = {"unset": 0, "ok": 1, "error": 2}[status.lower()]
    for span in collector.all_spans():
        if span.name.startswith("mcp.tool.") and span.status.code == want:
            return
    statuses = [(s.name, s.status.code) for s in collector.all_spans()]
    msg = (
        f"No mcp.tool.* span found with status {status!r} (code {want}). "
        f"All (name, status_code): {statuses!r}"
    )
    raise AssertionError(msg)


@then('that span has an attribute "{key}" equal to "{value}"')
def step_span_has_attribute(context: Context, key: str, value: str) -> None:
    collector = _get_collector(context)
    for span in collector.all_spans():
        if _span_has_attribute(span, key, value):
            return
    found = [
        (span.name, attr.key, attr.value.string_value)
        for span in collector.all_spans()
        for attr in span.attributes
    ]
    msg = (
        f"No span found with attribute {key}={value!r}. "
        f"All (span, key, value): {found!r}"
    )
    raise AssertionError(msg)


# ---------- Assertions: privacy ----------


def _require_some_exports(collector: MockOtlpCollector) -> None:
    # A passing privacy assertion requires something to scan. If the
    # collector received nothing, the scenario is vacuously passing and
    # tells us nothing about real privacy. Fail loudly in that case so
    # BDD bootstrap rightly fails until instrumentation lands.
    assert collector.all_spans() or collector.all_metrics(), (
        "Privacy check attempted but no exports were received. "
        "Telemetry instrumentation must emit at least one span or metric "
        "for this scenario to be meaningful."
    )


@then("no exported attribute contains the raw repo path")
def step_no_raw_repo_path(context: Context) -> None:
    collector = _get_collector(context)
    _require_some_exports(collector)
    repo_path = context.repo_path
    strings = _all_attribute_strings(collector)
    matches = [s for s in strings if repo_path in s]
    assert not matches, (
        f"Found raw repo path {repo_path!r} in exported attributes: "
        f"{matches!r}"
    )


@then("no exported attribute contains any commit SHA from the repo")
def step_no_raw_shas(context: Context) -> None:
    collector = _get_collector(context)
    _require_some_exports(collector)
    shas = _collect_repo_shas(context.repo_path)
    strings = _all_attribute_strings(collector)
    leaks = [
        (sha, s)
        for sha in shas
        for s in strings
        if sha in s or sha[:12] in s
    ]
    assert not leaks, (
        f"Found commit SHA(s) leaked in exported attributes: {leaks!r}"
    )


@then(
    'every "{key}" span attribute value is in '
    '{{"worktree", "single_commit", "range_double_dot", '
    '"range_triple_dot", "branch", "sha"}}',
)
def step_ref_pattern_bounded(context: Context, key: str) -> None:
    collector = _get_collector(context)
    values: list[str] = []
    for span in collector.all_spans():
        values.extend(_attribute_values(span, key))
    assert values, (
        f"No span attribute named {key!r} was exported. "
        f"Expected at least one bounded ref-pattern value."
    )
    bad = [v for v in values if v not in VALID_REF_PATTERNS]
    assert not bad, (
        f"Ref-pattern attribute {key!r} has out-of-enum values: {bad!r}. "
        f"Valid values: {sorted(VALID_REF_PATTERNS)!r}"
    )


# ---------- Lifecycle hooks for telemetry scenarios ----------


def _stop_server_procs(context: Context) -> None:
    """Terminate any spawned git-prism processes, draining their pipes."""
    procs = getattr(context, "server_procs", [])
    for proc in procs:
        if proc.poll() is not None:
            continue  # already exited
        proc.terminate()
        try:
            proc.communicate(timeout=5)
        except subprocess.TimeoutExpired:
            proc.kill()
            try:
                proc.communicate(timeout=2)
            except subprocess.TimeoutExpired:
                pass  # zombie — log and move on
    context.server_procs = []


def _stop_collector(context: Context) -> None:
    collector = getattr(context, "otlp_collector", None)
    if collector is not None:
        collector.stop()
        context.otlp_collector = None


# Ensure cleanup even if a step raises. behave calls step-defined
# `after_scenario` hooks via environment.py, so we expose helpers the
# environment hook can call by name.
def telemetry_after_scenario(context: Context) -> None:
    _stop_server_procs(context)
    _stop_collector(context)


__all__ = ["MockOtlpCollector", "telemetry_after_scenario"]
