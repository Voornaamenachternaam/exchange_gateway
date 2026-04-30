// src/attachment.rs
use crate::protocol_fixtures::{EWS_MSG_NS, EWS_TYPE_NS};
use crate::storage::Storage;
use crate::util::xml_escape;
use anyhow::{Result, anyhow};
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use mime::Mime;
use quick_xml::Reader;
use quick_xml::events::Event;
use serde::{Deserialize, Serialize};
use std::fmt::Write;
use std::sync::Arc;

const MAX_ATTACHMENT_NAME_LEN: usize = 255;
const MAX_CONTENT_TYPE_LEN: usize = 256;

const DANGEROUS_EXTENSIONS: &[&str] = &[
    "exe",
    "dll",
    "bat",
    "cmd",
    "com",
    "cpl",
    "gadget",
    "hta",
    "inf",
    "ins",
    "iso",
    "isp",
    "js",
    "jse",
    "lnk",
    "msc",
    "msi",
    "msp",
    "mst",
    "pif",
    "ps1",
    "ps2",
    "psm1",
    "psd1",
    "py",
    "pyc",
    "pyz",
    "pyzw",
    "scr",
    "sct",
    "shb",
    "shs",
    "vb",
    "vbe",
    "vbs",
    "vxd",
    "wsh",
    "ws",
    "wsc",
    "wsf",
    "application",
    "appx",
    "msix",
];

const RESERVED_WINDOWS_NAMES: &[&str] = &[
    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
    "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
];

const ALLOWED_MIME_TOP_LEVEL: &[&str] = &["text", "image", "audio", "video", "application"];

const DANGEROUS_MIME_TYPES: &[&str] = &[
    "text/html",
    "text/javascript",
    "text/x-javascript",
    "application/javascript",
    "application/x-javascript",
    "image/svg+xml",
];

const ALLOWED_APPLICATION_SUBTYPES: &[&str] = &[
    "pdf",
    "json",
    "xml",
    "zip",
    "gzip",
    "x-gzip",
    "x-tar",
    "vnd.openxmlformats-officedocument.spreadsheetml.sheet",
    "vnd.openxmlformats-officedocument.wordprocessingml.document",
    "vnd.openxmlformats-officedocument.presentationml.presentation",
    "vnd.ms-excel",
    "vnd.ms-word",
    "vnd.ms-powerpoint",
    "vnd.ms-outlook",
    "msword",
    "vnd.oasis.opendocument.text",
    "vnd.oasis.opendocument.spreadsheet",
    "vnd.oasis.opendocument.presentation",
    "rtf",
    "x-rtf",
    "octet-stream",
];

const MIME_EXTENSION_MAP: &[(&str, &str)] = &[
    ("txt", "text/plain"),
    ("csv", "text/csv"),
    ("htm", "text/html"),
    ("html", "text/html"),
    ("css", "text/css"),
    ("ics", "text/calendar"),
    ("xml", "application/xml"),
    ("json", "application/json"),
    ("pdf", "application/pdf"),
    ("rtf", "application/rtf"),
    ("zip", "application/zip"),
    ("gz", "application/gzip"),
    ("tar", "application/x-tar"),
    (
        "xlsx",
        "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
    ),
    ("xlsm", "application/vnd.ms-excel.sheet.macroEnabled.12"),
    (
        "xlsb",
        "application/vnd.ms-excel.sheet.binary.macroEnabled.12",
    ),
    ("xls", "application/vnd.ms-excel"),
    (
        "docx",
        "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
    ),
    ("doc", "application/msword"),
    ("docm", "application/vnd.ms-word.document.macroEnabled.12"),
    (
        "pptx",
        "application/vnd.openxmlformats-officedocument.presentationml.presentation",
    ),
    ("ppt", "application/vnd.ms-powerpoint"),
    (
        "pptm",
        "application/vnd.ms-powerpoint.presentation.macroEnabled.12",
    ),
    ("odt", "application/vnd.oasis.opendocument.text"),
    ("ods", "application/vnd.oasis.opendocument.spreadsheet"),
    ("odp", "application/vnd.oasis.opendocument.presentation"),
    ("png", "image/png"),
    ("jpg", "image/jpeg"),
    ("jpeg", "image/jpeg"),
    ("gif", "image/gif"),
    ("bmp", "image/bmp"),
    ("webp", "image/webp"),
    ("ico", "image/x-icon"),
    ("tiff", "image/tiff"),
    ("tif", "image/tiff"),
    ("mp3", "audio/mpeg"),
    ("wav", "audio/wav"),
    ("ogg", "audio/ogg"),
    ("mp4", "video/mp4"),
    ("mpeg", "video/mpeg"),
    ("webm", "video/webm"),
    ("avi", "video/x-msvideo"),
    ("eml", "message/rfc822"),
    ("msg", "application/vnd.ms-outlook"),
    ("vcf", "text/vcard"),
    ("dat", "application/octet-stream"),
    ("bin", "application/octet-stream"),
];

pub fn sanitize_filename(name: &str) -> String {
    let trimmed = name.trim();
    let trimmed = trimmed.strip_prefix('.').unwrap_or(trimmed);

    let sanitized: String = trimmed
        .chars()
        .map(|c| {
            if (u32::from(c) <= 0x1f)
                || c == '/'
                || c == '\\'
                || c == ':'
                || c == '*'
                || c == '?'
                || c == '"'
                || c == '<'
                || c == '>'
                || c == '|'
            {
                '_'
            } else {
                c
            }
        })
        .collect();

    let (base_name, extension) = match sanitized.rsplit_once('.') {
        Some((b, e)) => (b.to_string(), e.to_string()),
        None => (sanitized, String::new()),
    };

    let stem = match base_name.as_str() {
        "" | "." | ".." => "attachment",
        s => s,
    };

    let stem_upper = stem.to_ascii_uppercase();
    let stem_safe = if RESERVED_WINDOWS_NAMES.contains(&stem_upper.as_str()) {
        format!("{stem}_file")
    } else {
        stem.to_string()
    };

    let result = if extension.is_empty() {
        stem_safe
    } else {
        format!("{stem_safe}.{extension}")
    };

    let result = result.trim_end_matches(['.', ' ']).to_string();

    if result.is_empty() {
        "attachment".to_string()
    } else {
        result
    }
}

pub fn is_dangerous_extension(name: &str) -> bool {
    let ext = name
        .rsplit_once('.')
        .map(|(_, e)| e.to_ascii_lowercase())
        .unwrap_or_default();
    DANGEROUS_EXTENSIONS.contains(&ext.as_str())
}

pub fn validate_mime_type(content_type: &str) -> Result<Mime> {
    let mime: Mime = content_type
        .parse()
        .map_err(|_| anyhow!("invalid MIME type: {}", content_type))?;

    let normalised = format!("{}/{}", mime.type_().as_str(), mime.subtype().as_str());
    if DANGEROUS_MIME_TYPES.contains(&normalised.as_str()) {
        return Err(anyhow!(
            "MIME type '{}' is not allowed for security reasons",
            normalised
        ));
    }

    let top = mime.type_().as_str();
    if !ALLOWED_MIME_TOP_LEVEL.contains(&top) {
        return Err(anyhow!("MIME top-level type '{}' is not allowed", top));
    }
    if top == "application" {
        let sub = mime.subtype().as_str();
        if !ALLOWED_APPLICATION_SUBTYPES.contains(&sub) {
            return Err(anyhow!("application/{} MIME subtype is not allowed", sub));
        }
    }
    Ok(mime)
}

pub fn mime_type_for_filename(name: &str) -> &'static str {
    let ext = name
        .rsplit_once('.')
        .map(|(_, e)| e.to_ascii_lowercase())
        .unwrap_or_default();
    MIME_EXTENSION_MAP
        .iter()
        .find(|&&(e, _)| e == ext)
        .map(|&(_, m)| m)
        .unwrap_or("application/octet-stream")
}

pub fn validate_attachment_name(name: &str) -> Result<String> {
    if name.is_empty() {
        return Ok("attachment.dat".to_string());
    }
    if name.len() > MAX_ATTACHMENT_NAME_LEN {
        let mut end = MAX_ATTACHMENT_NAME_LEN;
        while !name.is_char_boundary(end) {
            end -= 1;
        }
        let truncated = &name[..end];
        let sanitized = sanitize_filename(truncated);
        if is_dangerous_extension(&sanitized) {
            return Err(anyhow!(
                "file extension is not allowed for security reasons"
            ));
        }
        return Ok(sanitized);
    }
    let sanitized = sanitize_filename(name);
    if is_dangerous_extension(&sanitized) {
        return Err(anyhow!(
            "file extension is not allowed for security reasons"
        ));
    }
    Ok(sanitized)
}

fn normalize_content_type(content_type: &str, name: &str) -> Result<String> {
    if content_type.is_empty() {
        let inferred = mime_type_for_filename(name);
        let mime: Mime = inferred
            .parse()
            .map_err(|_| anyhow!("internal error: hardcoded MIME type invalid"))?;
        let top = mime.type_().as_str();
        if top == "application" {
            let sub = mime.subtype().as_str();
            if !ALLOWED_APPLICATION_SUBTYPES.contains(&sub) {
                return Ok("application/octet-stream".to_string());
            }
        }
        return Ok(inferred.to_string());
    }
    let ct = if content_type.len() > MAX_CONTENT_TYPE_LEN {
        let mut end = MAX_CONTENT_TYPE_LEN;
        while !content_type.is_char_boundary(end) {
            end -= 1;
        }
        &content_type[..end]
    } else {
        content_type
    };
    match validate_mime_type(ct) {
        Ok(_) => Ok(ct.to_string()),
        Err(_) => Ok("application/octet-stream".to_string()),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
#[derive(Default)]
pub enum AttachmentType {
    #[default]
    File,
    Item,
}

impl AttachmentType {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::File => "file",
            Self::Item => "item",
        }
    }
}

impl From<&str> for AttachmentType {
    fn from(s: &str) -> Self {
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
    pub attachment_type: AttachmentType,
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
    pub fn from_record(rec: AttachmentRecord) -> Self {
        Self {
            id: rec.id,
            parent_item_server_id: rec.parent_item_server_id,
            owner: rec.owner,
            name: rec.name,
            content_type: rec.content_type,
            content_size: rec.content_size,
            content_base64: rec.content_base64,
            is_inline: rec.is_inline,
            content_id: rec.content_id,
            content_location: rec.content_location,
            last_modified_time: rec.last_modified_time,
        }
    }

    pub fn to_eas_summary(&self) -> EasAttachmentSummary {
        EasAttachmentSummary {
            file_reference: self.id.clone(),
            display_name: self.name.clone(),
            method: if self.is_inline { 2 } else { 1 },
            estimated_data_size: self.content_size,
            is_inline: self.is_inline,
            content_id: self.content_id.clone(),
            content_location: self.content_location.clone(),
        }
    }

    pub fn to_ews_summary(&self) -> EwsAttachmentSummary {
        EwsAttachmentSummary {
            attachment_id: self.id.clone(),
            name: self.name.clone(),
            content_type: self.content_type.clone(),
            content_size: self.content_size,
            is_inline: self.is_inline,
            content_id: self.content_id.clone(),
            content_location: self.content_location.clone(),
            last_modified_time: self.last_modified_time.as_deref().and_then(|s| {
                chrono::DateTime::parse_from_rfc3339(s)
                    .ok()
                    .map(|dt| dt.to_utc())
            }),
        }
    }
}

#[derive(Clone, Debug)]
pub struct EasAttachmentSummary {
    pub file_reference: String,
    pub display_name: String,
    pub method: u8,
    pub estimated_data_size: i64,
    pub is_inline: bool,
    pub content_id: Option<String>,
    pub content_location: Option<String>,
}

#[derive(Clone, Debug)]
pub struct EwsAttachmentSummary {
    pub attachment_id: String,
    pub name: String,
    pub content_type: String,
    pub content_size: i64,
    pub is_inline: bool,
    pub content_id: Option<String>,
    pub content_location: Option<String>,
    pub last_modified_time: Option<chrono::DateTime<chrono::Utc>>,
}

pub struct CreateAttachmentParams<'a> {
    pub owner: &'a str,
    pub parent_item_server_id: &'a str,
    pub name: &'a str,
    pub content_type: &'a str,
    pub content_base64: &'a str,
    pub is_inline: bool,
    pub content_id: Option<&'a str>,
    pub content_location: Option<&'a str>,
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

    pub async fn create_file_attachment(
        &self,
        params: &CreateAttachmentParams<'_>,
    ) -> Result<FileAttachment> {
        let name = validate_attachment_name(params.name)?;

        let content_type = normalize_content_type(params.content_type, &name)?;

        let decoded_len_estimate = base64::decoded_len_estimate(params.content_base64.len());
        if decoded_len_estimate > self.max_attachment_bytes {
            return Err(anyhow!("Attachment size exceeds maximum allowed size"));
        }

        let decoded = STANDARD
            .decode(params.content_base64)
            .map_err(|_| anyhow!("invalid base64 content in attachment"))?;

        if decoded.is_empty() {
            return Err(anyhow!("attachment content is empty"));
        }

        if decoded.len() > self.max_attachment_bytes {
            return Err(anyhow!(
                "Attachment size {} exceeds maximum allowed size {}",
                decoded.len(),
                self.max_attachment_bytes
            ));
        }

        let id = uuid::Uuid::new_v4().to_string();
        let now = chrono::Utc::now().to_rfc3339();
        let content_size = i64::try_from(decoded.len()).unwrap_or(i64::MAX);

        let record = AttachmentRecord {
            id,
            parent_item_server_id: params.parent_item_server_id.to_string(),
            owner: params.owner.to_string(),
            name,
            content_type,
            content_size,
            content_base64: params.content_base64.to_string(),
            is_inline: params.is_inline,
            content_id: params.content_id.map(String::from),
            content_location: params.content_location.map(String::from),
            attachment_type: AttachmentType::File,
            last_modified_time: Some(now.clone()),
            created_at: now.clone(),
            updated_at: now,
        };

        self.storage.upsert_calendar_attachment(&record).await?;

        Ok(FileAttachment::from_record(record))
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
        Ok(rec.map(FileAttachment::from_record))
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
        Ok(recs.into_iter().map(FileAttachment::from_record).collect())
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
        let parent_id = rec.map(|r| r.parent_item_server_id);
        self.storage
            .delete_calendar_attachment(owner, attachment_id)
            .await?;
        Ok(parent_id)
    }

    pub async fn delete_attachments_for_item(
        &self,
        owner: &str,
        parent_item_server_id: &str,
    ) -> Result<()> {
        let recs = self
            .storage
            .get_calendar_attachments_for_item(owner, parent_item_server_id)
            .await?;
        for rec in &recs {
            self.storage
                .delete_calendar_attachment(owner, &rec.id)
                .await?;
        }
        Ok(())
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
    let mut is_inline_buf = String::new();
    let mut content_id = None::<String>;
    let mut content_location = None::<String>;
    let mut attachment_type = AttachmentType::File;

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) => {
                let local = e.name().local_name();
                match local.as_ref() {
                    b"ParentItemId" => {
                        for attr in e.attributes().flatten() {
                            if attr.key.local_name().as_ref() == b"Id"
                                && let Ok(v) = attr.decode_and_unescape_value(reader.decoder())
                            {
                                parent_item_id = Some(v.into_owned());
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
                        is_inline_buf.clear();
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
                        name.push_str(&text);
                    } else if in_content_type {
                        content_type.push_str(&text);
                    } else if in_content {
                        content_base64.push_str(&text);
                    } else if in_is_inline {
                        is_inline_buf.push_str(&text);
                    } else if in_content_id {
                        content_id.get_or_insert_with(String::new).push_str(&text);
                    } else if in_content_location {
                        content_location
                            .get_or_insert_with(String::new)
                            .push_str(&text);
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
                        is_inline = is_inline_buf.trim().eq_ignore_ascii_case("true");
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

    attachment_ids
        .into_iter()
        .next()
        .map(|id| ParsedDeleteAttachment { attachment_id: id })
}

pub struct ParsedDeleteAttachment {
    pub attachment_id: String,
}

pub fn render_file_attachment_xml(attachment: &FileAttachment, include_content: bool) -> String {
    let mut xml = String::with_capacity(512);
    xml.push_str("<t:FileAttachment>");
    let _ = write!(
        xml,
        r#"<t:AttachmentId Id="{}"/>"#,
        xml_escape(&attachment.id)
    );
    let _ = write!(xml, "<t:Name>{}</t:Name>", xml_escape(&attachment.name));
    let _ = write!(
        xml,
        "<t:ContentType>{}</t:ContentType>",
        xml_escape(&attachment.content_type)
    );
    if include_content {
        let _ = write!(
            xml,
            "<t:Content>{}</t:Content>",
            xml_escape(&attachment.content_base64)
        );
    }
    let _ = write!(xml, "<t:Size>{}</t:Size>", attachment.content_size);
    let _ = write!(
        xml,
        "<t:IsInline>{}</t:IsInline>",
        if attachment.is_inline {
            "true"
        } else {
            "false"
        }
    );
    if let Some(cid) = &attachment.content_id {
        let _ = write!(xml, "<t:ContentId>{}</t:ContentId>", xml_escape(cid));
    }
    if let Some(cl) = &attachment.content_location {
        let _ = write!(
            xml,
            "<t:ContentLocation>{}</t:ContentLocation>",
            xml_escape(cl)
        );
    }
    if let Some(t) = &attachment.last_modified_time {
        let _ = write!(
            xml,
            "<t:LastModifiedTime>{}</t:LastModifiedTime>",
            xml_escape(t)
        );
    }
    xml.push_str("</t:FileAttachment>");
    xml
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

fn is_safe_xml_element_name(s: &str) -> bool {
    !s.is_empty()
        && s.chars().all(|c| {
            c.is_ascii_alphabetic()
                || c.is_ascii_digit()
                || c == '-'
                || c == '_'
                || c == '.'
                || c == ':'
        })
}

pub fn render_attachment_error_response(operation: &str, code: &str, message: &str) -> String {
    let safe_operation = if is_safe_xml_element_name(operation) {
        operation
    } else {
        "AttachmentResponseMessage"
    };
    format!(
        r#"<m:ResponseMessages xmlns:m="{}" xmlns:t="{}">
            <m:{safe_op} ResponseClass="Error">
                <m:MessageText>{}</m:MessageText>
                <m:ResponseCode>{}</m:ResponseCode>
                <m:DescriptiveLinkKey>0</m:DescriptiveLinkKey>
            </m:{safe_op}>
        </m:ResponseMessages>"#,
        EWS_MSG_NS,
        EWS_TYPE_NS,
        xml_escape(message),
        xml_escape(code),
        safe_op = safe_operation,
    )
}

pub fn render_eas_attachments_xml(attachments: &[EasAttachmentSummary]) -> String {
    if attachments.is_empty() {
        return String::new();
    }
    let mut xml = String::with_capacity(256 * attachments.len());
    xml.push_str("<AirSyncBase:Attachments>");
    for att in attachments {
        xml.push_str("<AirSyncBase:Attachment>");
        let _ = write!(
            xml,
            "<AirSyncBase:DisplayName>{}</AirSyncBase:DisplayName>",
            xml_escape(&att.display_name)
        );
        let _ = write!(
            xml,
            "<AirSyncBase:FileReference>{}</AirSyncBase:FileReference>",
            xml_escape(&att.file_reference)
        );
        let _ = write!(
            xml,
            "<AirSyncBase:Method>{}</AirSyncBase:Method>",
            att.method
        );
        let _ = write!(
            xml,
            "<AirSyncBase:EstimatedDataSize>{}</AirSyncBase:EstimatedDataSize>",
            att.estimated_data_size.max(0)
        );
        let _ = write!(
            xml,
            "<AirSyncBase:IsInline>{}</AirSyncBase:IsInline>",
            if att.is_inline { "1" } else { "0" }
        );
        if let Some(cid) = &att.content_id {
            let _ = write!(
                xml,
                "<AirSyncBase:ContentId>{}</AirSyncBase:ContentId>",
                xml_escape(cid)
            );
        }
        if let Some(cl) = &att.content_location {
            let _ = write!(
                xml,
                "<AirSyncBase:ContentLocation>{}</AirSyncBase:ContentLocation>",
                xml_escape(cl)
            );
        }
        xml.push_str("</AirSyncBase:Attachment>");
    }
    xml.push_str("</AirSyncBase:Attachments>");
    xml
}

pub fn render_ews_attachments_xml(attachments: &[EwsAttachmentSummary]) -> String {
    if attachments.is_empty() {
        return String::new();
    }
    let mut xml = String::with_capacity(512 * attachments.len());
    xml.push_str("<t:Attachments>");
    for att in attachments {
        xml.push_str("<t:FileAttachment>");
        let _ = write!(
            xml,
            r#"<t:AttachmentId Id="{}"/>"#,
            xml_escape(&att.attachment_id)
        );
        let _ = write!(xml, "<t:Name>{}</t:Name>", xml_escape(&att.name));
        let _ = write!(
            xml,
            "<t:ContentType>{}</t:ContentType>",
            xml_escape(&att.content_type)
        );
        let _ = write!(xml, "<t:Size>{}</t:Size>", att.content_size.max(0));
        let _ = write!(
            xml,
            "<t:IsInline>{}</t:IsInline>",
            if att.is_inline { "true" } else { "false" }
        );
        if let Some(cid) = &att.content_id {
            let _ = write!(xml, "<t:ContentId>{}</t:ContentId>", xml_escape(cid));
        }
        if let Some(cl) = &att.content_location {
            let _ = write!(
                xml,
                "<t:ContentLocation>{}</t:ContentLocation>",
                xml_escape(cl)
            );
        }
        if let Some(lmt) = &att.last_modified_time {
            let _ = write!(
                xml,
                "<t:LastModifiedTime>{}</t:LastModifiedTime>",
                lmt.to_rfc3339()
            );
        }
        xml.push_str("</t:FileAttachment>");
    }
    xml.push_str("</t:Attachments>");
    xml
}

pub fn render_eas_attachment_fetch_response(attachment: &FileAttachment, status: u32) -> String {
    let mut xml = String::with_capacity(512);
    xml.push_str("<ItemOperations:Fetch>");
    let _ = write!(
        xml,
        "<ItemOperations:Status>{}</ItemOperations:Status>",
        status
    );
    if status == 1 {
        xml.push_str(&render_eas_attachment_content_xml(attachment));
    }
    xml.push_str("</ItemOperations:Fetch>");
    xml
}

pub fn render_eas_attachment_content_xml(attachment: &FileAttachment) -> String {
    let mut xml = String::with_capacity(512);
    xml.push_str("<Properties>");
    let _ = write!(
        xml,
        "<AirSyncBase:DisplayName>{}</AirSyncBase:DisplayName>",
        xml_escape(&attachment.name)
    );
    let _ = write!(
        xml,
        "<AirSyncBase:FileReference>{}</AirSyncBase:FileReference>",
        xml_escape(&attachment.id)
    );
    let _ = write!(
        xml,
        "<AirSyncBase:ContentType>{}</AirSyncBase:ContentType>",
        xml_escape(&attachment.content_type)
    );
    let _ = write!(
        xml,
        "<AirSyncBase:EstimatedDataSize>{}</AirSyncBase:EstimatedDataSize>",
        attachment.content_size.max(0)
    );
    let _ = write!(
        xml,
        "<AirSyncBase:IsInline>{}</AirSyncBase:IsInline>",
        if attachment.is_inline { "1" } else { "0" }
    );
    let _ = write!(
        xml,
        "<AirSyncBase:Data>{}</AirSyncBase:Data>",
        xml_escape(&attachment.content_base64)
    );
    if let Some(cid) = &attachment.content_id {
        let _ = write!(
            xml,
            "<AirSyncBase:ContentId>{}</AirSyncBase:ContentId>",
            xml_escape(cid)
        );
    }
    if let Some(cl) = &attachment.content_location {
        let _ = write!(
            xml,
            "<AirSyncBase:ContentLocation>{}</AirSyncBase:ContentLocation>",
            xml_escape(cl)
        );
    }
    xml.push_str("</Properties>");
    xml
}

pub fn parse_eas_attachment_adds(xml: &str) -> Vec<ParsedEasAttachmentAdd> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);
    let mut buf = Vec::new();
    let mut results = Vec::new();
    let mut in_attachment = false;
    let mut in_display_name = false;
    let mut in_method = false;
    let mut in_estimated_data_size = false;
    let mut in_content_type = false;
    let mut in_content_id = false;
    let mut in_content_location = false;
    let mut in_is_inline = false;
    let mut in_data = false;
    let mut display_name = String::new();
    let mut method_buf = String::new();
    let mut estimated_data_size_buf = String::new();
    let mut is_inline_buf = String::new();
    let mut method: u8 = 1;
    let mut estimated_data_size: i64 = 0;
    let mut content_type = String::new();
    let mut content_id: Option<String> = None;
    let mut content_location: Option<String> = None;
    let mut is_inline = false;
    let mut data = String::new();

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) => match e.name().local_name().as_ref() {
                b"Attachment" => {
                    if in_attachment {
                        display_name.clear();
                        content_type.clear();
                        data.clear();
                        method_buf.clear();
                        estimated_data_size_buf.clear();
                        is_inline_buf.clear();
                        method = 1;
                        estimated_data_size = 0;
                        content_id = None;
                        content_location = None;
                        is_inline = false;
                    }
                    in_attachment = true;
                }
                b"DisplayName" => in_display_name = true,
                b"Method" => {
                    in_method = true;
                    method_buf.clear();
                }
                b"EstimatedDataSize" => {
                    in_estimated_data_size = true;
                    estimated_data_size_buf.clear();
                }
                b"ContentType" => in_content_type = true,
                b"ContentId" => in_content_id = true,
                b"ContentLocation" => in_content_location = true,
                b"IsInline" => {
                    in_is_inline = true;
                    is_inline_buf.clear();
                }
                b"Data" => in_data = true,
                _ => {}
            },
            Ok(Event::End(e)) => match e.name().local_name().as_ref() {
                b"Attachment" => {
                    if in_attachment && !data.is_empty() {
                        let dn = std::mem::take(&mut display_name);
                        let ct = std::mem::take(&mut content_type);
                        let d = std::mem::take(&mut data);
                        let ct_resolved = if ct.is_empty() {
                            mime_type_for_filename(&dn).to_string()
                        } else {
                            ct
                        };
                        results.push(ParsedEasAttachmentAdd {
                            display_name: if dn.is_empty() {
                                "attachment.dat".to_string()
                            } else {
                                dn
                            },
                            method,
                            estimated_data_size,
                            content_type: ct_resolved,
                            content_id: content_id.take(),
                            content_location: content_location.take(),
                            is_inline,
                            content_base64: d,
                        });
                    }
                    in_attachment = false;
                    method = 1;
                    estimated_data_size = 0;
                    method_buf.clear();
                    estimated_data_size_buf.clear();
                    is_inline_buf.clear();
                    content_id = None;
                    content_location = None;
                    is_inline = false;
                }
                b"DisplayName" => in_display_name = false,
                b"Method" => {
                    method = method_buf.trim().parse().unwrap_or(1);
                    in_method = false;
                }
                b"EstimatedDataSize" => {
                    estimated_data_size = estimated_data_size_buf.trim().parse().unwrap_or(0);
                    in_estimated_data_size = false;
                }
                b"ContentType" => in_content_type = false,
                b"ContentId" => in_content_id = false,
                b"ContentLocation" => in_content_location = false,
                b"IsInline" => {
                    let v = is_inline_buf.trim();
                    is_inline = v == "1" || v.eq_ignore_ascii_case("true");
                    in_is_inline = false;
                }
                b"Data" => in_data = false,
                _ => {}
            },
            Ok(Event::Text(t)) => {
                if let Ok(v) = t.decode() {
                    let text = v.as_ref();
                    if in_display_name {
                        display_name.push_str(text);
                    } else if in_method {
                        method_buf.push_str(text);
                    } else if in_estimated_data_size {
                        estimated_data_size_buf.push_str(text);
                    } else if in_content_type {
                        content_type.push_str(text);
                    } else if in_content_id {
                        content_id.get_or_insert_with(String::new).push_str(text);
                    } else if in_content_location {
                        content_location
                            .get_or_insert_with(String::new)
                            .push_str(text);
                    } else if in_is_inline {
                        is_inline_buf.push_str(text);
                    } else if in_data {
                        data.push_str(text);
                    }
                }
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
        buf.clear();
    }
    results
}

#[derive(Clone, Debug)]
pub struct ParsedEasAttachmentAdd {
    pub display_name: String,
    pub method: u8,
    pub estimated_data_size: i64,
    pub content_type: String,
    pub content_id: Option<String>,
    pub content_location: Option<String>,
    pub is_inline: bool,
    pub content_base64: String,
}

pub fn parse_eas_attachment_deletes(xml: &str) -> Vec<String> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);
    let mut buf = Vec::new();
    let mut ids = Vec::new();
    let mut in_file_reference = false;

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) if e.name().local_name().as_ref() == b"FileReference" => {
                in_file_reference = true;
            }
            Ok(Event::End(e)) if e.name().local_name().as_ref() == b"FileReference" => {
                in_file_reference = false;
            }
            Ok(Event::Text(t)) => {
                if in_file_reference && let Ok(v) = t.decode() {
                    ids.push(v.into_owned());
                }
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
        buf.clear();
    }
    ids
}
