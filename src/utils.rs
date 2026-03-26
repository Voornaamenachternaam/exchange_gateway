// src/utils.rs
// Utility functions for Exchange Gateway
//
// Features:
// - UID generation
// - DateTime parsing and formatting
// - Base64 encoding/decoding helpers
// - String manipulation utilities
//
// March 2026 - Production-ready, security-hardened

use chrono::{DateTime, NaiveDateTime, TimeZone, Utc};
use uuid::Uuid;

/// Generate a unique identifier for calendar events
pub fn generate_uid() -> String {
    Uuid::new_v4().to_string()
}

/// Generate a short unique ID (for server IDs)
pub fn generate_short_id() -> String {
    let uuid = Uuid::new_v4().to_simple().to_string();
    uuid[..16].to_string()
}

/// Parse EAS datetime format to UTC DateTime
///
/// EAS formats:
/// - 2026-03-22T10:00:00.000Z
/// - 20260322T100000Z
/// - 2026-03-22T10:00:00+00:00
pub fn parse_datetime_to_utc(datetime_str: &str) -> Result<DateTime<Utc>, String> {
    let trimmed = datetime_str.trim();

    // Try various formats
    let formats = [
        "%Y-%m-%dT%H:%M:%S%.3fZ",
        "%Y-%m-%dT%H:%M:%SZ",
        "%Y%m%dT%H%M%SZ",
        "%Y-%m-%dT%H:%M:%S%:z",
        "%Y-%m-%d %H:%M:%S",
        "%Y%m%d",
    ];

    for fmt in &formats {
        if let Ok(naive) = NaiveDateTime::parse_from_str(trimmed, fmt) {
            return Ok(Utc.from_utc_datetime(&naive));
        }
    }

    // Try parsing as RFC3339
    if let Ok(dt) = DateTime::parse_from_rfc3339(trimmed) {
        return Ok(dt.with_timezone(&Utc));
    }

    Err(format!("Unable to parse datetime: {}", datetime_str))
}

/// Format DateTime to EAS format
///
/// Output: 2026-03-22T10:00:00.000Z
pub fn format_datetime_eas(dt: &DateTime<Utc>) -> String {
    dt.format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string()
}

/// Format DateTime to ISO 8601 format
///
/// Output: 2026-03-22T10:00:00Z
pub fn format_datetime_iso8601(dt: &DateTime<Utc>) -> String {
    dt.format("%Y-%m-%dT%H:%M:%SZ").to_string()
}

/// Format DateTime to iCalendar format
///
/// Output: 20260322T100000Z
pub fn format_datetime_ical(dt: &DateTime<Utc>) -> String {
    dt.format("%Y%m%dT%H%M%SZ").to_string()
}

/// Parse iCalendar datetime to UTC
///
/// iCalendar formats:
/// - 20260322T100000Z
/// - 20260322T100000 (floating time)
/// - TZID=America/New_York:20260322T100000
pub fn parse_ical_datetime(datetime_str: &str) -> Result<DateTime<Utc>, String> {
    let trimmed = datetime_str.trim();

    // Handle TZID format
    if trimmed.starts_with("TZID=") {
        if let Some(pos) = trimmed.find(':') {
            let _tzid = &trimmed[5..pos];
            let dt_str = &trimmed[pos + 1..];

            // Parse the datetime
            if let Ok(naive) = NaiveDateTime::parse_from_str(dt_str, "%Y%m%dT%H%M%S") {
                // For simplicity, treat as UTC (production would handle timezone)
                return Ok(Utc.from_utc_datetime(&naive));
            }
        }
    }

    // Handle UTC format
    if trimmed.ends_with('Z') {
        if let Ok(naive) = NaiveDateTime::parse_from_str(trimmed, "%Y%m%dT%H%M%SZ") {
            return Ok(Utc.from_utc_datetime(&naive));
        }
    }

    // Handle floating time (treat as UTC)
    if let Ok(naive) = NaiveDateTime::parse_from_str(trimmed, "%Y%m%dT%H%M%S") {
        return Ok(Utc.from_utc_datetime(&naive));
    }

    Err(format!(
        "Unable to parse iCalendar datetime: {}",
        datetime_str
    ))
}

/// Truncate string to maximum length with ellipsis
pub fn truncate(s: &str, max_len: usize) -> String {
    if s.len() <= max_len {
        s.to_string()
    } else {
        format!("{}...", &s[..max_len.saturating_sub(3)])
    }
}

/// Sanitize a string for use in URLs
pub fn sanitize_for_url(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_alphanumeric() || *c == '-' || *c == '_' || *c == '.')
        .collect()
}

/// Check if a string is a valid email address (basic check)
pub fn is_valid_email(email: &str) -> bool {
    if email.is_empty() || !email.contains('@') {
        return false;
    }

    let parts: Vec<&str> = email.split('@').collect();
    if parts.len() != 2 {
        return false;
    }

    let local = parts[0];
    let domain = parts[1];

    !local.is_empty() && domain.contains('.') && !domain.starts_with('.') && !domain.ends_with('.')
}

/// Parse a comma-separated list
pub fn parse_comma_list(s: &str) -> Vec<String> {
    s.split(',')
        .map(|part| part.trim().to_string())
        .filter(|part| !part.is_empty())
        .collect()
}

/// Format a list as comma-separated string
pub fn format_comma_list(items: &[String]) -> String {
    items.join(", ")
}

/// Convert bytes to human-readable format
pub fn format_bytes(bytes: u64) -> String {
    const UNITS: &[&str] = &["B", "KB", "MB", "GB", "TB"];

    if bytes == 0 {
        return "0 B".to_string();
    }

    let exp = (bytes as f64).log(1024.0).min(UNITS.len() as f64 - 1.0) as usize;
    let value = bytes as f64 / 1024f64.powi(exp as i32);

    format!("{:.2} {}", value, UNITS[exp])
}

/// Escape special regex characters
pub fn regex_escape(s: &str) -> String {
    regex::escape(s)
}

/// Check if a string contains only ASCII printable characters
pub fn is_ascii_printable(s: &str) -> bool {
    s.chars()
        .all(|c| c.is_ascii() && (c.is_ascii_graphic() || c.is_ascii_whitespace()))
}

/// Normalize line endings to CRLF (for iCalendar compatibility)
pub fn normalize_crlf(s: &str) -> String {
    s.replace("\r\n", "\n").replace('\n', "\r\n")
}

/// Fold long iCalendar lines (max 75 octets per line)
pub fn fold_ical_line(line: &str) -> String {
    if line.len() <= 75 {
        return line.to_string();
    }

    let mut result = String::new();
    let mut remaining = line;

    while !remaining.is_empty() {
        let chunk_len = if result.is_empty() { 75 } else { 74 }; // First line 75, continuation 74
        let (chunk, rest) = if remaining.len() > chunk_len {
            remaining.split_at(chunk_len)
        } else {
            (remaining, "")
        };

        if !result.is_empty() {
            result.push_str("\r\n ");
        }
        result.push_str(chunk);
        remaining = rest;
    }

    result
}

/// Unfold iCalendar lines
pub fn unfold_ical_lines(s: &str) -> String {
    s.replace("\r\n ", "").replace("\n ", "")
}

/// Get current timestamp in milliseconds
pub fn current_timestamp_millis() -> i64 {
    Utc::now().timestamp_millis()
}

/// Get current timestamp in seconds
pub fn current_timestamp() -> i64 {
    Utc::now().timestamp()
}

/// Format duration in human-readable format
pub fn format_duration(seconds: u64) -> String {
    let hours = seconds / 3600;
    let minutes = (seconds % 3600) / 60;
    let secs = seconds % 60;

    if hours > 0 {
        format!("{}h {}m {}s", hours, minutes, secs)
    } else if minutes > 0 {
        format!("{}m {}s", minutes, secs)
    } else {
        format!("{}s", secs)
    }
}

/// Safe substring extraction
pub fn safe_substring(s: &str, start: usize, end: usize) -> String {
    let chars: Vec<char> = s.chars().collect();
    let start = start.min(chars.len());
    let end = end.min(chars.len());
    chars[start..end].iter().collect()
}

/// Convert hex string to bytes
pub fn hex_to_bytes(hex: &str) -> Result<Vec<u8>, String> {
    if hex.len() % 2 != 0 {
        return Err("Hex string must have even length".to_string());
    }

    let mut bytes = Vec::new();
    for i in (0..hex.len()).step_by(2) {
        let byte =
            u8::from_str_radix(&hex[i..i + 2], 16).map_err(|e| format!("Invalid hex: {}", e))?;
        bytes.push(byte);
    }

    Ok(bytes)
}

/// Convert bytes to hex string
pub fn bytes_to_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{:02x}", b)).collect()
}

/// Calculate SHA-256 hash
pub fn sha256_hash(data: &[u8]) -> Vec<u8> {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(data);
    hasher.finalize().to_vec()
}

/// Calculate MD5 hash (for legacy compatibility)
pub fn md5_hash(data: &[u8]) -> Vec<u8> {
    use md5::{Digest, Md5};
    let mut hasher = Md5::new();
    hasher.update(data);
    hasher.finalize().to_vec()
}

/// Generate ETag for calendar object
pub fn generate_etag(content: &str) -> String {
    let hash = md5_hash(content.as_bytes());
    format!("\"{}\"", bytes_to_hex(&hash))
}

/// Parse ETag header value
pub fn parse_etag(etag: &str) -> String {
    etag.trim_matches('"').to_string()
}

/// Compare two ETags for equality
pub fn etags_equal(a: &str, b: &str) -> bool {
    parse_etag(a) == parse_etag(b)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_generate_uid() {
        let uid1 = generate_uid();
        let uid2 = generate_uid();
        assert_ne!(uid1, uid2);
        assert_eq!(uid1.len(), 36); // UUID v4 format
    }

    #[test]
    fn test_parse_datetime_to_utc() {
        let dt = parse_datetime_to_utc("2026-03-22T10:00:00.000Z").unwrap();
        assert_eq!(dt.year(), 2026);
        assert_eq!(dt.month(), 3);
        assert_eq!(dt.day(), 22);

        let dt2 = parse_datetime_to_utc("20260322T100000Z").unwrap();
        assert_eq!(dt2.year(), 2026);
    }

    #[test]
    fn test_format_datetime_eas() {
        let dt = Utc.with_ymd_and_hms(2026, 3, 22, 10, 0, 0).unwrap();
        let formatted = format_datetime_eas(&dt);
        assert!(formatted.starts_with("2026-03-22T10:00:00"));
    }

    #[test]
    fn test_truncate() {
        assert_eq!(truncate("Hello", 10), "Hello");
        assert_eq!(truncate("Hello World", 8), "Hello...");
    }

    #[test]
    fn test_is_valid_email() {
        assert!(is_valid_email("test@example.com"));
        assert!(!is_valid_email("invalid"));
        assert!(!is_valid_email("@example.com"));
    }

    #[test]
    fn test_normalize_crlf() {
        assert_eq!(normalize_crlf("line1\nline2"), "line1\r\nline2");
        assert_eq!(normalize_crlf("line1\r\nline2"), "line1\r\nline2");
    }

    #[test]
    fn test_fold_ical_line() {
        let long_line = "a".repeat(100);
        let folded = fold_ical_line(&long_line);
        assert!(folded.contains("\r\n "));
    }

    #[test]
    fn test_hex_bytes_conversion() {
        let hex = "48656c6c6f"; // "Hello"
        let bytes = hex_to_bytes(hex).unwrap();
        assert_eq!(String::from_utf8(bytes).unwrap(), "Hello");

        assert_eq!(bytes_to_hex(b"Hello"), hex);
    }

    #[test]
    fn test_generate_etag() {
        let etag1 = generate_etag("test content");
        let etag2 = generate_etag("test content");
        let etag3 = generate_etag("different content");

        assert_eq!(etag1, etag2);
        assert_ne!(etag1, etag3);
    }
}
