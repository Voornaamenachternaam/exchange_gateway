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

/// Alias for xml_escape for compatibility with existing code
pub fn escape_xml_text(s: &str) -> Cow<'_, str> {
    xml_escape(s)
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

/// Derive the primary SMTP address for a user given their auth username and the
/// configured mail domain. The local part of the username is kept (the domain
/// is canonicalised to `GATEWAY_MAIL_DOMAIN`), matching Stalwart's per-account
/// primary address (`{local}@{mail_domain}`).
///
/// Returns `None` when the resulting address is empty.
pub fn user_primary_email(username: &str, mail_domain: &str) -> Option<String> {
    let local = match username.rsplit_once('@') {
        Some((local, domain)) if !domain.is_empty() => local,
        Some((local, _)) => local,
        None => username,
    };
    let local = local.trim();
    if local.is_empty() || mail_domain.trim().is_empty() {
        return None;
    }
    Some(format!("{}@{}", local, mail_domain.trim()))
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

/// Canonicalize a username's email domain to the configured mail domain.
///
/// Regardless of what domain the client supplies (e.g. `contact@exchange.com`),
/// the gateway normalises it to `GATEWAY_MAIL_DOMAIN` (e.g. `contact@example.com`).
/// This ensures:
/// - Consistent DB owner keys across devices and sessions
/// - CalDAV URL construction (`/cal/{canonical}/`) matches the Stalwart home set
/// - `active_user_emails()` reports the correct primary SMTP address
///
/// # Examples
/// ```
/// use exchange_gateway::util::canonicalize_username;
/// assert_eq!(canonicalize_username("contact@exchange.com", "example.com"), "contact@example.com");
/// assert_eq!(canonicalize_username("contact", "example.com"), "contact@example.com");
/// assert_eq!(canonicalize_username("contact@", "example.com"), "contact@example.com");
/// assert_eq!(canonicalize_username("contact@example.com", "example.com"), "contact@example.com");
/// ```
pub fn canonicalize_username(username: &str, mail_domain: &str) -> String {
    let local = match username.rsplit_once('@') {
        Some((local, domain)) if !domain.is_empty() => local,
        Some((local, _)) => local, // trailing @ or empty domain
        None => username,          // no @ at all
    };
    format!("{}@{}", local, mail_domain)
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
    fn test_canonicalize_username() {
        // Domain replacement: non-canonical domain → canonical
        assert_eq!(
            canonicalize_username("contact@exchange.com", "example.com"),
            "contact@example.com"
        );
        // Already canonical: no change
        assert_eq!(
            canonicalize_username("contact@example.com", "example.com"),
            "contact@example.com"
        );
        // Plain username: append domain
        assert_eq!(
            canonicalize_username("alice", "example.com"),
            "alice@example.com"
        );
        // Trailing @: append domain
        assert_eq!(
            canonicalize_username("carol@", "example.com"),
            "carol@example.com"
        );
        // Subdomain: still replaced
        assert_eq!(
            canonicalize_username("bob@sub.example.com", "example.com"),
            "bob@example.com"
        );
        // Local part with dots preserved
        assert_eq!(
            canonicalize_username("first.last@other.org", "example.com"),
            "first.last@example.com"
        );
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
