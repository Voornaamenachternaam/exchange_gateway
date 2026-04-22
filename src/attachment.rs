// src/attachment.rs
use crate::protocol_fixtures::{EWS_MSG_NS, EWS_TYPE_NS};
use crate::storage::Storage;
use crate::util::xml_escape;
use anyhow::{Result, anyhow};
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use quick_xml::Reader;
use quick_xml::events::Event;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

const MAX_ATTACHMENT_NAME_LEN: usize = 255;
const MAX_CONTENT_TYPE_LEN: usize = 256;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum AttachmentType {
    File,
    Item,
}

impl AttachmentType {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::File => "file",
            Self::Item => "item",
        }
    }

    pub fn attachment_type_from_str(s: &str) -> Self {
        match s.to_ascii_lowercase().as_str() {
            "item" => Self::Item,
            _ => Self::File,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AttachmentRecord {
    pub id: String,
    pub parent_item_server_id: String,
    pub owner: String,
    pub name: String,
    pub content_type: String,
    pub content_size: i64,
    pub content_base64: String,
    pub is_inline: bool,
    pub content_id: Option<String>,
    pub content_location: Option<String>,
    pub attachment_type: String,
    pub last_modified_time: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Clone, Debug)]
pub struct FileAttachment {
    pub id: String,
    pub parent_item_server_id: String,
    pub owner: String,
    pub name: String,
    pub content_type: String,
    pub content_size: i64,
    pub content_base64: String,
    pub is_inline: bool,
    pub content_id: Option<String>,
    pub content_location: Option<String>,
    pub last_modified_time: Option<String>,
}

impl FileAttachment {
    pub fn from_record(rec: &AttachmentRecord) -> Self {
        Self {
            id: rec.id.clone(),
            parent_item_server_id: rec.parent_item_server_id.clone(),
            owner: rec.owner.clone(),
            name: rec.name.clone(),
            content_type: rec.content_type.clone(),
            content_size: rec.content_size,
            content_base64: rec.content_base64.clone(),
            is_inline: rec.is_inline,
            content_id: rec.content_id.clone(),
            content_location: rec.content_location.clone(),
            last_modified_time: rec.last_modified_time.clone(),
        }
    }

    pub fn to_record(&self) -> AttachmentRecord {
        AttachmentRecord {
            id: self.id.clone(),
            parent_item_server_id: self.parent_item_server_id.clone(),
            owner: self.owner.clone(),
            name: self.name.clone(),
            content_type: self.content_type.clone(),
            content_size: self.content_size,
            content_base64: self.content_base64.clone(),
            is_inline: self.is_inline,
            content_id: self.content_id.clone(),
            content_location: self.content_location.clone(),
            attachment_type: AttachmentType::File.as_str().to_string(),
            last_modified_time: self.last_modified_time.clone(),
            created_at: String::new(),
            updated_at: String::new(),
        }
    }
}

pub struct AttachmentManager {
    storage: Arc<Storage>,
    max_attachment_bytes: usize,
}

impl AttachmentManager {
    pub fn new(storage: Arc<Storage>, max_attachment_bytes: usize) -> Self {
        Self {
            storage,
            max_attachment_bytes,
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn create_file_attachment(
        &self,
        owner: &str,
        parent_item_server_id: &str,
        name: &str,
        content_type: &str,
        content_base64: &str,
        is_inline: bool,
        content_id: Option<&str>,
        content_location: Option<&str>,
    ) -> Result<FileAttachment> {
        let name = if name.is_empty() {
            "attachment.dat".to_string()
        } else if name.len() > MAX_ATTACHMENT_NAME_LEN {
            let end = name.char_indices().map(|(i, _)| i).take_while(|&i| i <= MAX_ATTACHMENT_NAME_LEN).last().unwrap_or(0);
            name[..end].to_string()
            name.to_string()
        };

        let content_type = if content_type.is_empty() {
            "application/octet-stream".to_string()
        } else if content_type.len() > MAX_CONTENT_TYPE_LEN {
            content_type[..MAX_CONTENT_TYPE_LEN].to_string()
        } else {
            content_type.to_string()
        };

        let decoded_len = STANDARD
            .decode(content_base64)
            .map(|v| v.len())
            .unwrap_or(0);

        if decoded_len > self.max_attachment_bytes {
            return Err(anyhow!(
                "Attachment size {} exceeds maximum allowed size {}",
                decoded_len,
                self.max_attachment_bytes
            ));
        }

        let id = uuid::Uuid::new_v4().to_string();
        let now = chrono::Utc::now().to_rfc3339();

        let attachment = FileAttachment {
            id: id.clone(),
            parent_item_server_id: parent_item_server_id.to_string(),
            owner: owner.to_string(),
            name: name.clone(),
            content_type: content_type.clone(),
            content_size: decoded_len as i64,
            content_base64: content_base64.to_string(),
            is_inline,
            content_id: content_id.map(String::from),
            content_location: content_location.map(String::from),
            last_modified_time: Some(now.clone()),
        };

        self.storage
            .upsert_calendar_attachment(&attachment.to_record())
            .await?;

        Ok(attachment)
    }

    pub async fn get_attachment(
        &self,
        owner: &str,
        attachment_id: &str,
    ) -> Result<Option<FileAttachment>> {
        let rec = self
            .storage
            .get_calendar_attachment(owner, attachment_id)
            .await?;
        Ok(rec.as_ref().map(FileAttachment::from_record))
    }

    pub async fn get_attachments_for_item(
        &self,
        owner: &str,
        parent_item_server_id: &str,
    ) -> Result<Vec<FileAttachment>> {
        let recs = self
            .storage
            .get_calendar_attachments_for_item(owner, parent_item_server_id)
            .await?;
        Ok(recs.iter().map(FileAttachment::from_record).collect())
    }

    pub async fn delete_attachment(
        &self,
        owner: &str,
        attachment_id: &str,
    ) -> Result<Option<String>> {
        let rec = self
            .storage
            .get_calendar_attachment(owner, attachment_id)
            .await?;
        let parent_id = rec.as_ref().map(|r| r.parent_item_server_id.clone());
        self.storage
            .delete_calendar_attachment(owner, attachment_id)
            .await?;
        Ok(parent_id)
    }
}

pub fn parse_create_attachment_request(xml: &str) -> Option<ParsedCreateAttachment> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);
    let mut buf = Vec::new();
    let mut in_file_attachment = false;
    let mut in_item_attachment = false;
    let mut in_name = false;
    let mut in_content_type = false;
    let mut in_content = false;
    let mut in_is_inline = false;
    let mut in_content_id = false;
    let mut in_content_location = false;

    let mut parent_item_id = None;
    let mut name = String::new();
    let mut content_type = String::new();
    let mut content_base64 = String::new();
    let mut is_inline = false;
    let mut content_id = None;
    let mut content_location = None;
    let mut attachment_type = AttachmentType::File;

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) => {
                let local = e.name().local_name();
                match local.as_ref() {
                    b"ParentItemId" => {
                        for attr in e.attributes().flatten() {
                            if attr.key.local_name().as_ref() == b"Id"
                        for attr in e.attributes().flatten() {
                            if attr.key.local_name().as_ref() == b"Id" {
                                match attr.decode_and_unescape_value(reader.decoder()) {
                                    Ok(v) => parent_item_id = Some(v.into_owned()),
                                    Err(e) => log::error!("Failed to decode Id attribute: {}", e),
                                }
                            }
                        }
                    }
                    b"ItemId" if parent_item_id.is_none() => {
                        for attr in e.attributes().flatten() {
                            if attr.key.local_name().as_ref() == b"Id"
                                && let Ok(v) = attr.decode_and_unescape_value(reader.decoder())
                            {
                                parent_item_id = Some(v.into_owned());
                            }
                        }
                    }
                    b"FileAttachment" => {
                        in_file_attachment = true;
                        attachment_type = AttachmentType::File;
                    }
                    b"ItemAttachment" => {
                        in_item_attachment = true;
                        attachment_type = AttachmentType::Item;
                    }
                    b"Name" if in_file_attachment || in_item_attachment => {
                        in_name = true;
                    }
                    b"ContentType" if in_file_attachment || in_item_attachment => {
                        in_content_type = true;
                    }
                    b"Content" if in_file_attachment => {
                        in_content = true;
                    }
                    b"IsInline" if in_file_attachment || in_item_attachment => {
                        in_is_inline = true;
                    }
                    b"ContentId" if in_file_attachment || in_item_attachment => {
                        in_content_id = true;
                    }
                    b"ContentLocation" if in_file_attachment || in_item_attachment => {
                        in_content_location = true;
                    }
                    _ => {}
                }
            }
            Ok(Event::Empty(e)) => {
                let local = e.name().local_name();
                if (local.as_ref() == b"ParentItemId" || local.as_ref() == b"ItemId")
                    && parent_item_id.is_none()
                {
                    for attr in e.attributes().flatten() {
                        if attr.key.local_name().as_ref() == b"Id"
                            && let Ok(v) = attr.decode_and_unescape_value(reader.decoder())
                        {
                            parent_item_id = Some(v.into_owned());
                        }
                    }
                }
            }
            Ok(Event::Text(e)) => {
                if let Ok(text) = e.decode() {
                    if in_name {
                        name = text.into_owned();
                    } else if in_content_type {
                        content_type = text.into_owned();
                    } else if in_content {
                        content_base64 = text.into_owned();
                    } else if in_is_inline {
                        is_inline = text.eq_ignore_ascii_case("true");
                    } else if in_content_id {
                        content_id = Some(text.into_owned());
                    } else if in_content_location {
                        content_location = Some(text.into_owned());
                    }
                }
            }
            Ok(Event::End(e)) => {
                let local = e.name().local_name();
                match local.as_ref() {
                    b"FileAttachment" => {
                        in_file_attachment = false;
                    }
                    b"ItemAttachment" => {
                        in_item_attachment = false;
                    }
                    b"Name" => {
                        in_name = false;
                    }
                    b"ContentType" => {
                        in_content_type = false;
                    }
                    b"Content" => {
                        in_content = false;
                    }
                    b"IsInline" => {
                        in_is_inline = false;
                    }
                    b"ContentId" => {
                        in_content_id = false;
                    }
                    b"ContentLocation" => {
                        in_content_location = false;
                    }
                    _ => {}
                }
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
        buf.clear();
    }

    let parent_id = parent_item_id.unwrap_or_default();
    if parent_id.is_empty() {
        return None;
    }

    Some(ParsedCreateAttachment {
        parent_item_id: parent_id,
        name,
        content_type,
        content_base64,
        is_inline,
        content_id,
        content_location,
        attachment_type,
    })
}

pub struct ParsedCreateAttachment {
    pub parent_item_id: String,
    pub name: String,
    pub content_type: String,
    pub content_base64: String,
    pub is_inline: bool,
    pub content_id: Option<String>,
    pub content_location: Option<String>,
    pub attachment_type: AttachmentType,
}

pub fn parse_get_attachment_request(xml: &str) -> Vec<String> {
    let mut ids = Vec::new();
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);
    let mut buf = Vec::new();

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) | Ok(Event::Empty(e)) => {
                let local = e.name().local_name();
                if local.as_ref() == b"AttachmentId" || local.as_ref() == b"RequestAttachmentId" {
                    for attr in e.attributes().flatten() {
                        if attr.key.local_name().as_ref() == b"Id"
                            && let Ok(v) = attr.decode_and_unescape_value(reader.decoder())
                        {
                            ids.push(v.into_owned());
                        }
                    }
                }
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
        buf.clear();
    }

    ids
}

pub fn parse_delete_attachment_request(xml: &str) -> Option<ParsedDeleteAttachment> {
    let mut attachment_ids = Vec::new();
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);
    let mut buf = Vec::new();

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) | Ok(Event::Empty(e)) => {
                let local = e.name().local_name();
                if local.as_ref() == b"AttachmentId" {
                    for attr in e.attributes().flatten() {
                        if attr.key.local_name().as_ref() == b"Id"
                            && let Ok(v) = attr.decode_and_unescape_value(reader.decoder())
                        {
                            attachment_ids.push(v.into_owned());
                        }
                    }
                }
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
        buf.clear();
    }

    attachment_ids.first().map(|id| ParsedDeleteAttachment {
        attachment_id: id.clone(),
    })
}

pub struct ParsedDeleteAttachment {
    pub attachment_id: String,
}

pub fn render_file_attachment_xml(attachment: &FileAttachment, include_content: bool) -> String {
    let content_xml = if include_content {
        format!(
            "<t:Content>{}</t:Content>",
            xml_escape(&attachment.content_base64)
        )
    } else {
        String::new()
    };

    let is_inline_str = if attachment.is_inline {
        "true"
    } else {
        "false"
    };

    let content_id_xml = attachment
        .content_id
        .as_ref()
        .map(|cid| format!("<t:ContentId>{}</t:ContentId>", xml_escape(cid)))
        .unwrap_or_default();

    let content_location_xml = attachment
        .content_location
        .as_ref()
        .map(|cl| format!("<t:ContentLocation>{}</t:ContentLocation>", xml_escape(cl)))
        .unwrap_or_default();

    let last_modified_xml = attachment
        .last_modified_time
        .as_ref()
        .map(|t| format!("<t:LastModifiedTime>{}</t:LastModifiedTime>", xml_escape(t)))
        .unwrap_or_default();

    format!(
        r#"<t:FileAttachment>
            <t:AttachmentId Id="{}"/>
            <t:Name>{}</t:Name>
            <t:ContentType>{}</t:ContentType>
            {}
            <t:Size>{}</t:Size>
            <t:IsInline>{}</t:IsInline>
            {}
            {}
            {}
        </t:FileAttachment>"#,
        xml_escape(&attachment.id),
        xml_escape(&attachment.name),
        xml_escape(&attachment.content_type),
        content_xml,
        attachment.content_size,
        is_inline_str,
        content_id_xml,
        content_location_xml,
        last_modified_xml,
    )
}

pub fn render_create_attachment_response(attachment_id: &str, parent_item_id: &str) -> String {
    format!(
        r#"<m:CreateAttachmentResponse xmlns:m="{}" xmlns:t="{}">
            <m:ResponseMessages>
                <m:CreateAttachmentResponseMessage ResponseClass="Success">
                    <m:ResponseCode>NoError</m:ResponseCode>
                    <m:Attachments>
                        <t:FileAttachment>
                            <t:AttachmentId Id="{}" RootItemId="{}" RootItemChangeKey="01"/>
                        </t:FileAttachment>
                    </m:Attachments>
                </m:CreateAttachmentResponseMessage>
            </m:ResponseMessages>
        </m:CreateAttachmentResponse>"#,
        EWS_MSG_NS,
        EWS_TYPE_NS,
        xml_escape(attachment_id),
        xml_escape(parent_item_id),
    )
}

pub fn render_get_attachment_response(attachments_xml: &str) -> String {
    format!(
        r#"<m:GetAttachmentResponse xmlns:m="{}" xmlns:t="{}">
            <m:ResponseMessages>
                <m:GetAttachmentResponseMessage ResponseClass="Success">
                    <m:ResponseCode>NoError</m:ResponseCode>
                    <m:Attachments>
                        {}
                    </m:Attachments>
                </m:GetAttachmentResponseMessage>
            </m:ResponseMessages>
        </m:GetAttachmentResponse>"#,
        EWS_MSG_NS, EWS_TYPE_NS, attachments_xml,
    )
}

pub fn render_delete_attachment_response(root_item_id: &str) -> String {
    format!(
        r#"<m:DeleteAttachmentResponse xmlns:m="{}" xmlns:t="{}">
            <m:ResponseMessages>
                <m:DeleteAttachmentResponseMessage ResponseClass="Success">
                    <m:ResponseCode>NoError</m:ResponseCode>
                    <m:RootItemId RootItemId="{}" RootItemChangeKey="01"/>
                </m:DeleteAttachmentResponseMessage>
            </m:ResponseMessages>
        </m:DeleteAttachmentResponse>"#,
        EWS_MSG_NS,
        EWS_TYPE_NS,
        xml_escape(root_item_id),
    )
}

pub fn render_attachment_error_response(code: &str, message: &str) -> String {
    format!(
        r#"<m:ResponseMessages xmlns:m="{}" xmlns:t="{}">
            <m:CreateAttachmentResponseMessage ResponseClass="Error">
                <m:MessageText>{}</m:MessageText>
                <m:ResponseCode>{}</m:ResponseCode>
                <m:DescriptiveLinkKey>0</m:DescriptiveLinkKey>
            </m:CreateAttachmentResponseMessage>
        </m:ResponseMessages>"#,
        EWS_MSG_NS,
        EWS_TYPE_NS,
        xml_escape(message),
        xml_escape(code),
    )
}
