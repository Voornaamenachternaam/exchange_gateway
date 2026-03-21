// src/wbxml.rs
use anyhow::{Result, anyhow};
use std::collections::HashMap;

const SWITCH_PAGE: u8 = 0x00;
const END: u8 = 0x01;
const ENTITY: u8 = 0x02;
const STR_I: u8 = 0x03;
const LITERAL: u8 = 0x04;
const STR_T: u8 = 0x83;
const OPAQUE: u8 = 0xC3;

lazy_static::lazy_static! {
    static ref TAG_TO_NAME: HashMap<(u8, u8), &'static str> = {
        let mut m = HashMap::new();

        // Code Page 0: AirSync
        m.insert((0, 0x05), "Sync");
        m.insert((0, 0x06), "Responses");
        m.insert((0, 0x07), "Add");
        m.insert((0, 0x08), "Change");
        m.insert((0, 0x09), "Delete");
        m.insert((0, 0x0A), "Fetch");
        m.insert((0, 0x0B), "SyncKey");
        m.insert((0, 0x0C), "ClientId");
        m.insert((0, 0x0D), "ServerId");
        m.insert((0, 0x0E), "Status");
        m.insert((0, 0x0F), "Collection");
        m.insert((0, 0x10), "Class");
        m.insert((0, 0x12), "CollectionId");
        m.insert((0, 0x16), "Commands");
        m.insert((0, 0x17), "Options");
        m.insert((0, 0x1B), "Conflict");
        m.insert((0, 0x1C), "Collections");
        m.insert((0, 0x1D), "ApplicationData");
        m.insert((0, 0x27), "ConversationMode");


        // Code Page 1: Contacts (ASCNTC subset)
        m.insert((1, 0x05), "Contacts:Anniversary");
        m.insert((1, 0x08), "Contacts:Birthday");
        m.insert((1, 0x13), "Contacts:BusinessPhoneNumber");
        m.insert((1, 0x19), "Contacts:CompanyName");
        m.insert((1, 0x1B), "Contacts:Email1Address");
        m.insert((1, 0x1F), "Contacts:FirstName");
        m.insert((1, 0x29), "Contacts:LastName");
        m.insert((1, 0x2B), "Contacts:MobilePhoneNumber");

        // Code Page 2: Email (subset for ASEMAL)
        m.insert((2, 0x05), "Email:Attachment");
        m.insert((2, 0x06), "Email:Attachments");
        m.insert((2, 0x07), "Email:AttName");
        m.insert((2, 0x08), "Email:AttSize");
        m.insert((2, 0x14), "Email:Subject");
        m.insert((2, 0x16), "Email:To");
        m.insert((2, 0x17), "Email:Cc");
        m.insert((2, 0x18), "Email:From");
        m.insert((2, 0x0C), "Email:Body");

        // Code Page 4: Calendar (ASCAL)
        m.insert((4, 0x05), "Calendar:Timezone");
        m.insert((4, 0x06), "Calendar:AllDayEvent");
        m.insert((4, 0x07), "Calendar:Attendees");
        m.insert((4, 0x08), "Calendar:Attendee");
        m.insert((4, 0x09), "Calendar:Email");
        m.insert((4, 0x0A), "Calendar:Name");
        m.insert((4, 0x0D), "Calendar:BusyStatus");
        m.insert((4, 0x0E), "Calendar:Categories");
        m.insert((4, 0x0F), "Calendar:Category");
        m.insert((4, 0x11), "Calendar:DtStamp");
        m.insert((4, 0x12), "Calendar:EndTime");
        m.insert((4, 0x13), "Calendar:Exception");
        m.insert((4, 0x14), "Calendar:Exceptions");
        m.insert((4, 0x15), "Calendar:Deleted");
        m.insert((4, 0x16), "Calendar:ExceptionStartTime");
        m.insert((4, 0x17), "Calendar:Location");
        m.insert((4, 0x18), "Calendar:MeetingStatus");
        m.insert((4, 0x19), "Calendar:OrganizerEmail");
        m.insert((4, 0x1A), "Calendar:OrganizerName");
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
        m.insert((4, 0x29), "Calendar:AttendeeStatus");
        m.insert((4, 0x2A), "Calendar:AttendeeType");
        m.insert((4, 0x33), "Calendar:DisallowNewTimeProposal");
        m.insert((4, 0x34), "Calendar:ResponseRequested");
        m.insert((4, 0x35), "Calendar:AppointmentReplyTime");
        m.insert((4, 0x36), "Calendar:ResponseType");
        m.insert((4, 0x37), "Calendar:CalendarType");
        m.insert((4, 0x38), "Calendar:IsLeapMonth");
        m.insert((4, 0x39), "Calendar:FirstDayOfWeek");
        m.insert((4, 0x3A), "Calendar:OnlineMeetingConfLink");
        m.insert((4, 0x3B), "Calendar:OnlineMeetingExternalLink");
        m.insert((4, 0x3C), "Calendar:ClientUid");

        // Code Page 5: Move
        m.insert((5, 0x05), "MoveItems");
        m.insert((5, 0x06), "Move");
        m.insert((5, 0x07), "SrcMsgId");
        m.insert((5, 0x08), "SrcFldId");
        m.insert((5, 0x09), "DstFldId");
        m.insert((5, 0x0A), "Response");
        m.insert((5, 0x0B), "Status");

        // Code Page 6: GetItemEstimate
        m.insert((6, 0x05), "GetItemEstimate");
        m.insert((6, 0x06), "Version");
        m.insert((6, 0x07), "Collections");
        m.insert((6, 0x08), "Collection");
        m.insert((6, 0x09), "Class");
        m.insert((6, 0x0A), "CollectionId");
        m.insert((6, 0x0B), "DateTime");
        m.insert((6, 0x0C), "Estimate");
        m.insert((6, 0x0D), "Response");
        m.insert((6, 0x0E), "Status");

        // Code Page 8: MeetingResponse
        m.insert((8, 0x05), "MeetingResponse");
        m.insert((8, 0x06), "Request");
        m.insert((8, 0x07), "Result");
        m.insert((8, 0x08), "Status");
        m.insert((8, 0x09), "UserResponse");
        m.insert((8, 0x0A), "InstanceId");

        // Code Page 10: ResolveRecipients
        m.insert((10, 0x05), "ResolveRecipients");
        m.insert((10, 0x06), "Response");
        m.insert((10, 0x07), "Status");
        m.insert((10, 0x08), "Type");
        m.insert((10, 0x09), "Recipient");

        // Code Page 11: ValidateCert
        m.insert((11, 0x05), "ValidateCert");
        m.insert((11, 0x06), "Certificates");
        m.insert((11, 0x07), "Certificate");
        m.insert((11, 0x0B), "Status");

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

        // Code Page 17: AirSyncBase
        m.insert((17, 0x05), "AirSyncBase:BodyPreference");
        m.insert((17, 0x06), "AirSyncBase:Type");
        m.insert((17, 0x07), "AirSyncBase:TruncationSize");
        m.insert((17, 0x0A), "AirSyncBase:Body");
        m.insert((17, 0x0B), "AirSyncBase:Data");
        m.insert((17, 0x0C), "AirSyncBase:EstimatedDataSize");
        m.insert((17, 0x0D), "AirSyncBase:Truncated");

        // Code Page 18: Settings
        m.insert((18, 0x05), "Settings");
        m.insert((18, 0x06), "Status");
        m.insert((18, 0x1A), "EnableOutboundSMS");


        // Code Page 9: Tasks (ASTASK subset)
        m.insert((9, 0x08), "Tasks:Complete");
        m.insert((9, 0x09), "Tasks:DateCompleted");
        m.insert((9, 0x0D), "Tasks:DueDate");
        m.insert((9, 0x0F), "Tasks:Importance");
        m.insert((9, 0x17), "Tasks:StartDate");
        m.insert((9, 0x18), "Tasks:Subject");

        // Code Page 19: DocumentLibrary (ASDOC subset)
        m.insert((19, 0x05), "DocumentLibrary:LinkId");
        m.insert((19, 0x06), "DocumentLibrary:DisplayName");
        m.insert((19, 0x07), "DocumentLibrary:IsFolder");
        m.insert((19, 0x08), "DocumentLibrary:CreationDate");
        m.insert((19, 0x09), "DocumentLibrary:LastModifiedDate");

        // Code Page 23: Notes (ASNOTE subset)
        m.insert((23, 0x05), "Notes:Subject");
        m.insert((23, 0x06), "Notes:MessageClass");
        m.insert((23, 0x07), "Notes:LastModifiedDate");
        m.insert((23, 0x08), "Notes:Categories");
        m.insert((23, 0x0B), "Notes:Body");

        // Code Page 24: RightsManagement (ASRM subset)
        m.insert((24, 0x05), "RightsManagement:RightsManagementSupport");
        m.insert((24, 0x06), "RightsManagement:RightsManagementTemplates");
        m.insert((24, 0x08), "RightsManagement:RightsManagementLicense");

        // Code Page 21: ComposeMail (ASEMAIL)
        m.insert((21, 0x05), "SendMail");
        m.insert((21, 0x06), "SmartForward");
        m.insert((21, 0x07), "SmartReply");
        m.insert((21, 0x08), "SaveInSentItems");
        m.insert((21, 0x0B), "Mime");

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

    fn read_mb_uint(bytes: &[u8], pos: &mut usize) -> Result<u32> {
        let mut result: u32 = 0;
        let mut count = 0;
        loop {
            if *pos >= bytes.len() {
                return Err(anyhow!("Truncated WBXML mb_u_int32"));
            }
            let b = bytes[*pos];
            *pos += 1;
            result = (result << 7) | u32::from(b & 0x7F);
            count += 1;
            if (b & 0x80) == 0 {
                break;
            }
            if count > 5 {
                return Err(anyhow!("WBXML mb_u_int32 too large"));
            }
        }
        Ok(result)
    }

    fn read_inline_str(bytes: &[u8], pos: &mut usize) -> Result<String> {
        let start = *pos;
        while *pos < bytes.len() && bytes[*pos] != 0 {
            *pos += 1;
        }
        if *pos >= bytes.len() {
            return Err(anyhow!("Unterminated STR_I string"));
        }
        let s = String::from_utf8(bytes[start..*pos].to_vec())?;
        *pos += 1; // null terminator
        Ok(s)
    }

    pub fn decode(&self, bytes: &[u8]) -> Result<String> {
        if bytes.is_empty() {
            return Err(anyhow!("Empty WBXML payload"));
        }
        if bytes[0] == b'<' {
            return Ok(String::from_utf8(bytes.to_vec())?);
        }

        let mut pos = 0;
        let _version = *bytes
            .get(pos)
            .ok_or_else(|| anyhow!("Missing WBXML version"))?;
        pos += 1;
        let _public_id = Self::read_mb_uint(bytes, &mut pos)?;
        let _charset = Self::read_mb_uint(bytes, &mut pos)?;
        let str_table_len = usize::try_from(Self::read_mb_uint(bytes, &mut pos)?)
            .map_err(|_| anyhow!("Invalid WBXML string table length"))?;

        if pos + str_table_len > bytes.len() {
            return Err(anyhow!("WBXML string table exceeds payload"));
        }
        let string_table = &bytes[pos..pos + str_table_len];
        pos += str_table_len;

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
                        return Err(anyhow!("WBXML SWITCH_PAGE missing code page"));
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
                    let content = Self::read_inline_str(bytes, &mut pos)?;
                    let escaped = content
                        .replace("&", "&amp;")
                        .replace("<", "&lt;")
                        .replace(">", "&gt;");
                    output.push_str(&escaped);
                }
                STR_T => {
                    let offset = usize::try_from(Self::read_mb_uint(bytes, &mut pos)?)
                        .map_err(|_| anyhow!("Invalid STR_T offset"))?;
                    if offset >= string_table.len() {
                        return Err(anyhow!("STR_T offset outside string table"));
                    }
                    let mut end = offset;
                    while end < string_table.len() && string_table[end] != 0 {
                        end += 1;
                    }
                    let content = String::from_utf8(string_table[offset..end].to_vec())?;
                    let escaped = content
                        .replace("&", "&amp;")
                        .replace("<", "&lt;")
                        .replace(">", "&gt;");
                    output.push_str(&escaped);
                }
                ENTITY => {
                    let ent = Self::read_mb_uint(bytes, &mut pos)?;
                    output.push_str(&format!("&#{};", ent));
                }
                OPAQUE => {
                    let len = usize::try_from(Self::read_mb_uint(bytes, &mut pos)?)
                        .map_err(|_| anyhow!("Invalid OPAQUE length"))?;
                    if pos + len > bytes.len() {
                        return Err(anyhow!("OPAQUE data exceeds payload"));
                    }
                    let opaque = &bytes[pos..pos + len];
                    pos += len;
                    let b64 =
                        base64::Engine::encode(&base64::engine::general_purpose::STANDARD, opaque);
                    output.push_str(&b64);
                }
                LITERAL => {
                    return Err(anyhow!("LITERAL token unsupported in this profile"));
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

        while let Some(tag) = xml_stack.pop() {
            output.push_str(&format!("</{}>", tag));
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
