// src/util.rs

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
        // Use char_indices to safely handle multi-byte UTF-8 characters
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
    let stripped = trimmed
        .strip_prefix("mailto:")
        .unwrap_or(trimmed.strip_prefix("MAILTO:").unwrap_or(trimmed));
    stripped.nfc().collect::<String>().to_lowercase()
}

/// Escape text for iCal (RFC 5545) TEXT values.
/// Escapes `\`, `;`, `,` and newlines; strips `\r`.
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
        // \u{00e9} = NFC (precomposed é)
        // \u{0065}\u{0301} = NFD (e + combining acute accent)
        let nfc_e: String = "\u{00e9}".nfc().collect();
        let nfd_e: String = "\u{0065}\u{0301}".nfd().collect();
        assert_eq!(nfc_e.len(), 2); // NFC: single precomposed char in UTF-8
        assert_eq!(nfd_e.len(), 3); // NFD: base + combining accent
        assert_eq!(super::nfc(&nfd_e), nfc_e);
    }

    #[test]
    fn test_normalize_email() {
        // Lowercases
        assert_eq!(normalize_email("User@Example.COM"), "user@example.com");
        // Strips mailto: prefix (case-insensitive)
        assert_eq!(
            normalize_email("mailto:User@Example.COM"),
            "user@example.com"
        );
        assert_eq!(
            normalize_email("MAILTO:User@Example.COM"),
            "user@example.com"
        );
        // NFC-normalizes decomposed form (NFD \u{0065}\u{0301} = e + combining acute)
        let nfd_email = "user@\u{0065}\u{0301}xample.com";
        assert_eq!(normalize_email(nfd_email), "user@\u{00e9}xample.com");
        // Already-normalized ASCII passes through unchanged
        assert_eq!(normalize_email("alice@example.com"), "alice@example.com");
    }

    #[test]
    fn test_escape_ical_text() {
        assert_eq!(escape_ical_text("hello;world"), "hello\\;world");
        assert_eq!(escape_ical_text("a,b\\c"), "a\\,b\\\\c");
        assert_eq!(escape_ical_text("line1\nline2"), "line1\\nline2");
        assert_eq!(escape_ical_text("cr\r\nlf"), "cr\\nlf");
    }
}
