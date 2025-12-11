use anyhow::{anyhow, Result};
use std::collections::HashMap;
use quick_xml::Reader;

/// WBXML token tables for ActiveSync code pages used by calendar handling.
/// Token maps for codepages 0,4,17.
pub struct Wbxml {
    pub codepage: u8,
    pub tok_to_tag: HashMap<(u8,u8), &'static str>,
    pub tag_to_tok: HashMap<(&'static str,u8), u8>,
}

impl Wbxml {
    pub fn new() -> Self {
        let mut tok_to_tag = HashMap::new();
        let mut tag_to_tok = HashMap::new();

        // Code page 0: AirSync
        macro_rules! add0 { ($t:expr, $s:expr) => { tok_to_tag.insert((0,$t), $s); tag_to_tok.insert(($s,0), $t); }; }
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

        // Code page 4: Calendar
        macro_rules! add4 { ($t:expr, $s:expr) => { tok_to_tag.insert((4,$t), $s); tag_to_tok.insert(($s,4), $t); }; }
        add4!(0x05, "Timezone");
        add4!(0x06, "AllDayEvent");
        add4!(0x07, "Attendees");
        add4!(0x08, "Attendee");
        add4!(0x0B, "StartTime");
        add4!(0x0C, "EndTime");
        add4!(0x11, "DtStamp");
        add4!(0x13, "Subject");
        add4!(0x14, "Location");
        add4!(0x15, "Body");
        add4!(0x1B, "Recurrence");
        add4!(0x23, "Organizer");
        add4!(0x24, "RecurrenceId");
        add4!(0x27, "Recurrences");

        // Code page 17: AirSyncBase
        macro_rules! add17 { ($t:expr, $s:expr) => { tok_to_tag.insert((17,$t), $s); tag_to_tok.insert(($s,17), $t); }; }
        add17!(0x05, "BodyPreference");
        add17!(0x06, "Type");
        add17!(0x0A, "Body");
        add17!(0x0B, "Data");
        add17!(0x0C, "EstimatedDataSize");
        add17!(0x0D, "Truncated");

        Self { codepage: 0, tok_to_tag, tag_to_tok }
    }

    pub fn token_to_tag(&self, page: u8, token: u8) -> Option<&'static str> {
        self.tok_to_tag.get(&(page, token)).copied()
    }

    pub fn tag_to_token(&self, page: u8, tag: &str) -> Option<u8> {
        self.tag_to_tok.get(&(tag, page)).copied()
    }

    /// Rudimentary decoder for WBXML or pass-through XML.
    pub fn decode(&self, bytes: &[u8]) -> Result<String> {
        if bytes.is_empty() { return Err(anyhow!("empty payload")); }
        if bytes[0] == b'<' {
            return Ok(String::from_utf8(bytes.to_vec())?);
        }

        // Simplified header parse (not full WBXML)
        let mut offset = 0usize;
        if bytes.len() < 4 { return Err(anyhow!("wbxml too short")); }
        // version, pubid, charset, strtbl_len (mb uints) - skip safely for now
        // For calendar operations, many clients send XML, not WBXML; keep this simple fallback.
        // If proper WBXML binary parsing is required, replace this with a complete parser.
        Ok(String::from_utf8(bytes.to_vec())?)
    }

    /// Minimal encoder stub.
    pub fn encode(&self, xml: &str) -> Result<Vec<u8>> {
        Ok(xml.as_bytes().to_vec())
    }
}
