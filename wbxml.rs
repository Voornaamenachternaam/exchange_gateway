// src/wbxml.rs
use anyhow::{Result, anyhow};
use std::collections::HashMap;

/// WBXML token tables for ActiveSync code pages (minimal calendar support).
/// Code page 0: AirSync; Code page 4: Calendar; Code page 17: AirSyncBase.
pub struct Wbxml {
    pub codepage: u8,
    pub tok_to_tag: HashMap<(u8, u8), &'static str>,
    pub tag_to_tok: HashMap<(&'static str, u8), u8>,
}

impl Wbxml {
    pub fn new() -> Self {
        let mut tok_to_tag = HashMap::new();
        let mut tag_to_tok = HashMap::new();

        // Code page 0: AirSync (minimal)
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

        // Code page 4: Calendar (from MS-ASAIRS)
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
        add4!(0x0B, "Body");
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

        // Code page 17: AirSyncBase (for completeness)
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

    /// Decode WBXML payload: if it starts with '<', treat as XML, else return UTF-8 string.
    pub fn decode(&self, bytes: &[u8]) -> Result<String> {
        if bytes.is_empty() {
            return Err(anyhow!("empty payload"));
        }
        if bytes[0] == b'<' {
            return Ok(String::from_utf8(bytes.to_vec())?);
        }
        // No full WBXML parsing implemented; return raw as UTF-8
        Ok(String::from_utf8(bytes.to_vec())?)
    }

    /// Stub encoder (identity).
    pub fn encode(&self, xml: &str) -> Result<Vec<u8>> {
        Ok(xml.as_bytes().to_vec())
    }
}
