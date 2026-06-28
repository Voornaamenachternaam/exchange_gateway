// src/vcard.rs
// Minimal vCard parser/builder for contacts integration.
use anyhow::Result;

#[derive(Debug, Clone, Default)]
pub struct Vcard {
    pub full_name: String,
    pub email: Vec<Email>,
    pub telephone: Vec<Telephone>,
    pub org: Vec<Org>,
    pub title: Option<Title>,
    pub name: Vec<Name>, // from N property
    pub uid: Option<String>,
}

#[derive(Debug, Clone)]
pub struct Email {
    pub email: String,
}

#[derive(Debug, Clone)]
pub struct Telephone {
    pub number: String,
}

#[derive(Debug, Clone)]
pub struct Org {
    pub value: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct Title {
    pub value: String,
}

#[derive(Debug, Clone)]
pub struct Name {
    pub value: String,
}

/// Parse a vCard string into a Vcard struct.
/// Supports properties: FN, EMAIL, TEL, ORG, TITLE, N, UID.
pub fn parse_vcard_from_data(data: &str) -> Result<Vcard> {
    let mut out = Vcard::default();
    for line in data.lines() {
        let line = line.trim_end();
        if line.is_empty() {
            continue;
        }
        // Simple unfolding: ignore lines starting with whitespace (continuation)
        if line.starts_with(' ') || line.starts_with('\t') {
            continue;
        }
        let Some((name, rest)) = line.split_once(':') else { continue };
        let value = rest.trim_start();
        match name.to_ascii_uppercase().as_str() {
            "FN" => out.full_name = value.to_string(),
            "EMAIL" => out.email.push(Email { email: value.to_string() }),
            "TEL" => out.telephone.push(Telephone { number: value.to_string() }),
            "ORG" => {
                let parts: Vec<String> = value.split(';').map(|s| s.trim().to_string()).collect();
                if !parts.is_empty() {
                    out.org.push(Org { value: parts });
                }
            }
            "TITLE" => out.title = Some(Title { value: value.to_string() }),
            "N" => {
                let comps: Vec<&str> = value.split(';').collect();
                if let Some(fam) = comps.get(0) {
                    let mut nm = String::new();
                    if !fam.is_empty() {
                        nm.push_str(fam);
                    }
                    if let Some(given) = comps.get(1) {
                        if !given.is_empty() {
                            if !nm.is_empty() {
                                nm.push(' ');
                            }
                            nm.push_str(given);
                        }
                    }
                    if !nm.is_empty() {
                        out.name.push(Name { value: nm });
                    }
                }
            }
            "UID" => {
                out.uid = Some(value.to_string());
            }
            _ => {}
        }
    }
    Ok(out)
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
        format!("FN:{}", escape_text(display_name)),
    ];

    if let Some(email) = email {
        lines.push(format!("EMAIL;type=INTERNET;type=WORK:{}", escape_text(email)));
    }
    if let Some(phone) = phone {
        lines.push(format!("TEL;type=WORK;type=VOICE:{}", escape_text(phone)));
    }
    if let Some(org) = organization {
        lines.push(format!("ORG:{}", escape_text(org)));
    }
    if let Some(t) = title {
        lines.push(format!("TITLE:{}", escape_text(t)));
    }

    lines.push("END:VCARD".to_string());
    Ok(lines.join("\r\n"))
}

/// Escape text for vCard per RFC 6350 (minimal set)
fn escape_text(s: &str) -> String {
    let mut out = String::new();
    for ch in s.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            ';' => out.push_str("\\;"),
            ',' => out.push_str("\\,"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            _ => out.push(ch),
        }
    }
    out
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

    #[test]
    fn test_parse_vcard_simple() {
        let data = "\
BEGIN:VCARD\r
VERSION:3.0\r
UID:abc\r
FN:Test User\r
EMAIL:test@example.com\r
TEL;type=WORK:+123456789\r
ORG:Example Inc\r
TITLE:Engineer\r
END:VCARD";
        let v = parse_vcard_from_data(data).unwrap();
        assert_eq!(v.full_name, "Test User");
        assert_eq!(v.email[0].email, "test@example.com");
        assert_eq!(v.telephone[0].number, "+123456789");
        assert_eq!(v.org[0].value[0], "Example Inc");
        assert_eq!(v.title.as_ref().unwrap().value, "Engineer");
        assert_eq!(v.uid, Some("abc".to_string()));
    }
}
