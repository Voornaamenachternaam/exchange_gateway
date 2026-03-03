use lazy_static::lazy_static;
use std::collections::HashMap;

const TAG_SWITCH_PAGE: u8 = 0x00;
const TAG_END: u8 = 0x01;
const TAG_STR_I: u8 = 0x03;
const TAG_OPAQUE: u8 = 0xC3;

const MAX_DECODE_DEPTH: usize = 256;

/// Validate that `name` is a legal XML element name (simplified XML 1.0 Name production).
/// Rejects empty strings and strings containing characters that could break XML structure.
fn is_valid_xml_name(name: &str) -> bool {
    let mut chars = name.chars();
    match chars.next() {
        None => return false,
        Some(c) if !(c.is_ascii_alphabetic() || c == '_' || c == ':' || !c.is_ascii()) => {
            return false;
        }
        _ => {}
    }
    chars.all(|c| {
        c.is_ascii_alphanumeric()
            || c == '_'
            || c == ':'
            || c == '-'
            || c == '.'
            || !c.is_ascii()
    })
}

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
    if data[0] != 0x03 || data[2] != 0x6A {
        return Err("Invalid WBXML header".into());
    }

    // Read string table length (mb_u_int32)
    let mut pos = 3;
    let mut strtbl_len: usize = 0;
    loop {
        if pos >= data.len() {
            return Err("Unexpected end reading string table length".into());
        }
        let byte = data[pos];
        pos += 1;
        strtbl_len = strtbl_len
            .checked_shl(7)
            .and_then(|v| v.checked_add((byte & 0x7F) as usize))
            .ok_or_else(|| "String table length overflow".to_string())?;
        if (byte & 0x80) == 0 {
            break;
        }
    }

    // Read string table
    let strtbl_start = pos;
    let strtbl_end = strtbl_start.checked_add(strtbl_len)
        .ok_or_else(|| "String table end overflow".to_string())?;
    if strtbl_end > data.len() {
        return Err("String table exceeds data length".into());
    }
    let strtbl = &data[strtbl_start..strtbl_end];
    pos = strtbl_end;

    let mut current_page = 0;
    let mut xml = String::new();
    let mut stack: Vec<String> = Vec::new();
    let mut pending_tag: Option<String> = None;

    fn read_mb_u_int32(data: &[u8], pos: &mut usize) -> Result<usize, String> {
        let mut val: usize = 0;
        loop {
            if *pos >= data.len() {
                return Err("Unexpected end reading mb_u_int32".into());
            }
            let byte = data[*pos];
            *pos += 1;
            val = val
                .checked_shl(7)
                .and_then(|v| v.checked_add((byte & 0x7F) as usize))
                .ok_or_else(|| "mb_u_int32 overflow".to_string())?;
            if (byte & 0x80) == 0 {
                break;
            }
        }
        Ok(val)
    }

    fn read_strtbl_string(strtbl: &[u8], offset: usize) -> Result<String, String> {
        if offset >= strtbl.len() {
            return Err("String table offset out of bounds".into());
        }
        let mut end = offset;
        while end < strtbl.len() && strtbl[end] != 0 {
            end += 1;
        }
        let s = String::from_utf8_lossy(&strtbl[offset..end]).to_string();
        Ok(s)
    }

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
            let mut len: usize = 0;
            loop {
                if pos >= data.len() {
                    return Err("Unexpected end opaque".into());
                }
                let byte = data[pos];
                pos += 1;
                len = len
                    .checked_shl(7)
                    .and_then(|v| v.checked_add((byte & 0x7F) as usize))
                    .ok_or_else(|| "Opaque length overflow".to_string())?;
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

        // Handle LITERAL (0x04) and LITERAL_C (0x44) tokens
        if token == 0x04 || token == 0x44 {
            let has_content = token == 0x44;
            let offset = read_mb_u_int32(data, &mut pos)?;
            let tag_name = read_strtbl_string(strtbl, offset)?;

            if !is_valid_xml_name(&tag_name) {
                return Err(format!(
                    "Invalid LITERAL tag name in string table: {:?}",
                    tag_name
                ));
            }

            if pending_tag.is_some() {
                xml.push('>');
                stack.push(pending_tag.take().unwrap());
            }

            if has_content {
                if stack.len() >= MAX_DECODE_DEPTH {
                    return Err(format!(
                        "WBXML nesting depth exceeds maximum of {}",
                        MAX_DECODE_DEPTH
                    ));
                }
                pending_tag = Some(tag_name.clone());
                xml.push_str(&format!("<{}", tag_name));
            } else {
                xml.push_str(&format!("<{}/>", tag_name));
            }
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
                if stack.len() >= MAX_DECODE_DEPTH {
                    return Err(format!(
                        "WBXML nesting depth exceeds maximum of {}",
                        MAX_DECODE_DEPTH
                    ));
                }
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
            if stack.len() >= MAX_DECODE_DEPTH {
                return Err(format!(
                    "WBXML nesting depth exceeds maximum of {}",
                    MAX_DECODE_DEPTH
                ));
            }
            pending_tag = Some(placeholder.clone());
            xml.push_str(&format!("<{}", placeholder));
        }
    }
    Ok(xml)
}

pub fn encode(xml: &str) -> Result<Vec<u8>, String> {
    // WBXML header:
    // 0x03 = WBXML version 1.3
    // 0x01 = Public ID (unknown/opaque, matches prior behavior)
    // 0x6A = Charset UTF-8
    // <strtbl_len: mb_u_int32>
    // <string table bytes>
    // <WBXML body>
    let mut reader = quick_xml::Reader::from_str(xml);
    let mut buf = Vec::new();

    // Build WBXML body separately so we can prefix the final output with a string table.
    let mut body: Vec<u8> = Vec::new();
    let mut current_page: u8 = 0;

    // String table for LITERAL tags (used when a tag isn't present in NAME_MAP).
    let mut strtbl: Vec<u8> = Vec::new();
    let mut strtbl_index: HashMap<String, usize> = HashMap::new();

    fn write_mb_u_int32(out: &mut Vec<u8>, mut v: usize) {
        // WBXML mb_u_int32: 7-bit groups, big-endian, high bit indicates continuation.
        let mut bytes = [0u8; 10];
        let mut n = 0usize;
        loop {
            bytes[n] = (v & 0x7F) as u8;
            n += 1;
            v >>= 7;
            if v == 0 {
                break;
            }
        }
        for i in (0..n).rev() {
            let mut b = bytes[i];
            if i != 0 {
                b |= 0x80;
            }
            out.push(b);
        }
    }

    fn strtbl_offset(name: &str, idx: &mut HashMap<String, usize>, table: &mut Vec<u8>) -> usize {
        if let Some(&off) = idx.get(name) {
            return off;
        }
        let off = table.len();
        table.extend_from_slice(name.as_bytes());
        table.push(0x00);
        idx.insert(name.to_string(), off);
        off
    }

    fn encode_literal_tag(
        out: &mut Vec<u8>,
        name: &str,
        has_content: bool,
        idx: &mut HashMap<String, usize>,
        table: &mut Vec<u8>,
    ) {
        // WBXML LITERAL (0x04) / LITERAL_C (0x44): followed by string table index (mb_u_int32)
        let token = if has_content { 0x44 } else { 0x04 };
        out.push(token);
        let off = strtbl_offset(name, idx, table);
        write_mb_u_int32(out, off);
    }

    loop {
        buf.clear();
        match reader.read_event_into(&mut buf) {
            Ok(quick_xml::events::Event::Start(ref e)) => {
                let local_name = e.local_name();
                let name = String::from_utf8_lossy(local_name.as_ref());
                if !encode_tag(&mut body, &name, &mut current_page, true) {
                    encode_literal_tag(&mut body, &name, true, &mut strtbl_index, &mut strtbl);
                }
            }
            Ok(quick_xml::events::Event::Empty(ref e)) => {
                let local_name = e.local_name();
                let name = String::from_utf8_lossy(local_name.as_ref());
                if !encode_tag(&mut body, &name, &mut current_page, false) {
                    encode_literal_tag(&mut body, &name, false, &mut strtbl_index, &mut strtbl);
                }
            }
            Ok(quick_xml::events::Event::End(_)) => {
                body.push(TAG_END);
            }
            Ok(quick_xml::events::Event::Text(ref e)) => {
                body.push(TAG_STR_I);
                let text_str = std::str::from_utf8(e.as_ref())
                    .map_err(|_| "Invalid UTF-8 in XML text node".to_string())?;
                let t = quick_xml::escape::unescape(text_str)
                    .map_err(|e| format!("XML text unescape error: {}", e))?;
                body.extend(t.as_bytes());
                body.push(0x00);
            }
            Ok(quick_xml::events::Event::Eof) => break,
            Ok(_) => {}
            Err(e) => return Err(format!("XML parsing error: {}", e)),
        }
    }

    let mut output = vec![0x03, 0x01, 0x6A];
    write_mb_u_int32(&mut output, strtbl.len());
    output.extend_from_slice(&strtbl);
    output.extend_from_slice(&body);
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
