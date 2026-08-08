// src/mapi/tnef.rs
//
// Transport Neutral Encapsulation Format (TNEF) ŌĆö MS-OXTNEF reader & writer.
//
// Audit ┬¦2f.3: the gateway had no TNEF decode/encode, so meeting invites and
// voting buttons (which over SMTP/MAPI are produced/consumed as TNEF-wrapped
// named properties) lost their Outlook-specific richness even though the
// iMIP via SMTP path produced plain iCalendar. This module implements a
// parsing-fails-closed MS-OXTNEF reader (`parse`) and a determinism-faithful
// writer (`build`), plus the `PidTagTnefCorrelationKey` named property used
// to correlate a TNEF attachment back to the message ([MS-OXCMSG] ┬¦2.2.1.29).
//
// The module is deliberately I/O-free so it is fully unit-testable; the SMTP
// send path and the MAPI message-compose path hand it bytes (a winmail.dat
// blob) or a structured `TnefMessage` and receive the inverse. All multi-byte
// scalars are little-endian per MS-OXTNEF ┬¦2.1. Untrusted stream lengths use
// `usize::try_from` (no `as` casts on attacker-controlled values).

use thiserror::Error;

// ---------------------------------------------------------------------------
// Constants ([MS-OXTNEF] ┬¦2.1.3.2)
// ---------------------------------------------------------------------------

/// `TNEFSignature = %x78.9F.3E.22` -- the 4-byte LE magic opening every TNEF
/// stream (bytes 0x78 0x9F 0x3E 0x22 on the wire = 0x223E9F78 little-endian).
/// A reader rejects any stream that does not begin with these exact bytes
/// ([MS-OXTNEF] section 2.1.3.2).
pub const TNEF_SIGNATURE: u32 = 0x223E_9F78;

/// Default `LegacyKey` the writer emits. Per MS-OXTNEF section 2.1.3.2 ("Any
/// number will suffice here. This is now legacy."), the value is informational
/// only; real clients ignore it. We pick a stable non-zero value.
const LEGACY_KEY: u16 = 0x0002;

/// `attrLevelMessage = %x01`.
const LEVEL_MESSAGE: u8 = 0x01;
/// `attrLevelAttachment = %x02`.
const LEVEL_ATTACHMENT: u8 = 0x02;

/// TNEF version the writer advertises (`attTnefVersion`). Per [MS-OXTNEF]
/// section 2.1.3.3.1: "TNEF Writers MUST use %x00.00.01.00; TNEF Readers MUST
/// reject other values." The 4-byte data is the only wire-permitted value, so
/// we both emit and enforce it.
const TNEF_VERSION_DATA: [u8; 4] = [0x00, 0x00, 0x01, 0x00];

/// OEM primary code page the writer advertises (`attOemCodepage`). `1252`
/// (Windows-1252) is the conventional ANSI code page for attribute strings; the
/// secondary page is unused and zero ([MS-OXTNEF] ┬¦2.1.3.3.2).
const OEM_CODEPAGE_PRIMARY: u32 = 1252;
const OEM_CODEPAGE_SECONDARY: u32 = 0;

/// Hard upper bound on a single attribute's data length. The spec imposes no
/// explicit maximum, but Outlook attachment blobs are bounded by the message
/// size; cap at 64 MiB so a malformed length cannot drive an unbounded
/// allocation.
const MAX_ATTR_DATA: usize = 64 * 1024 * 1024;
/// Hard upper bound on the count of encapsulated properties / recipients /
/// named-property strings so a malicious count cannot loop the reader.
const MAX_PROP_COUNT: u32 = 1 << 20;
/// Hard upper bound on a single variable-length property value.
const MAX_PROP_VALUE: u32 = 64 * 1024 * 1024;

// Attribute IDs (the low 16 bits carry a type hint in the high byte; the IDs
// below are the full 32-bit LE values from ┬¦2.1.3.2).
mod id {
    pub const TNEF_VERSION: u32 = 0x00089006;
    pub const OEM_CODEPAGE: u32 = 0x00069007;
    pub const MESSAGE_CLASS: u32 = 0x00078008;
    pub const FROM: u32 = 0x00008000;
    pub const SUBJECT: u32 = 0x00018004;
    pub const DATE_SENT: u32 = 0x00038005;
    pub const DATE_RECD: u32 = 0x00038006;
    pub const MESSAGE_STATUS: u32 = 0x00068007;
    pub const MESSAGE_ID: u32 = 0x00018009;
    pub const BODY: u32 = 0x0002800C;
    pub const PRIORITY: u32 = 0x0004800D;
    pub const DATE_MODIFIED: u32 = 0x00038020;
    pub const MSG_PROPS: u32 = 0x00069003;
    pub const RECIP_TABLE: u32 = 0x00069004;
    pub const ORIGINAL_MESSAGE_CLASS: u32 = 0x00070600;
    pub const OWNER: u32 = 0x00060000;
    pub const SENT_FOR: u32 = 0x00060001;
    pub const DELEGATE: u32 = 0x00060002;
    pub const DATE_START: u32 = 0x00030006;
    pub const DATE_END: u32 = 0x00030007;
    pub const AID_OWNER: u32 = 0x00050008;
    pub const REQUEST_RES: u32 = 0x00040009;
    pub const ATTACH_DATA: u32 = 0x0006800F;
    pub const ATTACH_TITLE: u32 = 0x00018010;
    pub const ATTACH_META_FILE: u32 = 0x00068011;
    pub const ATTACH_CREATE_DATE: u32 = 0x00038012;
    pub const ATTACH_MODIFY_DATE: u32 = 0x00038013;
    pub const ATTACH_TRANSPORT_FILENAME: u32 = 0x00069001;
    pub const ATTACH_REND_DATA: u32 = 0x00069002;
    pub const ATTACHMENT: u32 = 0x00069005;
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// Parsing error for a malformed TNEF stream. The reader always fails closed:
/// a truncated/over-length/invalid stream yields `Err` rather than a partial
/// `TnefMessage`.
#[derive(Debug, Error)]
pub enum TnefError {
    #[error("truncated stream ({0})")]
    Truncated(&'static str),
    #[error("length {got} exceeds maximum {max}")]
    ExcessLength { got: u32, max: u32 },
    #[error("invalid TNEF signature 0x{0:08X}")]
    BadSignature(u32),
    #[error("invalid attribute level 0x{0:02X}")]
    BadLevel(u8),
    #[error("checksum mismatch: stored {stored:#06X} calculated {calculated:#06X}")]
    BadChecksum { stored: u16, calculated: u16 },
    #[error("invalid attribute id 0x{0:08X}")]
    BadAttrId(u32),
    #[error("invalid attTnefVersion data; expected 00 00 01 00")]
    BadVersion,
    #[error("invalid property type 0x{0:04X}")]
    BadPropType(u16),
    #[error("invalid UTF-8 in a TNEF string field")]
    BadUtf8,
    #[error("trailing bytes after the stream ({0} extra)")]
    Trailing(usize),
}

type Result<T> = std::result::Result<T, TnefError>;

// ---------------------------------------------------------------------------
// Abstract data model
// ---------------------------------------------------------------------------

/// A single address (sender / recipient / owner) in the TNEF `attFrom` /
/// `attOwner` formats: display name + the `TYPE:addr` email spec.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TnefAddress {
    pub display_name: String,
    pub address_type: String,
    pub email: String,
}

/// A TNEF attachment. The minimal set Outlook requires is a render-data row +
/// the binary payload + a filename; meeting/voting attachments add named
/// encapsulated properties via `props`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TnefAttachment {
    pub filename: String,
    pub transport_filename: String,
    /// Render position (`PidTagRenderingPosition`).
    pub render_position: i32,
    pub data: Vec<u8>,
    /// Encapsulated attachment properties (`attAttachment` payload).
    pub props: Vec<TnefProperty>,
}

/// An encapsulated property of a message (`attMsgProps`) or attachment
/// (`attAttachment`). The value is the typed scalar / variable payload per
/// MS-OXCDATA ┬¦2.11; named properties carry a non-empty `named` spec.
#[derive(Debug, Clone, PartialEq)]
pub struct TnefProperty {
    pub tag: crate::mapi::data::PropertyTag,
    /// Non-empty for named (id Ōēź 0x8000) properties.
    pub named: Option<NamedPropSpec>,
    pub value: TnefPropertyValue,
}

/// Named-property specification ([MS-OXTNEF] ┬¦2.1.3.4 `NamedPropSpec`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NamedPropSpec {
    /// `IDTypeNumber (0x00000000)` + a 32-bit numeric id.
    Numeric { guid: [u8; 16], id: u32 },
    /// `IDTypeString (0x00000000)` + a UTF-16LE string name.
    String { guid: [u8; 16], name: String },
}

/// A typed TNEF property value. Mirrors the subset of MS-OXCDATA ┬¦2.11 the
/// reader/writer round-trips; unknown wire types are preserved verbatim in
/// `Opaque` so a future caller can pass them through unchanged.
#[derive(Debug, Clone, PartialEq)]
pub enum TnefPropertyValue {
    Null,
    Boolean(bool),
    Integer16(i16),
    Integer32(i32),
    Integer64(i64),
    Floating32(f32),
    Floating64(f64),
    /// 64-bit FILETIME (100-ns ticks since 1601-01-01).
    Time(u64),
    /// 16-byte OLE GUID.
    Guid([u8; 16]),
    /// `PtypString` (UTF-16LE on the wire).
    String(String),
    /// `PtypString8` (ANSI/8-bit on the wire).
    String8(String),
    /// `PtypBinary`.
    Binary(Vec<u8>),
    /// `PtypMultiple*` value: the per-element wire bytes already serialised.
    Multi {
        element_type: u16,
        elements: Vec<Vec<u8>>,
    },
    /// Any wire type the typed decoders do not handle; carried verbatim so a
    /// re-encode round-trips the bytes without loss.
    Opaque { property_type: u16, bytes: Vec<u8> },
}

/// A parsed TNEF message ŌĆö the abstract model the writer serialises and the
/// reader produces.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TnefMessage {
    pub message_class: String,
    pub original_message_class: Option<String>,
    pub subject: String,
    pub body: Option<String>,
    pub message_id: Option<String>,
    pub priority: Option<u16>,
    pub sender: Option<TnefAddress>,
    /// Encapsulated message properties (`attMsgProps` payload). Meeting invites
    /// and voting buttons live here as named properties.
    pub props: Vec<TnefProperty>,
    pub attachments: Vec<TnefAttachment>,
    /// The `attDateSent` / `attDateRecd` / `attDateModified` DTR records, if
    /// present.
    pub date_sent: Option<Dtr>,
    pub date_received: Option<Dtr>,
    pub date_modified: Option<Dtr>,
}

/// A TNEF Date Time Record ([MS-OXTNEF] ┬¦2.1.3.3.4): 7 little-endian UINT16s.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Dtr {
    pub year: u16,
    pub month: u16,
    pub day: u16,
    pub hour: u16,
    pub minute: u16,
    pub second: u16,
    pub day_of_week: u16,
}

// ---------------------------------------------------------------------------
// Reader
// ---------------------------------------------------------------------------

/// A fail-closed little-endian cursor over a borrowed byte slice.
struct Cur<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Cur<'a> {
    fn new(buf: &'a [u8]) -> Self {
        Self { buf, pos: 0 }
    }
    fn remaining(&self) -> usize {
        self.buf.len().saturating_sub(self.pos)
    }
    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        if n > self.remaining() {
            return Err(TnefError::Truncated("attribute data"));
        }
        let s = &self.buf[self.pos..self.pos + n];
        self.pos += n;
        Ok(s)
    }
    fn u8(&mut self) -> Result<u8> {
        Ok(self.take(1)?[0])
    }
    fn u16(&mut self) -> Result<u16> {
        let b = self.take(2)?;
        Ok(u16::from_le_bytes([b[0], b[1]]))
    }
    fn i16(&mut self) -> Result<i16> {
        Ok(self.u16()? as i16)
    }
    fn u32(&mut self) -> Result<u32> {
        let b = self.take(4)?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }
    fn i32(&mut self) -> Result<i32> {
        Ok(self.u32()? as i32)
    }
    fn u64(&mut self) -> Result<u64> {
        let b = self.take(8)?;
        Ok(u64::from_le_bytes([
            b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7],
        ]))
    }
    fn f32(&mut self) -> Result<f32> {
        Ok(f32::from_bits(self.u32()?))
    }
    fn f64(&mut self) -> Result<f64> {
        Ok(f64::from_bits(self.u64()?))
    }
    fn i64(&mut self) -> Result<i64> {
        Ok(self.u64()? as i64)
    }
}

/// Parse a complete TNEF stream ([MS-OXTNEF] ┬¦2.1.3) into a `TnefMessage`.
/// Returns `Err` for any malformed/truncated/over-length input; never returns a
/// partially-populated message.
pub fn parse(bytes: &[u8]) -> Result<TnefMessage> {
    let mut cur = Cur::new(bytes);
    let mut msg = TnefMessage::default();

    // TNEFHeader = TNEFSignature LegacyKey
    let sig = cur.u32()?;
    if sig != TNEF_SIGNATURE {
        return Err(TnefError::BadSignature(sig));
    }
    let _legacy_key = cur.u16()?;

    // Mandatory leading attributes: TNEFVersion then OEMCodePage.
    let (lvl, attr_id, version_data) = read_attr(&mut cur)?;
    if lvl != LEVEL_MESSAGE || attr_id != id::TNEF_VERSION {
        return Err(TnefError::BadAttrId(attr_id));
    }
    // [MS-OXTNEF] section 2.1.3.3.1: "TNEF Readers MUST reject other values."
    // A fail-closed reader rejects any stream that does not carry the singular
    // wire-permitted `%x00.00.01.00` version data.
    if version_data != TNEF_VERSION_DATA {
        return Err(TnefError::BadVersion);
    }
    let (lvl, attr_id, data) = read_attr(&mut cur)?;
    if lvl != LEVEL_MESSAGE || attr_id != id::OEM_CODEPAGE {
        return Err(TnefError::BadAttrId(attr_id));
    }
    if data.len() < 8 {
        return Err(TnefError::Truncated("OEMCodePage"));
    }
    let _primary = u32::from_le_bytes([data[0], data[1], data[2], data[3]]);
    let _secondary = u32::from_le_bytes([data[4], data[5], data[6], data[7]]);

    // MessageData = *MessageAttribute [MessageProps], then *AttachData.
    // The stream is a flat sequence of (level, id, data) attributes; a
    // message-level `attAttachRendData` opens a new attachment block and any
    // subsequent attachment-level attributes append to it until the next
    // rend-data (or end of stream). Per ┬¦2.1.3 `attMsgProps` SHOULD be the last
    // message attribute, so we accept attributes in stream order without
    // enforcing strict sectioning.
    let mut current_attach: Option<TnefAttachment> = None;
    loop {
        if cur.remaining() == 0 {
            break;
        }
        let lvl = match cur.u8() {
            Ok(l) => l,
            Err(TnefError::Truncated(_)) => break,
            Err(e) => return Err(e),
        };
        match lvl {
            LEVEL_MESSAGE => {
                // Flush any in-flight attachment before a message-level block.
                if let Some(att) = current_attach.take() {
                    msg.attachments.push(att);
                }
                let attr_id = cur.u32()?;
                if attr_id == id::MSG_PROPS {
                    let data = read_attr_body_after_id(&mut cur)?;
                    apply_msg_props(&mut msg, &data)?;
                } else {
                    let data = read_attr_body_after_id(&mut cur)?;
                    apply_message_attr(&mut msg, attr_id, &data)?;
                }
            }
            LEVEL_ATTACHMENT => {
                let attr_id = cur.u32()?;
                let body = read_attr_body_after_id(&mut cur)?;
                if attr_id == id::ATTACH_REND_DATA {
                    // A new attachment block starts; flush the previous one.
                    if let Some(att) = current_attach.take() {
                        msg.attachments.push(att);
                    }
                    let mut att = TnefAttachment::default();
                    // attAttachRendData = AttachType(2) + AttachPosition(4) +
                    // RenderWidth(2) + RenderHeight(2) + DataFlags(4)
                    // ([MS-OXTNEF] section 2.1.3.3.15). Decode AttachPosition so
                    // a parsed blob's rendering placement survives a
                    // parse->build round-trip (writing 0 here would shift
                    // attachment placement).
                    if body.len() >= 6 {
                        let pos = i32::from_le_bytes([body[2], body[3], body[4], body[5]]);
                        att.render_position = pos;
                    }
                    current_attach = Some(att);
                } else if let Some(att) = current_attach.as_mut() {
                    apply_attach_attr(att, attr_id, &body)?;
                } else {
                    // An attachment attribute before any rend-data is malformed;
                    // tolerate it (no attachment to attach to) rather than fail.
                }
            }
            other => return Err(TnefError::BadLevel(other)),
        }
    }
    if let Some(att) = current_attach.take() {
        msg.attachments.push(att);
    }

    Ok(msg)
}

/// Read one full attribute `(level, id, data)` including the trailing checksum.
fn read_attr(cur: &mut Cur<'_>) -> Result<(u8, u32, Vec<u8>)> {
    let lvl = cur.u8()?;
    let attr_id = cur.u32()?;
    let data = read_attr_body_after_id(cur)?;
    Ok((lvl, attr_id, data))
}

/// Read the `Length Data Checksum` tail of an attribute (after the id has been
/// consumed). Returns the attribute data and verifies the checksum.
fn read_attr_body_after_id(cur: &mut Cur<'_>) -> Result<Vec<u8>> {
    let len = cur.u32()?;
    if len > MAX_ATTR_DATA as u32 {
        return Err(TnefError::ExcessLength {
            got: len,
            max: MAX_ATTR_DATA as u32,
        });
    }
    let n = usize::try_from(len).map_err(|_| TnefError::ExcessLength {
        got: len,
        max: MAX_ATTR_DATA as u32,
    })?;
    let data = cur.take(n)?.to_vec();
    let stored = cur.u16()?;
    let calc = checksum(&data);
    if stored != calc {
        return Err(TnefError::BadChecksum {
            stored,
            calculated: calc,
        });
    }
    Ok(data)
}

fn apply_message_attr(msg: &mut TnefMessage, attr_id: u32, data: &[u8]) -> Result<()> {
    match attr_id {
        id::MESSAGE_CLASS => msg.message_class = decode_cstr_ansi(data),
        id::ORIGINAL_MESSAGE_CLASS => msg.original_message_class = Some(decode_cstr_ansi(data)),
        id::SUBJECT => msg.subject = decode_cstr_ansi(data),
        id::BODY => msg.body = Some(decode_cstr_ansi(data)),
        id::MESSAGE_ID => msg.message_id = Some(decode_cstr_ansi(data)),
        id::PRIORITY => {
            if data.len() >= 2 {
                msg.priority = Some(u16::from_le_bytes([data[0], data[1]]));
            }
        }
        id::FROM => msg.sender = Some(parse_att_from(data)?),
        id::DATE_SENT => msg.date_sent = Some(parse_dtr(data)?),
        id::DATE_RECD => msg.date_received = Some(parse_dtr(data)?),
        id::DATE_MODIFIED => msg.date_modified = Some(parse_dtr(data)?),
        // Owner / sent-for / delegate / status / recipient-table / service
        // dates / request-res / aid-owner are message-level attributes that real
        // Outlook blobs carry but the gateway does not yet model in the
        // abstract `TnefMessage`. Tolerate them explicitly (fail-closed on the
        // wire check still happened in `read_attr_body_after_id`) so the reader
        // never rejects a valid stream over a documented attribute ŌĆö these arms
        // also keep the corresponding spec-declared attribute IDs referenced.
        id::MESSAGE_STATUS
        | id::RECIP_TABLE
        | id::OWNER
        | id::SENT_FOR
        | id::DELEGATE
        | id::DATE_START
        | id::DATE_END
        | id::AID_OWNER
        | id::REQUEST_RES => {}
        _ => {}
    }
    Ok(())
}

/// Parse the `attFrom` "TRP-structure" ([MS-OXTNEF] ┬¦2.1.3.3.3).
fn parse_att_from(data: &[u8]) -> Result<TnefAddress> {
    if data.len() < 8 {
        return Err(TnefError::Truncated("attFrom TRP header"));
    }
    let _trpid = u16::from_le_bytes([data[0], data[1]]);
    let _structure_len = u16::from_le_bytes([data[2], data[3]]);
    let sender_name_len = usize::from(u16::from_le_bytes([data[4], data[5]]));
    let sender_email_len = usize::from(u16::from_le_bytes([data[6], data[7]]));
    let mut pos = 8;
    if pos + sender_name_len > data.len() {
        return Err(TnefError::Truncated("attFrom display name"));
    }
    let disp = decode_cstr_ansi(&data[pos..pos + sender_name_len]);
    pos += sender_name_len;
    if pos + sender_email_len > data.len() {
        return Err(TnefError::Truncated("attFrom email"));
    }
    let email_field = &data[pos..pos + sender_email_len];
    // sender-email = type ":" address %x00
    let email_str = String::from_utf8_lossy(email_field).into_owned();
    let (atype, email) = email_str
        .split_once(':')
        .map(|(t, a)| (t.to_string(), a.trim_end_matches('\0').to_string()))
        .unwrap_or_default();
    Ok(TnefAddress {
        display_name: disp,
        address_type: atype,
        email,
    })
}

fn parse_dtr(data: &[u8]) -> Result<Dtr> {
    if data.len() < 14 {
        return Err(TnefError::Truncated("DTR"));
    }
    let u = |i: usize| u16::from_le_bytes([data[i], data[i + 1]]);
    Ok(Dtr {
        year: u(0),
        month: u(2),
        day: u(4),
        hour: u(6),
        minute: u(8),
        second: u(10),
        day_of_week: u(12),
    })
}

/// The `attMsgProps` / `attAttachment` property list ([MS-OXTNEF] ┬¦2.1.3.4):
/// `MsgPropertyCount` *`MsgPropertyValue`.
fn apply_msg_props(msg: &mut TnefMessage, data: &[u8]) -> Result<()> {
    msg.props = parse_prop_list(data)?;
    Ok(())
}

/// Apply one attachment-level attribute to the in-flight `TnefAttachment`.
/// ([MS-OXTNEF] ┬¦2.1.3.3 attachment attributes.)
fn apply_attach_attr(attach: &mut TnefAttachment, attr_id: u32, data: &[u8]) -> Result<()> {
    match attr_id {
        id::ATTACH_DATA => attach.data = data.to_vec(),
        id::ATTACH_TITLE => attach.filename = decode_cstr_ansi(data),
        id::ATTACH_TRANSPORT_FILENAME => attach.transport_filename = decode_cstr_ansi(data),
        id::ATTACHMENT => attach.props = parse_prop_list(data)?,
        // attAttachMetaFile / attAttachCreateDate / attAttachModifyDate are
        // parsed-but-not-modelled render metadata; tolerating them keeps the
        // reader robust to real Outlook blobs that always carry them.
        id::ATTACH_META_FILE | id::ATTACH_CREATE_DATE | id::ATTACH_MODIFY_DATE => {}
        _ => {}
    }
    Ok(())
}

fn parse_prop_list(data: &[u8]) -> Result<Vec<TnefProperty>> {
    let mut cur = Cur::new(data);
    let count = cur.u32()?;
    if count > MAX_PROP_COUNT {
        return Err(TnefError::ExcessLength {
            got: count,
            max: MAX_PROP_COUNT,
        });
    }
    let n = usize::try_from(count).map_err(|_| TnefError::ExcessLength {
        got: count,
        max: MAX_PROP_COUNT,
    })?;
    let mut out = Vec::with_capacity(n);
    for _ in 0..n {
        out.push(parse_prop_value(&mut cur)?);
    }
    Ok(out)
}

fn parse_prop_value(cur: &mut Cur<'_>) -> Result<TnefProperty> {
    let ptype = cur.u16()?;
    let pid = cur.u16()?;
    let tag = crate::mapi::data::PropertyTag::new(
        crate::mapi::data::PropertyType(ptype),
        pid,
    );
    let named = if pid >= 0x8000 {
        Some(parse_named_spec(cur)?)
    } else {
        None
    };
    let value = parse_prop_data(cur, ptype)?;
    Ok(TnefProperty { tag, named, value })
}

fn parse_named_spec(cur: &mut Cur<'_>) -> Result<NamedPropSpec> {
    let mut guid = [0u8; 16];
    guid.copy_from_slice(cur.take(16)?);
    let id_type = cur.u32()?;
    match id_type {
        0x0000_0000 => {
            let id = cur.u32()?;
            Ok(NamedPropSpec::Numeric { guid, id })
        }
        0x0000_0001 => {
            let len = cur.u32()?;
            if len > MAX_PROP_VALUE {
                return Err(TnefError::ExcessLength {
                    got: len,
                    max: MAX_PROP_VALUE,
                });
            }
            // `len` is the UTF-16LE code-unit count INCLUDING the terminating
            // 2-byte NUL.
            let units = usize::try_from(len).map_err(|_| TnefError::ExcessLength {
                got: len,
                max: MAX_PROP_VALUE,
            })?;
            let byte_len = units.checked_mul(2).ok_or(TnefError::ExcessLength {
                got: len,
                max: MAX_PROP_VALUE,
            })?;
            let raw = cur.take(byte_len)?;
            let mut code_units: Vec<u16> = Vec::with_capacity(units);
            for i in 0..units {
                code_units.push(u16::from_le_bytes([raw[2 * i], raw[2 * i + 1]]));
            }
            // Drop the trailing NUL.
            if code_units.last() == Some(&0) {
                code_units.pop();
            }
            // Optional 2-byte pad to a 4-byte boundary (reader permits non-zero).
            let pad = (4 - (byte_len % 4)) % 4;
            if pad != 0 {
                let _ = cur.take(pad)?;
            }
            let name = String::from_utf16_lossy(&code_units);
            Ok(NamedPropSpec::String { guid, name })
        }
        other => Err(TnefError::BadPropType(other as u16)),
    }
}

/// Decode a single property's typed value off the cursor (the property-tag
/// type has already been read). Per [MS-OXTNEF] section 2.1.3.4 /
/// `PropertyScalarContent = MsgPropertyContent [PropertyPad]`, every scalar's
/// content is padded with 0..3 zero bytes to a 4-byte boundary and those pad
/// bytes are counted in the attribute checksum, so the reader MUST consume
/// them to stay aligned for the following property tag. TNEF encodes
/// `PtypBoolean` and `PtypInteger16` as 2-byte values, each then padded to
/// 4 bytes.
fn parse_prop_data(cur: &mut Cur<'_>, ptype: u16) -> Result<TnefPropertyValue> {
    use crate::mapi::data::PropertyType as T;
    let t = T(ptype);
    Ok(match t {
        T::PTYP_NULL | T::PTYP_UNSPECIFIED => {
            // 2-byte content + 2-byte pad to a 4-byte boundary.
            let _ = cur.take(2)?;
            consume_scalar_pad(cur, 2);
            TnefPropertyValue::Null
        }
        T::PTYP_BOOLEAN => {
            // 2-byte content (WORD) per MS-OXCDATA section 2.11.1, padded to 4.
            let v = cur.u16()?;
            consume_scalar_pad(cur, 2);
            TnefPropertyValue::Boolean(v != 0)
        }
        T::PTYP_INTEGER16 => {
            let v = cur.i16()?;
            consume_scalar_pad(cur, 2);
            TnefPropertyValue::Integer16(v)
        }
        T::PTYP_INTEGER32 => TnefPropertyValue::Integer32(cur.i32()?),
        T::PTYP_ERROR_CODE => TnefPropertyValue::Integer32(cur.i32()?),
        T::PTYP_INTEGER64 => TnefPropertyValue::Integer64(cur.i64()?),
        T::PTYP_FLOATING32 => TnefPropertyValue::Floating32(cur.f32()?),
        T::PTYP_FLOATING64 => TnefPropertyValue::Floating64(cur.f64()?),
        T::PTYP_CURRENCY => TnefPropertyValue::Integer64(cur.i64()?),
        T::PTYP_TIME | T::PTYP_FLOATING_TIME => TnefPropertyValue::Time(cur.u64()?),
        T::PTYP_GUID => {
            let mut g = [0u8; 16];
            g.copy_from_slice(cur.take(16)?);
            TnefPropertyValue::Guid(g)
        }
        T::PTYP_STRING | T::PTYP_STRING8 => {
            // Count (UINT32), then per-value `VariableContent` (for a single
            // scalar the count is 1).
            let count = cur.u32()?;
            if count > MAX_PROP_VALUE {
                return Err(TnefError::ExcessLength {
                    got: count,
                    max: MAX_PROP_VALUE,
                });
            }
            let size = cur.u32()?;
            if size > MAX_PROP_VALUE {
                return Err(TnefError::ExcessLength {
                    got: size,
                    max: MAX_PROP_VALUE,
                });
            }
            let n = usize::try_from(size).map_err(|_| TnefError::ExcessLength {
                got: size,
                max: MAX_PROP_VALUE,
            })?;
            let raw = cur.take(n)?;
            // Pad to a 4-byte boundary.
            let pad = (4 - (n % 4)) % 4;
            if pad != 0 {
                let _ = cur.take(pad)?;
            }
            decode_string(t, raw)
        }
        T::PTYP_BINARY => {
            let _count = cur.u32()?;
            let size = cur.u32()?;
            if size > MAX_PROP_VALUE {
                return Err(TnefError::ExcessLength {
                    got: size,
                    max: MAX_PROP_VALUE,
                });
            }
            let n = usize::try_from(size).map_err(|_| TnefError::ExcessLength {
                got: size,
                max: MAX_PROP_VALUE,
            })?;
            let raw = cur.take(n)?.to_vec();
            let pad = (4 - (n % 4)) % 4;
            if pad != 0 {
                let _ = cur.take(pad)?;
            }
            TnefPropertyValue::Binary(raw)
        }
        // Multi-value: UINT32 count + per-element values. The element type is
        // the MV flag (0x1000) stripped off (`ptype & 0x0FFF`), so e.g. an
        // `MV_BINARY` (0x1102) decodes its elements as `PTYP_BINARY` (0x0102).
        mv if (mv.to_u16() & 0x1000) != 0 => {
            let elem_type = ptype & 0x0FFF;
            let count = cur.u32()?;
            if count > MAX_PROP_COUNT {
                return Err(TnefError::ExcessLength {
                    got: count,
                    max: MAX_PROP_COUNT,
                });
            }
            let n = usize::try_from(count).map_err(|_| TnefError::ExcessLength {
                got: count,
                max: MAX_PROP_COUNT,
            })?;
            let mut elements: Vec<Vec<u8>> = Vec::with_capacity(n);
            for _ in 0..n {
                // `read_one_mv_element` returns the element's PAYLOAD bytes
                // (no size prefix, no trailing pad) so the writer can re-frame
                // the variable shape unambiguously and fixed scalars are
                // written verbatim -- parse->build round-trips real Outlook MV
                // values byte-for-byte.
                let payload = read_one_mv_element(cur, elem_type)?;
                elements.push(payload);
            }
            TnefPropertyValue::Multi {
                element_type: elem_type,
                elements,
            }
        }
        // Unknown type: the reader cannot determine the value's wire length
        // (property types carry no self-describing length without the spec
        // rule for that type), so preserving "verbatim" is impossible -- the
        // cursor would have to swallow the rest of the property list, dropping
        // every subsequent property. Fail closed instead: a real Outlook/peer
        // blob does not emit documented copies of unsupported types into a
        // property list, and silently absorbing trailing data would desync
        // the rest of the message.
        other => return Err(TnefError::BadPropType(other.to_u16())),
    })
}

/// Consume the 0..3 byte `PropertyPad` that brings a scalar whose content is
/// `content_len` bytes onto a 4-byte boundary (section 2.1.3.4 `PropertyPad`).
fn consume_scalar_pad(cur: &mut Cur<'_>, content_len: usize) {
    let pad = (4 - (content_len % 4)) % 4;
    if pad != 0 {
        // Ignore the value of pad bytes (spec: writer MUST use zero, reader
        // MUST permit non-zero); only advance the cursor.
        let _ = cur.take(pad);
    }
}

/// Read a single multi-value element of the given (non-MV) element type off
/// the cursor and return its PAYLOAD bytes (the value itself, with no
/// per-element `UINT32` size prefix and no trailing `PropertyPad`), advancing
/// the cursor past size+payload+pad. Keeping the payload size-free lets the
/// writer re-emit the variable-element framing exactly once on re-encode;
/// fixed-size scalars are returned verbatim. TNEF encodes `PtypBoolean` as a
/// 2-byte value (per MS-OXCDATA section 2.11.1) and pads each element to 4
/// bytes.
fn read_one_mv_element(cur: &mut Cur<'_>, elem_type: u16) -> Result<Vec<u8>> {
    use crate::mapi::data::PropertyType as T;
    let t = T(elem_type);
    match t {
        T::PTYP_STRING | T::PTYP_STRING8 | T::PTYP_BINARY => {
            let size = cur.u32()?;
            let n = usize::try_from(size).map_err(|_| TnefError::ExcessLength {
                got: size,
                max: MAX_PROP_VALUE,
            })?;
            let payload = cur.take(n)?.to_vec();
            let pad = (4 - (n % 4)) % 4;
            if pad != 0 {
                let _ = cur.take(pad)?;
            }
            Ok(payload)
        }
        T::PTYP_BOOLEAN => {
            // 2-byte content; padded to a 4-byte boundary. The writer emits
            // the 2-byte value + 2-byte pad.
            let v = cur.u16()?;
            consume_scalar_pad(cur, 2);
            Ok(v.to_le_bytes().to_vec())
        }
        T::PTYP_INTEGER16 => {
            let v = cur.i16()?;
            consume_scalar_pad(cur, 2);
            Ok(v.to_le_bytes().to_vec())
        }
        T::PTYP_NULL | T::PTYP_UNSPECIFIED => {
            let _ = cur.take(2)?;
            consume_scalar_pad(cur, 2);
            Ok(Vec::new())
        }
        fixed if fixed.fixed_size().is_some() => Ok(cur.take(fixed.fixed_size().unwrap())?.to_vec()),
        other => Err(TnefError::BadPropType(other.to_u16())),
    }
}

fn decode_string(t: crate::mapi::data::PropertyType, raw: &[u8]) -> TnefPropertyValue {
    use crate::mapi::data::PropertyType as T;
    match t {
        T::PTYP_STRING => {
            let mut units: Vec<u16> = Vec::with_capacity(raw.len() / 2);
            for chunk in raw.chunks_exact(2) {
                units.push(u16::from_le_bytes([chunk[0], chunk[1]]));
            }
            if units.last() == Some(&0) {
                units.pop();
            }
            TnefPropertyValue::String(String::from_utf16_lossy(&units))
        }
        T::PTYP_STRING8 => {
            let i = raw.iter().position(|&b| b == 0).unwrap_or(raw.len());
            TnefPropertyValue::String8(String::from_utf8_lossy(&raw[..i]).into_owned())
        }
        _ => TnefPropertyValue::Binary(raw.to_vec()),
    }
}

/// Decode an ANSI NUL-terminated attribute string (used by the message-level
/// attributes such as `attSubject`). The TNEF header advertises code page
/// 1252, so attribute strings are Windows-1252 bytes (NOT UTF-8); decoding
/// them as UTF-8 would render 0x80-0xFF as the replacement char. Map each byte
/// through the Windows-1252 repertoire (the 0x80-0x9F range carries the
/// well-known extensions; 0xA0-0xFF are ISO-8859-1 identity).
fn decode_cstr_ansi(data: &[u8]) -> String {
    let i = data.iter().position(|&b| b == 0).unwrap_or(data.len());
    data[..i].iter().map(|&b| decode_cp1252_byte(b)).collect()
}

/// Map a single Windows-1252 byte to its `char`. Bytes 0x00-0x7F and
/// 0xA0-0xFF map by identity; the 0x80-0x9F extensions map to the Unicode
/// characters Windows-1252 defines there (inverse of `encode_cp1252`).
fn decode_cp1252_byte(b: u8) -> char {
    match b {
        0x80 => '\u{20AC}', // EURO SIGN
        0x82 => '\u{201A}', // SINGLE LOW-9 QUOTATION MARK
        0x83 => '\u{0192}', // LATIN SMALL LETTER F WITH HOOK
        0x84 => '\u{201E}', // DOUBLE LOW-9 QUOTATION MARK
        0x85 => '\u{2026}', // HORIZONTAL ELLIPSIS
        0x86 => '\u{2020}', // DAGGER
        0x87 => '\u{2021}', // DOUBLE DAGGER
        0x88 => '\u{02C6}', // MODIFIER LETTER CIRCUMFLEX ACCENT
        0x89 => '\u{2030}', // PER MILLE SIGN
        0x8A => '\u{0160}', // LATIN CAPITAL LETTER S WITH CARON
        0x8B => '\u{2039}', // SINGLE LEFT-POINTING ANGLE QUOTATION MARK
        0x8C => '\u{0152}', // LATIN CAPITAL LIGATURE OE
        0x8E => '\u{017D}', // LATIN CAPITAL LETTER Z WITH CARON
        0x91 => '\u{2018}', // LEFT SINGLE QUOTATION MARK
        0x92 => '\u{2019}', // RIGHT SINGLE QUOTATION MARK
        0x93 => '\u{201C}', // LEFT DOUBLE QUOTATION MARK
        0x94 => '\u{201D}', // RIGHT DOUBLE QUOTATION MARK
        0x95 => '\u{2022}', // BULLET
        0x96 => '\u{2013}', // EN DASH
        0x97 => '\u{2014}', // EM DASH
        0x98 => '\u{02DC}', // SMALL TILDE
        0x99 => '\u{2122}', // TRADE MARK SIGN
        0x9A => '\u{0161}', // LATIN SMALL LETTER S WITH CARON
        0x9B => '\u{203A}', // SINGLE RIGHT-POINTING ANGLE QUOTATION MARK
        0x9C => '\u{0153}', // LATIN SMALL LIGATURE OE
        0x9E => '\u{017E}', // LATIN SMALL LETTER Z WITH CARON
        0x9F => '\u{0178}', // LATIN CAPITAL LETTER Y WITH DIAERESIS
        // 0x81, 0x8D, 0x8F, 0x90, 0x9D are undefined in Windows-1252; map to
        // U+FFFD replacement so lossy decode never panics.
        0x81 | 0x8D | 0x8F | 0x90 | 0x9D => '\u{FFFD}',
        _ => char::from(b),
    }
}

// ---------------------------------------------------------------------------
// Writer
// ---------------------------------------------------------------------------

/// Serialise a `TnefMessage` into a byte buffer conforming to the MS-OXTNEF
/// ┬¦2.2 writer rules. The output is deterministic for a given input and
/// includes per-attribute checksums.
pub fn build(msg: &TnefMessage) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&TNEF_SIGNATURE.to_le_bytes());
    out.extend_from_slice(&LEGACY_KEY.to_le_bytes());

    // TNEFVersion & OEMCodePage (mandatory leading attributes).
    emit_attr(&mut out, LEVEL_MESSAGE, id::TNEF_VERSION, &TNEF_VERSION_DATA);
    emit_attr(
        &mut out,
        LEVEL_MESSAGE,
        id::OEM_CODEPAGE,
        &oem_codepage_data(),
    );

    // Message-level attributes (in the order the reader expects).
    if !msg.message_class.is_empty() {
        let d = ansi_cstr(&msg.message_class);
        emit_attr(&mut out, LEVEL_MESSAGE, id::MESSAGE_CLASS, &d);
    }
    if let Some(mc) = &msg.original_message_class {
        let d = ansi_cstr(mc);
        emit_attr(&mut out, LEVEL_MESSAGE, id::ORIGINAL_MESSAGE_CLASS, &d);
    }
    if let Some(from) = &msg.sender {
        let d = build_att_from(from);
        emit_attr(&mut out, LEVEL_MESSAGE, id::FROM, &d);
    }
    if !msg.subject.is_empty() {
        let d = ansi_cstr(&msg.subject);
        emit_attr(&mut out, LEVEL_MESSAGE, id::SUBJECT, &d);
    }
    if let Some(mid) = &msg.message_id {
        let d = ansi_cstr(mid);
        emit_attr(&mut out, LEVEL_MESSAGE, id::MESSAGE_ID, &d);
    }
    if let Some(dt) = msg.date_sent {
        let d = dtr_bytes(dt);
        emit_attr(&mut out, LEVEL_MESSAGE, id::DATE_SENT, &d);
    }
    if let Some(dt) = msg.date_received {
        let d = dtr_bytes(dt);
        emit_attr(&mut out, LEVEL_MESSAGE, id::DATE_RECD, &d);
    }
    if let Some(dt) = msg.date_modified {
        let d = dtr_bytes(dt);
        emit_attr(&mut out, LEVEL_MESSAGE, id::DATE_MODIFIED, &d);
    }
    if let Some(body) = &msg.body {
        let d = ansi_cstr(body);
        emit_attr(&mut out, LEVEL_MESSAGE, id::BODY, &d);
    }
    if let Some(pri) = msg.priority {
        emit_attr(&mut out, LEVEL_MESSAGE, id::PRIORITY, &pri.to_le_bytes());
    }

    // Encapsulated message properties (attMsgProps) ŌĆö last of the message
    // attributes per ┬¦2.1.3 "attMsgProps SHOULD be encoded after all other
    // message attributes".
    if !msg.props.is_empty() {
        let data = build_prop_list(&msg.props);
        emit_attr(&mut out, LEVEL_MESSAGE, id::MSG_PROPS, &data);
    }

    // Attachments: each set MUST begin with attAttachRendData.
    for att in &msg.attachments {
        let rend = build_rend_data(att.render_position);
        emit_attr(&mut out, LEVEL_ATTACHMENT, id::ATTACH_REND_DATA, &rend);
        if !att.filename.is_empty() {
            emit_attr(
                &mut out,
                LEVEL_ATTACHMENT,
                id::ATTACH_TITLE,
                &ansi_cstr(&att.filename),
            );
        }
        if !att.transport_filename.is_empty() {
            emit_attr(
                &mut out,
                LEVEL_ATTACHMENT,
                id::ATTACH_TRANSPORT_FILENAME,
                &ansi_cstr(&att.transport_filename),
            );
        }
        if !att.data.is_empty() {
            emit_attr(&mut out, LEVEL_ATTACHMENT, id::ATTACH_DATA, &att.data);
        }
        if !att.props.is_empty() {
            let data = build_prop_list(&att.props);
            emit_attr(&mut out, LEVEL_ATTACHMENT, id::ATTACHMENT, &data);
        }
    }

    out
}

fn emit_attr(out: &mut Vec<u8>, level: u8, attr_id: u32, data: &[u8]) {
    out.push(level);
    out.extend_from_slice(&attr_id.to_le_bytes());
    out.extend_from_slice(&(data.len() as u32).to_le_bytes());
    out.extend_from_slice(data);
    out.extend_from_slice(&checksum(data).to_le_bytes());
}

/// The 16-bit checksum ([MS-OXTNEF] ┬¦2.1.3.2): the sum of the data bytes,
/// modulo 65536. Pad bytes inside the attribute data MUST be included, which is
/// automatic here since the caller supplies the padded data verbatim.
fn checksum(data: &[u8]) -> u16 {
    let sum: u32 = data.iter().map(|&b| b as u32).sum();
    (sum & 0xFFFF) as u16
}

fn oem_codepage_data() -> Vec<u8> {
    let mut v = Vec::with_capacity(8);
    v.extend_from_slice(&OEM_CODEPAGE_PRIMARY.to_le_bytes());
    v.extend_from_slice(&OEM_CODEPAGE_SECONDARY.to_le_bytes());
    v
}

/// Build the `attFrom` "TRP-structure" ([MS-OXTNEF] ┬¦2.1.3.3.3).
fn build_att_from(addr: &TnefAddress) -> Vec<u8> {
    let disp = if addr.display_name.is_empty() {
        addr.email.clone()
    } else {
        addr.display_name.clone()
    };
    let disp_bytes = ansi_cstr(&disp);
    let email_field = format!(
        "{}:{}\0",
        if addr.address_type.is_empty() {
            "SMTP"
        } else {
            &addr.address_type
        },
        addr.email
    );
    let email_bytes = email_field.as_bytes();
    let mut out = Vec::with_capacity(8 + disp_bytes.len() + email_bytes.len() + 8);
    let trpid: u16 = 0x0004;
    let sender_name_len = disp_bytes.len() as u16;
    let sender_email_len = email_bytes.len() as u16;
    let structure_len: u16 = 8 + sender_name_len + sender_email_len + 8;
    out.extend_from_slice(&trpid.to_le_bytes());
    out.extend_from_slice(&structure_len.to_le_bytes());
    out.extend_from_slice(&sender_name_len.to_le_bytes());
    out.extend_from_slice(&sender_email_len.to_le_bytes());
    out.extend_from_slice(&disp_bytes);
    out.extend_from_slice(email_bytes);
    out.extend_from_slice(&[0u8; 8]);
    out
}

/// Build the `attAttachRendData` structure ([MS-OXTNEF] ┬¦2.1.3.3.15):
/// AttachType(2) + AttachPosition(4) + RenderWidth(2) + RenderHeight(2) +
/// DataFlags(4).
fn build_rend_data(render_position: i32) -> Vec<u8> {
    let mut out = Vec::with_capacity(14);
    out.extend_from_slice(&[0x01, 0x00]); // AttachTypeFile
    out.extend_from_slice(&render_position.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes()); // RenderWidth
    out.extend_from_slice(&0u16.to_le_bytes()); // RenderHeight
    out.extend_from_slice(&0u32.to_le_bytes()); // DataFlags = default
    out
}

fn dtr_bytes(dt: Dtr) -> Vec<u8> {
    let mut out = Vec::with_capacity(14);
    for v in [dt.year, dt.month, dt.day, dt.hour, dt.minute, dt.second, dt.day_of_week] {
        out.extend_from_slice(&v.to_le_bytes());
    }
    out
}

/// Serialise `MsgPropertyValue = MsgPropertyCount *MsgPropertyValue`
/// ([MS-OXTNEF] ┬¦2.1.3.4).
fn build_prop_list(props: &[TnefProperty]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&(props.len() as u32).to_le_bytes());
    for p in props {
        out.extend_from_slice(&p.tag.property_type.to_u16().to_le_bytes());
        out.extend_from_slice(&p.tag.property_id.to_le_bytes());
        if let Some(named) = &p.named {
            emit_named_spec(&mut out, named);
        }
        emit_prop_value(&mut out, &p.value);
    }
    out
}

fn emit_named_spec(out: &mut Vec<u8>, named: &NamedPropSpec) {
    match named {
        NamedPropSpec::Numeric { guid, id } => {
            out.extend_from_slice(guid);
            out.extend_from_slice(&0u32.to_le_bytes()); // IDTypeNumber
            out.extend_from_slice(&id.to_le_bytes());
        }
        NamedPropSpec::String { guid, name } => {
            out.extend_from_slice(guid);
            out.extend_from_slice(&1u32.to_le_bytes()); // IDTypeString
            let units: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();
            let len = units.len() as u32;
            out.extend_from_slice(&len.to_le_bytes());
            for u in &units {
                out.extend_from_slice(&u.to_le_bytes());
            }
            let pad = (4 - ((units.len() * 2) % 4)) % 4;
            for _ in 0..pad {
                out.push(0);
            }
        }
    }
}

fn emit_prop_value(out: &mut Vec<u8>, value: &TnefPropertyValue) {
    match value {
        TnefPropertyValue::Null => {
            // 2-byte content + 2-byte pad; reader consumes 4 bytes for NULL.
            out.extend_from_slice(&[0u8; 2]);
            pad2(out);
        }
        TnefPropertyValue::Boolean(b) => {
            // 2-byte WORD value per MS-OXCDATA section 2.11.1, padded to 4.
            out.extend_from_slice(&(if *b { 1u16 } else { 0u16 }).to_le_bytes());
            pad2(out);
        }
        TnefPropertyValue::Integer16(v) => {
            out.extend_from_slice(&v.to_le_bytes());
            pad2(out);
        }
        TnefPropertyValue::Integer32(v) => {
            out.extend_from_slice(&v.to_le_bytes());
        }
        TnefPropertyValue::Integer64(v) => {
            out.extend_from_slice(&v.to_le_bytes());
        }
        TnefPropertyValue::Floating32(v) => {
            out.extend_from_slice(&v.to_bits().to_le_bytes());
        }
        TnefPropertyValue::Floating64(v) => {
            out.extend_from_slice(&v.to_bits().to_le_bytes());
        }
        TnefPropertyValue::Time(v) => {
            out.extend_from_slice(&v.to_le_bytes());
        }
        TnefPropertyValue::Guid(g) => {
            out.extend_from_slice(g);
        }
        TnefPropertyValue::String(s) => {
            let wire: Vec<u8> = s
                .encode_utf16()
                .chain(std::iter::once(0))
                .flat_map(|u| u.to_le_bytes())
                .collect();
            emit_variable(out, &wire);
        }
        TnefPropertyValue::String8(s) => {
            let mut wire: Vec<u8> = s.bytes().collect();
            wire.push(0);
            emit_variable(out, &wire);
        }
        TnefPropertyValue::Binary(b) => {
            emit_variable(out, b);
        }
        TnefPropertyValue::Multi {
            element_type,
            elements,
        } => {
            out.extend_from_slice(&(elements.len() as u32).to_le_bytes());
            for e in elements {
                // Variable-string/binary elements carry their own per-element
                // size prefix + pad; fixed scalars are written verbatim.
                if matches!(
                    element_type,
                    0x001E | 0x001F | 0x0102
                ) {
                    out.extend_from_slice(&(e.len() as u32).to_le_bytes());
                    out.extend_from_slice(e);
                    let pad = (4 - (e.len() % 4)) % 4;
                    for _ in 0..pad {
                        out.push(0);
                    }
                } else {
                    out.extend_from_slice(e);
                }
            }
        }
        TnefPropertyValue::Opaque { bytes, .. } => {
            out.extend_from_slice(bytes);
        }
    }
}

/// Emit a variable-length `PtypString`/`PtypString8`/`PtypBinary` value as a
/// single-element multi-variable structure: `count(4)=1 | size(4) | data | pad`.
/// The caller supplies the already-serialised wire bytes (including the
/// terminating NUL/code-unit zero).
fn emit_variable(out: &mut Vec<u8>, data: &[u8]) {
    out.extend_from_slice(&1u32.to_le_bytes()); // count = 1
    out.extend_from_slice(&(data.len() as u32).to_le_bytes());
    out.extend_from_slice(data);
    let pad = (4 - (data.len() % 4)) % 4;
    for _ in 0..pad {
        out.push(0);
    }
}

fn pad2(out: &mut Vec<u8>) {
    out.extend_from_slice(&[0u8; 2]);
}

/// Encode a Rust string as a Windows-1252 (CP1252) NUL-terminated string.
/// The TNEF header advertises the OEM attribute-string code page as 1252
/// (`OEM_CODEPAGE_PRIMARY`), so attribute strings (`attSubject`,
/// `attMessageClass`, `attBody`, `attMessageID`, attachment filenames,
/// `attFrom` display name, ...) MUST be bytes in that code page -- emitting
/// raw UTF-8 would label a multibyte sequence as Windows-1252 and renders as
/// mojibake in Outlook. Characters outside the Windows-1252 repertoire are
/// lossy-replaced with `b'?'` (the spec offers no escape for unmappable
/// attribute strings; meeting-correlation fields are conventionally ASCII,
/// so this is a safe fallback).
fn ansi_cstr(s: &str) -> Vec<u8> {
    let mut v = Vec::with_capacity(s.len() + 1);
    v.extend(s.chars().map(encode_cp1252));
    v.push(0);
    v
}

/// Map a single `char` to its Windows-1252 byte. The 0x80-0x9F range in
/// Windows-1252 carries the well-known punctuation/letter extensions
/// (€‚ƒ„…†‡ˆ‰Š‹ŒŽ‘’“”•–—˜™š›œžŸ) rather than C1 control codes.
fn encode_cp1252(c: char) -> u8 {
    let u = c as u32;
    match u {
        0..=0x7F => u as u8,
        0xA0..=0xFF => u as u8,
        // Windows-1252 extensions in the 0x80-0x9F range.
        0x20AC => 0x80, // EURO SIGN
        0x201A => 0x82, // SINGLE LOW-9 QUOTATION MARK
        0x0192 => 0x83, // LATIN SMALL LETTER F WITH HOOK
        0x201E => 0x84, // DOUBLE LOW-9 QUOTATION MARK
        0x2026 => 0x85, // HORIZONTAL ELLIPSIS
        0x2020 => 0x86, // DAGGER
        0x2021 => 0x87, // DOUBLE DAGGER
        0x02C6 => 0x88, // MODIFIER LETTER CIRCUMFLEX ACCENT
        0x2030 => 0x89, // PER MILLE SIGN
        0x0160 => 0x8A, // LATIN CAPITAL LETTER S WITH CARON
        0x2039 => 0x8B, // SINGLE LEFT-POINTING ANGLE QUOTATION MARK
        0x0152 => 0x8C, // LATIN CAPITAL LIGATURE OE
        0x017D => 0x8E, // LATIN CAPITAL LETTER Z WITH CARON
        0x2018 => 0x91, // LEFT SINGLE QUOTATION MARK
        0x2019 => 0x92, // RIGHT SINGLE QUOTATION MARK
        0x201C => 0x93, // LEFT DOUBLE QUOTATION MARK
        0x201D => 0x94, // RIGHT DOUBLE QUOTATION MARK
        0x2022 => 0x95, // BULLET
        0x2013 => 0x96, // EN DASH
        0x2014 => 0x97, // EM DASH
        0x02DC => 0x98, // SMALL TILDE
        0x2122 => 0x99, // TRADE MARK SIGN
        0x0161 => 0x9A, // LATIN SMALL LETTER S WITH CARON
        0x203A => 0x9B, // SINGLE RIGHT-POINTING ANGLE QUOTATION MARK
        0x0153 => 0x9C, // LATIN SMALL LIGATURE OE
        0x017E => 0x9E, // LATIN SMALL LETTER Z WITH CARON
        0x0178 => 0x9F, // LATIN CAPITAL LETTER Y WITH DIAERESIS
        _ => b'?',
    }
}

// ---------------------------------------------------------------------------
// PidTagTnefCorrelationKey named property
// ---------------------------------------------------------------------------

/// The named-property GUID identifying Outlook's `PSETID_Meeting` property
/// set `{6ED8DA90-450B-101B-98DA-00AA003F1305}` (MS-OXPROPS "Commonly Used
/// Property Sets"). Callers that build named-property voting/meeting props
/// use the value returned by `meeting_property_guid()` so Outlook resolves
/// those properties under the correct namespace.
const CORRELATION_GUID: [u8; 16] = [
    0x90, 0xda, 0xd8, 0x6e, 0x0b, 0x45, 0x1b, 0x10, 0x98, 0xda, 0x00, 0xaa, 0x00, 0x3f, 0x13,
    0x05,
];

/// Build a `PidTagTnefCorrelationKey` (standard property tag `0x007F`,
/// `PtypBinary` -- NOT a named property, so `named` is `None`) whose value is
/// the delivered message's search key, so the recipient client correlates the
/// `winmail.dat` blob with the right message ([MS-OXTNEF] section 2.1.3.3.6
/// maps this to `PidTagSearchKey`). The `correlation_value` is the
/// search-key bytes.
pub fn tnef_correlation_property(correlation_value: &[u8]) -> TnefProperty {
    TnefProperty {
        tag: crate::mapi::data::PropertyTag::new(
            crate::mapi::data::PropertyType::PTYP_BINARY,
            0x007F, // PidTagTnefCorrelationKey
        ),
        named: None,
        value: TnefPropertyValue::Binary(correlation_value.to_vec()),
    }
}

/// The fixed PSETID_Meeting GUID the gateway advertises for named meeting
/// properties (used by callers that build named-property voting / invite props).
pub fn meeting_property_guid() -> [u8; 16] {
    CORRELATION_GUID
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_message() -> TnefMessage {
        TnefMessage {
            message_class: "IPM.Note".to_string(),
            subject: "Hello".to_string(),
            body: Some("hello world".to_string()),
            sender: Some(TnefAddress {
                display_name: "Alice".to_string(),
                address_type: "SMTP".to_string(),
                email: "alice@example.com".to_string(),
            }),
            priority: Some(2),
            attachments: vec![TnefAttachment {
                filename: "file.bin".to_string(),
                transport_filename: "file.bin".to_string(),
                render_position: -1,
                data: vec![0xDE, 0xAD, 0xBE, 0xEF],
                props: Vec::new(),
            }],
            ..Default::default()
        }
    }

    #[test]
    fn round_trip_core_attributes() {
        let msg = sample_message();
        let bytes = build(&msg);
        // The stream must open with the TNEF signature.
        assert_eq!(&bytes[..4], &TNEF_SIGNATURE.to_le_bytes());
        let parsed = parse(&bytes).expect("round-trip parse");
        assert_eq!(parsed.message_class, "IPM.Note");
        assert_eq!(parsed.subject, "Hello");
        assert_eq!(parsed.body.as_deref(), Some("hello world"));
        assert_eq!(parsed.priority, Some(2));
        let from = parsed.sender.expect("sender round-trips");
        assert_eq!(from.display_name, "Alice");
        assert_eq!(from.email, "alice@example.com");
        // Attachment round-trips its binary payload + filename.
        assert_eq!(parsed.attachments.len(), 1);
        assert_eq!(parsed.attachments[0].filename, "file.bin");
        assert_eq!(parsed.attachments[0].data, vec![0xDE, 0xAD, 0xBE, 0xEF]);
    }

    #[test]
    fn rejects_bad_signature() {
        let mut bytes = build(&sample_message());
        bytes[0] = 0x00;
        assert!(parse(&bytes).is_err());
    }

    #[test]
    fn rejects_truncated_stream() {
        let bytes = build(&sample_message());
        let truncated = &bytes[..bytes.len() / 2];
        assert!(parse(truncated).is_err());
    }

    #[test]
    fn checksum_is_modulo_65536() {
        // 258 bytes of 0xFF sum to 258 * 255 = 65790, which wraps the mod-65536
        // checksum to 254 — exercising the actual modulo (a sum ≤ 65535 would
        // not, so this proves the `& 0xFFFF` mask is non-trivial).
        let data = vec![0xFFu8; 258];
        assert_eq!(super::checksum(&data), (258 * 255 % 65536) as u16);
        assert_eq!(258 * 255 % 65536, 254);
    }

    #[test]
    fn named_string_property_round_trips() {
        let guid = meeting_property_guid();
        let prop = TnefProperty {
            tag: crate::mapi::data::PropertyTag::new(
                crate::mapi::data::PropertyType::PTYP_STRING,
                0x8003,
            ),
            named: Some(NamedPropSpec::String {
                guid,
                name: "voting".to_string(),
            }),
            value: TnefPropertyValue::String("yes".to_string()),
        };
        let msg = TnefMessage {
            props: vec![prop.clone()],
            ..Default::default()
        };
        let parsed = parse(&build(&msg)).expect("named-prop round-trip");
        let got = parsed
            .props
            .first()
            .expect("encapsulated prop survives the round trip");
        assert!(got.named.is_some());
        assert_eq!(got.value, prop.value);
    }

    #[test]
    fn integer_and_binary_props_round_trip() {
        let props = vec![
            TnefProperty {
                tag: crate::mapi::data::PropertyTag::new(
                    crate::mapi::data::PropertyType::PTYP_INTEGER32,
                    0x0017, // PR_IMPORTANCE
                ),
                named: None,
                value: TnefPropertyValue::Integer32(2),
            },
            TnefProperty {
                tag: crate::mapi::data::PropertyTag::new(
                    crate::mapi::data::PropertyType::PTYP_BINARY,
                    0x007F,
                ),
                named: None,
                value: TnefPropertyValue::Binary(vec![0x01, 0x02, 0x03]),
            },
        ];
        let msg = TnefMessage {
            props: props.clone(),
            ..Default::default()
        };
        let parsed = parse(&build(&msg)).expect("scalar round-trip");
        assert_eq!(parsed.props[0].value, props[0].value);
        assert_eq!(parsed.props[1].value, props[1].value);
    }

    #[test]
    fn correlation_property_carries_value() {
        let p = tnef_correlation_property(&[0xAB, 0xCD]);
        assert!(matches!(p.value, TnefPropertyValue::Binary(_)));
        if let TnefPropertyValue::Binary(b) = &p.value {
            assert_eq!(b, &vec![0xAB, 0xCD]);
        }
    }

    #[test]
    fn empty_message_still_valid_stream() {
        let bytes = build(&TnefMessage::default());
        assert!(parse(&bytes).is_ok());
    }

    #[test]
    fn dtr_round_trips() {
        let dt = Dtr {
            year: 2025,
            month: 1,
            day: 2,
            hour: 3,
            minute: 4,
            second: 5,
            day_of_week: 1,
        };
        let msg = TnefMessage {
            date_sent: Some(dt),
            ..Default::default()
        };
        let parsed = parse(&build(&msg)).expect("dtr round-trip");
        assert_eq!(parsed.date_sent, Some(dt));
    }

    #[test]
    fn signature_and_version_match_spec_wire_bytes() {
        // [MS-OXTNEF] section 2.1.3.2/2.1.3.3.1: signature is the 4-byte LE
        // value 0x223E9F78 (bytes 78 9F 3E 22) and attTnefVersion data is
        // 00 00 01 00. Round-trip tests do NOT catch a wrong constant (build
        // and parse both reference it), so assert the on-the-wire bytes.
        let bytes = build(&TnefMessage::default());
        assert_eq!(&bytes[..4], &[0x78, 0x9F, 0x3E, 0x22], "TNEF signature");
        assert_eq!(TNEF_SIGNATURE, 0x223E_9F78);
        assert_eq!(TNEF_VERSION_DATA, [0x00, 0x00, 0x01, 0x00]);
    }

    #[test]
    fn reader_rejects_non_canonical_version_data() {
        // The reader MUST reject attTnefVersion data other than 00 00 01 00.
        let mut bytes = build(&TnefMessage::default());
        // Layout: signature(4) + legacy_key(2) + [lvl(1)+id(4)+len(4)+data(4)+
        // checksum(2)] for attTnefVersion. The 4 data bytes + trailing 16-bit
        // checksum are patched together so the patched packet's checksum still
        // validates and the parser reaches the version-data comparison
        // (otherwise it would fail earlier with BadChecksum).
        let off = 4 + 2 + 1 + 4 + 4;
        assert_eq!(&bytes[off..off + 4], &[0x00, 0x00, 0x01, 0x00]);
        bytes[off + 3] = 0x02;
        let calc = checksum(&bytes[off..off + 4]);
        bytes[off + 4] = (calc & 0xFF) as u8;
        bytes[off + 5] = ((calc >> 8) & 0xFF) as u8;
        let err = parse(&bytes).expect_err("non-canonical version rejected");
        assert!(matches!(err, TnefError::BadVersion), "{err:?}");
    }

    #[test]
    fn boolean_scalar_round_trips_and_keeps_alignment() {
        // [MS-OXTNEF] encodes a Boolean scalar as a 2-byte WORD padded to 4
        // bytes. A following second property in the SAME message-props list
        // asserts the reader consumed all 4 bytes — proving alignment is not
        // off-by-2 (the old 1-byte reader misaligned every Boolean).
        let props = vec![
            TnefProperty {
                tag: crate::mapi::data::PropertyTag::new(
                    crate::mapi::data::PropertyType::PTYP_BOOLEAN,
                    0x0FFF, // arbitrary 0x000B-typed prop id for the alignment test
                ),
                named: None,
                value: TnefPropertyValue::Boolean(true),
            },
            TnefProperty {
                tag: crate::mapi::data::PropertyTag::new(
                    crate::mapi::data::PropertyType::PTYP_INTEGER32,
                    0x0017, // PR_IMPORTANCE
                ),
                named: None,
                value: TnefPropertyValue::Integer32(5),
            },
        ];
        let msg = TnefMessage {
            props,
            ..Default::default()
        };
        let parsed = parse(&build(&msg)).expect("boolean+int round-trip");
        assert_eq!(parsed.props.len(), 2);
        assert_eq!(parsed.props[0].value, TnefPropertyValue::Boolean(true));
        assert_eq!(parsed.props[1].value, TnefPropertyValue::Integer32(5));
    }

    #[test]
    fn mv_binary_property_round_trips() {
        // MV_BINARY (0x1102): the element type MUST be 0x0102 after stripping
        // the 0x1000 MV flag (`ptype & 0x0FFF`), and the variable elements
        // must round-trip with EXACTLY ONE per-element size prefix each (no
        // double-prefix on re-encode). Two unequal payloads exercise framing.
        use crate::mapi::data::{PropertyTag, PropertyType};
        let props = vec![TnefProperty {
            tag: PropertyTag::new(PropertyType(0x1102), 0x1004),
            named: None,
            value: TnefPropertyValue::Multi {
                element_type: 0x0102,
                elements: vec![vec![0xAA, 0xBB], vec![0x01, 0x02, 0x03, 0x04]],
            },
        }];
        let msg = TnefMessage {
            props,
            ..Default::default()
        };
        let built = build(&msg);
        let parsed = parse(&built).expect("MV_BINARY round-trip");
        assert_eq!(parsed.props.len(), 1);
        match &parsed.props[0].value {
            TnefPropertyValue::Multi {
                element_type,
                elements,
            } => {
                assert_eq!(*element_type, 0x0102, "MV flag stripped to base type");
                assert_eq!(elements.len(), 2);
                assert_eq!(elements[0], vec![0xAA, 0xBB]);
                assert_eq!(elements[1], vec![0x01, 0x02, 0x03, 0x04]);
            }
            other => panic!("expected Multi, got {other:?}"),
        }
    }

    #[test]
    fn attachment_render_position_round_trips() {
        // ATTACH_REND_DATA decoding preserves a nonzero AttachPosition so a
        // parse->build round-trip does not reset placement to 0.
        let att = TnefAttachment {
            filename: "img.png".to_string(),
            transport_filename: "img.png".to_string(),
            render_position: 12345,
            data: vec![0x01, 0x02, 0x03],
            props: Vec::new(),
        };
        let msg = TnefMessage {
            attachments: vec![att],
            ..Default::default()
        };
        let parsed = parse(&build(&msg)).expect("rend-data round-trip");
        assert_eq!(parsed.attachments.len(), 1);
        assert_eq!(parsed.attachments[0].render_position, 12345);
    }

    #[test]
    fn cp1252_string_attribute_round_trips_without_mojibake() {
        // Non-ASCII attribute strings are emitted as Windows-1252 (the header
        // advertises code page 1252); a round-trip preserves the glyphs that
        // 1252 can represent so Outlook does not display mojibake.
        let subject = "Café — student € rate"; // contains é, em-dash, euro
        let msg = TnefMessage {
            subject: subject.to_string(),
            ..Default::default()
        };
        let bytes = build(&msg);
        // Subject is an ANSI string attribute (Windows-1252 bytes). The string
        // 'é' is 0xE9 in 1252, '€' is 0x80, em-dash is 0x97 — NOT their UTF-8
        // multibyte forms.
        assert!(bytes.windows(subject.len().min(8)).any(|w| w.contains(&0xE9)));
        assert!(bytes.contains(&0x80), "euro sign encoded as 0x80");
        assert!(bytes.contains(&0x97), "em-dash encoded as 0x97");
        let parsed = parse(&bytes).expect("cp1252 subject round-trip");
        assert_eq!(parsed.subject, subject);
    }

    #[test]
    fn meeting_property_guid_is_psetid_meeting() {
        // The named-property namespace for meeting/voting props must be the
        // canonical PSETID_Meeting GUID {6ED8DA90-450B-101B-98DA-00AA003F1305}.
        let g = meeting_property_guid();
        assert_eq!(
            g,
            [
                0x90, 0xda, 0xd8, 0x6e, 0x0b, 0x45, 0x1b, 0x10, 0x98, 0xda, 0x00, 0xaa, 0x00,
                0x3f, 0x13, 0x05
            ]
        );
    }
}
