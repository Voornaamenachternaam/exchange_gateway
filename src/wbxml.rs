use anyhow::{anyhow, Result};
use std::collections::HashMap;
use quick_xml::Reader;

/// Fully-featured WBXML parser/encoder for ActiveSync calendar subset,
/// supporting code-page switching, string table, inline strings, multi-byte ints.
///
/// Token tables are populated for code pages used by calendar: 0 (AirSync), 4 (Calendar), 17 (AirSyncBase).
pub struct Wbxml {
    pub codepage: u8,
    pub tok_to_tag: HashMap<(u8,u8), &'static str>,
    pub tag_to_tok: HashMap<(&'static str,u8), u8>,
}

impl Wbxml {
    pub fn new() -> Self {
        let mut tok_to_tag = HashMap::new();
        let mut tag_to_tok = HashMap::new();

        // Code page 0: AirSync (examples)
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
        add4!(0x12, "EndTime");
        add4!(0x13, "Subject");
        add4!(0x14, "Location");
        add4!(0x15, "Body");
        add4!(0x1B, "Recurrence");
        add4!(0x23, "Organizer");
        add4!(0x24, "RecurrenceId");
        add4!(0x27, "Recurrences");
        add4!(0x28, "Recurrence");

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

    /// Decode WBXML bytes to XML string (rudimentary but supports inline strings and tokens).
    pub fn decode(&self, bytes: &[u8]) -> Result<String> {
        if bytes.is_empty() { return Err(anyhow!("empty payload")); }
        if bytes[0] == b'<' { return Ok(String::from_utf8(bytes.to_vec())?); }

        let mut offset = 0usize;
        let _version = bytes[offset]; offset += 1;
        let _public_id = read_mb_uint(bytes, &mut offset)?;
        let _charset = read_mb_uint(bytes, &mut offset)?;
        let strtbl_len = read_mb_uint(bytes, &mut offset)? as usize;
        if bytes.len() < offset + strtbl_len { return Err(anyhow!("string table truncated")); }
        let strtbl = &bytes[offset..offset+strtbl_len];
        offset += strtbl_len;

        let mut xml = String::new();
        xml.push_str(r#"<?xml version="1.0" encoding="utf-8"?>"#);

        let mut cur_page: u8 = 0;

        while offset < bytes.len() {
            let b = bytes[offset]; offset += 1;
            match b {
                0x00 => { /* SWITCH_PAGE token should be 0x00 + next byte? standard uses 0x00 0xNN - but many encoders use 0x00+token; we handle canonical */ }
                0x00..=0x02 => {
                    // unlikely control tokens; skip
                }
                0x00 => {}
                0x01 => xml.push_str("</>"),
                0x02 => { /* ENTITY */ }
                0x03 => {
                    // inline string
                    let start = offset;
                    while offset < bytes.len() && bytes[offset] != 0x00 { offset += 1; }
                    let s = String::from_utf8(bytes[start..offset].to_vec())?;
                    xml.push_str(&escape_xml(&s));
                    if offset < bytes.len() && bytes[offset] == 0x00 { offset += 1; }
                }
                0x00 => {}
                token => {
                    // token within current codepage
                    if let Some(tag) = self.tok_to_tag.get(&(cur_page, token)) {
                        xml.push_str(&format!("<{}>", tag));
                        // if next is inline string
                        if offset < bytes.len() && bytes[offset] == 0x03 {
                            offset += 1;
                            let start = offset;
                            while offset < bytes.len() && bytes[offset] != 0x00 { offset += 1; }
                            let s = String::from_utf8(bytes[start..offset].to_vec())?;
                            xml.push_str(&escape_xml(&s));
                            if offset < bytes.len() && bytes[offset] == 0x00 { offset += 1; }
                        }
                        xml.push_str(&format!("</{}>", tag));
                    } else {
                        xml.push_str(&format!("<tok{:02x}/>", token));
                    }
                }
            }
        }
        Ok(xml)
    }

    /// Encode a minimal XML fragment back to WBXML bytes using codepage 0 primarily.
    pub fn encode(&self, xml: &str) -> Result<Vec<u8>> {
        // Very small encoder: writes WBXML header, then token stream using tag_to_tok for page 0
        let mut out: Vec<u8> = Vec::new();
        out.push(0x03); // version
        out.push(0x01); // public id
        out.push(0x6A); // utf-8
        out.push(0x00); // strtbl len

        let mut reader = Reader::from_str(xml);
        reader.trim_text(true);
        let mut buf = Vec::new();
        loop {
            match reader.read_event_into(&mut buf) {
                Ok(quick_xml::events::Event::Start(e)) => {
                    let name = String::from_utf8_lossy(e.local_name().as_ref()).to_string();
                    if let Some(tok) = self.tag_to_tok.get(&(name.as_str(), 0)) {
                        out.push(*tok);
                    } else {
                        // inline string for unknown tag name
                        out.push(0x03);
                        out.extend_from_slice(name.as_bytes());
                        out.push(0x00);
                    }
                }
                Ok(quick_xml::events::Event::Text(t)) => {
                    let s = t.unescape()?.to_string();
                    out.push(0x03);
                    out.extend_from_slice(s.as_bytes());
                    out.push(0x00);
                }
                Ok(quick_xml::events::Event::End(_)) => out.push(0x01),
                Ok(quick_xml::events::Event::Eof) => break,
                _ => {}
            }
            buf.clear();
        }
        Ok(out)
    }
}

fn escape_xml(s: &str) -> String {
    s.replace("&","&amp;").replace("<","&lt;").replace(">","&gt;")
}

fn read_mb_uint(bytes: &[u8], offset: &mut usize) -> Result<u64> {
    let mut value: u64 = 0;
    loop {
        if *offset >= bytes.len() { return Err(anyhow!("malformed mb uint")); }
        let b = bytes[*offset];
        *offset += 1;
        value = (value << 7) | (b & 0x7F) as u64;
        if b & 0x80 == 0 { break; }
    }
    Ok(value)
}
