// src/logging.rs
// Production-ready colorful logging with configurable formats

use std::env;
use std::str::FromStr;
use tracing_subscriber::{fmt, prelude::*, EnvFilter};

/// Log format configuration
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogFormat {
    Pretty,
    Compact,
    Json,
}

impl Default for LogFormat {
    fn default() -> Self {
        Self::Pretty
    }
}

impl FromStr for LogFormat {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "pretty" => Ok(LogFormat::Pretty),
            "compact" => Ok(LogFormat::Compact),
            "json" => Ok(LogFormat::Json),
            _ => Err(format!("Invalid log format: {}. Use 'pretty', 'compact', or 'json'", s)),
        }
    }
}

/// Custom timestamp with ISO8601 UTC (ending with Z)
#[derive(Debug, Clone, Copy)]
pub struct TimestampFormatter;

impl fmt::time::FormatTime for TimestampFormatter {
    fn format_time(&self, w: &mut fmt::format::Writer<'_>) -> std::fmt::Result {
        let now = chrono::Utc::now();
        // Format as ISO8601 with trailing Z (required by EWS/Outlook)
        write!(w, "{}", now.format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string())
    }
}

/// Initialize logging from environment configuration
pub fn init_logging() -> Result<(), Box<dyn std::error::Error>> {
    let level = env::var("GATEWAY_LOG_LEVEL").unwrap_or_else(|_| "info".to_string());
    let format = get_log_format();
    let timestamps = !env::var("GATEWAY_LOG_NO_TIMESTAMPS").is_ok();
    let threads = env::var("GATEWAY_LOG_THREADS").is_ok();
    let target = matches!(level.as_str(), "trace" | "debug") || env::var("GATEWAY_LOG_TARGET").is_ok();
    
    install_subscriber(&level, format, timestamps, threads, target)?;
    
    tracing::info!(
        target: "logging",
        level = %level,
        format = ?format,
        timestamps = timestamps,
        threads = threads,
        target = target,
        "Logging initialized"
    );
    
    Ok(())
}

fn get_log_format() -> LogFormat {
    if env::var("GATEWAY_LOG_JSON").is_ok() {
        return LogFormat::Json;
    }
    env::var("GATEWAY_LOG_FORMAT")
        .ok()
        .and_then(|s| LogFormat::from_str(&s).ok())
        .unwrap_or_default()
}

fn install_subscriber(
    level: &str,
    format: LogFormat,
    timestamps: bool,
    threads: bool,
    target: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let filter = EnvFilter::try_new(level)
        .map_err(|e| format!("Failed to parse log level '{}': {}", level, e))?;
    
    let registry = tracing_subscriber::registry()
        .with(filter);
    
    match (format, timestamps) {
        (LogFormat::Pretty, true) => {
            let layer = fmt::layer()
                .with_timer(TimestampFormatter)
                .with_ansi(true)
                .with_target(target)
                .with_thread_names(threads)
                .with_thread_ids(threads)
                .with_file(target)
                .with_line_number(target)
                .event_format(
                    fmt::format()
                        .with_level(true)
                        .with_target(target)
                );
            registry.with(layer).try_init()?;
        }
        (LogFormat::Pretty, false) => {
            let layer = fmt::layer()
                .with_ansi(true)
                .with_target(target)
                .with_thread_names(threads)
                .with_thread_ids(threads)
                .with_file(target)
                .with_line_number(target)
                .event_format(
                    fmt::format()
                        .with_level(true)
                        .with_target(target)
                );
            registry.with(layer).try_init()?;
        }
        (LogFormat::Compact, true) => {
            let layer = fmt::layer()
                .with_timer(TimestampFormatter)
                .with_ansi(true)
                .with_target(false)
                .with_thread_names(threads)
                .with_thread_ids(threads)
                .with_file(false)
                .with_line_number(false)
                .event_format(
                    fmt::format()
                        .with_level(true)
                        .with_target(false)
                );
            registry.with(layer).try_init()?;
        }
        (LogFormat::Compact, false) => {
            let layer = fmt::layer()
                .with_ansi(true)
                .with_target(false)
                .with_thread_names(threads)
                .with_thread_ids(threads)
                .with_file(false)
                .with_line_number(false)
                .event_format(
                    fmt::format()
                        .with_level(true)
                        .with_target(false)
                );
            registry.with(layer).try_init()?;
        }
        (LogFormat::Json, true) => {
            let layer = fmt::layer()
                .json()
                .with_timer(TimestampFormatter)
                .with_ansi(false)
                .with_target(false)
                .with_thread_names(threads)
                .with_thread_ids(threads);
            registry.with(layer).try_init()?;
        }
        (LogFormat::Json, false) => {
            let layer = fmt::layer()
                .json()
                .with_ansi(false)
                .with_target(false)
                .with_thread_names(threads)
                .with_thread_ids(threads);
            registry.with(layer).try_init()?;
        }
    }
    
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tracing_subscriber::fmt::time::FormatTime;
    
    #[test]
    fn test_parse_formats() {
        assert_eq!(LogFormat::from_str("pretty").unwrap(), LogFormat::Pretty);
        assert_eq!(LogFormat::from_str("compact").unwrap(), LogFormat::Compact);
        assert_eq!(LogFormat::from_str("json").unwrap(), LogFormat::Json);
        assert!(LogFormat::from_str("bad").is_err());
    }
    
    #[test]
    fn test_default() {
        assert_eq!(LogFormat::default(), LogFormat::Pretty);
    }
    
    #[test]
    fn test_timestamp_format() {
        let formatter = TimestampFormatter;
        let mut buffer = String::new();
        {
            let mut writer = fmt::format::Writer::new(&mut buffer);
            formatter.format_time(&mut writer).unwrap();
        }
        eprintln!("DEBUG: timestamp output = {:?}", buffer);
        assert!(buffer.ends_with('Z'), "timestamp='{}' should end with Z", buffer);
        assert!(buffer.contains('T'), "timestamp='{}' should contain T", buffer);
        // Should match RFC3339 pattern: YYYY-MM-DDTHH:MM:SS[.fraction]Z
        assert!(buffer.len() >= 20); // "2025-12-28T14:30:45Z" is 20 chars
    }
}