use anyhow::{anyhow, Result};
use tracing::warn;
use vcard::parser::parse_vcard;
use vcard::Vcard;

/// Parse a vCard from raw iCalendar/text data.
pub fn parse_vcard_from_data(data: &str) -> Result<Vcard> {
    parse_vcard(data)
        .map_err(|e| anyhow!("Failed to parse vCard: {}", e))
}

/// Build a minimal vCard from contact fields.
/// Used for creating contacts via CardDAV.
pub fn build_vcard(
    uid: &str,
    display_name: &str,
    email: Option<&str>,
    phone: Option<&str>,
    organization: Option<&str>,
    title: Option<&str>,
) -> Result<String> {
    let mut lines = vec![
        "BEGIN:VCARD".to_string(),
        "VERSION:3.0".to_string(),
        format!("UID:{}", uid),
        format!("FN:{}", display_name),
    ];

    if let Some(email) = email {
        lines.push(format!("EMAIL;type=INTERNET;type=WORK:{}", email));
    }
    if let Some(phone) = phone {
        lines.push(format!("TEL;type=WORK;type=VOICE:{}", phone));
    }
    if let Some(org) = organization {
        lines.push(format!("ORG:{}", org));
    }
    if let Some(title) = title {
        lines.push(format!("TITLE:{}", title));
    }

    lines.push("END:VCARD".to_string());
    Ok(lines.join("\r\n"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_build_vcard_minimal() {
        let v = build_vcard("uid123", "John Doe", Some("john@example.com"), None, None, None).unwrap();
        assert!(v.contains("BEGIN:VCARD"));
        assert!(v.contains("FN:John Doe"));
        assert!(v.contains("EMAIL;type=INTERNET;type=WORK:john@example.com"));
        assert!(v.contains("UID:uid123"));
    }
}
