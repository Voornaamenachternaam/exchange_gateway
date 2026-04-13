// src/util.rs
//! Common utility functions for the Exchange Gateway.
//!
//! This module provides shared functionality to avoid code duplication
//! across the protocol handlers.

use chrono::{TimeZone, Utc};

/// Escapes special XML characters in a string.
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
/// assert_eq!(xml_escape("a<b&c>d"), "a&lt;b&amp;c&gt;d");
/// ```
pub fn xml_escape(s: &str) -> String {
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
/// This is a subset of [`xml_escape`] that only escapes the three
/// characters that must be escaped in XML text content: `&`, `<`, and `>`.
pub fn xml_escape_text(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            _ => out.push(ch),
        }
    }
    out
}

/// Escapes special XML characters for attribute values.
///
/// This escapes the five characters that must be escaped in XML attributes.
pub fn xml_escape_attr(s: &str) -> String {
    xml_escape(s)
}

/// Parses an ISO 8601 datetime string to a UTC DateTime.
///
/// Supports multiple formats commonly used in EWS and EAS protocols:
/// - `YYYY-MM-DDTHH:MM:SSZ`
/// - `YYYY-MM-DDTHH:MM:SS.sssZ`
/// - `YYYY-MM-DDTHH:MM:SS+HH:MM`
/// - `YYYY-MM-DDTHH:MM:SS-HH:MM`
/// - `YYYYMMDDTHHMMSSZ`
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

    // Try without timezone (assume UTC)
    // Note: 'Z' suffix is trimmed before parsing, so formats don't include it
    let formats = [
        "%Y-%m-%dT%H:%M:%S%.f",
        "%Y-%m-%dT%H:%M:%S",
        "%Y%m%dT%H%M%S",
    ];

    for fmt in formats {
        if let Ok(ndt) = chrono::NaiveDateTime::parse_from_str(val.trim_end_matches('Z'), fmt) {
            return Some(Utc.from_utc_datetime(&ndt));
        }
    }

    // Try date only
    if let Ok(nd) = chrono::NaiveDate::parse_from_str(val, "%Y-%m-%d") {
        return Some(Utc.from_utc_datetime(&nd.and_hms_opt(0, 0, 0)?));
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
    fn test_parse_datetime_invalid() {
        assert!(parse_datetime("").is_none());
        assert!(parse_datetime("invalid").is_none());
        assert!(parse_datetime("2024-13-01").is_none());
    }
}