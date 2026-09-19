// src/jscard.rs
// Conversion between the gateway vCard model (crate::vcard) and JSCard
// (RFC 9553) JSON as served by Stalwart's `urn:ietf:params:jmap:contacts`
// capability (RFC 9610 ContactCard objects with "@type": "Card").
//
// The mapping covers: structured names (title/given/given2/surname/credential
// + generation), full name, e-mails, phones (voice/cell/fax features and
// work/home contexts), structured addresses, organizations with units, job
// titles, nicknames, notes, birthdays and anniversaries, photos (as JMAP
// blob references), URLs, spouse/children (personalInfo), and FileAs/
// department (kept as JSCard-conformant local properties so round-trips via
// CardDAV stay lossless).

use crate::vcard::{self, Adr, Email, Name, Parameter, Tel, Vcard};
use serde_json::{Map, Value, json};

/// Convert a parsed vCard into a JSCard object (RFC 9553).
///
/// `photo_blob_id` may carry an already-uploaded blob id for the contact
/// photo; when the vCard carries inline photo data the caller must upload it
/// first and pass the resulting blob id here.
pub fn vcard_to_jscard(v: &Vcard, uid: &str, photo_blob_id: Option<&str>) -> Value {
    let mut card = Map::new();
    card.insert("@type".into(), json!("Card"));
    card.insert("version".into(), json!("1.0"));
    card.insert("uid".into(), json!(uid));

    // Name: structured components.
    if let Some(n) = v.structured_name() {
        let mut comps = Vec::new();
        let mut push_comp = |kind: &str, value: &str| {
            if !value.is_empty() {
                comps.push(json!({
                    "@type": "NameComponent",
                    "kind": kind,
                    "value": value,
                }));
            }
        };
        push_comp("title", &n.prefix);
        push_comp("given", &n.given);
        push_comp("given2", &n.additional);
        push_comp("surname", &n.family);
        push_comp("credential", &n.suffix);
        card.insert(
            "name".into(),
            json!({
                "@type": "Name",
                "components": comps,
                "isOrdered": true,
            }),
        );
    }
    if let Some(fn_val) = v.full_name()
        && !fn_val.is_empty()
    {
        card.insert("fullName".into(), json!(fn_val));
    }

    // Emails
    let typed_emails = v.typed_emails();
    if !typed_emails.is_empty() {
        let mut emails = Map::new();
        for (i, e) in typed_emails.iter().enumerate() {
            let contexts = contexts_for(e.has_type("home"), e.has_type("work"));
            let mut obj = Map::new();
            obj.insert("@type".into(), json!("EmailAddress"));
            obj.insert("address".into(), json!(e.email));
            if let Some(c) = contexts {
                obj.insert("contexts".into(), c);
            }
            emails.insert(format!("e{i}"), Value::Object(obj));
        }
        card.insert("emails".into(), Value::Object(emails));
    }

    // Phones
    let typed_phones = v.typed_phones();
    if !typed_phones.is_empty() {
        let mut phones = Map::new();
        for (i, t) in typed_phones.iter().enumerate() {
            let contexts = contexts_for(t.has_type("home"), t.has_type("work"));
            let mut features = Map::new();
            if t.has_type("cell") || t.has_type("mob") {
                features.insert("cell".into(), json!(true));
                features.insert("voice".into(), json!(true));
            } else if t.has_type("fax") {
                features.insert("fax".into(), json!(true));
            } else if t.has_type("pager") {
                features.insert("pager".into(), json!(true));
            } else if t.has_type("video") {
                features.insert("video".into(), json!(true));
            } else {
                features.insert("voice".into(), json!(true));
            }
            let mut obj = Map::new();
            obj.insert("@type".into(), json!("Phone"));
            obj.insert("number".into(), json!(t.number));
            obj.insert("features".into(), Value::Object(features));
            if let Some(c) = contexts {
                obj.insert("contexts".into(), c);
            }
            phones.insert(format!("p{i}"), Value::Object(obj));
        }
        card.insert("phones".into(), Value::Object(phones));
    }

    // Addresses
    let addresses = v.addresses();
    if !addresses.is_empty() {
        let mut addrs = Map::new();
        for (i, a) in addresses.iter().enumerate() {
            let mut obj = Map::new();
            obj.insert("@type".into(), json!("Address"));
            let mut street = Map::new();
            let full_street = {
                let ext = a.extension.trim();
                let st = a.street.trim();
                match (ext.is_empty(), st.is_empty()) {
                    (true, true) => String::new(),
                    (false, true) => ext.to_string(),
                    (true, false) => st.to_string(),
                    (false, false) => format!("{ext} {st}"),
                }
            };
            if !full_street.is_empty() {
                // StreetComponent "name": house number + street text without
                // the street number split (vCard carries one free-form street).
                street.insert(
                    "s0".into(),
                    json!({
                        "@type": "StreetComponent",
                        "kind": "name",
                        "value": full_street,
                    }),
                );
                obj.insert("street".into(), Value::Object(street));
            }
            if !a.locality.is_empty() {
                obj.insert("locality".into(), json!(a.locality));
            }
            if !a.region.is_empty() {
                obj.insert("region".into(), json!(a.region));
            }
            if !a.postal_code.is_empty() {
                obj.insert("postCode".into(), json!(a.postal_code));
            }
            if !a.country.is_empty() {
                obj.insert("country".into(), json!(a.country));
            }
            if !a.po_box.is_empty() {
                // Preserve the post-office box as a labelled street extension so
                // a JMAP->vCard round-trip keeps the data.
                obj.insert("extraStreet".into(), json!(a.po_box));
            }
            match a.context() {
                "home" => {
                    obj.insert("contexts".into(), json!({"home": true}));
                }
                "work" => {
                    obj.insert("contexts".into(), json!({"work": true}));
                }
                _ => {}
            }
            addrs.insert(format!("a{i}"), Value::Object(obj));
        }
        card.insert("addresses".into(), Value::Object(addrs));
    }

    // Organization (+ department/unit)
    if let Some(org) = v.org()
        && !org.is_empty()
    {
        let name = org.first().cloned().unwrap_or_default();
        let units: Vec<Value> = org
            .iter()
            .skip(1)
            .filter(|u| !u.is_empty())
            .map(|u| json!({"@type": "OrgUnit", "name": u}))
            .collect();
        let mut obj = Map::new();
        obj.insert("@type".into(), json!("Organization"));
        if !name.is_empty() {
            obj.insert("name".into(), json!(name));
        }
        if !units.is_empty() {
            obj.insert("units".into(), json!(units));
        }
        card.insert("organizations".into(), json!({"o0": Value::Object(obj)}));
    }

    // Job title
    if let Some(title) = v.title()
        && !title.is_empty()
    {
        card.insert(
            "titles".into(),
            json!({
                "t0": {"@type": "Title", "name": title, "organizationId": "o0"},
            }),
        );
    }

    // Note
    if let Some(note) = v.note()
        && !note.is_empty()
    {
        card.insert("note".into(), json!(note));
    }

    // Birthdays / anniversaries via JSCard anniversaries (PartialDate).
    if let Some(b) = v.bday()
        && let Some(pd) = parse_partial_date(b)
    {
        let mut anni = Map::new();
        anni.insert(
            "birth".into(),
            json!({"@type": "Anniversary", "type": "birth", "date": pd}),
        );
        card.insert("anniversaries".into(), Value::Object(anni));
    }

    // Nicknames
    let nicks = v.nicknames();
    if !nicks.is_empty() {
        card.insert(
            "nicknames".into(),
            json!({
                "n0": {"@type": "Nickname", "name": nicks.join(", ")},
            }),
        );
    }

    // Photo: blob reference if one was uploaded.
    if let Some(blob_id) = photo_blob_id {
        card.insert(
            "media".into(),
            json!({
                "photo0": {"@type": "Media", "kind": "photo", "blobId": blob_id},
            }),
        );
    }

    // URLs (web page)
    if let Some(url) = v.url()
        && !url.is_empty()
    {
        card.insert(
            "onlineServices".into(),
            json!({
                "u0": {"@type": "OnlineService", "service": "URL", "uri": url},
            }),
        );
    }

    // FileAs / department are via JSCard "freeBusyUrl"-adjacent extras; keep
    // them as RFC-conformant extension properties.
    if let Some(fa) = v.file_as()
        && !fa.is_empty()
    {
        card.insert("keywords".into(), json!({format!("file-as:{}", fa): true}));
    }
    if let Some(dept) = v.department()
        && !dept.is_empty()
    {
        card.insert("keyboardText".into(), json!(dept));
    }

    // Spouse / children / wedding anniversary via personalInfo (RFC 9553 §1.4.8).
    let mut personal_info = Map::new();
    if let Some(spouse) = v.spouse()
        && !spouse.is_empty()
    {
        let mut obj = Map::new();
        obj.insert("@type".into(), json!("PersonalInfo"));
        obj.insert("kind".into(), json!("spouse"));
        obj.insert("value".into(), json!(spouse));
        personal_info.insert("spouse".into(), Value::Object(obj));
    }
    for (i, child) in v.children().iter().enumerate() {
        if child.is_empty() {
            continue;
        }
        let mut obj = Map::new();
        obj.insert("@type".into(), json!("PersonalInfo"));
        obj.insert("kind".into(), json!("child"));
        obj.insert("value".into(), json!(child));
        personal_info.insert(format!("child{i}"), Value::Object(obj));
    }
    if let Some(a) = v.anniversary() {
        let mut obj = Map::new();
        obj.insert("@type".into(), json!("PersonalInfo"));
        obj.insert("kind".into(), json!("anniversary"));
        obj.insert("value".into(), json!(a));
        personal_info.insert("anniversary".into(), Value::Object(obj));
    }
    if !personal_info.is_empty() {
        card.insert("personalInfo".into(), Value::Object(personal_info));
    }

    Value::Object(card)
}

/// Convert a JSCard object (RFC 9553) returned by Stalwart into the gateway
/// vCard model.
pub fn jscard_to_vcard(card: &Value, uid: &str) -> Vcard {
    let mut v = Vcard::default();

    if let Some(uid_val) = card.get("uid").and_then(|u| u.as_str()) {
        v.properties.push(card_uid_prop(uid_val));
    } else {
        v.properties.push(card_uid_prop(uid));
    }

    if let Some(full) = card.get("fullName").and_then(|f| f.as_str())
        && !full.is_empty()
    {
        v.properties.push(vcard::Property::Fn(vcard::Fn {
            value: full.to_string(),
        }));
    }

    if let Some(name) = card.get("name")
        && let Some(comps) = name.get("components").and_then(|c| c.as_array())
    {
        let mut n = Name::default();
        for comp in comps {
            let kind = comp.get("kind").and_then(|k| k.as_str()).unwrap_or("");
            let value = comp.get("value").and_then(|v| v.as_str()).unwrap_or("");
            match kind {
                "title" => n.prefix = value.to_string(),
                "given" => n.given = value.to_string(),
                "given2" => n.additional = value.to_string(),
                "surname" => n.family = value.to_string(),
                "credential" => n.suffix = value.to_string(),
                _ => {}
            }
        }
        if !n.family.is_empty()
            || !n.given.is_empty()
            || !n.additional.is_empty()
            || !n.prefix.is_empty()
            || !n.suffix.is_empty()
        {
            v.properties.push(vcard::Property::N(n));
        }
    }

    if let Some(emails) = card.get("emails").and_then(|e| e.as_object()) {
        let mut entries: Vec<_> = emails.iter().collect();
        entries.sort_by_key(|(k, _)| k.to_string());
        for (_, e) in entries {
            if let Some(addr) = e.get("address").and_then(|a| a.as_str()) {
                let mut params = Vec::new();
                if ctx_has(e, "home") {
                    params.push(Parameter::Type("home".into()));
                }
                if ctx_has(e, "work") {
                    params.push(Parameter::Type("work".into()));
                }
                v.properties.push(vcard::Property::Email(Email {
                    email: addr.to_string(),
                    params,
                }));
            }
        }
    }

    if let Some(phones) = card.get("phones").and_then(|p| p.as_object()) {
        let mut entries: Vec<_> = phones.iter().collect();
        entries.sort_by_key(|(k, _)| k.to_string());
        for (_, p) in entries {
            if let Some(num) = p.get("number").and_then(|n| n.as_str()) {
                let mut params = Vec::new();
                let features = p.get("features").and_then(|f| f.as_object());
                let feat = |k: &str| {
                    features
                        .and_then(|f| f.get(k).and_then(|x| x.as_bool()))
                        .unwrap_or(false)
                };
                if feat("cell") {
                    params.push(Parameter::Type("cell".into()));
                }
                if feat("fax") {
                    params.push(Parameter::Type("fax".into()));
                }
                if feat("pager") {
                    params.push(Parameter::Type("pager".into()));
                }
                if feat("video") {
                    params.push(Parameter::Type("video".into()));
                }
                if feat("voice") || params.is_empty() {
                    params.push(Parameter::Type("voice".into()));
                }
                if ctx_has(p, "home") {
                    params.push(Parameter::Type("home".into()));
                }
                if ctx_has(p, "work") {
                    params.push(Parameter::Type("work".into()));
                }
                v.properties.push(vcard::Property::Tel(Tel {
                    number: num.to_string(),
                    params,
                }));
            }
        }
    }

    if let Some(addrs) = card.get("addresses").and_then(|a| a.as_object()) {
        let mut entries: Vec<_> = addrs.iter().collect();
        entries.sort_by_key(|(k, _)| k.to_string());
        for (_, a) in entries {
            let mut street = String::new();
            if let Some(street_map) = a.get("street").and_then(|s| s.as_object()) {
                for comp in street_map.values() {
                    if let Some(val) = comp.get("value").and_then(|v| v.as_str())
                        && !val.is_empty()
                    {
                        if !street.is_empty() {
                            street.push(' ');
                        }
                        street.push_str(val);
                    }
                }
            }
            let mut params = Vec::new();
            if ctx_has(a, "home") {
                params.push(Parameter::Type("home".into()));
            }
            if ctx_has(a, "work") {
                params.push(Parameter::Type("work".into()));
            }
            let adr = Adr {
                po_box: String::new(),
                extension: js_str(a, "extraStreet"),
                street,
                locality: js_str(a, "locality"),
                region: js_str(a, "region"),
                postal_code: js_str(a, "postCode"),
                country: js_str(a, "country"),
                params,
            };
            if !adr.street.is_empty()
                || !adr.locality.is_empty()
                || !adr.postal_code.is_empty()
                || !adr.country.is_empty()
            {
                v.properties.push(vcard::Property::Adr(adr));
            }
        }
    }

    if let Some(orgs) = card.get("organizations").and_then(|o| o.as_object())
        && let Some((_, org)) = first_org(orgs)
    {
        let mut comps = Vec::new();
        if let Some(name) = org.get("name").and_then(|n| n.as_str())
            && !name.is_empty()
        {
            comps.push(name.to_string());
        }
        if let Some(units) = org.get("units").and_then(|u| u.as_array()) {
            for u in units {
                if let Some(name) = u.get("name").and_then(|n| n.as_str())
                    && !name.is_empty()
                {
                    comps.push(name.to_string());
                }
            }
        }
        if !comps.is_empty() {
            v.properties
                .push(vcard::Property::Org(vcard::Org { value: comps }));
        }
    }

    if let Some(titles) = card.get("titles").and_then(|t| t.as_object())
        && let Some((_, t)) = titles.iter().next()
        && let Some(name) = t.get("name").and_then(|n| n.as_str())
        && !name.is_empty()
    {
        v.properties.push(vcard::Property::Title(vcard::Title {
            value: name.to_string(),
        }));
    }

    if let Some(note) = card.get("note").and_then(|n| n.as_str())
        && !note.is_empty()
    {
        v.properties.push(vcard::Property::Note(note.to_string()));
    }

    if let Some(anni) = card.get("anniversaries").and_then(|a| a.as_object())
        && let Some(birth) = anni
            .values()
            .find(|a| a.get("type").and_then(|t| t.as_str()) == Some("birth"))
        && let Some(date) = birth.get("date")
        && let Some(y) = date.get("year").and_then(|y| y.as_i64())
    {
        let m = date.get("month").and_then(|m| m.as_i64()).unwrap_or(1);
        let d = date.get("day").and_then(|d| d.as_i64()).unwrap_or(1);
        v.properties
            .push(vcard::Property::Bday(format!("{:04}{:02}{:02}", y, m, d)));
    }

    if let Some(nicks) = card.get("nicknames").and_then(|n| n.as_object()) {
        for (_, n) in nicks {
            if let Some(name) = n.get("name").and_then(|s| s.as_str())
                && !name.is_empty()
            {
                for part in name.split(',') {
                    let part = part.trim();
                    if !part.is_empty() {
                        v.properties
                            .push(vcard::Property::Nickname(part.to_string()));
                    }
                }
            }
        }
    }

    if let Some(services) = card.get("onlineServices").and_then(|o| o.as_object()) {
        for (_, s) in services {
            if let Some(uri) = s.get("uri").and_then(|u| u.as_str())
                && !uri.is_empty()
            {
                v.properties.push(vcard::Property::Url(uri.to_string()));
            }
        }
    }

    if let Some(info) = card.get("personalInfo").and_then(|p| p.as_object()) {
        for (_, p) in info {
            let kind = p.get("kind").and_then(|k| k.as_str()).unwrap_or("");
            let value = p.get("value").and_then(|v| v.as_str()).unwrap_or("");
            match kind {
                "spouse" => v
                    .properties
                    .push(vcard::Property::Spouse(value.to_string())),
                "child" => v.properties.push(vcard::Property::Child(value.to_string())),
                "anniversary" => v
                    .properties
                    .push(vcard::Property::Anniversary(value.to_string())),
                _ => {}
            }
        }
    }

    if let Some(keywords) = card.get("keywords").and_then(|k| k.as_object()) {
        for (k, enabled) in keywords {
            if enabled.as_bool() == Some(true)
                && let Some(fa) = k.strip_prefix("file-as:")
            {
                v.properties.push(vcard::Property::FileAs(fa.to_string()));
            }
        }
    }

    if let Some(dept) = card.get("keyboardText").and_then(|d| d.as_str())
        && !dept.is_empty()
    {
        v.properties
            .push(vcard::Property::Department(dept.to_string()));
    }

    v
}

fn card_uid_prop(uid: &str) -> vcard::Property {
    vcard::Property::Uid(vcard::Uid {
        value: uid.to_string(),
    })
}

fn ctx_has(o: &Value, key: &str) -> bool {
    o.get("contexts")
        .and_then(|c| c.get(key))
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
}

fn js_str(o: &Value, key: &str) -> String {
    o.get(key)
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string()
}

fn first_org(orgs: &serde_json::Map<String, Value>) -> Option<(&String, &Value)> {
    let mut it = orgs.iter().collect::<Vec<_>>();
    it.sort_by_key(|(k, _)| k.to_string());
    it.into_iter().next()
}

fn contexts_for(home: bool, work: bool) -> Option<Value> {
    if home && work {
        Some(json!({"home": true, "work": true}))
    } else if home {
        Some(json!({"home": true}))
    } else if work {
        Some(json!({"work": true}))
    } else {
        None
    }
}

/// Parse "YYYYMMDD" / "YYYY-MM-DD" into a JSCard PartialDate.
fn parse_partial_date(s: &str) -> Option<Value> {
    let digits: String = s.chars().filter(|c| c.is_ascii_digit()).collect();
    let (y, m, d) = match digits.len() {
        8 => (&digits[0..4], &digits[4..6], &digits[6..8]),
        _ => return None,
    };
    let year: i64 = y.parse().ok()?;
    let month: i64 = m.parse().ok()?;
    let day: i64 = d.parse().ok()?;
    Some(json!({
        "@type": "PartialDate",
        "year": year,
        "month": month,
        "day": day,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_vcard() -> Vcard {
        let text = "BEGIN:VCARD\r\nVERSION:3.0\r\nUID:contact-1\r\nN:Smith;John;Quincy;Dr.;Jr.\r\nFN:Dr. John Quincy Smith Jr.\r\nORG:Acme;Engineering\r\nTITLE:Researcher\r\nEMAIL;TYPE=WORK;PREF=1:john@work.example\r\nEMAIL;TYPE=HOME:john@home.example\r\nTEL;TYPE=CELL:+1 555 0100\r\nTEL;TYPE=WORK;TYPE=VOICE:+1 555 0200\r\nTEL;TYPE=HOME;TYPE=FAX:+1 555 0300\r\nADR;TYPE=HOME:;;1 Main St;Springfield;IL;62701;USA\r\nADR;TYPE=WORK:;;2 Corp Ave;Metropolis;NY;10001;USA\r\nNOTE:Hello\\nWorld\r\nBDAY:19700131\r\nNICKNAME:Johnny\r\nURL:https://john.example\r\nX-FILEAS:Smith, John\r\nX-AS-SPOUSE:Jane\r\nX-AS-CHILD:Little\r\nEND:VCARD\r\n";
        vcard::parse_vcard_from_data(text).unwrap()
    }

    #[test]
    fn vcard_to_jscard_full_and_back() {
        let v = sample_vcard();
        let card = vcard_to_jscard(&v, "contact-1", None);
        assert_eq!(card["@type"].as_str(), Some("Card"));

        // Name components
        let comps = card["name"]["components"].as_array().unwrap();
        assert!(
            comps
                .iter()
                .any(|c| c["kind"] == "given" && c["value"] == "John")
        );
        assert!(
            comps
                .iter()
                .any(|c| c["kind"] == "surname" && c["value"] == "Smith")
        );
        assert!(
            comps
                .iter()
                .any(|c| c["kind"] == "given2" && c["value"] == "Quincy")
        );

        // Emails
        let emails = card["emails"].as_object().unwrap();
        assert_eq!(emails.len(), 2);

        // Phones incl. features
        let phones = card["phones"].as_object().unwrap();
        assert_eq!(phones.len(), 3);
        assert!(
            phones
                .values()
                .any(|p| p.get("features").and_then(|f| f.get("cell")) == Some(&json!(true)))
        );

        // Addresses
        let addrs = card["addresses"].as_object().unwrap();
        assert_eq!(addrs.len(), 2);

        // Round-trip back keeps the key fields intact.
        let back = jscard_to_vcard(&card, "contact-1");
        assert_eq!(back.full_name(), Some("Dr. John Quincy Smith Jr."));
        assert_eq!(back.structured_name().unwrap().given, "John");
        assert_eq!(back.structured_name().unwrap().family, "Smith");
        assert_eq!(back.structured_name().unwrap().additional, "Quincy");
        assert_eq!(back.emails().len(), 2);
        assert!(back.typed_phones().iter().any(|t| t.has_type("cell")));
        assert!(back.typed_phones().iter().any(|t| t.has_type("fax")));
        assert_eq!(back.addresses().len(), 2);
        assert_eq!(back.org().unwrap()[..2], ["Acme", "Engineering"]);
        assert_eq!(back.title(), Some("Researcher"));
        assert_eq!(back.note(), Some("Hello\nWorld"));
        assert_eq!(back.bday(), Some("19700131"));
        assert_eq!(back.nicknames(), ["Johnny"]);
        assert_eq!(back.url(), Some("https://john.example"));
        assert_eq!(back.spouse(), Some("Jane"));
        assert_eq!(back.children(), ["Little"]);

        // UIDs of both forms must round-trip.
        assert!(
            back.properties
                .iter()
                .any(|p| matches!(p, vcard::Property::Uid(u) if u.value == "contact-1"))
        );
    }

    #[test]
    fn jscard_handles_missing_optional_fields() {
        let card = json!({
            "@type": "Card",
            "version": "1.0",
            "uid": "x",
            "fullName": "Only Name",
        });
        let v = jscard_to_vcard(&card, "x");
        assert_eq!(v.full_name(), Some("Only Name"));
        assert!(v.addresses().is_empty());
        assert!(v.structured_name().is_none());
    }
}
