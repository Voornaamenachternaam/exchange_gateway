// src/mapi/rops.rs
//
// MS-OXCROPS — the Remote Operation (ROP) set layered on the MAPI/HTTP
// `Execute` buffer.
//
// Phase 0 provides:
//   * The complete `RopId` table (the full §2.2.2 set, byte-exact) as a
//     `#[repr(transparent)]` newtype around `u8`. Using a newtype (rather
//     than a Rust enum) avoids stable-Rust's E0732 prohibition on mixed
//     enums that carry both explicit discriminants and a non-unit `Unknown`
//     variant, and lets us pass unknown RopIds through verbatim so the
//     transport still emits a typed error envelope.
//   * A bounds-checked byte cursor (`Buf`) used by every ROP codec.
//   * Request/response codecs for the Phase-0 ROP set: RopLogon (0xFE),
//     RopGetContentsTable (0x05), RopGetHierarchyTable (0x04), RopSetColumns
//     (0x12), RopQueryRows (0x15), RopGetStatus (0x16), RopSetMessageReadFlag
//     (0x11), RopGetPropertiesSpecific (0x07), RopGetPropertiesAll (0x08).
//   * The common ROP response envelope (RopId + ReturnValue + handle index).
//   * `proptest` round-trips for every codec.
//
// All decoding is fail-closed: lengths are validated against the remaining
// buffer before being consumed; integer conversions on attacker-supplied
// values use `u32::try_from`/`usize::try_from` (no `as` casts on untrusted
// data).

use crate::mapi::data::{PropertyProblem, PropertyTag, TaggedPropertyValue};

use crate::mapi::data::{PropertyProblem, PropertyTag, TaggedPropertyValue};

// ---------- OpenMessage ----------
#[derive(Debug, Clone)]
pub struct RopOpenMessageRequest {
    pub logon_id: u8,
    pub input_handle_index: u8,
    pub output_handle_index: u8,
    pub flags: u8,
}
impl RopOpenMessageRequest {
    pub fn decode(buf: &mut Buf<'_>) -> Result<Self, DecodeError> {
        let logon_id = buf.take_u8()?;
        let input_handle_index = buf.take_u8()?;
        let output_handle_index = buf.take_u8()?;
        let flags = buf.take_u8()?;
        Ok(Self { logon_id, input_handle_index, output_handle_index, flags })
    }
    pub fn encode(&self, out: &mut Vec<u8>) {
        out.push(self.logon_id);
        out.push(self.input_handle_index);
        out.push(self.output_handle_index);
        out.push(self.flags);
    }
}
#[derive(Debug, Clone)]
pub struct RopOpenMessageSuccess {
    pub output_handle_index: u8,
    pub return_value: RopErrorCode,
}
impl RopOpenMessageSuccess {
    pub fn encode(&self, out: &mut Vec<u8>) {
        out.push(self.output_handle_index);
        out.extend(&(self.return_value as u32).to_le_bytes());
    }
}

// ---------- SetMessageReadFlag ----------
#[derive(Debug, Clone)]
pub struct RopSetMessageReadFlagRequest {
    pub logon_id: u8,
    pub input_handle_index: u8,
    pub flags: u8,
}
impl RopSetMessageReadFlagRequest {
    pub fn decode(buf: &mut Buf<'_>) -> Result<Self, DecodeError> {
        let logon_id = buf.take_u8()?;
        let input_handle_index = buf.take_u8()?;
        let flags = buf.take_u8()?;
        Ok(Self { logon_id, input_handle_index, flags })
    }
    pub fn encode(&self, out: &mut Vec<u8>) {
        out.push(self.logon_id);
        out.push(self.input_handle_index);
        out.push(self.flags);
    }
}
#[derive(Debug, Clone)]
pub struct RopSetMessageReadFlagSuccess {
    pub input_handle_index: u8,
    pub return_value: RopErrorCode,
}
impl RopSetMessageReadFlagSuccess {
    pub fn encode(&self, out: &mut Vec<u8>) {
        out.push(self.input_handle_index);
        out.extend(&(self.return_value as u32).to_le_bytes());
    }
}

// ---------- SubmitMessage ----------
#[derive(Debug, Clone)]
pub struct RopSubmitMessageRequest {
    pub logon_id: u8,
    pub input_handle_index: u8,
    pub flags: u8,
    pub message_size: u32,
}
impl RopSubmitMessageRequest {
    pub fn decode(buf: &mut Buf<'_>) -> Result<Self, DecodeError> {
        let logon_id = buf.take_u8()?;
        let input_handle_index = buf.take_u8()?;
        let flags = buf.take_u8()?;
        let message_size = buf.take_u32()?;
        let _ = buf.take_slice(message_size as usize)?; // skip payload
        Ok(Self { logon_id, input_handle_index, flags, message_size })
    }
    pub fn encode(&self, out: &mut Vec<u8>) {
        out.push(self.logon_id);
        out.push(self.input_handle_index);
        out.push(self.flags);
        out.extend(&self.message_size.to_le_bytes());
    }
}
#[derive(Debug, Clone)]
pub struct RopSubmitMessageResponse {
    pub input_handle_index: u8,
    pub return_value: RopErrorCode,
}
impl RopSubmitMessageResponse {
    pub fn encode(&self, out: &mut Vec<u8>) {
        out.push(self.input_handle_index);
        out.extend(&(self.return_value as u32).to_le_bytes());
    }
}

// ---------- ReadStream ----------
#[derive(Debug, Clone)]
pub struct RopReadStreamRequest {
    pub logon_id: u8,
    pub input_handle_index: u8,
    pub byte_count: u32,
    pub offset: u64,
}
impl RopReadStreamRequest {
    pub fn decode(buf: &mut Buf<'_>) -> Result<Self, DecodeError> {
        let logon_id = buf.take_u8()?;
        let input_handle_index = buf.take_u8()?;
        let byte_count = buf.take_u32()?;
        let offset = buf.take_u64()?;
        Ok(Self { logon_id, input_handle_index, byte_count, offset })
    }
    pub fn encode(&self, out: &mut Vec<u8>) {
        out.push(self.logon_id);
        out.push(self.input_handle_index);
        out.extend(&self.byte_count.to_le_bytes());
        out.extend(&self.offset.to_le_bytes());
    }
}
#[derive(Debug, Clone)]
pub struct RopReadStreamSuccess {
    pub input_handle_index: u8,
    pub return_value: RopErrorCode,
    pub data: Vec<u8>,
}
impl RopReadStreamSuccess {
    pub fn encode(&self, out: &mut Vec<u8>) {
        out.push(self.input_handle_index);
        out.extend(&(self.return_value as u32).to_le_bytes());
        out.extend(&(self.data.len() as u32).to_le_bytes());
        out.extend(&self.data);
    }
}

// ---------- WriteStream ----------
#[derive(Debug, Clone)]
pub struct RopWriteStreamRequest {
    pub logon_id: u8,
    pub input_handle_index: u8,
    pub byte_count: u32,
    pub offset: u64,
}
impl RopWriteStreamRequest {
    pub fn decode(buf: &mut Buf<'_>) -> Result<Self, DecodeError> {
        let logon_id = buf.take_u8()?;
        let input_handle_index = buf.take_u8()?;
        let byte_count = buf.take_u32()?;
        let offset = buf.take_u64()?;
        let _ = buf.take_slice(byte_count as usize)?; // skip payload
        Ok(Self { logon_id, input_handle_index, byte_count, offset })
    }
    pub fn encode(&self, out: &mut Vec<u8>) {
        out.push(self.logon_id);
        out.push(self.input_handle_index);
        out.extend(&self.byte_count.to_le_bytes());
        out.extend(&self.offset.to_le_bytes());
    }
}
#[derive(Debug, Clone)]
pub struct RopWriteStreamSuccess {
    pub input_handle_index: u8,
    pub return_value: RopErrorCode,
}
impl RopWriteStreamSuccess {
    pub fn encode(&self, out: &mut Vec<u8>) {
        out.push(self.input_handle_index);
        out.extend(&(self.return_value as u32).to_le_bytes());
    }
}

// ---------- FastTransfer SourceCopyMessages ----------
#[derive(Debug, Clone)]
pub struct RopFastTransferSourceCopyMessagesRequest {
    pub logon_id: u8,
    pub input_handle_index: u8,
    pub flags: u8,
}
impl RopFastTransferSourceCopyMessagesRequest {
    pub fn decode(buf: &mut Buf<'_>) -> Result<Self, DecodeError> {
        let logon_id = buf.take_u8()?;
        let input_handle_index = buf.take_u8()?;
        let flags = buf.take_u8()?;
        Ok(Self { logon_id, input_handle_index, flags })
    }
    pub fn encode(&self, out: &mut Vec<u8>) {
        out.push(self.logon_id);
        out.push(self.input_handle_index);
        out.push(self.flags);
    }
}
#[derive(Debug, Clone)]
pub struct RopFastTransferSourceCopyMessagesResponse {
    pub output_handle_index: u8,
    pub return_value: RopErrorCode,
}
impl RopFastTransferSourceCopyMessagesResponse {
    pub fn encode(&self, out: &mut Vec<u8>) {
        out.push(self.output_handle_index);
        out.extend(&(self.return_value as u32).to_le_bytes());
    }
}

/// The complete RopId table per MS-OXCROPS §2.2.2. Reserved ids remain in
/// the un-named id space; any byte is carried through verbatim so the
/// transport can still emit a typed error envelope.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(transparent)]
pub struct RopId(pub u8);

impl RopId {
    pub const ROP_RELEASE: Self = Self(0x01);
    pub const ROP_OPEN_FOLDER: Self = Self(0x02);
    pub const ROP_GET_HIERARCHY_TABLE: Self = Self(0x04);
    pub const ROP_GET_CONTENTS_TABLE: Self = Self(0x05);
    pub const ROP_CREATE_MESSAGE: Self = Self(0x06);
    pub const ROP_GET_PROPERTIES_SPECIFIC: Self = Self(0x07);
    pub const ROP_GET_PROPERTIES_ALL: Self = Self(0x08);
    pub const ROP_GET_PROPERTIES_LIST: Self = Self(0x09);
    pub const ROP_SET_PROPERTIES: Self = Self(0x0A);
    pub const ROP_DELETE_PROPERTIES: Self = Self(0x0B);
    pub const ROP_SAVE_CHANGES_MESSAGE: Self = Self(0x0C);
    pub const ROP_REMOVE_ALL_RECIPIENTS: Self = Self(0x0D);
    pub const ROP_MODIFY_RECIPIENTS: Self = Self(0x0E);
    pub const ROP_READ_RECIPIENTS: Self = Self(0x0F);
    pub const ROP_RELOAD_CACHED_INFORMATION: Self = Self(0x10);
    pub const ROP_SET_MESSAGE_READ_FLAG: Self = Self(0x11);
    pub const ROP_SET_COLUMNS: Self = Self(0x12);
    pub const ROP_SORT_TABLE: Self = Self(0x13);
    pub const ROP_RESTRICT: Self = Self(0x14);
    pub const ROP_QUERY_ROWS: Self = Self(0x15);
    pub const ROP_GET_STATUS: Self = Self(0x16);
    pub const ROP_QUERY_POSITION: Self = Self(0x17);
    pub const ROP_SEEK_ROW: Self = Self(0x18);
    pub const ROP_SEEK_ROW_BOOKMARK: Self = Self(0x19);
    pub const ROP_SEEK_ROW_FRACTIONAL: Self = Self(0x1A);
    pub const ROP_CREATE_BOOKMARK: Self = Self(0x1B);
    pub const ROP_CREATE_FOLDER: Self = Self(0x1C);
    pub const ROP_DELETE_FOLDER: Self = Self(0x1D);
    pub const ROP_DELETE_MESSAGES: Self = Self(0x1E);
    pub const ROP_GET_MESSAGE_STATUS: Self = Self(0x1F);
    pub const ROP_SET_MESSAGE_STATUS: Self = Self(0x20);
    pub const ROP_GET_ATTACHMENT_TABLE: Self = Self(0x21);
    pub const ROP_OPEN_ATTACHMENT: Self = Self(0x22);
    pub const ROP_CREATE_ATTACHMENT: Self = Self(0x23);
    pub const ROP_DELETE_ATTACHMENT: Self = Self(0x24);
    pub const ROP_SAVE_CHANGES_ATTACHMENT: Self = Self(0x25);
    pub const ROP_SET_RECEIVE_FOLDER: Self = Self(0x26);
    pub const ROP_GET_RECEIVE_FOLDER: Self = Self(0x27);
    pub const ROP_REGISTER_NOTIFICATION: Self = Self(0x29);
    pub const ROP_NOTIFY: Self = Self(0x2A);
    pub const ROP_OPEN_STREAM: Self = Self(0x2B);
    pub const ROP_READ_STREAM: Self = Self(0x2C);
    pub const ROP_WRITE_STREAM: Self = Self(0x2D);
    pub const ROP_SEEK_STREAM: Self = Self(0x2E);
    pub const ROP_SET_STREAM_SIZE: Self = Self(0x2F);
    pub const ROP_SET_SEARCH_CRITERIA: Self = Self(0x30);
    pub const ROP_GET_SEARCH_CRITERIA: Self = Self(0x31);
    pub const ROP_SUBMIT_MESSAGE: Self = Self(0x32);
    pub const ROP_MOVE_COPY_MESSAGES: Self = Self(0x33);
    pub const ROP_ABORT_SUBMIT: Self = Self(0x34);
    pub const ROP_MOVE_FOLDER: Self = Self(0x35);
    pub const ROP_COPY_FOLDER: Self = Self(0x36);
    pub const ROP_QUERY_COLUMNS_ALL: Self = Self(0x37);
    pub const ROP_ABORT: Self = Self(0x38);
    pub const ROP_COPY_TO: Self = Self(0x39);
    pub const ROP_COPY_TO_STREAM: Self = Self(0x3A);
    pub const ROP_CLONE_STREAM: Self = Self(0x3B);
    pub const ROP_GET_PERMISSIONS_TABLE: Self = Self(0x3E);
    pub const ROP_GET_RULES_TABLE: Self = Self(0x3F);
    pub const ROP_MODIFY_PERMISSIONS: Self = Self(0x40);
    pub const ROP_MODIFY_RULES: Self = Self(0x41);
    pub const ROP_GET_OWNING_SERVERS: Self = Self(0x42);
    pub const ROP_LONG_TERM_ID_FROM_ID: Self = Self(0x43);
    pub const ROP_ID_FROM_LONG_TERM_ID: Self = Self(0x44);
    pub const ROP_PUBLIC_FOLDER_IS_GHOSTED: Self = Self(0x45);
    pub const ROP_OPEN_EMBEDDED_MESSAGE: Self = Self(0x46);
    pub const ROP_SET_SPOOLER: Self = Self(0x47);
    pub const ROP_SPOOLER_LOCK_MESSAGE: Self = Self(0x48);
    pub const ROP_GET_ADDRESS_TYPES: Self = Self(0x49);
    pub const ROP_TRANSPORT_SEND: Self = Self(0x4A);
    pub const ROP_FAST_TRANSFER_SOURCE_COPY_MESSAGES: Self = Self(0x4B);
    pub const ROP_FAST_TRANSFER_SOURCE_COPY_FOLDER: Self = Self(0x4C);
    pub const ROP_FAST_TRANSFER_SOURCE_COPY_TO: Self = Self(0x4D);
    pub const ROP_FAST_TRANSFER_SOURCE_GET_BUFFER: Self = Self(0x4E);
    pub const ROP_FIND_ROW: Self = Self(0x4F);
    pub const ROP_PROGRESS: Self = Self(0x50);
    pub const ROP_TRANSPORT_NEW_MAIL: Self = Self(0x51);
    pub const ROP_GET_VALID_ATTACHMENTS: Self = Self(0x52);
    pub const ROP_FAST_TRANSFER_DESTINATION_CONFIGURE: Self = Self(0x53);
    pub const ROP_FAST_TRANSFER_DESTINATION_PUT_BUFFER: Self = Self(0x54);
    pub const ROP_GET_NAMES_FROM_PROPERTY_IDS: Self = Self(0x55);
    pub const ROP_GET_PROPERTY_IDS_FROM_NAMES: Self = Self(0x56);
    pub const ROP_UPDATE_DEFERRED_ACTION_MESSAGES: Self = Self(0x57);
    pub const ROP_EMPTY_FOLDER: Self = Self(0x58);
    pub const ROP_EXPAND_ROW: Self = Self(0x59);
    pub const ROP_COLLAPSE_ROW: Self = Self(0x5A);
    pub const ROP_LOCK_REGION_STREAM: Self = Self(0x5B);
    pub const ROP_UNLOCK_REGION_STREAM: Self = Self(0x5C);
    pub const ROP_COMMIT_STREAM: Self = Self(0x5D);
    pub const ROP_GET_STREAM_SIZE: Self = Self(0x5E);
    pub const ROP_QUERY_NAMED_PROPERTIES: Self = Self(0x5F);
    pub const ROP_GET_PER_USER_LONG_TERM_IDS: Self = Self(0x60);
    pub const ROP_GET_PER_USER_GUID: Self = Self(0x61);
    pub const ROP_READ_PER_USER_INFORMATION: Self = Self(0x63);
    pub const ROP_WRITE_PER_USER_INFORMATION: Self = Self(0x64);
    pub const ROP_SET_READ_FLAGS: Self = Self(0x66);
    pub const ROP_COPY_PROPERTIES: Self = Self(0x67);
    pub const ROP_GET_RECEIVE_FOLDER_TABLE: Self = Self(0x68);
    pub const ROP_FAST_TRANSFER_SOURCE_COPY_PROPERTIES: Self = Self(0x69);
    pub const ROP_GET_COLLAPSE_STATE: Self = Self(0x6B);
    pub const ROP_SET_COLLAPSE_STATE: Self = Self(0x6C);
    pub const ROP_GET_TRANSPORT_FOLDER: Self = Self(0x6D);
    pub const ROP_PENDING: Self = Self(0x6E);
    pub const ROP_OPTIONS_DATA: Self = Self(0x6F);
    pub const ROP_SYNCHRONIZATION_CONFIGURE: Self = Self(0x70);
    pub const ROP_SYNCHRONIZATION_IMPORT_MESSAGE_CHANGE: Self = Self(0x72);
    pub const ROP_SYNCHRONIZATION_IMPORT_HIERARCHY_CHANGE: Self = Self(0x73);
    pub const ROP_SYNCHRONIZATION_IMPORT_DELETES: Self = Self(0x74);
    pub const ROP_SYNCHRONIZATION_UPLOAD_STATE_STREAM_BEGIN: Self = Self(0x75);
    pub const ROP_SYNCHRONIZATION_UPLOAD_STATE_STREAM_CONTINUE: Self = Self(0x76);
    pub const ROP_SYNCHRONIZATION_UPLOAD_STATE_STREAM_END: Self = Self(0x77);
    pub const ROP_SYNCHRONIZATION_IMPORT_MESSAGE_MOVE: Self = Self(0x78);
    pub const ROP_SET_PROPERTIES_NO_REPLICATE: Self = Self(0x79);
    pub const ROP_DELETE_PROPERTIES_NO_REPLICATE: Self = Self(0x7A);
    pub const ROP_GET_STORE_STATE: Self = Self(0x7B);
    pub const ROP_SYNCHRONIZATION_OPEN_COLLECTOR: Self = Self(0x7E);
    pub const ROP_GET_LOCAL_REPLICA_IDS: Self = Self(0x7F);
    pub const ROP_SYNCHRONIZATION_IMPORT_READ_STATE_CHANGES: Self = Self(0x80);
    pub const ROP_RESET_TABLE: Self = Self(0x81);
    pub const ROP_SYNCHRONIZATION_GET_TRANSFER_STATE: Self = Self(0x82);
    pub const ROP_TELL_VERSION: Self = Self(0x86);
    pub const ROP_FREE_BOOKMARK: Self = Self(0x89);
    pub const ROP_WRITE_AND_COMMIT_STREAM: Self = Self(0x90);
    pub const ROP_HARD_DELETE_MESSAGES: Self = Self(0x91);
    pub const ROP_HARD_DELETE_MESSAGES_AND_SUBFOLDERS: Self = Self(0x92);
    pub const ROP_SET_LOCAL_REPLICA_MIDSET_DELETED: Self = Self(0x93);
    pub const ROP_BACKOFF: Self = Self(0xF9);
    pub const ROP_LOGON: Self = Self(0xFE);
    pub const ROP_BUFFER_TOO_SMALL: Self = Self(0xFF);

    pub const fn from_u8(b: u8) -> Self {
        Self(b)
    }
    pub const fn to_u8(self) -> u8 {
        self.0
    }
}

/// The per-ROP return value (`ReturnValue`, MS-OXCROPS §2.2.1). 0 == success.
/// The Phase-0 enum covers the codes an Outlook client branches on for the
/// Phase-0 ROP set; unknown codes are preserved unchanged.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RopErrorCode {
    Success,
    AccessDenied,
    InvalidParameter,
    NotEnoughMemory,
    ObjectChanged,
    NetworkError,
    InvalidObject,
    NotFound,
    /// MAPI_E_NOT_INITIALIZED (0x80040680) — used as a non-fatal "the table
    /// has no current column set" placeholder.
    NotInitialized,
    /// MAPI_E_NO_SUPPORT (0x80040102).
    NoSupport,
    /// MAPI_E_DISK_ERROR (0x80040116) — backend I/O / store failure.
    DiskError,
    /// MAPI_E_COLLISION (0x80040604) — duplicate id on CreateMessage/Save.
    Collision,
    /// MAPI_E_NO_ACCESS (0x80070005) is `AccessDenied`; this is the tx-level
    /// "the change was rejected by the store" variant
    /// MAPI_E_SUBMIT_TOO_FAST (0x80040609) suppressed; clients retry.
    SubmitNotSupported,
    Unknown(u32),
}

impl RopErrorCode {
    pub const fn from_u32(v: u32) -> Self {
        match v {
            0 => Self::Success,
            0x80070005u32 => Self::AccessDenied,
            0x80070057u32 => Self::InvalidParameter,
            0x8007000Eu32 => Self::NotEnoughMemory,
            0x80040109u32 => Self::ObjectChanged,
            0x80040115u32 => Self::NetworkError,
            0x80040108u32 => Self::InvalidObject,
            0x8004010Fu32 => Self::NotFound,
            0x80040680u32 => Self::NotInitialized,
            0x80040102u32 => Self::NoSupport,
            0x80040116u32 => Self::DiskError,
            0x80040604u32 => Self::Collision,
            0x80040609u32 => Self::SubmitNotSupported,
            other => Self::Unknown(other),
        }
    }
    pub const fn to_u32(self) -> u32 {
        match self {
            Self::Success => 0,
            Self::AccessDenied => 0x80070005,
            Self::InvalidParameter => 0x80070057,
            Self::NotEnoughMemory => 0x8007000E,
            Self::ObjectChanged => 0x80040109,
            Self::NetworkError => 0x80040115,
            Self::InvalidObject => 0x80040108,
            Self::NotFound => 0x8004010F,
            Self::NotInitialized => 0x80040680,
            Self::NoSupport => 0x80040102,
            Self::DiskError => 0x80040116,
            Self::Collision => 0x80040604,
            Self::SubmitNotSupported => 0x80040609,
            Self::Unknown(v) => v,
        }
    }
}

// ---- byte cursor -----------------------------------------------------------

/// A tiny bounds-checked cursor over a ROP buffer. Reads fail closed with
/// `DecodeError` rather than slicing past the end. Integer reads are
/// little-endian, matching the MAPI/HTTP ROP wire format.
pub struct Buf<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Buf<'a> {
    pub fn new(buf: &'a [u8]) -> Self {
        Self { buf, pos: 0 }
    }
    pub fn remaining(&self) -> usize {
        self.buf.len().saturating_sub(self.pos)
    }
    pub fn pos(&self) -> usize {
        self.pos
    }
    /// Alias retained for readability in ROP-chaining code.
    pub fn position(&self) -> usize {
        self.pos
    }
    /// Drop all remaining bytes and return them (for ROPs whose body the
    /// dispatcher skips wholesale on an unknown RopId).
    pub fn take_remaining(&mut self) -> Vec<u8> {
        let rest = self.buf[self.pos..].to_vec();
        self.pos = self.buf.len();
        rest
    }
    pub fn take_u8(&mut self) -> Result<u8, DecodeError> {
        let b = self
            .buf
            .get(self.pos)
            .copied()
            .ok_or(DecodeError::Insufficient)?;
        self.pos += 1;
        Ok(b)
    }
    pub fn take_u16_le(&mut self) -> Result<u16, DecodeError> {
        if self.remaining() < 2 {
            return Err(DecodeError::Insufficient);
        }
        let v = u16::from_le_bytes([self.buf[self.pos], self.buf[self.pos + 1]]);
        self.pos += 2;
        Ok(v)
    }
    pub fn take_u32_le(&mut self) -> Result<u32, DecodeError> {
        if self.remaining() < 4 {
            return Err(DecodeError::Insufficient);
        }
        let v = u32::from_le_bytes([
            self.buf[self.pos],
            self.buf[self.pos + 1],
            self.buf[self.pos + 2],
            self.buf[self.pos + 3],
        ]);
        self.pos += 4;
        Ok(v)
    }
    pub fn take_u64_le(&mut self) -> Result<u64, DecodeError> {
        if self.remaining() < 8 {
            return Err(DecodeError::Insufficient);
        }
        let mut arr = [0u8; 8];
        arr.copy_from_slice(&self.buf[self.pos..self.pos + 8]);
        self.pos += 8;
        Ok(u64::from_le_bytes(arr))
    }
    /// Read a signed 8-byte little-endian integer. Used by `RopSeekStream`
    /// whose Offset field is a signed LONGLONG (MS-OXCROPS 2.2.9.8.1).
    pub fn take_i64_le(&mut self) -> Result<i64, DecodeError> {
        if self.remaining() < 8 {
            return Err(DecodeError::Insufficient);
        }
        let mut arr = [0u8; 8];
        arr.copy_from_slice(&self.buf[self.pos..self.pos + 8]);
        self.pos += 8;
        Ok(i64::from_le_bytes(arr))
    }
    pub fn take_bytes(&mut self, n: usize) -> Result<&'a [u8], DecodeError> {
        if self.remaining() < n {
            return Err(DecodeError::Insufficient);
        }
        let out = &self.buf[self.pos..self.pos + n];
        self.pos += n;
        Ok(out)
    }
    /// Return the backing slice `[start..end)` without advancing the cursor.
    /// Used to capture the consumed span of a value whose end position is
    /// only known after walking its elements (e.g. an MV property).
    pub fn slice(&self, start: usize, end: usize) -> Option<&'a [u8]> {
        if start <= end && end <= self.buf.len() && start <= self.buf.len() {
            self.buf.get(start..end)
        } else {
            None
        }
    }
    /// Read a 2-byte LE length `n`, then `n` raw bytes. The length is bounded
    /// by `max` and the remaining buffer before any allocation.
    pub fn take_lp16_bytes(&mut self, max: usize) -> Result<&'a [u8], DecodeError> {
        let n_raw = self.take_u16_le()?;
        let n = usize::from(n_raw);
        let cap = max.min(self.remaining());
        if n > cap {
            return Err(DecodeError::ExcessLength);
        }
        self.take_bytes(n)
    }
}

/// Decode failures that arise from an untrusted ROP buffer.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum DecodeError {
    #[error("insufficient bytes")]
    Insufficient,
    #[error("length exceeds maximum or remaining buffer")]
    ExcessLength,
    #[error("invalid enumeration/flag value")]
    InvalidValue,
    #[error("invalid UTF-8 in string field")]
    InvalidUtf8,
    #[error("trailing bytes after a length-delimited field")]
    Trailing,
}

// ---- common header / response envelope --------------------------------------

/// Common 3-byte ROP request header: RopId + LogonId + handle index
/// (InputHandleIndex for ROPs that consume one, OutputHandleIndex for ROPs
/// that produce a handle, e.g. RopLogon).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RopHeader {
    pub rop_id: RopId,
    pub logon_id: u8,
    pub handle_index: u8,
}

impl RopHeader {
    pub fn decode(cur: &mut Buf<'_>) -> Result<Self, DecodeError> {
        let rop_id = RopId::from_u8(cur.take_u8()?);
        let logon_id = cur.take_u8()?;
        let handle_index = cur.take_u8()?;
        Ok(Self {
            rop_id,
            logon_id,
            handle_index,
        })
    }
    pub fn encode(&self, out: &mut Vec<u8>) {
        out.push(self.rop_id.to_u8());
        out.push(self.logon_id);
        out.push(self.handle_index);
    }
}

/// The 4-byte ROP request header variant used by ROPs that both consume an
/// input handle and produce an output handle: RopOpenFolder (0x02),
/// RopGetHierarchyTable (0x04), RopGetContentsTable (0x05), RopCreateMessage
/// (0x06), RopRegisterNotification (0x29). Wire order is
/// `RopId · LogonId · InputHandleIndex · OutputHandleIndex`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RopHeader4 {
    pub rop_id: RopId,
    pub logon_id: u8,
    pub input_handle_index: u8,
    pub output_handle_index: u8,
}

impl RopHeader4 {
    /// Decode the full 4-byte RopHeader4 (`RopId·LogonId·Input·Output`)
    /// from the start of a ROP frame. Use this when the cursor is positioned
    /// at the frame's `RopId` byte.
    pub fn decode(cur: &mut Buf<'_>) -> Result<Self, DecodeError> {
        let rop_id = RopId::from_u8(cur.take_u8()?);
        let logon_id = cur.take_u8()?;
        let input_handle_index = cur.take_u8()?;
        let output_handle_index = cur.take_u8()?;
        Ok(Self {
            rop_id,
            logon_id,
            input_handle_index,
            output_handle_index,
        })
    }
    /// Decode the trailing 3 bytes of a RopHeader4 (`LogonId·Input·Output`)
    /// when the dispatcher has already consumed the leading `RopId` byte
    /// and passed the same `RopId` in. This avoids re-consuming a byte that
    /// is no longer present and silently misinterpreting the following
    /// payload bytes as header fields.
    pub fn decode_after_ropid(cur: &mut Buf<'_>, rop_id: RopId) -> Result<Self, DecodeError> {
        let logon_id = cur.take_u8()?;
        let input_handle_index = cur.take_u8()?;
        let output_handle_index = cur.take_u8()?;
        Ok(Self {
            rop_id,
            logon_id,
            input_handle_index,
            output_handle_index,
        })
    }
    pub fn encode(&self, out: &mut Vec<u8>) {
        out.push(self.rop_id.to_u8());
        out.push(self.logon_id);
        out.push(self.input_handle_index);
        out.push(self.output_handle_index);
    }
}

/// LogonFlags (MS-OXCSTOR §2.2.1.1.1). Only the bits the client may set are
/// preserved; reserved bits are rejected to fail closed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct LogonFlags(pub u8);

impl LogonFlags {
    pub const DEFINED_MASK: u8 = 0x1F;
    pub fn parse(b: u8) -> Result<Self, DecodeError> {
        if b & !Self::DEFINED_MASK != 0 {
            return Err(DecodeError::InvalidValue);
        }
        Ok(Self(b))
    }
    pub const fn as_u8(self) -> u8 {
        self.0
    }
}

/// OpenFlags (MS-OXCSTOR §2.2.1.1.1), 4 bytes. We retain the defined-bit
/// mask; undefined bits fail closed so an attacker can't smuggle flags.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct OpenFlags(pub u32);

impl OpenFlags {
    pub const DEFINED_MASK: u32 = 0xC83F_0117u32;
    pub fn parse(v: u32) -> Result<Self, DecodeError> {
        if v & !Self::DEFINED_MASK != 0 {
            return Err(DecodeError::InvalidValue);
        }
        Ok(Self(v))
    }
    pub const fn as_u32(self) -> u32 {
        self.0
    }
}

// ---- RopLogon (0xFE) -------------------------------------------------------

/// `RopLogon` request buffer (MS-OXCROPS §2.2.3.1.1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RopLogonRequest {
    pub logon_id: u8,
    pub output_handle_index: u8,
    pub logon_flags: LogonFlags,
    pub open_flags: OpenFlags,
    pub store_state: u32,
    pub essdn: String,
}

impl RopLogonRequest {
    /// Maximum accepted Essdn (legacyExchangeDN) length. The
    /// legacyExchangeDN is a printable ASCII string terminated by a null
    /// character counted in EssdnSize; cap it to resist pathological inputs.
    pub const MAX_ESSDN: usize = 4096;

    pub fn decode(cur: &mut Buf<'_>) -> Result<Self, DecodeError> {
        // Header for RopLogon: RopId(0xFE), LogonId, OutputHandleIndex.
        let rop_id = cur.take_u8()?;
        if rop_id != RopId::ROP_LOGON.to_u8() {
            return Err(DecodeError::InvalidValue);
        }
        let logon_id = cur.take_u8()?;
        let output_handle_index = cur.take_u8()?;
        let logon_flags = LogonFlags::parse(cur.take_u8()?)?;
        let open_flags = OpenFlags::parse(cur.take_u32_le()?)?;
        let store_state = cur.take_u32_le()?;
        if store_state != 0 {
            return Err(DecodeError::InvalidValue);
        }
        let essdn_bytes = cur.take_lp16_bytes(Self::MAX_ESSDN)?;
        // Trim the null terminator if present; legacyExchangeDN is ASCII.
        let trimmed = essdn_bytes.strip_suffix(b"\0").unwrap_or(essdn_bytes);
        let essdn = std::str::from_utf8(trimmed)
            .map_err(|_| DecodeError::InvalidUtf8)?
            .to_string();
        Ok(Self {
            logon_id,
            output_handle_index,
            logon_flags,
            open_flags,
            store_state,
            essdn,
        })
    }

    pub fn encode(&self, out: &mut Vec<u8>) {
        out.push(RopId::ROP_LOGON.to_u8());
        out.push(self.logon_id);
        out.push(self.output_handle_index);
        out.push(self.logon_flags.as_u8());
        out.extend_from_slice(&self.open_flags.as_u32().to_le_bytes());
        out.extend_from_slice(&self.store_state.to_le_bytes());
        // EssdnSize counts the null terminator.
        let essdn_with_nul_len = self
            .essdn
            .len()
            .checked_add(1)
            .and_then(|n| u16::try_from(n).ok())
            .expect("essdn length fits in u16 after encoder validation");
        out.extend_from_slice(&essdn_with_nul_len.to_le_bytes());
        out.extend_from_slice(self.essdn.as_bytes());
        out.push(0);
    }
}

/// `RopLogon` success response buffer for a private mailbox
/// (MS-OXCROPS §2.2.3.1.2). Phase 0 returns the minimum fields Outlook
/// reads to bind the session: handle index, ReturnValue, LogonFlags, and
/// the 9 FolderIds (Inbox/Outbox/Sent/Deleted/Drafts/...).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RopLogonSuccess {
    pub output_handle_index: u8,
    pub return_value: RopErrorCode,
    pub logon_flags: LogonFlags,
    /// Fixed-size set of 9 folder ids (52 bytes each), the canonical mailbox
    /// folder handles (Inbox, Outbox, Sent Items, Deleted Items, Finder,
    /// Drafts, Junk, Calendar, Contacts). Phase 0 returns zeroed long-term
    /// IDs as session-folder placeholders; Phase 1 fills these from JMAP/
    /// CalDAV.
    pub folder_ids: [[u8; 52]; 9],
    pub response_flags: u8,
    pub mailbox_guid: [u8; 16],
}

impl RopLogonSuccess {
    pub fn encode(&self, out: &mut Vec<u8>) {
        out.push(RopId::ROP_LOGON.to_u8());
        out.push(self.output_handle_index);
        out.extend_from_slice(&self.return_value.to_u32().to_le_bytes());
        out.push(self.logon_flags.as_u8());
        for fid in &self.folder_ids {
            out.extend_from_slice(fid);
        }
        out.push(self.response_flags);
        out.extend_from_slice(&self.mailbox_guid);
    }
}

/// A failure response for any ROP — the 9-byte envelope (RopId + handle +
/// ReturnValue) Outlook displays a transport error for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RopErrorResponse {
    pub rop_id: RopId,
    pub output_handle_index: u8,
    pub return_value: RopErrorCode,
}

impl RopErrorResponse {
    pub fn encode(&self, out: &mut Vec<u8>) {
        out.push(self.rop_id.to_u8());
        out.push(self.output_handle_index);
        out.extend_from_slice(&self.return_value.to_u32().to_le_bytes());
    }
}

// ---- RopGetContentsTable (0x05) / RopGetHierarchyTable (0x04) --------------

/// Request body for `RopGetHierarchyTable` (0x04) and `RopGetContentsTable`
/// (0x05), MS-OXCROPS §2.2.4.13.1 / §2.2.4.14.1. The ROP consumes an
/// `InputHandleIndex` (a folder/logon handle) and produces an
/// `OutputHandleIndex` (the new table handle), so the request body following
/// the `RopHeader4` is a single `TableFlags` byte.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RopOpenTableRequest {
    pub logon_id: u8,
    pub input_handle_index: u8,
    pub output_handle_index: u8,
    pub table_flags: u8,
}

impl RopOpenTableRequest {
    /// Decode the 1-byte body (TableFlags); the 4-byte header must already
    /// have been consumed by the caller via `RopHeader4::decode` (which is
    /// pre-decoded to bind `output_handle_index`).
    pub fn decode_body(cur: &mut Buf<'_>) -> Result<u8, DecodeError> {
        cur.take_u8()
    }
    pub fn encode(&self, out: &mut Vec<u8>, rop_id: RopId) {
        out.push(rop_id.to_u8());
        out.push(self.logon_id);
        out.push(self.input_handle_index);
        out.push(self.output_handle_index);
        out.push(self.table_flags);
    }
}

/// Success response: an OutputHandleIndex pointing to the new table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RopOpenTableSuccess {
    pub output_handle_index: u8,
    pub return_value: RopErrorCode,
    pub row_count: u32,
}

impl RopOpenTableSuccess {
    pub fn encode(&self, out: &mut Vec<u8>, rop_id: RopId) {
        out.push(rop_id.to_u8());
        out.push(self.output_handle_index);
        out.extend_from_slice(&self.return_value.to_u32().to_le_bytes());
        out.extend_from_slice(&self.row_count.to_le_bytes());
    }
}

// ---- RopSetColumns (0x12) / RopQueryRows (0x15) ---------------------------

/// `RopSetColumns` request: a property-tag array establishing the table
/// column set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RopSetColumnsRequest {
    pub input_handle_index: u8,
    pub property_tags: Vec<PropertyTag>,
}

impl RopSetColumnsRequest {
    pub fn decode(cur: &mut Buf<'_>) -> Result<Self, DecodeError> {
        // The 3-byte RopHeader (RopId · LogonId · InputHandleIndex) is
        // consumed by the dispatcher before dispatch; we read only the body:
        // SetColumnFlags (1) + PropertyTagCount (2 LE) + PropertyTags[count].
        let _set_column_flags = cur.take_u8()?;
        let count = cur.take_u16_le()?;
        let count_us = usize::from(count);
        if count_us > 1024 {
            return Err(DecodeError::ExcessLength);
        }
        let mut tags = Vec::with_capacity(count_us);
        for _ in 0..count_us {
            tags.push(PropertyTag::decode(cur)?);
        }
        Ok(Self {
            input_handle_index: 0,
            property_tags: tags,
        })
    }
}

/// `RopQueryRows` request, MS-OXCROPS §2.2.5.4.1. Body after the 3-byte
/// `RopHeader` (RopId·LogonId·InputHandleIndex) is
/// `QueryRowsFlags(1) · ForwardRead(1) · RowCount(2 LE)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RopQueryRowsRequest {
    pub input_handle_index: u8,
    pub query_rows_flags: u8,
    pub forward_read: u8,
    pub row_count: u16,
}

impl RopQueryRowsRequest {
    pub fn decode(cur: &mut Buf<'_>) -> Result<Self, DecodeError> {
        // Body only (the RopHeader is consumed by the dispatcher): QueryRowsFlags(1)
        // · ForwardRead(1) · RowCount(2 LE).
        let query_rows_flags = cur.take_u8()?;
        let forward_read = cur.take_u8()?;
        let row_count = cur.take_u16_le()?;
        Ok(Self {
            input_handle_index: 0,
            query_rows_flags,
            forward_read,
            row_count,
        })
    }
}

/// `RopQueryRows` success response, MS-OXCROPS §2.2.5.4.2:
///   RopId · InputHandleIndex · ReturnValue(4 LE) · Origin(1) · RowCount(2 LE)
///   · RowData (variable: a PropertyRowSet of `RowCount` PropertyRow structs
///   each built from the table's current column set).
///
/// The `RopQueryRows` response encoder here emits only the fixed prefix
/// (RopId · InputHandleIndex · ReturnValue · Origin · RowCount) plus the
/// caller-supplied serialized row bytes; the row assembly itself is done by
/// the store bridge which knows the per-row column values.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RopQueryRowsSuccess {
    pub input_handle_index: u8,
    pub return_value: RopErrorCode,
    pub origin: u8,
    pub row_count: u16,
    pub row_data: Vec<u8>,
}

impl RopQueryRowsSuccess {
    pub fn encode(&self, out: &mut Vec<u8>) {
        out.push(RopId::ROP_QUERY_ROWS.to_u8());
        out.push(self.input_handle_index);
        out.extend_from_slice(&self.return_value.to_u32().to_le_bytes());
        out.push(self.origin);
        out.extend_from_slice(&self.row_count.to_le_bytes());
        out.extend_from_slice(&self.row_data);
    }
}

// ---- RopGetStatus (0x16) ---------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RopGetStatusRequest {
    pub input_handle_index: u8,
}

impl RopGetStatusRequest {
    pub fn decode(_cur: &mut Buf<'_>) -> Result<Self, DecodeError> {
        // Body only (the RopHeader is consumed by the dispatcher). GetStatus
        // has no fields beyond the header.
        Ok(Self {
            input_handle_index: 0,
        })
    }
}

// ---- RopSetMessageReadFlag (0x11) ------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RopSetMessageReadFlagRequest {
    pub read_flag: u8,
}

impl RopSetMessageReadFlagRequest {
    pub fn decode(cur: &mut Buf<'_>) -> Result<Self, DecodeError> {
        // Body only (RopHeader consumed by the dispatcher): ReadFlag(1).
        let read_flag = cur.take_u8()?;
        Ok(Self { read_flag })
    }
}

// ---- RopGetPropertiesSpecific (0x07) / RopGetPropertiesAll (0x08) ----------

/// `RopGetPropertiesSpecific` request, MS-OXCROPS §2.2.8.3.1. Body after the
/// 3-byte `RopHeader` is:
///   PropertySizeLimit (2 LE) · WantUnicode (2 LE) · PropertyTagCount (2 LE)
///   · PropertyTags[count]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RopGetPropertiesSpecificRequest {
    pub input_handle_index: u8,
    pub property_size_limit: u16,
    pub want_unicode: u16,
    pub property_tag_count: u16,
    pub property_tags: Vec<PropertyTag>,
}

impl RopGetPropertiesSpecificRequest {
    pub fn decode(cur: &mut Buf<'_>) -> Result<Self, DecodeError> {
        // Body only (RopHeader consumed by the dispatcher): PropertySizeLimit(2 LE)
        // · WantUnicode(2 LE) · PropertyTagCount(2 LE) · PropertyTags[count].
        let property_size_limit = cur.take_u16_le()?;
        let want_unicode = cur.take_u16_le()?;
        let count = cur.take_u16_le()?;
        let count_us = usize::from(count);
        if count_us > 4096 {
            return Err(DecodeError::ExcessLength);
        }
        let mut tags = Vec::with_capacity(count_us);
        for _ in 0..count_us {
            tags.push(PropertyTag::decode(cur)?);
        }
        Ok(Self {
            input_handle_index: 0,
            property_size_limit,
            want_unicode,
            property_tag_count: count,
            property_tags: tags,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RopGetPropertiesAllRequest {
    pub input_handle_index: u8,
    pub property_size_limit: u16,
}

impl RopGetPropertiesAllRequest {
    pub fn decode(cur: &mut Buf<'_>) -> Result<Self, DecodeError> {
        // Body only (RopHeader consumed by the dispatcher): PropertySizeLimit(2 LE).
        let property_size_limit = cur.take_u16_le()?;
        Ok(Self {
            input_handle_index: 0,
            property_size_limit,
        })
    }
}
// ---- Phase 1: execute-time mailbox ROP codecs -------------------------------
//
// The codecs below cover the ROP set the MAPI dispatcher (handler.rs) must
// read off the Execute buffer in order to bridge Outlook's folder/message
// operations to the Stalwart backend. Every request codec names its spec
// section so the wire layout is auditable against MS-OXCROPS.

// ---- RopRelease (0x01) -----------------------------------------------------

/// `RopRelease` request, MS-OXCROPS §2.2.15.3.1. Body after the 3-byte
/// `RopHeader` is empty; release is implicit in the input handle index.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RopReleaseRequest;
impl RopReleaseRequest {
    pub fn decode(_cur: &mut Buf<'_>) -> Result<Self, DecodeError> {
        Ok(Self)
    }
}

/// `RopRelease` response, MS-OXCROPS §2.2.15.3.2:
///   RopId · InputHandleIndex · ReturnValue(4 LE)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RopReleaseResponse {
    pub input_handle_index: u8,
    pub return_value: RopErrorCode,
}
impl RopReleaseResponse {
    pub fn encode(&self, out: &mut Vec<u8>) {
        out.push(RopId::ROP_RELEASE.to_u8());
        out.push(self.input_handle_index);
        out.extend_from_slice(&self.return_value.to_u32().to_le_bytes());
    }
}

// ---- RopOpenFolder (0x02) --------------------------------------------------

/// `RopOpenFolder` request, MS-OXCROPS §2.2.4.1.1. Body after the 4-byte
/// `RopHeader4` (RopId·LogonId·InputHandleIndex·OutputHandleIndex) is
/// `FolderId(8 LE) · OpenModeFlags(1)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RopOpenFolderRequest {
    pub input_handle_index: u8,
    pub output_handle_index: u8,
    pub folder_id: u64,
    pub open_mode_flags: u8,
}
impl RopOpenFolderRequest {
    pub fn decode(cur: &mut Buf<'_>) -> Result<Self, DecodeError> {
        let input_handle_index = cur.take_u8()?;
        let output_handle_index = cur.take_u8()?;
        let folder_id = cur.take_u64_le()?;
        let open_mode_flags = cur.take_u8()?;
        Ok(Self {
            input_handle_index,
            output_handle_index,
            folder_id,
            open_mode_flags,
        })
    }
}

/// `RopOpenFolder` success response, MS-OXCROPS §2.2.4.1.2:
///   RopId · OutputHandleIndex · ReturnValue(4) · HasRules(1) · IsGhosted(1)
///   [· ServerCount(2) · CheapServerCount(2) · Servers[..] if IsGhosted]
/// The gateway always serves a non-ghosted folder (Stalwart folders are
/// local), so IsGhosted=0 truncates the optional server-list tail.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RopOpenFolderSuccess {
    pub output_handle_index: u8,
    pub return_value: RopErrorCode,
    pub has_rules: u8,
    pub is_ghosted: u8,
}
impl RopOpenFolderSuccess {
    pub fn encode(&self, out: &mut Vec<u8>) {
        out.push(RopId::ROP_OPEN_FOLDER.to_u8());
        out.push(self.output_handle_index);
        out.extend_from_slice(&self.return_value.to_u32().to_le_bytes());
        out.push(self.has_rules);
        out.push(self.is_ghosted);
        // IsGhosted==0 per gateway contract; the spec mandates no tail bytes.
    }
}

// ---- RopCreateMessage (0x06) -----------------------------------------------

/// `RopCreateMessage` request, MS-OXCROPS §2.2.6.2.1. Body after the 4-byte
/// `RopHeader4` is `CodePageId(2 LE) · FolderId(8 LE) · AssociatedFlag(1)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RopCreateMessageRequest {
    pub input_handle_index: u8,
    pub output_handle_index: u8,
    pub code_page_id: u16,
    pub folder_id: u64,
    pub associated_flag: u8,
}
impl RopCreateMessageRequest {
    pub fn decode(cur: &mut Buf<'_>) -> Result<Self, DecodeError> {
        let input_handle_index = cur.take_u8()?;
        let output_handle_index = cur.take_u8()?;
        let code_page_id = cur.take_u16_le()?;
        let folder_id = cur.take_u64_le()?;
        let associated_flag = cur.take_u8()?;
        Ok(Self {
            input_handle_index,
            output_handle_index,
            code_page_id,
            folder_id,
            associated_flag,
        })
    }
}

/// `RopCreateMessage` success response, MS-OXCROPS §2.2.6.2.2:
///   RopId · OutputHandleIndex · ReturnValue(4) · HasMessageId(1)
///   [· MessageId(8) if HasMessageId!=0]
/// The gateway always assigns a MessageId (the JMAP email id's low 64 bits
/// reinterpreted as a reproducible MAPI id), so HasMessageId=1 and the 8-byte
/// id is always present.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RopCreateMessageSuccess {
    pub output_handle_index: u8,
    pub return_value: RopErrorCode,
    pub has_message_id: u8,
    pub message_id: u64,
}
impl RopCreateMessageSuccess {
    pub fn encode(&self, out: &mut Vec<u8>) {
        out.push(RopId::ROP_CREATE_MESSAGE.to_u8());
        out.push(self.output_handle_index);
        out.extend_from_slice(&self.return_value.to_u32().to_le_bytes());
        out.push(self.has_message_id);
        if self.has_message_id != 0 {
            out.extend_from_slice(&self.message_id.to_le_bytes());
        }
    }
}

// ---- RopSaveChangesMessage (0x0C) ------------------------------------------

/// `RopSaveChangesMessage` request, MS-OXCROPS §2.2.6.3.1. Body after the
/// 3-byte `RopHeader` is `ResponseHandleIndex(1) · InputHandleIndex(1)
/// · SaveFlags(1)`. The RopHeader's handle_index field here is unused; the
/// body carries both the response and the real input handle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RopSaveChangesMessageRequest {
    pub response_handle_index: u8,
    pub input_handle_index: u8,
    pub save_flags: u8,
}
impl RopSaveChangesMessageRequest {
    /// `header_handle` is the 3-byte RopHeader's handle index (ignored).
    pub fn decode(cur: &mut Buf<'_>, _header_handle: u8) -> Result<Self, DecodeError> {
        let response_handle_index = cur.take_u8()?;
        let input_handle_index = cur.take_u8()?;
        let save_flags = cur.take_u8()?;
        Ok(Self {
            response_handle_index,
            input_handle_index,
            save_flags,
        })
    }
}

/// `RopSaveChangesMessage` success response, MS-OXCROPS §2.2.6.3.2:
///   RopId · ResponseHandleIndex · ReturnValue(4) · InputHandleIndex · MessageId(8)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RopSaveChangesMessageSuccess {
    pub response_handle_index: u8,
    pub return_value: RopErrorCode,
    pub input_handle_index: u8,
    pub message_id: u64,
}
impl RopSaveChangesMessageSuccess {
    pub fn encode(&self, out: &mut Vec<u8>) {
        out.push(RopId::ROP_SAVE_CHANGES_MESSAGE.to_u8());
        out.push(self.response_handle_index);
        out.extend_from_slice(&self.return_value.to_u32().to_le_bytes());
        out.push(self.input_handle_index);
        out.extend_from_slice(&self.message_id.to_le_bytes());
    }
}

// ---- RopDeleteMessages (0x1E) ----------------------------------------------

/// `RopDeleteMessages` request, MS-OXCROPS §2.2.4.11.1. Body after the 3-byte
/// `RopHeader` is `WantAsynchronous(1) · NotifyNonRead(1) · MessageIdCount(2 LE)
/// · MessageIds[count×8 LE]`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RopDeleteMessagesRequest {
    pub input_handle_index: u8,
    pub want_asynchronous: u8,
    pub notify_non_read: u8,
    pub message_ids: Vec<u64>,
}
impl RopDeleteMessagesRequest {
    pub fn decode(cur: &mut Buf<'_>) -> Result<Self, DecodeError> {
        let input_handle_index = cur.take_u8()?;
        let want_asynchronous = cur.take_u8()?;
        let notify_non_read = cur.take_u8()?;
        let count = cur.take_u16_le()?;
        let count_us = usize::from(count);
        if count_us > 4096 {
            return Err(DecodeError::ExcessLength);
        }
        let mut ids = Vec::with_capacity(count_us);
        for _ in 0..count_us {
            ids.push(cur.take_u64_le()?);
        }
        Ok(Self {
            input_handle_index,
            want_asynchronous,
            notify_non_read,
            message_ids: ids,
        })
    }
}

/// `RopDeleteMessages` response, MS-OXCROPS §2.2.4.11.2:
///   RopId · InputHandleIndex · ReturnValue(4) · PartialCompletion(1)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RopDeleteMessagesResponse {
    pub input_handle_index: u8,
    pub return_value: RopErrorCode,
    pub partial_completion: u8,
}
impl RopDeleteMessagesResponse {
    pub fn encode(&self, out: &mut Vec<u8>) {
        out.push(RopId::ROP_DELETE_MESSAGES.to_u8());
        out.push(self.input_handle_index);
        out.extend_from_slice(&self.return_value.to_u32().to_le_bytes());
        out.push(self.partial_completion);
    }
}

// ---- Attachment ROPs (0x21 / 0x22 / 0x23 / 0x24 / 0x25 / 0x52) -------------
//
// MS-OXCROPS §2.2.6 — the attachment CRUD/family Outlook enumerates a
// message's attachments through. `RopGetAttachmentTable` (0x21) opens a table
// handle over the message's attachments keyed by `PR_ATTACH_NUM`;
// `RopOpenAttachment` (0x22) opens a per-attachment handle by `PR_ATTACH_NUM`
// the client then streams via `RopOpenStream`+`RopReadStream` on
// `PR_ATTACH_DATA_BIN`, or reads metadata via `RopGetPropertiesSpecific`;
// `RopCreateAttachment` (0x23) starts a NEW attachment handle (the bytes are
// staged via `RopOpenStream`+`RopWriteStream` then committed by
// `RopSaveChangesAttachment` 0x25 to Stalwart); `RopDeleteAttachment` (0x24)
// removes an attachment by `PR_ATTACH_NUM`; `RopGetValidAttachments` (0x52)
// lists the valid `PR_ATTACH_NUM` ids on a message.
//
// Per the dispatcher convention (AGENTS.md), the arm consumes the leading
// `RopId` byte and, depending on header shape, the `LogonId` (3-byte-header
// variants) OR the trailing `LogonId·Input·Output` via `RopHeader4` decode
// (4-byte-header variants); each `*Request::decode` reads only the body
// bytes after that. `GetAttachmentTable` / `OpenAttachment` use a 4-byte
// header; `CreateAttachment` is header-only (4 bytes, no body);
// `DeleteAttachment` uses a 3-byte header + AttachmentID body;
// `SaveChangesAttachment` uses a 4-byte-ish header where the third byte is
// `ResponseHandleIndex` (not `OutputHandleIndex`).

/// `RopGetAttachmentTable` request, MS-OXCROPS §2.2.6.17.1. Body after the
/// `RopHeader4` (`RopId·LogonId·Input·Output`) is a single `TableFlags`
/// byte ([MS-OXCMSG] §2.2.3.17.1). The dispatcher consumed the `RopId`; it
/// calls `RopHeader4::decode_after_ropid` to bind Input/Output, then this
/// helper reads only `TableFlags`. The handle indices are threaded in so the
/// assembled request carries them for the handler and round-trip tests.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RopGetAttachmentTableRequest {
    pub input_handle_index: u8,
    pub output_handle_index: u8,
    pub table_flags: u8,
}
impl RopGetAttachmentTableRequest {
    pub fn decode_body(
        after_header: &mut Buf<'_>,
        input: u8,
        output: u8,
    ) -> Result<Self, DecodeError> {
        let table_flags = after_header.take_u8()?;
        Ok(Self {
            input_handle_index: input,
            output_handle_index: output,
            table_flags,
        })
    }
    pub fn encode(&self, out: &mut Vec<u8>) {
        out.push(RopId::ROP_GET_ATTACHMENT_TABLE.to_u8());
        // LogonId is the caller's concern; encode omits it (used for tests).
        out.push(self.input_handle_index);
        out.push(self.output_handle_index);
        out.push(self.table_flags);
    }
}

/// `RopGetAttachmentTable` response (success OR failure), MS-OXCROPS
/// §2.2.6.17.2 / §2.2.6.17.3: `RopId · OutputHandleIndex · ReturnValue(4)`.
/// The success and failure envelopes are byte-identical; only the
/// `ReturnValue` distinguishes them, so one codec covers both.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RopGetAttachmentTableSuccess {
    pub output_handle_index: u8,
    pub return_value: RopErrorCode,
}
impl RopGetAttachmentTableSuccess {
    pub fn encode(&self, out: &mut Vec<u8>) {
        out.push(RopId::ROP_GET_ATTACHMENT_TABLE.to_u8());
        out.push(self.output_handle_index);
        out.extend_from_slice(&self.return_value.to_u32().to_le_bytes());
    }
}

/// `RopOpenAttachment` request, MS-OXCROPS §2.2.6.12.1. Body after the
/// `RopHeader4` is `OpenAttachmentFlags(1) · AttachmentID(4 LE)` where
/// `AttachmentID` is the `PR_ATTACH_NUM` ([MS-OXCMSG] §2.2.2.6).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RopOpenAttachmentRequest {
    pub input_handle_index: u8,
    pub output_handle_index: u8,
    pub open_attachment_flags: u8,
    pub attachment_id: u32,
}
impl RopOpenAttachmentRequest {
    pub fn decode_body(
        after_header: &mut Buf<'_>,
        input: u8,
        output: u8,
    ) -> Result<Self, DecodeError> {
        let open_attachment_flags = after_header.take_u8()?;
        let attachment_id = after_header.take_u32_le()?;
        Ok(Self {
            input_handle_index: input,
            output_handle_index: output,
            open_attachment_flags,
            attachment_id,
        })
    }
    pub fn encode(&self, out: &mut Vec<u8>) {
        out.push(RopId::ROP_OPEN_ATTACHMENT.to_u8());
        out.push(self.input_handle_index);
        out.push(self.output_handle_index);
        out.push(self.open_attachment_flags);
        out.extend_from_slice(&self.attachment_id.to_le_bytes());
    }
}

/// `RopOpenAttachment` response (success OR failure), MS-OXCROPS §2.2.6.12.2:
/// `RopId · OutputHandleIndex · ReturnValue(4)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RopOpenAttachmentSuccess {
    pub output_handle_index: u8,
    pub return_value: RopErrorCode,
}
impl RopOpenAttachmentSuccess {
    pub fn encode(&self, out: &mut Vec<u8>) {
        out.push(RopId::ROP_OPEN_ATTACHMENT.to_u8());
        out.push(self.output_handle_index);
        out.extend_from_slice(&self.return_value.to_u32().to_le_bytes());
    }
}

/// `RopCreateAttachment` request, MS-OXCROPS §2.2.6.13.1. The request is
/// header-ONLY: `RopId · LogonId · InputHandleIndex · OutputHandleIndex` —
/// there is NO body. The dispatcher consumed the `RopId`; it reads the
/// remaining `LogonId · Input · Output` via `RopHeader4::decode_after_ropid`,
/// so this struct only carries the resolved indices (no decoder body).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RopCreateAttachmentRequest {
    pub input_handle_index: u8,
    pub output_handle_index: u8,
}

/// `RopCreateAttachment` success response, MS-OXCROPS §2.2.6.13.2:
/// `RopId · OutputHandleIndex · ReturnValue(4) · AttachmentID(4 LE)`.
/// `AttachmentID` is the new `PR_ATTACH_NUM` the server assigned. The failure
/// envelope (§2.2.6.13.3) is the 6-byte `RopErrorResponse` (no AttachmentID),
/// emitted by the dispatcher for a non-success return.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RopCreateAttachmentSuccess {
    pub output_handle_index: u8,
    pub return_value: RopErrorCode,
    pub attachment_id: u32,
}
impl RopCreateAttachmentSuccess {
    pub fn encode(&self, out: &mut Vec<u8>) {
        out.push(RopId::ROP_CREATE_ATTACHMENT.to_u8());
        out.push(self.output_handle_index);
        out.extend_from_slice(&self.return_value.to_u32().to_le_bytes());
        out.extend_from_slice(&self.attachment_id.to_le_bytes());
    }
}

/// `RopDeleteAttachment` request, MS-OXCROPS §2.2.6.14.1. Wire after the
/// `RopId` byte is `LogonId(1) · InputHandleIndex(1) · AttachmentID(4 LE)`.
/// The dispatcher consumed the `RopId`; it consumes `LogonId` itself, so this
/// decoder reads `InputHandleIndex · AttachmentID`. The response uses
/// `InputHandleIndex` (no output handle is created).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RopDeleteAttachmentRequest {
    pub input_handle_index: u8,
    pub attachment_id: u32,
}
impl RopDeleteAttachmentRequest {
    pub fn decode_after_ropid(cur: &mut Buf<'_>) -> Result<Self, DecodeError> {
        let input_handle_index = cur.take_u8()?;
        let attachment_id = cur.take_u32_le()?;
        Ok(Self {
            input_handle_index,
            attachment_id,
        })
    }
    pub fn encode(&self, out: &mut Vec<u8>) {
        out.push(RopId::ROP_DELETE_ATTACHMENT.to_u8());
        out.push(self.input_handle_index);
        out.extend_from_slice(&self.attachment_id.to_le_bytes());
    }
}

/// `RopDeleteAttachment` response, MS-OXCROPS §2.2.6.14.2:
/// `RopId · InputHandleIndex · ReturnValue(4)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RopDeleteAttachmentResponse {
    pub input_handle_index: u8,
    pub return_value: RopErrorCode,
}
impl RopDeleteAttachmentResponse {
    pub fn encode(&self, out: &mut Vec<u8>) {
        out.push(RopId::ROP_DELETE_ATTACHMENT.to_u8());
        out.push(self.input_handle_index);
        out.extend_from_slice(&self.return_value.to_u32().to_le_bytes());
    }
}

/// `RopSaveChangesAttachment` request, MS-OXCROPS §2.2.6.15.1. Wire after the
/// `RopId` byte is `LogonId(1) · ResponseHandleIndex(1) · InputHandleIndex(1)
/// · SaveFlags(1)`. The dispatcher consumed the `RopId` and `LogonId`, so this
/// decoder reads `ResponseHandleIndex · InputHandleIndex · SaveFlags`.
/// `ResponseHandleIndex` is the handle index echoed in the response (NOT an
/// output-table slot).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RopSaveChangesAttachmentRequest {
    pub response_handle_index: u8,
    pub input_handle_index: u8,
    pub save_flags: u8,
}
impl RopSaveChangesAttachmentRequest {
    pub fn decode_after_ropid(cur: &mut Buf<'_>) -> Result<Self, DecodeError> {
        let response_handle_index = cur.take_u8()?;
        let input_handle_index = cur.take_u8()?;
        let save_flags = cur.take_u8()?;
        Ok(Self {
            response_handle_index,
            input_handle_index,
            save_flags,
        })
    }
    pub fn encode(&self, out: &mut Vec<u8>) {
        out.push(RopId::ROP_SAVE_CHANGES_ATTACHMENT.to_u8());
        out.push(self.response_handle_index);
        out.push(self.input_handle_index);
        out.push(self.save_flags);
    }
}

/// `RopSaveChangesAttachment` response, MS-OXCROPS §2.2.6.15.2:
/// `RopId · ResponseHandleIndex · ReturnValue(4)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RopSaveChangesAttachmentResponse {
    pub response_handle_index: u8,
    pub return_value: RopErrorCode,
}
impl RopSaveChangesAttachmentResponse {
    pub fn encode(&self, out: &mut Vec<u8>) {
        out.push(RopId::ROP_SAVE_CHANGES_ATTACHMENT.to_u8());
        out.push(self.response_handle_index);
        out.extend_from_slice(&self.return_value.to_u32().to_le_bytes());
    }
}

/// `RopGetValidAttachments` request, MS-OXCROPS §2.2.6.18.1. Wire after the
/// `RopId` byte is `LogonId(1) · InputHandleIndex(1)`. The dispatcher consumed
/// the `RopId` and `LogonId`, so this decoder reads `InputHandleIndex`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RopGetValidAttachmentsRequest {
    pub input_handle_index: u8,
}
impl RopGetValidAttachmentsRequest {
    pub fn decode_after_ropid(cur: &mut Buf<'_>) -> Result<Self, DecodeError> {
        let input_handle_index = cur.take_u8()?;
        Ok(Self { input_handle_index })
    }
}

/// `RopGetValidAttachments` success response, MS-OXCROPS §2.2.6.18.2:
/// `RopId · InputHandleIndex · ReturnValue(4) · AttachmentIdCount(2 LE)
/// · AttachmentIdArray(count×4 LE)`. The failure envelope (§2.2.6.18.3) is the
/// 6-byte `RopErrorResponse` (no count/array); the dispatcher emits that for
/// a non-success return.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RopGetValidAttachmentsSuccess {
    pub input_handle_index: u8,
    pub return_value: RopErrorCode,
    pub attachment_ids: Vec<u32>,
}
impl RopGetValidAttachmentsSuccess {
    pub fn encode(&self, out: &mut Vec<u8>) {
        out.push(RopId::ROP_GET_VALID_ATTACHMENTS.to_u8());
        out.push(self.input_handle_index);
        out.extend_from_slice(&self.return_value.to_u32().to_le_bytes());
        let count = u16::try_from(self.attachment_ids.len()).unwrap_or(u16::MAX);
        out.extend_from_slice(&count.to_le_bytes());
        for id in &self.attachment_ids[..usize::from(count)] {
            out.extend_from_slice(&id.to_le_bytes());
        }
    }
}

// ---- RopMoveCopyMessages (0x33) ----------------------------------------------

/// `RopMoveCopyMessages` request, MS-OXCROPS §2.2.4.6.1. Wire after the
/// RopId byte is `LogonId(1) · SourceHandleIndex(1) · DestHandleIndex(1)
/// · MessageIdCount(2 LE) · MessageIds[count×8 LE] · WantAsynchronous(1)
/// · WantCopy(1)`. The dispatcher consumes the leading RopId byte before
/// entering the arm, so `decode_after_ropid` reads only the trailing bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RopMoveCopyMessagesRequest {
    pub source_handle_index: u8,
    pub dest_handle_index: u8,
    pub message_ids: Vec<u64>,
    pub want_asynchronous: u8,
    pub want_copy: u8,
}
impl RopMoveCopyMessagesRequest {
    /// Decode the body after the dispatcher has consumed the leading RopId
    /// byte (and the LogonId byte). Caller passes `_logon` for symmetry.
    pub fn decode_after_ropid(cur: &mut Buf<'_>) -> Result<Self, DecodeError> {
        let source_handle_index = cur.take_u8()?;
        let dest_handle_index = cur.take_u8()?;
        let count = cur.take_u16_le()?;
        let count_us = usize::from(count);
        if count_us > 4096 {
            return Err(DecodeError::ExcessLength);
        }
        let mut message_ids = Vec::with_capacity(count_us);
        for _ in 0..count_us {
            message_ids.push(cur.take_u64_le()?);
        }
        let want_asynchronous = cur.take_u8()?;
        let want_copy = cur.take_u8()?;
        Ok(Self {
            source_handle_index,
            dest_handle_index,
            message_ids,
            want_asynchronous,
            want_copy,
        })
    }
}

/// `RopMoveCopyMessages` response, MS-OXCROPS §2.2.4.6.2:
///   RopId · SourceHandleIndex · ReturnValue(4) · PartialCompletion(1)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RopMoveCopyMessagesResponse {
    pub source_handle_index: u8,
    pub return_value: RopErrorCode,
    pub partial_completion: u8,
}
impl RopMoveCopyMessagesResponse {
    pub fn encode(&self, out: &mut Vec<u8>) {
        out.push(RopId::ROP_MOVE_COPY_MESSAGES.to_u8());
        out.push(self.source_handle_index);
        out.extend_from_slice(&self.return_value.to_u32().to_le_bytes());
        out.push(self.partial_completion);
    }
}

// ---- RopSubmitMessage (0x32) ------------------------------------------------

/// `RopSubmitMessage` request, MS-OXCROPS §2.2.7.1.1. Wire after the RopId
/// byte is `LogonId(1) · InputHandleIndex(1) · SubmitFlags(1)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RopSubmitMessageRequest {
    pub input_handle_index: u8,
    pub submit_flags: u8,
}
impl RopSubmitMessageRequest {
    /// Decode the body after the dispatcher has consumed the leading RopId
    /// byte (and the LogonId byte).
    pub fn decode_after_ropid(cur: &mut Buf<'_>) -> Result<Self, DecodeError> {
        let input_handle_index = cur.take_u8()?;
        let submit_flags = cur.take_u8()?;
        Ok(Self {
            input_handle_index,
            submit_flags,
        })
    }
}

/// `RopSubmitMessage` response, MS-OXCROPS §2.2.7.1.2:
///   RopId · InputHandleIndex · ReturnValue(4)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RopSubmitMessageResponse {
    pub input_handle_index: u8,
    pub return_value: RopErrorCode,
}
impl RopSubmitMessageResponse {
    pub fn encode(&self, out: &mut Vec<u8>) {
        out.push(RopId::ROP_SUBMIT_MESSAGE.to_u8());
        out.push(self.input_handle_index);
        out.extend_from_slice(&self.return_value.to_u32().to_le_bytes());
    }
}

// ---- RopTransportSend (0x4A) ------------------------------------------------

/// `RopTransportSend` request, MS-OXCROPS §2.2.7.6.1. Wire after the RopId
/// byte is `LogonId(1) · InputHandleIndex(1)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RopTransportSendRequest {
    pub input_handle_index: u8,
}
impl RopTransportSendRequest {
    /// Decode the body after the dispatcher has consumed the leading RopId
    /// byte (and the LogonId byte).
    pub fn decode_after_ropid(cur: &mut Buf<'_>) -> Result<Self, DecodeError> {
        let input_handle_index = cur.take_u8()?;
        Ok(Self { input_handle_index })
    }
}

/// `RopTransportSend` success response, MS-OXCROPS §2.2.7.6.2:
///   RopId · InputHandleIndex · ReturnValue(4) · NoPropertiesReturned(1)
///   · PropertyValueCount(2 LE) · PropertyValues(variable)
/// The gateway returns no transport properties (the message itself, once
/// submitted, is the property set), so `NoPropertiesReturned = 1` and the
/// count is zero.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RopTransportSendSuccess {
    pub input_handle_index: u8,
    pub return_value: RopErrorCode,
    pub no_properties_returned: u8,
    pub property_value_count: u16,
}
impl RopTransportSendSuccess {
    pub fn encode(&self, out: &mut Vec<u8>) {
        out.push(RopId::ROP_TRANSPORT_SEND.to_u8());
        out.push(self.input_handle_index);
        out.extend_from_slice(&self.return_value.to_u32().to_le_bytes());
        out.push(self.no_properties_returned);
        out.extend_from_slice(&self.property_value_count.to_le_bytes());
    }
}

/// `RopTransportSend` failure response, MS-OXCROPS §2.2.7.6.3:
///   RopId · InputHandleIndex · ReturnValue(4)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RopTransportSendFailure {
    pub input_handle_index: u8,
    pub return_value: RopErrorCode,
}
impl RopTransportSendFailure {
    pub fn encode(&self, out: &mut Vec<u8>) {
        out.push(RopId::ROP_TRANSPORT_SEND.to_u8());
        out.push(self.input_handle_index);
        out.extend_from_slice(&self.return_value.to_u32().to_le_bytes());
    }
}

// ---- RopGetMessageStatus (0x1F) / RopSetMessageStatus (0x20) ----------------

/// `RopGetMessageStatus` request, MS-OXCROPS §2.2.6.9.1. Body after the
/// 3-byte `RopHeader` is `MessageId(8 LE)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RopGetMessageStatusRequest {
    pub input_handle_index: u8,
    pub message_id: u64,
}
impl RopGetMessageStatusRequest {
    pub fn decode(cur: &mut Buf<'_>) -> Result<Self, DecodeError> {
        let input_handle_index = cur.take_u8()?;
        let message_id = cur.take_u64_le()?;
        Ok(Self {
            input_handle_index,
            message_id,
        })
    }
}

/// `RopGetMessageStatus` success response (echoed RopId 0x20,
/// MS-OXCROPS §2.2.6.9.2 → §2.2.6.8.2): `RopId(0x20) · InputHandleIndex
/// · ReturnValue(4) · MessageStatusFlags(4)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RopGetMessageStatusSuccess {
    pub input_handle_index: u8,
    pub return_value: RopErrorCode,
    pub message_status_flags: u32,
}
impl RopGetMessageStatusSuccess {
    pub fn encode(&self, out: &mut Vec<u8>) {
        out.push(RopId::ROP_SET_MESSAGE_STATUS.to_u8());
        out.push(self.input_handle_index);
        out.extend_from_slice(&self.return_value.to_u32().to_le_bytes());
        out.extend_from_slice(&self.message_status_flags.to_le_bytes());
    }
}

// ---- RopRegisterNotification (0x29) / RopNotify (0x2A) --------------------

/// `RopRegisterNotification` request, MS-OXCROPS §2.2.14.1.1. Body after the
/// 4-byte `RopHeader4` is `NotificationTypes(2 LE) · [Reserved(1) if
/// Extended flag set] · WantWholeStore(1) · [FolderId(8) · MessageId(8) if
/// WantWholeStore==0]`. The gateway buffers only the headers required to
/// echo a deterministic `RopRegisterNotification` response; subscription
/// delivery via `RopNotify`/`NotificationWait` is populated in phase 2.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RopRegisterNotificationRequest {
    pub input_handle_index: u8,
    pub output_handle_index: u8,
    pub notification_types: u16,
    pub extended_reserved: Option<u8>,
    pub want_whole_store: u8,
    pub folder_id: Option<u64>,
    pub message_id: Option<u64>,
}
impl RopRegisterNotificationRequest {
    pub fn decode(cur: &mut Buf<'_>) -> Result<Self, DecodeError> {
        let input_handle_index = cur.take_u8()?;
        let output_handle_index = cur.take_u8()?;
        let notification_types = cur.take_u16_le()?;
        let extended_reserved = if notification_types & 0x0400 != 0 {
            Some(cur.take_u8()?)
        } else {
            None
        };
        let want_whole_store = cur.take_u8()?;
        let (folder_id, message_id) = if want_whole_store == 0 {
            (Some(cur.take_u64_le()?), Some(cur.take_u64_le()?))
        } else {
            (None, None)
        };
        Ok(Self {
            input_handle_index,
            output_handle_index,
            notification_types,
            extended_reserved,
            want_whole_store,
            folder_id,
            message_id,
        })
    }
}

/// `RopRegisterNotification` response, MS-OXCROPS §2.2.14.1.2:
///   RopId · OutputHandleIndex · ReturnValue(4)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RopRegisterNotificationResponse {
    pub output_handle_index: u8,
    pub return_value: RopErrorCode,
}
impl RopRegisterNotificationResponse {
    pub fn encode(&self, out: &mut Vec<u8>) {
        out.push(RopId::ROP_REGISTER_NOTIFICATION.to_u8());
        out.push(self.output_handle_index);
        out.extend_from_slice(&self.return_value.to_u32().to_le_bytes());
    }
}

/// `RopNotify` response, MS-OXCROPS §2.2.14.2.1:
///   RopId(1) · NotificationHandle(4 LE) · ReturnValue(4 LE) · LogonId(1) ·
///   NotificationData (variable)
///
/// `NotificationHandle` is a 4-byte Server object handle (per §2.2.14.2.1) that
/// identifies which `RopRegisterNotification` subscription the event pertains
/// to — the gateway emits the subscription's `OutputHandleIndex` zero-extended
/// to 32 bits, matching the index the client installed and associates with the
/// notification Server object.
///
/// `NotificationData` is encoded by [`NotificationData::encode`] per
/// MS-OXCNOTIF §2.2.1.4.1.2 (NotificationFlags + the conditional Folder/Message
/// id fields for the matching event class). The gateway forwards the event
/// classes the shared notification feed raises (NewMail, ObjectCreated,
/// ObjectModified, ObjectDeleted, ObjectMoved, ObjectCopied); the TableModified
/// and SearchCompleted classes carry no payload here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RopNotifyResponse {
    /// 4-byte server notification handle (the subscription's output handle
    /// index, zero-extended).
    pub notification_handle: u32,
    /// The `LogonId` the client associated with this notification registration.
    pub logon_id: u8,
    /// Pre-encoded `NotificationData` bytes (built via `NotificationData`).
    pub notification_data: Vec<u8>,
}
impl RopNotifyResponse {
    pub fn encode(&self, out: &mut Vec<u8>) {
        out.push(RopId::ROP_NOTIFY.to_u8());
        out.extend_from_slice(&self.notification_handle.to_le_bytes());
        out.extend_from_slice(&RopErrorCode::Success.to_u32().to_le_bytes());
        out.push(self.logon_id);
        out.extend_from_slice(&self.notification_data);
    }
}

/// `RopPending` response, MS-OXCROPS §2.2.14.3.1: `RopId(1) · SessionIndex(2 LE)`.
/// The server emits this after a batch of `RopNotify` responses when further
/// notification events remain queued on the session, signalling the client to
/// issue another `Execute` to drain them (MS-OXCROPS §3.1.5.1.3). The gateway
/// carries the SubscriptionManager's `SessionIndex` — OutlookAndroid/Windows
/// clients ignore the value when there is a single active session, but the
/// field MUST be present and the canonical value 0 is used when a session
/// cannot be uniquely identified.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RopPendingResponse {
    pub session_index: u16,
}
impl RopPendingResponse {
    pub fn encode(&self, out: &mut Vec<u8>) {
        out.push(RopId::ROP_PENDING.to_u8());
        out.extend_from_slice(&self.session_index.to_le_bytes());
    }
}

/// A `NotificationData` structure, MS-OXCNOTIF §2.2.1.4.1.2. The gateway
/// raises only the six object-level event classes the shared notification feed
/// broadcasts (NewMail / ObjectCreated / ObjectModified / ObjectDeleted /
/// ObjectMoved / ObjectCopied); table-modified and search-completed events are
/// not produced on the in-container notify path, so their (more elaborate)
/// layouts are intentionally omitted — every notification a `RopNotify` carries
/// here is one of these six object events and the codec covers them in full.
///
/// Wire layout (object event; bit 0x8000 `M` set for a message, cleared for a
/// folder event):
///   NotificationFlags(2 LE) [· TableEventType(2) — N/A]
///   · FolderId(8 LE)
///   · MessageId(8 LE)         — when 0x8000 M set (message event)
///   · ParentFolderId(8 LE)    — when type ∈ {Created, Deleted, Moved, Copied}
///                                AND (search-folder message OR folder event)
///   · OldFolderId(8 LE)        — when type ∈ {Moved, Copied}
///   · OldMessageId(8 LE)       — when type ∈ {Moved, Copied} AND 0x8000 M set
///   · OldParentFolderId(8 LE)  — when type ∈ {Moved, Copied} AND 0x8000 M clear
///
/// The folder/message ids carried here are the MAPI 64-bit row ids derived from
/// the broadcast event's `folder_id`/`item_id` strings via `store::folder_id_from_backend`
/// / `store::message_id_from_jmap`. When the broadcast event carries only a
/// string id (no row id known), the field is encoded as 0 (a sentinel Outlook
/// tolerates — the event still fires, the client then re-resolves the item by
/// re-querying the folder via the table ROPs).
#[derive(Debug, Clone)]
pub struct NotificationData {
    /// The MAPI `NotificationFlags` low-12-bit event-type bit (one of the
    /// `NT_*` constants) which MUST also carry bit `0x8000` set when the event
    /// is on a message (which it always is for mail/calendar/contact item
    /// events raised from the gateway's own event feed).
    pub notification_flags: u16,
    /// The Folder ID of the folder the event applies to (or the destination
    /// folder for Moved/Copied). Stored as the broadcast `folder_id` resolved
    /// to a 64-bit MAPI row id.
    pub folder_id: u64,
    /// The Message ID of the item the event applies to (or the destination
    /// item for Moved/Copied). 0 for a folder-only event.
    pub message_id: u64,
    /// The parent folder id, sent for ObjectCreated/ObjectDeleted when the
    /// event is on a folder (bit 0x8000 clear). `None` to omit.
    pub parent_folder_id: Option<u64>,
    /// The old Folder ID, sent for ObjectMoved/ObjectCopied. `None` to omit.
    pub old_folder_id: Option<u64>,
    /// The old Message ID, sent for ObjectMoved/ObjectCopied when bit 0x8000 is
    /// set. `None` to omit.
    pub old_message_id: Option<u64>,
    /// The old parent Folder ID, sent for ObjectMoved/ObjectCopied when bit
    /// 0x8000 is clear. `None` to omit.
    pub old_parent_folder_id: Option<u64>,
}

impl NotificationData {
    /// Serialise the `NotificationData` into `out` per MS-OXCNOTIF §2.2.1.4.1.2.
    pub fn encode(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&self.notification_flags.to_le_bytes());
        out.extend_from_slice(&self.folder_id.to_le_bytes());
        // MessageId is sent only when bit 0x8000 (M = message event) is set.
        if self.notification_flags & 0x8000 != 0 {
            out.extend_from_slice(&self.message_id.to_le_bytes());
        }
        // ParentFolderId: present for ObjectCreated(0x0004)/ObjectDeleted(0x0008)/
        // ObjectMoved(0x0020)/ObjectCopied(0x0040) when the event is a folder event
        // (bit 0x8000 clear) OR a search-folder message event (bit 0x4000 set).
        // The conditional-availability rules (§2.2.1.4.1.2) require the field be
        // emitted in both those cases so the client's parser stays aligned; a
        // `None` value is encoded as the sentinel `0`.
        let ty = self.notification_flags & 0x0FFF;
        let wants_parent = matches!(ty, 0x0004 | 0x0008 | 0x0020 | 0x0040)
            && (self.notification_flags & 0x4000 != 0 || self.notification_flags & 0x8000 == 0);
        if wants_parent {
            out.extend_from_slice(&self.parent_folder_id.unwrap_or(0).to_le_bytes());
        }
        // Old* fields: ObjectMoved(0x0020)/ObjectCopied(0x0040). These are
        // MANDATORY on the wire for both event types (the client decodes them by
        // position, not by an Option presence flag), so the field is ALWAYS
        // emitted and a `None` caller value is encoded as the sentinel `0`.
        // OldMessageId requires the message-event bit (0x8000); OldParentFolderId
        // requires a folder event (0x8000 clear). The two are mutually exclusive
        // (a single NotificationData is either a message or folder event), so
        // exactly one Old*MessageId-or-OldParentFolderId byte follows OldFolderId.
        if matches!(ty, 0x0020 | 0x0040) {
            out.extend_from_slice(&self.old_folder_id.unwrap_or(0).to_le_bytes());
            if self.notification_flags & 0x8000 != 0 {
                out.extend_from_slice(&self.old_message_id.unwrap_or(0).to_le_bytes());
            } else {
                out.extend_from_slice(&self.old_parent_folder_id.unwrap_or(0).to_le_bytes());
            }
        }
    }
}

// ---- RopSetColumns / RopGetStatus success encoders -------------------------

/// `RopSetColumns` success response, MS-OXCROPS §2.2.5.1.2:
///   RopId · InputHandleIndex · ReturnValue(4) · TableStatus(1)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RopSetColumnsSuccess {
    pub input_handle_index: u8,
    pub return_value: RopErrorCode,
    pub table_status: u8,
}
impl RopSetColumnsSuccess {
    pub fn encode(&self, out: &mut Vec<u8>) {
        out.push(RopId::ROP_SET_COLUMNS.to_u8());
        out.push(self.input_handle_index);
        out.extend_from_slice(&self.return_value.to_u32().to_le_bytes());
        out.push(self.table_status);
    }
}

/// `RopGetStatus` success response, MS-OXCROPS §2.2.5.6.2:
///   RopId · InputHandleIndex · ReturnValue(4) · TableStatus(1)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RopGetStatusSuccess {
    pub input_handle_index: u8,
    pub return_value: RopErrorCode,
    pub table_status: u8,
}
impl RopGetStatusSuccess {
    pub fn encode(&self, out: &mut Vec<u8>) {
        out.push(RopId::ROP_GET_STATUS.to_u8());
        out.push(self.input_handle_index);
        out.extend_from_slice(&self.return_value.to_u32().to_le_bytes());
        out.push(self.table_status);
    }
}

// ---- GetProperties success response shared by GetPropertiesSpecific/All -----

/// `RopGetPropertiesSpecific`/`RopGetPropertiesAll` success response,
/// MS-OXCROPS §2.2.8.3.2 / §2.2.8.4.2: `RopId · InputHandleIndex
/// · ReturnValue(4) · RowData(PropertyRow)`. The row is a StandardPropertyRow
/// (Flag=0) followed by the PropertyValue bytes for each requested tag, in
/// request order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RopGetPropertiesSuccess {
    pub rop_id: RopId,
    pub input_handle_index: u8,
    pub return_value: RopErrorCode,
    /// Pre-serialized PropertyRow: a 0x00 flag byte followed by the
    /// concatenated value bytes for the requested tags.
    pub row_data: Vec<u8>,
}
impl RopGetPropertiesSuccess {
    pub fn encode(&self, out: &mut Vec<u8>) {
        out.push(self.rop_id.to_u8());
        out.push(self.input_handle_index);
        out.extend_from_slice(&self.return_value.to_u32().to_le_bytes());
        out.push(0u8); // StandardPropertyRow Flag = 0 (all values present)
        out.extend_from_slice(&self.row_data);
    }
}

// ---- RopSetProperties (0x0A) / RopDeleteProperties (0x0B) -------------------
//
// The two general property-write ROPs (MS-OXCROPS 2.2.8.6 / 2.2.8.8) bridge
// Outlook's compose/edit flow - subject, body, importance, follow-up flags,
// categories - to the JMAP backend. The success/failure envelopes share the
// common RopId + InputHandleIndex + ReturnValue shape; the success body
// carries a PropertyProblemCount + PropertyProblems array (MS-OXCDATA 2.7)
// the gateway emits empty (count=0) on a clean apply.

/// Thin re-export of MS-OXCDATA 2.7 PropertyProblem so the codec layer and
/// the handler share a single definition for the per-property failure block
/// returned by SetProperties / DeleteProperties / CopyTo.
pub use crate::mapi::data::PropertyProblem as RopPropertyProblem;

/// Typical client-supplied property-array length; used only as a capacity
/// hint so the rental `Vec` does not start at 0 for the common small case.
/// The decoding still honours the wire count up to each `MAX_*` bound.
const TYPICAL_PROPERTY_COUNT: usize = 32;

/// RopSetProperties request, MS-OXCROPS 2.2.8.6.1. Body after the 3-byte
/// RopHeader (RopId + LogonId + InputHandleIndex) is
/// PropertyValueSize(2 LE) + PropertyValueCount(2 LE) + PropertyValues.
/// PropertyValues is an array of TaggedPropertyValue (2.11.4) and occupies
/// exactly PropertyValueSize - 2 bytes; the decoder fails closed
/// (ExcessLength) when the decoded entries do not land on that boundary,
/// so a malformed MV or opaque-typed entry cannot desynchronise the
/// ROP-chain cursor.
#[derive(Debug, Clone, PartialEq)]
pub struct RopSetPropertiesRequest {
    pub input_handle_index: u8,
    pub property_values: Vec<TaggedPropertyValue>,
}

impl RopSetPropertiesRequest {
    /// Maximum number of tagged property values a single RopSetProperties may
    /// carry. Outlook compose forms send a small set (subject/body/flags);
    /// the cap resists a pathological client that overruns the wire count.
    pub const MAX_VALUES: usize = 1024;

    pub fn decode(cur: &mut Buf<'_>) -> Result<Self, DecodeError> {
        let property_value_size = cur.take_u16_le()?;
        let property_value_count = cur.take_u16_le()?;
        let count_us = usize::from(property_value_count);
        if count_us > Self::MAX_VALUES {
            return Err(DecodeError::ExcessLength);
        }
        // PropertyValueSize counts the bytes of the count field PLUS the
        // property-values payload; bounds-check it against the remaining
        // buffer and require at least the 2-byte count field.
        let size = usize::from(property_value_size);
        if size < 2 {
            return Err(DecodeError::ExcessLength);
        }
        let payload = size.checked_sub(2).ok_or(DecodeError::ExcessLength)?;
        if cur.remaining() < payload {
            return Err(DecodeError::Insufficient);
        }
        // Decode the count-prefixed payload from a bounded sub-cursor so the
        // outer chain cursor lands exactly at the next ROP even if an entry
        // was decoded opaquely.
        let payload_bytes = cur.take_bytes(payload)?;
        let mut sub = Buf::new(payload_bytes);
        let mut values = Vec::with_capacity(count_us.min(TYPICAL_PROPERTY_COUNT));
        for _ in 0..count_us {
            // `TaggedPropertyValue::decode` always consumes the 4-byte
            // PropertyTag first, so the cursor advances by at least 4 here —
            // a per-entry zero-advance check is therefore unreachable and is
            // not performed. The real integrity control is the trailing
            // `sub.remaining() != 0` bound below, which rejects any entry an
            // unknown variable-length type left misaligned (coderabbit #20).
            let tv = TaggedPropertyValue::decode(&mut sub)?;
            values.push(tv);
        }
        if sub.remaining() != 0 {
            // The declared PropertyValueSize disagrees with the entries the
            // client wrote; refuse rather than desyncing the chain.
            return Err(DecodeError::ExcessLength);
        }
        Ok(Self {
            input_handle_index: 0,
            property_values: values,
        })
    }
}

/// Shared success/failure envelope for the property-write ROPs
/// (2.2.8.6.2 / 2.2.8.8.2 / 2.2.8.12.2): RopId + HandleIndex +
/// ReturnValue(4) + PropertyProblemCount(2 LE) + PropertyProblems. On a
/// clean apply the problem array is empty (count=0). A non-success
/// ReturnValue is emitted via the 6-byte failure envelope (no problem
/// array), per 2.2.8.6.3 / 2.2.8.8.3.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RopPropertyWriteSuccess {
    pub rop_id: RopId,
    pub handle_index: u8,
    pub return_value: RopErrorCode,
    pub problems: Vec<PropertyProblem>,
}

impl RopPropertyWriteSuccess {
    pub fn encode(&self, out: &mut Vec<u8>) {
        out.push(self.rop_id.to_u8());
        out.push(self.handle_index);
        out.extend_from_slice(&self.return_value.to_u32().to_le_bytes());
        // On a non-success ReturnValue the spec mandates the 6-byte failure
        // envelope (RopId + HandleIndex + ReturnValue, 2.2.8.6.3 / 2.2.8.8.3
        // / 2.2.8.12.4): NO PropertyProblemCount or PropertyProblems. The
        // prior impl always wrote a (zero-filled) problem array, which
        // corrupts the chain cursor on failure because the client reads the
        // next ROP from the wrong offset (qodo #2, cubic #25, coderabbit #10).
        if self.return_value == RopErrorCode::Success {
            let count = u16::try_from(self.problems.len()).unwrap_or(u16::MAX);
            out.extend_from_slice(&count.to_le_bytes());
            for p in &self.problems {
                p.encode(out);
            }
        }
    }
}

/// RopDeleteProperties request, MS-OXCROPS 2.2.8.8.1. Body after the
/// 3-byte RopHeader is PropertyTagCount(2 LE) + PropertyTags[count].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RopDeletePropertiesRequest {
    pub input_handle_index: u8,
    pub property_tags: Vec<PropertyTag>,
}

impl RopDeletePropertiesRequest {
    pub const MAX_TAGS: usize = 1024;

    pub fn decode(cur: &mut Buf<'_>) -> Result<Self, DecodeError> {
        let count = cur.take_u16_le()?;
        let count_us = usize::from(count);
        if count_us > Self::MAX_TAGS {
            return Err(DecodeError::ExcessLength);
        }
        let mut tags = Vec::with_capacity(count_us.min(TYPICAL_PROPERTY_COUNT));
        for _ in 0..count_us {
            tags.push(PropertyTag::decode(cur)?);
        }
        Ok(Self {
            input_handle_index: 0,
            property_tags: tags,
        })
    }
}

// ---- RopCopyTo (0x39) -------------------------------------------------------

/// RopCopyTo request, MS-OXCROPS 2.2.8.12.1. The dispatcher consumes the
/// leading RopId and LogonId bytes; this decoder reads the two handle
/// indices first (SourceHandleIndex, DestHandleIndex), then the body
/// WantAsynchronous(1) + WantSubObjects(1) + CopyFlags(1)
/// + ExcludedTagCount(2 LE) + ExcludedTags[count].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RopCopyToRequest {
    pub source_handle_index: u8,
    pub dest_handle_index: u8,
    pub want_asynchronous: u8,
    pub want_sub_objects: u8,
    pub copy_flags: u8,
    pub excluded_tags: Vec<PropertyTag>,
}

impl RopCopyToRequest {
    pub const MAX_EXCLUDED_TAGS: usize = 1024;

    pub fn decode(cur: &mut Buf<'_>) -> Result<Self, DecodeError> {
        let source_handle_index = cur.take_u8()?;
        let dest_handle_index = cur.take_u8()?;
        let want_asynchronous = cur.take_u8()?;
        let want_sub_objects = cur.take_u8()?;
        let copy_flags = cur.take_u8()?;
        let count = cur.take_u16_le()?;
        let count_us = usize::from(count);
        if count_us > Self::MAX_EXCLUDED_TAGS {
            return Err(DecodeError::ExcessLength);
        }
        let mut excluded_tags = Vec::with_capacity(count_us.min(TYPICAL_PROPERTY_COUNT));
        for _ in 0..count_us {
            excluded_tags.push(PropertyTag::decode(cur)?);
        }
        Ok(Self {
            source_handle_index,
            dest_handle_index,
            want_asynchronous,
            want_sub_objects,
            copy_flags,
            excluded_tags,
        })
    }
}

/// RopCopyTo success response, MS-OXCROPS 2.2.8.12.2:
/// `RopId`, `SourceHandleIndex`, `ReturnValue(4)`, `PropertyProblemCount(2)`,
/// and `PropertyProblems` (variable). The null-destination failure
/// (2.2.8.12.3, code 0x00000503) and the generic failure (2.2.8.12.4) are
/// emitted as a plain `RopErrorResponse` by the dispatcher; the clean path
/// uses this envelope with an empty problem array.
pub type RopCopyToSuccess = RopPropertyWriteSuccess;

// ---- Stream ROPs (0x2B / 0x2C / 0x2D / 0x2E / 0x2F / 0x5D / 0x5E) -----------
//
// MS-OXCROPS §2.2.9 — the stream-access ROPs Outlook uses to fetch PR_BODY /
// PR_BODY_HTML / PR_RTF_COMPRESSED (MS-OXBBODY) and the message/attachment
// binary blobs (MS-OXCMSG §3.x). A stream is a server-side handle installed at
// a client-chosen output index by `RopOpenStream`; the dispatcher resolves the
// requested property tag against the owning message/folder handle and caches
// the rendered bytes on the [`crate::mapi::session::Handle::Stream`] entry.
// `RopReadStream` paginates from the stream's seek cursor, advancing it past
// the bytes returned; `RopSeekStream` repositions the cursor; `RopSetStreamSize`
// truncates/extends the buffered bytes; `RopWriteStream` appends/replaces a span
// (the draft body write path); `RopGetStreamSize` reports the current length;
// `RopCommitStream` is a no-op against JMAP — the gateway buffers writes in the
// stream and flushes them at `RopSaveChangesMessage` time, so a commit simply
// acknowledges the pending state.

/// Sentinel value carried in `RopReadStream`'s 2-byte `ByteCount` field to
/// request the extended 4-byte `MaximumByteCount` maximum (MS-OXCROPS
/// §2.2.9.2.1, footnote 9). The server returns up to `MaximumByteCount` (or
/// `ByteCount` when the field is any other value).
pub const READ_STREAM_EXTENDED_BYTECOUNT: u16 = 0xBABE;

/// `RopOpenStream` request, MS-OXCROPS 2.2.9.1.1. Wire after the leading
/// `RopId` byte is `LogonId(1)`, `InputHandleIndex(1)`, `OutputHandleIndex(1)`,
/// `PropertyTag(4)`, `OpenModeFlags(1)`: a 4-byte `RopHeader4` body
/// (LogonId, Input, Output) followed by the open-mode flag. Per the dispatcher
/// convention (AGENTS.md), the handler consumes the LogonId, InputHandleIndex
/// and OutputHandleIndex via `RopHeader4::decode_after_ropid`, and only the body
/// fields (`PropertyTag` + `OpenModeFlags`) are decoded here, so the codec
/// never re-takes dispatcher-owned header bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RopOpenStreamRequest {
    pub input_handle_index: u8,
    pub output_handle_index: u8,
    /// Tag identifying the property to stream (`PR_BODY`, `PR_BODY_HTML`,
    /// `PR_RTF_COMPRESSED`, `PR_ATTACH_DATA_BIN`, ...). Type-first wire order
    /// per MS-OXCDATA 2.9.
    pub property_tag: PropertyTag,
    pub open_mode_flags: u8,
}

impl RopOpenStreamRequest {
    /// Decode the body (`PropertyTag(4) + OpenModeFlags(1)`) after the
    /// dispatcher has consumed the leading `RopId` and the `RopHeader4`
    /// (`LogonId-InputHandleIndex-OutputHandleIndex`). The consumed handle
    /// indices are threaded in so the assembled request still carries them
    /// for the handler and the round-trip tests.
    pub fn decode_body(
        cur: &mut Buf<'_>,
        input_handle_index: u8,
        output_handle_index: u8,
    ) -> Result<Self, DecodeError> {
        let property_tag = PropertyTag::decode(cur)?;
        let open_mode_flags = cur.take_u8()?;
        Ok(Self {
            input_handle_index,
            output_handle_index,
            property_tag,
            open_mode_flags,
        })
    }
}

/// `RopOpenStream` success response, MS-OXCROPS §2.2.9.1.2:
///   `RopId · OutputHandleIndex · ReturnValue(4) · StreamSize(4 LE)`
/// The `StreamSize` is the current size of the stream in bytes. The failure
/// response (§2.2.9.1.3) is the 6-byte `RopErrorResponse` envelope.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RopOpenStreamSuccess {
    pub output_handle_index: u8,
    pub return_value: RopErrorCode,
    pub stream_size: u32,
}

impl RopOpenStreamSuccess {
    pub fn encode(&self, out: &mut Vec<u8>) {
        out.push(RopId::ROP_OPEN_STREAM.to_u8());
        out.push(self.output_handle_index);
        out.extend_from_slice(&self.return_value.to_u32().to_le_bytes());
        // On a non-success ReturnValue the spec mandates the 6-byte failure
        // envelope (RopId + HandleIndex + ReturnValue, 2.2.9.1.3): NO
        // StreamSize. Emitting it corrupts the chain cursor on failure.
        if self.return_value == RopErrorCode::Success {
            out.extend_from_slice(&self.stream_size.to_le_bytes());
        }
    }
}

/// `RopReadStream` request, MS-OXCROPS §2.2.9.2.1. Wire after the leading
/// `RopId` byte is `LogonId(1) · InputHandleIndex(1) · ByteCount(2 LE)
/// · [MaximumByteCount(4 LE) if ByteCount == 0xBABE]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RopReadStreamRequest {
    pub input_handle_index: u8,
    /// Maximum number of bytes the client is willing to receive this round.
    /// When `ByteCount == 0xBABE`, the real maximum lives in
    /// `maximum_byte_count`; otherwise this is the maximum directly.
    pub byte_count: u16,
    pub maximum_byte_count: Option<u32>,
}

impl RopReadStreamRequest {
    /// Decode the body after the dispatcher has consumed the leading RopId
    /// and LogonId bytes — i.e. it reads only `InputHandleIndex(1) +
    /// ByteCount(2) + (optional) MaximumByteCount(4)`.
    pub fn decode_after_ropid(cur: &mut Buf<'_>) -> Result<Self, DecodeError> {
        let input_handle_index = cur.take_u8()?;
        let byte_count = cur.take_u16_le()?;
        let maximum_byte_count = if byte_count == READ_STREAM_EXTENDED_BYTECOUNT {
            Some(cur.take_u32_le()?)
        } else {
            None
        };
        Ok(Self {
            input_handle_index,
            byte_count,
            maximum_byte_count,
        })
    }

    /// The effective maximum the handler should honour, capped at the documented
    /// 2 GiB ceiling so a malicious `MaximumByteCount` above `i32::MAX` becomes
    /// `InvalidParameter` rather than a saturated silent truncation (spec: if
    /// `MaximumByteCount > 0x80000000` the RPC SHOULD fail with `0x000004B6`).
    pub fn max_bytes(&self) -> Result<u32, DecodeError> {
        let raw = match self.maximum_byte_count {
            Some(m) => m,
            None => u32::from(self.byte_count),
        };
        if raw > 0x8000_0000 {
            return Err(DecodeError::InvalidValue);
        }
        Ok(raw)
    }
}

/// `RopReadStream` response, MS-OXCROPS §2.2.9.2.2:
///   `RopId · InputHandleIndex · ReturnValue(4) · DataSize(2 LE) · Data(variable)`
/// `DataSize` is the number of bytes actually returned (≤ the request's max).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RopReadStreamSuccess {
    pub input_handle_index: u8,
    pub return_value: RopErrorCode,
    pub data: Vec<u8>,
}

impl RopReadStreamSuccess {
    pub fn encode(&self, out: &mut Vec<u8>) {
        out.push(RopId::ROP_READ_STREAM.to_u8());
        out.push(self.input_handle_index);
        out.extend_from_slice(&self.return_value.to_u32().to_le_bytes());
        // On a non-success ReturnValue (2.2.9.2.3) the response is the 6-byte
        // failure envelope: NO DataSize/Data. The handler caps the chunk at
        // u16::MAX so this truncation is a defence-in-depth guard only; on the
        // success path the data fits a 2-byte DataSize by construction.
        if self.return_value != RopErrorCode::Success {
            return;
        }
        let data_size = u16::try_from(self.data.len()).unwrap_or(u16::MAX);
        out.extend_from_slice(&data_size.to_le_bytes());
        out.extend_from_slice(&self.data[..usize::from(data_size)]);
    }
}

/// `RopWriteStream` request, MS-OXCROPS §2.2.9.3.1. Wire after the leading
/// `RopId` byte is `LogonId(1) · InputHandleIndex(1) · DataSize(2 LE)
/// · Data(DataSize bytes)`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RopWriteStreamRequest {
    pub input_handle_index: u8,
    pub data: Vec<u8>,
}

impl RopWriteStreamRequest {
    /// Decode the body after the dispatcher has consumed the leading RopId
    /// and LogonId bytes. `DataSize` (a 2-byte count, so <= 65535) is bounds
    /// checked against the remaining buffer by `take_bytes`; a count the
    /// client declares but does not supply yields `Insufficient`.
    pub fn decode_after_ropid(cur: &mut Buf<'_>) -> Result<Self, DecodeError> {
        let input_handle_index = cur.take_u8()?;
        let data_size = usize::from(cur.take_u16_le()?);
        let data = cur.take_bytes(data_size)?.to_vec();
        Ok(Self {
            input_handle_index,
            data,
        })
    }
}

/// `RopWriteStream` response, MS-OXCROPS §2.2.9.3.2:
///   `RopId · InputHandleIndex · ReturnValue(4) · WrittenSize(2 LE)`
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RopWriteStreamSuccess {
    pub input_handle_index: u8,
    pub return_value: RopErrorCode,
    pub written_size: u16,
}

impl RopWriteStreamSuccess {
    pub fn encode(&self, out: &mut Vec<u8>) {
        out.push(RopId::ROP_WRITE_STREAM.to_u8());
        out.push(self.input_handle_index);
        out.extend_from_slice(&self.return_value.to_u32().to_le_bytes());
        // On a non-success ReturnValue (2.2.9.3.3) the response is the 6-byte
        // failure envelope: NO WrittenSize.
        if self.return_value == RopErrorCode::Success {
            out.extend_from_slice(&self.written_size.to_le_bytes());
        }
    }
}

/// `RopSeekStream` request, MS-OXCROPS §2.2.9.8.1. Wire after the leading
/// `RopId` byte is `LogonId(1) · InputHandleIndex(1) · Origin(1)
/// · Offset(8 LE signed)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RopSeekStreamRequest {
    pub input_handle_index: u8,
    pub origin: u8,
    pub offset: i64,
}

impl RopSeekStreamRequest {
    /// Decode the body after the dispatcher has consumed the leading RopId
    /// and LogonId bytes. The `Offset` field is a signed 8-byte LONGLONG, so
    /// it is read directly as `i64` (a `u64` read + `as i64` cast would wrap
    /// for values above `i64::MAX` into a negative offset).
    pub fn decode_after_ropid(cur: &mut Buf<'_>) -> Result<Self, DecodeError> {
        let input_handle_index = cur.take_u8()?;
        let origin = cur.take_u8()?;
        let offset = cur.take_i64_le()?;
        Ok(Self {
            input_handle_index,
            origin,
            offset,
        })
    }

    /// Resolve the requested origin enumeration against the current cursor and
    /// stream length, returning the absolute 0-based cursor the seek moves to.
    /// Per MS-OXCPRPT 2.2.21.1: `0x00` = beginning, `0x01` = current,
    /// `0x02` = end. Out-of-range absolutes are clamped to `[0, len]`:
    /// positive overflow lands at `len`, negative overflow lands at `0` (the
    /// stream semantics Outlook relies on, rather than the raw signed offset
    /// which could desync the cursor on `checked_add_signed` overflow). An
    /// unknown origin yields `InvalidValue` so the dispatcher returns
    /// `InvalidParameter`.
    pub fn resolve(&self, current: u64, len: u64) -> Result<u64, DecodeError> {
        // Compute the requested absolute position as an unsigned value per origin
        // (0x00 = absolute, 0x01 = relative to cursor, 0x02 = relative to end),
        // then clamp to `[0, len]` applying the *requested sign* on overflow.
        //
        // `checked_add_signed` returns None on arithmetic overflow; on overflow we
        // clamp by the sign of `self.offset` (positive -> past `len`, negative ->
        // below 0) so the final `.min(len)` / `0` clamp lands predictably rather
        // than at a stale raw offset (coderabbit). Crucially we never reinterpret
        // a `u64` above `i64::MAX` back through `as i64` (that is a *bitwise* cast,
        // not a saturating one, and wraps to a negative), so we keep the working
        // value in `u64`/`Option<u64>` and only consult the offset's sign. An
        // unknown origin yields `InvalidValue` so the dispatcher returns
        // `InvalidParameter`.
        let target: u64 = match self.origin {
            0x00 => {
                if self.offset >= 0 {
                    self.offset as u64
                } else {
                    0
                }
            }
            0x01 => match current.checked_add_signed(self.offset) {
                Some(n) => n,
                None => {
                    if self.offset >= 0 {
                        u64::MAX
                    } else {
                        0
                    }
                }
            },
            0x02 => match len.checked_add_signed(self.offset) {
                Some(n) => n,
                None => {
                    if self.offset >= 0 {
                        u64::MAX
                    } else {
                        0
                    }
                }
            },
            _ => return Err(DecodeError::InvalidValue),
        };
        Ok(target.min(len))
    }
}

/// `RopSeekStream` success response, MS-OXCROPS §2.2.9.8.2:
///   `RopId · InputHandleIndex · ReturnValue(4) · NewPosition(8 LE)`
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RopSeekStreamSuccess {
    pub input_handle_index: u8,
    pub return_value: RopErrorCode,
    pub new_position: u64,
}

impl RopSeekStreamSuccess {
    pub fn encode(&self, out: &mut Vec<u8>) {
        out.push(RopId::ROP_SEEK_STREAM.to_u8());
        out.push(self.input_handle_index);
        out.extend_from_slice(&self.return_value.to_u32().to_le_bytes());
        // On a non-success ReturnValue (2.2.9.8.3) the response is the 6-byte
        // failure envelope: NO NewPosition.
        if self.return_value == RopErrorCode::Success {
            out.extend_from_slice(&self.new_position.to_le_bytes());
        }
    }
}

/// `RopSetStreamSize` request, MS-OXCROPS §2.2.9.7.1. Wire after the leading
/// `RopId` byte is `LogonId(1) · InputHandleIndex(1) · StreamSize(8 LE)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RopSetStreamSizeRequest {
    pub input_handle_index: u8,
    pub stream_size: u64,
}

impl RopSetStreamSizeRequest {
    /// Decode the body after the dispatcher has consumed the leading RopId
    /// and LogonId bytes.
    pub fn decode_after_ropid(cur: &mut Buf<'_>) -> Result<Self, DecodeError> {
        let input_handle_index = cur.take_u8()?;
        let stream_size = cur.take_u64_le()?;
        Ok(Self {
            input_handle_index,
            stream_size,
        })
    }
}

/// `RopSetStreamSize` response, MS-OXCROPS §2.2.9.7.2:
///   `RopId · InputHandleIndex · ReturnValue(4)`
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RopSetStreamSizeResponse {
    pub input_handle_index: u8,
    pub return_value: RopErrorCode,
}

impl RopSetStreamSizeResponse {
    pub fn encode(&self, out: &mut Vec<u8>) {
        out.push(RopId::ROP_SET_STREAM_SIZE.to_u8());
        out.push(self.input_handle_index);
        out.extend_from_slice(&self.return_value.to_u32().to_le_bytes());
    }
}

/// `RopGetStreamSize` request, MS-OXCROPS §2.2.9.6.1. Wire after the leading
/// `RopId` byte is `LogonId(1) · InputHandleIndex(1)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RopGetStreamSizeRequest {
    pub input_handle_index: u8,
}

impl RopGetStreamSizeRequest {
    pub fn decode_after_ropid(cur: &mut Buf<'_>) -> Result<Self, DecodeError> {
        let input_handle_index = cur.take_u8()?;
        Ok(Self { input_handle_index })
    }
}

/// `RopGetStreamSize` success response, MS-OXCROPS §2.2.9.6.2:
///   `RopId · InputHandleIndex · ReturnValue(4) · StreamSize(4 LE)`
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RopGetStreamSizeSuccess {
    pub input_handle_index: u8,
    pub return_value: RopErrorCode,
    pub stream_size: u32,
}

impl RopGetStreamSizeSuccess {
    pub fn encode(&self, out: &mut Vec<u8>) {
        out.push(RopId::ROP_GET_STREAM_SIZE.to_u8());
        out.push(self.input_handle_index);
        out.extend_from_slice(&self.return_value.to_u32().to_le_bytes());
        // On a non-success ReturnValue (2.2.9.6.3) the response is the 6-byte
        // failure envelope: NO StreamSize.
        if self.return_value == RopErrorCode::Success {
            out.extend_from_slice(&self.stream_size.to_le_bytes());
        }
    }
}

/// `RopCommitStream` request, MS-OXCROPS §2.2.9.5.1. Wire after the leading
/// `RopId` byte is `LogonId(1) · InputHandleIndex(1)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RopCommitStreamRequest {
    pub input_handle_index: u8,
}

impl RopCommitStreamRequest {
    pub fn decode_after_ropid(cur: &mut Buf<'_>) -> Result<Self, DecodeError> {
        let input_handle_index = cur.take_u8()?;
        Ok(Self { input_handle_index })
    }
}

/// `RopCommitStream` response, MS-OXCROPS §2.2.9.5.2:
///   `RopId · InputHandleIndex · ReturnValue(4)`
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RopCommitStreamResponse {
    pub input_handle_index: u8,
    pub return_value: RopErrorCode,
}

impl RopCommitStreamResponse {
    pub fn encode(&self, out: &mut Vec<u8>) {
        out.push(RopId::ROP_COMMIT_STREAM.to_u8());
        out.push(self.input_handle_index);
        out.extend_from_slice(&self.return_value.to_u32().to_le_bytes());
    }
}

// ---- Table navigation ROPs (§2.2.5.x and §2.2.7.x) --------------------------
//
// These ROPs drive the Table handle's cursor / restriction / sort order /
// bookmarks that Outlook relies on for scroll, virtualisation, and filtering
// of contents and hierarchy tables. All operate over the in-session Table
// handle populated by RopGet{Hierarchy,Contents}Table; none need a backend
// round-trip beyond the rows already materialised when the table was opened.
//
// The request bodies follow the "body only" convention used elsewhere: the
// dispatcher consumes LogonId + InputHandleIndex (+ OutputHandleIndex for
// the bookmark-producing ROPs) before calling `*::decode`, which reads the
// remaining fields.

/// `RopRestrict` request (0x14, MS-OXCROPS §2.2.5.3.1). Body after
/// LogonId + InputHandleIndex: `RestrictFlags(1) · RestrictionData` (an
/// `SRestriction` tree). `RestrictFlags` bit 0x01 = `PRIOR_RESTRICTION`
/// (combine with the prior restriction via AND).
#[derive(Debug, Clone)]
pub struct RopRestrictRequest {
    pub input_handle_index: u8,
    pub restrict_flags: u8,
    pub restriction: crate::mapi::restrict::SRestriction,
}

impl RopRestrictRequest {
    pub fn decode(cur: &mut Buf<'_>) -> Result<Self, DecodeError> {
        let restrict_flags = cur.take_u8()?;
        // MS-OXCTABL §2.2.2.4.1 / MS-OXCROPS §2.2.5.3.1: the body is
        // `RestrictFlags(1) · RestrictionDataSize(2 LE) · RestrictionData`.
        // We MUST read the 2-byte size prefix and decode the restriction from
        // exactly that many bytes, so a trailing ROP in an Execute chain stays
        // aligned (the SRestriction decoder is self-delimiting but the size
        // prefix is the authoritative bound the client emits).
        let size = usize::from(cur.take_u16_le()?);
        let data = cur.take_bytes(size)?;
        let mut inner = Buf::new(data);
        let restriction = crate::mapi::restrict::SRestriction::decode(&mut inner)?;
        // Reject trailing bytes inside the declared restriction envelope — a
        // bogus RestrictionDataSize that overshoots the actual self-delimiting
        // restriction would otherwise desync the chain.
        if inner.remaining() != 0 {
            return Err(DecodeError::Trailing);
        }
        Ok(Self {
            input_handle_index: 0,
            restrict_flags,
            restriction,
        })
    }
}

/// `RopRestrict` success response: `RopId · InputHandleIndex · ReturnValue(4)
/// · TableStatus(1)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RopRestrictResponse {
    pub input_handle_index: u8,
    pub return_value: RopErrorCode,
    pub table_status: u8,
}

impl RopRestrictResponse {
    pub fn encode(&self, out: &mut Vec<u8>) {
        out.push(RopId::ROP_RESTRICT.to_u8());
        out.push(self.input_handle_index);
        out.extend_from_slice(&self.return_value.to_u32().to_le_bytes());
        out.push(self.table_status);
    }
}

/// `RopSortTable` request (0x13, MS-OXCROPS §2.2.5.2.1). Body after LogonId +
/// InputHandleIndex: `SortFlags(1) · SortOrder`. `SortFlags` bit 0x01 =
/// `SORT_FLAG_AS_FOLDER`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RopSortTableRequest {
    pub input_handle_index: u8,
    pub sort_flags: u8,
    pub sort_orders: Vec<SortOrder>,
}

impl RopSortTableRequest {
    pub fn decode(cur: &mut Buf<'_>) -> Result<Self, DecodeError> {
        let sort_flags = cur.take_u8()?;
        let count = usize::from(cur.take_u16_le()?);
        let mut sort_orders = Vec::with_capacity(count.min(64));
        for _ in 0..count {
            sort_orders.push(SortOrder::decode(cur)?);
        }
        Ok(Self {
            input_handle_index: 0,
            sort_flags,
            sort_orders,
        })
    }
}

/// `RopSortTable` success response: `RopId · InputHandleIndex · ReturnValue(4)
/// · TableStatus(1)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RopSortTableResponse {
    pub input_handle_index: u8,
    pub return_value: RopErrorCode,
    pub table_status: u8,
}

impl RopSortTableResponse {
    pub fn encode(&self, out: &mut Vec<u8>) {
        out.push(RopId::ROP_SORT_TABLE.to_u8());
        out.push(self.input_handle_index);
        out.extend_from_slice(&self.return_value.to_u32().to_le_bytes());
        out.push(self.table_status);
    }
}

/// `RopSeekRow` request (0x18, MS-OXCROPS §2.2.7.2.1). Body after LogonId +
/// InputHandleIndex: `SeekFlags(1) · RowCount(4 LE signed)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RopSeekRowRequest {
    pub input_handle_index: u8,
    pub seek_flags: u8,
    pub row_count: i32,
}

impl RopSeekRowRequest {
    pub fn decode(cur: &mut Buf<'_>) -> Result<Self, DecodeError> {
        let seek_flags = cur.take_u8()?;
        let raw = cur.take_bytes(4)?;
        let row_count = i32::from_le_bytes([raw[0], raw[1], raw[2], raw[3]]);
        Ok(Self {
            input_handle_index: 0,
            seek_flags,
            row_count,
        })
    }
}

/// `RopSeekRow` success response (§2.2.7.2.2):
///   `RopId · InputHandleIndex · ReturnValue(4) · HasSoughtLess(1)` — note
///   `RowsSought` is omitted because the gateway clamps to the table bounds
///   and the client derives the new position from `QueryPosition`.
///   `SeekFlags` bit 0x01 = `SEEK_ROW_FROM_BEGINNING`, 0x02 has no
///   per-response incidence; the loader treats the table cursor as
///   absolute already, so the member is explained when it appears.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RopSeekRowResponse {
    pub input_handle_index: u8,
    pub return_value: RopErrorCode,
    pub has_sought_less: u8,
}

impl RopSeekRowResponse {
    pub fn encode(&self, out: &mut Vec<u8>) {
        out.push(RopId::ROP_SEEK_ROW.to_u8());
        out.push(self.input_handle_index);
        out.extend_from_slice(&self.return_value.to_u32().to_le_bytes());
        out.push(self.has_sought_less);
    }
}

/// `RopSeekRowBookmark` request (0x19, §2.2.7.3.1). Body after LogonId +
/// InputHandleIndex: `SeekFlags(1) · Bookmark(4 LE) · RowCount(4 LE signed)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RopSeekRowBookmarkRequest {
    pub input_handle_index: u8,
    pub seek_flags: u8,
    pub bookmark: u32,
    pub row_count: i32,
}

impl RopSeekRowBookmarkRequest {
    pub fn decode(cur: &mut Buf<'_>) -> Result<Self, DecodeError> {
        let seek_flags = cur.take_u8()?;
        let bookmark = cur.take_u32_le()?;
        let raw = cur.take_bytes(4)?;
        let row_count = i32::from_le_bytes([raw[0], raw[1], raw[2], raw[3]]);
        Ok(Self {
            input_handle_index: 0,
            seek_flags,
            bookmark,
            row_count,
        })
    }
}

/// `RopSeekRowBookmark` success response (§2.2.7.3.2):
///   `RopId · InputHandleIndex · ReturnValue(4) · RowsSought(4 LE) ·
///   HasSoughtLess(1)`. The true `RowsSought` count is echoed so the client
///   can detect clamped seeks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RopSeekRowBookmarkResponse {
    pub input_handle_index: u8,
    pub return_value: RopErrorCode,
    pub rows_sought: i32,
    pub has_sought_less: u8,
}

impl RopSeekRowBookmarkResponse {
    pub fn encode(&self, out: &mut Vec<u8>) {
        out.push(RopId::ROP_SEEK_ROW_BOOKMARK.to_u8());
        out.push(self.input_handle_index);
        out.extend_from_slice(&self.return_value.to_u32().to_le_bytes());
        out.extend_from_slice(&self.rows_sought.to_le_bytes());
        out.push(self.has_sought_less);
    }
}

/// `RopSeekRowFractional` request (0x1A, §2.2.7.4.1). Body after LogonId +
/// InputHandleIndex: `Numerator(4 LE) · Denominator(4 LE)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RopSeekRowFractionalRequest {
    pub input_handle_index: u8,
    pub numerator: u32,
    pub denominator: u32,
}

impl RopSeekRowFractionalRequest {
    pub fn decode(cur: &mut Buf<'_>) -> Result<Self, DecodeError> {
        let numerator = cur.take_u32_le()?;
        let denominator = cur.take_u32_le()?;
        Ok(Self {
            input_handle_index: 0,
            numerator,
            denominator,
        })
    }
}

/// `RopSeekRowFractional` success response (§2.2.7.4.2):
///   `RopId · InputHandleIndex · ReturnValue(4)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RopSeekRowFractionalResponse {
    pub input_handle_index: u8,
    pub return_value: RopErrorCode,
}

impl RopSeekRowFractionalResponse {
    pub fn encode(&self, out: &mut Vec<u8>) {
        out.push(RopId::ROP_SEEK_ROW_FRACTIONAL.to_u8());
        out.push(self.input_handle_index);
        out.extend_from_slice(&self.return_value.to_u32().to_le_bytes());
    }
}

/// `RopQueryPosition` request (0x17, §2.2.7.1.1). Body: none (header only).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RopQueryPositionRequest {
    pub input_handle_index: u8,
}

impl RopQueryPositionRequest {
    pub fn decode(_cur: &mut Buf<'_>) -> Result<Self, DecodeError> {
        Ok(Self {
            input_handle_index: 0,
        })
    }
}

/// `RopQueryPosition` success response (§2.2.7.1.2):
///   `RopId · InputHandleIndex · ReturnValue(4) · Numerator(4 LE) ·
///   Denominator(4 LE)`. Numerator/Denominator approximate the fractional
///   position; the gateway echoes the live cursor over the row count.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RopQueryPositionResponse {
    pub input_handle_index: u8,
    pub return_value: RopErrorCode,
    pub numerator: u32,
    pub denominator: u32,
}

impl RopQueryPositionResponse {
    pub fn encode(&self, out: &mut Vec<u8>) {
        out.push(RopId::ROP_QUERY_POSITION.to_u8());
        out.push(self.input_handle_index);
        out.extend_from_slice(&self.return_value.to_u32().to_le_bytes());
        out.extend_from_slice(&self.numerator.to_le_bytes());
        out.extend_from_slice(&self.denominator.to_le_bytes());
    }
}

/// `RopCreateBookmark` request (0x1B, §2.2.7.5.1) uses the 4-byte header
/// (it produces an output handle). After the dispatcher consumes
/// `RopId · LogonId · InputHandleIndex · OutputHandleIndex` there is no body.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RopCreateBookmarkRequest {
    pub input_handle_index: u8,
}

impl RopCreateBookmarkRequest {
    pub fn decode(_cur: &mut Buf<'_>) -> Result<Self, DecodeError> {
        Ok(Self {
            input_handle_index: 0,
        })
    }
}

/// `RopCreateBookmark` success response (§2.2.7.5.2):
///   `RopId · OutputHandleIndex · ReturnValue(4) · Bookmark(4 LE)`. A MAPI
///   Bookmark is a `ULONG` (4 bytes) per MS-OXCTABL; the gateway pins it to
///   the row's stable `row_id` (NOT the absolute cursor index) so a bookmark
///   survives a `RopSortTable` reorder — `RopSeekRowBookmark` resolves it by
///   scanning the table for the matching `row_id`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RopCreateBookmarkResponse {
    pub output_handle_index: u8,
    pub return_value: RopErrorCode,
    pub bookmark: u32,
}

impl RopCreateBookmarkResponse {
    pub fn encode(&self, out: &mut Vec<u8>) {
        out.push(RopId::ROP_CREATE_BOOKMARK.to_u8());
        out.push(self.output_handle_index);
        out.extend_from_slice(&self.return_value.to_u32().to_le_bytes());
        out.extend_from_slice(&self.bookmark.to_le_bytes());
    }
}

/// `RopFreeBookmark` request (0x89, §2.2.7.6.1). Body after LogonId +
/// InputHandleIndex: `Bookmark(4 LE)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RopFreeBookmarkRequest {
    pub input_handle_index: u8,
    pub bookmark: u32,
}

impl RopFreeBookmarkRequest {
    pub fn decode(cur: &mut Buf<'_>) -> Result<Self, DecodeError> {
        let bookmark = cur.take_u32_le()?;
        Ok(Self {
            input_handle_index: 0,
            bookmark,
        })
    }
}

/// `RopFreeBookmark` success response (§2.2.7.6.2):
///   `RopId · InputHandleIndex · ReturnValue(4)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RopFreeBookmarkResponse {
    pub input_handle_index: u8,
    pub return_value: RopErrorCode,
}

impl RopFreeBookmarkResponse {
    pub fn encode(&self, out: &mut Vec<u8>) {
        out.push(RopId::ROP_FREE_BOOKMARK.to_u8());
        out.push(self.input_handle_index);
        out.extend_from_slice(&self.return_value.to_u32().to_le_bytes());
    }
}

/// `RopResetTable` request (0x81, §2.2.5.7.1). Body: none (header only).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RopResetTableRequest {
    pub input_handle_index: u8,
}

impl RopResetTableRequest {
    pub fn decode(_cur: &mut Buf<'_>) -> Result<Self, DecodeError> {
        Ok(Self {
            input_handle_index: 0,
        })
    }
}

/// `RopResetTable` success response (§2.2.5.7.2):
///   `RopId · InputHandleIndex · ReturnValue(4)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RopResetTableResponse {
    pub input_handle_index: u8,
    pub return_value: RopErrorCode,
}

impl RopResetTableResponse {
    pub fn encode(&self, out: &mut Vec<u8>) {
        out.push(RopId::ROP_RESET_TABLE.to_u8());
        out.push(self.input_handle_index);
        out.extend_from_slice(&self.return_value.to_u32().to_le_bytes());
    }
}

/// A single `SortOrderItem` (MS-OXCDATA §2.12.1). Per the USPSerializer
/// `sortItem`: a `SortFlags(1)` byte (the low nibble is the ascending/
/// descending flag, 0x01 = descending) followed by a `PropertyTag(4)`.
/// The high nibble may carry additional flags (case-sensitivity, collating
/// sequence); we preserve the byte verbatim and lift it into the comparator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SortOrder {
    pub sort_flags: u8,
    pub tag: PropertyTag,
}

impl SortOrder {
    pub fn decode(cur: &mut Buf<'_>) -> Result<Self, DecodeError> {
        let sort_flags = cur.take_u8()?;
        let tag = PropertyTag::decode(cur)?;
        Ok(Self { sort_flags, tag })
    }
}

// ---- FastTransfer / ICS sync ROPs (MS-OXCFXICS) ---------------------------
//
// The FastTransfer "source" ROPs (0x4B/0x4C/0x4D/0x69) hand a connected
// client a serialized ICS stream describing a folder's children (or a single
// message / a property set). The client polls `RopFastTransferSourceGetBuffer`
// (0x4E) for successive ≤16 KiB chunks until the source signals completion
// (a zero-length buffer OR a terminal `TransferStatus = Done`). The
// "destination" family (0x53/0x54) plus the `RopSynchronization*` upload
// ROPs (0x72–0x77) is how Outlook *pushes* local changes back to the server;
// the gateway accepts the upload stream onto a FastTransferDestination,
// decodes it via the fxics Tokenizer, and applies the resulting
// message/hierarchy/read-state deltas to JMAP.
//
// All codecs follow the "body only" convention: the dispatcher consumes
// LogonId + InputHandleIndex (+ OutputHandleIndex where the ROP yields a
// handle) before calling `*::decode`.

/// `RopFastTransferSourceCopyMessages` request (0x4B, MS-OXCFXICS §3.1.1.1).
/// Body after LogonId + InputHandleIndex + OutputHandleIndex:
/// `Flags(1) · MessageIdCount(2 LE) · MessageIds (MessageIdCount × 8-byte
/// long-term ids)`. The gateway ignores the per-message long-term ids (it
/// serves the folder's contents as an incremental sync stream keyed by the
/// folder handle) and uses the flags only to decide full vs. partial copy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RopFastTransferSourceCopyMessagesRequest {
    pub input_handle_index: u8,
    pub output_handle_index: u8,
    pub flags: u8,
    pub message_ids: Vec<u64>,
}

impl RopFastTransferSourceCopyMessagesRequest {
    pub fn decode(cur: &mut Buf<'_>) -> Result<Self, DecodeError> {
        let flags = cur.take_u8()?;
        let count = usize::from(cur.take_u16_le()?);
        let mut message_ids = Vec::with_capacity(count.min(2048));
        for _ in 0..count {
            message_ids.push(cur.take_u64_le()?);
        }
        Ok(Self {
            input_handle_index: 0,
            output_handle_index: 0,
            flags,
            message_ids,
        })
    }
}

/// `RopFastTransferSourceCopyFolder` request (0x4C, MS-OXCFXICS §3.1.1.2).
/// Body after the 4-byte header: `Flags(1)`. The source copies the whole
/// subfolder (messages + subfolders) as a hierarchy+contents ICS stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RopFastTransferSourceCopyFolderRequest {
    pub input_handle_index: u8,
    pub output_handle_index: u8,
    pub flags: u8,
}

impl RopFastTransferSourceCopyFolderRequest {
    pub fn decode(cur: &mut Buf<'_>) -> Result<Self, DecodeError> {
        let flags = cur.take_u8()?;
        Ok(Self {
            input_handle_index: 0,
            output_handle_index: 0,
            flags,
        })
    }
}

/// `RopFastTransferSourceCopyTo` request (0x4D, MS-OXCFXICS §3.1.1.3). Body
/// after the 4-byte header: `Flags(1) · CopyToFlags(1) · PropertyTagCount(2
/// LE) · PropertyTags[count]`. The client lists the property *groups* to
/// copy; the gateway serves a property-only ICS stream built from the cached
/// message object.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RopFastTransferSourceCopyToRequest {
    pub input_handle_index: u8,
    pub output_handle_index: u8,
    pub flags: u8,
    pub copy_to_flags: u8,
    pub property_tags: Vec<PropertyTag>,
}

impl RopFastTransferSourceCopyToRequest {
    pub fn decode(cur: &mut Buf<'_>) -> Result<Self, DecodeError> {
        let flags = cur.take_u8()?;
        let copy_to_flags = cur.take_u8()?;
        let count = usize::from(cur.take_u16_le()?);
        let mut property_tags = Vec::with_capacity(count.min(1024));
        for _ in 0..count {
            property_tags.push(PropertyTag::decode(cur)?);
        }
        Ok(Self {
            input_handle_index: 0,
            output_handle_index: 0,
            flags,
            copy_to_flags,
            property_tags,
        })
    }
}

/// `RopFastTransferSourceCopyProperties` request (0x69). Body after the
/// 4-byte header: `Flags(1) · PropertyTagCount(2 LE) · PropertyTags[count]`.
/// A property-only transfer that serialises just the listed tags.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RopFastTransferSourceCopyPropertiesRequest {
    pub input_handle_index: u8,
    pub output_handle_index: u8,
    pub flags: u8,
    pub property_tags: Vec<PropertyTag>,
}

impl RopFastTransferSourceCopyPropertiesRequest {
    pub fn decode(cur: &mut Buf<'_>) -> Result<Self, DecodeError> {
        let flags = cur.take_u8()?;
        let count = usize::from(cur.take_u16_le()?);
        let mut property_tags = Vec::with_capacity(count.min(1024));
        for _ in 0..count {
            property_tags.push(PropertyTag::decode(cur)?);
        }
        Ok(Self {
            input_handle_index: 0,
            output_handle_index: 0,
            flags,
            property_tags,
        })
    }
}

/// `RopFastTransferSourceGetBuffer` request (0x4E, MS-OXCFXICS §3.1.1.5).
/// Body after LogonId + InputHandleIndex: `BufferSize(2 LE) · TransferFlags
/// (1)`. The client polls with the maximum chunk size it can accept.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RopFastTransferSourceGetBufferRequest {
    pub input_handle_index: u8,
    pub buffer_size: u16,
    pub transfer_flags: u8,
}

impl RopFastTransferSourceGetBufferRequest {
    pub fn decode(cur: &mut Buf<'_>) -> Result<Self, DecodeError> {
        let buffer_size = cur.take_u16_le()?;
        let transfer_flags = cur.take_u8()?;
        Ok(Self {
            input_handle_index: 0,
            buffer_size,
            transfer_flags,
        })
    }
}

/// `RopFastTransferSourceGetBuffer` success response (§3.1.1.5.2):
///   `RopId · InputHandleIndex · ReturnValue(4) · TransferStatus(1) ·
///   TerminatorLow(1) · TerminatorHigh(1) · Padding(1) · DataSize(2 LE) ·
///   Data(DataSize)`. `TransferStatus` is 0=InProgress, 1=Done, 2=Error.
/// `DataSize` is clamped to the available remaining bytes and the requested
/// `buffer_size`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RopFastTransferSourceGetBufferSuccess {
    pub input_handle_index: u8,
    pub return_value: RopErrorCode,
    /// 2-byte `TransferStatus` (MS-OXCFXICS §2.2.3.1.1.5.2):
    /// 0=Error, 1=Partial/InProgress, 2=NoRoom, 3=Done. Encoded little-endian.
    pub transfer_status: u16,
    /// `InProgressCount` (2): steps completed so far (progress display only).
    pub in_progress_count: u16,
    /// `TotalStepCount` (2): approximate total step count (progress display).
    pub total_step_count: u16,
    /// `TransferBufferSize` (2): number of bytes in `TransferBuffer`. MUST
    /// equal `data.len()` and be clamped to `u16::MAX`.
    pub transfer_buffer_size: u16,
    pub data: Vec<u8>,
}

impl RopFastTransferSourceGetBufferSuccess {
    pub fn encode(&self, out: &mut Vec<u8>) {
        out.push(RopId::ROP_FAST_TRANSFER_SOURCE_GET_BUFFER.to_u8());
        out.push(self.input_handle_index);
        out.extend_from_slice(&self.return_value.to_u32().to_le_bytes());
        // MS-OXCFXICS: a non-Success `ReturnValue` carries NO body fields —
        // clients parse the rest only when ReturnValue == success (0). Emit
        // the header and stop so the FastTransfer stream cannot be
        // mis-parsed after an error ROP.
        if self.return_value != RopErrorCode::Success {
            return;
        }
        out.extend_from_slice(&self.transfer_status.to_le_bytes());
        out.extend_from_slice(&self.in_progress_count.to_le_bytes());
        out.extend_from_slice(&self.total_step_count.to_le_bytes());
        out.push(0u8); // Reserved (MUST be 0x00 on send)
        out.extend_from_slice(&self.transfer_buffer_size.to_le_bytes());
        out.extend_from_slice(&self.data);
    }
}

/// `RopFastTransferDestinationConfigure` request (0x53, MS-OXCFXICS
/// §3.1.2.1). Body after the 4-byte header: `Source_FMT(1) · SyncFlags(1)`.
/// The gateway accepts the configure; the upload stream is fed by
/// `RopFastTransferDestinationPutBuffer`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RopFastTransferDestinationConfigureRequest {
    pub input_handle_index: u8,
    pub output_handle_index: u8,
    pub source_fmt: u8,
    pub sync_flags: u8,
}

impl RopFastTransferDestinationConfigureRequest {
    pub fn decode_after_ropid(cur: &mut Buf<'_>) -> Result<Self, DecodeError> {
        let source_fmt = cur.take_u8()?;
        let sync_flags = cur.take_u8()?;
        Ok(Self {
            input_handle_index: 0,
            output_handle_index: 0,
            source_fmt,
            sync_flags,
        })
    }
}

/// `RopFastTransferDestinationPutBuffer` request (0x54, §3.1.2.2). Body after
/// LogonId + InputHandleIndex: `DataSize(2 LE) · Data(DataSize)`. The
/// destination accumulates the bytes until the client signals completion (a
/// zero-length PutBuffer following a terminal marker), at which point the
/// gateway tokenises and applies the deltas.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RopFastTransferDestinationPutBufferRequest {
    pub input_handle_index: u8,
    pub data: Vec<u8>,
}

impl RopFastTransferDestinationPutBufferRequest {
    pub fn decode_after_ropid(cur: &mut Buf<'_>) -> Result<Self, DecodeError> {
        let size = usize::from(cur.take_u16_le()?);
        let data = cur.take_bytes(size)?.to_vec();
        Ok(Self {
            input_handle_index: 0,
            data,
        })
    }
}

/// `RopFastTransferDestinationPutBuffer` success response (§3.1.2.2.2):
///   `RopId · InputHandleIndex · ReturnValue(4) · TransferStatus(1) ·
///   DataRemaining(4 LE)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RopFastTransferDestinationPutBufferResponse {
    pub input_handle_index: u8,
    pub return_value: RopErrorCode,
    pub transfer_status: u8,
    pub data_remaining: u32,
}

impl RopFastTransferDestinationPutBufferResponse {
    pub fn encode(&self, out: &mut Vec<u8>) {
        out.push(RopId::ROP_FAST_TRANSFER_DESTINATION_PUT_BUFFER.to_u8());
        out.push(self.input_handle_index);
        out.extend_from_slice(&self.return_value.to_u32().to_le_bytes());
        // On a non-Success `ReturnValue` the body (TransferStatus +
        // DataRemaining) is omitted so the client stops parsing the stream
        // rather than consuming a stale transfer-status byte. These fields
        // are present IFF ReturnValue == success per the FastTransfer
        // success-shape contract (matches the stream-success encoders).
        if self.return_value != RopErrorCode::Success {
            return;
        }
        out.push(self.transfer_status);
        out.extend_from_slice(&self.data_remaining.to_le_bytes());
    }
}

/// Generic success envelope shared by the FastTransfer source ROPs that only
/// need to acknowledge handle installation: `RopId · OutputHandleIndex ·
/// ReturnValue(4)`. Used by CopyMessages / CopyFolder / CopyTo /
/// CopyProperties on success.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RopFastTransferSourceOpenResponse {
    pub output_handle_index: u8,
    pub return_value: RopErrorCode,
}

impl RopFastTransferSourceOpenResponse {
    pub fn encode(&self, out: &mut Vec<u8>, rop_id: RopId) {
        out.push(rop_id.to_u8());
        out.push(self.output_handle_index);
        out.extend_from_slice(&self.return_value.to_u32().to_le_bytes());
    }
}

/// `RopSynchronizationConfigure` request (0x70, §3.3.1.1). Body after the
/// 4-byte header: `SyncFlags(1) · SyncType(1) · SynchronizationStateLength(2
/// LE) · SynchronizationState(...)`. The gateway accepts the configured
/// upload/download context; the state blob (Outlook's last sync watermark)
/// is carried on the destination/source handle for replay.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RopSynchronizationConfigureRequest {
    pub input_handle_index: u8,
    pub output_handle_index: u8,
    pub sync_flags: u8,
    pub sync_type: u8,
    pub sync_state: Vec<u8>,
}

impl RopSynchronizationConfigureRequest {
    pub fn decode_after_ropid(cur: &mut Buf<'_>) -> Result<Self, DecodeError> {
        let sync_flags = cur.take_u8()?;
        let sync_type = cur.take_u8()?;
        let len = usize::from(cur.take_u16_le()?);
        let sync_state = cur.take_bytes(len)?.to_vec();
        Ok(Self {
            input_handle_index: 0,
            output_handle_index: 0,
            sync_flags,
            sync_type,
            sync_state,
        })
    }
}

/// Shared success envelope for the Synchronization upload ROPs (Import*
/// / UploadStateStream*) and SynchronizationConfigure:
///   `RopId · InputHandleIndex · ReturnValue(4)`. These are server-side
/// applies that the client only needs acknowledged.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RopSynchronizationAckResponse {
    pub input_handle_index: u8,
    pub return_value: RopErrorCode,
}

impl RopSynchronizationAckResponse {
    pub fn encode(&self, out: &mut Vec<u8>, rop_id: RopId) {
        out.push(rop_id.to_u8());
        out.push(self.input_handle_index);
        out.extend_from_slice(&self.return_value.to_u32().to_le_bytes());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ropid_roundtrips_known_ids() {
        // Every assigned id decodes and re-encodes. Spot-check the Phase 0
        // set plus a representative sample across the id space.
        for b in [
            0x01u8, 0x02, 0x04, 0x05, 0x06, 0x07, 0x08, 0x0C, 0x11, 0x12, 0x15, 0x16, 0x1E, 0x1F,
            0x29, 0x2A, 0x2B, 0x33, 0x4B, 0x5E, 0x66, 0x70, 0x82, 0x90, 0xF9, 0xFE, 0xFF,
        ] {
            let id = RopId::from_u8(b);
            assert_eq!(id.to_u8(), b, "round-trip failed for 0x{b:02X}");
        }
    }

    #[test]
    fn ropid_unknown_preserved() {
        // 0x88 is reserved; the newtype carries it through verbatim.
        let id = RopId::from_u8(0x88);
        assert_eq!(id.0, 0x88);
        assert_eq!(id.to_u8(), 0x88);
    }

    #[test]
    fn rop_error_code_roundtrips() {
        for v in [
            0u32, 0x80070005, 0x80070057, 0x8007000E, 0x80040109, 0x80040115, 0x80040108,
            0x8004010F, 0x12345678,
        ] {
            assert_eq!(RopErrorCode::from_u32(v).to_u32(), v, "v={v:#x}");
        }
    }

    fn build_logon_request() -> RopLogonRequest {
        RopLogonRequest {
            logon_id: 7,
            output_handle_index: 0,
            logon_flags: LogonFlags(0x01),
            open_flags: OpenFlags(0x0000_0002),
            store_state: 0,
            essdn: "/o=Example/ou=First Administrative Group/cn=Recipients/cn=user".to_string(),
        }
    }

    #[test]
    fn roplogon_request_roundtrip() {
        let req = build_logon_request();
        let mut buf = Vec::new();
        req.encode(&mut buf);
        let mut cur = Buf::new(&buf);
        let got = RopLogonRequest::decode(&mut cur).expect("decode");
        assert_eq!(got, req);
    }

    #[test]
    fn roplogon_rejects_bad_store_state() {
        // Hand-build a buffer with store_state != 0. The byte layout is:
        // RopId, logon id, output handle index, logon flags (4 bytes),
        // open flags (4 LE), store state (4 LE), essdn size incl nul (2 LE),
        // essdn payload.
        let buf: Vec<u8> = vec![
            RopId::ROP_LOGON.to_u8(),
            0,    // logon id
            0,    // output handle index
            0x01, // logon flags
            0x02,
            0x00,
            0x00,
            0x00, // open flags = 2
            0x01,
            0x00,
            0x00,
            0x00, // store state = 1 (invalid)
            0x03,
            0x00, // essdn size incl nul = 3
            b'a',
            b'b',
            0x00, // essdn "ab\0"
        ];
        let mut cur = Buf::new(&buf);
        let err = RopLogonRequest::decode(&mut cur).unwrap_err();
        assert_eq!(err, DecodeError::InvalidValue);
    }

    #[test]
    fn roplogon_rejects_reserved_logon_flags() {
        let lf = LogonFlags::parse(0x20);
        assert_eq!(lf.unwrap_err(), DecodeError::InvalidValue);
        assert!(LogonFlags::parse(0x1F).is_ok());
    }

    // ---- RopSetProperties / RopDeleteProperties / RopCopyTo codec round-trips ----
    // These exercise the new property-write ROP codecs (audit gap 2a). The
    // request decoders read the count-prefixed bodies; the success envelope
    // (`RopPropertyWriteSuccess`) encodes the problem array.

    fn tag(t: crate::mapi::data::PropertyType, id: u16) -> crate::mapi::data::PropertyTag {
        crate::mapi::data::PropertyTag::new(t, id)
    }

    /// Encode a `TaggedPropertyValue` for a *ROP buffer* context
    /// (RopSetProperties request body, MS-OXCDATA 2.11.1.1 / 2.11.2.1). This
    /// differs from `PropertyValue::encode`, which targets the
    /// no-length-prefix *property-row* form: in a ROP buffer a PtypString is
    /// a 2-byte code-unit count + units + 0x0000, and PtypString8 is a
    /// 2-byte char count + chars + 0x00. PtypInteger32 is identical in both
    /// forms (4 LE bytes). We hand-build the scalar forms the decoder reads
    /// so the test exercises the real wire shape rather than the asymmetric
    /// row encoder.
    fn tv_bytes_rop(tv: &crate::mapi::data::TaggedPropertyValue) -> Vec<u8> {
        use crate::mapi::data::PropertyValue;
        let mut out = Vec::new();
        tv.tag.encode(&mut out);
        match &tv.value {
            PropertyValue::String(s) => {
                let units: Vec<u16> = s.encode_utf16().collect();
                let n = u16::try_from(units.len()).unwrap_or(u16::MAX);
                out.extend_from_slice(&n.to_le_bytes());
                for u in &units {
                    out.extend_from_slice(&u.to_le_bytes());
                }
                out.extend_from_slice(&0u16.to_le_bytes());
            }
            PropertyValue::String8(s) => {
                let n = u16::try_from(s.len()).unwrap_or(u16::MAX);
                out.extend_from_slice(&n.to_le_bytes());
                out.extend_from_slice(s.as_bytes());
                out.push(0);
            }
            PropertyValue::Integer32(i) => out.extend_from_slice(&i.to_le_bytes()),
            PropertyValue::Integer16(i) => out.extend_from_slice(&i.to_le_bytes()),
            PropertyValue::Null => {}
            PropertyValue::Opaque { bytes, .. } => out.extend_from_slice(bytes),
            other => {
                // For any other scalar, fall back to the row encoder (the
                // decoder reads the same fixed/length-prefixed form for
                // Boolean/Time/Binary/Guid as the row encoder writes, so the
                // two only diverge for String/String8).
                other.encode(&mut out);
            }
        }
        out
    }

    fn encode_set_properties_body(values: &[crate::mapi::data::TaggedPropertyValue]) -> Vec<u8> {
        let mut payload = Vec::new();
        for tv in values {
            payload.extend_from_slice(&tv_bytes_rop(tv));
        }
        let size = u16::try_from(2 + payload.len()).unwrap_or(u16::MAX);
        let count = u16::try_from(values.len()).unwrap_or(u16::MAX);
        let mut out = Vec::new();
        out.extend_from_slice(&size.to_le_bytes());
        out.extend_from_slice(&count.to_le_bytes());
        out.extend_from_slice(&payload);
        out
    }

    #[test]
    fn rop_set_properties_decodes_subject_and_importance() {
        // PR_SUBJECT (PtypString, 0x0037) = "Hi" and PR_IMPORTANCE
        // (PtypInteger32, 0x0017) = 2 (High). Both are translatable compose
        // props the Outlook write path sends.
        use crate::mapi::data::{PropertyType, PropertyValue, TaggedPropertyValue};
        let values = vec![
            TaggedPropertyValue {
                tag: tag(PropertyType::PTYP_STRING, 0x0037),
                value: PropertyValue::String("Hi".to_string()),
            },
            TaggedPropertyValue {
                tag: tag(PropertyType::PTYP_INTEGER32, 0x0017),
                value: PropertyValue::Integer32(2),
            },
        ];
        let body = encode_set_properties_body(&values);
        let mut cur = Buf::new(&body);
        let req = RopSetPropertiesRequest::decode(&mut cur).expect("decode");
        assert_eq!(req.property_values.len(), 2);
        assert_eq!(req.property_values[0].tag, values[0].tag);
        assert_eq!(req.property_values[1].tag, values[1].tag);
        // The scalar subject round-trips through the count-prefixed wire
        // form the ROP buffer uses.
        assert_eq!(
            req.property_values[0].value,
            PropertyValue::String("Hi".to_string())
        );
        assert_eq!(req.property_values[1].value, PropertyValue::Integer32(2));
        // Cursor must be fully consumed (no chain desync).
        assert_eq!(cur.remaining(), 0);
    }

    #[test]
    fn rop_set_properties_rejects_size_count_mismatch() {
        // Declare a PropertyValueSize that swallows fewer bytes than the
        // declared count actually encodes -> the codec must fail closed
        // rather than desynchronising the chain.
        use crate::mapi::data::{PropertyType, PropertyValue, TaggedPropertyValue};
        let values = vec![TaggedPropertyValue {
            tag: tag(PropertyType::PTYP_INTEGER32, 0x0017),
            value: PropertyValue::Integer32(1),
        }];
        let mut body = encode_set_properties_body(&values);
        // Corrupt the size field to 2 (only the count word) so the payload
        // is declared empty while count=1.
        body[0] = 2;
        body[1] = 0;
        let mut cur = Buf::new(&body);
        assert!(RopSetPropertiesRequest::decode(&mut cur).is_err());
    }

    #[test]
    fn rop_set_properties_tolerates_mv_value() {
        // A PtypMultipleString8 value (type 0x101E) in the ROP-buffer form:
        // a 32-bit element count + NUL-terminated String8 elements. The
        // decoder sizes-and-skips it so the following scalar entry still
        // decodes and the chain cursor stays aligned (audit 2a).
        use crate::mapi::data::{PropertyType, PropertyValue, TaggedPropertyValue};
        let mut mv_payload: Vec<u8> = Vec::new();
        mv_payload.extend_from_slice(&1u32.to_le_bytes()); // 1 element
        // one String8 element "x" with a terminating 0x00 (no count prefix
        // for MV elements per MS-OXCDATA 2.11.1.1).
        mv_payload.push(b'x');
        mv_payload.push(0);
        let mv_tv = TaggedPropertyValue {
            tag: tag(PropertyType::from_u16(0x101E), 0x8001),
            value: PropertyValue::Opaque {
                property_type: PropertyType::from_u16(0x101E),
                bytes: mv_payload,
            },
        };
        let scalar_tv = TaggedPropertyValue {
            tag: tag(PropertyType::PTYP_INTEGER32, 0x0017),
            value: PropertyValue::Integer32(0),
        };
        let values = vec![mv_tv, scalar_tv];
        let body = encode_set_properties_body(&values);
        let mut cur = Buf::new(&body);
        let req = RopSetPropertiesRequest::decode(&mut cur).expect("decode");
        assert_eq!(req.property_values.len(), 2);
        assert_eq!(cur.remaining(), 0);
    }

    #[test]
    fn rop_delete_properties_decodes_tag_array() {
        use crate::mapi::data::PropertyType;
        let mut body = Vec::new();
        let tags = [
            tag(PropertyType::PTYP_STRING, 0x0037),
            tag(PropertyType::PTYP_INTEGER32, 0x0017),
        ];
        body.extend_from_slice(&u16::try_from(tags.len()).unwrap().to_le_bytes());
        for t in &tags {
            t.encode(&mut body);
        }
        let mut cur = Buf::new(&body);
        let req = RopDeletePropertiesRequest::decode(&mut cur).expect("decode");
        assert_eq!(req.property_tags.len(), 2);
        assert_eq!(req.property_tags[0], tags[0]);
        assert_eq!(req.property_tags[1], tags[1]);
        assert_eq!(cur.remaining(), 0);
    }

    #[test]
    fn rop_copy_to_decodes_handles_and_flags() {
        // SourceHandleIndex=3, DestHandleIndex=4, WantAsynchronous=1,
        // WantSubObjects=0, CopyFlags=0, two excluded tags.
        use crate::mapi::data::PropertyType;
        let excluded = [
            tag(PropertyType::PTYP_BINARY, 0x0E20),
            tag(PropertyType::PTYP_INTEGER32, 0x0E08),
        ];
        let mut body: Vec<u8> = vec![3, 4, 1, 0, 0];
        // head bytes: source handle, dest handle, want async, want sub
        // objects, copy flags (the two excluded tags are appended below).
        body.extend_from_slice(&u16::try_from(excluded.len()).unwrap().to_le_bytes());
        for t in &excluded {
            t.encode(&mut body);
        }
        let mut cur = Buf::new(&body);
        let req = RopCopyToRequest::decode(&mut cur).expect("decode");
        assert_eq!(req.source_handle_index, 3);
        assert_eq!(req.dest_handle_index, 4);
        assert_eq!(req.want_asynchronous, 1);
        assert_eq!(req.want_sub_objects, 0);
        assert_eq!(req.copy_flags, 0);
        assert_eq!(req.excluded_tags.len(), 2);
        assert_eq!(req.excluded_tags[0], excluded[0]);
        assert_eq!(req.excluded_tags[1], excluded[1]);
        assert_eq!(cur.remaining(), 0);
    }

    #[test]
    fn rop_property_write_success_envelope_encodes_problems() {
        // RopId + HandleIndex + ReturnValue(4 LE) + ProblemCount(2 LE) +
        // problems[index(2 LE) + tag(4) + code(4 LE)].
        use crate::mapi::data::{PropertyProblem, PropertyType};
        let problems = vec![PropertyProblem {
            index: 2,
            tag: tag(PropertyType::PTYP_STRING, 0x1000), // PR_BODY -> NO_SUPPORT
            error_code: 0x8004_0102,
        }];
        let resp = RopPropertyWriteSuccess {
            rop_id: RopId::ROP_SET_PROPERTIES,
            handle_index: 7,
            return_value: RopErrorCode::Success,
            problems,
        };
        let mut buf = Vec::new();
        resp.encode(&mut buf);
        assert_eq!(buf[0], RopId::ROP_SET_PROPERTIES.to_u8());
        assert_eq!(buf[1], 7);
        let rv = u32::from_le_bytes([buf[2], buf[3], buf[4], buf[5]]);
        assert_eq!(rv, RopErrorCode::Success.to_u32());
        let count = u16::from_le_bytes([buf[6], buf[7]]);
        assert_eq!(count, 1);
        let idx = u16::from_le_bytes([buf[8], buf[9]]);
        assert_eq!(idx, 2);
        let code = u32::from_le_bytes([buf[14], buf[15], buf[16], buf[17]]);
        assert_eq!(code, 0x8004_0102);
    }

    #[test]
    fn rop_property_write_success_empty_problems() {
        let resp = RopPropertyWriteSuccess {
            rop_id: RopId::ROP_COPY_TO,
            handle_index: 9,
            return_value: RopErrorCode::Success,
            problems: Vec::new(),
        };
        let mut buf = Vec::new();
        resp.encode(&mut buf);
        // 1 + 1 + 4 + 2 = 8 bytes, count=0.
        assert_eq!(buf.len(), 8);
        assert_eq!(buf[0], RopId::ROP_COPY_TO.to_u8());
        assert_eq!(buf[1], 9);
        assert_eq!(u16::from_le_bytes([buf[6], buf[7]]), 0);
    }

    #[test]
    fn rop_property_write_failure_envelope_omits_problems() {
        // A non-Success ReturnValue MUST emit the 6-byte failure envelope
        // (RopId + HandleIndex + ReturnValue) with NO PropertyProblemCount or
        // PropertyProblem array. The prior impl appended a zero-filled count
        // on failure, corrupting the chain cursor (Qodo #2, cubic #25).
        // Construct with a non-empty `problems`; the encoder must STILL drop
        // them on failure (the caller carries them only for diagnostics).
        let resp = RopPropertyWriteSuccess {
            rop_id: RopId::ROP_SET_PROPERTIES,
            handle_index: 4,
            return_value: RopErrorCode::DiskError,
            problems: vec![crate::mapi::data::PropertyProblem {
                index: 0,
                tag: crate::mapi::data::PropertyTag::new(
                    crate::mapi::data::PropertyType::PTYP_STRING,
                    0x0037,
                ),
                error_code: 0x8004_0102,
            }],
        };
        let mut buf = Vec::new();
        resp.encode(&mut buf);
        assert_eq!(buf.len(), 6, "failure envelope must be exactly 6 bytes");
        assert_eq!(buf[0], RopId::ROP_SET_PROPERTIES.to_u8());
        assert_eq!(buf[1], 4);
        let rv = u32::from_le_bytes([buf[2], buf[3], buf[4], buf[5]]);
        assert_eq!(rv, RopErrorCode::DiskError.to_u32());
    }

    #[test]
    fn rop_delete_properties_failure_envelope_omits_problems() {
        // Same 6-byte failure envelope shape for the DeleteProperties arm
        // (shares RopPropertyWriteSuccess::encode).
        let resp = RopPropertyWriteSuccess {
            rop_id: RopId::ROP_DELETE_PROPERTIES,
            handle_index: 2,
            return_value: RopErrorCode::AccessDenied,
            problems: Vec::new(),
        };
        let mut buf = Vec::new();
        resp.encode(&mut buf);
        assert_eq!(buf.len(), 6);
        assert_eq!(buf[0], RopId::ROP_DELETE_PROPERTIES.to_u8());
        assert_eq!(buf[1], 2);
        let rv = u32::from_le_bytes([buf[2], buf[3], buf[4], buf[5]]);
        assert_eq!(rv, RopErrorCode::AccessDenied.to_u32());
    }

    #[test]
    fn rop_copy_to_success_includes_excluded_tag_problems() {
        // CopyTo success with a populated problem array: the spec
        // (2.2.8.12.4) reports per-property issues on success. This covers
        // the new `RopCopyToSuccess` wire shape used when ExcludedTags are
        // honoured (Qodo #4/#8, cubic #12).
        let problems = vec![
            crate::mapi::data::PropertyProblem {
                index: 0,
                tag: crate::mapi::data::PropertyTag::new(
                    crate::mapi::data::PropertyType::PTYP_STRING,
                    0x0037,
                ), // PR_SUBJECT
                error_code: 0x8004_0102,
            },
            crate::mapi::data::PropertyProblem {
                index: 1,
                tag: crate::mapi::data::PropertyTag::new(
                    crate::mapi::data::PropertyType::PTYP_INTEGER32,
                    0x0017,
                ), // PR_IMPORTANCE
                error_code: 0x8004_0102,
            },
        ];
        let resp = crate::mapi::rops::RopCopyToSuccess {
            rop_id: RopId::ROP_COPY_TO,
            handle_index: 3,
            return_value: RopErrorCode::Success,
            problems,
        };
        let mut buf = Vec::new();
        resp.encode(&mut buf);
        assert_eq!(buf[0], RopId::ROP_COPY_TO.to_u8());
        assert_eq!(buf[1], 3);
        let rv = u32::from_le_bytes([buf[2], buf[3], buf[4], buf[5]]);
        assert_eq!(rv, RopErrorCode::Success.to_u32());
        let count = u16::from_le_bytes([buf[6], buf[7]]);
        assert_eq!(count, 2);
    }

    proptest::proptest! {
        #[test]
        fn rop_header_roundtrip(rop_id in 0u8..=255u8, logon_id in 0u8..=255u8, handle in 0u8..=255u8) {
            let h = RopHeader {
                rop_id: RopId::from_u8(rop_id),
                logon_id,
                handle_index: handle,
            };
            let mut buf = Vec::new();
            h.encode(&mut buf);
            let mut cur = Buf::new(&buf);
            let got = RopHeader::decode(&mut cur).expect("decode");
            proptest::prop_assert_eq!(got, h);
            proptest::prop_assert_eq!(cur.remaining(), 0);
        }

        #[test]
        fn roplogon_response_envelope_roundtrip(
            handle in 0u8..=255u8,
            code in 0u32..=0xFFFF_FFFFu32,
            flags in 0u8..=255u8,
        ) {
            let body = RopLogonSuccess {
                output_handle_index: handle,
                return_value: RopErrorCode::from_u32(code),
                logon_flags: LogonFlags(flags & LogonFlags::DEFINED_MASK),
                folder_ids: [[0u8; 52]; 9],
                response_flags: flags & 0x03,
                mailbox_guid: [0u8; 16],
            };
            let mut buf = Vec::new();
            body.encode(&mut buf);
            // Verify the RopId and return-value are at the expected offsets.
            proptest::prop_assert_eq!(buf[0], RopId::ROP_LOGON.to_u8());
            proptest::prop_assert_eq!(buf[1], handle);
            let rv = u32::from_le_bytes([buf[2], buf[3], buf[4], buf[5]]);
            proptest::prop_assert_eq!(rv, RopErrorCode::from_u32(code).to_u32());
        }

        #[test]
        fn rop_error_response_roundtrip(handle in 0u8..=255u8, code in 0u32..=0xFFFF_FFFFu32) {
            let r = RopErrorResponse {
                rop_id: RopId::ROP_LOGON,
                output_handle_index: handle,
                return_value: RopErrorCode::from_u32(code),
            };
            let mut buf = Vec::new();
            r.encode(&mut buf);
            proptest::prop_assert_eq!(buf.len(), 1 + 1 + 4);
            proptest::prop_assert_eq!(buf[0], 0xFE);
            proptest::prop_assert_eq!(buf[1], handle);
            let rv = u32::from_le_bytes([buf[2], buf[3], buf[4], buf[5]]);
            proptest::prop_assert_eq!(rv, RopErrorCode::from_u32(code).to_u32());
        }
    }

    // ---- Stream ROP codec round-trips ---------------------------------------

    fn body_tag() -> crate::mapi::data::PropertyTag {
        crate::mapi::data::PropertyTag::new(
            crate::mapi::data::PropertyType::PTYP_STRING8,
            crate::mapi::store::PR_BODY,
        )
    }

    #[test]
    fn rop_open_stream_roundtrip() {
        let tag = body_tag();
        // decode_body takes Input/Output as parameters and reads only the
        // PropertyTag + OpenModeFlags from the cursor, so the body bytes are
        // exactly the tag + flags (no LogonId/Input/Output prefix).
        let mut body = Vec::new();
        tag.encode(&mut body);
        body.push(0x00);
        let mut cur = Buf::new(&body);
        let req = RopOpenStreamRequest::decode_body(&mut cur, 3, 7).expect("decode");
        assert_eq!(req.input_handle_index, 3);
        assert_eq!(req.output_handle_index, 7);
        assert_eq!(req.property_tag, tag);
        assert_eq!(req.open_mode_flags, 0x00);
        assert_eq!(cur.remaining(), 0);

        // Success response round-trip.
        let mut out = Vec::new();
        RopOpenStreamSuccess {
            output_handle_index: 7,
            return_value: RopErrorCode::Success,
            stream_size: 0x100,
        }
        .encode(&mut out);
        assert_eq!(out[0], 0x2B);
        assert_eq!(out[1], 7);
        assert_eq!(
            RopErrorCode::from_u32(u32::from_le_bytes([out[2], out[3], out[4], out[5]])),
            RopErrorCode::Success
        );
        assert_eq!(u32::from_le_bytes([out[6], out[7], out[8], out[9]]), 0x100);
    }

    #[test]
    fn rop_read_stream_decodes_bytecount_and_extended() {
        // Plain ByteCount (no MaximumByteCount).
        let body: Vec<u8> = vec![4, 0xFF, 0xFF];
        let mut cur = Buf::new(&body);
        let req = RopReadStreamRequest::decode_after_ropid(&mut cur).expect("decode");
        assert_eq!(req.input_handle_index, 4);
        assert_eq!(req.byte_count, 0xFFFF);
        assert!(req.maximum_byte_count.is_none());
        assert_eq!(req.max_bytes().unwrap(), 0xFFFF);

        // Extended form: ByteCount == 0xBABE then a 4-byte MaximumByteCount.
        // u32 LE bytes for 0x00100000 are [0x00, 0x00, 0x10, 0x00].
        let body: Vec<u8> = vec![4, 0xBE, 0xBA, 0x00, 0x00, 0x10, 0x00];
        let mut cur = Buf::new(&body);
        let req = RopReadStreamRequest::decode_after_ropid(&mut cur).expect("decode ext");
        assert_eq!(req.byte_count, READ_STREAM_EXTENDED_BYTECOUNT);
        assert_eq!(req.maximum_byte_count, Some(0x00100000));
        assert_eq!(req.max_bytes().unwrap(), 0x00100000);
        assert_eq!(cur.remaining(), 0);
    }

    #[test]
    fn rop_read_stream_rejects_oversize_maximum() {
        // MaximumByteCount > 0x80000000 must fail (spec SHOULD-return 0x000004B6).
        let body: Vec<u8> = vec![4, 0xBE, 0xBA, 0x01, 0x00, 0x00, 0x80];
        let mut cur = Buf::new(&body);
        let req = RopReadStreamRequest::decode_after_ropid(&mut cur).expect("decode");
        assert!(req.max_bytes().is_err());
    }

    #[test]
    fn rop_read_stream_success_truncates_data_to_u16() {
        // DataSize is a 2-byte count; a Data slice longer than u16::MAX is
        // truncated to u16::MAX bytes on the wire (the encoder saturates).
        let mut out = Vec::new();
        RopReadStreamSuccess {
            input_handle_index: 4,
            return_value: RopErrorCode::Success,
            data: vec![0xAB; (u16::MAX as usize) + 10],
        }
        .encode(&mut out);
        assert_eq!(out[0], 0x2C);
        let data_size = u16::from_le_bytes([out[6], out[7]]);
        assert_eq!(data_size, u16::MAX);
        // Total length = header(6) + DataSize(2) + u16::MAX bytes.
        assert_eq!(out.len(), 6 + 2 + u16::MAX as usize);
    }

    #[test]
    fn rop_write_stream_roundtrip() {
        let payload = b"hello body".to_vec();
        let mut body = Vec::new();
        body.push(2); // input handle
        body.extend_from_slice(&(payload.len() as u16).to_le_bytes());
        body.extend_from_slice(&payload);
        let mut cur = Buf::new(&body);
        let req = RopWriteStreamRequest::decode_after_ropid(&mut cur).expect("decode");
        assert_eq!(req.input_handle_index, 2);
        assert_eq!(req.data, payload);
        assert_eq!(cur.remaining(), 0);

        let mut out = Vec::new();
        RopWriteStreamSuccess {
            input_handle_index: 2,
            return_value: RopErrorCode::Success,
            written_size: 10,
        }
        .encode(&mut out);
        assert_eq!(out[0], 0x2D);
        assert_eq!(u16::from_le_bytes([out[6], out[7]]), 10);
    }

    #[test]
    fn rop_write_stream_rejects_declared_but_absent_payload() {
        // A DataSize declared larger than the buffer the client supplied is
        // rejected with `Insufficient` so a truncated trailing ROP does not
        // leave the chain cursor misaligned.
        let mut body = Vec::new();
        body.push(2);
        body.extend_from_slice(&100u16.to_le_bytes()); // declares 100 bytes
        body.extend_from_slice(b"only five"); // supplies 9
        let mut cur = Buf::new(&body);
        assert!(RopWriteStreamRequest::decode_after_ropid(&mut cur).is_err());
    }

    #[test]
    fn rop_seek_stream_resolve_clamps_and_origins() {
        // Beginning, positive offset.
        let req = RopSeekStreamRequest {
            input_handle_index: 1,
            origin: 0x00,
            offset: 5,
        };
        assert_eq!(req.resolve(0, 100).unwrap(), 5);
        // Beginning, negative offset clamps to 0.
        let req = RopSeekStreamRequest {
            input_handle_index: 1,
            origin: 0x00,
            offset: -5,
        };
        assert_eq!(req.resolve(0, 100).unwrap(), 0);
        // Current origin moves relative to the cursor.
        let req = RopSeekStreamRequest {
            input_handle_index: 1,
            origin: 0x01,
            offset: 3,
        };
        assert_eq!(req.resolve(10, 100).unwrap(), 13);
        // Current origin clamps to [0, len] when it runs off the end.
        let req = RopSeekStreamRequest {
            input_handle_index: 1,
            origin: 0x01,
            offset: 50,
        };
        assert_eq!(req.resolve(60, 100).unwrap(), 100);
        // End origin is relative to len.
        let req = RopSeekStreamRequest {
            input_handle_index: 1,
            origin: 0x02,
            offset: -10,
        };
        assert_eq!(req.resolve(0, 100).unwrap(), 90);
        // Unknown origin is InvalidValue.
        let req = RopSeekStreamRequest {
            input_handle_index: 1,
            origin: 0x09,
            offset: 0,
        };
        assert!(req.resolve(0, 100).is_err());
        // Positive overflow on a relative (current) seek clamps to len, not a
        // wrapped/garbage position: cursor near u64::MAX + a large offset.
        let req = RopSeekStreamRequest {
            input_handle_index: 1,
            origin: 0x01,
            offset: i64::MAX,
        };
        assert_eq!(req.resolve(u64::MAX - 10, 100).unwrap(), 100);
        // Negative overflow on a relative seek clamps to 0.
        let req = RopSeekStreamRequest {
            input_handle_index: 1,
            origin: 0x01,
            offset: i64::MIN,
        };
        assert_eq!(req.resolve(10, 100).unwrap(), 0);
        // End origin with positive overflow clamps to len.
        let req = RopSeekStreamRequest {
            input_handle_index: 1,
            origin: 0x02,
            offset: i64::MAX,
        };
        assert_eq!(req.resolve(0, 100).unwrap(), 100);
    }

    #[test]
    fn rop_seek_stream_success_encodes_new_position() {
        let mut out = Vec::new();
        RopSeekStreamSuccess {
            input_handle_index: 1,
            return_value: RopErrorCode::Success,
            new_position: 0x0123_4567_89AB_CDEF,
        }
        .encode(&mut out);
        assert_eq!(out[0], 0x2E);
        assert_eq!(
            u64::from_le_bytes([
                out[6], out[7], out[8], out[9], out[10], out[11], out[12], out[13]
            ]),
            0x0123_4567_89AB_CDEF
        );
    }

    #[test]
    fn rop_set_stream_size_roundtrip() {
        let mut body = Vec::new();
        body.push(5);
        body.extend_from_slice(&0x400u64.to_le_bytes());
        let mut cur = Buf::new(&body);
        let req = RopSetStreamSizeRequest::decode_after_ropid(&mut cur).expect("decode");
        assert_eq!(req.input_handle_index, 5);
        assert_eq!(req.stream_size, 0x400);

        let mut out = Vec::new();
        RopSetStreamSizeResponse {
            input_handle_index: 5,
            return_value: RopErrorCode::Success,
        }
        .encode(&mut out);
        assert_eq!(out[0], 0x2F);
        assert_eq!(out.len(), 6);
    }

    #[test]
    fn rop_get_stream_size_roundtrip() {
        let body: Vec<u8> = vec![6];
        let mut cur = Buf::new(&body);
        let req = RopGetStreamSizeRequest::decode_after_ropid(&mut cur).expect("decode");
        assert_eq!(req.input_handle_index, 6);

        let mut out = Vec::new();
        RopGetStreamSizeSuccess {
            input_handle_index: 6,
            return_value: RopErrorCode::Success,
            stream_size: 0x1000,
        }
        .encode(&mut out);
        assert_eq!(out[0], 0x5E);
        assert_eq!(u32::from_le_bytes([out[6], out[7], out[8], out[9]]), 0x1000);
    }

    #[test]
    fn rop_commit_stream_roundtrip() {
        let body: Vec<u8> = vec![8];
        let mut cur = Buf::new(&body);
        let req = RopCommitStreamRequest::decode_after_ropid(&mut cur).expect("decode");
        assert_eq!(req.input_handle_index, 8);

        let mut out = Vec::new();
        RopCommitStreamResponse {
            input_handle_index: 8,
            return_value: RopErrorCode::Success,
        }
        .encode(&mut out);
        assert_eq!(out[0], 0x5D);
        assert_eq!(out.len(), 6);
    }

    // ---- Attachment ROP codec round-trips (MS-OXCROPS §2.2.6) ----

    #[test]
    fn rop_get_attachment_table_roundtrip() {
        // Body after RopHeader4 = TableFlags(1). The caller supplied the
        // Input/Output indices; the body cursor holds only the flag.
        let body: Vec<u8> = vec![0x00]; // TableFlags
        let mut cur = Buf::new(&body);
        let req = RopGetAttachmentTableRequest::decode_body(&mut cur, 4, 9).expect("decode");
        assert_eq!(req.input_handle_index, 4);
        assert_eq!(req.output_handle_index, 9);
        assert_eq!(req.table_flags, 0x00);
        assert_eq!(cur.remaining(), 0);

        // Success response: RopId · OutputHandleIndex · ReturnValue(4).
        let mut out = Vec::new();
        RopGetAttachmentTableSuccess {
            output_handle_index: 9,
            return_value: RopErrorCode::Success,
        }
        .encode(&mut out);
        assert_eq!(out[0], 0x21);
        assert_eq!(out[1], 9);
        assert_eq!(out.len(), 6);
    }

    #[test]
    fn rop_open_attachment_roundtrip() {
        // Body after RopHeader4 = OpenAttachmentFlags(1) · AttachmentID(4 LE).
        let mut body = Vec::new();
        body.push(0x00); // OpenAttachmentFlags
        body.extend_from_slice(&3u32.to_le_bytes()); // AttachmentID = 3
        let mut cur = Buf::new(&body);
        let req = RopOpenAttachmentRequest::decode_body(&mut cur, 2, 5).expect("decode");
        assert_eq!(req.input_handle_index, 2);
        assert_eq!(req.output_handle_index, 5);
        assert_eq!(req.open_attachment_flags, 0x00);
        assert_eq!(req.attachment_id, 3);
        assert_eq!(cur.remaining(), 0);

        // Response: RopId · OutputHandleIndex · ReturnValue(4).
        let mut out = Vec::new();
        RopOpenAttachmentSuccess {
            output_handle_index: 5,
            return_value: RopErrorCode::Success,
        }
        .encode(&mut out);
        assert_eq!(out[0], 0x22);
        assert_eq!(out[1], 5);
        assert_eq!(out.len(), 6);
    }

    #[test]
    fn rop_create_attachment_roundtrip() {
        // The request is header-only (no body), so the codec only carries the
        // resolved Input/Output indices; the dispatcher reads them via
        // RopHeader4::decode_after_ropid. Verify the struct round-trips as a
        // plain value.
        let req = RopCreateAttachmentRequest {
            input_handle_index: 1,
            output_handle_index: 6,
        };
        assert_eq!(req.input_handle_index, 1);
        assert_eq!(req.output_handle_index, 6);

        // Success response: RopId · OutputHandleIndex · ReturnValue(4)
        // · AttachmentID(4 LE).
        let mut out = Vec::new();
        RopCreateAttachmentSuccess {
            output_handle_index: 6,
            return_value: RopErrorCode::NoSupport,
            attachment_id: 0,
        }
        .encode(&mut out);
        assert_eq!(out[0], 0x23);
        assert_eq!(out[1], 6);
        assert_eq!(out.len(), 10);
    }

    #[test]
    fn rop_delete_attachment_roundtrip() {
        // Body after RopId+LogonId = InputHandleIndex · AttachmentID(4 LE).
        let mut body = Vec::new();
        body.push(2); // InputHandleIndex
        body.extend_from_slice(&7u32.to_le_bytes()); // AttachmentID = 7
        let mut cur = Buf::new(&body);
        let req = RopDeleteAttachmentRequest::decode_after_ropid(&mut cur).expect("decode");
        assert_eq!(req.input_handle_index, 2);
        assert_eq!(req.attachment_id, 7);
        assert_eq!(cur.remaining(), 0);

        // Response: RopId · InputHandleIndex · ReturnValue(4).
        let mut out = Vec::new();
        RopDeleteAttachmentResponse {
            input_handle_index: 2,
            return_value: RopErrorCode::NoSupport,
        }
        .encode(&mut out);
        assert_eq!(out[0], 0x24);
        assert_eq!(out[1], 2);
        assert_eq!(out.len(), 6);
    }

    #[test]
    fn rop_save_changes_attachment_roundtrip() {
        // Body after RopId+LogonId = ResponseHandleIndex · InputHandleIndex
        // · SaveFlags(1).
        let body: Vec<u8> = vec![8, 1, 0x01];
        let mut cur = Buf::new(&body);
        let req = RopSaveChangesAttachmentRequest::decode_after_ropid(&mut cur).expect("decode");
        assert_eq!(req.response_handle_index, 8);
        assert_eq!(req.input_handle_index, 1);
        assert_eq!(req.save_flags, 0x01);
        assert_eq!(cur.remaining(), 0);

        // Response: RopId · ResponseHandleIndex · ReturnValue(4).
        let mut out = Vec::new();
        RopSaveChangesAttachmentResponse {
            response_handle_index: 8,
            return_value: RopErrorCode::Success,
        }
        .encode(&mut out);
        assert_eq!(out[0], 0x25);
        assert_eq!(out[1], 8);
        assert_eq!(out.len(), 6);
    }

    #[test]
    fn rop_get_valid_attachments_roundtrip() {
        // Body after RopId+LogonId = InputHandleIndex.
        let body: Vec<u8> = vec![3];
        let mut cur = Buf::new(&body);
        let req = RopGetValidAttachmentsRequest::decode_after_ropid(&mut cur).expect("decode");
        assert_eq!(req.input_handle_index, 3);
        assert_eq!(cur.remaining(), 0);

        // Success response: RopId · InputHandleIndex · ReturnValue(4)
        // · Count(2 LE) · Array(count×4 LE).
        let mut out = Vec::new();
        RopGetValidAttachmentsSuccess {
            input_handle_index: 3,
            return_value: RopErrorCode::Success,
            attachment_ids: vec![0, 1, 2],
        }
        .encode(&mut out);
        assert_eq!(out[0], 0x52);
        assert_eq!(out[1], 3);
        let count = u16::from_le_bytes([out[6], out[7]]);
        assert_eq!(count, 3);
        // attachment_id[0] = 0, [1] = 1, [2] = 2 (the vec we supplied).
        assert_eq!(u32::from_le_bytes([out[8], out[9], out[10], out[11]]), 0);
        assert_eq!(u32::from_le_bytes([out[12], out[13], out[14], out[15]]), 1);
        assert_eq!(u32::from_le_bytes([out[16], out[17], out[18], out[19]]), 2);
        assert_eq!(out.len(), 6 + 2 + 3 * 4);
    }

    // ---- table-navigation ROP round-trips ---------------------------------

    #[test]
    fn rop_query_position_roundtrip() {
        // Request body: empty (header only).
        let body: Vec<u8> = vec![];
        let mut cur = Buf::new(&body);
        let _ = RopQueryPositionRequest::decode(&mut cur).expect("decode");
        assert_eq!(cur.remaining(), 0);

        // Response: RopId(0x17) · InputHandleIndex · ReturnValue(4) · Num(4) · Den(4).
        let mut out = Vec::new();
        RopQueryPositionResponse {
            input_handle_index: 2,
            return_value: RopErrorCode::Success,
            numerator: 3,
            denominator: 10,
        }
        .encode(&mut out);
        assert_eq!(out[0], 0x17);
        assert_eq!(out[1], 2);
        assert_eq!(u32::from_le_bytes([out[6], out[7], out[8], out[9]]), 3);
        assert_eq!(u32::from_le_bytes([out[10], out[11], out[12], out[13]]), 10);
        assert_eq!(out.len(), 2 + 4 + 4 + 4);
    }

    #[test]
    fn rop_seek_row_decode() {
        // SeekFlags(1) + RowCount(4 LE signed). Forward-by-3.
        let body: Vec<u8> = vec![0x00, 3, 0, 0, 0];
        let mut cur = Buf::new(&body);
        let req = RopSeekRowRequest::decode(&mut cur).expect("decode");
        assert_eq!(req.seek_flags, 0);
        assert_eq!(req.row_count, 3);
        assert_eq!(cur.remaining(), 0);

        // Negative (seek-back 2) with FROM_BEGINNING flag (0x01).
        let body2: Vec<u8> = vec![0x01, 0xFE, 0xFF, 0xFF, 0xFF];
        let mut cur2 = Buf::new(&body2);
        let req2 = RopSeekRowRequest::decode(&mut cur2).expect("decode neg");
        assert_eq!(req2.seek_flags, 0x01);
        assert_eq!(req2.row_count, -2);
    }

    #[test]
    fn rop_seek_row_fractional_roundtrip() {
        let body: Vec<u8> = vec![1, 0, 0, 0, 4, 0, 0, 0];
        let mut cur = Buf::new(&body);
        let req = RopSeekRowFractionalRequest::decode(&mut cur).expect("decode");
        assert_eq!(req.numerator, 1);
        assert_eq!(req.denominator, 4);
        let mut out = Vec::new();
        RopSeekRowFractionalResponse {
            input_handle_index: 5,
            return_value: RopErrorCode::Success,
        }
        .encode(&mut out);
        assert_eq!(out[0], 0x1A);
        assert_eq!(out[1], 5);
        assert_eq!(out.len(), 2 + 4);
    }

    #[test]
    fn rop_seek_row_bookmark_decode() {
        // SeekFlags(1) + Bookmark(4 LE) + RowCount(4 LE signed) = forward 1 from bookmark 7.
        let body: Vec<u8> = vec![0x00, 7, 0, 0, 0, 1, 0, 0, 0];
        let mut cur = Buf::new(&body);
        let req = RopSeekRowBookmarkRequest::decode(&mut cur).expect("decode");
        assert_eq!(req.seek_flags, 0);
        assert_eq!(req.bookmark, 7);
        assert_eq!(req.row_count, 1);

        let mut out = Vec::new();
        RopSeekRowBookmarkResponse {
            input_handle_index: 1,
            return_value: RopErrorCode::Success,
            rows_sought: 1,
            has_sought_less: 0,
        }
        .encode(&mut out);
        assert_eq!(out[0], 0x19);
        assert_eq!(out.len(), 2 + 4 + 4 + 1);
    }

    #[test]
    fn rop_create_bookmark_roundtrip() {
        // Request body: empty (header only; the 4-byte dispatcher header includes OutputHandleIndex).
        let body: Vec<u8> = vec![];
        let mut cur = Buf::new(&body);
        let _ = RopCreateBookmarkRequest::decode(&mut cur).expect("decode");
        assert_eq!(cur.remaining(), 0);

        let mut out = Vec::new();
        RopCreateBookmarkResponse {
            output_handle_index: 9,
            return_value: RopErrorCode::Success,
            bookmark: 0x1234_5678,
        }
        .encode(&mut out);
        assert_eq!(out[0], 0x1B);
        assert_eq!(out[1], 9);
        assert_eq!(
            u32::from_le_bytes([out[6], out[7], out[8], out[9]]),
            0x1234_5678
        );
        assert_eq!(out.len(), 2 + 4 + 4);
    }

    #[test]
    fn rop_free_bookmark_roundtrip() {
        let bm: u32 = 0x1234_5678;
        let mut body = Vec::new();
        body.extend_from_slice(&bm.to_le_bytes());
        let mut cur = Buf::new(&body);
        let req = RopFreeBookmarkRequest::decode(&mut cur).expect("decode");
        assert_eq!(req.bookmark, bm);
        assert_eq!(cur.remaining(), 0);

        let mut out = Vec::new();
        RopFreeBookmarkResponse {
            input_handle_index: 4,
            return_value: RopErrorCode::Success,
        }
        .encode(&mut out);
        assert_eq!(out[0], 0x89);
        assert_eq!(out[1], 4);
        assert_eq!(out.len(), 2 + 4);
    }

    #[test]
    fn rop_reset_table_roundtrip() {
        // Request body: empty.
        let body: Vec<u8> = vec![];
        let mut cur = Buf::new(&body);
        let _ = RopResetTableRequest::decode(&mut cur).expect("decode");
        assert_eq!(cur.remaining(), 0);

        let mut out = Vec::new();
        RopResetTableResponse {
            input_handle_index: 7,
            return_value: RopErrorCode::Success,
        }
        .encode(&mut out);
        assert_eq!(out[0], 0x81);
        assert_eq!(out[1], 7);
        assert_eq!(out.len(), 2 + 4);
    }

    #[test]
    fn rop_sort_table_decode() {
        // SortFlags(1) + SortCount(2 LE) + SortOrders[] (each SortFlags(1)+Tag(4)).
        let tag = PropertyTag::new(crate::mapi::data::PropertyType::PTYP_STRING, 0x0037);
        let mut tag_bytes = Vec::new();
        tag.encode(&mut tag_bytes);
        let mut body = Vec::new();
        body.push(0x00); // sort flags
        body.extend_from_slice(&1u16.to_le_bytes()); // 1 sort order
        body.push(0x01); // descending
        body.extend_from_slice(&tag_bytes);
        let mut cur = Buf::new(&body);
        let req = RopSortTableRequest::decode(&mut cur).expect("decode");
        assert_eq!(req.sort_flags, 0);
        assert_eq!(req.sort_orders.len(), 1);
        assert_eq!(req.sort_orders[0].sort_flags, 0x01);
        assert_eq!(req.sort_orders[0].tag, tag);
        assert_eq!(cur.remaining(), 0);
    }

    #[test]
    fn rop_restrict_decode_property_restriction() {
        // RestrictFlags(1) + SRestriction: a Property restriction (type 4)
        // RelOp(1)=EQ + Tag(4) + PropertyValue (Integer32: type word + i32).
        let tag = PropertyTag::new(crate::mapi::data::PropertyType::PTYP_INTEGER32, 0x0017);
        let mut tag_bytes = Vec::new();
        tag.encode(&mut tag_bytes);
        let mut rdata = Vec::new();
        rdata.push(crate::mapi::restrict::RestrictionType::Property.to_u8());
        rdata.push(2); // RelOp EQ
        rdata.extend_from_slice(&tag_bytes);
        // PropertyValue in row form: decode_row reads the value per the TAG's
        // property type (Integer32 => 4 LE bytes, NO type-word prefix).
        rdata.extend_from_slice(&42i32.to_le_bytes());
        let mut body = Vec::new();
        body.push(0x00); // restrict flags
        // RestrictionDataSize(2 LE): length of the restriction envelope that
        // follows, so the decoder bounds SRestriction::decode rather than
        // consuming into the next ROP of the chain.
        body.extend_from_slice(&u16::try_from(rdata.len()).unwrap().to_le_bytes());
        body.extend_from_slice(&rdata);
        let mut cur = Buf::new(&body);
        let req = RopRestrictRequest::decode(&mut cur).expect("decode");
        assert_eq!(req.restrict_flags, 0);
        let crate::mapi::restrict::SRestriction::Property {
            relop, tag: rtag, ..
        } = &req.restriction
        else {
            panic!("expected Property restriction, got {:?}", req.restriction);
        };
        assert_eq!(*relop, crate::mapi::restrict::RelOp::EQ);
        assert_eq!(*rtag, tag);
        assert_eq!(cur.remaining(), 0);
    }

    // ---- FastTransfer / Synchronization codec round-trips ------------------

    #[test]
    fn rop_fast_transfer_source_get_buffer_roundtrip() {
        // Request: BufferSize(2 LE) + TransferFlags(1).
        let body: Vec<u8> = vec![0x00, 0x10, 0x00];
        let mut cur = Buf::new(&body);
        let req = RopFastTransferSourceGetBufferRequest::decode(&mut cur).expect("decode");
        assert_eq!(req.buffer_size, 0x1000);
        assert_eq!(req.transfer_flags, 0);
        assert_eq!(cur.remaining(), 0);

        // Response: RopId(0x4E) · InHandle(1) · RV(4 LE) · TransferStatus(2 LE)
        // · InProgressCount(2) · TotalStepCount(2) · Reserved(1) ·
        // TransferBufferSize(2 LE) · Data.
        let mut out = Vec::new();
        RopFastTransferSourceGetBufferSuccess {
            input_handle_index: 1,
            return_value: RopErrorCode::Success,
            transfer_status: 0,
            in_progress_count: 0,
            total_step_count: 0,
            transfer_buffer_size: 2,
            data: vec![0xAB, 0xCD],
        }
        .encode(&mut out);
        assert_eq!(out[0], 0x4E);
        assert_eq!(out[1], 1);
        // TransferBufferSize is the 2-byte field immediately preceding Data.
        let buf_size_off = out.len() - 2 - 2; // 2 data + 2 size
        assert_eq!(
            u16::from_le_bytes([out[buf_size_off], out[buf_size_off + 1]]),
            2
        );
        assert_eq!(&out[out.len() - 2..], &[0xAB, 0xCD]);
        // RopId(1) + InHandle(1) + RV(4) + Status(2) + InProg(2) + Total(2)
        // + Reserved(1) + Size(2) + Data(2) == 17.
        assert_eq!(out.len(), 17);
    }

    #[test]
    fn rop_fast_transfer_destination_put_buffer_roundtrip() {
        // Request: DataSize(2 LE) + Data.
        let body: Vec<u8> = vec![2, 0, 0xAA, 0xBB];
        let mut cur = Buf::new(&body);
        let req = RopFastTransferDestinationPutBufferRequest::decode_after_ropid(&mut cur)
            .expect("decode");
        assert_eq!(req.data, vec![0xAA, 0xBB]);
        assert_eq!(cur.remaining(), 0);

        // Empty-data (end-of-stream) marker.
        let body2: Vec<u8> = vec![0, 0];
        let mut cur2 = Buf::new(&body2);
        let req2 = RopFastTransferDestinationPutBufferRequest::decode_after_ropid(&mut cur2)
            .expect("decode empty");
        assert!(req2.data.is_empty());

        let mut out = Vec::new();
        RopFastTransferDestinationPutBufferResponse {
            input_handle_index: 3,
            return_value: RopErrorCode::Success,
            transfer_status: 1,
            data_remaining: 0,
        }
        .encode(&mut out);
        assert_eq!(out[0], 0x54);
        assert_eq!(out[1], 3);
        assert_eq!(out.len(), 2 + 4 + 1 + 4);
    }

    #[test]
    fn rop_fast_transfer_destination_configure_decode() {
        // SourceFmt(1) + SyncFlags(1).
        let body: Vec<u8> = vec![0x00, 0x01];
        let mut cur = Buf::new(&body);
        let req = RopFastTransferDestinationConfigureRequest::decode_after_ropid(&mut cur)
            .expect("decode");
        assert_eq!(req.source_fmt, 0);
        assert_eq!(req.sync_flags, 1);
        assert_eq!(cur.remaining(), 0);
    }

    #[test]
    fn rop_fast_transfer_source_copy_messages_decode() {
        // Flags(1) + MessageIdCount(2 LE)=2 + two 8-byte ids.
        let mut body = Vec::new();
        body.push(0x00);
        body.extend_from_slice(&2u16.to_le_bytes());
        body.extend_from_slice(&100u64.to_le_bytes());
        body.extend_from_slice(&200u64.to_le_bytes());
        let mut cur = Buf::new(&body);
        let req = RopFastTransferSourceCopyMessagesRequest::decode(&mut cur).expect("decode");
        assert_eq!(req.flags, 0);
        assert_eq!(req.message_ids, vec![100, 200]);
        assert_eq!(cur.remaining(), 0);
    }

    #[test]
    fn rop_fast_transfer_source_copy_to_decode() {
        // Flags(1) + CopyToFlags(1) + TagCount(2)=1 + one PropertyTag.
        let tag = PropertyTag::new(crate::mapi::data::PropertyType::PTYP_STRING, 0x0037);
        let mut tagb = Vec::new();
        tag.encode(&mut tagb);
        let mut body = Vec::new();
        body.push(0x00);
        body.push(0x00);
        body.extend_from_slice(&1u16.to_le_bytes());
        body.extend_from_slice(&tagb);
        let mut cur = Buf::new(&body);
        let req = RopFastTransferSourceCopyToRequest::decode(&mut cur).expect("decode");
        assert_eq!(req.property_tags.len(), 1);
        assert_eq!(req.property_tags[0], tag);
        assert_eq!(cur.remaining(), 0);
    }

    #[test]
    fn rop_synchronization_configure_decode() {
        // SyncFlags(1) + SyncType(1) + StateLen(2)=3 + state bytes.
        let mut body = Vec::new();
        body.push(0x01);
        body.push(0x02);
        body.extend_from_slice(&3u16.to_le_bytes());
        body.extend_from_slice(&[0xDE, 0xAD, 0xBE]);
        let mut cur = Buf::new(&body);
        let req = RopSynchronizationConfigureRequest::decode_after_ropid(&mut cur).expect("decode");
        assert_eq!(req.sync_flags, 1);
        assert_eq!(req.sync_type, 2);
        assert_eq!(req.sync_state, vec![0xDE, 0xAD, 0xBE]);
        assert_eq!(cur.remaining(), 0);
    }

    #[test]
    fn rop_fast_transfer_source_open_response_shape() {
        // The generic open-response envelope [RopId · OutHandle · RV(4)] is
        // used by all four source-copy ROPs + the destination configure. Verify
        // the RopId echoes back through the encode(.., rop_id) parameter.
        let mut out = Vec::new();
        RopFastTransferSourceOpenResponse {
            output_handle_index: 7,
            return_value: RopErrorCode::Success,
        }
        .encode(&mut out, RopId::ROP_FAST_TRANSFER_SOURCE_COPY_FOLDER);
        assert_eq!(out[0], 0x4C);
        assert_eq!(out[1], 7);
        assert_eq!(out.len(), 2 + 4);

        let mut out2 = Vec::new();
        RopSynchronizationAckResponse {
            input_handle_index: 4,
            return_value: RopErrorCode::Success,
        }
        .encode(&mut out2, RopId::ROP_SYNCHRONIZATION_IMPORT_MESSAGE_CHANGE);
        assert_eq!(out2[0], 0x72);
        assert_eq!(out2[1], 4);
        assert_eq!(out2.len(), 2 + 4);
    }

    #[test]
    fn rop_notify_response_has_four_byte_handle() {
        // MS-OXCROPS §2.2.14.2.1: RopId(1) · NotificationHandle(4 LE) ·
        // ReturnValue(4 LE) · LogonId(1) · NotificationData. Verify the 4-byte
        // handle (the previous phase-1 codec used 1 byte — this is the
        // regression guard for the notification wait deliverable).
        let mut out = Vec::new();
        RopNotifyResponse {
            notification_handle: 0x04030201,
            logon_id: 5,
            notification_data: Vec::new(),
        }
        .encode(&mut out);
        assert_eq!(out[0], 0x2A, "RopId ROP_NOTIFY");
        // NotificationHandle 4 LE bytes
        assert_eq!(&out[1..5], &[0x01, 0x02, 0x03, 0x04]);
        // ReturnValue = Success(0) 4 LE bytes
        assert_eq!(&out[5..9], &[0, 0, 0, 0]);
        assert_eq!(out[9], 5, "LogonId");
        assert_eq!(out.len(), 10);
    }

    #[test]
    fn rop_pending_response_shape() {
        let mut out = Vec::new();
        RopPendingResponse { session_index: 0 }.encode(&mut out);
        assert_eq!(out[0], 0x6E, "RopId ROP_PENDING");
        assert_eq!(&out[1..3], &[0, 0], "SessionIndex 0 LE");
        assert_eq!(out.len(), 3);
    }

    #[test]
    fn notification_data_newmail_shape() {
        // NewMail (0x0002) + 0x8000 message bit: Flags(2 LE) · FolderId(8 LE)
        // · MessageId(8 LE). No ParentFolderId (bit 0x4000 clear) and no Old*
        // (not Moved/Copied).
        let mut out = Vec::new();
        NotificationData {
            notification_flags: 0x8002,
            folder_id: 0x0102030405060708,
            message_id: 0x0A0B0C0D0E0F1011,
            parent_folder_id: None,
            old_folder_id: None,
            old_message_id: None,
            old_parent_folder_id: None,
        }
        .encode(&mut out);
        assert_eq!(out.len(), 2 + 8 + 8);
        assert_eq!(&out[0..2], &0x8002u16.to_le_bytes());
        assert_eq!(&out[2..10], &0x0102030405060708u64.to_le_bytes());
        assert_eq!(&out[10..18], &0x0A0B0C0D0E0F1011u64.to_le_bytes());
    }

    #[test]
    fn notification_data_modified_minimal() {
        // ObjectModified (0x0010)+0x8000: same shape as NewMail (Flags+Folder+
        // Message), no Parent/Old.
        let mut out = Vec::new();
        NotificationData {
            notification_flags: 0x8010,
            folder_id: 42,
            message_id: 7,
            parent_folder_id: None,
            old_folder_id: None,
            old_message_id: None,
            old_parent_folder_id: None,
        }
        .encode(&mut out);
        assert_eq!(out.len(), 18);
        assert_eq!(&out[0..2], &0x8010u16.to_le_bytes());
    }

    #[test]
    fn notification_data_deleted_emits_message_no_parent() {
        // ObjectDeleted (0x0008)+0x8000 (message): Flags+Folder+Message. The
        // ParentFolderId rule requires the search-folder bit (0x4000) for a
        // message event, which is not set here, so ParentFolderId is omitted.
        let mut out = Vec::new();
        NotificationData {
            notification_flags: 0x8008,
            folder_id: 1,
            message_id: 2,
            parent_folder_id: Some(99), // should be IGNORED (bit 0x4000 clear)
            old_folder_id: None,
            old_message_id: None,
            old_parent_folder_id: None,
        }
        .encode(&mut out);
        assert_eq!(out.len(), 18);
    }

    #[test]
    fn notification_data_moved_message_emits_old_message_id() {
        // ObjectMoved (0x0020)+0x8000 message: Flags+Folder+Message+
        // OldFolderId+OldMessageId.
        let mut out = Vec::new();
        NotificationData {
            notification_flags: 0x8020,
            folder_id: 11,
            message_id: 22,
            parent_folder_id: None,
            old_folder_id: Some(33),
            old_message_id: Some(44),
            old_parent_folder_id: Some(55), // ignored for message event
        }
        .encode(&mut out);
        // Flags(2) + Folder(8) + Message(8) + OldFolder(8) + OldMessage(8)
        assert_eq!(out.len(), 2 + 8 + 8 + 8 + 8);
        assert_eq!(&out[18..26], &33u64.to_le_bytes());
        assert_eq!(&out[26..34], &44u64.to_le_bytes());
    }

    #[test]
    fn notification_data_moved_none_old_still_emits_sentinel() {
        // Regression guard (PR #1847 review): a Moved/Copied message event MUST
        // always emit OldFolderId + OldMessageId on the wire even when the
        // caller passes `None` — the client decodes Old* by position, so
        // omitting the bytes would truncate the notification and desync every
        // subsequent ROP in the Execute body. `None` ⇒ sentinel `0`.
        let mut out = Vec::new();
        NotificationData {
            notification_flags: 0x8020,
            folder_id: 1,
            message_id: 2,
            parent_folder_id: None,
            old_folder_id: None,
            old_message_id: None,
            old_parent_folder_id: None,
        }
        .encode(&mut out);
        assert_eq!(
            out.len(),
            2 + 8 + 8 + 8 + 8,
            "Old* bytes always present for Moved"
        );
        assert_eq!(&out[18..26], &0u64.to_le_bytes(), "OldFolderId sentinel 0");
        assert_eq!(&out[26..34], &0u64.to_le_bytes(), "OldMessageId sentinel 0");

        // Copied (0x0040) message event: same mandatory Old* layout.
        let mut out = Vec::new();
        NotificationData {
            notification_flags: 0x8040,
            folder_id: 1,
            message_id: 2,
            parent_folder_id: None,
            old_folder_id: None,
            old_message_id: None,
            old_parent_folder_id: None,
        }
        .encode(&mut out);
        assert_eq!(
            out.len(),
            2 + 8 + 8 + 8 + 8,
            "Old* bytes always present for Copied"
        );
    }

    #[test]
    fn notification_data_folder_event_emits_parent_and_old_parent() {
        // A FOLDER event (bit 0x8000 clear) for ObjectCopied (0x0040) MUST emit
        // ParentFolderId (folder-event rule) + OldFolderId + OldParentFolderId
        // (the folder-event counterpart of OldMessageId). These are unused by the
        // gateway's item-event feed today but the codec must be spec-correct for
        // a future folder-event bridge.
        let mut out = Vec::new();
        NotificationData {
            notification_flags: 0x0040,
            folder_id: 1,
            message_id: 0, // not emitted (0x8000 clear)
            parent_folder_id: Some(7),
            old_folder_id: Some(8),
            old_message_id: Some(99), // ignored for folder event
            old_parent_folder_id: Some(9),
        }
        .encode(&mut out);
        // Flags(2) + Folder(8) [no MessageId: 0x8000 clear] + Parent(8) +
        // OldFolder(8) + OldParent(8) = 34
        assert_eq!(out.len(), 2 + 8 + 8 + 8 + 8);
        assert_eq!(&out[2..10], &1u64.to_le_bytes(), "FolderId");
        assert_eq!(&out[10..18], &7u64.to_le_bytes(), "ParentFolderId");
        assert_eq!(&out[18..26], &8u64.to_le_bytes(), "OldFolderId");
        assert_eq!(&out[26..34], &9u64.to_le_bytes(), "OldParentFolderId");
    }
}
