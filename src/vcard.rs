// src/vcard.rs
// vCard parser/builder for contacts integration with enum-based property model.
use anyhow::Result;

/// A vCard with a list of properties.
#[derive(Debug, Clone, Default)]
pub struct Vcard {
    pub properties: Vec<Property>,
}

/// vCard property types per RFC 6350
#[derive(Debug, Clone)]
pub enum Property {
    /// FN (Formatted Name)
    Fn(Fn),
    /// EMAIL
    Email(Email),
    /// TEL (Telephone)
    Tel(Tel),
    /// ORG (Organization)
    Org(Org),
    /// TITLE
    Title(Title),
    /// N (Structured name)
    N(Name),
    /// UID
    Uid(Uid),
}

/// FN property
#[derive(Debug, Clone)]
pub struct Fn {
    pub value: String,
}

/// EMAIL property
#[derive(Debug, Clone, Default)]
pub struct Email {
    pub email: String,
}

/// TEL property
#[derive(Debug, Clone, Default)]
pub struct Tel {
    pub number: String,
    pub params: Vec<Parameter>,
}

/// ORG property
#[derive(Debug, Clone)]
pub struct Org {
    pub value: Vec<String>,
}

/// TITLE property
#[derive(Debug, Clone)]
pub struct Title {
    pub value: String,
}

/// N property (structured name)
#[derive(Debug, Clone)]
pub struct Name {
    pub value: String,
}

/// UID property
#[derive(Debug, Clone)]
pub struct Uid {
    pub value: String,
}

/// vCard parameter types (simplified subset)
#[derive(Debug, Clone)]
pub enum Parameter {
    Type(Type),
    // Other parameters can be added as needed
}

/// TYPE parameter values
#[derive(Debug, Clone)]
pub enum Type {
    Work,
    Home,
    Voice,
    Cell,
    // Extend as needed
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
            "FN" => out.properties.push(Property::Fn(Fn { value: value.to_string() })),
            "EMAIL" => {
                out.properties.push(Property::Email(Email {
                    email: value.to_string(),
                }))
            }
            "TEL" => out.properties.push(Property::Tel(Tel {
                number: value.to_string(),
                params: vec![],
            })),
            "ORG" => {
                let parts: Vec<String> = value.split(';').map(|s| s.trim().to_string()).collect();
                if !parts.is_empty() {
                    out.properties.push(Property::Org(Org { value: parts }));
                }
            }
            "TITLE" => out.properties.push(Property::Title(Title {
                value: value.to_string(),
            })),
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
                        out.properties.push(Property::N(Name { value: nm }));
                    }
                }
            }
            "UID" => out
                .properties
                .push(Property::Uid(Uid { value: value.to_string() })),
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
    let mut vcard = Vcard::default();
    vcard.properties.push(Property::Fn(Fn {
        value: display_name.to_string(),
    }));
    vcard.properties.push(Property::Uid(Uid {
        value: uid.to_string(),
    }));
    if let Some(email_str) = email {
        vcard.properties.push(Property::Email(Email {
            email: email_str.to_string(),
        }));
    }
    if let Some(phone_str) = phone {
        let mut tel = Tel {
            number: phone_str.to_string(),
            params: vec![Parameter::Type(Type::Work), Parameter::Type(Type::Voice)],
        };
        vcard.properties.push(Property::Tel(tel));
    }
    if let Some(org_str) = organization {
        let parts: Vec<String> = org_str.split(';').map(|s| s.trim().to_string()).collect();
        vcard.properties.push(Property::Org(Org { value: parts }));
    }
    if let Some(title_str) = title {
        vcard.properties.push(Property::Title(Title {
            value: title_str.to_string(),
        }));
    }

    Ok(vcard.to_string())
}

impl std::fmt::Display for Vcard {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("BEGIN:VCARD\r\nVERSION:3.0\r\n")?;
        for prop in &self.properties {
            match prop {
                Property::Fn(fn_val) => {
                    f.write_str(&format!("FN:{}\r\n", escape_text(&fn_val.value)))?;
                }
                Property::Email(email) => {
                    f.write_str(&format!("EMAIL;type=INTERNET;type=WORK:{}\r\n", escape_text(&email.email)))?;
                }
                Property::Tel(tel) => {
                    let param_str = if tel.params.is_empty() {
                        String::new()
                    } else {
                        tel.params.iter().map(|p| match p {
                            Parameter::Type(tp) => match tp {
                                Type::Work => "type=WORK",
                                Type::Home => "type=HOME",
                                Type::Voice => "type=VOICE",
                                Type::Cell => "type=CELL",
                            },
                        }).collect::<Vec<_>>().join(";")
                    };
                    if param_str.is_empty() {
                        f.write_str(&format!("TEL:{}\r\n", escape_text(&tel.number)))?;
                    } else {
                        f.write_str(&format!("TEL;{}:{}\r\n", param_str, escape_text(&tel.number)))?;
                    }
                }
                Property::Org(org) => {
                    let org_str = org.value.join(";");
                    f.write_str(&format!("ORG:{}\r\n", escape_text(&org_str)))?;
                }
                Property::Title(title) => {
                    f.write_str(&format!("TITLE:{}\r\n", escape_text(&title.value)))?;
                }
                Property::N(name) => {
                    f.write_str(&format!("N:{};;;;\r\n", escape_text(&name.value)))?;
                }
                Property::Uid(uid) => {
                    f.write_str(&format!("UID:{}\r\n", uid.value))?;
                }
            }
        }
        f.write_str("END:VCARD")?;
        Ok(())
    }
}

impl Vcard {
    /// Extract full name from FN property
    pub fn full_name(&self) -> Option<&str> {
        for prop in &self.properties {
            if let Property::Fn(fn_val) = prop {
                return Some(fn_val.value.as_str());
            }
        }
        None
    }

    /// Get email addresses
    pub fn emails(&self) -> Vec<&str> {
        let mut emails = Vec::new();
        for prop in &self.properties {
            if let Property::Email(email) = prop {
                emails.push(email.email.as_str());
            }
        }
        emails
    }

    /// Get phone numbers
    pub fn phones(&self) -> Vec<&str> {
        let mut phones = Vec::new();
        for prop in &self.properties {
            if let Property::Tel(tel) = prop {
                phones.push(tel.number.as_str());
            }
        }
        phones
    }

    /// Get organization
    pub fn org(&self) -> Option<Vec<String>> {
        for prop in &self.properties {
            if let Property::Org(org) = prop {
                return Some(org.value.clone());
            }
        }
        None
    }

    /// Get title
    pub fn title(&self) -> Option<&str> {
        for prop in &self.properties {
            if let Property::Title(title) = prop {
                return Some(title.value.as_str());
            }
        }
        None
    }
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
        // Use the new helper methods
        assert_eq!(v.full_name(), Some("Test User"));
        assert_eq!(v.emails(), vec!["test@example.com"]);
        assert_eq!(v.phones(), vec!["+123456789"]);
        assert_eq!(v.org(), Some(vec!["Example Inc".to_string()]));
        assert_eq!(v.title(), Some("Engineer"));
        // Check UID via property iteration
        let mut found_uid = None;
        for prop in &v.properties {
            if let Property::Uid(uid) = prop {
                found_uid = Some(&uid.value);
                break;
            }
        }
        assert_eq!(found_uid, Some(&"abc".to_string()));
    }

    #[test]
    fn test_vcard_display() {
        let v = Vcard {
            properties: vec![
                Property::Fn(Fn { value: "Test User".to_string() }),
                Property::Uid(Uid { value: "uid123".to_string() }),
                Property::Email(Email { email: "test@example.com".to_string() }),
            ],
        };
        let s = v.to_string();
        assert!(s.contains("BEGIN:VCARD"));
        assert!(s.contains("FN:Test User"));
        assert!(s.contains("EMAIL;type=INTERNET;type=WORK:test@example.com"));
        assert!(s.contains("UID:uid123"));
        assert!(s.contains("END:VCARD"));
    }
}
