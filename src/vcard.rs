// src/vcard.rs
// vCard 3.0 parser/serializer with a lossless-minded property model.
//
// The model captures structured names (N), typed addresses (ADR), typed and
// preference-annotated telephones (TEL), typed e-mails (EMAIL), organisation
// units (ORG), job titles (TITLE), notes (NOTE), birthdays (BDAY), wedding
// anniversaries, nicknames, photos (PHOTO), URLs (URL), FileAs (X-FILEAS),
// departments (X-DEPARTMENT), spouse/children (X-AS-SPOUSE / X-AS-CHILD) and
// UIDs (UID). Unknown properties are preserved verbatim so a round-trip
// through the gateway never silently drops data.
use anyhow::Result;

/// A vCard with a list of properties.
#[derive(Debug, Clone, Default)]
pub struct Vcard {
    pub properties: Vec<Property>,
}

/// vCard property types per RFC 6350 plus gateway extensions.
#[derive(Debug, Clone)]
pub enum Property {
    /// FN (Formatted Name)
    Fn(Fn),
    /// EMAIL
    Email(Email),
    /// TEL (Telephone)
    Tel(Tel),
    /// ORG (Organization; component 0 = company, 1+ = units/departments)
    Org(Org),
    /// TITLE (job title)
    Title(Title),
    /// N (Structured name)
    N(Name),
    /// ADR (structured postal address)
    Adr(Adr),
    /// NOTE (free-form notes)
    Note(String),
    /// BDAY (date of birth, e.g. "19700131" or "1970-01-31")
    Bday(String),
    /// Wedding anniversary (same date formats as BDAY)
    Anniversary(String),
    /// NICKNAME
    Nickname(String),
    /// PHOTO; normalised to a data URI, remote URI, or raw base64 payload
    Photo(String),
    /// URL (web page)
    Url(String),
    /// File-as (display ordering), from FileAs / X-FILEAS
    FileAs(String),
    /// X-DEPARTMENT department override
    Department(String),
    /// Spouse name (EAS Spouse; serialised as X-AS-SPOUSE)
    Spouse(String),
    /// Child name (EAS Child elements; serialised as X-AS-CHILD)
    Child(String),
    /// UID
    Uid(Uid),
    /// Any other, unparsed property preserved verbatim (name;params:value)
    Raw(String),
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
    pub params: Vec<Parameter>,
}

/// TEL property
#[derive(Debug, Clone, Default)]
pub struct Tel {
    pub number: String,
    pub params: Vec<Parameter>,
}

impl Tel {
    pub fn has_type(&self, t: &str) -> bool {
        self.params.iter().any(|p| p.is_type(t))
    }
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

/// N property (structured name): family;given;additional;prefix;suffix
#[derive(Debug, Clone, Default)]
pub struct Name {
    pub family: String,
    pub given: String,
    pub additional: String,
    pub prefix: String,
    pub suffix: String,
}

/// ADR property (structured address): po;ext;street;locality;region;postal;country
#[derive(Debug, Clone, Default)]
pub struct Adr {
    pub po_box: String,
    pub extension: String,
    pub street: String,
    pub locality: String,
    pub region: String,
    pub postal_code: String,
    pub country: String,
    pub params: Vec<Parameter>,
}

impl Adr {
    pub fn has_type(&self, t: &str) -> bool {
        self.params.iter().any(|p| p.is_type(t))
    }

    /// home/work/other classification used by the EAS mapping.
    pub fn context(&self) -> &'static str {
        if self.has_type("home") {
            "home"
        } else if self.has_type("work") {
            "work"
        } else {
            "other"
        }
    }
}

/// UID property
#[derive(Debug, Clone)]
pub struct Uid {
    pub value: String,
}

/// vCard parameter. TYPE labels are preserved verbatim so vendor labels keep
/// working (e.g. Apple X-ABLabel, non-standard tokens).
#[derive(Debug, Clone)]
pub enum Parameter {
    Type(String),
    Pref(u32),
}

impl Parameter {
    pub fn is_type(&self, t: &str) -> bool {
        match self {
            Parameter::Type(v) => v.eq_ignore_ascii_case(t),
            _ => false,
        }
    }
}

impl Email {
    pub fn has_type(&self, t: &str) -> bool {
        self.params.iter().any(|p| p.is_type(t))
    }
}

/// Well-known TYPE labels used by the EAS/JSCard mappings.
#[allow(dead_code)]
pub struct Type;

#[allow(dead_code)]
impl Type {
    pub const WORK: &'static str = "work";
    pub const HOME: &'static str = "home";
    pub const VOICE: &'static str = "voice";
    pub const CELL: &'static str = "cell";
    pub const FAX: &'static str = "fax";
    pub const PAGER: &'static str = "pager";
}

/// Parse a parameter string like `TYPE=work,cell;PREF=1`, or a legacy bare
/// list (`TEL;WORK;VOICE`) into `Parameter`s.
fn parse_params(param_str: &str) -> Vec<Parameter> {
    let mut out = Vec::new();
    if param_str.is_empty() {
        return out;
    }
    for part in param_str.split(';') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        if let Some((name, value)) = part.split_once('=') {
            let name = name.trim().to_ascii_uppercase();
            match name.as_str() {
                "TYPE" => {
                    for t in value.trim_matches('"').split(',') {
                        let t = t.trim();
                        if !t.is_empty() {
                            out.push(Parameter::Type(t.to_string()));
                        }
                    }
                }
                "PREF" => {
                    if let Ok(n) = value.trim_matches('"').parse::<u32>() {
                        out.push(Parameter::Pref(n));
                    }
                }
                _ => {
                    // ENCODING/VALUE/etc. are folded into normalised values by
                    // the property-specific handlers (e.g. PHOTO).
                }
            }
        } else {
            out.push(Parameter::Type(part.to_string()));
        }
    }
    out
}

/// Split `name;params` from `value` at the first colon outside quotes.
fn split_value(line: &str) -> Option<(&str, &str)> {
    let mut in_quotes = false;
    for (idx, ch) in line.char_indices() {
        match ch {
            '"' => in_quotes = !in_quotes,
            ':' if !in_quotes => return Some((&line[..idx], &line[idx + 1..])),
            _ => {}
        }
    }
    None
}

/// Parse a vCard string into a Vcard struct.
pub fn parse_vcard_from_data(data: &str) -> Result<Vcard> {
    let mut out = Vcard::default();

    // RFC 6350 §3.1 folding: CRLF followed by SP/HTAB continues the line.
    let mut logical: Vec<String> = Vec::new();
    let mut cur = String::new();
    for raw in data.lines() {
        let line = raw.trim_end_matches(['\r', '\n']);
        if let Some(rest) = line.strip_prefix(' ').or_else(|| line.strip_prefix('\t')) {
            cur.push_str(rest);
        } else {
            if !cur.is_empty() {
                logical.push(std::mem::take(&mut cur));
            }
            cur = line.to_string();
        }
    }
    if !cur.is_empty() {
        logical.push(cur);
    }

    for line in logical {
        if line.is_empty()
            || line.eq_ignore_ascii_case("BEGIN:VCARD")
            || line.eq_ignore_ascii_case("END:VCARD")
            || line.starts_with("VERSION:")
        {
            continue;
        }
        let Some((name_params, value)) = split_value(&line) else {
            continue;
        };
        let mut parts = name_params.splitn(2, ';');
        let name = parts.next().unwrap_or("").to_ascii_uppercase();
        let params = parse_params(parts.next().unwrap_or(""));

        match name.as_str() {
            "FN" => out.properties.push(Property::Fn(Fn {
                value: unescape_text(value),
            })),
            "EMAIL" => out.properties.push(Property::Email(Email {
                email: unescape_text(value),
                params,
            })),
            "TEL" => out.properties.push(Property::Tel(Tel {
                number: unescape_text(value),
                params,
            })),
            "ORG" => {
                // ORG components are ';'-separated; escapes apply per component.
                let parts: Vec<String> = value.split(';').map(unescape_text).collect();
                if !parts.is_empty() {
                    out.properties.push(Property::Org(Org { value: parts }));
                }
            }
            "TITLE" => out.properties.push(Property::Title(Title {
                value: unescape_text(value),
            })),
            "N" => {
                let comps: Vec<String> = value.split(';').map(unescape_text).collect();
                out.properties.push(Property::N(Name {
                    family: comps.first().cloned().unwrap_or_default(),
                    given: comps.get(1).cloned().unwrap_or_default(),
                    additional: comps.get(2).cloned().unwrap_or_default(),
                    prefix: comps.get(3).cloned().unwrap_or_default(),
                    suffix: comps.get(4).cloned().unwrap_or_default(),
                }));
            }
            "ADR" => {
                let comps: Vec<String> = value.split(';').map(unescape_text).collect();
                out.properties.push(Property::Adr(Adr {
                    po_box: comps.first().cloned().unwrap_or_default(),
                    extension: comps.get(1).cloned().unwrap_or_default(),
                    street: comps.get(2).cloned().unwrap_or_default(),
                    locality: comps.get(3).cloned().unwrap_or_default(),
                    region: comps.get(4).cloned().unwrap_or_default(),
                    postal_code: comps.get(5).cloned().unwrap_or_default(),
                    country: comps.get(6).cloned().unwrap_or_default(),
                    params,
                }));
            }
            "NOTE" => out.properties.push(Property::Note(unescape_text(value))),
            "BDAY" => out
                .properties
                .push(Property::Bday(unescape_text(value).trim().to_string())),
            "ANNIVERSARY" | "X-ANNIVERSARY" => out
                .properties
                .push(Property::Anniversary(unescape_text(value))),
            "NICKNAME" => {
                for n in value.split(',').map(unescape_text) {
                    let n = n.trim();
                    if !n.is_empty() {
                        out.properties.push(Property::Nickname(n.to_string()));
                    }
                }
            }
            "PHOTO" => out.properties.push(Property::Photo(photo_normalise(
                &name_params[name.len()..],
                value,
            ))),
            "URL" => out.properties.push(Property::Url(unescape_text(value))),
            "FILEAS" | "X-FILEAS" => out.properties.push(Property::FileAs(unescape_text(value))),
            "X-DEPARTMENT" | "DEPARTMENT" => out
                .properties
                .push(Property::Department(unescape_text(value))),
            "X-AS-SPOUSE" => out.properties.push(Property::Spouse(unescape_text(value))),
            "X-AS-CHILD" => out.properties.push(Property::Child(unescape_text(value))),
            "UID" => out.properties.push(Property::Uid(Uid {
                value: value.to_string(),
            })),
            _ => {
                // Preserve unknown properties verbatim so no data is dropped.
                out.properties.push(Property::Raw(line.to_string()));
            }
        }
    }
    Ok(out)
}

/// Normalise a PHOTO value to a data URI, remote URI, or raw base64 payload.
fn photo_normalise(params: &str, value: &str) -> String {
    let value = value.trim();
    if value.starts_with("data:") || value.starts_with("http://") || value.starts_with("https://") {
        return value.to_string();
    }
    let upper = params.to_ascii_uppercase();
    if upper.contains("ENCODING=B") || upper.contains("ENCODING=BASE64") || upper.contains("BASE64")
    {
        let compact: String = value.chars().filter(|c| !c.is_whitespace()).collect();
        let mime = if upper.contains("PNG") {
            "image/png"
        } else if upper.contains("GIF") {
            "image/gif"
        } else {
            "image/jpeg"
        };
        return format!("data:{mime};base64,{compact}");
    }
    value.to_string()
}

/// Build a minimal vCard from contact fields. Used for creating contacts.
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
            params: vec![
                Parameter::Type("INTERNET".to_string()),
                Parameter::Type(Type::WORK.to_string()),
            ],
        }));
    }
    if let Some(phone_str) = phone {
        vcard.properties.push(Property::Tel(Tel {
            number: phone_str.to_string(),
            params: vec![
                Parameter::Type(Type::WORK.to_string()),
                Parameter::Type(Type::VOICE.to_string()),
            ],
        }));
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

fn params_to_string(params: &[Parameter]) -> String {
    if params.is_empty() {
        return String::new();
    }
    let mut types: Vec<&str> = Vec::new();
    let mut pref = None;
    for p in params {
        match p {
            Parameter::Type(t) => types.push(t.as_str()),
            Parameter::Pref(n) => pref = Some(*n),
        }
    }
    let mut parts: Vec<String> = Vec::new();
    if !types.is_empty() {
        parts.push(types.join(",").to_uppercase());
    }
    let mut out = String::new();
    for t in parts {
        out.push_str(&format!(";TYPE={}", t));
    }
    if let Some(n) = pref {
        out.push_str(&format!(";PREF={}", n));
    }
    out
}

fn write_line(f: &mut std::fmt::Formatter<'_>, line: &str) -> std::fmt::Result {
    // RFC 2426 §2.6 / RFC 6350 §3.2: content lines longer than 75 octets
    // SHOULD be folded with CRLF + a single space; a multi-byte UTF-8
    // character must never be split across the fold boundary.
    if line.len() <= 75 {
        return f.write_str(&format!("{}\r\n", line));
    }
    let mut rest = line;
    // First physical line: 75 octets; continuations carry 74 octets of
    // content (plus the leading space).
    let mut budget = 75;
    while !rest.is_empty() {
        let take = budget.min(rest.len());
        let cut = rest.floor_char_boundary(take);
        let (head, tail) = rest.split_at(if cut == 0 { rest.len() } else { cut });
        if cut == 0 {
            return f.write_str(&format!("{}\r\n", rest));
        }
        if tail.is_empty() {
            f.write_str(&format!("{}\r\n", head))?;
            break;
        }
        f.write_str(&format!("{}\r\n ", head))?;
        rest = tail;
        budget = 74;
    }
    Ok(())
}

impl std::fmt::Display for Vcard {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("BEGIN:VCARD\r\nVERSION:3.0\r\n")?;
        for prop in &self.properties {
            match prop {
                Property::Fn(fn_val) => {
                    write_line(f, &format!("FN:{}", escape_text(&fn_val.value)))?;
                }
                Property::Email(email) => {
                    write_line(
                        f,
                        &format!(
                            "EMAIL{}:{}",
                            params_to_string(&email.params),
                            escape_text(&email.email)
                        ),
                    )?;
                }
                Property::Tel(tel) => {
                    write_line(
                        f,
                        &format!(
                            "TEL{}:{}",
                            params_to_string(&tel.params),
                            escape_text(&tel.number)
                        ),
                    )?;
                }
                Property::Org(org) => {
                    // ';' is the structural ORG component delimiter: escape
                    // each component, then join with unescaped ';'.
                    let escaped: Vec<String> = org.value.iter().map(|p| escape_text(p)).collect();
                    write_line(f, &format!("ORG:{}", escaped.join(";")))?;
                }
                Property::Title(title) => {
                    write_line(f, &format!("TITLE:{}", escape_text(&title.value)))?;
                }
                Property::N(name) => {
                    write_line(
                        f,
                        &format!(
                            "N:{};{};{};{};{}",
                            escape_text(&name.family),
                            escape_text(&name.given),
                            escape_text(&name.additional),
                            escape_text(&name.prefix),
                            escape_text(&name.suffix)
                        ),
                    )?;
                }
                Property::Adr(adr) => {
                    write_line(
                        f,
                        &format!(
                            "ADR{}:{};{};{};{};{};{};{}",
                            params_to_string(&adr.params),
                            escape_text(&adr.po_box),
                            escape_text(&adr.extension),
                            escape_text(&adr.street),
                            escape_text(&adr.locality),
                            escape_text(&adr.region),
                            escape_text(&adr.postal_code),
                            escape_text(&adr.country)
                        ),
                    )?;
                }
                Property::Note(note) => {
                    write_line(f, &format!("NOTE:{}", escape_text(note)))?;
                }
                Property::Bday(b) => {
                    write_line(f, &format!("BDAY:{}", b.trim()))?;
                }
                Property::Anniversary(a) => {
                    write_line(f, &format!("ANNIVERSARY:{}", escape_text(a)))?;
                }
                Property::Nickname(n) => {
                    write_line(f, &format!("NICKNAME:{}", escape_text(n)))?;
                }
                Property::Photo(p) => {
                    if is_uri(p) {
                        write_line(f, &format!("PHOTO;VALUE=URI:{p}"))?;
                    } else {
                        write_line(f, &format!("PHOTO;ENCODING=b;TYPE=JPEG:{p}"))?;
                    }
                }
                Property::Url(u) => {
                    write_line(f, &format!("URL:{}", escape_text(u)))?;
                }
                Property::FileAs(v) => {
                    write_line(f, &format!("X-FILEAS:{}", escape_text(v)))?;
                }
                Property::Department(d) => {
                    write_line(f, &format!("X-DEPARTMENT:{}", escape_text(d)))?;
                }
                Property::Spouse(s) => {
                    write_line(f, &format!("X-AS-SPOUSE:{}", escape_text(s)))?;
                }
                Property::Child(c) => {
                    write_line(f, &format!("X-AS-CHILD:{}", escape_text(c)))?;
                }
                Property::Uid(uid) => {
                    write_line(f, &format!("UID:{}", uid.value))?;
                }
                Property::Raw(line) => {
                    write_line(f, line)?;
                }
            }
        }
        f.write_str("END:VCARD")?;
        Ok(())
    }
}

fn is_uri(p: &str) -> bool {
    p.starts_with("data:") || p.starts_with("http://") || p.starts_with("https://")
}

impl Vcard {
    /// Extract full name from FN property.
    pub fn full_name(&self) -> Option<&str> {
        for prop in &self.properties {
            if let Property::Fn(fn_val) = prop {
                return Some(fn_val.value.as_str());
            }
        }
        None
    }

    /// Get email addresses (values only).
    pub fn emails(&self) -> Vec<&str> {
        self.properties
            .iter()
            .filter_map(|p| match p {
                Property::Email(e) if !e.email.is_empty() => Some(e.email.as_str()),
                _ => None,
            })
            .collect()
    }

    /// Get typed e-mails.
    pub fn typed_emails(&self) -> Vec<&Email> {
        self.properties
            .iter()
            .filter_map(|p| match p {
                Property::Email(e) if !e.email.is_empty() => Some(e),
                _ => None,
            })
            .collect()
    }

    /// Get phone numbers (values only).
    pub fn phones(&self) -> Vec<&str> {
        self.properties
            .iter()
            .filter_map(|p| match p {
                Property::Tel(t) if !t.number.is_empty() => Some(t.number.as_str()),
                _ => None,
            })
            .collect()
    }

    /// Get typed phones.
    pub fn typed_phones(&self) -> Vec<&Tel> {
        self.properties
            .iter()
            .filter_map(|p| match p {
                Property::Tel(t) if !t.number.is_empty() => Some(t),
                _ => None,
            })
            .collect()
    }

    /// Get structured addresses.
    pub fn addresses(&self) -> Vec<&Adr> {
        self.properties
            .iter()
            .filter_map(|p| match p {
                Property::Adr(a) => Some(a),
                _ => None,
            })
            .collect()
    }

    /// Get organization components (0 = company, 1+ = department units).
    pub fn org(&self) -> Option<Vec<String>> {
        for prop in &self.properties {
            if let Property::Org(org) = prop {
                return Some(org.value.clone());
            }
        }
        None
    }

    /// Get job title.
    pub fn title(&self) -> Option<&str> {
        for prop in &self.properties {
            if let Property::Title(title) = prop {
                return Some(title.value.as_str());
            }
        }
        None
    }

    /// Legacy accessor: the family name component of N (legacy single-name
    /// behaviour for callers that just want "a" name).
    pub fn name(&self) -> Option<&str> {
        for prop in &self.properties {
            if let Property::N(name) = prop {
                if !name.family.is_empty() {
                    return Some(name.family.as_str());
                }
                if !name.given.is_empty() {
                    return Some(name.given.as_str());
                }
                return Some("");
            }
        }
        None
    }

    /// Get full structured name.
    pub fn structured_name(&self) -> Option<&Name> {
        for prop in &self.properties {
            if let Property::N(name) = prop {
                return Some(name);
            }
        }
        None
    }

    /// Get note text.
    pub fn note(&self) -> Option<&str> {
        for prop in &self.properties {
            if let Property::Note(n) = prop {
                return Some(n.as_str());
            }
        }
        None
    }

    /// Get birthday (raw date string).
    pub fn bday(&self) -> Option<&str> {
        for prop in &self.properties {
            if let Property::Bday(b) = prop {
                return Some(b.as_str());
            }
        }
        None
    }

    /// Get wedding anniversary (raw date string).
    pub fn anniversary(&self) -> Option<&str> {
        for prop in &self.properties {
            if let Property::Anniversary(a) = prop {
                return Some(a.as_str());
            }
        }
        None
    }

    /// Get nicknames.
    pub fn nicknames(&self) -> Vec<&str> {
        self.properties
            .iter()
            .filter_map(|p| match p {
                Property::Nickname(n) => Some(n.as_str()),
                _ => None,
            })
            .collect()
    }

    /// Get photo (data URI, remote URI, or raw base64 payload).
    pub fn photo(&self) -> Option<&str> {
        for prop in &self.properties {
            if let Property::Photo(p) = prop {
                return Some(p.as_str());
            }
        }
        None
    }

    /// Get web page URL.
    pub fn url(&self) -> Option<&str> {
        for prop in &self.properties {
            if let Property::Url(u) = prop {
                return Some(u.as_str());
            }
        }
        None
    }

    /// Get FileAs.
    pub fn file_as(&self) -> Option<&str> {
        for prop in &self.properties {
            if let Property::FileAs(v) = prop {
                return Some(v.as_str());
            }
        }
        None
    }

    /// Get department (explicit X-DEPARTMENT, else second ORG component).
    pub fn department(&self) -> Option<String> {
        for prop in &self.properties {
            if let Property::Department(d) = prop
                && !d.is_empty()
            {
                return Some(d.clone());
            }
        }
        self.org()
            .and_then(|o| o.get(1).cloned())
            .filter(|s| !s.is_empty())
    }

    /// Get spouse.
    pub fn spouse(&self) -> Option<&str> {
        for prop in &self.properties {
            if let Property::Spouse(s) = prop {
                return Some(s.as_str());
            }
        }
        None
    }

    /// Get children.
    pub fn children(&self) -> Vec<&str> {
        self.properties
            .iter()
            .filter_map(|p| match p {
                Property::Child(c) => Some(c.as_str()),
                _ => None,
            })
            .collect()
    }
}

/// Escape text for vCard per RFC 6350.
pub(crate) fn escape_text(s: &str) -> String {
    let mut out = String::new();
    for ch in s.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            ';' => out.push_str("\\;"),
            ',' => out.push_str("\\,"),
            '\n' => out.push_str("\\n"),
            '\r' => {}
            _ => out.push(ch),
        }
    }
    out
}

/// Unescape vCard text.
pub(crate) fn unescape_text(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut iter = s.chars();
    while let Some(c) = iter.next() {
        if c == '\\' {
            match iter.next() {
                Some('n') | Some('N') => out.push('\n'),
                Some('\\') => out.push('\\'),
                Some(',') => out.push(','),
                Some(';') => out.push(';'),
                Some(other) => {
                    out.push('\\');
                    out.push(other);
                }
                None => out.push('\\'),
            }
        } else {
            out.push(c);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_long_lines_fold_and_unfold_utf8_safely() {
        let mut v = Vcard::default();
        // >75 bytes worth of multi-byte characters.
        let note = "é".repeat(60);
        v.properties.push(Property::Note(note.clone()));
        let out = v.to_string();
        // Every physical line must be <= 75 octets (continuation lines have
        // one leading space + <=74 content bytes).
        for line in out.split("\r\n") {
            assert!(line.len() <= 75, "line too long: {} bytes", line.len());
        }
        assert!(out.contains("\r\n "), "expected a folded continuation");
        let back = parse_vcard_from_data(&out).unwrap();
        assert_eq!(back.note(), Some(note.as_str()));
    }

    #[test]
    fn test_long_photo_folds() {
        let mut v = Vcard::default();
        let b64 = "AQID".repeat(40); // 160 bytes
        v.properties.push(Property::Photo(b64.clone()));
        let out = v.to_string();
        for line in out.split("\r\n") {
            assert!(line.len() <= 75);
        }
        let back = parse_vcard_from_data(&out).unwrap();
        // The parser normalizes inline photos to data URIs; the payload must
        // survive the fold/unfold intact.
        let photo = back.photo().unwrap();
        assert!(photo.strip_prefix("data:image/jpeg;base64,") == Some(b64.as_str()));
    }

    #[test]
    fn test_structured_name_full_round_trip() {
        let v = "BEGIN:VCARD\r\nVERSION:3.0\r\nN:Smith;John;Quincy;Dr.;Jr.\r\nFN:Dr. John Quincy Smith Jr.\r\nEND:VCARD\r\n";
        let parsed = parse_vcard_from_data(v).unwrap();
        let name = parsed.structured_name().unwrap();
        assert_eq!(name.family, "Smith");
        assert_eq!(name.given, "John");
        assert_eq!(name.additional, "Quincy");
        assert_eq!(name.prefix, "Dr.");
        assert_eq!(name.suffix, "Jr.");
        // No mangling: FirstName must not collapse multi-part names.
        let out = parsed.to_string();
        let parsed2 = parse_vcard_from_data(&out).unwrap();
        let name2 = parsed2.structured_name().unwrap();
        assert_eq!(name2.prefix, "Dr.");
        assert_eq!(name2.suffix, "Jr.");
    }

    #[test]
    fn test_typed_addresses_phones_emails_round_trip() {
        let v = "BEGIN:VCARD\r\nVERSION:3.0\r\nFN:X\r\nADR;TYPE=HOME:;;1 Main St;Springfield;IL;62701;USA\r\nADR;TYPE=WORK;PREF=1:;;2 Corp Ave;Metropolis;NY;10001;USA\r\nTEL;TYPE=cell:+1 555-1234\r\nTEL;TYPE=fax,work:+1 555-9999\r\nTEL;TYPE=pager:+1 555-7777\r\nEMAIL;TYPE=work;PREF=1:a@example.com\r\nEMAIL;TYPE=home:b@example.com\r\nEND:VCARD\r\n";
        let parsed = parse_vcard_from_data(v).unwrap();
        let adrs = parsed.addresses();
        assert_eq!(adrs.len(), 2);
        assert_eq!(adrs[0].street, "1 Main St");
        assert_eq!(adrs[0].context(), "home");
        assert_eq!(adrs[1].locality, "Metropolis");
        assert_eq!(adrs[1].context(), "work");
        let tels = parsed.typed_phones();
        assert!(tels[0].has_type("cell"));
        assert!(tels[1].has_type("fax") && tels[1].has_type("work"));
        assert!(tels[2].has_type("pager"));
        assert_eq!(parsed.typed_emails().len(), 2);
        let out = parsed.to_string();
        let round = parse_vcard_from_data(&out).unwrap();
        assert_eq!(round.addresses().len(), 2);
        assert_eq!(round.typed_phones().len(), 3);
        assert!(round.typed_phones()[0].has_type("CELL"));
        assert_eq!(round.typed_emails()[1].email, "b@example.com");
    }

    #[test]
    fn test_notes_bday_photo_url_misc_round_trip() {
        let v = "BEGIN:VCARD\r\nVERSION:3.0\r\nFN:X\r\nNOTE:Line1\\nLine2\r\nBDAY:19700131\r\nANNIVERSARY:19950604\r\nPHOTO;ENCODING=b;TYPE=JPEG:QUJD\r\nURL:https://x.example\r\nNICKNAME:Bob,Bobby\r\nX-FILEAS:Doe\\, Jane\r\nX-DEPARTMENT:R&D\r\nX-AS-SPOUSE:Alice\r\nX-AS-CHILD:Chris\r\nX-AS-CHILD:Pat\r\nEND:VCARD\r\n";
        let parsed = parse_vcard_from_data(v).unwrap();
        assert_eq!(parsed.note(), Some("Line1\nLine2"));
        assert_eq!(parsed.bday(), Some("19700131"));
        assert_eq!(parsed.anniversary(), Some("19950604"));
        assert!(
            parsed
                .photo()
                .unwrap()
                .starts_with("data:image/jpeg;base64,QUJD")
        );
        assert_eq!(parsed.url(), Some("https://x.example"));
        assert_eq!(parsed.nicknames(), vec!["Bob", "Bobby"]);
        assert_eq!(parsed.file_as(), Some("Doe, Jane"));
        assert_eq!(parsed.department().as_deref(), Some("R&D"));
        assert_eq!(parsed.spouse(), Some("Alice"));
        assert_eq!(parsed.children(), vec!["Chris", "Pat"]);
        let out = parsed.to_string();
        let round = parse_vcard_from_data(&out).unwrap();
        assert_eq!(round.note(), Some("Line1\nLine2"));
        assert_eq!(round.children(), vec!["Chris", "Pat"]);
        assert_eq!(round.spouse(), Some("Alice"));
    }

    #[test]
    fn test_unknown_property_preserved() {
        let v = "BEGIN:VCARD\r\nVERSION:3.0\r\nFN:X\r\nX-CUSTOM-THING:something\r\nX-ABLABEL:WorkMail\r\nEND:VCARD\r\n";
        let parsed = parse_vcard_from_data(v).unwrap();
        let out = parsed.to_string();
        assert!(out.contains("X-CUSTOM-THING:something"));
        assert!(out.contains("X-ABLABEL:WorkMail"));
    }

    #[test]
    fn test_folded_line_unfolding() {
        let v = "BEGIN:VCARD\r\nVERSION:3.0\r\nFN:Some Very Long Name That Is Folded A\r\n  Cross Lines Here\r\nEND:VCARD\r\n";
        let parsed = parse_vcard_from_data(v).unwrap();
        assert_eq!(
            parsed.full_name(),
            Some("Some Very Long Name That Is Folded A Cross Lines Here")
        );
    }
}
