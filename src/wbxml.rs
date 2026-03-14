// src/wbxml.rs
use anyhow::{Result, anyhow};
use std::collections::HashMap;

const SWITCH_PAGE: u8 = 0x00;
const END: u8 = 0x01;
const STR_I: u8 = 0x03;

lazy_static::lazy_static! {
    static ref TAG_TO_NAME: HashMap<(u8, u8), &'static str> = {
        let mut m = HashMap::new();

        // Code Page 0: AirSync
        m.insert((0, 0x05), "Sync");
        m.insert((0, 0x06), "Responses");
        m.insert((0, 0x07), "Add");
        m.insert((0, 0x08), "Change");
        m.insert((0, 0x09), "Delete");
        m.insert((0, 0x0B), "SyncKey");
        m.insert((0, 0x0C), "ClientId");
        m.insert((0, 0x0D), "ServerId");
        m.insert((0, 0x0E), "Status");
        m.insert((0, 0x0F), "Collection");
        m.insert((0, 0x10), "Class");
        m.insert((0, 0x12), "CollectionId");

        // Additional AirSync tags used by command/reference semantics
        m.insert((0, 0x16), "Commands");
        m.insert((0, 0x17), "Options");
        m.insert((0, 0x1B), "Conflict");
        m.insert((0, 0x1C), "Collections");
        m.insert((0, 0x1D), "ApplicationData");

        // Code Page 4: Calendar (ASCAL)
        m.insert((4, 0x05), "Calendar:Timezone");
        m.insert((4, 0x06), "Calendar:AllDayEvent");
        m.insert((4, 0x0B), "Calendar:Body");
        m.insert((4, 0x0D), "Calendar:BusyStatus");
        m.insert((4, 0x11), "Calendar:DtStamp");
        m.insert((4, 0x12), "Calendar:EndTime");
        m.insert((4, 0x13), "Calendar:Exception");
        m.insert((4, 0x14), "Calendar:Exceptions");
        m.insert((4, 0x15), "Calendar:Deleted");
        m.insert((4, 0x16), "Calendar:ExceptionStartTime");
        m.insert((4, 0x17), "Calendar:Location");
        m.insert((4, 0x1B), "Calendar:Recurrence");
        m.insert((4, 0x1C), "Calendar:Type");
        m.insert((4, 0x1D), "Calendar:Until");
        m.insert((4, 0x1E), "Calendar:Occurrences");
        m.insert((4, 0x1F), "Calendar:Interval");
        m.insert((4, 0x20), "Calendar:DayOfWeek");
        m.insert((4, 0x21), "Calendar:DayOfMonth");
        m.insert((4, 0x22), "Calendar:WeekOfMonth");
        m.insert((4, 0x23), "Calendar:MonthOfYear");
        m.insert((4, 0x24), "Calendar:Reminder");
        m.insert((4, 0x25), "Calendar:Sensitivity");
        m.insert((4, 0x26), "Calendar:Subject");
        m.insert((4, 0x27), "Calendar:StartTime");
        m.insert((4, 0x28), "Calendar:UID");

        // Code Page 17: AirSyncBase
        m.insert((17, 0x05), "AirSyncBase:BodyPreference");
        m.insert((17, 0x06), "AirSyncBase:Type");
        m.insert((17, 0x07), "AirSyncBase:TruncationSize");
        m.insert((17, 0x0A), "AirSyncBase:Body");
        m.insert((17, 0x0B), "AirSyncBase:Data");
        m.insert((17, 0x0C), "AirSyncBase:EstimatedDataSize");
        m.insert((17, 0x0D), "AirSyncBase:Truncated");


        // Code Page 7: FolderHierarchy
        m.insert((7, 0x05), "Folders");
        m.insert((7, 0x06), "Folder");
        m.insert((7, 0x07), "DisplayName");
        m.insert((7, 0x08), "ServerId");
        m.insert((7, 0x09), "ParentId");
        m.insert((7, 0x0A), "Type");
        m.insert((7, 0x0C), "Status");
        m.insert((7, 0x0E), "Changes");
        m.insert((7, 0x0F), "Add");
        m.insert((7, 0x10), "Delete");
        m.insert((7, 0x11), "Update");
        m.insert((7, 0x12), "SyncKey");
        m.insert((7, 0x16), "FolderSync");
        m.insert((7, 0x17), "Count");

        // Code Page 13: Ping
        m.insert((13, 0x05), "Ping");
        m.insert((13, 0x07), "Status");
        m.insert((13, 0x08), "HeartbeatInterval");
        m.insert((13, 0x09), "Folders");
        m.insert((13, 0x0A), "Folder");
        m.insert((13, 0x0B), "Id");
        m.insert((13, 0x0C), "Class");

        // Code Page 14: Provision
        m.insert((14, 0x05), "Provision");
        m.insert((14, 0x06), "Policies");
        m.insert((14, 0x07), "Policy");
        m.insert((14, 0x08), "PolicyType");
        m.insert((14, 0x09), "PolicyKey");
        m.insert((14, 0x0A), "Data");
        m.insert((14, 0x0B), "Status");

        // Code Page 18: Settings
        m.insert((18, 0x05), "Settings");
        m.insert((18, 0x06), "Status");

        m
    };

    static ref NAME_TO_TAG: HashMap<&'static str, (u8, u8)> = {
        let mut m = HashMap::new();
        for ((cp, id), name) in TAG_TO_NAME.iter() {
            m.insert(*name, (*cp, *id));
        }
        m
    };
}

pub struct Wbxml;

impl Wbxml {
    pub fn new() -> Self {
        Wbxml
    }

    pub fn decode(&self, bytes: &[u8]) -> Result<String> {
        if bytes.is_empty() {
            return Err(anyhow!("Empty WBXML payload"));
        }
        if bytes[0] == b'<' {
            return Ok(String::from_utf8(bytes.to_vec())?);
        }

        let mut pos = 0;
        let _version = bytes[pos];
        pos += 1;
        let _public_id = bytes[pos];
        pos += 1;
        let _charset = bytes[pos];
        pos += 1;
        let _str_table_len = bytes[pos];
        pos += 1;

        let mut current_code_page = 0u8;
        let mut xml_stack: Vec<String> = Vec::new();
        let mut output = String::new();

        output.push_str("<?xml version=\"1.0\" encoding=\"utf-8\"?>\n");

        while pos < bytes.len() {
            let token = bytes[pos];
            pos += 1;

            match token {
                SWITCH_PAGE => {
                    if pos >= bytes.len() {
                        break;
                    }
                    current_code_page = bytes[pos];
                    pos += 1;
                }
                END => {
                    if let Some(tag) = xml_stack.pop() {
                        output.push_str(&format!("</{}>", tag));
                    }
                }
                STR_I => {
                    let mut s_bytes = Vec::new();
                    while pos < bytes.len() && bytes[pos] != 0 {
                        s_bytes.push(bytes[pos]);
                        pos += 1;
                    }
                    pos += 1;
                    let content = String::from_utf8_lossy(&s_bytes);
                    let escaped = content
                        .replace("&", "&amp;")
                        .replace("<", "&lt;")
                        .replace(">", "&gt;");
                    output.push_str(&escaped);
                }
                _ => {
                    if token >= 0x05 {
                        let has_content = (token & 0x40) != 0;
                        let tag_id = token & 0x3F;

                        if let Some(name) = TAG_TO_NAME.get(&(current_code_page, tag_id)) {
                            output.push_str(&format!("<{}>", name));
                            if has_content {
                                xml_stack.push(name.to_string());
                            } else {
                                output.push_str(&format!("</{}>", name));
                            }
                        }
                    }
                }
            }
        }
        Ok(output)
    }

    pub fn encode(&self, xml: &str) -> Result<Vec<u8>> {
        let mut buf = vec![0x03, 0x01, 0x6A, 0x00];

        let mut reader = quick_xml::Reader::from_str(xml);
        reader.config_mut().trim_text(true);
        let mut current_code_page = 0u8;
        let mut buf_event = Vec::new();

        loop {
            match reader.read_event_into(&mut buf_event) {
                Ok(quick_xml::events::Event::Start(e)) => {
                    let name = e.name().local_name();
                    let name_str = std::str::from_utf8(name.as_ref())?;

                    if let Some((cp, tag)) = NAME_TO_TAG.get(name_str) {
                        if *cp != current_code_page {
                            buf.push(SWITCH_PAGE);
                            buf.push(*cp);
                            current_code_page = *cp;
                        }
                        buf.push(tag | 0x40);
                    } else {
                        let mut found = false;
                        for (k, (cp, tag)) in NAME_TO_TAG.iter() {
                            if k.ends_with(name_str) {
                                if *cp != current_code_page {
                                    buf.push(SWITCH_PAGE);
                                    buf.push(*cp);
                                    current_code_page = *cp;
                                }
                                buf.push(tag | 0x40);
                                found = true;
                                break;
                            }
                        }
                        if !found {
                            tracing::warn!("Unknown tag in encoder: {}", name_str);
                        }
                    }
                }
                Ok(quick_xml::events::Event::Empty(e)) => {
                    let name = e.name().local_name();
                    let name_str = std::str::from_utf8(name.as_ref())?;
                    if let Some((cp, tag)) = NAME_TO_TAG.get(name_str) {
                        if *cp != current_code_page {
                            buf.push(SWITCH_PAGE);
                            buf.push(*cp);
                            current_code_page = *cp;
                        }
                        buf.push(*tag);
                    } else {
                        let mut found = false;
                        for (k, (cp, tag)) in NAME_TO_TAG.iter() {
                            if k.ends_with(name_str) {
                                if *cp != current_code_page {
                                    buf.push(SWITCH_PAGE);
                                    buf.push(*cp);
                                    current_code_page = *cp;
                                }
                                buf.push(*tag);
                                found = true;
                                break;
                            }
                        }
                        if !found {
                            tracing::warn!("Unknown empty tag in encoder: {}", name_str);
                        }
                    }
                }
                Ok(quick_xml::events::Event::Text(e)) => {
                    buf.push(STR_I);
                    // FIXED: Use decode() for quick-xml 0.39
                    let txt = e
                        .decode()
                        .map_err(|e| anyhow!("XML Decode Error: {}", e))?
                        .into_owned();
                    buf.extend_from_slice(txt.as_bytes());
                    buf.push(0x00);
                }
                Ok(quick_xml::events::Event::End(_)) => {
                    buf.push(END);
                }
                Ok(quick_xml::events::Event::Eof) => break,
                Err(e) => return Err(anyhow!("XML Encode Error: {:?}", e)),
                _ => {}
            }
            buf_event.clear();
        }
        Ok(buf)
    }
}

#[cfg(test)]
mod tests {
    use super::Wbxml;

    #[test]
    fn round_trip_sync_with_namespaces() {
        let codec = Wbxml::new();
        let xml = r#"<?xml version=\"1.0\" encoding=\"utf-8\"?><Sync xmlns=\"AirSync:\"><Collections><Collection><SyncKey>1</SyncKey><CollectionId>1</CollectionId><Commands></Commands></Collection></Collections></Sync>"#;
        let wb = codec.encode(xml).expect("encode");
        let dec = codec.decode(&wb).expect("decode");
        assert!(dec.contains("<Sync>"));
        assert!(dec.contains("<SyncKey>1</SyncKey>"));
    }

    #[test]
    fn round_trip_folder_sync_and_provision() {
        let codec = Wbxml::new();
        let folder = r#"<FolderSync xmlns=\"FolderHierarchy:\"><Status>1</Status><SyncKey>1</SyncKey><Changes><Count>1</Count></Changes></FolderSync>"#;
        let wb_folder = codec.encode(folder).expect("encode folder");
        let folder_dec = codec.decode(&wb_folder).expect("decode folder");
        assert!(folder_dec.contains("<FolderSync>"));

        let prov = r#"<Provision xmlns=\"Provision:\"><Status>1</Status><Policies><Policy><PolicyType>MS-EAS-Provisioning-WBXML</PolicyType><PolicyKey>1</PolicyKey></Policy></Policies></Provision>"#;
        let wb_prov = codec.encode(prov).expect("encode provision");
        let prov_dec = codec.decode(&wb_prov).expect("decode provision");
        assert!(prov_dec.contains("<Provision>"));
    }
}
