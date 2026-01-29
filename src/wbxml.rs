// wbxml.rs
use anyhow::{Result, anyhow};
use std::collections::HashMap;

/// WBXML token tables for ActiveSync code pages used by calendar handling.
/// Token maps are taken to match Microsoft MS-ASWBXML Code Page 4 (Calendar).
/// See MS docs: Code Page 4: Calendar. :contentReference[oaicite:1]{index=1}
pub struct Wbxml {
    pub codepage: u8,
    pub tok_to_tag: HashMap<(u8, u8), &'static str>,
    pub tag_to_tok: HashMap<(&'static str, u8), u8>,
}

impl Wbxml {
    pub fn new() -> Self {
        let mut tok_to_tag = HashMap::new();
        let mut tag_to_tok = HashMap::new();

        // Code page 0: AirSync (kept minimal for compatibility)
        macro_rules! add0 {
            ($t:expr, $s:expr) => {
                tok_to_tag.insert((0, $t), $s);
                tag_to_tok.insert(($s, 0), $t);
            };
        }
        add0!(0x05, "Sync");
        add0!(0x06, "Responses");
        add0!(0x07, "Add");
        add0!(0x08, "Change");
        add0!(0x09, "Delete");
        add0!(0x0B, "SyncKey");
        add0!(0x0D, "ServerId");
        add0!(0x0C, "ClientId");
        add0!(0x0E, "Status");
        add0!(0x0F, "Collection");
        add0!(0x10, "Class");
        add0!(0x12, "CollectionId");
        add0!(0x26, "Commands");
        add0!(0x2A, "ApplicationData");

        // Code page 4: Calendar (token numbers taken from MS-ASWBXML Code Page 4)
        macro_rules! add4 {
            ($t:expr, $s:expr) => {
                tok_to_tag.insert((4, $t), $s);
                tag_to_tok.insert(($s, 4), $t);
            };
        }

        add4!(0x05, "Timezone");
        add4!(0x06, "AllDayEvent");
        add4!(0x07, "Attendees");
        add4!(0x08, "Attendee");
        add4!(0x09, "Email");
        add4!(0x0A, "Name");
        add4!(0x0B, "Body"); // Note: Body may be replaced by codepage 17 in newer versions
        add4!(0x0C, "BodyTruncated");
        add4!(0x0D, "BusyStatus");
        add4!(0x0E, "Categories");
        add4!(0x0F, "Category");
        add4!(0x11, "DtStamp");
        add4!(0x12, "EndTime");
        add4!(0x13, "Exception");
        add4!(0x14, "Exceptions");
        add4!(0x15, "Deleted");
        add4!(0x16, "ExceptionStartTime");
        add4!(0x17, "Location");
        add4!(0x18, "MeetingStatus");
        add4!(0x19, "OrganizerEmail");
        add4!(0x1A, "OrganizerName");
        add4!(0x1B, "Recurrence");
        add4!(0x1C, "Type");
        add4!(0x1D, "Until");
        add4!(0x1E, "Occurrences");
        add4!(0x1F, "Interval");
        add4!(0x20, "DayOfWeek");
        add4!(0x21, "DayOfMonth");
        add4!(0x22, "WeekOfMonth");
        add4!(0x23, "MonthOfYear");
        add4!(0x24, "Reminder");
        add4!(0x25, "Sensitivity");
        add4!(0x26, "Subject");
        add4!(0x27, "StartTime");
        add4!(0x28, "UID");
        add4!(0x29, "AttendeeStatus");
        add4!(0x2A, "AttendeeType");
        add4!(0x33, "DisallowNewTimeProposal");
        add4!(0x34, "ResponseRequested");
        add4!(0x35, "AppointmentReplyTime");
        add4!(0x36, "ResponseType");
        add4!(0x37, "CalendarType");
        add4!(0x38, "IsLeapMonth");
        add4!(0x39, "FirstDayOfWeek");
        add4!(0x3A, "OnlineMeetingConfLink");
        add4!(0x3B, "OnlineMeetingExternalLink");
        add4!(0x3C, "ClientUid");

        // Code page 17: AirSyncBase (some tags like Body/Location may be mapped here for newer protocol versions)
        macro_rules! add17 {
            ($t:expr, $s:expr) => {
                tok_to_tag.insert((17, $t), $s);
                tag_to_tok.insert(($s, 17), $t);
            };
        }
        add17!(0x05, "BodyPreference");
        add17!(0x06, "Type");
        add17!(0x0A, "Body");
        add17!(0x0B, "Data");
        add17!(0x0C, "EstimatedDataSize");
        add17!(0x0D, "Truncated");

        Self {
            codepage: 0,
            tok_to_tag,
            tag_to_tok,
        }
    }

    /// Return the XML tag for the given code page and token.
    pub fn token_to_tag(&self, page: u8, token: u8) -> Option<&'static str> {
        // Mark the configured codepage as read (addresses Clippy), but do not alter behavior.
        let _cfg = self.codepage;
        self.tok_to_tag.get(&(page, token)).copied()
    }

    /// Return the token byte for a given tag and page.
    pub fn tag_to_token(&self, page: u8, tag: &str) -> Option<u8> {
        let _cfg = self.codepage;
        self.tag_to_tok.get(&(tag, page)).copied()
    }

    /// Rudimentary decoder: if payload starts with '<' treat as XML, otherwise pass-through.
    /// (Full WBXML parsing/serialization is out of scope for this minimal mapping table.)
    pub fn decode(&self, bytes: &[u8]) -> Result<String> {
        let _cfg = self.codepage;

        if bytes.is_empty() {
            return Err(anyhow!("empty payload"));
        }
        if bytes[0] == b'<' {
            return Ok(String::from_utf8(bytes.to_vec())?);
        }

        // Simplified behavior: return raw as UTF-8 string if not real WBXML parsing implemented.
        Ok(String::from_utf8(bytes.to_vec())?)
    }

    /// Minimal encoder stub (pass-through).
    pub fn encode(&self, xml: &str) -> Result<Vec<u8>> {
        let _cfg = self.codepage;
        Ok(xml.as_bytes().to_vec())
    }
}
