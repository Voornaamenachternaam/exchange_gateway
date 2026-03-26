//! Observability - Logging and Metrics Framework
//!
//! This module provides comprehensive logging, metrics collection, and tracing
//! for the Exchange Gateway with support for structured logging, performance
//! metrics, and distributed tracing.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Log level
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum LogLevel {
    Trace = 0,
    Debug = 1,
    Info = 2,
    Warn = 3,
    Error = 4,
    Fatal = 5,
}

impl LogLevel {
    pub fn as_str(&self) -> &'static str {
        match self {
            LogLevel::Trace => "TRACE",
            LogLevel::Debug => "DEBUG",
            LogLevel::Info => "INFO",
            LogLevel::Warn => "WARN",
            LogLevel::Error => "ERROR",
            LogLevel::Fatal => "FATAL",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s.to_uppercase().as_str() {
            "TRACE" => Some(LogLevel::Trace),
            "DEBUG" => Some(LogLevel::Debug),
            "INFO" => Some(LogLevel::Info),
            "WARN" | "WARNING" => Some(LogLevel::Warn),
            "ERROR" => Some(LogLevel::Error),
            "FATAL" => Some(LogLevel::Fatal),
            _ => None,
        }
    }
}

/// Log entry
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogEntry {
    pub timestamp: DateTime<Utc>,
    pub level: LogLevel,
    pub message: String,
    pub component: String,
    pub operation: String,
    pub user_id: Option<String>,
    pub device_id: Option<String>,
    pub request_id: String,
    pub duration_ms: Option<u64>,
    pub metadata: HashMap<String, String>,
    pub error: Option<String>,
    pub stack_trace: Option<String>,
}

impl LogEntry {
    /// Create a new log entry
    pub fn new(level: LogLevel, message: impl Into<String>, component: impl Into<String>) -> Self {
        Self {
            timestamp: Utc::now(),
            level,
            message: message.into(),
            component: component.into(),
            operation: String::new(),
            user_id: None,
            device_id: None,
            request_id: generate_request_id(),
            duration_ms: None,
            metadata: HashMap::new(),
            error: None,
            stack_trace: None,
        }
    }

    /// Set operation
    pub fn with_operation(mut self, operation: impl Into<String>) -> Self {
        self.operation = operation.into();
        self
    }

    /// Set user ID
    pub fn with_user_id(mut self, user_id: impl Into<String>) -> Self {
        self.user_id = Some(user_id.into());
        self
    }

    /// Set device ID
    pub fn with_device_id(mut self, device_id: impl Into<String>) -> Self {
        self.device_id = Some(device_id.into());
        self
    }

    /// Set request ID
    pub fn with_request_id(mut self, request_id: impl Into<String>) -> Self {
        self.request_id = request_id.into();
        self
    }

    /// Set duration
    pub fn with_duration(mut self, duration: Duration) -> Self {
        self.duration_ms = Some(duration.as_millis() as u64);
        self
    }

    /// Add metadata
    pub fn with_metadata(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.metadata.insert(key.into(), value.into());
        self
    }

    /// Set error
    pub fn with_error(mut self, error: impl Into<String>) -> Self {
        self.error = Some(error.into());
        self
    }

    /// Set stack trace
    pub fn with_stack_trace(mut self, stack_trace: impl Into<String>) -> Self {
        self.stack_trace = Some(stack_trace.into());
        self
    }

    /// Format as JSON
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }

    /// Format as structured text
    pub fn to_text(&self) -> String {
        let mut text = format!(
            "[{}] {} [{}] {} - {}",
            self.timestamp.to_rfc3339(),
            self.level.as_str(),
            self.component,
            self.operation,
            self.message
        );

        if let Some(ref user_id) = self.user_id {
            text.push_str(&format!(" [user={}]", user_id));
        }

        if let Some(ref device_id) = self.device_id {
            text.push_str(&format!(" [device={}]", device_id));
        }

        if let Some(duration) = self.duration_ms {
            text.push_str(&format!(" [duration={}ms]", duration));
        }

        if let Some(ref error) = self.error {
            text.push_str(&format!(" [error={}]", error));
        }

        text
    }
}

/// Logger trait
pub trait Logger: Send + Sync {
    fn log(&self, entry: LogEntry);
    fn flush(&self);
}

/// Console logger
pub struct ConsoleLogger {
    min_level: LogLevel,
}

impl ConsoleLogger {
    pub fn new(min_level: LogLevel) -> Self {
        Self { min_level }
    }
}

impl Logger for ConsoleLogger {
    fn log(&self, entry: LogEntry) {
        if entry.level >= self.min_level {
            println!("{}", entry.to_text());
        }
    }

    fn flush(&self) {
        // Console output is immediate
    }
}

/// JSON logger
pub struct JsonLogger {
    min_level: LogLevel,
}

impl JsonLogger {
    pub fn new(min_level: LogLevel) -> Self {
        Self { min_level }
    }
}

impl Logger for JsonLogger {
    fn log(&self, entry: LogEntry) {
        if entry.level >= self.min_level {
            println!("{}", entry.to_json());
        }
    }

    fn flush(&self) {
        // Console output is immediate
    }
}

/// Multi logger (logs to multiple destinations)
pub struct MultiLogger {
    loggers: Vec<Box<dyn Logger>>,
}

impl MultiLogger {
    pub fn new() -> Self {
        Self { loggers: Vec::new() }
    }

    pub fn add_logger(&mut self, logger: Box<dyn Logger>) {
        self.loggers.push(logger);
    }
}

impl Default for MultiLogger {
    fn default() -> Self {
        Self::new()
    }
}

impl Logger for MultiLogger {
    fn log(&self, entry: LogEntry) {
        for logger in &self.loggers {
            logger.log(entry.clone());
        }
    }

    fn flush(&self) {
        for logger in &self.loggers {
            logger.flush();
        }
    }
}

/// Logging context for request tracing
pub struct LogContext {
    logger: Arc<dyn Logger>,
    request_id: String,
    user_id: Option<String>,
    device_id: Option<String>,
    component: String,
    start_time: Instant,
}

impl LogContext {
    /// Create a new logging context
    pub fn new(
        logger: Arc<dyn Logger>,
        component: impl Into<String>,
    ) -> Self {
        Self {
            logger,
            request_id: generate_request_id(),
            user_id: None,
            device_id: None,
            component: component.into(),
            start_time: Instant::now(),
        }
    }

    /// Set user ID
    pub fn with_user_id(mut self, user_id: impl Into<String>) -> Self {
        self.user_id = Some(user_id.into());
        self
    }

    /// Set device ID
    pub fn with_device_id(mut self, device_id: impl Into<String>) -> Self {
        self.device_id = Some(device_id.into());
        self
    }

    /// Log at a specific level
    pub fn log(&self, level: LogLevel, message: impl Into<String>, operation: impl Into<String>) {
        let entry = LogEntry::new(level, message, &self.component)
            .with_request_id(&self.request_id)
            .with_operation(operation)
            .with_duration(self.start_time.elapsed());

        let entry = if let Some(ref user_id) = self.user_id {
            entry.with_user_id(user_id)
        } else {
            entry
        };

        let entry = if let Some(ref device_id) = self.device_id {
            entry.with_device_id(device_id)
        } else {
            entry
        };

        self.logger.log(entry);
    }

    /// Log trace
    pub fn trace(&self, message: impl Into<String>, operation: impl Into<String>) {
        self.log(LogLevel::Trace, message, operation);
    }

    /// Log debug
    pub fn debug(&self, message: impl Into<String>, operation: impl Into<String>) {
        self.log(LogLevel::Debug, message, operation);
    }

    /// Log info
    pub fn info(&self, message: impl Into<String>, operation: impl Into<String>) {
        self.log(LogLevel::Info, message, operation);
    }

    /// Log warn
    pub fn warn(&self, message: impl Into<String>, operation: impl Into<String>) {
        self.log(LogLevel::Warn, message, operation);
    }

    /// Log error
    pub fn error(&self, message: impl Into<String>, operation: impl Into<String>) {
        self.log(LogLevel::Error, message, operation);
    }

    /// Log error with error object
    pub fn error_with_err(
        &self,
        message: impl Into<String>,
        operation: impl Into<String>,
        error: &dyn std::error::Error,
    ) {
        let entry = LogEntry::new(LogLevel::Error, message, &self.component)
            .with_request_id(&self.request_id)
            .with_operation(operation)
            .with_duration(self.start_time.elapsed())
            .with_error(error.to_string());

        let entry = if let Some(ref user_id) = self.user_id {
            entry.with_user_id(user_id)
        } else {
            entry
        };

        let entry = if let Some(ref device_id) = self.device_id {
            entry.with_device_id(device_id)
        } else {
            entry
        };

        self.logger.log(entry);
    }

    /// Get elapsed time
    pub fn elapsed(&self) -> Duration {
        self.start_time.elapsed()
    }

    /// Get request ID
    pub fn request_id(&self) -> &str {
        &self.request_id
    }
}

/// Metrics counter
#[derive(Debug, Clone)]
pub struct Counter {
    name: String,
    value: Arc<Mutex<u64>>,
    labels: HashMap<String, String>,
}

impl Counter {
    /// Create a new counter
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            value: Arc::new(Mutex::new(0)),
            labels: HashMap::new(),
        }
    }

    /// Add labels
    pub fn with_label(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.labels.insert(key.into(), value.into());
        self
    }

    /// Increment counter
    pub fn increment(&self) {
        if let Ok(mut value) = self.value.lock() {
            *value += 1;
        }
    }

    /// Increment by value
    pub fn increment_by(&self, delta: u64) {
        if let Ok(mut value) = self.value.lock() {
            *value += delta;
        }
    }

    /// Get current value
    pub fn get(&self) -> u64 {
        self.value.lock().map(|v| *v).unwrap_or(0)
    }

    /// Get name
    pub fn name(&self) -> &str {
        &self.name
    }
}

/// Metrics gauge
#[derive(Debug, Clone)]
pub struct Gauge {
    name: String,
    value: Arc<Mutex<f64>>,
    labels: HashMap<String, String>,
}

impl Gauge {
    /// Create a new gauge
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            value: Arc::new(Mutex::new(0.0)),
            labels: HashMap::new(),
        }
    }

    /// Set gauge value
    pub fn set(&self, value: f64) {
        if let Ok(mut v) = self.value.lock() {
            *v = value;
        }
    }

    /// Get current value
    pub fn get(&self) -> f64 {
        self.value.lock().map(|v| *v).unwrap_or(0.0)
    }
}

/// Metrics histogram
#[derive(Debug, Clone)]
pub struct Histogram {
    name: String,
    values: Arc<Mutex<Vec<f64>>>,
    labels: HashMap<String, String>,
}

impl Histogram {
    /// Create a new histogram
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            values: Arc::new(Mutex::new(Vec::new())),
            labels: HashMap::new(),
        }
    }

    /// Record a value
    pub fn record(&self, value: f64) {
        if let Ok(mut values) = self.values.lock() {
            values.push(value);
        }
    }

    /// Get all values
    pub fn get_values(&self) -> Vec<f64> {
        self.values.lock().map(|v| v.clone()).unwrap_or_default()
    }

    /// Get count
    pub fn count(&self) -> usize {
        self.values.lock().map(|v| v.len()).unwrap_or(0)
    }

    /// Get mean
    pub fn mean(&self) -> f64 {
        let values = self.get_values();
        if values.is_empty() {
            0.0
        } else {
            values.iter().sum::<f64>() / values.len() as f64
        }
    }

    /// Get percentile
    pub fn percentile(&self, p: f64) -> f64 {
        let mut values = self.get_values();
        if values.is_empty() {
            return 0.0;
        }
        values.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let index = ((p / 100.0) * (values.len() - 1) as f64) as usize;
        values[index.min(values.len() - 1)]
    }
}

/// Metrics registry
pub struct MetricsRegistry {
    counters: Arc<Mutex<HashMap<String, Counter>>>,
    gauges: Arc<Mutex<HashMap<String, Gauge>>>,
    histograms: Arc<Mutex<HashMap<String, Histogram>>>,
}

impl MetricsRegistry {
    /// Create a new metrics registry
    pub fn new() -> Self {
        Self {
            counters: Arc::new(Mutex::new(HashMap::new())),
            gauges: Arc::new(Mutex::new(HashMap::new())),
            histograms: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Register a counter
    pub fn register_counter(&self, counter: Counter) {
        if let Ok(mut counters) = self.counters.lock() {
            counters.insert(counter.name.clone(), counter);
        }
    }

    /// Get a counter
    pub fn get_counter(&self, name: &str) -> Option<Counter> {
        self.counters.lock().ok()?.get(name).cloned()
    }

    /// Register a gauge
    pub fn register_gauge(&self, gauge: Gauge) {
        if let Ok(mut gauges) = self.gauges.lock() {
            gauges.insert(gauge.name.clone(), gauge);
        }
    }

    /// Get a gauge
    pub fn get_gauge(&self, name: &str) -> Option<Gauge> {
        self.gauges.lock().ok()?.get(name).cloned()
    }

    /// Register a histogram
    pub fn register_histogram(&self, histogram: Histogram) {
        if let Ok(mut histograms) = self.histograms.lock() {
            histograms.insert(histogram.name.clone(), histogram);
        }
    }

    /// Get a histogram
    pub fn get_histogram(&self, name: &str) -> Option<Histogram> {
        self.histograms.lock().ok()?.get(name).cloned()
    }

    /// Get all metrics as JSON
    pub fn to_json(&self) -> String {
        let mut metrics = HashMap::new();

        if let Ok(counters) = self.counters.lock() {
            for (name, counter) in counters.iter() {
                metrics.insert(name.clone(), serde_json::json!(counter.get()));
            }
        }

        if let Ok(gauges) = self.gauges.lock() {
            for (name, gauge) in gauges.iter() {
                metrics.insert(name.clone(), serde_json::json!(gauge.get()));
            }
        }

        if let Ok(histograms) = self.histograms.lock() {
            for (name, histogram) in histograms.iter() {
                let h = serde_json::json!({
                    "count": histogram.count(),
                    "mean": histogram.mean(),
                    "p50": histogram.percentile(50.0),
                    "p95": histogram.percentile(95.0),
                    "p99": histogram.percentile(99.0),
                });
                metrics.insert(name.clone(), h);
            }
        }

        serde_json::to_string_pretty(&metrics).unwrap_or_default()
    }
}

impl Default for MetricsRegistry {
    fn default() -> Self {
        Self::new()
    }
}

/// Performance timer
pub struct Timer {
    start: Instant,
    name: String,
}

impl Timer {
    /// Start a new timer
    pub fn start(name: impl Into<String>) -> Self {
        Self {
            start: Instant::now(),
            name: name.into(),
        }
    }

    /// Stop timer and get elapsed
    pub fn stop(&self) -> Duration {
        self.start.elapsed()
    }

    /// Get timer name
    pub fn name(&self) -> &str {
        &self.name
    }
}

/// Generate a unique request ID
fn generate_request_id() -> String {
    use rand::Rng;
    let mut rng = rand::thread_rng();
    let bytes: Vec<u8> = (0..16).map(|_| rng.gen()).collect();
    hex::encode(bytes)
}

/// Global logging facade
pub struct Log;

impl Log {
    /// Log at trace level
    pub fn trace(component: &str, message: impl Into<String>) {
        let entry = LogEntry::new(LogLevel::Trace, message, component);
        eprintln!("{}", entry.to_text());
    }

    /// Log at debug level
    pub fn debug(component: &str, message: impl Into<String>) {
        let entry = LogEntry::new(LogLevel::Debug, message, component);
        eprintln!("{}", entry.to_text());
    }

    /// Log at info level
    pub fn info(component: &str, message: impl Into<String>) {
        let entry = LogEntry::new(LogLevel::Info, message, component);
        eprintln!("{}", entry.to_text());
    }

    /// Log at warn level
    pub fn warn(component: &str, message: impl Into<String>) {
        let entry = LogEntry::new(LogLevel::Warn, message, component);
        eprintln!("{}", entry.to_text());
    }

    /// Log at error level
    pub fn error(component: &str, message: impl Into<String>) {
        let entry = LogEntry::new(LogLevel::Error, message, component);
        eprintln!("{}", entry.to_text());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_log_entry_creation() {
        let entry = LogEntry::new(LogLevel::Info, "Test message", "TestComponent")
            .with_operation("test_op")
            .with_user_id("user1")
            .with_metadata("key", "value");

        assert_eq!(entry.level, LogLevel::Info);
        assert_eq!(entry.message, "Test message");
        assert_eq!(entry.component, "TestComponent");
        assert_eq!(entry.operation, "test_op");
        assert_eq!(entry.user_id, Some("user1".to_string()));
        assert_eq!(entry.metadata.get("key"), Some(&"value".to_string()));
    }

    #[test]
    fn test_log_entry_json() {
        let entry = LogEntry::new(LogLevel::Info, "Test", "Component");
        let json = entry.to_json();
        assert!(json.contains("Test"));
        assert!(json.contains("INFO"));
    }

    #[test]
    fn test_log_entry_text() {
        let entry = LogEntry::new(LogLevel::Info, "Test message", "Component")
            .with_operation("op");
        let text = entry.to_text();
        assert!(text.contains("INFO"));
        assert!(text.contains("Test message"));
    }

    #[test]
    fn test_counter() {
        let counter = Counter::new("test_counter");
        assert_eq!(counter.get(), 0);

        counter.increment();
        assert_eq!(counter.get(), 1);

        counter.increment_by(5);
        assert_eq!(counter.get(), 6);
    }

    #[test]
    fn test_gauge() {
        let gauge = Gauge::new("test_gauge");
        assert_eq!(gauge.get(), 0.0);

        gauge.set(42.5);
        assert_eq!(gauge.get(), 42.5);
    }

    #[test]
    fn test_histogram() {
        let histogram = Histogram::new("test_histogram");
        assert_eq!(histogram.count(), 0);

        histogram.record(10.0);
        histogram.record(20.0);
        histogram.record(30.0);

        assert_eq!(histogram.count(), 3);
        assert_eq!(histogram.mean(), 20.0);
    }

    #[test]
    fn test_histogram_percentile() {
        let histogram = Histogram::new("test_histogram");

        for i in 1..=100 {
            histogram.record(i as f64);
        }

        assert_eq!(histogram.percentile(50.0), 50.0);
        assert_eq!(histogram.percentile(95.0), 95.0);
        assert_eq!(histogram.percentile(99.0), 99.0);
    }

    #[test]
    fn test_timer() {
        let timer = Timer::start("test_timer");
        std::thread::sleep(Duration::from_millis(10));
        let elapsed = timer.stop();
        assert!(elapsed >= Duration::from_millis(10));
    }

    #[test]
    fn test_log_level_ordering() {
        assert!(LogLevel::Trace < LogLevel::Debug);
        assert!(LogLevel::Debug < LogLevel::Info);
        assert!(LogLevel::Info < LogLevel::Warn);
        assert!(LogLevel::Warn < LogLevel::Error);
        assert!(LogLevel::Error < LogLevel::Fatal);
    }

    #[test]
    fn test_log_level_from_str() {
        assert_eq!(LogLevel::from_str("INFO"), Some(LogLevel::Info));
        assert_eq!(LogLevel::from_str("info"), Some(LogLevel::Info));
        assert_eq!(LogLevel::from_str("WARN"), Some(LogLevel::Warn));
        assert_eq!(LogLevel::from_str("warning"), Some(LogLevel::Warn));
        assert_eq!(LogLevel::from_str("invalid"), None);
    }
}
