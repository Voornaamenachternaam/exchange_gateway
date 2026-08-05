// src/mapi/nspi.rs
//
// NSPI address-book dispatcher (MS-OXNSPI / MS-OXOABK / MS-OXOAB / MS-OXABREF /
// MS-OXOABKT) served over the MS-OXCMAPIHTTP §2.2.5 framing on `/mapi/nspi`.
//
// Closes audit gap §2d ("GAL / Address Book / user photo — BLOCKING for
// recipient resolution"). Previously the entire NSPI surface was rejected at
// the transport layer (any address-book `X-RequestType` ⇒
// `InvalidRequestType`), so New Outlook for Windows could not resolve
// sender/recipient display names against anything other than the user's own
// contacts, "Check Names" failed, and the OAB download URL had no backing
// server. This module serves the address-book RPCs Outlook actually issues over
// MAPI/HTTP — `Bind`, `Unbind`, `UpdateStat`, `QueryRows`, `DnToMinId`,
// `ResolveNames`, `GetMatches`, `GetProps`, `GetPropList`, `GetSpecialTable`,
// `SeekEntries`, `QueryColumns`, `ResortRestriction`, `CompareMIds` plus the
// URL helpers — backed by the operator-configured directory
// (`Arc<dyn DirectoryLookup>`, the same store EWS `ResolveNames` /
// `FindPeople` and the OAB download already use, backed by the Stalwart admin
// API). With no directory configured it serves a *minimal GAL stub* containing
// only the authenticated caller's own mailbox entry so "Check Names" for self
// still resolves; non-self resolutions return an empty
// result set, the documented behaviour of a directory-less Exchange look-alike.
//
// Wire format: over MAPI/HTTP the `/mapi/nspi` Execute body carries the NSPI
// method call directly — `Reserved/Flags(4) + [State(36-byte STAT)] + RPC-
// specific fields + AuxiliaryBufferSize(4) + AuxiliaryBuffer(variable)`
// (MS-OXNSPI §3.1.4.1.*; the `[State]` presence is method-specific, e.g.
// QueryRows/UpdateStat/ResolveNames carry a STAT input while Bind does not and
// Unbind carries only Flags). There is NO separate `HasState(1)` selector byte
// here: that byte belongs to the EMSMDB ROP / RPC-over-HTTP (ncacn_http)
// transport, NOT the `/mapi/nspi` MAPI/HTTP Execute body. The STAT structure
// is nine little-endian DWORDs (`SortType, ContainerID, CurrentRec, Delta,
// NumPos, TotalRecs, CodePage, TemplateLocale, SortLocale`) per MS-OXNSPI
// §2.2.8. Property rows are `AddressBookPropertyRow`s (§2.2.1.7) whose
// per-cell value is an `AddressBookPropertyValue` (§2.2.1.1) — a 1-byte
// `HasValue` flag for the variable-length types (String/String8/Binary)
// followed by the length-prefixed value, and a direct inline value for the
// fixed-size scalars.
//
// Security:
//   * Every `/mapi/nspi` RPC is authenticated against the same
//     `AuthVerifier` the mailbox path uses (Stalwart JMAP/CalDAV creds).
//     Anonymous access ⇒ transport `NoPrivilege` (code 11) so the GAL surface
//     never leaks recipient PII. The username/password travel in
//     `MapiRequest.{username,password}` (plumbed by the router from the
//     raw `Authorization: Basic` header).
//   * Directory lookups (blocking, network) run on `spawn_blocking` so the
//     async runtime is never blocked; `JoinError`s are logged (redacted) and
//     degrade to the directory-less minimal stub rather than panicking.
//   * The minimal-entry-id (MId) space is built deterministically from the
//     directory snapshot (alphabetical by email, 1-based so MId 0 stays the
//     "no current record" sentinel Outlook sends in `CurrentRec`), so a
//     `DnToMinId` → `QueryRows` round-trip within one Bind session is stable
//     and never cross-resolves two distinct directory entries.

use crate::directory::{Contact, SearchResult};
use crate::mapi::data::{PropertyType, PropertyValue};
use crate::mapi::rops::Buf;
use crate::mapi::transport::{AddressBookRpc, MapiRequest, MapiResponse, ResponseCode};
use crate::util::canonicalize_username;
use secrecy::ExposeSecret;

use super::handler::MapiState;

// ---------------------------------------------------------------------------
// Address-book property tags (MS-OXPROPS / MS-OXOABK). The canonical GAL set
// Outlook requests in a `QueryRows` / `ResolveNames` column set. Defined here
// (not in `store.rs`) because they are NSPI-specific: `store.rs` models the
// *mailbox* property universe, while the address-book table exposes a smaller
// recipient-shaped subset whose tags (`PidTagObjectType`, `PidTagDisplayType`,
// `PidTagTemplateId`, `PidTagInstanceKey`, … ) have no mailbox meaning.
// ---------------------------------------------------------------------------

/// PidTagObjectType — `0x0FFE` (Integer32). MAPI_MAILUSER = 0x00000006.
const PR_OBJECT_TYPE: u16 = 0x0FFE;
/// PidTagDisplayType — `0x3900` (Integer32). DT_MAILUSER = 0x00000000.
/// (Not `0x3FFF`: per MS-OXPROPS §2.738 the canonical `PidTagDisplayType`
/// tag id is `0x3900`; `0x3FFF` is unused.)
const PR_DISPLAY_TYPE: u16 = 0x3900;
/// PidTagDisplayName — `0x3001` (already in store.rs as PR_DISPLAY_NAME).
const PR_DISPLAY_NAME_ABOOK: u16 = 0x3001;
/// PidTagEmailAddress — `0x3003` (String). The X500 DN for legacy lookups, or
/// the SMTP address for one-off contacts; the gateway serves SMTP.
const PR_EMAIL_ADDRESS_ABOOK: u16 = 0x3003;
/// PidTagAddressType — `0x3002` (String). "SMTP" for the served rows.
const PR_ADDRESS_TYPE: u16 = 0x3002;
/// PidTagSmtpAddress — `0x39FE` (String). The canonical SMTP address Outlook
/// shows in the recipient preview and uses for `ResolveNames` ANR matching.
const PR_SMTP_ADDRESS: u16 = 0x39FE;
/// PidTagTemplateId — `0x3FFA` (Integer32). Per MS-OXOABK the explicit-table
/// template id; the gateway serves a non-template-bound table and renders 0.
const PR_TEMPLATE_ID: u16 = 0x3FFA;
/// PidTagInstanceKey — `0x0FF6` (Binary, 4-byte). The 4-byte per-row key the
/// NSPI Explicit-Table algorithm keys off; the gateway uses the MId bytes.
const PR_INSTANCE_KEY: u16 = 0x0FF6;
/// PidTagRecordKey — `0x0FF9` (Binary, 4-byte). Stable per-row record key
/// (== InstanceKey for the gateway's single-table GAL).
const PR_RECORD_KEY_ABOOK: u16 = 0x0FF9;
/// PidTagSearchKey — `0x300B` (Binary). Per-object search key; for a mail user
/// it is `SMTP:<UPCASED email>` (MS-OXCDATA §2.6.8).
const PR_SEARCH_KEY_ABOOK: u16 = 0x300B;
/// PidTagTransmittableDisplayName — `0x3A20` (String). Rendered on messages
/// the recipient receives; the gateway mirrors the display name.
const PR_TRANSMIT_DISPLAY_NAME_ABK: u16 = 0x3A20;
/// PidTagSevenBitDisplayName — `0x39FF` (String8). ASCII-safe display name.
const PR_7BIT_DISPLAY_NAME: u16 = 0x39FF;
/// PidTagTitle — supported by the directory `Contact.title` (already in store
/// as 0x8015 for the CardDAV path); the address-book canonical PidTagTitle is
/// `0x3A04`.
const PR_TITLE_ABOOK: u16 = 0x3A04;
/// PidTagCompanyName — `0x3A16` (String).
const PR_COMPANY_NAME_ABOOK: u16 = 0x3A16;
/// PidTagDepartmentName — `0x3A18` (String).
const PR_DEPARTMENT_NAME: u16 = 0x3A18;
/// PidTagBusinessTelephoneNumber — `0x3A08` (String).
const PR_BUSINESS_TEL_ABOOK: u16 = 0x3A08;
/// PidTagPrimaryTelephoneNumber — `0x3A0A` (String).
const PR_PRIMARY_TEL_ABOOK: u16 = 0x3A0A;
/// PidTagMobileTelephoneNumber — `0x3A1C` (String).
const PR_MOBILE_TEL_ABOOK: u16 = 0x3A1C;
/// PidTagHomeTelephoneNumber — `0x3A09` (String).
const PR_HOME_TEL_ABOOK: u16 = 0x3A09;
/// PidTagAccount — `0x3A00` (String). The logon account name; == SMTP local.
const PR_ACCOUNT: u16 = 0x3A00;
/// PidTagEntryId — `0x0FFF` (Binary). Long-term entry id (server-relative).
const PR_ENTRYID_ABOOK: u16 = 0x0FFF;
/// PidTagSendRichInfo — `0x3A40` (Boolean). Whether the recipient accepts
/// RTF; we advertise TRUE so Outlook composes RTF for GAL recipients.
const PR_SEND_RICH_INFO: u16 = 0x3A40;
/// PidTagDisplayTypeEx — `0x3905` (Integer32) per MS-OXPROPS §2.668 / MS-OXOABK
/// §2.2.4.1 (the canonical GAL column-set id; `0x3FFD` is NOT a published
/// PidTag). Mirrors DisplayType for an address-book mail-user (`DT_MAILUSER`).
const PR_DISPLAY_TYPE_EX: u16 = 0x3905;
/// PidTagMappingSignature — `0x0FF8` (Binary). The address-book provider
/// signature; the gateway uses the constant EMSMDB provider uid prefix so a
/// one-off store compare from Outlook succeeds.
const PR_MAPPING_SIGNATURE: u16 = 0x0FF8;

/// The address-book "root" / GAL container Minimal Entry ID. The GAL is
/// container MId `0x00000000` per MS-OXNSPI §2.2.8 `ContainerID` examples;
/// the special-table hierarchy container is `0xFFFFFFFF` ("no specific
/// container"). For a single-container gateway the GAL *is* container 0.
const GAL_CONTAINER_MID: u32 = 0x00000000;

/// The MAPI object-type value for a mail user (MS-OXNSPI §2.2.1.3).
const MAPI_MAILUSER: u32 = 0x0000_0006;
/// The display-type value for a mail user (MS-OXNSPI §2.2.1.2 `DT_MAILUSER`).
const DT_MAILUSER: u32 = 0x0000_0000;

/// Upper bound on the number of rows a single `QueryRows` returns. MS-OXNSPI
/// §3.1.4.1.8 caps the server's `Count` parameter; Outlook issues sequential
/// `QueryRows` to page through, so a per-call cap (not a global cap) is the
/// correct shape. Generous enough to satisfy an ANR result set in a single
/// call, conservative enough to bound memory against a runaway client.
const MAX_QUERY_ROWS: u32 = 1024;

/// Upper bound on the number of names a single `ResolveNames` accepts. The
/// client typically sends one name; cap to resist abuse.
const MAX_RESOLVE_NAMES: u32 = 256;

/// Upper bound on the number of DNs a single `DnToMinId` accepts.
const MAX_DN_TO_MID: u32 = 1024;

// ---------------------------------------------------------------------------
// STAT codec — MS-OXNSPI §2.2.8 (nine little-endian DWORDs = 36 bytes).
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Stat {
    pub sort_type: u32,
    pub container_id: u32,
    pub current_rec: u32,
    pub delta: i32,
    pub num_pos: u32,
    pub total_recs: u32,
    pub code_page: u32,
    pub template_locale: u32,
    pub sort_locale: u32,
}

impl Stat {
    /// Fixed wire size of the STAT structure over MAPI/HTTP.
    pub const WIRE_SIZE: usize = 36;
    /// Alias used by tolerant handlers that peek the remaining body length.
    pub const ENCODED_LEN: usize = Self::WIRE_SIZE;

    /// A fresh "cursor at the top of a `container_len`-row table" STAT: the
    /// defaults plus `total_recs` set to the container size (clamped to u32).
    /// Used by handlers that do not receive an input STAT (e.g. Bind).
    fn default_for(container_len: usize) -> Self {
        Self {
            total_recs: u32::try_from(container_len).unwrap_or(u32::MAX),
            ..Self::default()
        }
    }

    /// Decode a STAT off a cursor, failing closed on a short buffer.
    fn decode(cur: &mut Buf<'_>) -> Result<Self, NspiDecodeError> {
        if cur.remaining() < Self::WIRE_SIZE {
            return Err(NspiDecodeError::Insufficient);
        }
        // Use take_u32_le wrapped through a helper that returns DecodeError.
        let sort_type = take_u32(cur)?;
        let container_id = take_u32(cur)?;
        let current_rec = take_u32(cur)?;
        let delta = take_i32(cur)?;
        let num_pos = take_u32(cur)?;
        let total_recs = take_u32(cur)?;
        let code_page = take_u32(cur)?;
        let template_locale = take_u32(cur)?;
        let sort_locale = take_u32(cur)?;
        Ok(Self {
            sort_type,
            container_id,
            current_rec,
            delta,
            num_pos,
            total_recs,
            code_page,
            template_locale,
            sort_locale,
        })
    }

    /// Encode the STAT into `out`.
    fn encode(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&self.sort_type.to_le_bytes());
        out.extend_from_slice(&self.container_id.to_le_bytes());
        out.extend_from_slice(&self.current_rec.to_le_bytes());
        out.extend_from_slice(&self.delta.to_le_bytes());
        out.extend_from_slice(&self.num_pos.to_le_bytes());
        out.extend_from_slice(&self.total_recs.to_le_bytes());
        out.extend_from_slice(&self.code_page.to_le_bytes());
        out.extend_from_slice(&self.template_locale.to_le_bytes());
        out.extend_from_slice(&self.sort_locale.to_le_bytes());
    }

    /// The row window a `QueryRows` serves. `CurrentRec` carries the 1-based
    /// MId of the row the cursor points at; the convention is that the NEXT
    /// `QueryRows` reads **past** `CurrentRec` (forward) or **before** it
    /// (backward), so a forward page does NOT re-serve the last row it
    /// returned. `Delta` is a signed offset applied to the cursor.
    ///
    /// Returns `(start, n, backward)`:
    ///   * forward (count > 0):  rows `[start .. start+n)`
    ///   * backward (count < 0): rows `[start-n .. start)` (clamped at 0)
    ///   * count == 0:           `n == 0` (no rows)
    fn query_window(&self, count: i32) -> (usize, usize, bool) {
        // 0-based row index the cursor sits on. CurrentRec MId M ⇒ row index
        // M-1. The next forward read starts AT this index only when MId==0
        // (begin); otherwise it starts PAST CurrentRec (index = M, i.e. the
        // row after the one MId M names), which is what stops the duplicate.
        let cursor_row = if self.current_rec == 0 {
            0
        } else {
            (self.current_rec as i64) + self.delta as i64
        };
        let cursor = cursor_row.max(0) as usize;
        if count < 0 {
            // `unsigned_abs()` avoids the i32::MIN overflow `-count` would
            // trigger; clamp the window so a backward read never underflows 0.
            let n = (count.unsigned_abs() as usize).min(cursor);
            (cursor, n, true)
        } else {
            // Cap the forward count at `MAX_QUERY_ROWS` per call (matches
            // `GetMatches`) so a runaway `Count` cannot materialise an
            // unbounded rowset; the caller pages via successive calls.
            let n = (count as usize).min(MAX_QUERY_ROWS as usize);
            (cursor, n, false)
        }
    }
}

// ---------------------------------------------------------------------------
// PropertyTagArray codec — MS-OXNSPI §2.2.1.6 (a 32-bit count followed by
// count × 4-byte PropertyTags). Over MAPI/HTTP the count is a 32-bit DWORD.
// ---------------------------------------------------------------------------

/// Decode a property-tag array off the cursor; the `Has<…>` flag has already
/// been consumed by the caller. Returns the tags and the *raw* byte span they
/// occupied (so a re-echo can round-trip verbatim on a partially-supported
/// set). Capped at a sane upper bound to resist a pathological count.
fn decode_tag_array(cur: &mut Buf<'_>) -> Result<Vec<u32>, NspiDecodeError> {
    const MAX_TAGS: u32 = 4096;
    let count = take_u32(cur)?;
    if count > MAX_TAGS {
        return Err(NspiDecodeError::ExcessLength);
    }
    let mut tags = Vec::with_capacity(count as usize);
    for _ in 0..count {
        tags.push(take_u32(cur)?);
    }
    Ok(tags)
}

/// Encode a property-tag array (count + tags) into `out`.
fn encode_tag_array(out: &mut Vec<u8>, tags: &[u32]) {
    let n = u32::try_from(tags.len()).unwrap_or(u32::MAX);
    out.extend_from_slice(&n.to_le_bytes());
    for t in tags {
        out.extend_from_slice(&t.to_le_bytes());
    }
}

// ---------------------------------------------------------------------------
// Property-value codec for the address-book rowsets.
//
// The MS-OXCMAPIHTTP address-book surfaces use the "flagged property value"
// shape per §2.2.1.5 `AddressBookFlaggedPropertyValue`:
//   Flag (1 byte):
//     0x0 — a `PropertyValue` with a value compatible with the property type
//           implied by the context (caller-supplied PropertyTag set) follows.
//     0x1 — the value is NOT present (the property is absent for this row).
//     0xA — a `PtypErrorCode` value follows: a 4-byte LE HRESULT explaining
//           why the property is unavailable (MAPI_E_NOT_FOUND for unknown props).
//
// The rowset that `QueryRows` / `ResolveNames` return uses caller-supplied
// property tags (each row cell is an `AddressBookFlaggedPropertyValue` whose
// surrounding type is the matching entry in the supplied PropertyTagArray). The
// `GetProps` response — which carries no caller-supplied tag set — uses the
// `AddressBookFlaggedPropertyValueWithType` variant that prepends a 4-byte
// `PropertyType` so each cell is self-describing.
//
// Within a cell the scalar values (Integer16/32/64, Boolean, Time, ErrorCode)
// are inlined directly; the variable-length values (PtypString/PtypString8/
// PtypBinary) carry a 4-byte little-endian byte count followed by the bytes
// (PtypString/PtypString8 include the trailing NUL in the byte count, per
// MS-OXCMAPIHTTP §2.2.1.1).
// ---------------------------------------------------------------------------

/// Decode a PtypString blob (UTF-16LE, NUL-terminated) — used by the
/// codec round-trip unit test to verify the encoder's byte count + NUL
/// invariant. Kept `cfg(test)` so the production build carries no decode
/// path beyond what `handle_resolve_names` needs (which reads name blobs via
/// `decode_name_blob` instead).
#[cfg(test)]
fn decode_pstring(bytes: Vec<u8>) -> Option<PropertyValue> {
    let mut trimmed = bytes;
    if trimmed.len() >= 2 && trimmed[trimmed.len() - 2] == 0 && trimmed[trimmed.len() - 1] == 0 {
        trimmed.truncate(trimmed.len() - 2);
    }
    if !trimmed.len().is_multiple_of(2) {
        trimmed.pop();
    }
    let units: Vec<u16> = trimmed
        .chunks_exact(2)
        .map(|c| u16::from_le_bytes([c[0], c[1]]))
        .collect();
    Some(PropertyValue::String(String::from_utf16_lossy(&units)))
}

/// Encode the scalar payload of a present cell (the inner value, WITHOUT the
/// leading Flag byte). Variable-length values carry the 4-byte byte count
/// (PtypString/PtypString8 include the trailing NUL in the count).
fn encode_scalar(out: &mut Vec<u8>, value: &PropertyValue) {
    match value {
        PropertyValue::Boolean(b) => out.push(if *b { 1 } else { 0 }),
        PropertyValue::Integer16(v) => out.extend_from_slice(&v.to_le_bytes()),
        PropertyValue::Integer32(v) => out.extend_from_slice(&v.to_le_bytes()),
        PropertyValue::Integer64(v) => out.extend_from_slice(&v.to_le_bytes()),
        PropertyValue::ErrorCode(v) => out.extend_from_slice(&v.to_le_bytes()),
        PropertyValue::Time(v) => out.extend_from_slice(&v.to_le_bytes()),
        PropertyValue::Guid(g) => out.extend_from_slice(g),
        PropertyValue::Floating32(f) => out.extend_from_slice(&f.to_le_bytes()),
        PropertyValue::Floating64(f) => out.extend_from_slice(&f.to_le_bytes()),
        PropertyValue::Currency(c) => out.extend_from_slice(&c.to_le_bytes()),
        PropertyValue::String(s) => {
            let mut buf = Vec::with_capacity(s.len() * 2 + 2);
            for u in s.encode_utf16() {
                buf.extend_from_slice(&u.to_le_bytes());
            }
            buf.extend_from_slice(&0u16.to_le_bytes()); // trailing NUL
            out.extend_from_slice(&(buf.len() as u32).to_le_bytes());
            out.extend_from_slice(&buf);
        }
        PropertyValue::String8(s) => {
            let mut buf = s.as_bytes().to_vec();
            buf.push(0); // trailing NUL
            out.extend_from_slice(&(buf.len() as u32).to_le_bytes());
            out.extend_from_slice(&buf);
        }
        PropertyValue::Binary(b) => {
            out.extend_from_slice(&(b.len() as u32).to_le_bytes());
            out.extend_from_slice(b);
        }
        PropertyValue::Null => {}
        PropertyValue::Opaque { .. } => {
            // Render as the documented NOT_FOUND error so the row survives with
            // a typed gap for an unsupported property.
            const MAPI_E_NOT_FOUND: u32 = 0x8004_0119;
            out.extend_from_slice(&MAPI_E_NOT_FOUND.to_le_bytes());
        }
    }
}

const CELL_FLAG_VALUE: u8 = 0x0;
#[allow(dead_code)] // documented wire value (§2.2.1.5 Flag 0x1); the gateway never emits an absent cell.
const CELL_FLAG_ABSENT: u8 = 0x1;
const CELL_FLAG_ERROR: u8 = 0xA;

/// Encode a single flagged cell (Flag + optional value) for the given tag.
/// `option` is `Some(value)` for a known property, `None` for an
/// unsupported/unknown property (emitted as a NOT_FOUND error cell). The
/// caller-supplied column set drives the surrounding type; we never need to
/// emit the type inline because the rowset cells mirror the tag order.
fn encode_cell(out: &mut Vec<u8>, tag: u32, option: Option<&PropertyValue>) {
    match option {
        Some(v) => {
            out.push(CELL_FLAG_VALUE);
            encode_scalar(out, v);
        }
        None => {
            // Unknown property ⇒ error cell (Mapi_E_NotFound). The caller's
            // tag carries PtypErrorCode semantics via the 0xA flag.
            let _ = tag;
            out.push(CELL_FLAG_ERROR);
            const MAPI_E_NOT_FOUND: u32 = 0x8004_0119;
            out.extend_from_slice(&MAPI_E_NOT_FOUND.to_le_bytes());
        }
    }
}

/// Encode a rowset (caller-tagged shape used by `QueryRows`/`ResolveNames`):
/// `RowCount(4 LE) + per-row (Flags(1) + per-cell flagged value)`.
/// Every row mirrors the caller's PropertyTag set order.
fn encode_rowset(out: &mut Vec<u8>, rows: &[Vec<PropertyValue>], tags: &[u32]) {
    let n = u32::try_from(rows.len()).unwrap_or(u32::MAX);
    out.extend_from_slice(&n.to_le_bytes());
    for row in rows {
        out.push(CELL_FLAG_VALUE); // row Flags: every cell present (or error cell).
        for (idx, tag) in tags.iter().enumerate() {
            encode_cell(out, *tag, row.get(idx));
        }
    }
}

/// Encode a rowset in the `…WithType` shape (used by `GetProps`, which carries
/// no caller-supplied tag set): each cell prepends its 2-byte `PropertyType`
/// (the low 2 bytes of the packed `u32` tag, MS-OXCDATA §2.9 — the type is a
/// `WORD`, not a `DWORD`) so the row is self-describing.
fn encode_rowset_with_type(
    out: &mut Vec<u8>,
    rows: &[(Vec<u32>, Vec<PropertyValue>)],
) {
    let n = u32::try_from(rows.len()).unwrap_or(u32::MAX);
    out.extend_from_slice(&n.to_le_bytes());
    for (tags, row) in rows {
        out.push(CELL_FLAG_VALUE); // row Flags.
        for (idx, tag) in tags.iter().enumerate() {
            // PropertyType is a 2-byte WORD (the low half of the packed tag).
            let pt: u16 = (*tag & 0xFFFF) as u16;
            out.extend_from_slice(&pt.to_le_bytes());
            encode_cell(out, *tag, row.get(idx));
        }
    }
}

// ---------------------------------------------------------------------------
// Fail-closed cursor readers returning `NspiDecodeError`. The shared `Buf`
// cursor from `rops.rs` already bounds-checks; these wrap its `take_*` into
// the NSPI-specific error enum so a short buffer never panics a codec.
// ---------------------------------------------------------------------------

/// Decode failures that arise from an untrusted NSPI request body. Kept a
/// distinct enum from `mapi::rops::DecodeError` so the NSPI codecs read with
/// their own contextual errors (the framing differs from the ROP layer).
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum NspiDecodeError {
    #[error("insufficient bytes")]
    Insufficient,
    #[error("length exceeds maximum or remaining buffer")]
    ExcessLength,
    #[error("invalid enumeration/flag value")]
    InvalidValue,
}

/// The shared `Buf` cursor from `rops.rs` returns `rops::DecodeError`; map it
/// into the NSPI-specific error so the question-mark operators in the codec
/// helpers convert automatically.
impl From<crate::mapi::rops::DecodeError> for NspiDecodeError {
    fn from(e: crate::mapi::rops::DecodeError) -> Self {
        use crate::mapi::rops::DecodeError as D;
        match e {
            D::Insufficient => Self::Insufficient,
            D::ExcessLength => Self::ExcessLength,
            D::InvalidValue | D::InvalidUtf8 => Self::InvalidValue,
            D::Trailing => Self::InvalidValue,
        }
    }
}

/// Read a 4-byte little-endian `u32` off the cursor.
fn take_u32(cur: &mut Buf<'_>) -> Result<u32, NspiDecodeError> {
    if cur.remaining() < 4 {
        return Err(NspiDecodeError::Insufficient);
    }
    // Reuse the underlying cursor by taking 4 bytes and re-interpreting.
    let raw = cur.take_bytes(4).map_err(|_| NspiDecodeError::Insufficient)?;
    Ok(u32::from_le_bytes([raw[0], raw[1], raw[2], raw[3]]))
}

/// Read a 4-byte little-endian signed `i32` off the cursor.
fn take_i32(cur: &mut Buf<'_>) -> Result<i32, NspiDecodeError> {
    Ok(take_u32(cur)? as i32)
}

/// Map an NSPI decode error to a transport `ResponseCode`. A malformed body is
/// `InvalidRequestBody` (12) per MS-OXCMAPIHTTP §2.2.3.3.3.
fn decode_err_to_code(_e: NspiDecodeError) -> ResponseCode {
    ResponseCode::InvalidRequestBody
}

// ---------------------------------------------------------------------------
// Success / failure response framing — MS-OXCMAPIHTTP §2.2.5.*.
//
// Every success response body is:
//   StatusCode (4 bytes LE, 0x00000000 = success)
//   + per-RPC response fields
//   + AuxiliaryBufferSize (4 bytes LE = 0)  // no auxiliary payload served
//
// Every failure response body is:
//   StatusCode (4 bytes LE, != 0)
//   + AuxiliaryBufferSize (4 bytes LE = 0)
//
// The `trailer` helper stamps the trailing zero-length auxiliary buffer; callers
// prepend their per-RPC fields between the StatusCode and the trailer.
// ---------------------------------------------------------------------------

/// Stamp the trailing `AuxiliaryBufferSize(0)` that closes every NSPI success
/// or failure response body. The gateway never serves an auxiliary buffer.
fn trailer(out: &mut Vec<u8>) {
    out.extend_from_slice(&0u32.to_le_bytes());
}

// ---------------------------------------------------------------------------
// DN synthesis — the canonical legacyExchangeDN the gateway advertises.
//
// Mirrors `oab::synth_dn` (`/o=Stalwart/ou=Exchange Administrative Group
// (FYDIBOHF23SPDLT)/cn=Recipients/cn=<localpart>`) so a DN resolved through
// NSPI matches the DN carried in the OAB download (audit gap §1.1) and the
// one MAPI/RopLogon accepts (`logon::recipient_local_name` parses the
// trailing `/cn=<localpart>`).
// ---------------------------------------------------------------------------

/// The local-part of an SMTP address (`alice` in `alice@example.com`).
fn email_local_part(email: &str) -> Option<String> {
    let at = email.find('@')?;
    let local = &email[..at];
    if local.is_empty() {
        return None;
    }
    Some(local.to_string())
}

/// Synthesise the canonical legacyExchangeDN for a mailbox address. Mirrors
/// `oab::synth_dn` exactly so the NSPI surface, the OAB download, and the
/// mailbox `RopLogon` agree on the same DN for one account.
fn synth_dn(email: &str) -> String {
    let local = email_local_part(email).unwrap_or_else(|| email.to_string());
    format!(
        "/o=Stalwart/ou=Exchange Administrative Group (FYDIBOHF23SPDLT)/cn=Recipients/cn={}",
        local.to_ascii_lowercase()
    )
}

/// Extract the `/cn=<localpart>` recipient local-part from a legacyExchangeDN.
/// Returns `None` when the DN is not a gateway-shaped recipient DN.
fn dn_local_part(dn: &str) -> Option<String> {
    let marker = "/cn=Recipients/cn=";
    let idx = dn.find(marker)?;
    let tail = &dn[idx + marker.len()..];
    if tail.is_empty() {
        return None;
    }
    Some(tail.to_string())
}

// ---------------------------------------------------------------------------
// GAL assembly — the address-book container the gateway serves.
//
// The container is the union of the operator-configured directory snapshot
// (when present) and the authenticated caller's own mailbox entry (always —
// so "Check Names" for self resolves even with no directory, and so the row the
// `RopLogon` DN points at is resolvable). Entries are ordered deterministically
// (alphabetical by lowercased SMTP) and assigned 1-based Minimal Entry IDs so a
// `DnToMinId` → `QueryRows` round-trip within one Bind resolves stabily.
// ---------------------------------------------------------------------------

/// One GAL entry — the resolved contact fields plus the derived DN.
#[derive(Clone)]
struct GalEntry {
    /// 1-based Minimal Entry ID (the row index in the in-memory table).
    mid: u32,
    display_name: String,
    email: String,
    /// The synthesised legacyExchangeDN.
    dn: String,
    title: Option<String>,
    company: Option<String>,
    department: Option<String>,
    phone: Option<String>,
}

/// TTL-cached directory snapshot shared across NSPI RPCs. A single Outlook
/// address-book interaction is a multi-RPC sequence (Bind → GetSpecialTable →
/// QueryColumns → QueryRows × N pages → ResolveNames → GetProps); resolving the
/// full directory (`search_blocking("*", Some(5000))`) on EVERY RPC is
/// O(rpcs × directory) work per user action and an admin-API amplification.
/// The cache stores only the *directory* side (without the caller's own entry,
/// which is assembled per call); a short TTL bounds staleness so a newly
/// provisioned mailbox surfaces within the window. Failures refresh nothing
/// (the stale snapshot, if any, is still served), matching the established
/// "degrade gracefully" codebase pattern.
pub struct GalCache(tokio::sync::RwLock<Option<Snapshot>>);

struct Snapshot {
    fetched_at: std::time::Instant,
    entries: Vec<GalEntry>,
}

/// GalCache TTL — how long a directory snapshot is reused before a refresh.
const GAL_CACHE_TTL: std::time::Duration = std::time::Duration::from_secs(60);

impl GalCache {
    /// Build an empty cache.
    pub fn new() -> Self {
        Self(tokio::sync::RwLock::new(None))
    }

    /// Return the cached directory snapshot if it is fresh enough, else `None`.
    /// Never blocks a reader on a refresh: callers refresh out-of-band.
    async fn get_if_fresh(&self) -> Option<Vec<GalEntry>> {
        let guard = self.0.read().await;
        match guard.as_ref() {
            Some(s) if s.fetched_at.elapsed() < GAL_CACHE_TTL => Some(s.entries.clone()),
            _ => None,
        }
    }

    /// Store a freshly-resolved directory snapshot (replacing any prior one).
    async fn store(&self, entries: Vec<GalEntry>) {
        let mut guard = self.0.write().await;
        *guard = Some(Snapshot {
            fetched_at: std::time::Instant::now(),
            entries,
        });
    }
}

impl Default for GalCache {
    fn default() -> Self {
        Self::new()
    }
}

/// Map a directory `Contact` to an in-process GAL entry (MId unknown until the
/// container is assembled). The DN is derived the same way the OAB download
/// derives it so a recipient resolved through NSPI carries the same DN the
/// recurring message body and the OAB carry.
fn contact_to_entry(contact: Contact) -> GalEntry {
    let display = if contact.display_name.is_empty() {
        contact.email.clone()
    } else {
        contact.display_name
    };
    let dn = synth_dn(&contact.email);
    GalEntry {
        mid: 0, // assigned by `assemble_gal`.
        display_name: display,
        email: contact.email,
        dn,
        title: contact.title,
        company: contact.company,
        department: contact.department,
        phone: contact.phone,
    }
}

/// Build a synthetic self-entry for the authenticated principal. `email` must
/// be the canonicalised principal email; the DN mirrors `synth_dn` so a
/// `RopLogon` for the same account resolves the same row.
fn self_entry(email: &str) -> GalEntry {
    // Derive a readable display name from the email local-part: capitalise the
    // first character and keep the rest (e.g. `alice@example.com` -> `Alice`).
    // When there is no local-part (e.g. `@example.com`) fall back to the email
    // itself; an empty local-part never advertises a fabricated identity.
    let display = match email_local_part(email) {
        Some(local) => {
            let mut chars = local.chars();
            match chars.next() {
                Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
                None => email.to_string(),
            }
        }
        None => email.to_string(),
    };
    GalEntry {
        mid: 0,
        display_name: display,
        email: email.to_string(),
        dn: synth_dn(email),
        title: None,
        company: None,
        department: None,
        phone: None,
    }
}

/// Assemble the GAL container: union the directory snapshot (when present) with
/// the caller's own entry, de-duping by lowercased SMTP, ordering alphabetically
/// and assigning 1-based Minimal Entry IDs. Runs the (blocking) directory query
/// on a `spawn_blocking` task; `JoinError`s are logged (redacted email) and the
/// GAL degrades to the caller-only stub.
async fn assemble_gal(
    state: &MapiState,
    principal_email: &str,
) -> Vec<GalEntry> {
    use std::collections::BTreeMap;

    // Resolve the directory side ONCE per `GAL_CACHE_TTL` window (shared across
    // every NSPI RPC in every concurrent session), so a multi-RPC Outlook
    // address-book handshake reuses one directory snapshot instead of issuing a
    // full `search_blocking` per RPC. The caller's own entry is added per call.
    let directory_entries: Vec<GalEntry> = match state.directory.clone() {
        Some(dir) => {
            if let Some(cache) = &state.gal_cache {
                if let Some(cached) = cache.get_if_fresh().await {
                    cached
                } else {
                    let dir_clone = dir.clone();
                    match tokio::task::spawn_blocking(move || {
                        dir_clone.search_blocking("*", Some(5000))
                    })
                    .await
                    {
                        Ok(Ok(SearchResult { mut contacts, .. })) => {
                            contacts.sort_by(|a, b| a.email.cmp(&b.email));
                            let entries: Vec<GalEntry> =
                                contacts.into_iter().map(contact_to_entry).collect();
                            cache.store(entries.clone()).await;
                            entries
                        }
                        Ok(Err(e)) => {
                            tracing::warn!(
                                target: "nspi",
                                error = %e,
                                principal = redact_email(principal_email),
                                "Directory GAL query failed; serving caller-only stub"
                            );
                            Vec::new()
                        }
                        Err(e) => {
                            tracing::warn!(
                                target: "nspi",
                                error = %e,
                                principal = redact_email(principal_email),
                                "Directory GAL task join failed; serving caller-only stub"
                            );
                            Vec::new()
                        }
                    }
                }
            } else {
                // No cache wired (unit-test fixtures): resolve directly each call.
                let dir_clone = dir.clone();
                match tokio::task::spawn_blocking(move || {
                    dir_clone.search_blocking("*", Some(5000))
                })
                .await
                {
                    Ok(Ok(SearchResult { mut contacts, .. })) => {
                        contacts.sort_by(|a, b| a.email.cmp(&b.email));
                        contacts.into_iter().map(contact_to_entry).collect()
                    }
                    Ok(Err(e)) => {
                        tracing::warn!(
                            target: "nspi",
                            error = %e,
                            principal = redact_email(principal_email),
                            "Directory GAL query failed; serving caller-only stub"
                        );
                        Vec::new()
                    }
                    Err(e) => {
                        tracing::warn!(
                            target: "nspi",
                            error = %e,
                            principal = redact_email(principal_email),
                            "Directory GAL task join failed; serving caller-only stub"
                        );
                        Vec::new()
                    }
                }
            }
        }
        None => Vec::new(),
    };

    // De-dup by lowercased SMTP and always include the caller's own entry.
    let mut by_email: BTreeMap<String, GalEntry> = BTreeMap::new();
    for entry in directory_entries {
        by_email.insert(entry.email.to_ascii_lowercase(), entry);
    }
    let self_e = self_entry(principal_email);
    by_email
        .entry(self_e.email.to_ascii_lowercase())
        .or_insert(self_e);

    // Assign 1-based Minimal Entry IDs in the (now-sorted) alphabetical order.
    let mut out: Vec<GalEntry> = by_email.into_values().collect();
    // BTreeMap iteration is already sorted by key; re-sort defensively.
    out.sort_by_key(|a| a.email.to_ascii_lowercase());
    for (i, entry) in out.iter_mut().enumerate() {
        entry.mid = u32::try_from(i + 1).unwrap_or(u32::MAX);
    }
    out
}

/// Mask an email for log lines (keep the local-part length hidden): show the
/// domain only. Per-house-style, the directory email itself is not logged in
/// the clear.
fn redact_email(email: &str) -> String {
    match email.split_once('@') {
        Some((local, domain)) => {
            let local_len = local.len();
            format!("***({}@{})", local_len, domain)
        }
        None => "***".to_string(),
    }
}

/// Map a known address-book property tag to the `PropertyValue` for a given
/// GAL entry (or `None` when the property is not one the gateway surfaces).
/// Unknown tags become the NOT_FOUND error cell in `encode_cell`.
///
/// Wire PropertyTag layout per MS-OXCDATA §2.9 is `PropertyType(2 LE) +
/// PropertyId(2 LE)`; the packed `u32` (`from_le_bytes`) therefore carries the
/// TYPE in the low half and the ID in the high half. Decode accordingly.
fn entry_property(entry: &GalEntry, tag: u32) -> Option<PropertyValue> {
    let id = (tag >> 16) as u16;
    let ty = crate::mapi::data::PropertyType::from_u16((tag & 0xFFFF) as u16);
    match id {
        PR_DISPLAY_NAME_ABOOK | PR_TRANSMIT_DISPLAY_NAME_ABK => {
            string_value(ty, &entry.display_name)
        }
        PR_7BIT_DISPLAY_NAME => string8_value(ty, &entry.display_name),
        PR_EMAIL_ADDRESS_ABOOK | PR_SMTP_ADDRESS => string_value(ty, &entry.email),
        PR_ACCOUNT => {
            let local = email_local_part(&entry.email).unwrap_or_else(|| entry.email.clone());
            string_value(ty, &local)
        }
        PR_ADDRESS_TYPE => string_value(ty, "SMTP"),
        PR_OBJECT_TYPE => Some(PropertyValue::Integer32(MAPI_MAILUSER as i32)),
        PR_DISPLAY_TYPE | PR_DISPLAY_TYPE_EX => {
            Some(PropertyValue::Integer32(DT_MAILUSER as i32))
        }
        PR_TEMPLATE_ID => Some(PropertyValue::Integer32(0)), // not a template-bound row.
        PR_INSTANCE_KEY | PR_RECORD_KEY_ABOOK | PR_MAPPING_SIGNATURE => {
            Some(PropertyValue::Binary(entry.mid.to_le_bytes().to_vec()))
        }
        PR_SEARCH_KEY_ABOOK => {
            // `SMTP:<UPCASED email>` per MS-OXCDATA §2.6.8.
            let mut v = b"SMTP:".to_vec();
            v.extend_from_slice(entry.email.to_ascii_uppercase().as_bytes());
            Some(PropertyValue::Binary(v))
        }
        PR_ENTRYID_ABOOK => Some(PropertyValue::Binary(abook_entry_id(entry))),
        PR_SEND_RICH_INFO => Some(PropertyValue::Boolean(true)),
        PR_TITLE_ABOOK => entry
            .title
            .clone()
            .and_then(|s| string_value(ty, &s)),
        PR_COMPANY_NAME_ABOOK => entry
            .company
            .clone()
            .and_then(|s| string_value(ty, &s)),
        PR_DEPARTMENT_NAME => entry
            .department
            .clone()
            .and_then(|s| string_value(ty, &s)),
        PR_BUSINESS_TEL_ABOOK | PR_PRIMARY_TEL_ABOOK | PR_MOBILE_TEL_ABOOK | PR_HOME_TEL_ABOOK => {
            entry
                .phone
                .clone()
                .and_then(|s| string_value(ty, &s))
        }
        _ => None,
    }
}

/// Build a string cell honouring the tag's PropertyType (PtypString vs
/// PtypString8); unknown types degrade to None (NOT_FOUND cell).
fn string_value(t: PropertyType, s: &str) -> Option<PropertyValue> {
    match t {
        PropertyType::PTYP_STRING => Some(PropertyValue::String(s.to_string())),
        PropertyType::PTYP_STRING8 => Some(PropertyValue::String8(s.to_string())),
        _ => None,
    }
}

/// Build a String8 cell honouring the tag's PropertyType.
fn string8_value(t: PropertyType, s: &str) -> Option<PropertyValue> {
    match t {
        PropertyType::PTYP_STRING8 => Some(PropertyValue::String8(s.to_string())),
        PropertyType::PTYP_STRING => Some(PropertyValue::String(s.to_string())),
        _ => None,
    }
}

/// Build a long-term address-book EntryId for the row (MS-OXCDATA §2.6.4 —
/// the "Permanent Entry ID" for a mail user). Packed as:
///   Flags(4)=0  + ProviderUID(16)=EMSMDB-AB + Version(1)=0 + Type(1)=0
///   + X500DN (null-terminated UTF-8).
fn abook_entry_id(entry: &GalEntry) -> Vec<u8> {
    // The Exchange address-book provider GUID {001A0400-…} identifies the
    // EMSMDB address-book EntryId. We emit the documented 16-byte prefix so a
    // client comparing EntryIds round-trips it unchanged.
    const AB_PROVIDER_UID: [u8; 16] = [
        0x00, 0x04, 0x00, 0x00, 0x12, 0xB6, 0xD0, 0x44, 0xB8, 0x5C, 0x4B, 0x49, 0x07, 0x00, 0x00,
        0x00,
    ];
    let mut out = Vec::with_capacity(4 + 16 + 1 + 1 + entry.dn.len() + 1);
    out.extend_from_slice(&0u32.to_le_bytes()); // Flags = 0 (no MAPI one-off bits).
    out.extend_from_slice(&AB_PROVIDER_UID);
    out.push(0); // Version.
    out.push(0); // Type = DT_MAILUSER.
    out.extend_from_slice(entry.dn.as_bytes());
    out.push(0); // null-terminated.
    out
}

// ---------------------------------------------------------------------------
// Row materialisation — map a GAL entry to a caller-requested column set.
// ---------------------------------------------------------------------------

/// Build the (type, id) → u32 packed tag the directory codec stores.
fn pack_tag(ty: PropertyType, id: u16) -> u32 {
    // wire = type(2 LE) + id(2 LE) ⟹ packed u32 = (id << 16) | type.
    ((id as u32) << 16) | (ty.to_u16() as u32)
}

/// The default column set Outlook requests of `Bind`/`QueryRows` when it does
/// not supply its own PropertyTagArray. Serves the "Outlook issues Bind then
/// QueryRows with no tags" fallback path so the very first resolve always
/// returns a non-empty row rather than an empty column set.
fn default_column_tags() -> Vec<u32> {
    use PropertyType as T;
    vec![
        pack_tag(T::PTYP_BINARY, PR_ENTRYID_ABOOK),
        pack_tag(T::PTYP_STRING, PR_DISPLAY_NAME_ABOOK),
        pack_tag(T::PTYP_STRING, PR_EMAIL_ADDRESS_ABOOK),
        pack_tag(T::PTYP_STRING8, PR_ADDRESS_TYPE),
        pack_tag(T::PTYP_STRING, PR_SMTP_ADDRESS),
        pack_tag(T::PTYP_INTEGER32, PR_OBJECT_TYPE),
        pack_tag(T::PTYP_INTEGER32, PR_DISPLAY_TYPE),
        pack_tag(T::PTYP_INTEGER32, PR_DISPLAY_TYPE_EX),
        pack_tag(T::PTYP_BINARY, PR_INSTANCE_KEY),
        pack_tag(T::PTYP_BINARY, PR_RECORD_KEY_ABOOK),
        pack_tag(T::PTYP_BOOLEAN, PR_SEND_RICH_INFO),
    ]
}

/// Materialise one GAL row for the requested tag set. An unsupported/
/// unknown tag resolves to `PropertyValue::ErrorCode(MAPI_E_NOT_FOUND)` — a
/// *present* cell (Flag 0x0) carrying the `MAPI_E_NOT_FOUND` HRESULT, the
/// spec-correct shape for "property not found on this object" (MS-OXNSPI
/// §2.2.1.5). It MUST NOT be `PropertyValue::Null`: a value-flagged cell with
/// zero payload bytes is malformed and desynchronises client parsing.
fn materialise_row(entry: &GalEntry, tags: &[u32]) -> Vec<PropertyValue> {
    const MAPI_E_NOT_FOUND: u32 = 0x8004_0119;
    tags.iter()
        .map(|tag| entry_property(entry, *tag))
        .map(|opt| opt.unwrap_or(PropertyValue::ErrorCode(MAPI_E_NOT_FOUND)))
        .collect()
}

/// Materialise the requested rows from the in-memory GAL container.
///   * forward: rows `[start .. start+n)` (clamped at container end)
///   * backward: rows `[start-n .. start)` (clamped at 0), in table order so
///     the rowset mirrors the forward reading order Outlook re-paginates from
fn materialise_rows(container: &[GalEntry], tags: &[u32], start: usize, n: usize, backward: bool) -> Vec<Vec<PropertyValue>> {
    let (lo, hi) = if n == 0 {
        return Vec::new();
    } else if backward {
        let lo = start.saturating_sub(n);
        let hi = start;
        (lo, hi)
    } else {
        let lo = start;
        let hi = (start + n).min(container.len());
        (lo, hi)
    };
    if lo >= hi || hi > container.len() {
        return Vec::new();
    }
    container[lo..hi]
        .iter()
        .map(|entry| materialise_row(entry, tags))
        .collect()
}

// ---------------------------------------------------------------------------
// Authentication.
//
// Every `/mapi/nspi` RPC reuses the same `AuthVerifier::verify` the mailbox
// path uses, so the GAL surface never leaks recipient PII without a valid
// Stalwart credential. The password arrives in `MapiRequest.password`
// (wrapped into a `SecretString` for the check only); the username is
// canonicalised against the configured mail domain (the same canonicalisation
// every other authenticated path uses).
// ---------------------------------------------------------------------------

/// Validate the request's Basic credentials against Stalwart; returns the
/// canonical principal email on success, `None` on absent/failed auth.
async fn authenticate(req: &MapiRequest, state: &MapiState) -> Option<String> {
    let user = req.username.clone()?;
    let pass = req.password.clone()?;
    if user.is_empty() || pass.is_empty() {
        return None;
    }
    // Canonicalise the same way the mailbox path and the Autodiscover auth gate do.
    let principal = canonicalize_username(&user, &state.cfg.mail_domain);
    let secret = secrecy::SecretString::new(pass.into());
    if state.auth.verify(&principal, secret.expose_secret()).await {
        Some(principal)
    } else {
        None
    }
}

/// The transport-level anonymous-access rejection code (MS-OXCMAPIHTTP §2.2.3.4
/// `ResponseCode::NoPrivilege` = 11). Matched with the mailbox logon path.
fn anonymous_rejected(req: MapiRequest) -> MapiResponse {
    MapiResponse::error(ResponseCode::NoPrivilege, req.request_id)
}

// ---------------------------------------------------------------------------
// MAPI_E_* HRESULTs the NSPI surface uses (MS-OXNSPI §3.1.4.1). These are the
// *application-level* return values inside a success-shaped response body
// (the transport layer remains 200/SUCCESS); Outlook reads the body
// StatusCode to know the NSPI RPC's own outcome.
// ---------------------------------------------------------------------------

const NSPI_SUCCESS: u32 = 0x0000_0000;
const NSPI_NAME_NOT_FOUND: u32 = 0x8004_0117; // MAPI_E_NOT_FOUND alias for AB
const NSPI_TABLE_TOO_BIG: u32 = 0x8004_0106; // MAPI_E_TABLE_TOO_BIG

// ---------------------------------------------------------------------------
// Per-RPC handlers (§2.2.5.*). Each is a thin frame around `assemble_gal` +
// the codec helpers. STAT echo rules: server MUST NOT mutate SortType /
// ContainerID / CodePage / TemplateLocale / SortLocale; it MAY set
// CurrentRec / Delta / NumPos / TotalRecs as the cursor advances.
// ---------------------------------------------------------------------------

/// §2.2.5.1.Bind (MS-OXNSPI §3.1.4.1.1) — establish a Session Context. The
/// gateway is stateless across the `/mapi/nspi` endpoint (it resolves the
/// directory via the shared TTL cache), so the "session context" it returns is
/// a single freshly-initialised STAT carrying the container's row count.
///
/// Bind's documented inputs are `Flags` + `CodePage` + `LocaleId` (NOT a
/// STAT — that is a Bind *output*). To be robust to a client transport that
/// nonetheless prepends a STAT, the handler tolerates BOTH shapes:
///
/// - if ≥36 bytes remain after Flags ⇒ a STAT was sent (consume + honour its
///   CodePage/SortLocale context), then any trailing CodePage/LocaleId;
/// - otherwise ⇒ the Bind input is Flags+CodePage+LocaleId (or just Flags);
///   a default STAT is built from the container.
///
/// The echoed `CodePage`/`TemplateLocale`/`SortLocale` are preserved from the
/// input STAT (when present) so a locale-sensitive client's preferences survive.
fn handle_bind(req: &MapiRequest, container: &[GalEntry]) -> MapiResponse {
    let mut cur = Buf::new(&req.body);
    let _flags = match take_u32(&mut cur) {
        Ok(v) => v,
        Err(e) => return decode_err_response(e, req),
    };
    // Tolerant: a STAT input is optional for Bind. If enough bytes remain it
    // is consumed; otherwise the remaining advisory CodePage/LocaleId are the
    // Bind inputs and we build a default STAT for the response.
    let stat = if cur.remaining() >= Stat::ENCODED_LEN {
        match Stat::decode(&mut cur) {
            Ok(s) => Some(s),
            Err(e) => return decode_err_response(e, req),
        }
    } else {
        None
    };
    // Tolerate trailing CodePage(4) + LocaleId(4) either way.
    let _ = take_u32(&mut cur);
    let _ = take_u32(&mut cur);

    let body = render_with_container(container, |out, container| {
        out.extend_from_slice(&NSPI_SUCCESS.to_le_bytes());
        // Preserve the client's locale/context preferences when a STAT was sent;
        // otherwise build a default cursor-at-top STAT for the container.
        let mut echoed = match stat {
            Some(s) => s,
            None => Stat::default_for(container.len()),
        };
        echoed.current_rec = 0; // initial cursor: before the first row.
        echoed.delta = 0;
        echoed.num_pos = 0;
        echoed.total_recs = u32::try_from(container.len()).unwrap_or(u32::MAX);
        echoed.encode(out);
        trailer(out);
    });
    MapiResponse::success(req.request_id.clone(), "Bind", None, body)
}

/// §2.2.5.18 Unbind — conclude the Session Context. Stateless: succeed.
/// Body shape mirrors every other NSPI success response: `StatusCode(0)` +
/// trailing `AuxiliaryBufferSize(0)`. Note `NSPI_SUCCESS == 0`, so the single
/// `NSPI_SUCCESS.to_le_bytes()` write IS the StatusCode (do not prepend a
/// second `success_prefix()` — that would emit a duplicated 0x00000000).
fn handle_unbind(req: &MapiRequest, _container: &[GalEntry]) -> MapiResponse {
    let mut out = Vec::with_capacity(8);
    out.extend_from_slice(&NSPI_SUCCESS.to_le_bytes());
    trailer(&mut out);
    MapiResponse::success(req.request_id.clone(), "Unbind", None, out)
}

/// §2.2.5.17 UpdateStat — advance the table cursor. Returns the echoed
/// STAT positioned at the new record.
fn handle_update_stat(req: &MapiRequest, container: &[GalEntry]) -> MapiResponse {
    let mut cur = Buf::new(&req.body);
    let _flags = match take_u32(&mut cur) {
        Ok(v) => v,
        Err(e) => return decode_err_response(e, req),
    };
    let stat = match Stat::decode(&mut cur) {
        Ok(s) => s,
        Err(e) => return decode_err_response(e, req),
    };
    let body = render_with_container(container, |out, container| {
        out.extend_from_slice(&NSPI_SUCCESS.to_le_bytes());
        let echoed = clamp_stat(stat, container.len());
        echoed.encode(out);
        trailer(out);
    });
    MapiResponse::success(req.request_id.clone(), "UpdateStat", None, body)
}

/// §2.2.5.12 QueryRows — return up to `Count` rows from the table cursor.
fn handle_query_rows(req: &MapiRequest, container: &[GalEntry]) -> MapiResponse {
    let mut cur = Buf::new(&req.body);
    let _flags = match take_u32(&mut cur) {
        Ok(v) => v,
        Err(e) => return decode_err_response(e, req),
    };
    let stat = match Stat::decode(&mut cur) {
        Ok(s) => s,
        Err(e) => return decode_err_response(e, req),
    };
    // HasColumns(1) + optional PropertyTagArray.
    let has_cols = match cur.take_u8() {
        Ok(v) => v,
        Err(_e) => return decode_err_response(NspiDecodeError::Insufficient, req),
    };
    let tags = if has_cols != 0 {
        match decode_tag_array(&mut cur) {
            Ok(t) => t,
            Err(e) => return decode_err_response(e, req),
        }
    } else {
        default_column_tags()
    };
    // Count (4 bytes) — the requested row count (signed; negative ⇒ backwards).
    let count_raw = match take_u32(&mut cur) {
        Ok(v) => v,
        Err(e) => return decode_err_response(e, req),
    };
    let count = count_raw as i32;

    let body = render_with_container(container, |out, container| {
        let total = container.len();
        let (start, n, backward) = stat.query_window(count);
        let rows = materialise_rows(container, &tags, start, n, backward);
        out.extend_from_slice(&NSPI_SUCCESS.to_le_bytes());
        // Echo STAT advancing the cursor to the row just past the served window
        // (forward) or to the first served row (backward). `current_rec` is a
        // 1-based MId; `num_pos` mirrors it. `delta` is cleared to 0 after the
        // read so the next QueryRows' `Delta` offset is interpreted from the new
        // cursor rather than re-applying the prior offset.
        let mut echoed = stat;
        let served = rows.len();
        if served == 0 {
            // No rows served: the cursor does not move.
            echoed.delta = 0;
        } else if backward {
            // Served `[start-n .. start)` ⇒ the new cursor is the first served
            // row's MId so a subsequent forward read resumes there.
            let first_mid = u32::try_from(start.saturating_sub(n) + 1).unwrap_or(u32::MAX);
            echoed.current_rec = first_mid;
            echoed.delta = 0;
            echoed.num_pos = first_mid;
        } else {
            // Served `[start .. start+served)` ⇒ the new cursor's MId is the
            // row just past the window (start+served+1 as a 1-based MId), so the
            // NEXT forward QueryRows reads the row after the last served one —
            // no duplicate. Clamp at total+1 (EOF sentinel stays readable).
            let next_mid = u32::try_from(start + served + 1).unwrap_or(u32::MAX);
            echoed.current_rec = next_mid;
            echoed.delta = 0;
            echoed.num_pos = u32::try_from(start + served).unwrap_or(u32::MAX);
        }
        echoed.total_recs = u32::try_from(total).unwrap_or(u32::MAX);
        echoed.encode(out);
        // HasColumnsAlready(1): the rowset mirrors the supplied column set.
        out.push(0xFF);
        encode_rowset(out, &rows, &tags);
        trailer(out);
    });
    MapiResponse::success(req.request_id.clone(), "QueryRows", None, body)
}

/// §2.2.5.4 DnToMinId — convert a list of DNs to Minimal Entry IDs.
/// Returns one MId per DN (0 when the DN is not present in the container).
fn handle_dn_to_min_id(req: &MapiRequest, container: &[GalEntry]) -> MapiResponse {
    let mut cur = Buf::new(&req.body);
    let _flags = match take_u32(&mut cur) {
        Ok(v) => v,
        Err(e) => return decode_err_response(e, req),
    };
    let dn_count = match take_u32(&mut cur) {
        Ok(v) => v,
        Err(e) => return decode_err_response(e, req),
    };
    if dn_count > MAX_DN_TO_MID {
        return min_id_too_big(req, "DNToMId");
    }
    let mut dns: Vec<String> = Vec::with_capacity(dn_count as usize);
    for _ in 0..dn_count {
        let n = match take_u32(&mut cur) {
            Ok(v) => v,
            Err(e) => return decode_err_response(e, req),
        };
        const MAX_DN: u32 = 8 * 1024;
        if n > MAX_DN {
            return decode_err_response(NspiDecodeError::ExcessLength, req);
        }
        let bytes = match cur.take_bytes(n as usize) {
            Ok(b) => b,
            Err(_) => return decode_err_response(NspiDecodeError::Insufficient, req),
        };
        // DN is NUL-terminated UTF-8 within the carried byte count.
        let trimmed = bytes.strip_suffix(&[0]).unwrap_or(bytes);
        match std::str::from_utf8(trimmed) {
            Ok(s) => dns.push(s.to_string()),
            Err(_) => return decode_err_response(NspiDecodeError::InvalidValue, req),
        }
    }

    let body = render_with_container(container, |out, container| {
        out.extend_from_slice(&NSPI_SUCCESS.to_le_bytes());
        // MIdCount(4) + MinIds().
        let ids: Vec<u32> = dns
            .iter()
            .map(|dn| {
                // Match by legacyExchangeDN, then fall back to the local-part.
                container
                    .iter()
                    .find(|e| e.dn == *dn)
                    .map(|e| e.mid)
                    .or_else(|| {
                        dn_local_part(dn).and_then(|lp| {
                            container
                                .iter()
                                .find(|e| email_local_part(&e.email).as_deref() == Some(&lp))
                                .map(|e| e.mid)
                        })
                    })
                    .unwrap_or(0)
            })
            .collect();
        out.extend_from_slice(&(ids.len() as u32).to_le_bytes());
        for id in &ids {
            out.extend_from_slice(&id.to_le_bytes());
        }
        trailer(out);
    });
    MapiResponse::success(req.request_id.clone(), "DNToMId", None, body)
}

/// §2.2.5.14 ResolveNames — ambiguous-name resolution. Maps a passed array of
/// names (one per ANR probe) to the matching rows. The gateway matches each
/// name case-insensitively against the GAL by email or display-name prefix;
/// an unmatched name yields a `NSPI_NAME_NOT_FOUND` errored row so Outlook
/// surfaces "name not found" rather than dropping the call.
fn handle_resolve_names(req: &MapiRequest, container: &[GalEntry]) -> MapiResponse {
    let mut cur = Buf::new(&req.body);
    let _flags = match take_u32(&mut cur) {
        Ok(v) => v,
        Err(e) => return decode_err_response(e, req),
    };
    let stat = match Stat::decode(&mut cur) {
        Ok(s) => s,
        Err(e) => return decode_err_response(e, req),
    };
    // HasPropTags(1) + optional PropertyTagArray (the column set for the resolved rows).
    let has_tags = match cur.take_u8() {
        Ok(v) => v,
        Err(_e) => return decode_err_response(NspiDecodeError::Insufficient, req),
    };
    let tags = if has_tags != 0 {
        match decode_tag_array(&mut cur) {
            Ok(t) => t,
            Err(e) => return decode_err_response(e, req),
        }
    } else {
        default_column_tags()
    };
    // HasNames(1) + optional NameCount(4) + NameArray (each: 4-byte byte count + UTF-16LE incl. NUL).
    let has_names = match cur.take_u8() {
        Ok(v) => v,
        Err(_e) => return decode_err_response(NspiDecodeError::Insufficient, req),
    };
    let mut names: Vec<String> = Vec::new();
    if has_names != 0 {
        let name_count = match take_u32(&mut cur) {
            Ok(v) => v,
            Err(e) => return decode_err_response(e, req),
        };
        if name_count > MAX_RESOLVE_NAMES {
            return decode_err_response(NspiDecodeError::ExcessLength, req);
        }
        for _ in 0..name_count {
            let n = match take_u32(&mut cur) {
                Ok(v) => v,
                Err(e) => return decode_err_response(e, req),
            };
            const MAX_NAME: u32 = 4096;
            if n > MAX_NAME {
                return decode_err_response(NspiDecodeError::ExcessLength, req);
            }
            let bytes = match cur.take_bytes(n as usize) {
                Ok(b) => b,
                Err(_) => return decode_err_response(NspiDecodeError::Insufficient, req),
            };
            names.push(decode_name_blob(bytes));
        }
    }

    let _ = stat;
    let body = render_with_container(container, |out, container| {
        // For each name: find the first GAL entry whose email or display name
        // matches case-insensitively (prefix or exact). A match ⇒ the row; an
        // unmatched name ⇒ an errored row (the rowset carries one row per
        // name, ordered, so Outlook indexes MatchStatus by position).
        let mut rows: Vec<Vec<PropertyValue>> = Vec::new();
        let mut any_not_found = false;
        for name in &names {
            let needle = name.to_ascii_lowercase();
            // An empty needle would `starts_with`-match EVERY row (every
            // string starts with ""), flooding the result with the whole GAL.
            // Treat it as no-match so an empty ResolveNames probe yields the
            // NOT_FOUND row Outlook expects, not the entire directory.
            let found = if needle.is_empty() {
                None
            } else {
                container.iter().find(|e| {
                    let em = e.email.to_ascii_lowercase();
                    let dn = e.display_name.to_ascii_lowercase();
                    // ANR: match if the needle equals or prefixes either field,
                    // or matches the bare local-part. `starts_with` already
                    // covers exact equality, so the explicit `==` checks are
                    // redundant and removed.
                    em.starts_with(&needle)
                        || dn.starts_with(&needle)
                        || email_local_part(&e.email)
                            .map(|lp| lp.to_ascii_lowercase().starts_with(&needle))
                            .unwrap_or(false)
                })
            };
            match found {
                Some(entry) => rows.push(materialise_row(entry, &tags)),
                None => {
                    any_not_found = true;
                    // An unmatched name ⇒ a row whose every cell is a
                    // NOT_FOUND error cell (present cell carrying the HRESULT;
                    // never a zero-payload Null cell).
                    const MAPI_E_NOT_FOUND: u32 = 0x8004_0119;
                    rows.push(tags
                        .iter()
                        .map(|_| PropertyValue::ErrorCode(MAPI_E_NOT_FOUND))
                        .collect());
                }
            }
        }
        let status = if any_not_found { NSPI_NAME_NOT_FOUND } else { NSPI_SUCCESS };
        out.extend_from_slice(&status.to_le_bytes());
        out.push(0xFF); // HasRowsAlready.
        encode_rowset(out, &rows, &tags);
        trailer(out);
    });
    MapiResponse::success(req.request_id.clone(), "ResolveNames", None, body)
}

/// §2.2.5.5 GetMatches — like QueryRows, but restricted by the supplied
/// ANR-style needle carried in STAT-resident `CurrentRec` semantics. The
/// gateway treats GetMatches as "resolve every row that matches the most
/// recent ResolveNames needle"; without a needle it returns the whole
/// container (clamped to MAX_QUERY_ROWS), matching the Exchange behaviour
/// for an unrestricted Explicit Table.
fn handle_get_matches(req: &MapiRequest, container: &[GalEntry]) -> MapiResponse {
    let mut cur = Buf::new(&req.body);
    let _flags = match take_u32(&mut cur) {
        Ok(v) => v,
        Err(e) => return decode_err_response(e, req),
    };
    let stat = match Stat::decode(&mut cur) {
        Ok(s) => s,
        Err(e) => return decode_err_response(e, req),
    };
    let has_cols = match cur.take_u8() {
        Ok(v) => v,
        Err(_e) => return decode_err_response(NspiDecodeError::Insufficient, req),
    };
    let tags = if has_cols != 0 {
        match decode_tag_array(&mut cur) {
            Ok(t) => t,
            Err(e) => return decode_err_response(e, req),
        }
    } else {
        default_column_tags()
    };
    // HasCount(1) + optional Count(4).
    let has_count = match cur.take_u8() {
        Ok(v) => v,
        Err(_e) => return decode_err_response(NspiDecodeError::Insufficient, req),
    };
    let count = if has_count != 0 {
        match take_u32(&mut cur) {
            Ok(v) => v.min(MAX_QUERY_ROWS),
            Err(e) => return decode_err_response(e, req),
        }
    } else {
        MAX_QUERY_ROWS
    };

    let _ = stat;
    let body = render_with_container(container, |out, container| {
        let n = (count as usize).min(container.len());
        let rows = materialise_rows(container, &tags, 0, n, false);
        out.extend_from_slice(&NSPI_SUCCESS.to_le_bytes());
        out.push(0xFF); // HasRowsAlready.
        encode_rowset(out, &rows, &tags);
        trailer(out);
    });
    MapiResponse::success(req.request_id.clone(), "GetMatches", None, body)
}

/// §2.2.5.11 GetProps — return the properties for a set of Minimal Entry IDs.
/// The gateway carries no caller-supplied tag set for GetProps, so each cell
/// prepends its PropertyType (the `…WithType` row shape).
fn handle_get_props(req: &MapiRequest, container: &[GalEntry]) -> MapiResponse {
    let mut cur = Buf::new(&req.body);
    let _flags = match take_u32(&mut cur) {
        Ok(v) => v,
        Err(e) => return decode_err_response(e, req),
    };
    let _stat = match Stat::decode(&mut cur) {
        Ok(s) => s,
        Err(e) => return decode_err_response(e, req),
    };
    let has_tags = match cur.take_u8() {
        Ok(v) => v,
        Err(_e) => return decode_err_response(NspiDecodeError::Insufficient, req),
    };
    let tags = if has_tags != 0 {
        match decode_tag_array(&mut cur) {
            Ok(t) => t,
            Err(e) => return decode_err_response(e, req),
        }
    } else {
        default_column_tags()
    };
    let mid_count = match take_u32(&mut cur) {
        Ok(v) => v,
        Err(e) => return decode_err_response(e, req),
    };
    const MAX_MIDS: u32 = 4096;
    if mid_count > MAX_MIDS {
        return min_id_too_big(req, "GetProps");
    }
    let mut mids: Vec<u32> = Vec::with_capacity(mid_count as usize);
    for _ in 0..mid_count {
        mids.push(match take_u32(&mut cur) {
            Ok(v) => v,
            Err(e) => return decode_err_response(e, req),
        });
    }

    let body = render_with_container(container, |out, container| {
        const MAPI_E_NOT_FOUND: u32 = 0x8004_0119;
        let mut rows: Vec<(Vec<u32>, Vec<PropertyValue>)> = Vec::new();
        let mut any_not_found = false;
        for mid in &mids {
            match container.iter().find(|e| e.mid == *mid) {
                Some(entry) => {
                    let row = tags
                        .iter()
                        .map(|tag| {
                            entry_property(entry, *tag)
                                .unwrap_or(PropertyValue::ErrorCode(MAPI_E_NOT_FOUND))
                        })
                        .collect();
                    rows.push((tags.clone(), row));
                }
                None => {
                    any_not_found = true;
                    // Unknown MId ⇒ every requested cell is a NOT_FOUND error
                    // cell (a present cell carrying the HRESULT; never a
                    // zero-payload Null cell).
                    rows.push((
                        tags.clone(),
                        tags.iter()
                            .map(|_| PropertyValue::ErrorCode(MAPI_E_NOT_FOUND))
                            .collect(),
                    ));
                }
            }
        }
        let status = if any_not_found { NSPI_NAME_NOT_FOUND } else { NSPI_SUCCESS };
        out.extend_from_slice(&status.to_le_bytes());
        out.push(0xFF); // HasRowsAlready.
        encode_rowset_with_type(out, &rows);
        trailer(out);
    });
    MapiResponse::success(req.request_id.clone(), "GetProps", None, body)
}

/// §2.2.5.10 GetPropList — return the property tags the object exposes.
fn handle_get_prop_list(req: &MapiRequest, container: &[GalEntry]) -> MapiResponse {
    let mut cur = Buf::new(&req.body);
    let _flags = match take_u32(&mut cur) {
        Ok(v) => v,
        Err(e) => return decode_err_response(e, req),
    };
    let _stat = match Stat::decode(&mut cur) {
        Ok(s) => s,
        Err(e) => return decode_err_response(e, req),
    };
    let mid = match take_u32(&mut cur) {
        Ok(v) => v,
        Err(e) => return decode_err_response(e, req),
    };
    let body = render_with_container(container, |out, container| {
        let exposed = match container.iter().find(|e| e.mid == mid) {
            Some(_) => default_column_tags(),
            None => Vec::new(),
        };
        let status = if exposed.is_empty() { NSPI_NAME_NOT_FOUND } else { NSPI_SUCCESS };
        out.extend_from_slice(&status.to_le_bytes());
        encode_tag_array(out, &exposed);
        trailer(out);
    });
    MapiResponse::success(req.request_id.clone(), "GetPropList", None, body)
}

/// §2.2.5.13 GetSpecialTable — return the hierarchy / GAL container lists
/// Outlook iterates when binding. The gateway advertises a single GAL
/// container alongside the two top-level "root" entries the spec mandates.
fn handle_get_special_table(req: &MapiRequest, container: &[GalEntry]) -> MapiResponse {
    let mut cur = Buf::new(&req.body);
    let _flags = match take_u32(&mut cur) {
        Ok(v) => v,
        Err(e) => return decode_err_response(e, req),
    };
    let _stat = match Stat::decode(&mut cur) {
        Ok(s) => s,
        Err(e) => return decode_err_response(e, req),
    };
    let _version = match take_u32(&mut cur) {
        Ok(v) => v,
        Err(e) => return decode_err_response(e, req),
    };

    let body = render_with_container(container, |out, container| {
        let total = u32::try_from(container.len()).unwrap_or(u32::MAX);
        // Special-Table rows: each is (Flags(4) + MId(4) + HasChildren(1) + Depth(4)
        //   + ContainerDisplayName(string) + ContainerDN(string)).
        // We emit the GAL (container MId 0) plus a "Global Address List" row.
        out.extend_from_slice(&NSPI_SUCCESS.to_le_bytes());
        let rows = special_table_rows(total);
        out.extend_from_slice(&(rows.len() as u32).to_le_bytes());
        for row in &rows {
            out.extend_from_slice(&row.flags.to_le_bytes());
            out.extend_from_slice(&row.mid.to_le_bytes());
            out.push(if row.has_children { 1 } else { 0 });
            out.extend_from_slice(&row.depth.to_le_bytes());
            encode_pstring_inline(out, &row.display_name);
            encode_pstring_inline(out, &row.dn);
        }
        trailer(out);
    });
    MapiResponse::success(req.request_id.clone(), "GetSpecialTable", None, body)
}

/// A special-table row.
struct SpecialRow {
    flags: u32,
    mid: u32,
    has_children: bool,
    depth: u32,
    display_name: String,
    dn: String,
}

/// Build the special-table for a single-GAL gateway: the GAL container plus
/// the Documentation/GAL top-level rows the Outlook client expects.
fn special_table_rows(gal_total: u32) -> Vec<SpecialRow> {
    vec![
        SpecialRow {
            flags: 0,
            mid: GAL_CONTAINER_MID,
            has_children: gal_total > 0,
            depth: 0,
            display_name: "Global Address List".to_string(),
            dn: "/o=Stalwart/ou=Exchange Administrative Group (FYDIBOHF23SPDLT)".to_string(),
        },
        SpecialRow {
            flags: 0,
            mid: GAL_CONTAINER_MID,
            has_children: false,
            depth: 1,
            display_name: "All Address Lists".to_string(),
            dn: "/o=Stalwart/ou=Exchange Administrative Group (FYDIBOHF23SPDLT)/cn=addrlists".to_string(),
        },
    ]
}

/// Encode a PtypString value inline (4-byte LE byte count incl. trailing NUL +
/// UTF-16LE + trailing 0x0000) for the special-table rows.
fn encode_pstring_inline(out: &mut Vec<u8>, s: &str) {
    let mut buf: Vec<u8> = s.encode_utf16().flat_map(|u| u.to_le_bytes()).collect();
    buf.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&(buf.len() as u32).to_le_bytes());
    out.extend_from_slice(&buf);
}

/// §2.2.5.16 SeekEntries — reposition the cursor; the gateway treats this as
/// an UpdateStat-style no-op success over the in-memory table (the cursor
/// re-clamps against the container the same way UpdateStat does).
fn handle_seek_entries(req: &MapiRequest, container: &[GalEntry]) -> MapiResponse {
    let mut cur = Buf::new(&req.body);
    let _flags = match take_u32(&mut cur) {
        Ok(v) => v,
        Err(e) => return decode_err_response(e, req),
    };
    let stat = match Stat::decode(&mut cur) {
        Ok(s) => s,
        Err(e) => return decode_err_response(e, req),
    };
    let body = render_with_container(container, |out, container| {
        out.extend_from_slice(&NSPI_SUCCESS.to_le_bytes());
        let echoed = clamp_stat(stat, container.len());
        echoed.encode(out);
        trailer(out);
    });
    MapiResponse::success(req.request_id.clone(), "SeekEntries", None, body)
}

/// §2.2.5.15 QueryColumns — return the column set the container exposes.
fn handle_query_columns(req: &MapiRequest, container: &[GalEntry]) -> MapiResponse {
    let body = render_with_container(container, |out, _container| {
        out.extend_from_slice(&NSPI_SUCCESS.to_le_bytes());
        let cols = default_column_tags();
        // HasColumns(1)+columns, then HasInstanceKeys(1)+keys (we mirror with 0).
        out.push(0xFF);
        encode_tag_array(out, &cols);
        out.push(0x00); // HasInstanceKeys = false.
        trailer(out);
    });
    MapiResponse::success(req.request_id.clone(), "QueryColumns", None, body)
}

/// §2.2.5.2 ResortRestriction + §2.2.5.3 CompareMIds + admin-only
/// §2.2.5.9 GetTemplateInfo + §2.2.5.* ModLinkAtt/ModProps + the
/// MailboxUrl/AddressBookUrl helpers. The gateway serves a single in-memory
/// table, so each of these is a deterministic success whose echo matches the
/// input STAT. (CompareMIds compares the underlying MIds numerically;
/// GetTemplateInfo echoes an empty tag set; ModLinkAtt/ModProps are
/// admin-only and unused by Outlook.)
fn handle_admin_or_stateless_success(req: &MapiRequest, container: &[GalEntry], rt: &'static str) -> MapiResponse {
    let body = render_with_container(container, |out, container| {
        out.extend_from_slice(&NSPI_SUCCESS.to_le_bytes());
        let stat = Stat {
            total_recs: u32::try_from(container.len()).unwrap_or(u32::MAX),
            ..Stat::default()
        };
        stat.encode(out);
        trailer(out);
    });
    MapiResponse::success(req.request_id.clone(), rt, None, body)
}

/// §2.2.5.3 CompareMIds — compare two MIds; returns 1 if equal, 0 otherwise.
fn handle_compare_mids(req: &MapiRequest, container: &[GalEntry]) -> MapiResponse {
    let mut cur = Buf::new(&req.body);
    let _flags = match take_u32(&mut cur) {
        Ok(v) => v,
        Err(e) => return decode_err_response(e, req),
    };
    let _stat = match Stat::decode(&mut cur) {
        Ok(s) => s,
        Err(e) => return decode_err_response(e, req),
    };
    let mid1 = match take_u32(&mut cur) {
        Ok(v) => v,
        Err(e) => return decode_err_response(e, req),
    };
    let mid2 = match take_u32(&mut cur) {
        Ok(v) => v,
        Err(e) => return decode_err_response(e, req),
    };
    let body = render_with_container(container, |out, _container| {
        out.extend_from_slice(&NSPI_SUCCESS.to_le_bytes());
        out.push(if mid1 == mid2 { 1 } else { 0 });
        trailer(out);
    });
    MapiResponse::success(req.request_id.clone(), "CompareMIds", None, body)
}

// ---------------------------------------------------------------------------
// Helpers shared across handlers.
// ---------------------------------------------------------------------------

/// Decode the name blob Outlook sends in `ResolveNames` (a UTF-16LE,
/// NUL-terminated string). `&[u8]` is the raw byte span (count incl. NUL).
fn decode_name_blob(bytes: &[u8]) -> String {
    let mut trimmed = bytes;
    if trimmed.len() >= 2 && trimmed[trimmed.len() - 2] == 0 && trimmed[trimmed.len() - 1] == 0 {
        trimmed = &trimmed[..trimmed.len() - 2];
    }
    if !trimmed.len().is_multiple_of(2) {
        trimmed = &trimmed[..trimmed.len() - 1];
    }
    let units: Vec<u16> = trimmed
        .chunks_exact(2)
        .map(|c| u16::from_le_bytes([c[0], c[1]]))
        .collect();
    String::from_utf16_lossy(&units)
}

/// Clamp a STAT against the container bounds (CurrentRec / NumPos within
/// [0, total]); SortType / ContainerID / CodePage / Locale are NOT mutated.
fn clamp_stat(stat: Stat, total: usize) -> Stat {
    let total = total as u32;
    let mut s = stat;
    s.total_recs = total;
    if s.current_rec > total {
        s.current_rec = total;
    }
    if s.num_pos > total {
        s.num_pos = total;
    }
    s
}

/// Render a response body whose closure receives the resolved in-memory GAL
/// container. The caller resolves the container once (authenticated) before
/// dispatching into the per-RPC handler, so every NSPI RPC over one Bind
/// session consults the SAME snapshot.
fn render_with_container<F>(container: &[GalEntry], mut render: F) -> Vec<u8>
where
    F: FnMut(&mut Vec<u8>, &[GalEntry]),
{
    let mut out = Vec::new();
    render(&mut out, container);
    out
}

/// Decode-error → transport response.
fn decode_err_response(e: NspiDecodeError, req: &MapiRequest) -> MapiResponse {
    MapiResponse::error(decode_err_to_code(e), req.request_id.clone())
}

/// Table-too-big → transport response (used when a requested count exceeds the
/// documented `NSPI_TABLE_TOO_BIG` cap). `rt` is the request-type string the
/// response echoes verbatim (so the echo matches the verb the client sent).
fn min_id_too_big(req: &MapiRequest, rt: &'static str) -> MapiResponse {
    let mut out = Vec::with_capacity(8);
    out.extend_from_slice(&NSPI_TABLE_TOO_BIG.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    MapiResponse::success(req.request_id.clone(), rt, None, out)
}

// ---------------------------------------------------------------------------
// Entry point dispatched from `handler::handle` for `RpcKind::AddressBook(_)`.
// ---------------------------------------------------------------------------

pub async fn handle_address_book(rpc: AddressBookRpc, req: MapiRequest, state: &MapiState) -> MapiResponse {
    // Authenticate every NSPI RPC against the shared Stalwart `AuthVerifier`.
    // A request without valid Basic credentials is rejected with the transport
    // `NoPrivilege` (11) — never reaching the directory, so recipient PII does
    // not leak to an unauthenticated caller.
    let principal = match authenticate(&req, state).await {
        Some(p) => p,
        None => return anonymous_rejected(req),
    };

    // Resolve the directory snapshot once (on a `spawn_blocking` task) so every
    // NSPI RPC dispatched below consults the SAME container — a Bind then
    // QueryRows round-trip within one Outlook session resolves stabily.
    let container = assemble_gal(state, &principal).await;

    match rpc {
        AddressBookRpc::Bind => handle_bind(&req, &container),
        AddressBookRpc::Unbind => handle_unbind(&req, &container),
        AddressBookRpc::UpdateStat => handle_update_stat(&req, &container),
        AddressBookRpc::QueryRows => handle_query_rows(&req, &container),
        AddressBookRpc::DnToMinId => handle_dn_to_min_id(&req, &container),
        AddressBookRpc::ResolveNames => handle_resolve_names(&req, &container),
        AddressBookRpc::GetMatches => handle_get_matches(&req, &container),
        AddressBookRpc::GetProps => handle_get_props(&req, &container),
        AddressBookRpc::GetPropList => handle_get_prop_list(&req, &container),
        AddressBookRpc::GetSpecialTable => handle_get_special_table(&req, &container),
        AddressBookRpc::SeekEntries => handle_seek_entries(&req, &container),
        AddressBookRpc::QueryColumns => handle_query_columns(&req, &container),
        AddressBookRpc::CompareMIds => handle_compare_mids(&req, &container),
        AddressBookRpc::ResortRestriction
        | AddressBookRpc::ModLinkAtt
        | AddressBookRpc::ModProps
        | AddressBookRpc::GetTemplateInfo
        | AddressBookRpc::GetMailboxUrl
        | AddressBookRpc::GetAddressBookUrl => {
            // Stateless / admin-only verbs the gateway serves as a deterministic
            // success so Outlook's address-book handshake never stalls.
            handle_admin_or_stateless_success(&req, &container, rpc.as_str())
        }
    }
}

// ---------------------------------------------------------------------------
// Unit tests — codec round-trips and the GAL assembly's caller-stub fallback.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::AuthVerifier;
    use crate::config::Config;
    use crate::mapi::data::{PropertyTag, PropertyType};
    use crate::mapi::handler::MapiState;
    use crate::mapi::transport::{AddressBookRpc, MapiRequest, RpcKind, ResponseCode};

    #[test]
    fn stat_round_trips() {
        let s = Stat {
            sort_type: 0x00000200,
            container_id: 0,
            current_rec: 5,
            delta: -1,
            num_pos: 5,
            total_recs: 42,
            code_page: 0x4E4,
            template_locale: 0x409,
            sort_locale: 0x409,
        };
        let mut out = Vec::new();
        s.encode(&mut out);
        assert_eq!(out.len(), Stat::WIRE_SIZE);
        let mut cur = Buf::new(&out);
        let s2 = Stat::decode(&mut cur).unwrap();
        assert_eq!(s, s2);
    }

    #[test]
    fn stat_decode_insufficient_is_short_buffer() {
        let bytes = [0u8; 8];
        let mut cur = Buf::new(&bytes);
        assert_eq!(Stat::decode(&mut cur), Err(NspiDecodeError::Insufficient));
    }

    #[test]
    fn tag_array_round_trips() {
        let tags = vec![
            pack_tag(PropertyType::PTYP_STRING, PR_DISPLAY_NAME_ABOOK),
            pack_tag(PropertyType::PTYP_STRING, PR_SMTP_ADDRESS),
            pack_tag(PropertyType::PTYP_INTEGER32, PR_OBJECT_TYPE),
        ];
        let mut out = Vec::new();
        encode_tag_array(&mut out, &tags);
        let mut cur = Buf::new(&out);
        assert_eq!(decode_tag_array(&mut cur).unwrap(), tags);
    }

    #[test]
    fn pack_tag_decodes_in_property_tag_order() {
        // wire layout type-first; the PropertyTag decode reads type then id.
        let ty = PropertyType::PTYP_STRING;
        let id = PR_SMTP_ADDRESS;
        let packed = pack_tag(ty, id);
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&packed.to_le_bytes());
        let mut cur = Buf::new(&bytes);
        let tag = PropertyTag::decode(&mut cur).unwrap();
        assert_eq!(tag.property_type, ty);
        assert_eq!(tag.property_id, id);
    }

    #[test]
    fn synth_dn_matches_oab_shape() {
        let dn = synth_dn("Alice@Example.COM");
        assert_eq!(
            dn,
            "/o=Stalwart/ou=Exchange Administrative Group (FYDIBOHF23SPDLT)/cn=Recipients/cn=alice"
        );
        assert_eq!(dn_local_part(&dn).as_deref(), Some("alice"));
    }

    #[test]
    fn string_cell_round_trips() {
        let v = PropertyValue::String("Résumé".to_string());
        let mut out = Vec::new();
        encode_scalar(&mut out, &v);
        // 4-byte count + UTF-16LE + 0x0000 terminator.
        let count = u32::from_le_bytes([out[0], out[1], out[2], out[3]]) as usize;
        assert_eq!(out.len(), 4 + count);
        let bytes = &out[4..].to_vec();
        assert_eq!(decode_pstring(bytes.clone()), Some(v));
    }

    #[test]
    fn entry_property_resolves_smtp_and_entryid() {
        let entry = GalEntry {
            mid: 7,
            display_name: "Alice Example".to_string(),
            email: "alice@example.com".to_string(),
            dn: synth_dn("alice@example.com"),
            title: None,
            company: None,
            department: None,
            phone: None,
        };
        let smtp_tag = pack_tag(PropertyType::PTYP_STRING, PR_SMTP_ADDRESS);
        match entry_property(&entry, smtp_tag) {
            Some(PropertyValue::String(s)) => assert_eq!(s, "alice@example.com"),
            other => panic!("expected String, got {other:?}"),
        }
        let obj_tag = pack_tag(PropertyType::PTYP_INTEGER32, PR_OBJECT_TYPE);
        assert_eq!(
            entry_property(&entry, obj_tag),
            Some(PropertyValue::Integer32(MAPI_MAILUSER as i32))
        );
        let entryid_tag = pack_tag(PropertyType::PTYP_BINARY, PR_ENTRYID_ABOOK);
        match entry_property(&entry, entryid_tag) {
            Some(PropertyValue::Binary(b)) => {
                // trailing byte is the null terminator; the DN is present.
                assert!(b.windows(entry.dn.len()).any(|w| w == entry.dn.as_bytes()));
            }
            other => panic!("expected Binary entry id, got {other:?}"),
        }
    }

    #[test]
    fn encode_rowset_mirrors_column_set() {
        let tags = vec![
            pack_tag(PropertyType::PTYP_STRING, PR_DISPLAY_NAME_ABOOK),
            pack_tag(PropertyType::PTYP_STRING, PR_SMTP_ADDRESS),
        ];
        let entry = GalEntry {
            mid: 1,
            display_name: "Bob".to_string(),
            email: "bob@example.com".to_string(),
            dn: synth_dn("bob@example.com"),
            title: None,
            company: None,
            department: None,
            phone: None,
        };
        let rows = vec![materialise_row(&entry, &tags)];
        let mut out = Vec::new();
        encode_rowset(&mut out, &rows, &tags);
        // RowCount(4) == 1, row Flags (offset 4) == present.
        assert_eq!(u32::from_le_bytes([out[0], out[1], out[2], out[3]]), 1);
        assert_eq!(out[4], CELL_FLAG_VALUE);
        // Both cell payloads (the UTF-16LE + NUL of "Bob" and "bob@example.com")
        // MUST be present in the byte stream. A malformed Null cell (flag 0x0
        // present with zero payload) would have dropped the body entirely, so
        // verifying the text round-trips pins the error-cell-vs-value-cell fix.
        let bob_utf16: Vec<u8> = "Bob"
            .encode_utf16()
            .flat_map(|u| u.to_le_bytes().to_vec())
            .collect();
        let email_utf16: Vec<u8> = "bob@example.com"
            .encode_utf16()
            .flat_map(|u| u.to_le_bytes().to_vec())
            .collect();
        assert!(
            out.windows(bob_utf16.len()).any(|w| w == bob_utf16.as_slice()),
            "display-name UTF-16LE payload missing from rowset bytes"
        );
        assert!(
            out.windows(email_utf16.len()).any(|w| w == email_utf16.as_slice()),
            "smtp-address UTF-16LE payload missing from rowset bytes"
        );
        // The first cell's Flag byte (offset 5) is present, never an empty-payload Null.
        assert_eq!(out[5], CELL_FLAG_VALUE);
    }

    #[test]
    fn unsupported_tag_encodes_as_notfound_error_cell_never_null() {
        // An unknown/unsupported tag MUST serialise as a present cell carrying the
        // MAPI_E_NOT_FOUND HRESULT (Flag 0x0 + 4-byte value), NOT a zero-payload
        // present cell (the malformed `PropertyValue::Null` path flagged in PR #1845).
        const UNSUPPORTED_ID: u16 = 0x6F00;
        let tags = vec![pack_tag(PropertyType::PTYP_INTEGER32, UNSUPPORTED_ID)];
        let entry = GalEntry {
            mid: 1,
            display_name: "Bob".to_string(),
            email: "bob@example.com".to_string(),
            dn: synth_dn("bob@example.com"),
            title: None,
            company: None,
            department: None,
            phone: None,
        };
        let row = materialise_row(&entry, &tags);
        assert!(
            matches!(row[0], PropertyValue::ErrorCode(_)),
            "unsupported tag resolved to {:?}, expected ErrorCode",
            row[0]
        );
        let mut out = Vec::new();
        encode_rowset(&mut out, std::slice::from_ref(&row), &tags);
        // RowCount(4) + row Flags(1) + cell Flag(1) + 4-byte HRESULT = 10.
        assert_eq!(out.len(), 4 + 1 + 1 + 4);
        assert_eq!(out[5], CELL_FLAG_VALUE); // cell present (NOT absent 0x1)
    }

    #[tokio::test]
    async fn address_book_rejects_anonymous_request() {
        // An empty-credentials `/mapi/nspi` Bind must be rejected with the
        // transport `NoPrivilege` (11) code — the directory is never consulted,
        // so recipient PII does not leak to an unauthenticated caller. The
        // auth gate short-circuits on the empty username/password before any
        // network round-trip, so this test requires no Stalwart backend.
        let cfg = Config::test_with_mail_domain("example.com");
        let auth = std::sync::Arc::new(AuthVerifier::new(&cfg));
        let state = MapiState::new(cfg, auth);
        let req = MapiRequest {
            kind: RpcKind::AddressBook(AddressBookRpc::Bind),
            request_id: "{G}:1".into(),
            client_application: None,
            client_info: None,
            username: None,
            password: None,
            cookies: Vec::new(),
            body: Vec::new(),
        };
        let resp = handle_address_book(AddressBookRpc::Bind, req, &state).await;
        assert_eq!(resp.code, ResponseCode::NoPrivilege);
    }
}


