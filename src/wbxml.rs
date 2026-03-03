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
        // AirSync (0) – per MS-ASWBXML spec
        add!(CP_AIRSYNC, 0x05, "Sync", true);
        add!(CP_AIRSYNC, 0x06, "Responses", true);
        add!(CP_AIRSYNC, 0x07, "Add", true);
        add!(CP_AIRSYNC, 0x08, "Change", true);
        add!(CP_AIRSYNC, 0x09, "Delete", true);
        add!(CP_AIRSYNC, 0x0A, "Fetch", true);
        add!(CP_AIRSYNC, 0x0B, "SyncKey", true);
        add!(CP_AIRSYNC, 0x0C, "ClientId", true);
        add!(CP_AIRSYNC, 0x0D, "ServerId", true);
        add!(CP_AIRSYNC, 0x0E, "Status", true);
        add!(CP_AIRSYNC, 0x0F, "Collection", true);
        add!(CP_AIRSYNC, 0x10, "Class", true);
        add!(CP_AIRSYNC, 0x12, "CollectionId", true);
        add!(CP_AIRSYNC, 0x13, "GetChanges", true);
        add!(CP_AIRSYNC, 0x14, "MoreAvailable", true);
        add!(CP_AIRSYNC, 0x15, "WindowSize", true);
        add!(CP_AIRSYNC, 0x16, "Commands", true);
        add!(CP_AIRSYNC, 0x17, "Options", true);
        add!(CP_AIRSYNC, 0x18, "FilterType", true);
        add!(CP_AIRSYNC, 0x19, "Truncation", true);
        add!(CP_AIRSYNC, 0x1B, "Conflict", true);
        add!(CP_AIRSYNC, 0x1C, "Collections", true);
        add!(CP_AIRSYNC, 0x1D, "ApplicationData", true);
        add!(CP_AIRSYNC, 0x1E, "DeletesAsMoves", true);
        add!(CP_AIRSYNC, 0x20, "Supported", true);
        add!(CP_AIRSYNC, 0x21, "SoftDelete", true);
        add!(CP_AIRSYNC, 0x22, "MIMESupport", true);
        add!(CP_AIRSYNC, 0x23, "MIMETruncation", true);
        add!(CP_AIRSYNC, 0x24, "Wait", true);
        add!(CP_AIRSYNC, 0x25, "Limit", true);
        add!(CP_AIRSYNC, 0x26, "Partial", true);
        add!(CP_AIRSYNC, 0x27, "ConversationMode", true);
        add!(CP_AIRSYNC, 0x28, "MaxItems", true);
        add!(CP_AIRSYNC, 0x29, "HeartbeatInterval", true);

        // Calendar (4) – per MS-ASWBXML spec
        add!(CP_CALENDAR, 0x05, "Timezone", true);
        add!(CP_CALENDAR, 0x06, "AllDayEvent", true);
        add!(CP_CALENDAR, 0x07, "Attendees", true);
        add!(CP_CALENDAR, 0x08, "Attendee", true);
        add!(CP_CALENDAR, 0x09, "Email", true);
        add!(CP_CALENDAR, 0x0A, "Name", true);
        add!(CP_CALENDAR, 0x0D, "BusyStatus", true);
        add!(CP_CALENDAR, 0x0E, "Categories", true);
        add!(CP_CALENDAR, 0x0F, "Category", true);
        add!(CP_CALENDAR, 0x11, "DtStamp", true);
        add!(CP_CALENDAR, 0x12, "EndTime", true);
        add!(CP_CALENDAR, 0x13, "Exception", true);
        add!(CP_CALENDAR, 0x14, "Exceptions", true);
        add!(CP_CALENDAR, 0x15, "Deleted", true);
        add!(CP_CALENDAR, 0x16, "ExceptionStartTime", true);
        add!(CP_CALENDAR, 0x17, "Location", true);
        add!(CP_CALENDAR, 0x18, "MeetingStatus", true);
        add!(CP_CALENDAR, 0x19, "OrganizerEmail", true);
        add!(CP_CALENDAR, 0x1A, "OrganizerName", true);
        add!(CP_CALENDAR, 0x1B, "Recurrence", true);
        add!(CP_CALENDAR, 0x1C, "Type", true);
        add!(CP_CALENDAR, 0x1D, "Until", true);
        add!(CP_CALENDAR, 0x1E, "Occurrences", true);
        add!(CP_CALENDAR, 0x1F, "Interval", true);
        add!(CP_CALENDAR, 0x20, "DayOfWeek", true);
        add!(CP_CALENDAR, 0x21, "DayOfMonth", true);
        add!(CP_CALENDAR, 0x22, "WeekOfMonth", true);
        add!(CP_CALENDAR, 0x23, "MonthOfYear", true);
        add!(CP_CALENDAR, 0x24, "Reminder", true);
        add!(CP_CALENDAR, 0x25, "Sensitivity", true);
        add!(CP_CALENDAR, 0x26, "Subject", true);
        add!(CP_CALENDAR, 0x27, "StartTime", true);
        add!(CP_CALENDAR, 0x28, "UID", true);
        add!(CP_CALENDAR, 0x29, "AttendeeStatus", true);
        add!(CP_CALENDAR, 0x2A, "AttendeeType", true);
        add!(CP_CALENDAR, 0x33, "DisallowNewTimeProposal", true);
        add!(CP_CALENDAR, 0x34, "ResponseRequested", true);

        // AirSyncBase (17) – per MS-ASWBXML spec
        add!(CP_AIRSYNCBASE, 0x05, "BodyPreference", true);
        add!(CP_AIRSYNCBASE, 0x06, "Type", true);
        add!(CP_AIRSYNCBASE, 0x07, "TruncationSize", true);
        add!(CP_AIRSYNCBASE, 0x08, "AllOrNone", true);
        add!(CP_AIRSYNCBASE, 0x0A, "Body", true);
        add!(CP_AIRSYNCBASE, 0x0B, "Data", true);
        add!(CP_AIRSYNCBASE, 0x0C, "EstimatedDataSize", true);
        add!(CP_AIRSYNCBASE, 0x0D, "Truncated", true);
        add!(CP_AIRSYNCBASE, 0x0E, "Attachments", true);
        add!(CP_AIRSYNCBASE, 0x0F, "Attachment", true);
        add!(CP_AIRSYNCBASE, 0x10, "DisplayName", true);
        add!(CP_AIRSYNCBASE, 0x11, "FileReference", true);
        add!(CP_AIRSYNCBASE, 0x12, "Method", true);
        add!(CP_AIRSYNCBASE, 0x13, "ContentId", true);
        add!(CP_AIRSYNCBASE, 0x14, "ContentLocation", true);
        add!(CP_AIRSYNCBASE, 0x15, "IsInline", true);
        add!(CP_AIRSYNCBASE, 0x16, "NativeBodyType", true);
        add!(CP_AIRSYNCBASE, 0x17, "ContentType", true);
        add!(CP_AIRSYNCBASE, 0x18, "Preview", true);
        add!(CP_AIRSYNCBASE, 0x19, "BodyPartPreference", true);
        add!(CP_AIRSYNCBASE, 0x1A, "BodyPart", true);
        add!(CP_AIRSYNCBASE, 0x1B, "Status", true);

        // Settings (18) – per MS-ASWBXML spec
        add!(CP_SETTINGS, 0x05, "Settings", true);
        add!(CP_SETTINGS, 0x06, "Status", true);
        add!(CP_SETTINGS, 0x16, "DeviceInformation", true);
        add!(CP_SETTINGS, 0x19, "FriendlyName", true);

        // ItemOperations (20) – per MS-ASWBXML spec
        add!(CP_ITEMOPERATIONS, 0x05, "ItemOperations", true);
        add!(CP_ITEMOPERATIONS, 0x06, "Fetch", true);
        add!(CP_ITEMOPERATIONS, 0x07, "Store", true);
        add!(CP_ITEMOPERATIONS, 0x0D, "Status", true);
        add!(CP_ITEMOPERATIONS, 0x0E, "Response", true);

        // Search (15) – per MS-ASWBXML spec
        add!(CP_SEARCH, 0x05, "Search", true);
        add!(CP_SEARCH, 0x07, "Store", true);
        add!(CP_SEARCH, 0x08, "Name", true);
        add!(CP_SEARCH, 0x09, "Query", true);
        add!(CP_SEARCH, 0x0A, "Options", true);
        add!(CP_SEARCH, 0x0B, "Range", true);
        add!(CP_SEARCH, 0x0C, "Status", true);
        add!(CP_SEARCH, 0x0D, "Response", true);
        add!(CP_SEARCH, 0x0E, "Result", true);
        add!(CP_SEARCH, 0x0F, "Properties", true);
        add!(CP_SEARCH, 0x10, "Total", true);
        add!(CP_SEARCH, 0x15, "FreeText", true);

        // Provision (14) – per MS-ASWBXML spec
        add!(CP_PROVISION, 0x05, "Provision", true);
        add!(CP_PROVISION, 0x06, "Policies", true);
        add!(CP_PROVISION, 0x07, "Policy", true);
        add!(CP_PROVISION, 0x08, "PolicyType", true);
        add!(CP_PROVISION, 0x09, "PolicyKey", true);
        add!(CP_PROVISION, 0x0A, "Data", true);
        add!(CP_PROVISION, 0x0B, "Status", true);
        add!(CP_PROVISION, 0x0C, "RemoteWipe", true);
        add!(CP_PROVISION, 0x0D, "EASProvisionDoc", true);
        add!(CP_PROVISION, 0x0E, "DevicePasswordEnabled", true);
        add!(CP_PROVISION, 0x10, "RequireStorageCardEncryption", true);
        add!(CP_PROVISION, 0x1D, "RequireDeviceEncryption", true);

        // Ping (13) – per MS-ASWBXML spec
        add!(CP_PING, 0x05, "Ping", true);
        add!(CP_PING, 0x07, "Status", true);
        add!(CP_PING, 0x08, "HeartbeatInterval", true);
        add!(CP_PING, 0x09, "Folders", true);
        add!(CP_PING, 0x0A, "Folder", true);
        add!(CP_PING, 0x0B, "Id", true);
        add!(CP_PING, 0x0C, "Class", true);
        add!(CP_PING, 0x0D, "MaxFolders", true);

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
    if data[0] != 0x03 || data[2] != 0x6A || data[3] != 0x00 {
        return Err("Invalid WBXML header".into());
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
            } else {
                return Err("Unexpected end in inline string".into());
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
            let end = pos.checked_add(len).ok_or_else(|| "Opaque overflow".to_string())?;
            if end > data.len() {
                return Err("Opaque overflow".into());
            }
            let content = &data[pos..end];
            pos = end;

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

    loop {
        buf.clear();
        match reader.read_event_into(&mut buf) {
            Ok(quick_xml::events::Event::Start(ref e)) => {
                let local_name = e.local_name();
                let name = String::from_utf8_lossy(local_name.as_ref());
                if !encode_tag(&mut output, &name, &mut current_page, true) {
                    return Err(format!("Unknown WBXML tag: {}", name));
                }
            }
            Ok(quick_xml::events::Event::Empty(ref e)) => {
                let local_name = e.local_name();
                let name = String::from_utf8_lossy(local_name.as_ref());
                if !encode_tag(&mut output, &name, &mut current_page, false) {
                    return Err(format!("Unknown WBXML tag: {}", name));
                }
            }
            Ok(quick_xml::events::Event::End(_)) => {
                output.push(TAG_END);
            }
            Ok(quick_xml::events::Event::Text(ref e)) => {
                output.push(TAG_STR_I);
                let text_str = std::str::from_utf8(e.as_ref())
                    .map_err(|_| "Invalid UTF-8 in XML text node".to_string())?;
                let t = quick_xml::escape::unescape(text_str)
                    .map_err(|e| format!("XML text unescape error: {}", e))?;
                output.extend(t.as_bytes());
                output.push(0x00);
            }
            Ok(quick_xml::events::Event::Eof) => break,
            Ok(_) => {}
            Err(e) => return Err(format!("XML parsing error: {}", e)),
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
