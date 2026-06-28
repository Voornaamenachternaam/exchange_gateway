// src/metrics.rs
//! Observability metrics using Prometheus.
//! Provides request latency histograms, error counters, and application-specific gauges.

use std::{
    collections::HashMap,
    time::{Duration, Instant},
    sync::Arc,
    sync::LazyLock,
};
use prometheus::{
    Counter, CounterVec, Gauge, GaugeVec, Histogram, HistogramOpts, HistogramVec, Opts,
    Registry,
};
use axum::{
    body::Body,
    extract::State,
    http::{Request, StatusCode},
    middleware::Next,
    response::Response,
};
use crate::AppState;

/// Request latency buckets in seconds: 1ms, 5ms, 10ms, 25ms, 50ms, 100ms, 250ms, 500ms, 1s, 2.5s, 5s, 10s
pub const REQUEST_LATENCY_BUCKETS: &[f64] = &[
    0.001, 0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0,
];

/// Global Prometheus registry.
pub static REGISTRY: LazyLock<Arc<Registry>> = LazyLock::new(|| Arc::new(Registry::new()));

/// Metric name constants.
pub const REQUEST_COUNT: &str = "http_requests_total";
pub const REJECTION_COUNT: &str = "http_request_rejections_total";

/// Normalize a request path for metric labels by replacing identifier-like segments with {id}.
/// This prevents high cardinality from dynamic path elements (UUIDs, numeric IDs).
/// Heuristic: any segment consisting only of alphanumerics, '-' or '_' with length >= 8 is considered an ID.
fn normalize_path(path: &str) -> String {
    let mut segments = Vec::new();
    for seg in path.split('/') {
        if seg.is_empty() {
            segments.push(seg.to_string());
            continue;
        }
        let mut is_id_candidate = true;
        let mut len = 0;
        for c in seg.chars() {
            len += 1;
            if !(c.is_ascii_alphanumeric() || c == '-' || c == '_') {
                is_id_candidate = false;
                break;
            }
        }
        if is_id_candidate && len >= 8 {
            segments.push("{id}".to_string());
        } else {
            segments.push(seg.to_string());
        }
    }
    segments.join("/")
}

/// HTTP request metrics.
#[derive(Clone)]
pub struct HttpMetrics {
    /// Total requests counter (by method, path, status).
    pub requests_total: CounterVec,
    /// Request latency histogram (by method, path, status).
    pub request_duration_seconds: HistogramVec,
    /// In-flight requests gauge (by method, path).
    pub requests_in_flight: GaugeVec,
    /// Rejected requests counter (by reason, scope).
    pub request_rejections: CounterVec,
}

impl HttpMetrics {
    /// Create a new HttpMetrics instance with the given registry.
    pub fn new(registry: &Registry) -> Self {
        let requests_total = CounterVec::new(
            Opts::new("http_requests_total", "Total number of HTTP requests processed")
                .const_labels(HashMap::from([("service".to_string(), "exchange_gateway".to_string())])),
            &["method", "path", "status"],
        )
        .unwrap();
        registry.register(Box::new(requests_total.clone())).unwrap();

        let opts = HistogramOpts::new(
            "http_request_duration_seconds",
            "HTTP request duration in seconds",
        )
        .const_labels(HashMap::from([("service".to_string(), "exchange_gateway".to_string())]))
        .buckets(REQUEST_LATENCY_BUCKETS.to_vec());
        let request_duration_seconds = HistogramVec::new(opts, &["method", "path", "status"]).unwrap();
        registry.register(Box::new(request_duration_seconds.clone())).unwrap();

        let requests_in_flight = GaugeVec::new(
            Opts::new(
                "http_requests_in_flight",
                "Current number of in-flight HTTP requests",
            )
            .const_labels(HashMap::from([("service".to_string(), "exchange_gateway".to_string())])),
            &["method", "path"],
        )
        .unwrap();
        registry.register(Box::new(requests_in_flight.clone())).unwrap();

        let request_rejections = CounterVec::new(
            Opts::new(
                "http_request_rejections_total",
                "Total number of rejected requests (rate limit, etc.)",
            )
            .const_labels(HashMap::from([("service".to_string(), "exchange_gateway".to_string())])),
            &["reason", "scope"],
        )
        .unwrap();
        registry.register(Box::new(request_rejections.clone())).unwrap();

        Self {
            requests_total,
            request_duration_seconds,
            requests_in_flight,
            request_rejections,
        }
    }

    /// Record a request start (increment in-flight gauge).
    pub fn inc_in_flight(&self, method: &str, path: &str) {
        self.requests_in_flight.with_label_values(&[method, path]).inc();
    }

    /// Record a request end (decrement in-flight gauge and record duration).
    pub fn record_request(&self, method: &str, path: &str, status: StatusCode, elapsed: Duration) {
        let status = status.as_u16().to_string();
        let elapsed_secs = elapsed.as_secs_f64();
        // Decrement in-flight.
        self.requests_in_flight.with_label_values(&[method, path]).dec();
        self.requests_total
            .with_label_values(&[method, path, &status])
            .inc();
        self.request_duration_seconds
            .with_label_values(&[method, path, &status])
            .observe(elapsed_secs);
    }
}

/// Backend health metrics.
#[derive(Clone)]
pub struct BackendMetrics {
    /// Gauge for backend health status (1=healthy, 0=unhealthy).
    pub backend_health: GaugeVec,
}

impl BackendMetrics {
    pub fn new(registry: &Registry) -> Self {
        let backend_health = GaugeVec::new(
            Opts::new(
                "backend_health",
                "Health status of backends (1=healthy, 0=unhealthy)",
            )
            .const_labels(HashMap::from([("service".to_string(), "exchange_gateway".to_string())])),
            &["backend", "backend_type"],
        )
        .unwrap();
        registry.register(Box::new(backend_health.clone())).unwrap();

        Self { backend_health }
    }

    /// Set health status for a specific backend.
    pub fn set_backend_health(&self, backend: &str, backend_type: &str, healthy: bool) {
        self.backend_health
            .with_label_values(&[backend, backend_type])
            .set(if healthy { 1.0 } else { 0.0 });
    }
}

/// Subscriptions and connectivity metrics.
#[derive(Clone)]
pub struct SubscriptionMetrics {
    /// Active subscriptions gauge.
    pub active_subscriptions: Gauge,
    /// Total subscription creations.
    pub subscriptions_created_total: Counter,
    /// Total subscription deletions.
    pub subscriptions_deleted_total: Counter,
    /// Total notifications sent.
    pub notifications_sent_total: Counter,
    /// Subscription duration histogram.
    pub subscription_duration_seconds: Histogram,
}

impl SubscriptionMetrics {
    pub fn new(registry: &Registry) -> Self {
        let active_subscriptions = Gauge::with_opts(
            Opts::new(
                "active_subscriptions",
                "Number of currently active subscriptions",
            )
            .const_labels(HashMap::from([("service".to_string(), "exchange_gateway".to_string())])),
        )
        .unwrap();
        registry.register(Box::new(active_subscriptions.clone())).unwrap();

        let subscriptions_created_total = Counter::with_opts(
            Opts::new(
                "subscriptions_created_total",
                "Total number of subscriptions created",
            )
            .const_labels(HashMap::from([("service".to_string(), "exchange_gateway".to_string())])),
        )
        .unwrap();
        registry.register(Box::new(subscriptions_created_total.clone())).unwrap();

        let subscriptions_deleted_total = Counter::with_opts(
            Opts::new(
                "subscriptions_deleted_total",
                "Total number of subscriptions deleted",
            )
            .const_labels(HashMap::from([("service".to_string(), "exchange_gateway".to_string())])),
        )
        .unwrap();
        registry.register(Box::new(subscriptions_deleted_total.clone())).unwrap();

        let notifications_sent_total = Counter::with_opts(
            Opts::new(
                "notifications_sent_total",
                "Total number of notifications sent to subscribers",
            )
            .const_labels(HashMap::from([("service".to_string(), "exchange_gateway".to_string())])),
        )
        .unwrap();
        registry.register(Box::new(notifications_sent_total.clone())).unwrap();

        let subscription_duration_seconds = Histogram::with_opts(
            HistogramOpts::new(
                "subscription_duration_seconds",
                "Subscription lifetime in seconds",
            )
            .const_labels(HashMap::from([("service".to_string(), "exchange_gateway".to_string())]))
            .buckets(REQUEST_LATENCY_BUCKETS.to_vec()),
        )
        .unwrap();
        registry.register(Box::new(subscription_duration_seconds.clone())).unwrap();

        Self {
            active_subscriptions,
            subscriptions_created_total,
            subscriptions_deleted_total,
            notifications_sent_total,
            subscription_duration_seconds,
        }
    }
}

/// Email and calendar operation metrics.
#[derive(Clone)]
pub struct OperationMetrics {
    /// Total backend API calls (JMAP/CalDAV).
    pub backend_calls_total: CounterVec,
    /// Backend call latency (by operation, backend).
    pub backend_call_duration_seconds: HistogramVec,
    /// Failed backend calls counter.
    pub backend_call_errors_total: CounterVec,
    /// Attachments processed counter.
    pub attachments_processed_total: CounterVec,
    /// Email send operations counter.
    pub email_send_total: CounterVec,
    /// Calendar operation metrics (by type).
    pub calendar_operations_total: CounterVec,
    /// Email operation metrics.
    pub email_operations_total: CounterVec,
}

impl OperationMetrics {
    pub fn new(registry: &Registry) -> Self {
        let backend_calls_total = CounterVec::new(
            Opts::new("backend_calls_total", "Total backend API calls made")
                .const_labels(HashMap::from([("service".to_string(), "exchange_gateway".to_string())])),
            &["operation", "backend"],
        )
        .unwrap();
        registry.register(Box::new(backend_calls_total.clone())).unwrap();

        let backend_call_duration_seconds = HistogramVec::new(
            HistogramOpts::new(
                "backend_call_duration_seconds",
                "Backend API call duration in seconds",
            )
            .const_labels(HashMap::from([("service".to_string(), "exchange_gateway".to_string())]))
            .buckets(REQUEST_LATENCY_BUCKETS.to_vec()),
            &["operation", "backend"],
        )
        .unwrap();
        registry.register(Box::new(backend_call_duration_seconds.clone())).unwrap();

        let backend_call_errors_total = CounterVec::new(
            Opts::new("backend_call_errors_total", "Total backend API call errors")
                .const_labels(HashMap::from([("service".to_string(), "exchange_gateway".to_string())])),
            &["operation", "backend", "error_type"],
        )
        .unwrap();
        registry.register(Box::new(backend_call_errors_total.clone())).unwrap();

        let attachments_processed_total = CounterVec::new(
            Opts::new("attachments_processed_total", "Total attachments processed")
                .const_labels(HashMap::from([("service".to_string(), "exchange_gateway".to_string())])),
            &["operation"], // create, get, delete
        )
        .unwrap();
        registry.register(Box::new(attachments_processed_total.clone())).unwrap();

        let email_send_total = CounterVec::new(
            Opts::new("email_send_total", "Total email send operations")
                .const_labels(HashMap::from([("service".to_string(), "exchange_gateway".to_string())])),
            &["method"], // jmap, smtp
        )
        .unwrap();
        registry.register(Box::new(email_send_total.clone())).unwrap();

        let calendar_operations_total = CounterVec::new(
            Opts::new("calendar_operations_total", "Total calendar operations")
                .const_labels(HashMap::from([("service".to_string(), "exchange_gateway".to_string())])),
            &["operation", "protocol"], // get, query, set, delete, freebusy; jmap, caldav
        )
        .unwrap();
        registry.register(Box::new(calendar_operations_total.clone())).unwrap();

        let email_operations_total = CounterVec::new(
            Opts::new("email_operations_total", "Total email operations")
                .const_labels(HashMap::from([("service".to_string(), "exchange_gateway".to_string())])),
            &["operation", "protocol"], // query, get, changes, send, set
        )
        .unwrap();
        registry.register(Box::new(email_operations_total.clone())).unwrap();

        Self {
            backend_calls_total,
            backend_call_duration_seconds,
            backend_call_errors_total,
            attachments_processed_total,
            email_send_total,
            calendar_operations_total,
            email_operations_total,
        }
    }
}

/// Central application metrics container.
#[derive(Clone)]
pub struct AppMetrics {
    pub http: HttpMetrics,
    pub backend: BackendMetrics,
    pub subscriptions: SubscriptionMetrics,
    pub operations: OperationMetrics,
}

impl AppMetrics {
    /// Initialize all metrics with the global registry.
    pub fn new() -> Self {
        let registry = REGISTRY.clone();
        Self {
            http: HttpMetrics::new(&registry),
            backend: BackendMetrics::new(&registry),
            subscriptions: SubscriptionMetrics::new(&registry),
            operations: OperationMetrics::new(&registry),
        }
    }
}

impl Default for AppMetrics {
    fn default() -> Self {
        Self::new()
    }
}



/// Axum middleware that records request metrics and applies rate limiting.
pub async fn record_http(
    State(state): State<Arc<AppState>>,
    req: Request<Body>,
    next: Next,
) -> Response {
    let method = req.method().clone();
    let raw_path = req.uri().path().to_string();
    let normalized_path = normalize_path(&raw_path);

    // Increment in-flight gauge
    state
        .metrics
        .http
        .inc_in_flight(method.as_str(), &normalized_path);

    let start = Instant::now();
    let response = next.run(req).await;
    let duration = start.elapsed();

    // Record metrics
    let status = response.status();
    state
        .metrics
        .http
        .record_request(method.as_str(), &normalized_path, status, duration);

    response
}
