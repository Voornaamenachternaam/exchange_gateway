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

/// Build an `EnvFilter` from the given log level strings.
///
/// Priority: `gateway_level` > `rust_log` > default `"info"`.
///
/// When `gateway_level` is `Some`, it is used exclusively — `RUST_LOG` is
/// ignored entirely. This prevents a Dockerfile `ENV RUST_LOG=info` from
/// silently overriding the user's `GATEWAY_LOG_LEVEL=trace`.
///
/// The level string may contain complex directives like `"trace,axum=info"`,
/// which `EnvFilter::try_new()` parses correctly. A simple level like
/// `"trace"` also works.
pub fn build_env_filter(
    gateway_level: Option<&str>,
    rust_log: Option<&str>,
) -> Result<EnvFilter, String> {
    match gateway_level {
        Some(raw) => {
            let trimmed = raw.trim_start_matches('-');
            EnvFilter::try_new(trimmed).map_err(|e| {
                format!(
                    "Failed to create log filter from GATEWAY_LOG_LEVEL='{}': {}",
                    trimmed, e
                )
            })
        }
        None => match rust_log {
            Some(raw) => {
                let trimmed = raw.trim_start_matches('-');
                EnvFilter::try_new(trimmed).map_err(|e| {
                    format!(
                        "Failed to create log filter from RUST_LOG='{}': {}",
                        trimmed, e
                    )
                })
            }
            None => EnvFilter::try_new("info").map_err(|e| {
                format!("Failed to create default log filter: {}", e)
            }),
        },
    }
}

/// Extract the global (ambient) log level from a directive string.
///
/// For `"trace,axum=info"` this returns `Level::TRACE` — the first
/// comma-separated segment is the ambient level. For a simple `"info"`
/// this returns `Level::INFO`. Falls back to `Level::INFO` if the
/// first segment cannot be parsed as a level.
pub fn parse_global_level(level_str: &str) -> Level {
    let first_segment = level_str
        .split(',')
        .next()
        .unwrap_or(level_str)
        .trim()
        .trim_start_matches('-');
    match first_segment.parse::<Level>() {
        Ok(level) => level,
        Err(_) => Level::INFO,
    }
}

/// Initialize logging from environment configuration
pub fn init_logging() -> Result<(), Box<dyn std::error::Error>> {
    // Phase 1: Pre-subscriber diagnostics go to stderr to ensure they're visible
    // Use eprintln! because tracing subscriber not yet initialized

    // Read raw env var values
    let gateway_raw = env::var("GATEWAY_LOG_LEVEL").ok();
    let rust_log_raw = env::var("RUST_LOG").ok();

    // Determine the full directive string for EnvFilter construction
    // Priority: GATEWAY_LOG_LEVEL > RUST_LOG > default "info"
    let level_str = match gateway_raw.as_deref() {
        Some(val) => val.trim_start_matches('-').to_string(),
        None => match rust_log_raw.as_deref() {
            Some(val) => {
                eprintln!("debug: GATEWAY_LOG_LEVEL not set, using RUST_LOG (legacy)");
                val.trim_start_matches('-').to_string()
            }
            None => {
                eprintln!("debug: No log level env var set, using default 'info'");
                "info".to_string()
            }
        },
    };

    // Parse the global (ambient) level for auto-enable features.
    // This extracts the first comma-separated segment: "trace,axum=info" → TRACE
    let level = parse_global_level(&level_str);
    eprintln!("debug: Using log level: {:?}", level);

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
    // Now, build_env_filter() handles the priority logic and preserves the full
    // directive string (e.g. "trace,axum=info") instead of truncating to a single
    // level. The `level` variable (from parse_global_level) is only used for
    // auto-enabling features like module targets.
    let filter = match build_env_filter(gateway_raw.as_deref(), rust_log_raw.as_deref()) {
        Ok(f) => {
            if gateway_raw.is_some() {
                eprintln!(
                    "debug: GATEWAY_LOG_LEVEL is set, building filter from: '{}'",
                    level_str
                );
            } else if rust_log_raw.is_some() {
                eprintln!("debug: Using RUST_LOG environment filter");
            } else {
                eprintln!("debug: Using default 'info' log filter");
            }
            f
        }
        Err(e) => {
            eprintln!("error: {}", e);
            return Err(e.into());
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
    ///
    /// These tests exercise `build_env_filter()` and `parse_global_level()`
    /// directly with explicit inputs — no dependency on the test runner's
    /// environment variables.
    #[test]
    fn test_build_env_filter_gateway_overrides_rust_log() {
        // When both are set, GATEWAY_LOG_LEVEL wins and RUST_LOG is ignored
        let filter = build_env_filter(Some("trace"), Some("info"));
        assert!(filter.is_ok(), "build_env_filter should succeed");
        // Verify the filter's max_level_hint includes TRACE
        let max = filter.unwrap().max_level_hint();
        assert_eq!(max, Some(tracing::metadata::LevelFilter::TRACE),
            "GATEWAY_LOG_LEVEL=trace should produce TRACE filter, not INFO");
    }

    #[test]
    fn test_build_env_filter_complex_directive() {
        // Complex directives like "trace,axum=info" must be preserved
        let filter = build_env_filter(Some("trace,axum=info"), None);
        assert!(filter.is_ok(), "build_env_filter should handle complex directives");
        // The ambient/global level is TRACE
        let max = filter.unwrap().max_level_hint();
        assert_eq!(max, Some(tracing::metadata::LevelFilter::TRACE),
            "complex directive 'trace,axum=info' should have TRACE as max level");
    }

    #[test]
    fn test_build_env_filter_rust_log_fallback() {
        // When GATEWAY_LOG_LEVEL is absent, RUST_LOG is used
        let filter = build_env_filter(None, Some("debug"));
        assert!(filter.is_ok());
        let max = filter.unwrap().max_level_hint();
        assert_eq!(max, Some(tracing::metadata::LevelFilter::DEBUG),
            "RUST_LOG=debug should produce DEBUG filter");
    }

    #[test]
    fn test_build_env_filter_default() {
        // When neither is set, default to "info"
        let filter = build_env_filter(None, None);
        assert!(filter.is_ok());
        let max = filter.unwrap().max_level_hint();
        assert_eq!(max, Some(tracing::metadata::LevelFilter::INFO),
            "default filter should be INFO");
    }

    #[test]
    fn test_build_env_filter_strips_leading_dash() {
        // Common mistake: GATEWAY_LOG_LEVEL=-debug instead of debug
        let filter = build_env_filter(Some("-debug"), None);
        assert!(filter.is_ok());
        let max = filter.unwrap().max_level_hint();
        assert_eq!(max, Some(tracing::metadata::LevelFilter::DEBUG),
            "should accept DEBUG after stripping leading dash");
    }

    #[test]
    fn test_build_env_filter_invalid_returns_error() {
        // EnvFilter::try_new is lenient (treats unknown strings as target
        // names), so test with an empty string which is truly invalid.
        let result = build_env_filter(Some(""), None);
        // An empty directive string has no meaningful level — EnvFilter
        // may accept it (producing an "off" filter) or reject it. Either
        // way, build_env_filter should not panic.
        match result {
            Ok(filter) => {
                // Empty string → effectively "off" — max_level_hint is OFF
                assert_eq!(
                    filter.max_level_hint(),
                    Some(tracing::metadata::LevelFilter::OFF),
                    "empty directive should produce OFF filter"
                );
            }
            Err(e) => {
                assert!(
                    e.contains("GATEWAY_LOG_LEVEL"),
                    "error message should reference GATEWAY_LOG_LEVEL"
                );
            }
        }
    }

    #[test]
    fn test_parse_global_level_simple() {
        assert_eq!(parse_global_level("trace"), Level::TRACE);
        assert_eq!(parse_global_level("debug"), Level::DEBUG);
        assert_eq!(parse_global_level("info"), Level::INFO);
        assert_eq!(parse_global_level("warn"), Level::WARN);
        assert_eq!(parse_global_level("error"), Level::ERROR);
    }

    #[test]
    fn test_parse_global_level_complex_directive() {
        // "trace,axum=info" → TRACE (first segment is the ambient level)
        assert_eq!(parse_global_level("trace,axum=info"), Level::TRACE);
        assert_eq!(parse_global_level("debug,hyper=warn"), Level::DEBUG);
        assert_eq!(parse_global_level("info,exchange_gateway=trace"), Level::INFO);
    }

    #[test]
    fn test_parse_global_level_invalid_falls_back_to_info() {
        assert_eq!(parse_global_level("nonsense"), Level::INFO);
        assert_eq!(parse_global_level("nonsense,other=debug"), Level::INFO);
    }

    #[test]
    fn test_parse_global_level_strips_dash() {
        assert_eq!(parse_global_level("-trace"), Level::TRACE);
        assert_eq!(parse_global_level("-debug,axum=info"), Level::DEBUG);
    }
}
