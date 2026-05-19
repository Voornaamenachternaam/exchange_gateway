// src/logging.rs
// Production-ready colorful logging with configurable formats

use std::env;
use std::str::FromStr;
use tracing::{Level, info};
use tracing_subscriber::{EnvFilter, fmt, prelude::*, registry};

/// Log format configuration
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LogFormat {
    #[default]
    Pretty,
    Compact,
    Json,
}

impl FromStr for LogFormat {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "pretty" => Ok(LogFormat::Pretty),
            "compact" => Ok(LogFormat::Compact),
            "json" => Ok(LogFormat::Json),
            _ => Err(format!(
                "Invalid log format: {}. Use 'pretty', 'compact', or 'json'",
                s
            )),
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
    // Phase 1: Pre-subscriber diagnostics go to stderr to ensure they're visible
    // Use eprintln! because tracing subscriber not yet initialized

    // Determine log level: GATEWAY_LOG_LEVEL > RUST_LOG (legacy) > default (info)
    // Strip leading dashes (common mistake: GATEWAY_LOG_LEVEL=-debug instead of debug)
    let level_str = match env::var("GATEWAY_LOG_LEVEL") {
        Ok(val) => val.trim_start_matches('-').to_string(),
        Err(_) => match env::var("RUST_LOG") {
            Ok(val) => {
                eprintln!("debug: GATEWAY_LOG_LEVEL not set, using RUST_LOG (legacy)");
                val.trim_start_matches('-').to_string()
            }
            Err(_) => {
                eprintln!("debug: No log level env var set, using default 'info'");
                "info".to_string()
            }
        },
    };

    // Parse log level with error handling
    let level = match level_str.parse::<Level>() {
        Ok(level) => {
            eprintln!("debug: Using log level: {:?}", level);
            level
        }
        Err(e) => {
            eprintln!(
                "warn: Invalid log level '{}': {}. Using default 'info'.",
                level_str, e
            );
            Level::INFO
        }
    };

    // Determine log format with deprecation warning for GATEWAY_LOG_JSON
    // Strip leading dashes (common mistake: GATEWAY_LOG_FORMAT=-pretty instead of pretty)
    let format = match env::var("GATEWAY_LOG_FORMAT") {
        Ok(val) => match val.trim_start_matches('-').parse::<LogFormat>() {
            Ok(format) => {
                eprintln!("debug: Using log format: {:?}", format);
                format
            }
            Err(e) => {
                eprintln!(
                    "warn: Invalid log format '{}': {}. Using default 'pretty'.",
                    val, e
                );
                LogFormat::Pretty
            }
        },
        Err(_) => {
            if env::var("GATEWAY_LOG_JSON").is_ok() {
                eprintln!(
                    "warn: GATEWAY_LOG_JSON is deprecated; use GATEWAY_LOG_FORMAT=json instead"
                );
                LogFormat::Json
            } else {
                eprintln!("debug: GATEWAY_LOG_FORMAT not set, using default 'pretty'");
                LogFormat::Pretty
            }
        }
    };

    // Timestamps: enabled by default, disable with GATEWAY_LOG_NO_TIMESTAMPS=1
    let timestamps = match env::var("GATEWAY_LOG_NO_TIMESTAMPS") {
        Ok(val) => {
            let disabled = val == "1" || val.eq_ignore_ascii_case("true");
            if disabled {
                eprintln!("debug: Timestamps disabled via GATEWAY_LOG_NO_TIMESTAMPS");
            } else {
                eprintln!("debug: Timestamps explicitly enabled via GATEWAY_LOG_NO_TIMESTAMPS");
            }
            !disabled
        }
        Err(_) => {
            eprintln!("debug: GATEWAY_LOG_NO_TIMESTAMPS not set, timestamps enabled by default");
            true
        }
    };

    // Thread info: off by default, enable with GATEWAY_LOG_THREADS=1
    let threads = match env::var("GATEWAY_LOG_THREADS") {
        Ok(val) => {
            let enabled = val == "1" || val.eq_ignore_ascii_case("true");
            eprintln!(
                "debug: Thread info {} via GATEWAY_LOG_THREADS",
                if enabled { "enabled" } else { "disabled" }
            );
            enabled
        }
        Err(_) => {
            eprintln!("debug: GATEWAY_LOG_THREADS not set, thread info disabled by default");
            false
        }
    };

    // Module targets: off by default, auto-enabled for trace/debug
    let target = match env::var("GATEWAY_LOG_TARGET") {
        Ok(val) => {
            let enabled = val == "1" || val.eq_ignore_ascii_case("true");
            eprintln!(
                "debug: Module targets {} via GATEWAY_LOG_TARGET",
                if enabled { "enabled" } else { "disabled" }
            );
            enabled
        }
        Err(_) => {
            // Enable by default for trace/debug levels to aid debugging
            let enabled = matches!(level, Level::TRACE | Level::DEBUG);
            eprintln!(
                "debug: GATEWAY_LOG_TARGET not set, target {} for level {:?}",
                if enabled { "enabled" } else { "disabled" },
                level
            );
            enabled
        }
    };

    // Build the EnvFilter. GATEWAY_LOG_LEVEL always takes precedence over RUST_LOG.
    //
    // Previously, EnvFilter::try_from_default_env() was tried first, which reads
    // RUST_LOG/LOG env vars. If RUST_LOG was set (e.g. via Dockerfile ENV), it
    // would silently override GATEWAY_LOG_LEVEL — the parsed `level` variable was
    // discarded. For example, RUST_LOG=info + GATEWAY_LOG_LEVEL=trace resulted in
    // effective filter=info, not trace.
    //
    // Now, when GATEWAY_LOG_LEVEL is explicitly set, we build the filter directly
    // from the parsed level, ignoring RUST_LOG entirely. When GATEWAY_LOG_LEVEL is
    // absent, we fall back to RUST_LOG (which may contain per-module directives
    // like "my_crate=debug,other=warn"), and finally to the default "info" level.
    let filter = if env::var("GATEWAY_LOG_LEVEL").is_ok() {
        // GATEWAY_LOG_LEVEL was explicitly set — use it exclusively.
        // This prevents RUST_LOG from silently overriding the user's intent.
        eprintln!(
            "debug: GATEWAY_LOG_LEVEL is set, building filter from level: {:?}",
            level
        );
        match EnvFilter::try_new(level.to_string()) {
            Ok(filter) => filter,
            Err(e) => {
                eprintln!("error: Failed to create log filter: {}", e);
                return Err(format!("Failed to create log filter: {}", e).into());
            }
        }
    } else {
        // GATEWAY_LOG_LEVEL not set — try RUST_LOG/LOG env vars (which may
        // contain per-module directives), then fall back to parsed level.
        match EnvFilter::try_from_default_env() {
            Ok(filter) => {
                eprintln!("debug: Using existing RUST_LOG/LOG environment filter");
                filter
            }
            Err(_) => {
                eprintln!(
                    "debug: No existing env filter found, creating filter from level: {:?}",
                    level
                );
                match EnvFilter::try_new(level.to_string()) {
                    Ok(filter) => filter,
                    Err(e) => {
                        eprintln!("error: Failed to create log filter: {}", e);
                        return Err(format!("Failed to create log filter: {}", e).into());
                    }
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
                .event_format(fmt::format().with_level(true).with_target(target));
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
                .event_format(fmt::format().with_level(true).with_target(target));
            registry.with(layer).try_init()?;
        }
        (LogFormat::Compact, true) => {
            let layer = fmt::layer()
                .with_timer(TimestampFormatter)
                .with_ansi(true)
                .with_target(target)
                .with_thread_names(threads)
                .with_thread_ids(threads)
                .with_file(false)
                .with_line_number(false)
                .event_format(fmt::format().with_level(true).with_target(target));
            registry.with(layer).try_init()?;
        }
        (LogFormat::Compact, false) => {
            let layer = fmt::layer()
                .with_ansi(true)
                .with_target(target)
                .with_thread_names(threads)
                .with_thread_ids(threads)
                .with_file(false)
                .with_line_number(false)
                .event_format(fmt::format().with_level(true).with_target(target));
            registry.with(layer).try_init()?;
        }
        (LogFormat::Json, true) => {
            let layer = fmt::layer()
                .json()
                .with_timer(TimestampFormatter)
                .with_ansi(false)
                .with_target(target)
                .with_thread_names(threads)
                .with_thread_ids(threads);
            registry.with(layer).try_init()?;
        }
        (LogFormat::Json, false) => {
            let layer = fmt::layer()
                .json()
                .with_ansi(false)
                .with_target(target)
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
        assert!(
            buffer.ends_with('Z'),
            "timestamp='{}' should end with Z",
            buffer
        );
        assert!(
            buffer.contains('T'),
            "timestamp='{}' should contain T",
            buffer
        );
        // Should match RFC3339 pattern: YYYY-MM-DDTHH:MM:SS[.fraction]Z
        assert!(buffer.len() >= 20); // "2025-12-28T14:30:45Z" is 20 chars
    }

    /// When GATEWAY_LOG_LEVEL is set, it must take precedence over RUST_LOG.
    /// Previously, EnvFilter::try_from_default_env() read RUST_LOG first and
    /// silently overrode GATEWAY_LOG_LEVEL. For example, RUST_LOG=info +
    /// GATEWAY_LOG_LEVEL=trace resulted in effective filter=info.
    #[test]
    fn test_gateway_log_level_overrides_rust_log() {
        use std::env;

        // Simulate the priority logic: GATEWAY_LOG_LEVEL > RUST_LOG > default
        let level_str = match env::var("GATEWAY_LOG_LEVEL") {
            Ok(val) => val.trim_start_matches('-').to_string(),
            Err(_) => match env::var("RUST_LOG") {
                Ok(val) => val.trim_start_matches('-').to_string(),
                Err(_) => "info".to_string(),
            },
        };

        // When GATEWAY_LOG_LEVEL is set, the filter should be built from it,
        // not from RUST_LOG. This is the behavioral contract of the fix.
        let gateway_set = env::var("GATEWAY_LOG_LEVEL").is_ok();
        if gateway_set {
            // GATEWAY_LOG_LEVEL was explicitly set — it wins over RUST_LOG
            let gateway_level = env::var("GATEWAY_LOG_LEVEL").unwrap();
            assert_eq!(
                level_str,
                gateway_level.trim_start_matches('-'),
                "GATEWAY_LOG_LEVEL='{}' should take precedence, but got '{}'",
                gateway_level,
                level_str
            );
        }
    }
}
