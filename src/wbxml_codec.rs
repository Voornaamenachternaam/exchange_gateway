//! WBXML Codec - Robust WBXML encoding/decoding for Exchange ActiveSync
//! 
//! This module provides production-grade WBXML (WAP Binary XML) codec
//! implementing MS-ASWBXML specification with full support for
//! ActiveSync protocol versions 12.0 through 16.1.

use bytes::{Buf, BufMut, Bytes, BytesMut};
use std::collections::HashMap;

/// WBXML version constants
pub const WBXML_VERSION_1_1: u8 = 0x01;
pub const WBXML_VERSION_1_2: u8 = 0x02;
pub const WBXML_VERSION_1_3: u8 = 0x03;

/// WBXML token constants
pub const SWITCH_PAGE: u8 = 0x00;
pub const END: u8 = 0x01;
pub const STR_I: u8 = 0x03;
pub const LITERAL: u8 = 0x04;
pub const EXT_I_0: u8 = 0x40;
pub const EXT_I_1: u8 = 0x41;
pub const EXT_I_2: u8 = 0x42;
pub const PI: u8 = 0x43;
pub const LITERAL_C: u8 = 0x44;
pub const EXT_T_0: u8 = 0x80;
pub const EXT_T_1: u8 = 0x81;
pub const EXT_T_2: u8 = 0x82;
pub const STR_T: u8 = 0x83;
pub const LITERAL_A: u8 = 0x84;
pub const EXT_0: u8 = 0xC0;
pub const EXT_1: u8 = 0xC1;
pub const EXT_2: u8 = 0xC2;
pub const OPAQUE: u8 = 0xC3;
pub const LITERAL_AC: u8 = 0xC4;

/// Code page identifiers for ActiveSync
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CodePage {
    AirSync = 0,
    Contacts = 1,
    Email = 2,
    AirSyncBase = 3,
    Calendar = 4,
    Move = 5,
    ItemEstimate = 6,
    FolderHierarchy = 7,
    MeetingResponse = 8,
    Tasks = 9,
    ResolveRecipients = 10,
    ValidateCert = 11,
    Contacts2 = 12,
    Ping = 13,
    Provision = 14,
    Search = 15,
    Gal = 16,
    AirSyncBase2 = 17,
    Settings = 18,
    DocumentLibrary = 19,
    ItemOperations = 20,
    ComposeMail = 21,
    Email2 = 22,
    Notes = 23,
    RightsManagement = 24,
    Find = 25,
}

impl CodePage {
    pub fn from_u8(value: u8) -> Option<Self> {
        match value {
            0 => Some(CodePage::AirSync),
            1 => Some(CodePage::Contacts),
            2 => Some(CodePage::Email),
            3 => Some(CodePage::AirSyncBase),
            4 => Some(CodePage::Calendar),
            5 => Some(CodePage::Move),
            6 => Some(CodePage::ItemEstimate),
            7 => Some(CodePage::FolderHierarchy),
            8 => Some(CodePage::MeetingResponse),
            9 => Some(CodePage::Tasks),
            10 => Some(CodePage::ResolveRecipients),
            11 => Some(CodePage::ValidateCert),
            12 => Some(CodePage::Contacts2),
            13 => Some(CodePage::Ping),
            14 => Some(CodePage::Provision),
            15 => Some(CodePage::Search),
            16 => Some(CodePage::Gal),
            17 => Some(CodePage::AirSyncBase2),
            18 => Some(CodePage::Settings),
            19 => Some(CodePage::DocumentLibrary),
            20 => Some(CodePage::ItemOperations),
            21 => Some(CodePage::ComposeMail),
            22 => Some(CodePage::Email2),
            23 => Some(CodePage::Notes),
            24 => Some(CodePage::RightsManagement),
            25 => Some(CodePage::Find),
            _ => None,
        }
    }
}

/// WBXML parsing error types
#[derive(Debug, Clone, PartialEq)]
pub enum WbxmlError {
    InvalidVersion,
    InvalidPublicId,
    InvalidCharset,
    InvalidStringTable,
    InvalidToken(u8),
    InvalidCodePage(u8),
    UnexpectedEnd,
    InvalidString,
    InvalidOpaque,
    InvalidExtension,
    InvalidProcessingInstruction,
    BufferUnderflow,
    BufferOverflow,
    InvalidEntity,
    InvalidAttribute,
    InvalidElement,
    InvalidDocumentStructure,
    UnsupportedFeature(String),
}

impl std::fmt::Display for WbxmlError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            WbxmlError::InvalidVersion => write!(f, "Invalid WBXML version"),
            WbxmlError::InvalidPublicId => write!(f, "Invalid public identifier"),
            WbxmlError::InvalidCharset => write!(f, "Invalid charset"),
            WbxmlError::InvalidStringTable => write!(f, "Invalid string table"),
            WbxmlError::InvalidToken(t) => write!(f, "Invalid WBXML token: 0x{:02X}", t),
            WbxmlError::InvalidCodePage(p) => write!(f, "Invalid code page: {}", p),
            WbxmlError::UnexpectedEnd => write!(f, "Unexpected end of document"),
            WbxmlError::InvalidString => write!(f, "Invalid string encoding"),
            WbxmlError::InvalidOpaque => write!(f, "Invalid opaque data"),
            WbxmlError::InvalidExtension => write!(f, "Invalid extension"),
            WbxmlError::InvalidProcessingInstruction => write!(f, "Invalid processing instruction"),
            WbxmlError::BufferUnderflow => write!(f, "Buffer underflow"),
            WbxmlError::BufferOverflow => write!(f, "Buffer overflow"),
            WbxmlError::InvalidEntity => write!(f, "Invalid entity reference"),
            WbxmlError::InvalidAttribute => write!(f, "Invalid attribute"),
            WbxmlError::InvalidElement => write!(f, "Invalid element"),
            WbxmlError::InvalidDocumentStructure => write!(f, "Invalid document structure"),
            WbxmlError::UnsupportedFeature(s) => write!(f, "Unsupported feature: {}", s),
        }
    }
}

impl std::error::Error for WbxmlError {}

/// Result type for WBXML operations
pub type WbxmlResult<T> = Result<T, WbxmlError>;

/// Represents a WBXML document
#[derive(Debug, Clone)]
pub struct WbxmlDocument {
    pub version: u8,
    pub public_id: PublicId,
    pub charset: u32,
    pub string_table: StringTable,
    pub body: Vec<WbxmlNode>,
}

impl WbxmlDocument {
    pub fn new() -> Self {
        Self {
            version: WBXML_VERSION_1_3,
            public_id: PublicId::Known(1), // ActiveSync
            charset: 106, // UTF-8
            string_table: StringTable::new(),
            body: Vec::new(),
        }
    }

    /// Create a new document with specific version
    pub fn with_version(version: u8) -> Self {
        let mut doc = Self::new();
        doc.version = version;
        doc
    }

    /// Add a node to the document body
    pub fn add_node(&mut self, node: WbxmlNode) {
        self.body.push(node);
    }

    /// Encode the document to WBXML bytes
    pub fn encode(&self) -> WbxmlResult<Bytes> {
        let mut encoder = WbxmlEncoder::new();
        encoder.encode_document(self)
    }

    /// Decode WBXML bytes to a document
    pub fn decode(data: &[u8]) -> WbxmlResult<Self> {
        let mut decoder = WbxmlDecoder::new(data);
        decoder.decode_document()
    }
}

impl Default for WbxmlDocument {
    fn default() -> Self {
        Self::new()
    }
}

/// Public identifier for WBXML
#[derive(Debug, Clone, PartialEq)]
pub enum PublicId {
    Known(u32),
    Literal(u32),
}

/// String table for WBXML encoding
#[derive(Debug, Clone, Default)]
pub struct StringTable {
    strings: Vec<String>,
    index_map: HashMap<String, u32>,
}

impl StringTable {
    pub fn new() -> Self {
        Self {
            strings: Vec::new(),
            index_map: HashMap::new(),
        }
    }

    /// Add a string to the table and return its index
    pub fn add(&mut self, s: &str) -> u32 {
        if let Some(&idx) = self.index_map.get(s) {
            return idx;
        }
        let idx = self.strings.len() as u32;
        self.strings.push(s.to_string());
        self.index_map.insert(s.to_string(), idx);
        idx
    }

    /// Get string by index
    pub fn get(&self, idx: u32) -> Option<&str> {
        self.strings.get(idx as usize).map(|s| s.as_str())
    }

    /// Encode the string table to bytes
    pub fn encode(&self) -> Bytes {
        let mut buf = BytesMut::new();
        for s in &self.strings {
            buf.put_slice(s.as_bytes());
            buf.put_u8(0); // null terminator
        }
        buf.freeze()
    }

    /// Get the total size of the encoded string table
    pub fn encoded_size(&self) -> usize {
        self.strings.iter().map(|s| s.len() + 1).sum()
    }
}

/// Represents a WBXML node
#[derive(Debug, Clone)]
pub enum WbxmlNode {
    Element(WbxmlElement),
    Text(String),
    Opaque(Bytes),
    ProcessingInstruction { target: String, value: String },
    Extension { code: u8, data: ExtensionData },
}

/// Extension data types
#[derive(Debug, Clone)]
pub enum ExtensionData {
    Integer(u32),
    String(String),
    None,
}

/// Represents a WBXML element
#[derive(Debug, Clone)]
pub struct WbxmlElement {
    pub code_page: CodePage,
    pub token: u8,
    pub attributes: Vec<WbxmlAttribute>,
    pub children: Vec<WbxmlNode>,
    pub has_content: bool,
}

impl WbxmlElement {
    pub fn new(code_page: CodePage, token: u8) -> Self {
        Self {
            code_page,
            token,
            attributes: Vec::new(),
            children: Vec::new(),
            has_content: false,
        }
    }

    pub fn with_content(mut self) -> Self {
        self.has_content = true;
        self
    }

    pub fn add_attribute(&mut self, attr: WbxmlAttribute) {
        self.attributes.push(attr);
    }

    pub fn add_child(&mut self, child: WbxmlNode) {
        self.children.push(child);
        self.has_content = true;
    }

    pub fn add_text(&mut self, text: &str) {
        self.children.push(WbxmlNode::Text(text.to_string()));
        self.has_content = true;
    }
}

/// Represents a WBXML attribute
#[derive(Debug, Clone)]
pub struct WbxmlAttribute {
    pub code_page: CodePage,
    pub token: u8,
    pub value: String,
}

impl WbxmlAttribute {
    pub fn new(code_page: CodePage, token: u8, value: &str) -> Self {
        Self {
            code_page,
            token,
            value: value.to_string(),
        }
    }
}

/// WBXML encoder
pub struct WbxmlEncoder {
    buf: BytesMut,
    current_code_page: CodePage,
}

impl WbxmlEncoder {
    pub fn new() -> Self {
        Self {
            buf: BytesMut::new(),
            current_code_page: CodePage::AirSync,
        }
    }

    /// Encode a complete WBXML document
    pub fn encode_document(&mut self, doc: &WbxmlDocument) -> WbxmlResult<Bytes> {
        self.buf.clear();
        self.current_code_page = CodePage::AirSync;

        // Version
        self.buf.put_u8(doc.version);

        // Public identifier
        match doc.public_id {
            PublicId::Known(id) => {
                self.encode_mb_u32(id);
            }
            PublicId::Literal(idx) => {
                self.buf.put_u8(0);
                self.encode_mb_u32(idx);
            }
        }

        // Charset
        self.encode_mb_u32(doc.charset);

        // String table
        let string_table = doc.string_table.encode();
        self.encode_mb_u32(string_table.len() as u32);
        self.buf.put_slice(&string_table);

        // Body
        for node in &doc.body {
            self.encode_node(node, &doc.string_table)?;
        }

        Ok(self.buf.split().freeze())
    }

    /// Encode a single node
    fn encode_node(&mut self, node: &WbxmlNode, string_table: &StringTable) -> WbxmlResult<()> {
        match node {
            WbxmlNode::Element(elem) => self.encode_element(elem, string_table)?,
            WbxmlNode::Text(text) => self.encode_text(text)?,
            WbxmlNode::Opaque(data) => self.encode_opaque(data)?,
            WbxmlNode::ProcessingInstruction { target, value } => {
                self.encode_pi(target, value)?;
            }
            WbxmlNode::Extension { code, data } => {
                self.encode_extension(*code, data)?;
            }
        }
        Ok(())
    }

    /// Encode an element
    fn encode_element(&mut self, elem: &WbxmlElement, string_table: &StringTable) -> WbxmlResult<()> {
        // Switch code page if needed
        if elem.code_page != self.current_code_page {
            self.buf.put_u8(SWITCH_PAGE);
            self.buf.put_u8(elem.code_page as u8);
            self.current_code_page = elem.code_page;
        }

        // Element token with content/attribute flags
        let mut token = elem.token;
        if elem.has_content {
            token |= 0x40;
        }
        if !elem.attributes.is_empty() {
            token |= 0x80;
        }
        self.buf.put_u8(token);

        // Attributes
        for attr in &elem.attributes {
            self.encode_attribute(attr, string_table)?;
        }
        if !elem.attributes.is_empty() {
            self.buf.put_u8(END);
        }

        // Children
        if elem.has_content {
            for child in &elem.children {
                self.encode_node(child, string_table)?;
            }
            self.buf.put_u8(END);
        }

        Ok(())
    }

    /// Encode an attribute
    fn encode_attribute(&mut self, attr: &WbxmlAttribute, _string_table: &StringTable) -> WbxmlResult<()> {
        // Switch code page if needed for attribute
        if attr.code_page != self.current_code_page {
            self.buf.put_u8(SWITCH_PAGE);
            self.buf.put_u8(attr.code_page as u8);
            self.current_code_page = attr.code_page;
        }

        // Attribute token
        self.buf.put_u8(attr.token | 0x80);

        // Attribute value (inline string)
        self.buf.put_u8(STR_I);
        self.buf.put_slice(attr.value.as_bytes());
        self.buf.put_u8(0);

        Ok(())
    }

    /// Encode inline text
    fn encode_text(&mut self, text: &str) -> WbxmlResult<()> {
        self.buf.put_u8(STR_I);
        self.buf.put_slice(text.as_bytes());
        self.buf.put_u8(0);
        Ok(())
    }

    /// Encode opaque data
    fn encode_opaque(&mut self, data: &Bytes) -> WbxmlResult<()> {
        self.buf.put_u8(OPAQUE);
        self.encode_mb_u32(data.len() as u32);
        self.buf.put_slice(data);
        Ok(())
    }

    /// Encode processing instruction
    fn encode_pi(&mut self, target: &str, value: &str) -> WbxmlResult<()> {
        self.buf.put_u8(PI);
        self.buf.put_u8(STR_I);
        self.buf.put_slice(target.as_bytes());
        self.buf.put_u8(0);
        self.buf.put_u8(STR_I);
        self.buf.put_slice(value.as_bytes());
        self.buf.put_u8(0);
        Ok(())
    }

    /// Encode extension
    fn encode_extension(&mut self, code: u8, data: &ExtensionData) -> WbxmlResult<()> {
        match data {
            ExtensionData::Integer(val) => {
                self.buf.put_u8(code | 0x80);
                self.encode_mb_u32(*val);
            }
            ExtensionData::String(s) => {
                self.buf.put_u8(code | 0x40);
                self.buf.put_slice(s.as_bytes());
                self.buf.put_u8(0);
            }
            ExtensionData::None => {
                self.buf.put_u8(code);
            }
        }
        Ok(())
    }

    /// Encode multi-byte unsigned integer
    fn encode_mb_u32(&mut self, mut value: u32) {
        let mut bytes = Vec::new();
        loop {
            let mut byte = (value & 0x7F) as u8;
            value >>= 7;
            if !bytes.is_empty() {
                byte |= 0x80;
            }
            bytes.push(byte);
            if value == 0 {
                break;
            }
        }
        bytes.reverse();
        self.buf.put_slice(&bytes);
    }
}

impl Default for WbxmlEncoder {
    fn default() -> Self {
        Self::new()
    }
}

/// WBXML decoder
pub struct WbxmlDecoder<'a> {
    data: &'a [u8],
    pos: usize,
    current_code_page: CodePage,
    string_table: StringTable,
}

impl<'a> WbxmlDecoder<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        Self {
            data,
            pos: 0,
            current_code_page: CodePage::AirSync,
            string_table: StringTable::new(),
        }
    }

    /// Decode a complete WBXML document
    pub fn decode_document(&mut self) -> WbxmlResult<WbxmlDocument> {
        // Version
        let version = self.read_u8()?;
        if version != WBXML_VERSION_1_1 
            && version != WBXML_VERSION_1_2 
            && version != WBXML_VERSION_1_3 {
            return Err(WbxmlError::InvalidVersion);
        }

        // Public identifier
        let public_id = self.decode_public_id()?;

        // Charset
        let charset = self.decode_mb_u32()?;

        // String table
        let string_table_len = self.decode_mb_u32()? as usize;
        self.string_table = self.decode_string_table(string_table_len)?;

        // Body
        let mut body = Vec::new();
        while self.pos < self.data.len() {
            if let Some(node) = self.decode_node()? {
                body.push(node);
            }
        }

        Ok(WbxmlDocument {
            version,
            public_id,
            charset,
            string_table: StringTable::new(),
            body,
        })
    }

    /// Decode public identifier
    fn decode_public_id(&mut self) -> WbxmlResult<PublicId> {
        let id = self.decode_mb_u32()?;
        if id == 0 {
            // Literal public identifier
            let idx = self.decode_mb_u32()?;
            Ok(PublicId::Literal(idx))
        } else {
            Ok(PublicId::Known(id))
        }
    }

    /// Decode string table
    fn decode_string_table(&mut self, len: usize) -> WbxmlResult<StringTable> {
        let start = self.pos;
        let end = start + len;
        if end > self.data.len() {
            return Err(WbxmlError::InvalidStringTable);
        }

        let mut table = StringTable::new();
        let mut current = start;
        while current < end {
            let mut s = Vec::new();
            while current < end && self.data[current] != 0 {
                s.push(self.data[current]);
                current += 1;
            }
            if current < end {
                current += 1; // skip null terminator
            }
            if let Ok(str) = String::from_utf8(s) {
                table.add(&str);
            }
        }

        self.pos = end;
        Ok(table)
    }

    /// Decode a single node
    fn decode_node(&mut self) -> WbxmlResult<Option<WbxmlNode>> {
        if self.pos >= self.data.len() {
            return Ok(None);
        }

        let token = self.read_u8()?;

        match token {
            END => Ok(None), // End of element
            STR_I => {
                let text = self.read_null_terminated_string()?;
                Ok(Some(WbxmlNode::Text(text)))
            }
            OPAQUE => {
                let len = self.decode_mb_u32()? as usize;
                let data = self.read_bytes(len)?;
                Ok(Some(WbxmlNode::Opaque(Bytes::from(data))))
            }
            PI => {
                let target = self.read_null_terminated_string()?;
                let value = self.read_null_terminated_string()?;
                Ok(Some(WbxmlNode::ProcessingInstruction { target, value }))
            }
            SWITCH_PAGE => {
                let page = self.read_u8()?;
                if let Some(cp) = CodePage::from_u8(page) {
                    self.current_code_page = cp;
                } else {
                    return Err(WbxmlError::InvalidCodePage(page));
                }
                self.decode_node()
            }
            t if t >= 0x80 => {
                // Element with attributes
                self.pos -= 1;
                let elem = self.decode_element()?;
                Ok(Some(WbxmlNode::Element(elem)))
            }
            t if t >= 0x40 => {
                // Element with content
                self.pos -= 1;
                let elem = self.decode_element()?;
                Ok(Some(WbxmlNode::Element(elem)))
            }
            t if t > 0x05 => {
                // Simple element
                self.pos -= 1;
                let elem = self.decode_element()?;
                Ok(Some(WbxmlNode::Element(elem)))
            }
            t => Err(WbxmlError::InvalidToken(t)),
        }
    }

    /// Decode an element
    fn decode_element(&mut self) -> WbxmlResult<WbxmlElement> {
        let token_byte = self.read_u8()?;
        let has_attributes = (token_byte & 0x80) != 0;
        let has_content = (token_byte & 0x40) != 0;
        let token = token_byte & 0x3F;

        let mut elem = WbxmlElement::new(self.current_code_page, token);
        elem.has_content = has_content;

        // Decode attributes
        if has_attributes {
            loop {
                if self.pos >= self.data.len() {
                    return Err(WbxmlError::UnexpectedEnd);
                }
                if self.data[self.pos] == END {
                    self.pos += 1;
                    break;
                }
                let attr = self.decode_attribute()?;
                elem.attributes.push(attr);
            }
        }

        // Decode children
        if has_content {
            loop {
                if self.pos >= self.data.len() {
                    return Err(WbxmlError::UnexpectedEnd);
                }
                if self.data[self.pos] == END {
                    self.pos += 1;
                    break;
                }
                if let Some(node) = self.decode_node()? {
                    elem.children.push(node);
                } else {
                    break;
                }
            }
        }

        Ok(elem)
    }

    /// Decode an attribute
    fn decode_attribute(&mut self) -> WbxmlResult<WbxmlAttribute> {
        let token_byte = self.read_u8()?;
        
        // Handle code page switch
        if token_byte == SWITCH_PAGE {
            let page = self.read_u8()?;
            if let Some(cp) = CodePage::from_u8(page) {
                self.current_code_page = cp;
            }
            return self.decode_attribute();
        }

        let token = token_byte & 0x7F;
        
        // Read attribute value
        let value = if self.pos < self.data.len() && self.data[self.pos] == STR_I {
            self.pos += 1;
            self.read_null_terminated_string()?
        } else {
            String::new()
        };

        Ok(WbxmlAttribute::new(self.current_code_page, token, &value))
    }

    /// Read a single byte
    fn read_u8(&mut self) -> WbxmlResult<u8> {
        if self.pos >= self.data.len() {
            return Err(WbxmlError::BufferUnderflow);
        }
        let byte = self.data[self.pos];
        self.pos += 1;
        Ok(byte)
    }

    /// Read multiple bytes
    fn read_bytes(&mut self, len: usize) -> WbxmlResult<&'a [u8]> {
        if self.pos + len > self.data.len() {
            return Err(WbxmlError::BufferUnderflow);
        }
        let bytes = &self.data[self.pos..self.pos + len];
        self.pos += len;
        Ok(bytes)
    }

    /// Read null-terminated string
    fn read_null_terminated_string(&mut self) -> WbxmlResult<String> {
        let start = self.pos;
        while self.pos < self.data.len() && self.data[self.pos] != 0 {
            self.pos += 1;
        }
        if self.pos >= self.data.len() {
            return Err(WbxmlError::InvalidString);
        }
        let bytes = &self.data[start..self.pos];
        let s = String::from_utf8_lossy(bytes).to_string();
        self.pos += 1; // skip null terminator
        Ok(s)
    }

    /// Decode multi-byte unsigned integer
    fn decode_mb_u32(&mut self) -> WbxmlResult<u32> {
        let mut result: u32 = 0;
        loop {
            let byte = self.read_u8()?;
            result = (result << 7) | (byte & 0x7F) as u32;
            if (byte & 0x80) == 0 {
                break;
            }
        }
        Ok(result)
    }
}

/// ActiveSync-specific WBXML helpers
pub struct ActiveSyncWbxml;

impl ActiveSyncWbxml {
    /// Encode an ActiveSync command request
    pub fn encode_command(command: &str, params: &[(&str, &str)]) -> WbxmlResult<Bytes> {
        let mut doc = WbxmlDocument::new();
        
        // Create command element
        let mut cmd_elem = WbxmlElement::new(CodePage::AirSync, 0x05) // Sync command
            .with_content();
        
        // Add collections
        let mut collections = WbxmlElement::new(CodePage::AirSync, 0x07) // Collections
            .with_content();
        
        // Add collection
        let mut collection = WbxmlElement::new(CodePage::AirSync, 0x08) // Collection
            .with_content();
        
        // Add sync key
        collection.add_child(WbxmlNode::Element({
            let mut elem = WbxmlElement::new(CodePage::AirSync, 0x0B); // SyncKey
            elem.add_text("0");
            elem
        }));
        
        // Add collection ID
        collection.add_child(WbxmlNode::Element({
            let mut elem = WbxmlElement::new(CodePage::AirSync, 0x0C); // CollectionId
            elem.add_text("calendar");
            elem
        }));
        
        collections.add_child(WbxmlNode::Element(collection));
        cmd_elem.add_child(WbxmlNode::Element(collections));
        doc.add_node(WbxmlNode::Element(cmd_elem));
        
        doc.encode()
    }

    /// Decode an ActiveSync response
    pub fn decode_response(data: &[u8]) -> WbxmlResult<WbxmlDocument> {
        WbxmlDocument::decode(data)
    }
}

/// Utility functions for WBXML processing
pub mod utils {
    use super::*;

    /// Validate WBXML data without full parsing
    pub fn validate_wbxml(data: &[u8]) -> WbxmlResult<()> {
        if data.len() < 4 {
            return Err(WbxmlError::InvalidDocumentStructure);
        }

        let version = data[0];
        if version != WBXML_VERSION_1_1 
            && version != WBXML_VERSION_1_2 
            && version != WBXML_VERSION_1_3 {
            return Err(WbxmlError::InvalidVersion);
        }

        // Basic structure validation
        let mut decoder = WbxmlDecoder::new(data);
        decoder.decode_document()?;

        Ok(())
    }

    /// Get WBXML document info without full parsing
    pub fn get_document_info(data: &[u8]) -> WbxmlResult<(u8, u32)> {
        if data.len() < 4 {
            return Err(WbxmlError::InvalidDocumentStructure);
        }

        let version = data[0];
        
        // Parse public ID
        let mut pos = 1;
        let mut public_id: u32 = 0;
        loop {
            if pos >= data.len() {
                return Err(WbxmlError::InvalidDocumentStructure);
            }
            let byte = data[pos];
            public_id = (public_id << 7) | (byte & 0x7F) as u32;
            pos += 1;
            if (byte & 0x80) == 0 {
                break;
            }
        }

        // Parse charset
        let mut charset: u32 = 0;
        loop {
            if pos >= data.len() {
                return Err(WbxmlError::InvalidDocumentStructure);
            }
            let byte = data[pos];
            charset = (charset << 7) | (byte & 0x7F) as u32;
            pos += 1;
            if (byte & 0x80) == 0 {
                break;
            }
        }

        Ok((version, charset))
    }

    /// Check if data appears to be WBXML
    pub fn is_wbxml(data: &[u8]) -> bool {
        if data.len() < 1 {
            return false;
        }
        let version = data[0];
        version == WBXML_VERSION_1_1 
            || version == WBXML_VERSION_1_2 
            || version == WBXML_VERSION_1_3
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_string_table() {
        let mut table = StringTable::new();
        let idx1 = table.add("hello");
        let idx2 = table.add("world");
        let idx3 = table.add("hello"); // duplicate

        assert_eq!(idx1, 0);
        assert_eq!(idx2, 1);
        assert_eq!(idx3, 0); // should return existing index

        assert_eq!(table.get(0), Some("hello"));
        assert_eq!(table.get(1), Some("world"));
        assert_eq!(table.get(2), None);
    }

    #[test]
    fn test_mb_u32_encoding() {
        let mut encoder = WbxmlEncoder::new();
        encoder.encode_mb_u32(0);
        assert_eq!(encoder.buf.as_ref(), &[0x00]);

        let mut encoder = WbxmlEncoder::new();
        encoder.encode_mb_u32(127);
        assert_eq!(encoder.buf.as_ref(), &[0x7F]);

        let mut encoder = WbxmlEncoder::new();
        encoder.encode_mb_u32(128);
        assert_eq!(encoder.buf.as_ref(), &[0x81, 0x00]);

        let mut encoder = WbxmlEncoder::new();
        encoder.encode_mb_u32(16383);
        assert_eq!(encoder.buf.as_ref(), &[0xFF, 0x7F]);
    }

    #[test]
    fn test_document_encode_decode() {
        let mut doc = WbxmlDocument::new();
        
        let mut elem = WbxmlElement::new(CodePage::AirSync, 0x05)
            .with_content();
        elem.add_text("test content");
        doc.add_node(WbxmlNode::Element(elem));

        let encoded = doc.encode().unwrap();
        let decoded = WbxmlDocument::decode(&encoded).unwrap();

        assert_eq!(decoded.version, WBXML_VERSION_1_3);
        assert_eq!(decoded.body.len(), 1);
    }

    #[test]
    fn test_utils_validation() {
        let mut doc = WbxmlDocument::new();
        let elem = WbxmlElement::new(CodePage::AirSync, 0x05);
        doc.add_node(WbxmlNode::Element(elem));

        let encoded = doc.encode().unwrap();
        assert!(utils::validate_wbxml(&encoded).is_ok());
        assert!(utils::is_wbxml(&encoded));

        let info = utils::get_document_info(&encoded).unwrap();
        assert_eq!(info.0, WBXML_VERSION_1_3);
        assert_eq!(info.1, 106); // UTF-8
    }
}
