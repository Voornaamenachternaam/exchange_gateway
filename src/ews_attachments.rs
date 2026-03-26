/* File: src/ews_attachments.rs */
// Removed the line `mod ews_attachments;`
//!
//! This module implements comprehensive Exchange Web Services attachment operations
//! including CreateAttachment, GetAttachment, DeleteAttachment with support for
//! file attachments, item attachments, and reference attachments.

use crate::ews_handlers::EwsResponse;
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use bytes::Bytes;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Attachment types
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AttachmentType {
    /// File attachment (binary data)
    FileAttachment,
    /// Item attachment (embedded message)
    ItemAttachment,
    /// Reference attachment (link to external storage)
    ReferenceAttachment,
}

impl AttachmentType {
    pub fn as_str(&self) -> &'static str {
        match self {
            AttachmentType::FileAttachment => "FileAttachment",
            AttachmentType::ItemAttachment => "ItemAttachment",
            AttachmentType::ReferenceAttachment => "ReferenceAttachment",
        }
    }
}

/// File attachment data
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileAttachment {
    pub attachment_id: String,
    pub parent_item_id: String,
    pub name: String,
    pub content_type: String,
    pub content_id: Option<String>,
    pub content_location: Option<String>,
    pub size: u64,
    pub is_inline: bool,
    pub is_contact_photo: bool,
    pub content: Option<Bytes>,
    pub last_modified_time: DateTime<Utc>,
}

impl FileAttachment {
    /// Create a new file attachment
    pub fn new(
        parent_item_id: impl Into<String>,
        name: impl Into<String>,
        content_type: impl Into<String>,
        content: Bytes,
    ) -> Self {
        let now = Utc::now();
        Self {
            attachment_id: format!("att-{}-{}", Uuid::new_v4(), now.timestamp()),
            parent_item_id: parent_item_id.into(),
            name: name.into(),
            content_type: content_type.into(),
            content_id: None,
            content_location: None,
            size: content.len() as u64,
            is_inline: false,
            is_contact_photo: false,
            content: Some(content),
            last_modified_time: now,
        }
    }

    /// Set inline flag
    pub fn set_inline(mut self, content_id: impl Into<String>) -> Self {
        self.is_inline = true;
        self.content_id = Some(content_id.into());
        self
    }

    /// Get content as base64
    pub fn content_base64(&self) -> Option<String> {
        self.content.as_ref().map(|c| BASE64.encode(c))
    }

    /// Generate EWS XML for attachment
    pub fn to_ews_xml(&self) -> String {
        let mut xml = String::new();
        xml.push_str(&format!("<t:FileAttachment>"));
        xml.push_str(&format!("<t:AttachmentId Id=\"{}\"/>", self.attachment_id));
        xml.push_str(&format!("<t:Name>{}</t:Name>", xml_escape(&self.name)));
        xml.push_str(&format!("<t:ContentType>{}</t:ContentType>", xml_escape(&self.content_type)));
        xml.push_str(&format!("<t:Size>{}</t:Size>", self.size));
        
        if self.is_inline {
            xml.push_str("<t:IsInline>true</t:IsInline>");
        }
        
        if let Some(ref content_id) = self.content_id {
            xml.push_str(&format!("<t:ContentId>{}</t:ContentId>", xml_escape(content_id)));
        }
        
        if let Some(ref content_location) = self.content_location {
            xml.push_str(&format!("<t:ContentLocation>{}</t:ContentLocation>", xml_escape(content_location)));
        }
        
        if let Some(ref content) = self.content {
            xml.push_str(&format!("<t:Content>{}</t:Content>", BASE64.encode(content)));
        }
        
        xml.push_str("</t:FileAttachment>");
        xml
    }
}

/// Item attachment data (embedded message)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ItemAttachment {
    pub attachment_id: String,
    pub parent_item_id: String,
    pub name: String,
    pub item_type: String,
    pub item_id: Option<String>,
    pub is_inline: bool,
    pub last_modified_time: DateTime<Utc>,
}

impl ItemAttachment {
    /// Create a new item attachment
    pub fn new(
        parent_item_id: impl Into<String>,
        name: impl Into<String>,
        item_type: impl Into<String>,
    ) -> Self {
        let now = Utc::now();
        Self {
            attachment_id: format!("item-att-{}-{}", Uuid::new_v4(), now.timestamp()),
            parent_item_id: parent_item_id.into(),
            name: name.into(),
            item_type: item_type.into(),
            item_id: None,
            is_inline: false,
            last_modified_time: now,
        }
    }

    /// Generate EWS XML for attachment
    pub fn to_ews_xml(&self) -> String {
        let mut xml = String::new();
        xml.push_str(&format!("<t:ItemAttachment>"));
        xml.push_str(&format!("<t:AttachmentId Id=\"{}\"/>", self.attachment_id));
        xml.push_str(&format!("<t:Name>{}</t:Name>", xml_escape(&self.name)));
        
        if self.is_inline {
            xml.push_str("<t:IsInline>true</t:IsInline>");
        }
        
        xml.push_str(&format!("<t:{}>", self.item_type));
        if let Some(ref item_id) = self.item_id {
            xml.push_str(&format!("<t:ItemId Id=\"{}\"/>", item_id));
        }
        xml.push_str(&format!("</t:{}>", self.item_type));
        
        xml.push_str("</t:ItemAttachment>");
        xml
    }
}

/// Reference attachment (link to external storage)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReferenceAttachment {
    pub attachment_id: String,
    pub parent_item_id: String,
    pub name: String,
    pub attach_method: AttachMethod,
    pub content_type: String,
    pub content_location: String,
    pub size: Option<u64>,
    pub is_inline: bool,
    pub provider_endpoint_url: Option<String>,
    pub provider_type: Option<String>,
    pub permission_type: PermissionType,
    pub last_modified_time: DateTime<Utc>,
}

/// Attachment method for reference attachments
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AttachMethod {
    /// Standard attachment
    ByValue = 1,
    /// By reference (OneDrive, SharePoint, etc.)
    ByReference = 6,
    /// Embedded message
    EmbeddedMessage = 5,
    /// Storage
    Storage = 7,
}

impl AttachMethod {
    pub fn as_u8(&self) -> u8 {
        *self as u8
    }
}

/// Permission type for reference attachments
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PermissionType {
    /// Anyone with link can view
    AnyoneCanView = 0,
    /// Anyone with link can edit
    AnyoneCanEdit = 1,
    /// Organization can view
    OrganizationCanView = 2,
    /// Organization can edit
    OrganizationCanEdit = 3,
    /// Recipients can view
    RecipientsCanView = 4,
    /// Recipients can edit
    RecipientsCanEdit = 5,
}

impl ReferenceAttachment {
    /// Create a new reference attachment
    pub fn new(
        parent_item_id: impl Into<String>,
        name: impl Into<String>,
        content_location: impl Into<String>,
    ) -> Self {
        let now = Utc::now();
        Self {
            attachment_id: format!("ref-att-{}-{}", Uuid::new_v4(), now.timestamp()),
            parent_item_id: parent_item_id.into(),
            name: name.into(),
            attach_method: AttachMethod::ByReference,
            content_type: "application/octet-stream".to_string(),
            content_location: content_location.into(),
            size: None,
            is_inline: false,
            provider_endpoint_url: None,
            provider_type: None,
            permission_type: PermissionType::RecipientsCanView,
            last_modified_time: now,
        }
    }

    /// Generate EWS XML for attachment
    pub fn to_ews_xml(&self) -> String {
        let mut xml = String::new();
        xml.push_str(&format!("<t:ReferenceAttachment>"));
        xml.push_str(&format!("<t:AttachmentId Id=\"{}\"/>", self.attachment_id));
        xml.push_str(&format!("<t:Name>{}</t:Name>", xml_escape(&self.name)));
        xml.push_str(&format!("<t:AttachMethod>{}</t:AttachMethod>", self.attach_method.as_u8()));
        xml.push_str(&format!("<t:ContentType>{}</t:ContentType>", xml_escape(&self.content_type)));
        xml.push_str(&format!("<t:ContentLocation>{}</t:ContentLocation>", xml_escape(&self.content_location)));
        
        if let Some(size) = self.size {
            xml.push_str(&format!("<t:Size>{}</t:Size>", size));
        }
        
        if self.is_inline {
            xml.push_str("<t:IsInline>true</t:IsInline>");
        }
        
        if let Some(ref provider_url) = self.provider_endpoint_url {
            xml.push_str(&format!("<t:ProviderEndpointUrl>{}</t:ProviderEndpointUrl>", xml_escape(provider_url)));
        }
        
        if let Some(ref provider_type) = self.provider_type {
            xml.push_str(&format!("<t:ProviderType>{}</t:ProviderType>", xml_escape(provider_type)));
        }
        
        xml.push_str(&format!("<t:PermissionType>{}</t:PermissionType>", self.permission_type as u8));
        xml.push_str("</t:ReferenceAttachment>");
        xml
    }
}

/// Union type for all attachment types
#[derive(Debug, Clone)]
pub enum Attachment {
    File(FileAttachment),
    Item(ItemAttachment),
    Reference(ReferenceAttachment),
}

impl Attachment {
    pub fn attachment_id(&self) -> &str {
        match self {
            Attachment::File(a) => &a.attachment_id,
            Attachment::Item(a) => &a.attachment_id,
            Attachment::Reference(a) => &a.attachment_id,
        }
    }

    pub fn parent_item_id(&self) -> &str {
        match self {
            Attachment::File(a) => &a.parent_item_id,
            Attachment::Item(a) => &a.parent_item_id,
            Attachment::Reference(a) => &a.parent_item_id,
        }
    }

    pub fn name(&self) -> &str {
        match self {
            Attachment::File(a) => &a.name,
            Attachment::Item(a) => &a.name,
            Attachment::Reference(a) => &a.name,
        }
    }

    pub fn to_ews_xml(&self) -> String {
        match self {
            Attachment::File(a) => a.to_ews_xml(),
            Attachment::Item(a) => a.to_ews_xml(),
            Attachment::Reference(a) => a.to_ews_xml(),
        }
    }
}

/// Create attachment request
#[derive(Debug, Clone)]
pub struct CreateAttachmentRequest {
    pub parent_item_id: String,
    pub parent_item_change_key: Option<String>,
    pub attachments: Vec<Attachment>,
}

/// Create attachment response
#[derive(Debug, Clone)]
pub struct CreateAttachmentResponse {
    pub response_code: String,
    pub attachments: Vec<Attachment>,
    pub parent_item_id: Option<String>,
    pub parent_item_change_key: Option<String>,
}

/// Get attachment request
#[derive(Debug, Clone)]
pub struct GetAttachmentRequest {
    pub attachment_ids: Vec<String>,
    pub include_mime_content: bool,
    pub body_type: Option<BodyType>,
    pub filtered_html_content: bool,
    pub convert_html_to_utf8: bool,
}

/// Body type for attachment retrieval
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BodyType {
    Best,
    HTML,
    Text,
}

/// Get attachment response
#[derive(Debug, Clone)]
pub struct GetAttachmentResponse {
    pub response_code: String,
    pub attachments: Vec<Attachment>,
}

/// Delete attachment request
#[derive(Debug, Clone)]
pub struct DeleteAttachmentRequest {
    pub attachment_ids: Vec<String>,
}

/// Delete attachment response
#[derive(Debug, Clone)]
pub struct DeleteAttachmentResponse {
    pub response_code: String,
    pub deleted_attachment_ids: Vec<String>,
}

/// Attachment store trait
pub trait AttachmentStore: Send + Sync {
    /// Store a new attachment
    fn store_attachment(&mut self, attachment: Attachment) -> Result<String, AttachmentError>;
    
    /// Retrieve an attachment by ID
    fn get_attachment(&self, attachment_id: &str) -> Result<Option<Attachment>, AttachmentError>;
    
    /// Delete an attachment
    fn delete_attachment(&mut self, attachment_id: &str) -> Result<bool, AttachmentError>;
    
    /// Get attachments for a parent item
    fn get_attachments_for_item(&self, parent_item_id: &str) -> Result<Vec<Attachment>, AttachmentError>;
    
    /// Update attachment
    fn update_attachment(&mut self, attachment: Attachment) -> Result<(), AttachmentError>;
}

/// In-memory attachment store
pub struct InMemoryAttachmentStore {
    attachments: std::collections::HashMap<String, Attachment>,
    parent_index: std::collections::HashMap<String, Vec<String>>,
}

impl InMemoryAttachmentStore {
    pub fn new() -> Self {
        Self {
            attachments: std::collections::HashMap::new(),
            parent_index: std::collections::HashMap::new(),
        }
    }
}

impl Default for InMemoryAttachmentStore {
    fn default() -> Self {
        Self::new()
    }
}

impl AttachmentStore for InMemoryAttachmentStore {
    fn store_attachment(&mut self, attachment: Attachment) -> Result<String, AttachmentError> {
        let id = attachment.attachment_id().to_string();
        let parent_id = attachment.parent_item_id().to_string();
        
        self.attachments.insert(id.clone(), attachment);
        self.parent_index
            .entry(parent_id)
            .or_default()
            .push(id.clone());
        
        Ok(id)
    }

    fn get_attachment(&self, attachment_id: &str) -> Result<Option<Attachment>, AttachmentError> {
        Ok(self.attachments.get(attachment_id).cloned())
    }

    fn delete_attachment(&mut self, attachment_id: &str) -> Result<bool, AttachmentError> {
        if let Some(attachment) = self.attachments.remove(attachment_id) {
            if let Some(index) = self.parent_index.get_mut(attachment.parent_item_id()) {
                index.retain(|id| id != attachment_id);
            }
            Ok(true)
        } else {
            Ok(false)
        }
    }

    fn get_attachments_for_item(&self, parent_item_id: &str) -> Result<Vec<Attachment>, AttachmentError> {
        Ok(self.parent_index
            .get(parent_item_id)
            .map(|ids| {
                ids.iter()
                    .filter_map(|id| self.attachments.get(id).cloned())
                    .collect()
            })
            .unwrap_or_default())
    }

    fn update_attachment(&mut self, attachment: Attachment) -> Result<(), AttachmentError> {
        let id = attachment.attachment_id().to_string();
        self.attachments.insert(id, attachment);
        Ok(())
    }
}

/// Attachment error
#[derive(Debug, Clone, PartialEq)]
pub enum AttachmentError {
    NotFound,
    StorageError(String),
    InvalidAttachment,
    SizeLimitExceeded,
    UnsupportedType,
}

impl std::fmt::Display for AttachmentError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AttachmentError::NotFound => write!(f, "Attachment not found"),
            AttachmentError::StorageError(s) => write!(f, "Storage error: {}", s),
            AttachmentError::InvalidAttachment => write!(f, "Invalid attachment"),
            AttachmentError::SizeLimitExceeded => write!(f, "Attachment size limit exceeded"),
            AttachmentError::UnsupportedType => write!(f, "Unsupported attachment type"),
        }
    }
}

impl std::error::Error for AttachmentError {}

/// Attachment handler for EWS operations
pub struct AttachmentHandler<S: AttachmentStore> {
    store: S,
    max_attachment_size: u64,
}

impl<S: AttachmentStore> AttachmentHandler<S> {
    pub fn new(store: S) -> Self {
        Self {
            store,
            max_attachment_size: 150 * 1024 * 1024, // 150MB default
        }
    }

    pub fn with_max_size(mut self, max_size: u64) -> Self {
        self.max_attachment_size = max_size;
        self
    }

    /// Handle CreateAttachment request
    pub fn create_attachments(
        &mut self,
        request: CreateAttachmentRequest,
    ) -> Result<CreateAttachmentResponse, AttachmentError> {
        let mut created_attachments = Vec::new();
        
        for attachment in request.attachments {
            // Validate attachment size
            if let Attachment::File(ref file_att) = attachment {
                if let Some(ref content) = file_att.content {
                    if content.len() as u64 > self.max_attachment_size {
                        return Err(AttachmentError::SizeLimitExceeded);
                    }
                }
            }
            
            let id = self.store.store_attachment(attachment.clone())?;
            if let Some(stored) = self.store.get_attachment(&id)? {
                created_attachments.push(stored);
            }
        }
        
        Ok(CreateAttachmentResponse {
            response_code: "NoError".to_string(),
            attachments: created_attachments,
            parent_item_id: Some(request.parent_item_id),
            parent_item_change_key: request.parent_item_change_key,
        })
    }

    /// Handle GetAttachment request
    pub fn get_attachments(
        &self,
        request: GetAttachmentRequest,
    ) -> Result<GetAttachmentResponse, AttachmentError> {
        let mut attachments = Vec::new();
        
        for attachment_id in &request.attachment_ids {
            if let Some(attachment) = self.store.get_attachment(attachment_id)? {
                attachments.push(attachment);
            }
        }
        
        Ok(GetAttachmentResponse {
            response_code: "NoError".to_string(),
            attachments,
        })
    }

    /// Handle DeleteAttachment request
    pub fn delete_attachments(
        &mut self,
        request: DeleteAttachmentRequest,
    ) -> Result<DeleteAttachmentResponse, AttachmentError> {
        let mut deleted_ids = Vec::new();
        
        for attachment_id in &request.attachment_ids {
            if self.store.delete_attachment(attachment_id)? {
                deleted_ids.push(attachment_id.clone());
            }
        }
        
        Ok(DeleteAttachmentResponse {
            response_code: "NoError".to_string(),
            deleted_attachment_ids: deleted_ids,
        })
    }

    /// Get attachments for an item
    pub fn get_item_attachments(
        &self,
        parent_item_id: &str,
    ) -> Result<Vec<Attachment>, AttachmentError> {
        self.store.get_attachments_for_item(parent_item_id)
    }

    /// Generate EWS CreateAttachment response XML
pub fn generate_create_response_xml(&self, response: &CreateAttachmentResponse) -> String {
        use quick_xml::events::{BytesEnd, BytesStart, Event};
        use quick_xml::Writer;
        use std::io::Cursor;

        let mut buffer = Cursor::new(Vec::new());
        let mut writer = Writer::new_with_indent(buffer, b' ', 4);

        let _ = writer.write_event(Event::Decl(quick_xml::events::BytesDecl::new("1.0", Some("utf-8"), None)));
        let mut envelope = BytesStart::new("s:Envelope");
        envelope.push_attribute(("xmlns:s", "http://schemas.xmlsoap.org/soap/envelope/"));
        let _ = writer.write_event(Event::Start(envelope));
        let _ = writer.write_event(Event::Start(BytesStart::new("s:Body")));

        let mut create_resp = BytesStart::new("m:CreateAttachmentResponse");
        create_resp.push_attribute(("xmlns:m", "http://schemas.microsoft.com/exchange/services/2006/messages"));
        create_resp.push_attribute(("xmlns:t", "http://schemas.microsoft.com/exchange/services/2006/types"));
        let _ = writer.write_event(Event::Start(create_resp));
        let _ = writer.write_event(Event::Start(BytesStart::new("m:ResponseMessages")));
        let _ = writer.write_event(Event::Start(BytesStart::new("m:CreateAttachmentResponseMessage")));
        writer.create_element("m:ResponseCode").write_text_content(quick_xml::events::BytesText::new(&response.response_code)).unwrap();

        if let Some(ref parent_id) = response.parent_item_id {
            let mut parent_item = BytesStart::new("m:ParentItemId");
            parent_item.push_attribute(("Id", parent_id.as_str()));
            if let Some(ref change_key) = response.parent_item_change_key {
                parent_item.push_attribute(("ChangeKey", change_key.as_str()));
            }
            let _ = writer.write_event(Event::Empty(parent_item));
        }

        let _ = writer.write_event(Event::Start(BytesStart::new("m:Attachments")));
        for attachment in &response.attachments {
            let _ = writer.write_raw(attachment.to_ews_xml().as_bytes());
        }
        let _ = writer.write_event(Event::End(BytesEnd::new("m:Attachments")));
        let _ = writer.write_event(Event::End(BytesEnd::new("m:CreateAttachmentResponseMessage")));
        let _ = writer.write_event(Event::End(BytesEnd::new("m:ResponseMessages")));
        let _ = writer.write_event(Event::End(BytesEnd::new("m:CreateAttachmentResponse")));
        let _ = writer.write_event(Event::End(BytesEnd::new("s:Body")));
        let _ = writer.write_event(Event::End(BytesEnd::new("s:Envelope")));

        String::from_utf8(writer.into_inner().into_inner()).unwrap()
    }

/// XML escape helper
fn xml_escape(input: &str) -> String {
    input
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_file_attachment_creation() {
        let content = Bytes::from_static(b"Hello, World!");
        let attachment = FileAttachment::new(
            "parent-123",
            "test.txt",
            "text/plain",
            content.clone(),
        );
        
        assert_eq!(attachment.name, "test.txt");
        assert_eq!(attachment.content_type, "text/plain");
        assert_eq!(attachment.size, content.len() as u64);
        assert!(attachment.attachment_id.starts_with("att-"));
    }

    #[test]
    fn test_file_attachment_base64() {
        let content = Bytes::from_static(b"Hello, World!");
        let attachment = FileAttachment::new(
            "parent-123",
            "test.txt",
            "text/plain",
            content,
        );
        
        let base64 = attachment.content_base64().unwrap();
        assert!(!base64.is_empty());
    }

    #[test]
    fn test_file_attachment_xml() {
        let content = Bytes::from_static(b"Hello");
        let attachment = FileAttachment::new(
            "parent-123",
            "test.txt",
            "text/plain",
            content,
        );
        
        let xml = attachment.to_ews_xml();
        assert!(xml.contains("FileAttachment"));
        assert!(xml.contains("test.txt"));
        assert!(xml.contains("text/plain"));
    }

    #[test]
    fn test_attachment_store() {
        let mut store = InMemoryAttachmentStore::new();
        
        let content = Bytes::from_static(b"Test content");
        let attachment = Attachment::File(FileAttachment::new(
            "parent-123",
            "test.txt",
            "text/plain",
            content,
        ));
        
        let id = store.store_attachment(attachment.clone()).unwrap();
        assert!(!id.is_empty());
        
        let retrieved = store.get_attachment(&id).unwrap();
        assert!(retrieved.is_some());
        assert_eq!(retrieved.unwrap().name(), "test.txt");
        
        let item_attachments = store.get_attachments_for_item("parent-123").unwrap();
        assert_eq!(item_attachments.len(), 1);
        
        assert!(store.delete_attachment(&id).unwrap());
        assert!(store.get_attachment(&id).unwrap().is_none());
    }

    #[test]
    fn test_attachment_handler_create() {
        let store = InMemoryAttachmentStore::new();
        let mut handler = AttachmentHandler::new(store);
        
        let content = Bytes::from_static(b"Test content");
        let attachment = Attachment::File(FileAttachment::new(
            "parent-123",
            "test.txt",
            "text/plain",
            content,
        ));
        
        let request = CreateAttachmentRequest {
            parent_item_id: "parent-123".to_string(),
            parent_item_change_key: None,
            attachments: vec![attachment],
        };
        
        let response = handler.create_attachments(request).unwrap();
        assert_eq!(response.response_code, "NoError");
        assert_eq!(response.attachments.len(), 1);
    }

    #[test]
    fn test_attachment_handler_get() {
        let store = InMemoryAttachmentStore::new();
        let mut handler = AttachmentHandler::new(store);
        
        // Create attachment first
        let content = Bytes::from_static(b"Test content");
        let attachment = Attachment::File(FileAttachment::new(
            "parent-123",
            "test.txt",
            "text/plain",
            content,
        ));
        
        let create_request = CreateAttachmentRequest {
            parent_item_id: "parent-123".to_string(),
            parent_item_change_key: None,
            attachments: vec![attachment],
        };
        
        let create_response = handler.create_attachments(create_request).unwrap();
        let attachment_id = create_response.attachments[0].attachment_id().to_string();
        
        // Get attachment
        let get_request = GetAttachmentRequest {
            attachment_ids: vec![attachment_id],
            include_mime_content: false,
            body_type: None,
            filtered_html_content: false,
            convert_html_to_utf8: false,
        };
        
        let get_response = handler.get_attachments(get_request).unwrap();
        assert_eq!(get_response.attachments.len(), 1);
    }

    #[test]
    fn test_reference_attachment() {
        let attachment = ReferenceAttachment::new(
            "parent-123",
            "document.docx",
            "https://storage.example.com/document.docx",
        );
        
        assert_eq!(attachment.name, "document.docx");
        assert_eq!(attachment.content_location, "https://storage.example.com/document.docx");
        assert_eq!(attachment.attach_method, AttachMethod::ByReference);
        
        let xml = attachment.to_ews_xml();
        assert!(xml.contains("ReferenceAttachment"));
        assert!(xml.contains("document.docx"));
    }

    #[test]
    fn test_xml_escape() {
        assert_eq!(xml_escape("<test>"), "&lt;test&gt;");
        assert_eq!(xml_escape("&"), "&amp;");
        assert_eq!(xml_escape("\"quoted\""), "&quot;quoted&quot;");
    }
}
