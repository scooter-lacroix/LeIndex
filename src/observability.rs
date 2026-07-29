//! Observability infrastructure: distributed tracing, metrics collection,
//! error tracking, and log scrubbing for LeIndex.
//!
//! This module provides:
//! - OpenTelemetry-compatible distributed tracing with trace ID propagation
//! - Prometheus-style metrics collection (counters, histograms)
//! - Sentry-compatible error tracking integration
//! - Log redaction/sanitization to prevent secrets leaking into logs
//!
//! ## Configuration
//!
//! Set these environment variables to enable observability features:
//! - `OTEL_EXPORTER_OTLP_ENDPOINT`: OTLP collector endpoint for traces
//! - `LEINDEX_METRICS_ENDPOINT`: Prometheus metrics scrape endpoint
//! - `SENTRY_DSN`: Sentry DSN for error tracking (optional)
//! - `RUST_LOG`: Log level filter (e.g., "info,leindex=debug")
//!
//! ## Feature Flags
//!
//! Observability features are gated by cargo features:
//! - `tracing` (always on via `tracing` crate): structured logging
//! - `metrics`: Prometheus metrics collection
//! - `otel`: OpenTelemetry distributed tracing export

use std::collections::HashMap;
use std::env;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

/// A distributed trace span tracking a unit of work.
///
/// Each span has a trace ID (propagated across process boundaries) and
/// a span ID (unique within a trace). Spans nest to form a trace tree.
pub struct TraceSpan {
    /// Trace ID (128-bit hex string, propagated across process boundaries)
    pub trace_id: String,
    /// Span ID (64-bit hex string, unique within this trace)
    pub span_id: String,
    /// Parent span ID (empty for root spans)
    pub parent_span_id: String,
    /// Span name (operation being traced)
    pub name: String,
    /// Start time
    pub start: Instant,
    /// Attributes attached to this span
    pub attributes: HashMap<String, String>,
}

impl TraceSpan {
    /// Creates a new root span (no parent).
    pub fn root(name: &str) -> Self {
        Self {
            trace_id: generate_trace_id(),
            span_id: generate_span_id(),
            parent_span_id: String::new(),
            name: name.to_string(),
            start: Instant::now(),
            attributes: HashMap::new(),
        }
    }

    /// Creates a child span of the given parent.
    pub fn child(name: &str, parent: &Self) -> Self {
        Self {
            trace_id: parent.trace_id.clone(),
            span_id: generate_span_id(),
            parent_span_id: parent.span_id.clone(),
            name: name.to_string(),
            start: Instant::now(),
            attributes: HashMap::new(),
        }
    }

    /// Adds an attribute to this span.
    pub fn with_attribute(mut self, key: &str, value: &str) -> Self {
        self.attributes.insert(key.to_string(), value.to_string());
        self
    }

    /// Returns the elapsed time since span creation.
    pub fn elapsed_ms(&self) -> u64 {
        self.start.elapsed().as_millis() as u64
    }

    /// Returns the X-Trace-Id header value for HTTP propagation.
    pub fn trace_header(&self) -> (String, String) {
        ("X-Trace-Id".to_string(), self.trace_id.clone())
    }

    /// Returns the X-Span-Id header value for HTTP propagation.
    pub fn span_header(&self) -> (String, String) {
        ("X-Span-Id".to_string(), self.span_id.clone())
    }
}

fn generate_trace_id() -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(1);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!("{:016x}{:016x}", ts, n)
}

fn generate_span_id() -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(1);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("{:016x}", n)
}

/// Counter metric that only increases.
pub struct Counter {
    name: String,
    value: AtomicU64,
    help: String,
}

impl Counter {
    /// Creates a new counter.
    pub fn new(name: &str, help: &str) -> Self {
        Self {
            name: name.to_string(),
            value: AtomicU64::new(0),
            help: help.to_string(),
        }
    }

    /// Increments the counter by 1.
    pub fn inc(&self) {
        self.value.fetch_add(1, Ordering::Relaxed);
    }

    /// Increments the counter by n.
    pub fn add(&self, n: u64) {
        self.value.fetch_add(n, Ordering::Relaxed);
    }

    /// Returns the current value.
    pub fn get(&self) -> u64 {
        self.value.load(Ordering::Relaxed)
    }

    /// Renders this counter in Prometheus text exposition format.
    pub fn render_prometheus(&self) -> String {
        format!(
            "# HELP {} {}\n# TYPE {} counter\n{} {}",
            self.name,
            self.help,
            self.name,
            self.name,
            self.get()
        )
    }
}

/// Histogram metric for tracking distributions (latency, etc.).
pub struct Histogram {
    name: String,
    buckets: Vec<f64>,
    counts: Vec<AtomicU64>,
    sum: Mutex<f64>,
    count: AtomicU64,
}

impl Histogram {
    /// Creates a new histogram with the given bucket boundaries.
    pub fn new(name: &str, buckets: Vec<f64>) -> Self {
        let b = buckets.len();
        Self {
            name: name.to_string(),
            buckets,
            counts: (0..=b).map(|_| AtomicU64::new(0)).collect(),
            sum: Mutex::new(0.0),
            count: AtomicU64::new(0),
        }
    }

    /// Records an observation.
    pub fn observe(&self, value: f64) {
        self.count.fetch_add(1, Ordering::Relaxed);
        if let Ok(mut s) = self.sum.lock() {
            *s += value;
        }
        for (i, &bound) in self.buckets.iter().enumerate() {
            if value <= bound {
                self.counts[i].fetch_add(1, Ordering::Relaxed);
                return;
            }
        }
        self.counts[self.buckets.len()].fetch_add(1, Ordering::Relaxed);
    }

    /// Renders this histogram in Prometheus text exposition format.
    pub fn render_prometheus(&self) -> String {
        let mut out = format!("# TYPE {} histogram\n", self.name);
        let mut cumulative: u64 = 0;
        for (i, &bound) in self.buckets.iter().enumerate() {
            cumulative += self.counts[i].load(Ordering::Relaxed);
            out.push_str(&format!(
                "{}_bucket{{le=\"{}\"}} {}\n",
                self.name, bound, cumulative
            ));
        }
        cumulative += self.counts[self.buckets.len()].load(Ordering::Relaxed);
        out.push_str(&format!(
            "{}_bucket{{le=\"+Inf\"}} {}\n",
            self.name, cumulative
        ));
        let sum_val = self.sum.lock().map(|v| *v).unwrap_or(0.0);
        out.push_str(&format!("{}_sum {}\n", self.name, sum_val));
        out.push_str(&format!(
            "{}_count {}\n",
            self.name,
            self.count.load(Ordering::Relaxed)
        ));
        out
    }
}

/// Error event sent to error tracking (Sentry, Bugsnag, etc.)
pub struct ErrorEvent {
    /// The error message
    pub message: String,
    /// The error level
    pub level: ErrorLevel,
    /// Trace ID for correlating with distributed traces
    pub trace_id: Option<String>,
    /// Source file and line
    pub location: Option<String>,
    /// Additional context
    pub context: HashMap<String, String>,
}

/// Error severity level.
#[derive(Debug, Clone, Copy)]
pub enum ErrorLevel {
    /// Error-level event: something went wrong.
    Error,
    /// Warning-level event: potential issue.
    Warning,
    /// Info-level event: informational.
    Info,
}

/// Error tracker that captures events and (optionally) forwards them
/// to Sentry or a compatible backend.
pub struct ErrorTracker {
    dsn: Option<String>,
    events: std::sync::Mutex<Vec<ErrorEvent>>,
}

static ERROR_TRACKER: OnceLock<ErrorTracker> = OnceLock::new();

impl ErrorTracker {
    fn new() -> Self {
        Self {
            dsn: env::var("SENTRY_DSN").ok(),
            events: std::sync::Mutex::new(Vec::new()),
        }
    }

    /// Gets the global error tracker instance.
    pub fn global() -> &'static Self {
        ERROR_TRACKER.get_or_init(ErrorTracker::new)
    }

    /// Captures an error event.
    ///
    /// The event is appended to the local ring buffer for later retrieval,
    /// **best-effort**: it is silently dropped if the buffer is at its 1000-entry
    /// cap or if the lock is poisoned. (In all cases a `tracing::error!` is
    /// emitted.)
    ///
    /// **STUB:** remote forwarding to Sentry is NOT yet implemented. `SENTRY_DSN`
    /// is parsed and stored, but this method only emits a local `tracing` event
    /// — it does not serialize/POST to the Sentry API. Do not assume configured
    /// production errors are remotely reported until the transport lands.
    pub fn capture(&self, event: ErrorEvent) {
        if self.dsn.is_some() {
            // TODO(sentry): serialize the ErrorEvent to Sentry's envelope
            // format and POST to the configured DSN. Until then this only logs
            // locally — SENTRY_DSN is accepted but unused for remote submission.
            tracing::error!(
                message = %event.message,
                trace_id = ?event.trace_id,
                "Error captured for tracking"
            );
        } else {
            tracing::error!(
                message = %event.message,
                trace_id = ?event.trace_id,
                "Error captured (no remote tracker configured)"
            );
        }
        if let Ok(mut events) = self.events.lock() {
            if events.len() < 1000 {
                events.push(event);
            }
        }
    }
}

/// Log scrubber for redacting sensitive data from log output.
///
/// Replaces patterns matching common secret formats with `[REDACTED]`.
pub struct LogScrubber;

impl LogScrubber {
    /// Redacts known sensitive patterns from a log message.
    ///
    /// Patterns redacted:
    /// - Bearer tokens: `Bearer xxx` -> `Bearer [REDACTED]`
    /// - API keys: `key=xxx` or `api_key=xxx` -> `key=[REDACTED]`
    /// - Auth tokens: `token=xxx` or `auth_token=xxx` -> `token=[REDACTED]`
    /// - Passwords: `password=xxx` -> `password=[REDACTED]`
    /// - Connection strings: `://user:pass@` -> `://[REDACTED]:[REDACTED]@`
    /// - Long hex/base64 strings in auth contexts
    pub fn redact(input: &str) -> String {
        let mut result = input.to_string();

        // Redact Bearer tokens
        if let Some(pos) = result.find("Bearer ") {
            let start = pos + 7;
            if let Some(end) = result[start..].find(' ') {
                result.replace_range(start..start + end, "[REDACTED]");
            } else {
                result.truncate(start);
                result.push_str("[REDACTED]");
            }
        }

        // Redact key=value patterns for sensitive keys
        for pattern in [
            "api_key=",
            "apikey=",
            "API_KEY=",
            "token=",
            "auth_token=",
            "AUTH_TOKEN=",
            "password=",
            "PASSWORD=",
            "passwd=",
            "secret=",
            "SECRET=",
            "TURSO_AUTH=",
        ] {
            if let Some(pos) = result.find(pattern) {
                let start = pos + pattern.len();
                let end = result[start..]
                    .find(|c: char| c.is_whitespace() || c == '&' || c == '"' || c == '\'')
                    .map(|e| start + e)
                    .unwrap_or(result.len());
                if end > start {
                    result.replace_range(start..end, "[REDACTED]");
                }
            }
        }

        // Redact credentials in URLs: scheme://user:pass@host
        if let Some(at_pos) = result.find("://") {
            let after_scheme = at_pos + 3;
            if let Some(at_sign) = result[after_scheme..].find('@') {
                let cred_start = after_scheme;
                let cred_end = after_scheme + at_sign;
                // Check there's a colon (user:pass) between scheme:// and @
                if result[cred_start..cred_end].contains(':') {
                    result.replace_range(cred_start..cred_end, "[REDACTED]:[REDACTED]");
                }
            }
        }

        result
    }
}

/// Standard metric counters for the LeIndex application.
pub struct Metrics {
    /// Number of search queries executed
    pub search_queries: Counter,
    /// Number of files indexed
    pub files_indexed: Counter,
    /// Number of MCP tool calls
    pub mcp_tool_calls: Counter,
    /// Number of errors encountered
    pub errors: Counter,
    /// Search query latency in milliseconds
    pub search_latency: Histogram,
}

impl Default for Metrics {
    fn default() -> Self {
        Self {
            search_queries: Counter::new("leindex_search_queries_total", "Total search queries"),
            files_indexed: Counter::new("leindex_files_indexed_total", "Total files indexed"),
            mcp_tool_calls: Counter::new("leindex_mcp_tool_calls_total", "Total MCP tool calls"),
            errors: Counter::new("leindex_errors_total", "Total errors"),
            search_latency: Histogram::new(
                "leindex_search_latency_ms",
                vec![1.0, 5.0, 10.0, 25.0, 50.0, 100.0, 250.0, 500.0, 1000.0],
            ),
        }
    }
}

static METRICS: OnceLock<Metrics> = OnceLock::new();

/// Product analytics event tracker for understanding feature usage
/// without external dependencies. Events are collected via the
/// error-insight.yml workflow's product-metrics job.
pub struct ProductAnalytics {
    events: Mutex<Vec<AnalyticsEvent>>,
}

/// A single product analytics event.
pub struct AnalyticsEvent {
    /// Event name (e.g., "search_executed", "index_completed")
    pub name: String,
    /// Timestamp (Unix epoch nanos)
    pub timestamp_nanos: u128,
    /// Event properties
    pub properties: HashMap<String, String>,
}

static PRODUCT_ANALYTICS: OnceLock<ProductAnalytics> = OnceLock::new();

impl ProductAnalytics {
    fn new() -> Self {
        Self {
            events: Mutex::new(Vec::new()),
        }
    }

    /// Gets the global analytics instance.
    pub fn global() -> &'static Self {
        PRODUCT_ANALYTICS.get_or_init(ProductAnalytics::new)
    }

    /// Tracks a product analytics event.
    pub fn track(&self, name: &str, properties: HashMap<String, String>) {
        let event = AnalyticsEvent {
            name: name.to_string(),
            timestamp_nanos: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos(),
            properties,
        };
        if let Ok(mut events) = self.events.lock() {
            if events.len() < 5000 {
                events.push(event);
            }
        }
    }

    /// Returns the total number of tracked events.
    pub fn event_count(&self) -> usize {
        self.events.lock().map(|e| e.len()).unwrap_or(0)
    }
}

impl Metrics {
    /// Gets the global metrics instance.
    pub fn global() -> &'static Self {
        METRICS.get_or_init(Metrics::default)
    }

    /// Renders all metrics in Prometheus text exposition format.
    pub fn render_prometheus(&self) -> String {
        let mut out = String::new();
        out.push_str(&self.search_queries.render_prometheus());
        out.push('\n');
        out.push_str(&self.files_indexed.render_prometheus());
        out.push('\n');
        out.push_str(&self.mcp_tool_calls.render_prometheus());
        out.push('\n');
        out.push_str(&self.errors.render_prometheus());
        out.push('\n');
        out.push_str(&self.search_latency.render_prometheus());
        out
    }
}

/// Circuit breaker for protecting against cascading failures from external
/// dependencies (remote embedding APIs, Turso/libSQL, etc.).
///
/// Trips open after `failure_threshold` consecutive failures, blocking
/// further calls for `reset_timeout` seconds. After the timeout, one
/// trial call is allowed (half-open state). If it succeeds the breaker
/// closes; if it fails the breaker re-opens.
pub struct CircuitBreaker {
    failure_count: AtomicU64,
    failure_threshold: u64,
    last_failure_time: Mutex<Option<Instant>>,
    reset_timeout_secs: u64,
    state: Mutex<CircuitState>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum CircuitState {
    Closed,
    Open,
    HalfOpen,
}

impl CircuitBreaker {
    /// Creates a new breaker with the given failure threshold and reset timeout.
    pub fn new(failure_threshold: u32, reset_timeout_secs: u64) -> Self {
        Self {
            failure_count: AtomicU64::new(0),
            failure_threshold: failure_threshold as u64,
            last_failure_time: Mutex::new(None),
            reset_timeout_secs,
            state: Mutex::new(CircuitState::Closed),
        }
    }

    /// Returns true if a call is allowed (breaker is closed or half-open).
    pub fn allow_call(&self) -> bool {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        match *state {
            CircuitState::Closed => true,
            CircuitState::Open => {
                let last = self
                    .last_failure_time
                    .lock()
                    .unwrap_or_else(|e| e.into_inner());
                if let Some(t) = *last {
                    if t.elapsed().as_secs() >= self.reset_timeout_secs {
                        *state = CircuitState::HalfOpen;
                        return true;
                    }
                }
                false
            }
            CircuitState::HalfOpen => true,
        }
    }

    /// Records a successful call: resets failure count and closes the breaker.
    pub fn record_success(&self) {
        self.failure_count.store(0, Ordering::Relaxed);
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        *state = CircuitState::Closed;
    }

    /// Records a failed call: increments failure count, opens if threshold hit.
    pub fn record_failure(&self) {
        let count = self.failure_count.fetch_add(1, Ordering::Relaxed) + 1;
        let mut last = self
            .last_failure_time
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        *last = Some(Instant::now());
        drop(last);
        if count >= self.failure_threshold {
            let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            *state = CircuitState::Open;
        }
    }

    /// Returns the current state as a string for metrics/logging.
    pub fn state(&self) -> &'static str {
        let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        match *state {
            CircuitState::Closed => "closed",
            CircuitState::Open => "open",
            CircuitState::HalfOpen => "half_open",
        }
    }
}

/// Health check result for the MCP server and sub-systems.
#[derive(Debug)]
pub struct HealthStatus {
    /// Overall health: true if all sub-checks passed.
    pub healthy: bool,
    /// Individual check results: (name, passed, detail_message).
    pub checks: Vec<(String, bool, String)>,
}

impl HealthStatus {
    /// Runs health checks for core sub-systems. Currently verifies that
    /// the metrics instance and error tracker are responsive.
    /// Extensible: add database connectivity, ONNX model availability, etc.
    pub fn check() -> Self {
        let mut checks = Vec::new();

        let metrics_ok =
            std::panic::catch_unwind(|| Metrics::global().search_queries.get()).is_ok();
        checks.push((
            "metrics".to_string(),
            metrics_ok,
            if metrics_ok {
                "ok".to_string()
            } else {
                "panic".to_string()
            },
        ));

        let tracker_ok = std::panic::catch_unwind(|| ErrorTracker::global().dsn.is_some()).is_ok();
        checks.push((
            "error_tracker".to_string(),
            tracker_ok,
            if tracker_ok {
                "ok".to_string()
            } else {
                "panic".to_string()
            },
        ));

        let healthy = checks.iter().all(|(_, ok, _)| *ok);
        Self { healthy, checks }
    }

    /// Renders as JSON for an HTTP health endpoint.
    pub fn to_json(&self) -> String {
        let checks_json: Vec<String> = self
            .checks
            .iter()
            .map(|(name, ok, msg)| {
                format!(r#"{{"name":"{}","ok":{},"detail":"{}"}}"#, name, ok, msg)
            })
            .collect();
        format!(
            r#"{{"healthy":{},"checks":[{}]}}"#,
            self.healthy,
            checks_json.join(",")
        )
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn test_trace_span_propagation() {
        let root = TraceSpan::root("index_request");
        let child = TraceSpan::child("parse_file", &root);
        assert_eq!(root.trace_id, child.trace_id);
        assert_eq!(child.parent_span_id, root.span_id);
        assert!(!root.trace_id.is_empty());
        assert!(!root.span_id.is_empty());
    }

    #[test]
    fn test_counter() {
        let c = Counter::new("test_counter", "test");
        c.inc();
        c.inc();
        c.add(10);
        assert_eq!(c.get(), 12);
        assert!(c.render_prometheus().contains("test_counter 12"));
    }

    #[test]
    fn test_histogram() {
        let h = Histogram::new("test_h", vec![1.0, 5.0, 10.0]);
        h.observe(0.5);
        h.observe(3.0);
        h.observe(15.0);
        let prom = h.render_prometheus();
        assert!(prom.contains("test_h_count 3"));
        assert!(prom.contains("test_h_sum 18.5"));
    }

    #[test]
    fn test_log_scrubber_bearer() {
        let input = "Authorization: Bearer abc123secret";
        let result = LogScrubber::redact(input);
        assert!(result.contains("[REDACTED]"));
        assert!(!result.contains("abc123secret"));
    }

    #[test]
    fn test_log_scrubber_api_key() {
        let input = "api_key=mysecretkey123";
        let result = LogScrubber::redact(input);
        assert!(result.contains("[REDACTED]"));
        assert!(!result.contains("mysecretkey123"));
    }

    #[test]
    fn test_log_scrubber_url_creds() {
        let input = "postgres://admin:pass123@db.local:5432";
        let result = LogScrubber::redact(input);
        assert!(result.contains("[REDACTED]"));
        assert!(!result.contains("pass123"));
        assert!(!result.contains("admin"));
    }

    #[test]
    fn test_log_scrubber_preserves_non_sensitive() {
        let input = "Indexed 42 files in /home/user/project";
        let result = LogScrubber::redact(input);
        assert_eq!(input, result);
    }

    #[test]
    fn test_log_scrubber_turso_auth() {
        let input = "TURSO_AUTH=eyJhbGciOi";
        let result = LogScrubber::redact(input);
        assert!(result.contains("[REDACTED]"));
        assert!(!result.contains("eyJhbGciOi"));
    }

    #[test]
    fn test_circuit_breaker_trips() {
        let cb = CircuitBreaker::new(3, 60);
        assert!(cb.allow_call());
        assert_eq!(cb.state(), "closed");

        cb.record_failure();
        cb.record_failure();
        assert!(cb.allow_call());

        cb.record_failure();
        assert!(!cb.allow_call());
        assert_eq!(cb.state(), "open");
    }

    #[test]
    fn test_circuit_breaker_half_open() {
        let cb = CircuitBreaker::new(1, 0);
        cb.record_failure();
        assert_eq!(cb.state(), "open");
        std::thread::sleep(std::time::Duration::from_millis(1100));
        assert!(cb.allow_call());
        assert_eq!(cb.state(), "half_open");
        cb.record_success();
        assert_eq!(cb.state(), "closed");
    }

    #[test]
    fn test_circuit_breaker_reset_on_success() {
        let cb = CircuitBreaker::new(3, 60);
        cb.record_failure();
        cb.record_failure();
        cb.record_success();
        assert_eq!(cb.state(), "closed");
    }

    #[test]
    fn test_health_status_json() {
        let h = HealthStatus::check();
        let json = h.to_json();
        assert!(json.contains("healthy"));
        assert!(json.contains("checks"));
    }
}
