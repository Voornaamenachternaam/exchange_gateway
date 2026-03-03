use lazy_static::lazy_static;
use std::collections::HashMap;

const TAG_SWITCH_PAGE: u8 = 0x00;
const TAG_END: u8 = 0x01;
const TAG_STR_I: u8 = 0x03;
const TAG_OPAQUE: u8 = 0xC3;

const CP_AIRSYNC: u8 = 0;
const CP_CALENDAR: u8 = 4;
const CP_AIRSYNCBASE: u8 = 17;
const CP_SETTINGS: u8 = 18;
const CP_ITEMOPERATIONS: u8 = 20;
const CP_SEARCH: u8 = 15;
const CP_PROVISION: u8 = 14;
const CP_PING: u8 = 13;

#[derive(Debug, Clone)]
struct Tag {
    name: &'static str,
    _has_content: bool,
}

lazy_static! {
    static ref TAG_MAP: HashMap<(u8, u8), Tag> = {
        let mut m = HashMap::new();
        macro_rules! add {
            ($page:expr, $token:expr, $name:expr, $content:expr) => {
                m.insert(($page, $token), Tag { name: $name, _has_content: $content });
            };
        }
        // AirSync (0)
        add!(CP_AIRSYNC, 0x05, "Sync", true);
        add!(CP_AIRSYNC, 0x06, "Collections", true);
        add!(CP_AIRSYNC, 0x07, "Collection", true);
        add!(CP_AIRSYNC, 0x09, "SyncKey", true);
        add!(CP_AIRSYNC, 0x0A, "CollectionId", true);
        add!(CP_AIRSYNC, 0x0B, "Status", true);
        add!(CP_AIRSYNC, 0x0C, "Commands", true);
        add!(CP_AIRSYNC, 0x0E, "Add", true);
        add!(CP_AIRSYNC, 0x0F, "Change", true);
        add!(CP_AIRSYNC, 0x10, "Delete", true);
        add!(CP_AIRSYNC, 0x12, "ServerId", true);
        add!(CP_AIRSYNC, 0x13, "ClientId", true);
        add!(CP_AIRSYNC, 0x16, "SendMail", true);
        add!(CP_AIRSYNC, 0x18, "Options", true);
        add!(CP_AIRSYNC, 0x24, "ApplicationData", true);

        // Calendar (4)
        add!(CP_CALENDAR, 0x05, "Timezone", true);
        add!(CP_CALENDAR, 0x06, "AllDayEvent", true);
        add!(CP_CALENDAR, 0x07, "Attendees", true);
        add!(CP_CALENDAR, 0x08, "Attendee", true);
        add!(CP_CALENDAR, 0x09, "Email", true);
        add!(CP_CALENDAR, 0x0A, "Name", true);
        add!(CP_CALENDAR, 0x0D, "Location", true);
        add!(CP_CALENDAR, 0x11, "Recurrence", true);
        add!(CP_CALENDAR, 0x12, "Type", true);
        add!(CP_CALENDAR, 0x13, "Interval", true);
        add!(CP_CALENDAR, 0x17, "DayOfWeek", true);
        add!(CP_CALENDAR, 0x1A, "StartTime", true);
        add!(CP_CALENDAR, 0x1B, "EndTime", true);
        add!(CP_CALENDAR, 0x1D, "Subject", true);
        add!(CP_CALENDAR, 0x1E, "UID", true);
        add!(CP_CALENDAR, 0x20, "Reminder", true);

        // AirSyncBase (17)
        add!(CP_AIRSYNCBASE, 0x05, "BodyPreference", true);
        add!(CP_AIRSYNCBASE, 0x06, "Type", true);
        add!(CP_AIRSYNCBASE, 0x07, "TruncationSize", true);
        add!(CP_AIRSYNCBASE, 0x08, "AllOrNone", true);
        add!(CP_AIRSYNCBASE, 0x0A, "Body", true);
        add!(CP_AIRSYNCBASE, 0x0B, "Data", true);
        add!(CP_AIRSYNCBASE, 0x0C, "EstimatedDataSize", true);

        // Settings (18)
        add!(CP_SETTINGS, 0x05, "Settings", true);
        add!(CP_SETTINGS, 0x13, "DeviceInformation", true);

        // ItemOperations (10)
        add!(CP_ITEMOPERATIONS, 0x05, "ItemOperations", true);
        add!(CP_ITEMOPERATIONS, 0x06, "Fetch", true);
        add!(CP_ITEMOPERATIONS, 0x07, "Store", true);

        // Search (11)
        add!(CP_SEARCH, 0x05, "Search", true);
        add!(CP_SEARCH, 0x06, "Store", true);
        add!(CP_SEARCH, 0x08, "Query", true);
        add!(CP_SEARCH, 0x0E, "Properties", true);

        // Provision (12)
        add!(CP_PROVISION, 0x05, "Provision", true);
        add!(CP_PROVISION, 0x09, "PolicyKey", true);

        // Ping (13)
        add!(CP_PING, 0x05, "Ping", true);

        m
    };

    static ref NAME_MAP: HashMap<&'static str, Vec<(u8, u8)>> = {
        let mut m: HashMap<&'static str, Vec<(u8, u8)>> = HashMap::new();
        for ((page, token), tag) in TAG_MAP.iter() {
            m.entry(tag.name).or_default().push((*page, *token));
        }
        for entries in m.values_mut() {
            entries.sort();
        }
        m
    };
}

pub fn decode(data: &[u8]) -> Result<String, String> {
    if data.len() < 4 {
        return Err("Data too short".into());
    }
    let mut pos = 4;
    let mut current_page = 0;
    let mut xml = String::new();
    let mut stack: Vec<String> = Vec::new();
    let mut pending_tag: Option<String> = None;

    while pos < data.len() {
        let token = data[pos];
        pos += 1;

        if token == TAG_SWITCH_PAGE {
            if pos >= data.len() {
                return Err("Unexpected end".into());
            }
            current_page = data[pos];
            pos += 1;
            continue;
        }

        if token == TAG_END {
            if pending_tag.is_some() {
                xml.push_str("/>");
                pending_tag = None;
            } else if let Some(tag) = stack.pop() {
                xml.push_str(&format!("</{}>", tag));
            }
            continue;
        }

        if token == TAG_STR_I {
            let mut str_buf = Vec::new();
            while pos < data.len() && data[pos] != 0 {
                str_buf.push(data[pos]);
                pos += 1;
            }
            if pos < data.len() {
                pos += 1;
            }

            if let Some(tag) = pending_tag.take() {
                xml.push('>');
                stack.push(tag);
            }
            let text = String::from_utf8_lossy(&str_buf);
            xml.push_str(
                &text
                    .replace('&', "&amp;")
                    .replace('<', "&lt;")
                    .replace('>', "&gt;"),
            );
            continue;
        }

        if token == TAG_OPAQUE {
            let mut len = 0;
            loop {
                if pos >= data.len() {
                    return Err("Unexpected end opaque".into());
                }
                let byte = data[pos];
                pos += 1;
                len = (len << 7) | ((byte & 0x7F) as usize);
                if (byte & 0x80) == 0 {
                    break;
                }
            }
            if pos + len > data.len() {
                return Err("Opaque overflow".into());
            }
            let content = &data[pos..pos + len];
            pos += len;

            if let Some(tag) = pending_tag.take() {
                xml.push('>');
                stack.push(tag);
            }
            let text = String::from_utf8_lossy(content);
            xml.push_str(&text.replace('&', "&amp;").replace('<', "&lt;"));
            continue;
        }

        let has_content = (token & 0x40) != 0;
        let token_id = token & 0x3F;

        if let Some(tag_def) = TAG_MAP.get(&(current_page, token_id)) {
            if pending_tag.is_some() {
                xml.push('>');
                stack.push(pending_tag.take().unwrap());
            }

            if has_content {
                pending_tag = Some(tag_def.name.to_string());
                xml.push_str(&format!("<{}", tag_def.name));
            } else {
                xml.push_str(&format!("<{}/>", tag_def.name));
            }
        } else if has_content {
            // Unknown tag with content: push a sentinel so the matching
            // TAG_END consumes it instead of incorrectly closing a parent.
            let placeholder = format!("Unknown_{}_{:02X}", current_page, token_id);
            if pending_tag.is_some() {
                xml.push('>');
                stack.push(pending_tag.take().unwrap());
            }
            pending_tag = Some(placeholder.clone());
            xml.push_str(&format!("<{}", placeholder));
        }
    }
    Ok(xml)
}

pub fn encode(xml: &str) -> Result<Vec<u8>, String> {
    let mut reader = quick_xml::Reader::from_str(xml);
    let mut buf = Vec::new();
    let mut output = vec![0x03, 0x01, 0x6A, 0x00];
    let mut current_page = 0;
    let mut skip_depth: usize = 0;

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(quick_xml::events::Event::Start(ref e)) => {
                if skip_depth > 0 {
                    skip_depth += 1;
                    continue;
                }
                let local_name = e.local_name();
                let name = String::from_utf8_lossy(local_name.as_ref());
                if !encode_tag(&mut output, &name, &mut current_page, true) {
                    skip_depth = 1;
                }
            }
            Ok(quick_xml::events::Event::Empty(ref e)) => {
                if skip_depth > 0 {
                    continue;
                }
                let local_name = e.local_name();
                let name = String::from_utf8_lossy(local_name.as_ref());
                encode_tag(&mut output, &name, &mut current_page, false);
            }
            Ok(quick_xml::events::Event::End(_)) => {
                if skip_depth > 0 {
                    skip_depth -= 1;
                    continue;
                }
                output.push(TAG_END);
            }
            Ok(quick_xml::events::Event::Text(ref e)) => {
                if skip_depth > 0 {
                    continue;
                }
                output.push(TAG_STR_I);
                let text_str = std::str::from_utf8(e.as_ref()).unwrap_or("");
                let t = quick_xml::escape::unescape(text_str).unwrap_or_default();
                output.extend(t.as_bytes());
                output.push(0x00);
            }
            Ok(quick_xml::events::Event::Eof) => break,
            _ => {}
        }
    }
    Ok(output)
}

fn encode_tag(output: &mut Vec<u8>, name: &str, current_page: &mut u8, has_content: bool) -> bool {
    if let Some(entries) = NAME_MAP.get(name) {
        // Prefer the entry on the current page to avoid unnecessary page switches
        // and to correctly resolve ambiguous tag names (e.g., "Type", "Store").
        let (page, token) = entries
            .iter()
            .find(|(p, _)| *p == *current_page)
            .unwrap_or(&entries[0]);
        if *page != *current_page {
            output.push(TAG_SWITCH_PAGE);
            output.push(*page);
            *current_page = *page;
        }
        let mut final_token = *token;
        if has_content {
            final_token |= 0x40;
        }
        output.push(final_token);
        true
    } else {
        false
    }
}
