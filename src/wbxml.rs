// src/wbxml.rs
use bytes::Buf;
use quick_xml::events::Event;
use quick_xml::escape;
use quick_xml::Reader;
use std::collections::HashMap;
use std::io::Cursor;
use std::sync::OnceLock;
use thiserror::Error;

// WBXML Global Tokens
const SWITCH_PAGE: u8 = 0x00;
const END: u8 = 0x01;
const STR_I: u8 = 0x03; // Inline string
const STR_T: u8 = 0x04; // String table (rarely used in EAS but supported per spec)

// WBXML Header constants
const WBXML_VERSION: u8 = 0x03; // WBXML 1.3
const WBXML_PUBLIC_ID: u8 = 0x01; // Unknown/Any
const WBXML_CHARSET_UTF8: u8 = 0x6A; // IANA charset ID for UTF-8
const WBXML_STRTBL_EMPTY: u8 = 0x00;

#[derive(Debug, Error)]
pub enum WbXmlError {
    #[error("Unexpected end of input")]
    UnexpectedEof,
    #[error("Invalid WBXML header")]
    InvalidHeader,
    #[error("Unknown tag token: {0} on page {1}")]
    UnknownTag(u8, u8),
    #[error("Unknown code page: {0}")]
    UnknownPage(u8),
    #[error("Invalid global token: {0}")]
    InvalidGlobalToken(u8),
    #[error("XML parsing error: {0}")]
    XmlError(#[from] quick_xml::Error),
    #[error("String table not supported in this context")]
    StringTableUnsupported,
    #[error("Malformed string")]
    MalformedString,
    #[error("Ambiguous tag without context")]
    AmbiguousTag,
}

// --- Code Page Definitions (Per MS-ASWBXML) ---

#[derive(Debug, Clone)]
struct TagInfo {
    name: &'static str,
    token: u8,
}

// Static lookup tables generated from MS-ASWBXML spec
static CODE_PAGES: OnceLock<Vec<HashMap<u8, TagInfo>>> = OnceLock::new();
static TAG_TO_TOKEN_MAP: OnceLock<HashMap<String, (u8, u8)>> = OnceLock::new();
static NS_TO_PAGE_MAP: OnceLock<HashMap<String, u8>> = OnceLock::new();

fn get_code_pages() -> &'static Vec<HashMap<u8, TagInfo>> {
    CODE_PAGES.get_or_init(|| {
        let mut pages = Vec::with_capacity(17);

        // Page 0: AirSync
        let mut page = HashMap::new();
        page.insert(0x05, TagInfo { name: "Sync", token: 0x05 });
        page.insert(0x06, TagInfo { name: "Responses", token: 0x06 });
        page.insert(0x07, TagInfo { name: "Add", token: 0x07 }); 
        page.insert(0x08, TagInfo { name: "Change", token: 0x08 });
        page.insert(0x09, TagInfo { name: "Delete", token: 0x09 });
        page.insert(0x0A, TagInfo { name: "Fetch", token: 0x0A });
        page.insert(0x0B, TagInfo { name: "SyncKey", token: 0x0B });
        page.insert(0x0C, TagInfo { name: "ClientId", token: 0x0C });
        page.insert(0x0D, TagInfo { name: "ServerId", token: 0x0D });
        page.insert(0x0E, TagInfo { name: "Status", token: 0x0E });
        page.insert(0x0F, TagInfo { name: "Collection", token: 0x0F }); 
        page.insert(0x10, TagInfo { name: "Class", token: 0x10 });
        page.insert(0x11, TagInfo { name: "Version", token: 0x11 });
        page.insert(0x12, TagInfo { name: "Collections", token: 0x12 });
        page.insert(0x13, TagInfo { name: "GetChanges", token: 0x13 });
        page.insert(0x14, TagInfo { name: "MoreAvailable", token: 0x14 });
        page.insert(0x15, TagInfo { name: "WindowSize", token: 0x15 });
        page.insert(0x16, TagInfo { name: "Commands", token: 0x16 });
        page.insert(0x17, TagInfo { name: "Options", token: 0x17 });
        page.insert(0x18, TagInfo { name: "FilterType", token: 0x18 });
        page.insert(0x19, TagInfo { name: "Truncation", token: 0x19 });
        page.insert(0x1A, TagInfo { name: "RTF", token: 0x1A });
        page.insert(0x1B, TagInfo { name: "ConversationMode", token: 0x1B });
        page.insert(0x1C, TagInfo { name: "MaxItems", token: 0x1C });
        page.insert(0x1D, TagInfo { name: "HeartbeatInterval", token: 0x1D });
        page.insert(0x1E, TagInfo { name: "Folders", token: 0x1E });
        page.insert(0x1F, TagInfo { name: "Folder", token: 0x1F });
        page.insert(0x20, TagInfo { name: "ApplicationData", token: 0x20 });
        page.insert(0x21, TagInfo { name: "DeletesAsMoves", token: 0x21 });
        page.insert(0x22, TagInfo { name: "Supported", token: 0x22 });
        page.insert(0x23, TagInfo { name: "SoftDelete", token: 0x23 });
        page.insert(0x24, TagInfo { name: "MIMETruncation", token: 0x24 });
        page.insert(0x25, TagInfo { name: "MIMESize", token: 0x25 });
        page.insert(0x26, TagInfo { name: "BodyPreference", token: 0x26 });
        page.insert(0x27, TagInfo { name: "BodyPartPreference", token: 0x27 });
        page.insert(0x28, TagInfo { name: "RightsManagementSupport", token: 0x28 });
        pages.push(page);

        // Page 1: Contacts (Selected relevant tags)
        let mut page = HashMap::new();
        page.insert(0x05, TagInfo { name: "Anniversary", token: 0x05 });
        page.insert(0x06, TagInfo { name: "AssistantName", token: 0x06 });
        page.insert(0x17, TagInfo { name: "CompanyName", token: 0x17 });
        page.insert(0x1F, TagInfo { name: "FirstName", token: 0x1F });
        page.insert(0x29, TagInfo { name: "LastName", token: 0x29 });
        page.insert(0x2A, TagInfo { name: "MiddleName", token: 0x2A });
        pages.push(page);

        // Page 2: Email
        let mut page = HashMap::new();
        page.insert(0x05, TagInfo { name: "DateReceived", token: 0x05 });
        page.insert(0x07, TagInfo { name: "Importance", token: 0x07 });
        page.insert(0x09, TagInfo { name: "Subject", token: 0x09 });
        page.insert(0x0A, TagInfo { name: "Read", token: 0x0A });
        page.insert(0x0D, TagInfo { name: "From", token: 0x0D });
        page.insert(0x0F, TagInfo { name: "AllDayEvent", token: 0x0F });
        page.insert(0x13, TagInfo { name: "EndTime", token: 0x13 });
        page.insert(0x16, TagInfo { name: "Location", token: 0x16 });
        page.insert(0x1A, TagInfo { name: "Reminder", token: 0x1A });
        page.insert(0x1E, TagInfo { name: "Type", token: 0x1E });
        page.insert(0x26, TagInfo { name: "StartTime", token: 0x26 });
        page.insert(0x27, TagInfo { name: "Sensitivity", token: 0x27 });
        page.insert(0x35, TagInfo { name: "TimeZone", token: 0x35 });
        page.insert(0x36, TagInfo { name: "Attendees", token: 0x36 });
        page.insert(0x37, TagInfo { name: "Attendee", token: 0x37 });
        page.insert(0x38, TagInfo { name: "Email", token: 0x38 });
        page.insert(0x39, TagInfo { name: "Name", token: 0x39 });
        page.insert(0x3A, TagInfo { name: "AttendeeStatus", token: 0x3A });
        page.insert(0x3B, TagInfo { name: "AttendeeType", token: 0x3B });
        pages.push(page);

        // Page 3: AirSyncBase
        let mut page = HashMap::new();
        page.insert(0x05, TagInfo { name: "Body", token: 0x05 });
        page.insert(0x06, TagInfo { name: "BodyType", token: 0x06 });
        page.insert(0x07, TagInfo { name: "Data", token: 0x07 });
        page.insert(0x08, TagInfo { name: "EstimatedDataSize", token: 0x08 });
        page.insert(0x09, TagInfo { name: "Truncated", token: 0x09 });
        page.insert(0x0A, TagInfo { name: "Attachments", token: 0x0A });
        page.insert(0x0B, TagInfo { name: "Attachment", token: 0x0B });
        page.insert(0x0C, TagInfo { name: "DisplayName", token: 0x0C });
        page.insert(0x0D, TagInfo { name: "FileReference", token: 0x0D });
        page.insert(0x12, TagInfo { name: "NativeBodyType", token: 0x12 });
        page.insert(0x14, TagInfo { name: "Preview", token: 0x14 });
        page.insert(0x15, TagInfo { name: "BodyPart", token: 0x15 });
        page.insert(0x16, TagInfo { name: "Status", token: 0x16 });
        page.insert(0x17, TagInfo { name: "BodyPartPreference", token: 0x17 });
        page.insert(0x18, TagInfo { name: "Type", token: 0x18 });
        page.insert(0x19, TagInfo { name: "TruncationSize", token: 0x19 });
        page.insert(0x1A, TagInfo { name: "AllOrNone", token: 0x1A });
        pages.push(page);

        // Page 4: Calendar
        let mut page = HashMap::new();
        page.insert(0x05, TagInfo { name: "TimeZone", token: 0x05 });
        page.insert(0x06, TagInfo { name: "AllDayEvent", token: 0x06 });
        page.insert(0x07, TagInfo { name: "Attendees", token: 0x07 });
        page.insert(0x08, TagInfo { name: "Attendee", token: 0x08 });
        page.insert(0x09, TagInfo { name: "Email", token: 0x09 });
        page.insert(0x0A, TagInfo { name: "Name", token: 0x0A });
        page.insert(0x0B, TagInfo { name: "AttendeeStatus", token: 0x0B });
        page.insert(0x0C, TagInfo { name: "AttendeeType", token: 0x0C });
        page.insert(0x0D, TagInfo { name: "BusyStatus", token: 0x0D });
        page.insert(0x10, TagInfo { name: "DtStamp", token: 0x10 });
        page.insert(0x11, TagInfo { name: "EndTime", token: 0x11 });
        page.insert(0x16, TagInfo { name: "Location", token: 0x16 });
        page.insert(0x1A, TagInfo { name: "Recurrence", token: 0x1A });
        page.insert(0x1B, TagInfo { name: "RecurrenceType", token: 0x1B });
        page.insert(0x23, TagInfo { name: "Reminder", token: 0x23 });
        page.insert(0x24, TagInfo { name: "Sensitivity", token: 0x24 });
        page.insert(0x25, TagInfo { name: "Subject", token: 0x25 });
        page.insert(0x26, TagInfo { name: "StartTime", token: 0x26 });
        page.insert(0x27, TagInfo { name: "UID", token: 0x27 });
        page.insert(0x29, TagInfo { name: "DisallowNewTimeProposal", token: 0x29 });
        page.insert(0x2A, TagInfo { name: "ResponseRequested", token: 0x2A });
        pages.push(page);

        // Page 7: Tasks
        let mut page = HashMap::new();
        page.insert(0x05, TagInfo { name: "Subject", token: 0x05 });
        page.insert(0x1F, TagInfo { name: "Reminder", token: 0x1F });
        pages.push(page);

        // Page 10: Ping
        let mut page = HashMap::new();
        page.insert(0x05, TagInfo { name: "Ping", token: 0x05 });
        page.insert(0x07, TagInfo { name: "Status", token: 0x07 });
        page.insert(0x08, TagInfo { name: "HeartbeatInterval", token: 0x08 });
        page.insert(0x09, TagInfo { name: "Folders", token: 0x09 });
        page.insert(0x0A, TagInfo { name: "Folder", token: 0x0A });
        pages.push(page);

        // Page 11: Provision
        let mut page = HashMap::new();
        page.insert(0x05, TagInfo { name: "Provision", token: 0x05 });
        page.insert(0x06, TagInfo { name: "Policies", token: 0x06 });
        page.insert(0x07, TagInfo { name: "Policy", token: 0x07 });
        page.insert(0x08, TagInfo { name: "PolicyType", token: 0x08 });
        page.insert(0x09, TagInfo { name: "PolicyKey", token: 0x09 });
        page.insert(0x0A, TagInfo { name: "Data", token: 0x0A });
        page.insert(0x0B, TagInfo { name: "Status", token: 0x0B });
        page.insert(0x0C, TagInfo { name: "RemoteWipe", token: 0x0C });
        page.insert(0x0D, TagInfo { name: "EASProvisionDoc", token: 0x0D });
        page.insert(0x0E, TagInfo { name: "DevicePasswordEnabled", token: 0x0E });
        page.insert(0x10, TagInfo { name: "RequireDeviceEncryption", token: 0x10 });
        pages.push(page);

        // Page 12: Settings
        let mut page = HashMap::new();
        page.insert(0x05, TagInfo { name: "Settings", token: 0x05 });
        page.insert(0x06, TagInfo { name: "Status", token: 0x06 });
        page.insert(0x16, TagInfo { name: "DeviceInformation", token: 0x16 });
        page.insert(0x19, TagInfo { name: "FriendlyName", token: 0x19 });
        pages.push(page);

        // Page 14: ItemOperations
        let mut page = HashMap::new();
        page.insert(0x05, TagInfo { name: "ItemOperations", token: 0x05 });
        page.insert(0x06, TagInfo { name: "Fetch", token: 0x06 });
        page.insert(0x0D, TagInfo { name: "Status", token: 0x0D });
        page.insert(0x0E, TagInfo { name: "Response", token: 0x0E });
        pages.push(page);

        // Page 16: Search
        let mut page = HashMap::new();
        page.insert(0x05, TagInfo { name: "Search", token: 0x05 });
        page.insert(0x06, TagInfo { name: "Store", token: 0x06 });
        page.insert(0x0B, TagInfo { name: "Status", token: 0x0B });
        page.insert(0x0C, TagInfo { name: "Response", token: 0x0C });
        page.insert(0x0D, TagInfo { name: "Result", token: 0x0D });
        pages.push(page);

        pages
    })
}

// Build a reverse map for Encoding: Name -> (Page, Token)
fn get_tag_to_token_map() -> &'static HashMap<String, (u8, u8)> {
    TAG_TO_TOKEN_MAP.get_or_init(|| {
        let mut map = HashMap::new();
        for (page_idx, page) in get_code_pages().iter().enumerate() {
            for (_, info) in page {
                map.insert(info.name.to_string(), (page_idx as u8, info.token));
            }
        }
        map
    })
}

fn get_ns_to_page_map() -> &'static HashMap<String, u8> {
    NS_TO_PAGE_MAP.get_or_init(|| {
        let mut map = HashMap::new();
        map.insert("AirSync".to_string(), 0);
        map.insert("AirSyncBase".to_string(), 3);
        map.insert("Calendar".to_string(), 4);
        map.insert("Email".to_string(), 2);
        map.insert("Tasks".to_string(), 7);
        map.insert("Ping".to_string(), 10);
        map.insert("Provision".to_string(), 11);
        map.insert("Settings".to_string(), 12);
        map.insert("ItemOperations".to_string(), 14);
        map.insert("Search".to_string(), 16);
        map
    })
}

/// Encode XML string to WBXML bytes
pub fn encode(xml: &str) -> Result<Vec<u8>, WbXmlError> {
    let mut buf = Vec::new();
    
    // Write Header
    buf.push(WBXML_VERSION);
    buf.push(WBXML_PUBLIC_ID);
    buf.push(WBXML_CHARSET_UTF8);
    buf.push(WBXML_STRTBL_EMPTY);

    let mut current_page = 0;
    let mut tag_stack: Vec<String> = Vec::new();

    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);

    loop {
        match reader.read_event() {
            Ok(Event::Start(ref e)) => {
                let (prefix_opt, local_name) = extract_name_parts(e.name());
                handle_element_start(&mut buf, &mut current_page, &mut tag_stack, prefix_opt, local_name)?;
            }
            Ok(Event::Empty(ref e)) => {
                let (prefix_opt, local_name) = extract_name_parts(e.name());
                handle_element_start(&mut buf, &mut current_page, &mut tag_stack, prefix_opt, local_name.clone())?;
                buf.push(END);
                tag_stack.pop();
            }
            Ok(Event::End(ref _e)) => {
                if !tag_stack.is_empty() {
                    buf.push(END);
                    tag_stack.pop();
                }
            }
            Ok(Event::Text(ref e)) => {
                // Fix for quick-xml 0.39: unescape is a free function
                let text_str = std::str::from_utf8(e.as_ref()).map_err(|_| WbXmlError::MalformedString)?;
                let text = escape::unescape(text_str).map_err(|_| WbXmlError::MalformedString)?;
                
                if !text.is_empty() {
                    buf.push(STR_I);
                    buf.extend_from_slice(text.as_bytes());
                    buf.push(0x00);
                }
            }
            Ok(Event::Eof) => break,
            _ => {}
        }
    }

    Ok(buf)
}

fn handle_element_start(
    buf: &mut Vec<u8>,
    current_page: &mut u8,
    tag_stack: &mut Vec<String>,
    prefix_opt: Option<&str>,
    local_name: String,
) -> Result<(), WbXmlError> {
    let (page, token) = {
        if let Some(prefix) = prefix_opt {
            if let Some(&target_page) = get_ns_to_page_map().get(prefix) {
                if let Some(&(_, token)) = get_tag_to_token_map().get(&local_name) {
                    (target_page, token)
                } else {
                    return Err(WbXmlError::UnknownTag(0, target_page));
                }
            } else {
                lookup_tag(&local_name, *current_page)?
            }
        } else {
            lookup_tag(&local_name, *current_page)?
        }
    };

    if page != *current_page {
        buf.push(SWITCH_PAGE);
        buf.push(page);
        *current_page = page;
    }

    buf.push(token | 0x40);
    tag_stack.push(local_name);

    Ok(())
}

fn lookup_tag(local_name: &str, current_page: u8) -> Result<(u8, u8), WbXmlError> {
    if let Some(&(page, token)) = get_tag_to_token_map().get(local_name) {
        Ok((page, token))
    } else {
        Err(WbXmlError::UnknownTag(0, current_page))
    }
}

/// Decode WBXML bytes to XML string
pub fn decode(bytes: &[u8]) -> Result<String, WbXmlError> {
    let mut cursor = Cursor::new(bytes);
    
    let version = cursor.get_u8();
    let _public_id = cursor.get_u8();
    let charset = cursor.get_u8();
    let strtbl_len = cursor.get_u8();

    if strtbl_len != 0 {
        return Err(WbXmlError::StringTableUnsupported);
    }
    
    if version != WBXML_VERSION || charset != WBXML_CHARSET_UTF8 {
        return Err(WbXmlError::InvalidHeader);
    }

    let mut xml = String::new();
    let mut current_page = 0;
    let mut tag_stack: Vec<String> = Vec::new();

    while cursor.has_remaining() {
        let token = cursor.get_u8();

        match token {
            SWITCH_PAGE => {
                if !cursor.has_remaining() {
                    return Err(WbXmlError::UnexpectedEof);
                }
                current_page = cursor.get_u8();
            }
            END => {
                if let Some(tag) = tag_stack.pop() {
                    xml.push_str(&format!("</{}>", tag));
                }
            }
            STR_I => {
                let mut str_bytes = Vec::new();
                loop {
                    if !cursor.has_remaining() {
                        return Err(WbXmlError::UnexpectedEof);
                    }
                    let b = cursor.get_u8();
                    if b == 0x00 {
                        break;
                    }
                    str_bytes.push(b);
                }
                let text = String::from_utf8(str_bytes).map_err(|_| WbXmlError::MalformedString)?;
                xml.push_str(&text);
            }
            STR_T => {
                return Err(WbXmlError::StringTableUnsupported);
            }
            _ => {
                let has_content = (token & 0x40) != 0;
                let tag_id = token & 0x3F;

                let pages = get_code_pages();
                let page = pages.get(current_page as usize).ok_or(WbXmlError::UnknownPage(current_page))?;
                let tag_info = page.get(&tag_id).ok_or(WbXmlError::UnknownTag(tag_id, current_page))?;

                let tag_name = tag_info.name;
                
                if has_content {
                    xml.push_str(&format!("<{}>", tag_name));
                    tag_stack.push(tag_name.to_string());
                } else {
                    xml.push_str(&format!("<{}/>", tag_name));
                }
            }
        }
    }

    Ok(xml)
}

fn extract_name_parts(name: quick_xml::name::QName) -> (Option<&str>, String) {
    let name_bytes = name.0;
    let name_str = std::str::from_utf8(name_bytes).unwrap_or("");
    let mut parts = name_str.split(':');
    let first = parts.next();
    let second = parts.next();

    match (first, second) {
        (Some(prefix), Some(local)) => (Some(prefix), local.to_string()),
        (Some(local), None) => (None, local.to_string()),
        _ => (None, "Unknown".to_string()),
    }
} 
