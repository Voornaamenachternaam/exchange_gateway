// src/util.rs

use chrono::Utc;
use std::borrow::Cow;
use unicode_normalization::UnicodeNormalization;

pub fn xml_escape(s: &str) -> Cow<'_, str> {
    quick_xml::escape::escape(s)
}

pub fn xml_escape_text(s: &str) -> Cow<'_, str> {
    quick_xml::escape::partial_escape(s)
}

pub fn sanitize_path_segment(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' || c == '_' || c == '.' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

pub fn truncate_string(s: &str, max_len: usize) -> String {
    if s.len() <= max_len {
        s.to_string()
    } else {
        let target_len = max_len.saturating_sub(3);
        let end = s
            .char_indices()
            .take_while(|(idx, _)| *idx < target_len)
            .last()
            .map(|(idx, c)| idx + c.len_utf8())
            .unwrap_or(0);
        format!("{}...", &s[..end])
    }
}

pub fn nfc(input: &str) -> String {
    input.nfc().collect()
}

pub fn normalize_email(email: &str) -> String {
    let trimmed = email.trim();
    let lower = trimmed.to_lowercase();
    let stripped = if lower.starts_with("mailto:") {
        &trimmed["mailto:".len()..]
    } else {
        trimmed
    };
    let normalized: String = stripped.nfc().collect::<String>().to_lowercase();
    if !email_address::EmailAddress::is_valid(&normalized) {
        tracing::debug!(
            "Normalized email does not pass RFC validation: {}",
            normalized
        );
    }
    normalized
}

pub fn escape_ical_text(s: &str) -> String {
    let mut result = String::with_capacity(s.len() + s.len() / 10);
    for c in s.chars() {
        match c {
            '\\' => result.push_str("\\\\"),
            ';' => result.push_str("\\;"),
            ',' => result.push_str("\\,"),
            '\n' => result.push_str("\\n"),
            '\r' => {}
            _ => result.push(c),
        }
    }
    result
}

/// Strip domain prefix from username: "DOMAIN\user" → "user"
/// If backslash is at the end (e.g., "user\"), strip it instead of returning empty string.
pub fn normalize_username(username: &str) -> &str {
    if let Some(backslash) = username.rfind('\\') {
        if backslash + 1 < username.len() {
            &username[backslash + 1..]
        } else {
            // Backslash at the end: strip it
            &username[..backslash]
        }
    } else {
        username
    }
}

/// Format datetime for EWS responses with proper UTC 'Z' suffix
/// Converts from RFC3339 offset format (+00:00) to .NET expected format (Z)
pub fn format_ews_datetime(dt: &chrono::DateTime<Utc>) -> String {
    dt.format("%Y-%m-%dT%H:%M:%SZ").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_xml_escape() {
        assert_eq!(
            xml_escape("a<b>c&d\"e'f"),
            "a&lt;b&gt;c&amp;d&quot;e&apos;f"
        );
    }

    #[test]
    fn test_xml_escape_text() {
        assert_eq!(xml_escape_text("a<b>c&d"), "a&lt;b&gt;c&amp;d");
    }

    #[test]
    fn test_sanitize_path_segment() {
        assert_eq!(sanitize_path_segment("hello world"), "hello_world");
        assert_eq!(sanitize_path_segment("test/file:name"), "test_file_name");
    }

    #[test]
    fn test_truncate_string() {
        assert_eq!(truncate_string("hello", 10), "hello");
        assert_eq!(truncate_string("hello world", 8), "hello...");
    }
    #[test]
    fn test_nfc_normalization() {
        let nfc_e: String = "\u{00e9}".nfc().collect();
        let nfd_e: String = "\u{0065}\u{0301}".nfd().collect();
        assert_eq!(nfc_e.len(), 2);
        assert_eq!(nfd_e.len(), 3);
        assert_eq!(super::nfc(&nfd_e), nfc_e);
    }

    #[test]
    fn test_normalize_email() {
        assert_eq!(normalize_email("User@Example.COM"), "user@example.com");
        assert_eq!(
            normalize_email("mailto:User@Example.COM"),
            "user@example.com"
        );
        assert_eq!(
            normalize_email("MAILTO:User@Example.COM"),
            "user@example.com"
        );
        let nfd_email = "user@\u{0065}\u{0301}xample.com";
        assert_eq!(normalize_email(nfd_email), "user@\u{00e9}xample.com");
        assert_eq!(normalize_email("alice@example.com"), "alice@example.com");
    }

    #[test]
    fn test_normalize_username() {
        assert_eq!(normalize_username("user"), "user");
        assert_eq!(normalize_username("DOMAIN\\user"), "user");
        assert_eq!(normalize_username("EXAMPLE\\john.doe"), "john.doe");
        assert_eq!(normalize_username("user@example.com"), "user@example.com");
        assert_eq!(normalize_username("\\user"), "user"); // backslash at start
        assert_eq!(normalize_username("user\\"), "user"); // backslash at end
        assert_eq!(normalize_username("DOMAIN\\user\\extra"), "extra"); // multiple backslashes - last wins
    }

    #[test]
    fn test_format_ews_datetime() {
        use chrono::{TimeZone, Utc};
        let dt = Utc.with_ymd_and_hms(2026, 6, 15, 11, 0, 0).unwrap();
        assert_eq!(format_ews_datetime(&dt), "2026-06-15T11:00:00Z");

        // Check that no offset is appended
        let dt2 = Utc.with_ymd_and_hms(2025, 12, 31, 23, 59, 59).unwrap();
        let formatted = format_ews_datetime(&dt2);
        assert!(!formatted.contains('+'));
        assert!(formatted.ends_with('Z'));
    }

    #[test]
    fn test_escape_ical_text() {
        assert_eq!(escape_ical_text("hello;world"), "hello\\;world");
        assert_eq!(escape_ical_text("a,b\\c"), "a\\,b\\\\c");
        assert_eq!(escape_ical_text("line1\nline2"), "line1\\nline2");
        assert_eq!(escape_ical_text("cr\r\nlf"), "cr\\nlf");
    }
}
