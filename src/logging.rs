// src/logging.rs
// Production-ready colorful logging with configurable formats

use std::env;
use std::str::FromStr;
use tracing::{debug, error, info, warn, Level};
use tracing_subscriber::{fmt, prelude::*, registry, EnvFilter};

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
        // DelayedFormat implements Display, avoiding unnecessary allocation
        write!(w, "{}", now.format("%Y-%m-%dT%H:%M:%S%.3fZ"))
    }
}

/// Initialize logging from environment configuration
pub fn init_logging() -> Result<(), Box<dyn std::error::Error>> {
    // Determine log level: GATEWAY_LOG_LEVEL > RUST_LOG (legacy) > default (info)
    // Use match blocks to log errors and fallbacks (avoid unwrap_or_else as per repo rules)
    let level_str = match env::var("GATEWAY_LOG_LEVEL") {
        Ok(val) => val,
        Err(_) => match env::var("RUST_LOG") {
            Ok(val) => {
                debug!("GATEWAY_LOG_LEVEL not set, using RUST_LOG (legacy)");
                val
            }
            Err(_) => {
                debug!("No log level env var set, using default 'info'");
                "info".to_string()
            }
        }
    };

    // Parse log level with error handling
    let level = match level_str.parse::<Level>() {
        Ok(level) => {
            debug!("Using log level: {:?}", level);
            level
        }
        Err(e) => {
            warn!("Invalid log level '{}': {}. Using default 'info'.", level_str, e);
            Level::INFO
        }
    };

    // Determine log format with deprecation warning for GATEWAY_LOG_JSON
    let format = match env::var("GATEWAY_LOG_FORMAT") {
        Ok(val) => match val.parse::<LogFormat>() {
            Ok(format) => {
                debug!("Using log format: {:?}", format);
                format
            }
            Err(e) => {
                warn!("Invalid log format '{}': {}. Using default 'pretty'.", val, e);
                LogFormat::Pretty
            }
        },
        Err(_) => {
            if env::var("GATEWAY_LOG_JSON").is_ok() {
                warn!("GATEWAY_LOG_JSON is deprecated; use GATEWAY_LOG_FORMAT=json instead");
                LogFormat::Json
            } else {
                debug!("GATEWAY_LOG_FORMAT not set, using default 'pretty'");
                LogFormat::Pretty
            }
        }
    };

    // Timestamps: enabled by default, disable with GATEWAY_LOG_NO_TIMESTAMPS=1
    let timestamps = match env::var("GATEWAY_LOG_NO_TIMESTAMPS") {
        Ok(val) => {
            let disabled = val == "1" || val.eq_ignore_ascii_case("true");
            if disabled {
                debug!("Timestamps disabled via GATEWAY_LOG_NO_TIMESTAMPS");
            } else {
                debug!("Timestamps explicitly enabled via GATEWAY_LOG_NO_TIMESTAMPS");
            }
            !disabled
        }
        Err(_) => {
            debug!("GATEWAY_LOG_NO_TIMESTAMPS not set, timestamps enabled by default");
            true
        }
    };

    // Thread info: off by default, enable with GATEWAY_LOG_THREADS=1
    let threads = match env::var("GATEWAY_LOG_THREADS") {
        Ok(val) => {
            let enabled = val == "1" || val.eq_ignore_ascii_case("true");
            debug!("Thread info {} via GATEWAY_LOG_THREADS", if enabled { "enabled" } else { "disabled" });
            enabled
        }
        Err(_) => {
            debug!("GATEWAY_LOG_THREADS not set, thread info disabled by default");
            false
        }
    };

    // Module targets: off by default, auto-enabled for trace/debug
    let target = match env::var("GATEWAY_LOG_TARGET") {
        Ok(val) => {
            let enabled = val == "1" || val.eq_ignore_ascii_case("true");
            debug!("Module targets {} via GATEWAY_LOG_TARGET", if enabled { "enabled" } else { "disabled" });
            enabled
        }
        Err(_) => {
            // Enable by default for trace/debug levels to aid debugging
            let enabled = matches!(level, Level::TRACE | Level::DEBUG);
            debug!("GATEWAY_LOG_TARGET not set, target {} for level {:?}", if enabled { "enabled" } else { "disabled" }, level);
            enabled
        }
    };

    // Build filter: try existing env filter first (respects RUST_LOG/LOG patterns), 
    // then fall back to level-specific filter with proper error handling
    let filter = match EnvFilter::try_from_default_env() {
        Ok(filter) => {
            debug!("Using existing RUST_LOG/LOG environment filter");
            filter
        }
        Err(_) => {
            debug!("No existing env filter found, creating filter from level: {:?}", level);
            match EnvFilter::try_new(&level.to_string()) {
                Ok(filter) => filter,
                Err(e) => {
                    error!("Failed to create log filter: {}", e);
                    return Err(format!("Failed to create log filter: {}", e).into());
                }
            }
        }
    };

    let registry = registry().with(filter);

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

    info!(
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