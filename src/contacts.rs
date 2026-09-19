// src/contacts.rs
// EAS contact sync (MS-ASCNTC) with a JMAP-first backend.
//
// Backend order (mirrors the calendar implementation):
//   1. JMAP Contacts (`urn:ietf:params:jmap:contacts`, RFC 9610 with
//      JSCard per RFC 9553) — default when Stalwart advertises the
//      capability and `GATEWAY_PREFER_JMAP_CONTACTS` is enabled.
//   2. CardDAV (legacy fallback).
//
// The EAS <-> vCard mapping is lossless for the MS-ASCNTC field set:
// structured names (title/given/middle/surname/suffix), up to three typed
// e-mails, typed phone numbers (work x2, home x2, mobile, work fax, home
// fax, pager, car, radio), the three typed postal addresses
// (home/business/other, all components), company/department, job title,
// birthday, wedding anniversary, notes, picture, web page, nickname,
// spouse, children and FileAs.

use crate::carddav::Contact as CarddavContact;
use crate::jscard::{jscard_to_vcard, vcard_to_jscard};
use crate::util::resolve_xml_reference;
use crate::vcard::{self, Adr, Email, Name, Parameter, Property, Tel, Vcard};
use anyhow::{Context as _, anyhow};
use base64::Engine as _;
use reqwest::StatusCode;
use std::collections::HashSet;
use uuid::Uuid;

// --------------------------------------------------------------------------
// EAS field mapping constants
// --------------------------------------------------------------------------

/// Field-class names used in `ContactsMutation::Change` merge logic.
const K_NAME: &str = "name";
const K_EMAILS: &str = "emails";
const K_PHONES: &str = "phones";
const K_ADDR_HOME: &str = "addresses:home";
const K_ADDR_WORK: &str = "addresses:work";
const K_ADDR_OTHER: &str = "addresses:other";
const K_ORG: &str = "org";
const K_TITLE: &str = "title";
const K_NOTE: &str = "note";
const K_BDAY: &str = "bday";
const K_ANNIVERSARY: &str = "anniversary";
const K_NICKNAME: &str = "nickname";
const K_URL: &str = "url";
const K_PHOTO: &str = "photo";
const K_FILEAS: &str = "fileas";
const K_DEPARTMENT: &str = "department";
const K_SPOUSE: &str = "spouse";
const K_CHILDREN: &str = "children";

// --------------------------------------------------------------------------
// EAS rendering (backend vCard -> MS-ASCNTC ApplicationData)
// --------------------------------------------------------------------------

/// Render a contact for EAS Sync in MS-ASCNTC format.
/// The returned XML is the content of `<ApplicationData>` with
/// `Contacts:`-prefixed fields.
pub fn render_eas_contact(_server_id: &str, carddav_contact: &CarddavContact) -> String {
    let vcard = match vcard::parse_vcard_from_data(&carddav_contact.vcard) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(href = %carddav_contact.href, error = %e, "Failed to parse vCard, using blank");
            Vcard::default()
        }
    };

    let mut xml = String::new();

    // -- Structured name ---------------------------------------------------
    // Prefer the structured N. If absent, keep FN as FileAs and FirstName
    // (never split multi-part names into fake first/last components).
    let n = vcard.structured_name();
    let fn_val = vcard.full_name().unwrap_or_default();
    match n {
        Some(n) => {
            eas_field(&mut xml, "FirstName", &n.given);
            eas_field(&mut xml, "MiddleName", &n.additional);
            eas_field(&mut xml, "LastName", &n.family);
            eas_field(&mut xml, "Suffix", &n.suffix);
            eas_field(&mut xml, "Title", &n.prefix);
        }
        None => {
            eas_field(&mut xml, "FirstName", fn_val);
        }
    }

    // FileAs
    let file_as = vcard
        .file_as()
        .map(|s| s.to_string())
        .filter(|s| !s.is_empty())
        .or_else(|| {
            n.and_then(|n| {
                if !n.family.is_empty() && !n.given.is_empty() {
                    Some(format!("{}, {}", n.family, n.given))
                } else {
                    None
                }
            })
        })
        .unwrap_or_else(|| fn_val.to_string());
    eas_field(&mut xml, "FileAs", &file_as);

    // Company / department / job title
    if let Some(org) = vcard.org() {
        eas_field(
            &mut xml,
            "CompanyName",
            org.first().map(String::as_str).unwrap_or(""),
        );
        if let Some(u) = org.get(1) {
            eas_field(&mut xml, "Department", u);
        }
    }
    if let Some(dept) = vcard.department()
        && !dept.is_empty()
        && !xml.contains("<Contacts:Department>")
    {
        eas_field(&mut xml, "Department", &dept);
    }
    eas_field(&mut xml, "JobTitle", vcard.title().unwrap_or_default());

    // Emails: PREF first, then declaration order, up to three slots.
    let mut emails = vcard.typed_emails();
    emails.sort_by_key(|e| {
        e.params
            .iter()
            .find_map(|p| match p {
                Parameter::Pref(n) => Some(*n),
                _ => None,
            })
            .unwrap_or(50)
    });
    for (idx, email) in emails.iter().take(3).enumerate() {
        let tag = match idx {
            0 => "Email1Address",
            1 => "Email2Address",
            _ => "Email3Address",
        };
        eas_field(&mut xml, tag, &email.email);
    }

    // Phones: typed mapping to the MS-ASCNTC phone fields. Each tag may be
    // written at most once; the first phone wins per slot.
    let mut used: HashSet<&'static str> = HashSet::new();
    // 1st pass: explicitly typed phones.
    for tel in vcard.typed_phones() {
        if tel.has_type("cell") {
            eas_phone(&mut xml, &mut used, "MobilePhoneNumber", &tel.number);
        } else if tel.has_type("fax") {
            if tel.has_type("home") {
                eas_phone(&mut xml, &mut used, "HomeFaxNumber", &tel.number);
            } else {
                eas_phone(&mut xml, &mut used, "BusinessFaxNumber", &tel.number);
            }
        } else if tel.has_type("pager") {
            eas_phone(&mut xml, &mut used, "PagerNumber", &tel.number);
        } else if tel.has_type("home") {
            if used.contains("HomePhoneNumber") {
                eas_phone(&mut xml, &mut used, "Home2PhoneNumber", &tel.number);
            } else {
                eas_phone(&mut xml, &mut used, "HomePhoneNumber", &tel.number);
            }
        } else if tel.has_type("work") {
            if used.contains("BusinessPhoneNumber") {
                eas_phone(&mut xml, &mut used, "Business2PhoneNumber", &tel.number);
            } else {
                eas_phone(&mut xml, &mut used, "BusinessPhoneNumber", &tel.number);
            }
        }
    }
    // 2nd pass: untyped phones fill the first free voice slot.
    for tel in vcard.typed_phones() {
        if ["cell", "fax", "pager", "home", "work"]
            .iter()
            .any(|t| tel.has_type(t))
        {
            continue;
        }
        for slot in [
            "BusinessPhoneNumber",
            "HomePhoneNumber",
            "Business2PhoneNumber",
            "Home2PhoneNumber",
        ] {
            if !used.contains(slot) {
                eas_phone(&mut xml, &mut used, slot, &tel.number);
                break;
            }
        }
    }

    // Addresses: home / business / other groups.
    let mut wrote: HashSet<&str> = HashSet::new();
    for adr in vcard.addresses() {
        let ctx = adr.context();
        if wrote.contains(ctx) {
            // EAS carries only one address per context; extras stay in the
            // backend vCard (preserved in storage) but are not rendered.
            continue;
        }
        wrote.insert(ctx);
        let (p_street, p_city, p_state, p_zip, p_country) = match ctx {
            "home" => (
                "HomeAddressStreet",
                "HomeAddressCity",
                "HomeAddressState",
                "HomeAddressPostalCode",
                "HomeAddressCountry",
            ),
            "work" => (
                "BusinessAddressStreet",
                "BusinessAddressCity",
                "BusinessAddressState",
                "BusinessAddressPostalCode",
                "BusinessAddressCountry",
            ),
            _ => (
                "OtherAddressStreet",
                "OtherAddressCity",
                "OtherAddressState",
                "OtherAddressPostalCode",
                "OtherAddressCountry",
            ),
        };
        let street = if adr.extension.is_empty() {
            adr.street.clone()
        } else if adr.street.is_empty() {
            adr.extension.clone()
        } else {
            format!("{} {}", adr.extension, adr.street)
        };
        eas_field(&mut xml, p_street, &street);
        eas_field(&mut xml, p_city, &adr.locality);
        eas_field(&mut xml, p_state, &adr.region);
        eas_field(&mut xml, p_zip, &adr.postal_code);
        eas_field(&mut xml, p_country, &adr.country);
    }

    // Dates: EAS expects "YYYY-MM-DDT00:00:00.000Z".
    if let Some(b) = vcard.bday() {
        eas_field(&mut xml, "Birthday", &vcard_date_to_eas(b));
    }
    if let Some(a) = vcard.anniversary() {
        eas_field(&mut xml, "Anniversary", &vcard_date_to_eas(a));
    }

    // Free-form note -> AirSyncBase text body (EAS 16.1).
    if let Some(note) = vcard.note()
        && !note.is_empty()
    {
        let escaped = xml_escape_str(note);
        xml.push_str("<AirSyncBase:Body><AirSyncBase:Type>1</AirSyncBase:Type>");
        xml.push_str(&format!("<AirSyncBase:Data>{escaped}</AirSyncBase:Data>"));
        xml.push_str(&format!(
            "<AirSyncBase:EstimatedDataSize>{}</AirSyncBase:EstimatedDataSize>",
            note.len(),
        ));
        xml.push_str("<AirSyncBase:Truncated>0</AirSyncBase:Truncated></AirSyncBase:Body>");
    }

    // Picture (base64 image payload).
    if let Some(p) = vcard.photo().and_then(extract_photo_base64) {
        xml.push_str(&format!("<Contacts:Picture>{}</Contacts:Picture>", p));
    }

    // Misc simple fields.
    eas_field(&mut xml, "WebPage", vcard.url().unwrap_or_default());
    eas_field(&mut xml, "Spouse", vcard.spouse().unwrap_or_default());
    if let Some(nick) = vcard.nicknames().first() {
        xml.push_str(&format!(
            "<Contacts2:NickName>{}</Contacts2:NickName>",
            xml_escape_str(nick)
        ));
    }
    let children = vcard.children();
    if !children.is_empty() {
        xml.push_str("<Contacts:Children>");
        for child in children {
            if !child.is_empty() {
                xml.push_str(&format!(
                    "<Contacts:Child>{}</Contacts:Child>",
                    xml_escape_str(child)
                ));
            }
        }
        xml.push_str("</Contacts:Children>");
    }

    xml
}

fn xml_escape_str(s: &str) -> String {
    xml_escape(s)
}

/// Append a non-empty `Contacts:` field.
fn eas_field(xml: &mut String, tag: &str, val: &str) {
    if !val.is_empty() {
        xml.push_str(&format!(
            "<Contacts:{tag}>{}</Contacts:{tag}>",
            xml_escape_str(val)
        ));
    }
}

/// Append a phone field, writing each tag at most once.
fn eas_phone(xml: &mut String, used: &mut HashSet<&'static str>, tag: &'static str, num: &str) {
    if num.is_empty() || used.contains(tag) {
        return;
    }
    used.insert(tag);
    eas_field(xml, tag, num);
}

/// Render a complete <Add> element for EAS Sync.
pub fn render_eas_add(server_id: &str, carddav_contact: &CarddavContact) -> String {
    format!(
        r#"<Add><ServerId>{}</ServerId><ApplicationData>{}</ApplicationData></Add>"#,
        xml_escape(server_id),
        render_eas_contact(server_id, carddav_contact)
    )
}

/// Render a complete <Change> element for EAS Sync.
pub fn render_eas_change(server_id: &str, carddav_contact: &CarddavContact) -> String {
    format!(
        r#"<Change><ServerId>{}</ServerId><ApplicationData>{}</ApplicationData></Change>"#,
        xml_escape(server_id),
        render_eas_contact(server_id, carddav_contact)
    )
}

// --------------------------------------------------------------------------
// EAS <-> vCard date helpers
// --------------------------------------------------------------------------

/// "19700131" / "1970-01-31" -> "1970-01-31T00:00:00.000Z".
pub fn vcard_date_to_eas(v: &str) -> String {
    let digits: String = v.chars().filter(|c| c.is_ascii_digit()).collect();
    if digits.len() == 8 {
        format!(
            "{}-{}-{}T00:00:00.000Z",
            &digits[0..4],
            &digits[4..6],
            &digits[6..8]
        )
    } else {
        v.to_string()
    }
}

/// EAS datetime "1970-01-31T00:00:00.000Z" -> vCard "19700131".
fn eas_date_to_vcard(v: &str) -> String {
    let digits: String = v.chars().filter(|c| c.is_ascii_digit()).collect();
    if digits.len() >= 8 {
        digits[..8].to_string()
    } else {
        v.to_string()
    }
}

/// Extract the base64 image payload from a photo value (data URI or raw b64).
fn extract_photo_base64(p: &str) -> Option<&str> {
    if p.is_empty() || p.starts_with("http://") || p.starts_with("https://") {
        return None;
    }
    if let Some(rest) = p.strip_prefix("data:") {
        let (_, b64) = rest.split_once(',')?;
        return if b64.is_empty() { None } else { Some(b64) };
    }
    Some(p)
}

// --------------------------------------------------------------------------
// EAS mutation parsing (MS-ASCNTC ApplicationData -> vCard)
// --------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
pub enum ContactsOpKind {
    Add,
    Change,
    Delete,
}

#[derive(Debug, Clone)]
pub enum ContactsMutation {
    Add {
        client_id: Option<String>,
        server_id: String,
        vcard: String,
    },
    Change {
        server_id: String,
        vcard: String,
        /// Field classes present in the client Change; only these are
        /// replaced server-side so untouched data is never wiped out.
        present: HashSet<String>,
    },
    Delete {
        server_id: String,
    },
}

/// Map an EAS local tag name to the merged field class (None = ignore).
fn field_class(local: &str) -> Option<&'static str> {
    Some(match local {
        "FirstName" | "MiddleName" | "LastName" | "Suffix" | "Title" => K_NAME,
        "Email1Address" | "Email2Address" | "Email3Address" => K_EMAILS,
        "BusinessPhoneNumber"
        | "Business2PhoneNumber"
        | "HomePhoneNumber"
        | "Home2PhoneNumber"
        | "MobilePhoneNumber"
        | "BusinessFaxNumber"
        | "HomeFaxNumber"
        | "PagerNumber"
        | "CarPhoneNumber"
        | "RadioPhoneNumber"
        | "AssistantPhoneNumber"
        | "CompanyMainPhone" => K_PHONES,
        "HomeAddressStreet"
        | "HomeAddressCity"
        | "HomeAddressState"
        | "HomeAddressPostalCode"
        | "HomeAddressCountry"
        | "HomeCity"
        | "HomeCountry" => K_ADDR_HOME,
        "BusinessAddressStreet"
        | "BusinessAddressCity"
        | "BusinessAddressState"
        | "BusinessAddressPostalCode"
        | "BusinessAddressCountry" => K_ADDR_WORK,
        "OtherAddressStreet"
        | "OtherAddressCity"
        | "OtherAddressState"
        | "OtherAddressPostalCode"
        | "OtherAddressCountry" => K_ADDR_OTHER,
        "CompanyName" => K_ORG,
        "Department" => K_DEPARTMENT,
        "JobTitle" => K_TITLE,
        "Birthday" => K_BDAY,
        "Anniversary" => K_ANNIVERSARY,
        "NickName" => K_NICKNAME,
        "WebPage" => K_URL,
        "Picture" => K_PHOTO,
        "FileAs" => K_FILEAS,
        "Spouse" => K_SPOUSE,
        "Children" | "Child" => K_CHILDREN,
        _ => return None,
    })
}

/// Collected raw EAS fields from one ApplicationData element.
#[derive(Default)]
struct EasContactFields {
    fields: Vec<(String, String)>, // (local name, text)
    note: String,
}

impl EasContactFields {
    fn get(&self, local: &str) -> Option<&str> {
        self.fields
            .iter()
            .find(|(name, _)| name == local)
            .map(|(_, v)| v.as_str())
            .filter(|s| !s.is_empty())
    }

    fn get_all(&self, local: &str) -> Vec<&str> {
        self.fields
            .iter()
            .filter(|(name, _)| name == local)
            .map(|(_, v)| v.as_str())
            .filter(|s| !s.is_empty())
            .collect()
    }

    fn present(&self) -> HashSet<String> {
        self.fields
            .iter()
            .filter_map(|(name, _)| field_class(name).map(|c| c.to_string()))
            .collect()
    }
}

/// Build a vCard from parsed EAS fields.
fn vcard_from_eas(f: &EasContactFields, uid: &str) -> Vcard {
    let mut v = Vcard::default();
    v.properties.push(Property::Uid(vcard::Uid {
        value: uid.to_string(),
    }));

    // Name / full name
    let first = f.get("FirstName").unwrap_or("");
    let middle = f.get("MiddleName").unwrap_or("");
    let last = f.get("LastName").unwrap_or("");
    let suffix = f.get("Suffix").unwrap_or("");
    let courtesy = f.get("Title").unwrap_or("");
    if !first.is_empty()
        || !middle.is_empty()
        || !last.is_empty()
        || !suffix.is_empty()
        || !courtesy.is_empty()
    {
        v.properties.push(Property::N(Name {
            family: last.to_string(),
            given: first.to_string(),
            additional: middle.to_string(),
            prefix: courtesy.to_string(),
            suffix: suffix.to_string(),
        }));
        // Compose FN from the structured name so clients that only display FN
        // still show the full name.
        let fn_composed = [courtesy, first, middle, last, suffix]
            .into_iter()
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join(" ");
        v.properties
            .push(Property::Fn(vcard::Fn { value: fn_composed }));
    }
    if let Some(fa) = f.get("FileAs") {
        v.properties.push(Property::FileAs(fa.to_string()));
    }

    // Company (+ department)
    let company = f.get("CompanyName").unwrap_or("");
    let department = f.get("Department").unwrap_or("");
    if !company.is_empty() || !department.is_empty() {
        let mut comps = vec![company.to_string()];
        if !department.is_empty() {
            comps.push(department.to_string());
        } else {
            comps.push(String::new());
        }
        v.properties
            .push(Property::Org(vcard::Org { value: comps }));
    }
    if let Some(job) = f.get("JobTitle") {
        v.properties.push(Property::Title(vcard::Title {
            value: job.to_string(),
        }));
    }

    // Emails
    for local in ["Email1Address", "Email2Address", "Email3Address"] {
        if let Some(addr) = f.get(local) {
            v.properties.push(Property::Email(Email {
                email: addr.to_string(),
                params: Vec::new(),
            }));
        }
    }

    // Phones
    let mut push_tel = |local: &str, types: &[&str]| {
        if let Some(num) = f.get(local) {
            let params = types
                .iter()
                .map(|t| Parameter::Type(t.to_string()))
                .collect();
            v.properties.push(Property::Tel(Tel {
                number: num.to_string(),
                params,
            }));
        }
    };
    push_tel("BusinessPhoneNumber", &["work", "voice"]);
    push_tel("Business2PhoneNumber", &["work", "voice"]);
    push_tel("HomePhoneNumber", &["home", "voice"]);
    push_tel("Home2PhoneNumber", &["home", "voice"]);
    push_tel("MobilePhoneNumber", &["cell", "voice"]);
    push_tel("BusinessFaxNumber", &["work", "fax"]);
    push_tel("HomeFaxNumber", &["home", "fax"]);
    push_tel("PagerNumber", &["pager"]);
    push_tel("CarPhoneNumber", &["car"]);
    push_tel("RadioPhoneNumber", &["radio"]);

    // Addresses: group per EAS address block. Legacy EAS 2.5 names
    // ("HomeCity", "HomeCountry") are accepted as fallbacks.
    let mut push_addr = |prefix: &str, legacy: &str, context: &str| {
        let get2 = |new: &str, old: &str| {
            if new != old
                && let Some(v) = f.get(new)
            {
                return Some(v);
            }
            f.get(old)
        };
        let street = get2(
            &format!("{prefix}AddressStreet"),
            &format!("{legacy}Street"),
        )
        .unwrap_or("");
        let city = get2(&format!("{prefix}AddressCity"), &format!("{legacy}City")).unwrap_or("");
        let state = get2(&format!("{prefix}AddressState"), &format!("{legacy}State")).unwrap_or("");
        let zip = get2(
            &format!("{prefix}AddressPostalCode"),
            &format!("{legacy}PostalCode"),
        )
        .unwrap_or("");
        let country = get2(
            &format!("{prefix}AddressCountry"),
            &format!("{legacy}Country"),
        )
        .unwrap_or("");
        if street.is_empty()
            && city.is_empty()
            && state.is_empty()
            && zip.is_empty()
            && country.is_empty()
        {
            return;
        }
        let mut params = Vec::new();
        if context != "other" {
            params.push(Parameter::Type(context.to_string()));
        }
        v.properties.push(Property::Adr(Adr {
            po_box: String::new(),
            extension: String::new(),
            street: street.to_string(),
            locality: city.to_string(),
            region: state.to_string(),
            postal_code: zip.to_string(),
            country: country.to_string(),
            params,
        }));
    };
    push_addr("Home", "Home", "home");
    push_addr("Business", "Business", "work");
    push_addr("Other", "Other", "other");

    // Dates
    if let Some(b) = f.get("Birthday") {
        v.properties.push(Property::Bday(eas_date_to_vcard(b)));
    }
    if let Some(a) = f.get("Anniversary") {
        v.properties
            .push(Property::Anniversary(eas_date_to_vcard(a)));
    }

    // Note / picture / url / misc
    if !f.note.is_empty() {
        v.properties.push(Property::Note(f.note.clone()));
    }
    if let Some(pic) = f.get("Picture") {
        v.properties
            .push(Property::Photo(format!("data:image/jpeg;base64,{pic}")));
    }
    if let Some(url) = f.get("WebPage") {
        v.properties.push(Property::Url(url.to_string()));
    }
    if let Some(nick) = f.get("NickName") {
        v.properties.push(Property::Nickname(nick.to_string()));
    }
    if let Some(spouse) = f.get("Spouse") {
        v.properties.push(Property::Spouse(spouse.to_string()));
    }
    for child in f.get_all("Child") {
        v.properties.push(Property::Child(child.to_string()));
    }

    v
}

/// Parse the fields inside one `<ApplicationData>` element.
fn parse_eas_application_data(xml: &str) -> EasContactFields {
    use quick_xml::Reader;
    use quick_xml::events::Event;

    let mut out = EasContactFields::default();
    let mut stack: Vec<String> = Vec::new();
    let mut reader = Reader::from_str(xml);
    let mut buf = Vec::new();

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) => {
                let name = e.name().as_ref().to_string();
                stack.push(name);
            }
            Ok(Event::End(_)) => {
                stack.pop();
            }
            Ok(Event::Text(e)) => {
                let text = e.as_ref().to_string();
                record_eas_text(&mut out, &stack, text);
            }
            Ok(Event::GeneralRef(e)) => {
                let text = resolve_xml_reference(e.as_ref()).to_string();
                record_eas_text(&mut out, &stack, text);
            }
            Ok(Event::Eof) => break,
            _ => {}
        }
        buf.clear();
    }
    out
}

fn local_name(tag: &str) -> &str {
    match tag.rsplit_once(':') {
        Some((_, local)) => local,
        None => tag,
    }
}

fn record_eas_text(out: &mut EasContactFields, stack: &[String], text: String) {
    let Some(leaf) = stack.last() else { return };
    let leaf = local_name(leaf);
    // AirSyncBase <Body><Data>...</Data></Body> carries the note text.
    if leaf == "Data" {
        let in_body = stack
            .iter()
            .rev()
            .nth(1)
            .map(|p| local_name(p) == "Body")
            .unwrap_or(false);
        if in_body {
            if !out.note.is_empty() {
                out.note.push('\n');
            }
            out.note.push_str(&text);
        }
    } else if leaf == "Children" {
        // Structural parent — no text.
    } else if leaf != "ApplicationData"
        && leaf != "Add"
        && leaf != "Change"
        && leaf != "Delete"
        && leaf != "ServerId"
        && leaf != "ClientId"
        && leaf != "Body"
        && leaf != "Type"
        && leaf != "EstimatedDataSize"
        && leaf != "Truncated"
    {
        out.fields.push((leaf.to_string(), text));
    }
}

/// Merge client-Change fields into the server copy without dropping data.
///
/// `present` names the field classes that appeared in the Change payload;
/// only those classes are replaced — every other property of the old vCard
/// is carried over untouched.
pub fn merge_vcards(old: &Vcard, new_fields: &Vcard, present: &HashSet<String>) -> Vcard {
    let class_of = |p: &Property| -> Option<&'static str> {
        match p {
            Property::Fn(_) | Property::N(_) => Some(K_NAME),
            Property::Email(_) => Some(K_EMAILS),
            Property::Tel(_) => Some(K_PHONES),
            Property::Adr(a) => Some(match a.context() {
                "home" => K_ADDR_HOME,
                "work" => K_ADDR_WORK,
                _ => K_ADDR_OTHER,
            }),
            Property::Org(_) => Some(K_ORG),
            Property::Title(_) => Some(K_TITLE),
            Property::Note(_) => Some(K_NOTE),
            Property::Bday(_) => Some(K_BDAY),
            Property::Anniversary(_) => Some(K_ANNIVERSARY),
            Property::Nickname(_) => Some(K_NICKNAME),
            Property::Url(_) => Some(K_URL),
            Property::Photo(_) => Some(K_PHOTO),
            Property::FileAs(_) => Some(K_FILEAS),
            Property::Department(_) => Some(K_DEPARTMENT),
            Property::Spouse(_) => Some(K_SPOUSE),
            Property::Child(_) => Some(K_CHILDREN),
            Property::Uid(_) | Property::Raw(_) => None,
        }
    };

    let mut merged = Vcard::default();
    for p in &old.properties {
        match class_of(p) {
            Some(class) if present.contains(class) => { /* replaced below */ }
            _ => merged.properties.push(p.clone()),
        }
    }
    for p in &new_fields.properties {
        match class_of(p) {
            Some(class) if present.contains(class) => merged.properties.push(p.clone()),
            _ => {
                // Not part of the changed set: keep only if old copy absent
                // (e.g. missing legacy classes).
            }
        }
    }
    merged
}

// --------------------------------------------------------------------------
// Mutation XML parsing
// --------------------------------------------------------------------------

/// Parse EAS Sync contact mutations from the `<Collection>` body.
///
/// Two payload shapes are accepted:
///  1. MS-ASCNTC `<ApplicationData>` elements (what real EAS clients send)
///     whose `Contacts:`/`Contacts2:`/`AirSyncBase:` fields are mapped into a
///     full vCard, with only present field classes tracked for Change merges.
///  2. Legacy `<vCard>` payloads used by older gateway protocol fixtures
///     (full-replace semantics for Change).
pub fn parse_contacts_mutations(xml: &str) -> anyhow::Result<Vec<ContactsMutation>> {
    use quick_xml::Reader;
    use quick_xml::events::Event;

    #[derive(Default)]
    struct Current {
        kind: Option<ContactsOpKind>,
        server_id: String,
        client_id: Option<String>,
        appdata: String,
        vcard: String,
        in_vcard: bool,
    }

    let mut reader = Reader::from_str(xml);
    let mut buf = Vec::new();
    let mut stack: Vec<String> = Vec::new();
    let mut cur = Current::default();
    let mut mutations = Vec::new();
    let mut appdata_xml = String::new();
    let mut in_appdata = false;

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) => {
                let name = e.name().as_ref().to_string();
                let local = local_name(&name).to_string();
                match local.as_str() {
                    "Add" | "Change" | "Delete" if cur.kind.is_none() => {
                        cur = Current::default();
                        cur.kind = Some(match local.as_str() {
                            "Add" => ContactsOpKind::Add,
                            "Change" => ContactsOpKind::Change,
                            _ => ContactsOpKind::Delete,
                        });
                    }
                    "vCard" => cur.in_vcard = true,
                    "ApplicationData" if cur.kind.is_some() => {
                        in_appdata = true;
                        appdata_xml.clear();
                    }
                    _ => {}
                }
                stack.push(name);
            }
            Ok(Event::End(e)) => {
                let name = e.name().as_ref().to_string();
                let local = local_name(&name);
                let is_top = stack.last().map(|s| s.as_str()) == Some(name.as_str());
                stack.pop();
                match local {
                    "ApplicationData" if in_appdata => {
                        // Ensure the closing tag itself is excluded by trimming
                        // trailing "</ApplicationData>" ... it was never added.
                        cur.appdata = appdata_xml.clone();
                        in_appdata = false;
                    }
                    "vCard" => cur.in_vcard = false,
                    "Add" | "Change" | "Delete" if is_top || cur.kind.is_some() => {
                        finish_current(&mut cur, &mut mutations)?;
                    }
                    _ => {}
                }
            }
            Ok(Event::Text(e)) => {
                let text = e.as_ref().to_string();
                let leaf = stack.last().map(|s| local_name(s)).unwrap_or("");
                if cur.in_vcard {
                    cur.vcard.push_str(&text);
                } else if in_appdata {
                    track_appdata_text(&mut appdata_xml, &stack, &text);
                } else if leaf == "ServerId" {
                    cur.server_id.push_str(&text);
                } else if leaf == "ClientId" {
                    cur.client_id
                        .get_or_insert_with(String::new)
                        .push_str(&text);
                }
            }
            Ok(Event::GeneralRef(e)) => {
                let text = resolve_xml_reference(e.as_ref()).to_string();
                if cur.in_vcard {
                    cur.vcard.push_str(&text);
                } else if in_appdata {
                    track_appdata_text(&mut appdata_xml, &stack, &text);
                } else {
                    let leaf = stack.last().map(|s| local_name(s)).unwrap_or("");
                    if leaf == "ServerId" {
                        cur.server_id.push_str(&text);
                    } else if leaf == "ClientId" {
                        cur.client_id
                            .get_or_insert_with(String::new)
                            .push_str(&text);
                    }
                }
            }
            Ok(Event::Eof) => break,
            _ => {}
        }
        buf.clear();
    }

    fn finish_current(
        cur: &mut Current,
        mutations: &mut Vec<ContactsMutation>,
    ) -> anyhow::Result<()> {
        let kind = cur.kind.take();
        let server_id = std::mem::take(&mut cur.server_id);
        let client_id = cur.client_id.take();
        let appdata = std::mem::take(&mut cur.appdata);
        let legacy_vcard = std::mem::take(&mut cur.vcard);
        cur.in_vcard = false;
        let Some(kind) = kind else { return Ok(()) };
        match kind {
            ContactsOpKind::Delete => {
                if !server_id.is_empty() {
                    mutations.push(ContactsMutation::Delete { server_id });
                }
            }
            ContactsOpKind::Add => {
                if !appdata.is_empty() {
                    let fields = parse_eas_application_data(&appdata);
                    let uid = Uuid::new_v4().simple().to_string();
                    let v = vcard_from_eas(&fields, &format!("contact-{uid}"));
                    mutations.push(ContactsMutation::Add {
                        client_id,
                        server_id,
                        vcard: v.to_string(),
                    });
                } else if !legacy_vcard.is_empty() {
                    mutations.push(ContactsMutation::Add {
                        client_id,
                        server_id,
                        vcard: legacy_vcard,
                    });
                }
            }
            ContactsOpKind::Change => {
                if server_id.is_empty() {
                    return Ok(());
                }
                if !appdata.is_empty() {
                    let fields = parse_eas_application_data(&appdata);
                    let present = fields.present();
                    let v = vcard_from_eas(&fields, "");
                    mutations.push(ContactsMutation::Change {
                        server_id,
                        vcard: v.to_string(),
                        present,
                    });
                } else if !legacy_vcard.is_empty() {
                    // Legacy full-replace semantics.
                    let all: HashSet<String> = [
                        K_NAME,
                        K_EMAILS,
                        K_PHONES,
                        K_ADDR_HOME,
                        K_ADDR_WORK,
                        K_ADDR_OTHER,
                        K_ORG,
                        K_TITLE,
                        K_NOTE,
                        K_BDAY,
                        K_ANNIVERSARY,
                        K_NICKNAME,
                        K_URL,
                        K_PHOTO,
                        K_FILEAS,
                        K_DEPARTMENT,
                        K_SPOUSE,
                        K_CHILDREN,
                    ]
                    .iter()
                    .map(|s| s.to_string())
                    .collect();
                    mutations.push(ContactsMutation::Change {
                        server_id,
                        vcard: legacy_vcard,
                        present: all,
                    });
                }
            }
        }
        Ok(())
    }

    Ok(mutations)
}

/// Re-emit ApplicationData text preserving element structure so the inner
/// XML can be re-parsed (with tags) for field extraction.
fn track_appdata_text(out: &mut String, stack: &[String], text: &str) {
    let Some(leaf) = stack.last() else { return };
    let leaf = local_name(leaf);
    if leaf == "ApplicationData" {
        return;
    }
    out.push_str(&format!(
        "<{}>{}</{}>",
        leaf,
        crate::util::escape_xml_text(text),
        leaf
    ));
}

// --------------------------------------------------------------------------
// Backend-agnostic sync diff
// --------------------------------------------------------------------------

/// Shared diff logic: `current` is the authoritative backend snapshot. The
/// gateway diff journal (SQLite, unchanged since the last sync) determines
/// Added/Changed/Deleted relative to the previous snapshot.
async fn diff_contacts(
    state: &crate::models::AppState,
    username: &str,
    device_id: &str,
    current: &[CarddavContact],
) -> anyhow::Result<String> {
    let state_collection_id = format!("8::{}", device_id);

    let (_server_sync_key, _token) = state
        .storage
        .get_sync_key(username, &state_collection_id)
        .await?
        .unwrap_or_default();

    let current_hrefs: std::collections::HashSet<_> = current.iter().map(|c| &c.href).collect();

    let db_contacts_by_server_id: std::collections::HashMap<_, _> = state
        .storage
        .get_all_contacts_for_owner(username)
        .await?
        .into_iter()
        .map(|row| (row.server_id.clone(), row))
        .collect();

    let db_contacts_by_href: std::collections::HashMap<_, _> = db_contacts_by_server_id
        .values()
        .map(|row| (row.carddav_href.clone(), row))
        .collect();

    let mut adds = Vec::new();
    let mut changes = Vec::new();
    let mut deletes = Vec::new();

    for c in current {
        if let Some(db_row) = db_contacts_by_href.get(&c.href) {
            if db_row.etag.as_deref() != c.etag.as_deref() {
                changes.push((db_row.server_id.clone(), c.clone()));
            }
        } else {
            let new_server_id = format!("contact-{}", Uuid::new_v4().simple());
            adds.push((new_server_id.clone(), c.clone()));
            state
                .storage
                .insert_contact(
                    username,
                    &c.href,
                    &new_server_id,
                    c.etag.as_deref(),
                    Some(&c.vcard),
                )
                .await?;
        }
    }

    for (server_id, db_row) in db_contacts_by_server_id {
        if !current_hrefs.contains(&db_row.carddav_href) {
            deletes.push(server_id.clone());
            state.storage.delete_contact(username, &server_id).await?;
        }
    }

    let mut response = String::new();
    for (server_id, contact) in adds {
        response.push_str(&render_eas_add(&server_id, &contact));
    }
    for (server_id, contact) in changes {
        response.push_str(&render_eas_change(&server_id, &contact));
    }
    for server_id in deletes {
        response.push_str(&format!(
            r#"<Delete><ServerId>{}</ServerId></Delete>"#,
            xml_escape(&server_id)
        ));
    }

    let new_sync_key = Uuid::new_v4().simple().to_string();
    state
        .storage
        .set_contacts_sync_state(username, &state_collection_id, &new_sync_key)
        .await?;

    Ok(response)
}

// --------------------------------------------------------------------------
// Public sync entry point: JMAP-first, CardDAV fallback
// --------------------------------------------------------------------------

/// Sync contacts for a user. JMAP is the primary backend (RFC 9610); CardDAV
/// is the fallback. Returns the `<Add>/<Change>/<Delete>` fragment.
pub async fn sync_contacts(
    state: &crate::models::AppState,
    username: &str,
    password: &str,
    _client_sync_key: Option<&str>,
    device_id: &str,
) -> anyhow::Result<String> {
    if state.cfg.prefer_jmap_contacts
        && let Some(jmap) = state.jmap_client.as_ref()
    {
        let secret = secrecy::SecretString::from(password.to_string());
        if jmap.supports_contacts(username, &secret).await {
            match collect_jmap_contacts(state, username, password).await {
                Ok(cards) => {
                    return diff_contacts(state, username, device_id, &cards).await;
                }
                Err(e) => {
                    tracing::warn!(
                        error = %e,
                        "JMAP contact sync failed; falling back to CardDAV"
                    );
                }
            }
        }
    }
    sync_contacts_carddav(state, username, password, device_id).await
}

/// Collect all contacts from the JMAP backend as CarddavContact values,
/// downloading photo blobs so rendering stays self-contained.
async fn collect_jmap_contacts(
    state: &crate::models::AppState,
    username: &str,
    password: &str,
) -> anyhow::Result<Vec<CarddavContact>> {
    let jmap = state
        .jmap_client
        .as_ref()
        .ok_or_else(|| anyhow!("JMAP client not configured"))?;
    let secret = secrecy::SecretString::from(password.to_string());
    let account_id = jmap.get_contacts_account_id(username, &secret).await?;
    let (cards, state_token) = jmap.list_contact_cards(username, &secret).await?;

    let mut out = Vec::with_capacity(cards.len());
    for card in cards {
        let mut v = jscard_to_vcard(&card.card, &card.id);
        // Download the photo blob so the EAS Picture element carries data.
        if let Some(blob) = photo_blob_id(&card.card) {
            match jmap
                .download_blob(&account_id, &blob, username, &secret)
                .await
            {
                Ok(bytes) => {
                    let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
                    v.properties
                        .push(Property::Photo(format!("data:image/jpeg;base64,{b64}")));
                }
                Err(e) => {
                    tracing::warn!(error = %e, blob = %blob, "Failed to download contact photo blob");
                }
            }
        }
        use sha2::Digest as _;
        let etag = card.etag.or_else(|| {
            let hash = sha2::Sha256::digest(serde_json::to_vec(&card.card).unwrap_or_default());
            Some(hex::encode(&hash[..16]))
        });
        out.push(CarddavContact {
            href: format!("jmap://contacts/{}", card.id),
            etag,
            vcard: v.to_string(),
        });
    }
    // Touch the state token so future delta syncs can use it; keep full-diff
    // parity for now (Stalwart does not return partial ranges for contacts).
    let _ = state_token;
    Ok(out)
}

// --------------------------------------------------------------------------
// CardDAV fallback sync
// --------------------------------------------------------------------------

async fn sync_contacts_carddav(
    state: &crate::models::AppState,
    username: &str,
    password: &str,
    device_id: &str,
) -> anyhow::Result<String> {
    let carddav = state
        .carddav_client
        .as_ref()
        .ok_or_else(|| anyhow!("CardDAV client not configured"))?;
    let (contacts, _sync_token) = carddav.list_contacts(username, password, None).await?;
    diff_contacts(state, username, device_id, &contacts).await
}

// --------------------------------------------------------------------------
// Client-side mutation application: JMAP-first, CardDAV fallback
// --------------------------------------------------------------------------

/// Apply client mutations from a Sync request. JMAP path is taken when the
/// server supports contacts; CardDAV used as fallback.
pub async fn apply_contacts_mutations(
    state: &crate::models::AppState,
    username: &str,
    password: &str,
    mutations_xml: &str,
) -> anyhow::Result<Vec<ContactsMutationResult>> {
    let mutations = parse_contacts_mutations(mutations_xml)?;
    let mut results = Vec::new();

    // JMAP-first: use it whenever the configured client is present and the
    // server advertises the contacts capability. CardDAV is the fallback.
    let jmap_available = state.cfg.prefer_jmap_contacts
        && match state.jmap_client.as_ref() {
            Some(jmap) => {
                let secret = secrecy::SecretString::from(password.to_string());
                jmap.supports_contacts(username, &secret).await
            }
            None => false,
        };

    for mutation in mutations {
        let result = if jmap_available {
            match apply_mutation_jmap(state, username, password, &mutation).await {
                Ok(r) => r,
                Err(e) => {
                    tracing::warn!(
                        error = %e,
                        "JMAP contact mutation failed; falling back to CardDAV"
                    );
                    apply_mutation_carddav(state, username, password, &mutation).await
                }
            }
        } else {
            apply_mutation_carddav(state, username, password, &mutation).await
        };
        results.push(result);
    }

    Ok(results)
}

fn mutation_failure(m: &ContactsMutation, status: &'static str) -> ContactsMutationResult {
    let (server_id, op_kind) = match m {
        ContactsMutation::Add { server_id, .. } => (server_id.clone(), ContactsOpKind::Add),
        ContactsMutation::Change { server_id, .. } => (server_id.clone(), ContactsOpKind::Change),
        ContactsMutation::Delete { server_id } => (server_id.clone(), ContactsOpKind::Delete),
    };
    ContactsMutationResult {
        server_id,
        status,
        op_kind,
    }
}

// --------------------------------------------------------------------------
// JMAP mutation implementation
// --------------------------------------------------------------------------

async fn apply_mutation_jmap(
    state: &crate::models::AppState,
    username: &str,
    password: &str,
    mutation: &ContactsMutation,
) -> anyhow::Result<ContactsMutationResult> {
    let secret = secrecy::SecretString::from(password.to_string());
    let jmap = state
        .jmap_client
        .as_ref()
        .ok_or_else(|| anyhow!("JMAP client not configured"))?;

    match mutation {
        ContactsMutation::Add { vcard, .. } => {
            let v = vcard::parse_vcard_from_data(vcard).context("Add: unparsable vCard")?;
            // Upload photo blob (if inline) before creating the card.
            let photo_blob = if let Some(p) = v.photo().map(str::to_string) {
                match decode_photo_data(&p) {
                    Some((bytes, mime)) => {
                        let account = jmap.get_contacts_account_id(username, &secret).await?;
                        let blob = jmap
                            .upload_blob(&account, &bytes, Some("photo"), username, &secret)
                            .await?;
                        Some((blob, mime))
                    }
                    None => None,
                }
            } else {
                None
            };

            let uid = Uuid::new_v4().simple().to_string();
            let mut card = vcard_to_jscard(&v, &uid, photo_blob.as_ref().map(|(b, _)| b.as_str()));
            let book = jmap.get_default_address_book_id(username, &secret).await?;
            card["addressBookIds"] = serde_json::json!({ book: true });

            let (id, etag) = jmap.create_contact_card(username, &secret, &card).await?;
            let href = format!("jmap://contacts/{id}");
            let stored = v.with_photo_cleared();
            let server_id = format!("contact-{}", Uuid::new_v4().simple());
            if let Err(e) = state
                .storage
                .insert_contact(username, &href, &server_id, etag.as_deref(), Some(&stored))
                .await
            {
                tracing::warn!(error = %e, "Failed to store contact in DB after JMAP Add");
            }
            Ok(ContactsMutationResult {
                server_id,
                status: "1",
                op_kind: ContactsOpKind::Add,
            })
        }
        ContactsMutation::Change {
            server_id: sid,
            vcard,
            present,
        } => {
            let db_contact = state
                .storage
                .get_contact(username, sid)
                .await?
                .ok_or_else(|| anyhow!("Unknown contact {}", sid))?;

            let id = db_contact
                .carddav_href
                .strip_prefix("jmap://contacts/")
                .ok_or_else(|| anyhow!("Change on non-JMAP contact {}", db_contact.carddav_href))?;

            // Pull existing state and merge (Change is partial in EAS).
            let (cards, _) = jmap.list_contact_cards(username, &secret).await?;
            let found = cards
                .iter()
                .find(|c| c.id == id)
                .ok_or_else(|| anyhow!("JMAP contact {} not found", id))?;
            let old_v = jscard_to_vcard(&found.card, id);
            let new_v = vcard::parse_vcard_from_data(vcard).context("Change: unparsable vCard")?;
            let merged = merge_vcards(&old_v, &new_v, present);

            // Photo handling: if the client supplied a photo in the Change,
            // upload it; otherwise keep the existing blob reference.
            let photo_blob = if present.contains(K_PHOTO) {
                match merged.photo().and_then(decode_photo_data) {
                    Some((bytes, _mime)) => {
                        let account = jmap.get_contacts_account_id(username, &secret).await?;
                        Some(
                            jmap.upload_blob(&account, &bytes, Some("photo"), username, &secret)
                                .await?,
                        )
                    }
                    None => None,
                }
            } else {
                photo_blob_id(&found.card)
            };

            // Merge into a full replacement update with null-outs for removed keys.
            let mut card = vcard_to_jscard(&merged, id, photo_blob.as_deref());
            let existing_obj = found.card.as_object().cloned().unwrap_or_default();
            // Preserve server-side bookkeeping fields.
            if let Some(ab) = existing_obj.get("addressBookIds") {
                card["addressBookIds"] = ab.clone();
            }
            if let Some(uid_v) = existing_obj.get("uid").cloned() {
                card["uid"] = uid_v;
            }
            if let Some(kind) = existing_obj.get("kind").cloned() {
                card["kind"] = kind;
            }
            for key in [
                "name",
                "fullName",
                "emails",
                "phones",
                "addresses",
                "organizations",
                "titles",
                "note",
                "anniversaries",
                "nicknames",
                "media",
                "onlineServices",
                "personalInfo",
                "keywords",
                "keyboardText",
            ] {
                if card.get(key).is_none() && existing_obj.get(key).is_some() {
                    card[key] = serde_json::Value::Null;
                }
            }

            let etag = jmap
                .update_contact_card(username, &secret, id, &card)
                .await?;
            if let Err(e) = state
                .storage
                .update_contact(username, sid, etag.as_deref(), Some(&merged.to_string()))
                .await
            {
                tracing::warn!(error = %e, "Failed to update contact in DB after JMAP Change");
            }

            Ok(ContactsMutationResult {
                server_id: sid.clone(),
                status: "1",
                op_kind: ContactsOpKind::Change,
            })
        }
        ContactsMutation::Delete { server_id: sid } => {
            let db_contact = state
                .storage
                .get_contact(username, sid)
                .await?
                .ok_or_else(|| anyhow!("Unknown contact {}", sid))?;
            let id = db_contact
                .carddav_href
                .strip_prefix("jmap://contacts/")
                .ok_or_else(|| anyhow!("Delete on non-JMAP contact {}", db_contact.carddav_href))?;

            jmap.destroy_contact_card(username, &secret, id).await?;
            if let Err(e) = state.storage.delete_contact(username, sid).await {
                tracing::warn!(error = %e, "Failed to delete contact from DB after JMAP Delete");
            }
            Ok(ContactsMutationResult {
                server_id: sid.clone(),
                status: "1",
                op_kind: ContactsOpKind::Delete,
            })
        }
    }
}

// --------------------------------------------------------------------------
// CardDAV fallback mutation path
// --------------------------------------------------------------------------

async fn apply_mutation_carddav(
    state: &crate::models::AppState,
    username: &str,
    password: &str,
    mutation: &ContactsMutation,
) -> ContactsMutationResult {
    let carddav = match state.carddav_client.as_ref() {
        Some(c) => c,
        None => return mutation_failure(mutation, "6"),
    };

    match mutation {
        ContactsMutation::Add { vcard, .. } => {
            let response = match carddav
                .client
                .post(carddav.addressbook_home(username))
                .basic_auth(username, Some(password))
                .header("Content-Type", "text/vcard; charset=utf-8")
                .body(vcard.clone())
                .send()
                .await
            {
                Ok(resp) => resp,
                Err(_) => return mutation_failure(mutation, "6"),
            };

            if !response.status().is_success() {
                return mutation_failure(mutation, "6");
            }

            let location = response
                .headers()
                .get("Location")
                .and_then(|h| h.to_str().ok())
                .unwrap_or("")
                .to_string();
            let etag = response
                .headers()
                .get("ETag")
                .and_then(|h| h.to_str().ok())
                .map(|s| s.trim_matches('"').to_string());
            let server_id = format!("contact-{}", Uuid::new_v4().simple());
            if let Err(e) = state
                .storage
                .insert_contact(
                    username,
                    &location,
                    &server_id,
                    etag.as_deref(),
                    Some(vcard.as_str()),
                )
                .await
            {
                tracing::warn!(error = %e, "Failed to store contact in DB after CardDAV Add");
            }

            ContactsMutationResult {
                server_id,
                status: "1",
                op_kind: ContactsOpKind::Add,
            }
        }
        ContactsMutation::Change {
            server_id: sid,
            vcard,
            present,
        } => {
            let db_contact = match state.storage.get_contact(username, sid).await {
                Ok(Some(c)) => c,
                _ => return mutation_failure(mutation, "6"),
            };

            // Merge partial Change onto the stored server copy.
            let final_vcard = match (
                vcard::parse_vcard_from_data(db_contact.vcard.as_deref().unwrap_or_default()),
                vcard::parse_vcard_from_data(vcard),
            ) {
                (Ok(old), Ok(new)) => merge_vcards(&old, &new, present).to_string(),
                _ => vcard.clone(),
            };

            let url = format!(
                "{}{}",
                carddav.addressbook_home(username),
                db_contact.carddav_href
            );
            let mut req = carddav
                .client
                .put(&url)
                .basic_auth(username, Some(password))
                .header("Content-Type", "text/vcard; charset=utf-8")
                .body(final_vcard.clone());
            if let Some(ref etag) = db_contact.etag {
                req = req.header("If-Match", format!("\"{}\"", etag));
            }

            let response = match req.send().await {
                Ok(resp) => resp,
                Err(_) => return mutation_failure(mutation, "6"),
            };

            if response.status().is_success() {
                let new_etag = response
                    .headers()
                    .get("ETag")
                    .and_then(|h| h.to_str().ok())
                    .map(|s| s.trim_matches('"').to_string());
                if let Err(e) = state
                    .storage
                    .update_contact(
                        username,
                        sid,
                        new_etag.as_deref(),
                        Some(final_vcard.as_str()),
                    )
                    .await
                {
                    tracing::warn!(error = %e, "Failed to update contact in DB for CardDAV Change");
                }
                ContactsMutationResult {
                    server_id: sid.clone(),
                    status: "1",
                    op_kind: ContactsOpKind::Change,
                }
            } else if response.status() == StatusCode::PRECONDITION_FAILED {
                ContactsMutationResult {
                    server_id: sid.clone(),
                    status: "5",
                    op_kind: ContactsOpKind::Change,
                }
            } else {
                mutation_failure(mutation, "6")
            }
        }
        ContactsMutation::Delete { server_id: sid } => {
            let db_contact = match state.storage.get_contact(username, sid).await {
                Ok(Some(c)) => c,
                _ => return mutation_failure(mutation, "6"),
            };

            let url = format!(
                "{}{}",
                carddav.addressbook_home(username),
                db_contact.carddav_href
            );
            let mut req = carddav
                .client
                .delete(&url)
                .basic_auth(username, Some(password));
            if let Some(ref etag) = db_contact.etag {
                req = req.header("If-Match", format!("\"{}\"", etag));
            }

            let response = match req.send().await {
                Ok(resp) => resp,
                Err(_) => return mutation_failure(mutation, "6"),
            };

            if response.status().is_success() {
                if let Err(e) = state.storage.delete_contact(username, sid).await {
                    tracing::warn!(error = %e, "Failed to delete contact from DB after CardDAV Delete");
                }
                ContactsMutationResult {
                    server_id: sid.clone(),
                    status: "1",
                    op_kind: ContactsOpKind::Delete,
                }
            } else if response.status() == StatusCode::PRECONDITION_FAILED {
                ContactsMutationResult {
                    server_id: sid.clone(),
                    status: "5",
                    op_kind: ContactsOpKind::Delete,
                }
            } else {
                mutation_failure(mutation, "6")
            }
        }
    }
}

// --------------------------------------------------------------------------
// Result rendering / small helpers
// --------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct ContactsMutationResult {
    pub server_id: String,
    pub status: &'static str,
    pub op_kind: ContactsOpKind,
}

/// Render the mutation responses for the EAS Sync response body.
pub fn render_contacts_mutation_responses(results: &[ContactsMutationResult]) -> String {
    let mut xml = String::new();
    for res in results {
        match res.op_kind {
            ContactsOpKind::Add => {
                xml.push_str(&format!(
                    r#"<Add><ServerId>{}</ServerId><Status>{}</Status></Add>"#,
                    xml_escape(&res.server_id),
                    xml_escape(res.status)
                ));
            }
            ContactsOpKind::Change => {
                xml.push_str(&format!(
                    r#"<Change><ServerId>{}</ServerId><Status>{}</Status></Change>"#,
                    xml_escape(&res.server_id),
                    xml_escape(res.status)
                ));
            }
            ContactsOpKind::Delete => {
                xml.push_str(&format!(
                    r#"<Delete><ServerId>{}</ServerId><Status>{}</Status></Delete>"#,
                    xml_escape(&res.server_id),
                    xml_escape(res.status)
                ));
            }
        }
    }
    xml
}

/// Return the blob id of a photo media entry from a JSCard object.
fn photo_blob_id(card: &serde_json::Value) -> Option<String> {
    card.get("media")
        .and_then(|m| m.as_object())?
        .values()
        .find(|m| m.get("kind").and_then(|k| k.as_str()) == Some("photo"))
        .and_then(|m| m.get("blobId"))
        .and_then(|b| b.as_str())
        .map(|s| s.to_string())
}

/// Decode a photo value ("data:image/...;base64,..." or raw base64) into
/// raw bytes plus a best-guess MIME type.
fn decode_photo_data(photo: &str) -> Option<(Vec<u8>, String)> {
    if let Some(rest) = photo.strip_prefix("data:") {
        let (meta, b64) = rest.split_once(',')?;
        let mimes: Vec<&str> = meta.split(';').collect();
        let mime = mimes.first().unwrap_or(&"image/jpeg").to_string();
        let bytes = base64::engine::general_purpose::STANDARD.decode(b64).ok()?;
        return Some((bytes, mime));
    }
    if photo.starts_with("http://") || photo.starts_with("https://") {
        return None;
    }
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(photo)
        .ok()?;
    Some((bytes, "image/jpeg".to_string()))
}

/// Small convenience: build a copy of the vCard with the photo removed.
trait VcardPhotoClear {
    fn with_photo_cleared(&self) -> String;
}

impl VcardPhotoClear for Vcard {
    fn with_photo_cleared(&self) -> String {
        let mut v = self.clone();
        v.properties.retain(|p| !matches!(p, Property::Photo(_)));
        v.to_string()
    }
}

// --------------------------------------------------------------------------
// HTML/XML escaping helpers (kept local to this module)
// --------------------------------------------------------------------------

/// xml_escape used above.
fn xml_escape(s: &str) -> String {
    crate::util::escape_xml_text(s).to_string()
}

// --------------------------------------------------------------------------
// Tests
// --------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_carddav(vcard_body: &str) -> CarddavContact {
        CarddavContact {
            href: "u.vcf".into(),
            etag: Some("etag-1".into()),
            vcard: vcard_body.into(),
        }
    }

    #[test]
    fn test_render_eas_contact_structured_name() {
        let v = vcard::parse_vcard_from_data(
            "BEGIN:VCARD\r\nVERSION:3.0\r\nN:Doe;Jane;Ann;Dr.;III\r\nFN:Dr. Jane Ann Doe III\r\nUID:x\r\nEND:VCARD\r\n",
        ).unwrap();
        let xml = render_eas_contact("1", &sample_carddav(&v.to_string()));
        assert!(xml.contains("<Contacts:FirstName>Jane</Contacts:FirstName>"));
        assert!(xml.contains("<Contacts:MiddleName>Ann</Contacts:MiddleName>"));
        assert!(xml.contains("<Contacts:LastName>Doe</Contacts:LastName>"));
        assert!(xml.contains("<Contacts:Suffix>III</Contacts:Suffix>"));
        assert!(xml.contains("<Contacts:Title>Dr.</Contacts:Title>"));
    }

    #[test]
    fn test_render_eas_contact_addresses_phones_emails() {
        let v = vcard::parse_vcard_from_data(
            "BEGIN:VCARD\r\nVERSION:3.0\r\nFN:X\r\nN:;X;;;;\r\nADR;TYPE=HOME:;;1 Home St;HomeTown;HS;12345;HCountry\r\nADR;TYPE=WORK:;;9 Work Rd;JobTown;JS;99999;WCountry\r\nTEL;TYPE=CELL:+100\r\nTEL;TYPE=WORK;TYPE=VOICE:+200\r\nTEL;TYPE=HOME;TYPE=VOICE:+300\r\nTEL;TYPE=FAX,WORK:+400\r\nEMAIL;TYPE=WORK:a@w\r\nEMAIL;TYPE=HOME:b@h\r\nEND:VCARD\r\n",
        ).unwrap();
        let xml = render_eas_contact("1", &sample_carddav(&v.to_string()));
        assert!(xml.contains("<Contacts:HomeAddressStreet>1 Home St</Contacts:HomeAddressStreet>"));
        assert!(
            xml.contains("<Contacts:BusinessAddressCity>JobTown</Contacts:BusinessAddressCity>")
        );
        assert!(xml.contains("<Contacts:MobilePhoneNumber>+100</Contacts:MobilePhoneNumber>"));
        assert!(xml.contains("<Contacts:BusinessPhoneNumber>+200</Contacts:BusinessPhoneNumber>"));
        assert!(xml.contains("<Contacts:HomePhoneNumber>+300</Contacts:HomePhoneNumber>"));
        assert!(xml.contains("<Contacts:BusinessFaxNumber>+400</Contacts:BusinessFaxNumber>"));
        assert!(xml.contains("<Contacts:Email1Address>a@w</Contacts:Email1Address>"));
        assert!(xml.contains("<Contacts:Email2Address>b@h</Contacts:Email2Address>"));
    }

    #[test]
    fn test_render_eas_contact_misc_fields() {
        let v = vcard::parse_vcard_from_data(
            "BEGIN:VCARD\r\nVERSION:3.0\r\nFN:X\r\nN:;X;;;;\r\nNOTE:Line1\\nLine2\r\nBDAY:19700131\r\nANNIVERSARY:19950604\r\nPHOTO;ENCODING=b;TYPE=JPEG:QUJD\r\nURL:https://x\r\nNICKNAME:Bobby\r\nX-AS-SPOUSE:Spousey\r\nX-AS-CHILD:KidOne\r\nEND:VCARD\r\n",
        ).unwrap();
        let xml = render_eas_contact("1", &sample_carddav(&v.to_string()));
        assert!(xml.contains("<AirSyncBase:Body>"));
        assert!(xml.contains("Line1\nLine2"));
        assert!(xml.contains("<Contacts:Birthday>1970-01-31T00:00:00.000Z</Contacts:Birthday>"));
        assert!(xml.contains("<Contacts:Picture>QUJD</Contacts:Picture>"));
        assert!(xml.contains("<Contacts:WebPage>https://x</Contacts:WebPage>"));
        assert!(xml.contains("<Contacts2:NickName>Bobby</Contacts2:NickName>"));
        assert!(xml.contains("<Contacts:Spouse>Spousey</Contacts:Spouse>"));
        assert!(xml.contains("<Contacts:Child>KidOne</Contacts:Child>"));
    }

    #[test]
    fn test_parse_eas_add_to_vcard() {
        let xml = "<Add><ServerId></ServerId><ClientId>42</ClientId><ApplicationData>\
            <Contacts:FirstName>Jane</Contacts:FirstName><Contacts:LastName>Doe</Contacts:LastName>\
            <Contacts:Email1Address>jane@ex</Contacts:Email1Address>\
            <Contacts:MobilePhoneNumber>+1</Contacts:MobilePhoneNumber>\
            <Contacts:BusinessAddressCity>Metropolis</Contacts:BusinessAddressCity>\
            <Contacts:Birthday>1970-01-31T00:00:00.000Z</Contacts:Birthday>\
            <Contacts:CompanyName>Acme</Contacts:CompanyName><Contacts:Department>RD</Contacts:Department>\
            <Contacts:Spouse>John</Contacts:Spouse>\
            <Contacts:Children><Contacts:Child>A</Contacts:Child></Contacts:Children>\
            </ApplicationData></Add>";
        let muts = parse_contacts_mutations(xml).unwrap();
        assert_eq!(muts.len(), 1);
        let ContactsMutation::Add { vcard: vtxt, .. } = &muts[0] else {
            panic!("expected Add");
        };
        let v = vcard::parse_vcard_from_data(vtxt).unwrap();
        assert_eq!(v.structured_name().unwrap().given, "Jane");
        assert_eq!(v.emails(), vec!["jane@ex"]);
        assert!(v.typed_phones().iter().any(|t| t.has_type("cell")));
        assert_eq!(v.addresses()[0].locality, "Metropolis");
        assert_eq!(v.bday().unwrap(), "19700131");
        assert_eq!(v.spouse(), Some("John"));
        assert_eq!(v.children(), ["A"]);
        assert_eq!(v.org().unwrap()[0], "Acme");
        assert_eq!(v.department().unwrap(), "RD");
    }

    #[test]
    fn test_merge_partial_change() {
        let old = vcard::parse_vcard_from_data(
            "BEGIN:VCARD\r\nVERSION:3.0\r\nN:Old;Name;;;\r\nEMAIL:old@e\r\nTEL:+111\r\nEND:VCARD\r\n",
        ).unwrap();
        let new = vcard::parse_vcard_from_data(
            "BEGIN:VCARD\r\nVERSION:3.0\r\nN:;New;;;;\r\nEND:VCARD\r\n",
        )
        .unwrap();
        let present: HashSet<String> = [K_NAME.to_string()].into_iter().collect();
        let merged = merge_vcards(&old, &new, &present);
        assert_eq!(merged.structured_name().unwrap().given, "New");
        assert!(merged.structured_name().unwrap().family.is_empty());
        assert_eq!(merged.emails(), vec!["old@e"]);
        assert_eq!(merged.phones(), vec!["+111"]);
    }

    #[test]
    fn test_legacy_vcard_passthrough() {
        let xml = "<Add><ServerId>x</ServerId><vCard>BEGIN:VCARD\r\nVERSION:3.0\r\nFN:Y\r\nEND:VCARD\r\n</vCard></Add>";
        let muts = parse_contacts_mutations(xml).unwrap();
        assert_eq!(muts.len(), 1);
        assert!(matches!(&muts[0], ContactsMutation::Add { .. }));
    }
}
