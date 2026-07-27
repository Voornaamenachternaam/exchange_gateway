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

use crate::mapi::data::{PropertyProblem, PropertyTag,
    TaggedPropertyValue, PropertyValue
};

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
    pub fn take_bytes(&mut self, n: usize) -> Result<&'a [u8], DecodeError> {
        if self.remaining() < n {
            return Err(DecodeError::Insufficient);
        }
        let out = &self.buf[self.pos..self.pos + n];
        self.pos += n;
        Ok(out)
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

/// `RopNotify` response, MS-OXCROPS §2.2.14.2.1. The gateway emits an empty
/// notification slot here (no events pending in phase 1) so the transport
/// round-trips correctly; phase 2 fills `NotificationData`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RopNotifyResponse {
    pub notification_handle: u8,
    pub logon_id: u8,
    pub notification_data: Vec<u8>,
}
impl RopNotifyResponse {
    pub fn encode(&self, out: &mut Vec<u8>) {
        out.push(RopId::ROP_NOTIFY.to_u8());
        out.push(self.notification_handle);
        out.extend_from_slice(&RopErrorCode::Success.to_u32().to_le_bytes());
        out.push(self.logon_id);
        out.extend_from_slice(&self.notification_data);
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

/// Small capacity hint used to size the rental Vec for the (usually tiny)
/// client-supplied property arrays. The decoding still honours the wire
/// count up to MAX_*; this constant only avoids a 0..1024 capacity for the
/// common small case.
struct SubGuard;
impl SubGuard {
    const TYPICAL: usize = 32;
}

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
        let mut values = Vec::with_capacity(count_us.min(SubGuard::TYPICAL));
        for _ in 0..count_us {
            let start = sub.position();
            let tv = TaggedPropertyValue::decode(&mut sub)?;
            // Push-back guard: a variable-length opaque/MV entry that the
            // typed decoder could not size must not silently advance the
            // sub-cursor to a wrong offset. An opaque entry that decoded to
            // zero payload bytes (unknown variable length) or any entry
            // that consumed no wire bytes would desynchronise the chain;
            // refuse it so the cursor fails closed rather than mis-stating
            // the apply.
            let opaque_empty = matches!(tv.value, PropertyValue::Opaque { ref bytes, .. } if bytes.is_empty());
            if opaque_empty || sub.position() == start {
                return Err(DecodeError::InvalidValue);
            }
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
        let count = u16::try_from(self.problems.len()).unwrap_or(u16::MAX);
        out.extend_from_slice(&count.to_le_bytes());
        for p in &self.problems {
            p.encode(out);
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
        let mut tags = Vec::with_capacity(count_us.min(SubGuard::TYPICAL));
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
        let mut excluded_tags = Vec::with_capacity(count_us.min(SubGuard::TYPICAL));
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
///   RopId + SourceHandleIndex + ReturnValue(4) + PropertyProblemCount(2)
///   + PropertyProblems (variable). The null-destination failure
///   (2.2.8.12.3, code 0x00000503) and the generic failure (2.2.8.12.4)
///   are emitted as a plain RopErrorResponse by the dispatcher; the clean
///   path uses this envelope with an empty problem array.
pub type RopCopyToSuccess = RopPropertyWriteSuccess;

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
}
