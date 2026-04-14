// src/util.rs
//! Common utility functions for the Exchange Gateway.
//!
//! This module provides shared functionality to avoid code duplication
//! across the protocol handlers.

use chrono::{TimeZone, Utc};
use std::fmt;

/// Escapes special XML characters in a string.
///
/// This function returns an `impl Display` for lazy evaluation, meaning
/// the string is only allocated when actually displayed or converted.
///
/// This function escapes the five predefined XML entities:
/// - `&` → `&amp;`
/// - `<` → `&lt;`
/// - `>` → `&gt;`
/// - `"` → `&quot;`
/// - `'` → `&apos;`
///
/// # Example
/// ```
/// use exchange_gateway::util::xml_escape;
/// use std::fmt::Display;
/// 
/// let escaped = xml_escape("a<b&c>d");
/// assert_eq!(format!("{}", escaped), "a&lt;b&amp;c&gt;d");
/// ```
pub fn xml_escape(s: &str) -> impl fmt::Display + '_ {
    struct XmlEscape<'a>(&'a str);
    
    impl<'a> fmt::Display for XmlEscape<'a> {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            for ch in self.0.chars() {
                match ch {
                    '&' => f.write_str("&amp;")?,
                    '<' => f.write_str("&lt;")?,
                    '>' => f.write_str("&gt;")?,
                    '"' => f.write_str("&quot;")?,
                    '\'' => f.write_str("&apos;")?,
                    _ => f.write_char(ch)?,
                }
            }
            Ok(())
        }
    }
    
    XmlEscape(s)
}

/// Escapes special XML characters into a String (eager evaluation).
///
/// Use this when you need an owned String rather than lazy evaluation.
///
/// # Example
/// ```
/// use exchange_gateway::util::xml_escape_owned;
/// assert_eq!(xml_escape_owned("a<b&c>d"), "a&lt;b&amp;c&gt;d");
/// ```
pub fn xml_escape_owned(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            _ => out.push(ch),
        }
    }
    out
}

/// Escapes special XML characters for text content.
///
/// This is a lazy-evaluation version that only escapes the three
/// characters that must be escaped in XML text content: `&`, `<`, and `>`.
pub fn xml_escape_text(s: &str) -> impl fmt::Display + '_ {
    struct XmlEscapeText<'a>(&'a str);
    
    impl<'a> fmt::Display for XmlEscapeText<'a> {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            for ch in self.0.chars() {
                match ch {
                    '&' => f.write_str("&amp;")?,
                    '<' => f.write_str("&lt;")?,
                    '>' => f.write_str("&gt;")?,
                    _ => f.write_char(ch)?,
                }
            }
            Ok(())
        }
    }
    
    XmlEscapeText(s)
}

/// Escapes special XML characters for attribute values.
///
/// This returns a lazy `impl Display` that escapes all five required characters.
pub fn xml_escape_attr(s: &str) -> impl fmt::Display + '_ {
    xml_escape(s)
}

/// Parses an ISO 8601 datetime string to a UTC DateTime.
///
/// Supports multiple formats commonly used in EWS, EAS, and iCalendar (RFC 5545) protocols:
/// - `YYYY-MM-DDTHH:MM:SSZ`
/// - `YYYY-MM-DDTHH:MM:SS.sssZ`
/// - `YYYY-MM-DDTHH:MM:SS+HH:MM`
/// - `YYYY-MM-DDTHH:MM:SS-HH:MM`
/// - `YYYYMMDDTHHMMSSZ`
/// - `YYYY-MM-DDZ` (date only with Z suffix)
/// - `YYYYMMDD` (date only, iCalendar UNTIL format)
///
/// Returns `None` if the string cannot be parsed.
pub fn parse_datetime(val: &str) -> Option<chrono::DateTime<Utc>> {
    let val = val.trim();
    if val.is_empty() {
        return None;
    }

    // Try ISO 8601 with timezone offset
    if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(val) {
        return Some(dt.with_timezone(&Utc));
    }

    // Trim 'Z' suffix once for all subsequent parsing
    // This handles both standard 'Z' suffix and edge cases with extra 'Z'
    let val_no_z = val.trim_end_matches('Z');

    // Try without timezone (assume UTC)
    let datetime_formats = [
        "%Y-%m-%dT%H:%M:%S%.f",
        "%Y-%m-%dT%H:%M:%S",
        "%Y%m%dT%H%M%S",
    ];

    for fmt in datetime_formats {
        if let Ok(ndt) = chrono::NaiveDateTime::parse_from_str(val_no_z, fmt) {
            return Some(ndt.and_utc());
        }
    }

    // Try date only (handles "YYYY-MM-DD", "YYYY-MM-DDZ", and "YYYYMMDD")
    let date_formats = ["%Y-%m-%d", "%Y%m%d"];
    for fmt in date_formats {
        if let Ok(nd) = chrono::NaiveDate::parse_from_str(val_no_z, fmt) {
            return Some(nd.and_hms_opt(0, 0, 0)?.and_utc());
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Datelike, Timelike};

    #[test]
    fn test_xml_escape_all_entities() {
        assert_eq!(xml_escape("&"), "&amp;");
        assert_eq!(xml_escape("<"), "&lt;");
        assert_eq!(xml_escape(">"), "&gt;");
        assert_eq!(xml_escape("\""), "&quot;");
        assert_eq!(xml_escape("'"), "&apos;");
    }

    #[test]
    fn test_xml_escape_combined() {
        assert_eq!(
            xml_escape("Hello <world> & \"friends\""),
            "Hello &lt;world&gt; &amp; &quot;friends&quot;"
        );
    }

    #[test]
    fn test_xml_escape_text() {
        assert_eq!(xml_escape_text("&<>'\""), "&amp;&lt;&gt;'\"");
    }

    #[test]
    fn test_parse_datetime_rfc3339() {
        let dt = parse_datetime("2024-01-15T10:30:00Z").unwrap();
        assert_eq!(dt.year(), 2024);
        assert_eq!(dt.month(), 1);
        assert_eq!(dt.day(), 15);
    }

    #[test]
    fn test_parse_datetime_with_tz() {
        let dt = parse_datetime("2024-01-15T10:30:00+05:00").unwrap();
        assert_eq!(dt.year(), 2024);
    }

    #[test]
    fn test_parse_datetime_compact() {
        let dt = parse_datetime("20240115T103000Z").unwrap();
        assert_eq!(dt.year(), 2024);
        assert_eq!(dt.month(), 1);
        assert_eq!(dt.day(), 15);
    }

    #[test]
    fn test_parse_datetime_date_only() {
        let dt = parse_datetime("2024-01-15").unwrap();
        assert_eq!(dt.year(), 2024);
        assert_eq!(dt.hour(), 0);
        assert_eq!(dt.minute(), 0);
    }

    #[test]
    fn test_parse_datetime_date_only_with_z() {
        let dt = parse_datetime("2024-01-15Z").unwrap();
        assert_eq!(dt.year(), 2024);
        assert_eq!(dt.hour(), 0);
        assert_eq!(dt.minute(), 0);
    }

    #[test]
    fn test_parse_datetime_icalendar_date() {
        // YYYYMMDD format used in iCalendar UNTIL
        let dt = parse_datetime("20240115").unwrap();
        assert_eq!(dt.year(), 2024);
        assert_eq!(dt.month(), 1);
        assert_eq!(dt.day(), 15);
        assert_eq!(dt.hour(), 0);
        assert_eq!(dt.minute(), 0);
    }

    #[test]
    fn test_parse_datetime_invalid() {
        assert!(parse_datetime("").is_none());
        assert!(parse_datetime("invalid").is_none());
        assert!(parse_datetime("2024-13-01").is_none());
    }
}