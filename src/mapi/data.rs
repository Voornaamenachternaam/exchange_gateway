// src/mapi/data.rs
//
// MS-OXCDATA — the property/type universe that ROPs shuttle across the wire.
//
// Phase 0 covers:
//   * The `PropertyType` enumeration (the full §2.11.1 type table, byte-exact).
//   * The 4-byte `PropertyTag` structure (PropertyType + PropertyId) and its
//     codec, used by RopSetColumns / RopGetPropertiesSpecific.
//   * Typed property values (`PropertyValue`) for the Phase-0 scalar types
//     (Boolean, Integer16/32/64, String, String8, Time, Guid, Binary, Error)
//     plus the multi-value marker bit (`MV_INSTANCE` / 0x2000).
//
// All decoding is fail-closed against the buffer bounds; unknown type codes
// degrade to `PropertyType::Unknown(n)` so the transport can still emit a
// deterministic error envelope.

use crate::mapi::rops::{Buf, DecodeError};

/// Every PropertyType value from MS-OXCDATA §2.11.1, byte-exact. Unknown
/// codes are preserved verbatim so a malformed/novel value does not crash
/// the server.
///
/// We model this as a `#[repr(transparent)]` newtype around `u16` rather than
/// a Rust enum: a mixed enum with explicit discriminants AND a `Unknown(u16)`
/// non-unit variant triggers E0732 on stable Rust, and a pure-unit enum would
/// force us to drop unknown values (breaking the fail-preserve contract). The
/// newtype pattern keeps `from_u16`/`to_u16` trivially byte-exact and lets us
/// carry any unknown code through to the client unchanged.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(transparent)]
pub struct PropertyType(pub u16);

impl PropertyType {
    pub const PTYP_UNSPECIFIED: Self = Self(0x0000);
    pub const PTYP_NULL: Self = Self(0x0001);
    pub const PTYP_INTEGER16: Self = Self(0x0002);
    pub const PTYP_INTEGER32: Self = Self(0x0003);
    pub const PTYP_FLOATING32: Self = Self(0x0004);
    pub const PTYP_FLOATING64: Self = Self(0x0005);
    pub const PTYP_CURRENCY: Self = Self(0x0006);
    pub const PTYP_FLOATING_TIME: Self = Self(0x0007);
    pub const PTYP_ERROR_CODE: Self = Self(0x000A);
    pub const PTYP_BOOLEAN: Self = Self(0x000B);
    pub const PTYP_INTEGER64: Self = Self(0x0014);
    pub const PTYP_STRING: Self = Self(0x001F);
    pub const PTYP_STRING8: Self = Self(0x001E);
    pub const PTYP_TIME: Self = Self(0x0040);
    pub const PTYP_GUID: Self = Self(0x0048);
    pub const PTYP_SERVER_ID: Self = Self(0x00FB);
    pub const PTYP_RESTRICTION: Self = Self(0x00FD);
    pub const PTYP_RULE_ACTION: Self = Self(0x00FE);
    pub const PTYP_BINARY: Self = Self(0x0102);
    pub const PTYP_MV_INTEGER16: Self = Self(0x1002);
    pub const PTYP_MV_INTEGER32: Self = Self(0x1003);
    pub const PTYP_MV_FLOATING32: Self = Self(0x1004);
    pub const PTYP_MV_FLOATING64: Self = Self(0x1005);
    pub const PTYP_MV_CURRENCY: Self = Self(0x1006);
    pub const PTYP_MV_FLOATING_TIME: Self = Self(0x1007);
    pub const PTYP_MV_INTEGER64: Self = Self(0x1014);
    pub const PTYP_MV_STRING: Self = Self(0x101F);
    pub const PTYP_MV_STRING8: Self = Self(0x101E);
    pub const PTYP_MV_TIME: Self = Self(0x1040);
    pub const PTYP_MV_GUID: Self = Self(0x1048);
    pub const PTYP_MV_BINARY: Self = Self(0x1102);

    pub const fn from_u16(v: u16) -> Self {
        Self(v)
    }
    pub const fn to_u16(self) -> u16 {
        self.0
    }

    /// The 0x2000 bit is the `MV_INSTANCE` (multi-value-instance) marker per
    /// MS-OXCDATA §2.11.1. It is informational for Phase 0 — we preserve it
    /// but do not branch on it.
    pub const MV_INSTANCE: u16 = 0x2000;

    /// Fixed wire size in bytes for scalar types, or `None` if variable.
    pub const fn fixed_size(self) -> Option<usize> {
        Some(match self {
            Self::PTYP_INTEGER16 | Self::PTYP_UNSPECIFIED | Self::PTYP_NULL => 2,
            Self::PTYP_INTEGER32 | Self::PTYP_FLOATING32 | Self::PTYP_ERROR_CODE => 4,
            Self::PTYP_INTEGER64
            | Self::PTYP_FLOATING64
            | Self::PTYP_FLOATING_TIME
            | Self::PTYP_CURRENCY
            | Self::PTYP_TIME => 8,
            Self::PTYP_BOOLEAN => 1,
            Self::PTYP_GUID => 16,
            _ => return None,
        })
    }
}

/// A 4-byte `PropertyTag` (§2.9): PropertyType (2 bytes LE) + PropertyId (2
/// bytes LE). The high bit of PropertyId (0x8000) marks a named property per
/// MS-OXCDATA §2.12.1; unknown/named ids are preserved verbatim.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PropertyTag {
    pub property_type: PropertyType,
    pub property_id: u16,
}

impl PropertyTag {
    pub const fn new(property_type: PropertyType, property_id: u16) -> Self {
        Self {
            property_type,
            property_id,
        }
    }

    /// Whether the PropertyId's high bit (0x8000) is set, indicating a named
    /// property (MS-OXCDATA §2.12.1).
    pub const fn is_named(self) -> bool {
        self.property_id & 0x8000 != 0
    }

    pub fn decode(cur: &mut Buf<'_>) -> Result<Self, DecodeError> {
        let property_type = PropertyType::from_u16(cur.take_u16_le()?);
        let property_id = cur.take_u16_le()?;
        Ok(Self {
            property_type,
            property_id,
        })
    }

    pub fn encode(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&self.property_type.to_u16().to_le_bytes());
        out.extend_from_slice(&self.property_id.to_le_bytes());
    }
}

/// A typed property value for the Phase-0 scalar set. Variable-length values
/// are length-prefixed on the wire (2-byte LE count for binary/string8; the
/// string count is in UTF-16 code units).
#[derive(Debug, Clone, PartialEq)]
pub enum PropertyValue {
    Null,
    Boolean(bool),
    Integer16(i16),
    Integer32(i32),
    Integer64(i64),
    Floating32(f32),
    Floating64(f64),
    Currency(i64),
    Time(u64), // FILETIME: 100ns ticks since 1601-01-01
    Guid([u8; 16]),
    ErrorCode(u32),
    String(String),  // PtypString: UTF-16LE wire form
    String8(String), // PtypString8: lossy ASCII on the wire
    Binary(Vec<u8>), // PtypBinary
    /// Any property type we do not yet decode carries the raw bytes verbatim
    /// so a future phase can pass them through unchanged.
    Opaque {
        property_type: PropertyType,
        bytes: Vec<u8>,
    },
}

impl PropertyValue {
    /// Decode a single fixed-size or variable-length scalar `PropertyValue`
    /// for the given `PropertyTag` from a ROP request body, per
    /// MS-OXCDATA §2.11.2.1. This is the inverse of [`PropertyValue::encode`]
    /// and is used by `RopSetProperties`/`RopCopyTo` to read the typed
    /// property values the client supplies. Multi-value properties (`PTYP_MV_*`)
    /// are decoded as `PropertyValue::Opaque` carrying the raw MV_INSTANCE
    /// payload: the gateway's write paths only act on the scalar write props
    /// Outlook composes mail with (subject, body, importance, flags), so
    /// passing multi-value bytes through verbatim keeps the wire shape
    /// intact without committing to full MV decode.
    pub fn decode(cur: &mut Buf<'_>, tag: &PropertyTag) -> Result<Self, DecodeError> {
        use PropertyType as T;
        let t = tag.property_type;
        // The MV_INSTANCE bit (0x2000) marks an MV variant of an otherwise
        // scalar type; the wire payload is a count-prefixed array. We carry
        // it through opaquely to avoid drop-on-floor behaviour.
        if t.to_u16() & Self::MV_INSTANCE_MARKER != 0 {
            return Self::decode_opaque(cur, t);
        }
        Ok(match t {
            T::PTYP_UNSPECIFIED => {
                // Unspecified/PT_NULL is empty on the wire.
                Self::Null
            }
            T::PTYP_NULL => Self::Null,
            T::PTYP_BOOLEAN => Self::Boolean(cur.take_u8()? != 0),
            T::PTYP_INTEGER16 => {
                // Bit-reinterpret the two's-complement bytes directly: the
                // prior `take_u16_le` + `i16::try_from` rejected valid
                // negative values, so a SetProperties carrying a negative
                // PtypInteger16 failed to decode (cubic/code review).
                let raw = cur.take_bytes(2)?;
                Self::Integer16(i16::from_le_bytes([raw[0], raw[1]]))
            }
            T::PTYP_INTEGER32 => {
                let raw = cur.take_bytes(4)?;
                Self::Integer32(i32::from_le_bytes([raw[0], raw[1], raw[2], raw[3]]))
            }
            T::PTYP_ERROR_CODE => Self::ErrorCode(cur.take_u32_le()?),
            T::PTYP_INTEGER64 => {
                let raw = cur.take_bytes(8)?;
                Self::Integer64(i64::from_le_bytes([
                    raw[0], raw[1], raw[2], raw[3], raw[4], raw[5], raw[6], raw[7],
                ]))
            }
            T::PTYP_FLOATING64 => {
                let raw = cur.take_u64_le()?;
                Self::Floating64(f64::from_bits(raw))
            }
            T::PTYP_FLOATING32 => {
                let raw = cur.take_u32_le()?;
                Self::Floating32(f32::from_bits(raw))
            }
            T::PTYP_CURRENCY => {
                let raw = cur.take_bytes(8)?;
                Self::Currency(i64::from_le_bytes([
                    raw[0], raw[1], raw[2], raw[3], raw[4], raw[5], raw[6], raw[7],
                ]))
            }
            T::PTYP_TIME | T::PTYP_FLOATING_TIME => {
                // Both PtypTime (0x0040) and PtypFloatingTime (0x0007) are
                // 64-bit FILETIME values on the wire (MS-OXCDATA 2.11.1):
                // 100-ns ticks since 1601-01-01. Treat the floating variant
                // identically so it decodes to `Time` rather than an 8-byte
                // Opaque (which would drop the typed value on re-encode).
                let raw = cur.take_u64_le()?;
                Self::Time(raw)
            }
            T::PTYP_GUID => {
                let mut g = [0u8; 16];
                g.copy_from_slice(cur.take_bytes(16)?);
                Self::Guid(g)
            }
            // PtypString8: 1-byte-count-prefixed? No — MS-OXCDATA §2.11.2.1
            // wire form: a 2-byte count (number of CHARS, NOT including the
            // terminator) followed by the chars and a trailing 0x00.
            T::PTYP_STRING8 => {
                let n = usize::from(cur.take_u16_le()?);
                if cur.remaining() < n.checked_add(1).ok_or(DecodeError::ExcessLength)? {
                    return Err(DecodeError::Insufficient);
                }
                let raw = cur.take_bytes(n)?.to_vec();
                cur.take_u8()?; // terminating NUL
                let s = String::from_utf8_lossy(&raw).into_owned();
                Self::String8(s)
            }
            T::PTYP_STRING => {
                // 2-byte count of UTF-16 CODE UNITS (not including terminator).
                let n = usize::from(cur.take_u16_le()?);
                let want = n.checked_mul(2).ok_or(DecodeError::ExcessLength)?;
                if cur.remaining() < want.checked_add(2).ok_or(DecodeError::ExcessLength)? {
                    return Err(DecodeError::Insufficient);
                }
                let mut units = Vec::with_capacity(n);
                for _ in 0..n {
                    units.push(cur.take_u16_le()?);
                }
                cur.take_u16_le()?; // terminating 0x0000
                let s = String::from_utf16_lossy(&units);
                Self::String(s)
            }
            T::PTYP_BINARY => {
                let n = usize::from(cur.take_u16_le()?);
                let bytes = cur.take_bytes(n)?.to_vec();
                Self::Binary(bytes)
            }
            // Multi-value properties (PtypMultiple*, the 0x1000 bit set)
            // use a 32-bit element count followed by `count` per-element
            // encodings. The gateway's write paths only act on the scalar
            // compose props; MV values (e.g. PidLidCategories) are
            // sized-and-skipped into an Opaque carrying the consumed bytes
            // so a SetProperties array that includes a categories entry
            // does not desynchronise the cursor. Named-property persistence
            // of categories is Phase 2.
            mv if Self::is_multivalue(mv) => Self::skip_multivalue(cur, mv)?,
            // Anything else (PTYP_SERVER_ID/RESTRICTION/RULE_ACTION/unknowns):
            // return Null and leave the cursor where we are, since the wire
            // length is unknown to a generic decoder. The caller is expected
            // to skip such tags explicitly rather than request a typed value.
            other => return Self::decode_opaque(cur, other),
        })
    }

    /// True iff `t` is a PtypMultiple* property type (the 0x1000
    /// Multivalue bit, MS-OXCDATA 2.11.1), excluding the table-only
    /// 0x2000 MultivalueInstance flag handled separately above.
    fn is_multivalue(t: PropertyType) -> bool {
        t.to_u16() & 0x1000 != 0 && t.to_u16() & Self::MV_INSTANCE_MARKER == 0
    }

    /// Walk one multi-value value off the cursor, returning the raw bytes
    /// consumed as an `Opaque` so the caller's array stays byte-aligned.
    /// Per MS-OXCDATA 2.11.1.1 ROP buffers use a 32-bit element count; each
    /// element mirrors its scalar encoding (String/String8 are
    /// NUL-terminated, Binary is a 2-byte count prefix).
    fn skip_multivalue(cur: &mut Buf<'_>, t: PropertyType) -> Result<Self, DecodeError> {
        use PropertyType as T;
        // Capture the consumed span (count + per-element encodings) into the
        // returned `Opaque.bytes` so a future re-encode of the value
        // round-trips verbatim; prior to this the placeholder carried
        // `Vec::new()`, silently dropping the payload on any encode path
        // (sourcery / cubic / coderabbit review).
        let start = cur.position();
        let count = cur.take_u32_le()?;
        // The element count for a single SetProperties MV value is bounded
        // by a sane upper limit; Outlook categories are a handful of strings.
        const MAX_MV_ELEMENTS: u32 = 1 << 20;
        if count > MAX_MV_ELEMENTS {
            return Err(DecodeError::ExcessLength);
        }
        let elem_type = PropertyType::from_u16(t.to_u16() & 0x0FFF);
        for _ in 0..count {
            match elem_type {
                T::PTYP_STRING => Self::skip_terminated_utf16(cur)?,
                T::PTYP_STRING8 => Self::skip_terminated_string8(cur)?,
                T::PTYP_BINARY => {
                    let n = usize::from(cur.take_u16_le()?);
                    let _ = cur.take_bytes(n)?;
                }
                fixed if fixed.fixed_size().is_some() => {
                    let n = fixed.fixed_size().unwrap();
                    let _ = cur.take_bytes(n)?;
                }
                _ => return Err(DecodeError::InvalidValue),
            }
        }
        let end = cur.position();
        let bytes = cur
            .slice(start, end)
            .map(|s| s.to_vec())
            .unwrap_or_default();
        Ok(Self::Opaque {
            property_type: t,
            bytes,
        })
    }

    /// Skip a NUL-terminated UTF-16 (PtypString) value: UTF-16LE code
    /// units up to and including the terminating 0x0000.
    fn skip_terminated_utf16(cur: &mut Buf<'_>) -> Result<(), DecodeError> {
        loop {
            let u = cur.take_u16_le()?;
            if u == 0 {
                return Ok(());
            }
        }
    }

    /// Skip a NUL-terminated String8 value: bytes up to and including 0x00.
    fn skip_terminated_string8(cur: &mut Buf<'_>) -> Result<(), DecodeError> {
        loop {
            let b = cur.take_u8()?;
            if b == 0 {
                return Ok(());
            }
        }
    }

    /// Read the raw bytes for a property whose type the codec does not yet
    /// understand. The fixed-size variants are read inline; otherwise we
    /// surface nothing and let the caller handle the residual cursor.
    fn decode_opaque(cur: &mut Buf<'_>, t: PropertyType) -> Result<Self, DecodeError> {
        Ok(match t.fixed_size() {
            Some(0) => Self::Null,
            Some(n) => {
                let bytes = cur.take_bytes(n)?.to_vec();
                Self::Opaque {
                    property_type: t,
                    bytes,
                }
            }
            None => Self::Opaque {
                property_type: t,
                bytes: Vec::new(),
            },
        })
    }

    /// The multi-value-instance marker bit per MS-OXCDATA §2.11.1.
    pub const MV_INSTANCE_MARKER: u16 = 0x2000;

    /// Encode the value for a `GetPropertiesSpecific`/`All` response row,
    /// emitting the typed payload bytes (no PropertyTag prefix — the row
    /// struct in MS-OXCDATA prefixes each value with its reflected tag, done
    /// by the row assembler at a higher layer).
    pub fn encode(&self, out: &mut Vec<u8>) {
        match self {
            Self::Null => {}
            Self::Boolean(b) => out.push(u8::from(*b)),
            Self::Integer16(v) => out.extend_from_slice(&v.to_le_bytes()),
            Self::Integer32(v) => out.extend_from_slice(&v.to_le_bytes()),
            Self::Integer64(v) => out.extend_from_slice(&v.to_le_bytes()),
            Self::Floating32(v) => out.extend_from_slice(&v.to_le_bytes()),
            Self::Floating64(v) => out.extend_from_slice(&v.to_le_bytes()),
            Self::Currency(v) => out.extend_from_slice(&v.to_le_bytes()),
            Self::Time(v) => out.extend_from_slice(&v.to_le_bytes()),
            Self::Guid(g) => out.extend_from_slice(g),
            Self::ErrorCode(v) => out.extend_from_slice(&v.to_le_bytes()),
            // Per MS-OXCDATA §2.11.2.1 PropertyValue Structure, a PtypString
            // is the UTF-16LE code units INCLUDING the terminating 0x0000,
            // with NO length prefix — the client walks to the NUL. A length
            // prefix here would be misread as the first UTF-16 character.
            Self::String(s) => {
                // Cap at u16::MAX code units so the NUL terminator fits within
                // the u16-domain string size Outlook tolerates; `.take` enforces
                // the cap without materialising a Vec or hand-rolling a counter.
                let max_units = u16::MAX as usize / 2;
                for u in s.encode_utf16().take(max_units) {
                    out.extend_from_slice(&u.to_le_bytes());
                }
                out.extend_from_slice(&0u16.to_le_bytes()); // terminating NUL
            }
            // PtypString8 is likewise the bytes INCLUDING the terminating 0x00,
            // with NO length prefix.
            Self::String8(s) => {
                let max = u16::MAX as usize;
                let bytes = s.as_bytes();
                let take = bytes.len().min(max);
                out.extend_from_slice(&bytes[..take]);
                out.push(0); // terminating NUL
            }
            // PtypBinary is a 16-bit byte-count (§2.11.1.1) followed by the
            // bytes. Cap both the prefix and the payload to keep them in lock
            // step (a 0 / oversized split would desynchronise the row stream).
            Self::Binary(b) => {
                let max = u16::MAX as usize;
                let take = b.len().min(max);
                out.extend_from_slice(&u16::try_from(take).unwrap_or(u16::MAX).to_le_bytes());
                out.extend_from_slice(&b[..take]);
            }
            Self::Opaque { bytes, .. } => {
                out.extend_from_slice(bytes);
            }
        }
    }

    /// Decode a property value in the **row form** (MS-OXCDATA §2.11.2.1
    /// PropertyValue Structure): `PtypString`/`PtypString8` are NUL-terminated
    /// with NO length prefix and `PtypBinary` is a 2-byte count prefix + bytes.
    /// This is the exact inverse of [`Self::encode`] and is the form embedded
    /// inside `SRestriction` Content/Property arms (MS-OXCDATA §2.12.3) and
    /// inside FastTransfer propValue elements (MS-OXCFXICS §2.2.4.1). The
    /// ROP-buffer [`Self::decode`] (2-byte count prefix for strings) is a
    /// different shape and must NOT be re-used here.
    ///
    /// Bounds-checked and fail-closed: a missing terminator, an over-long
    /// binary count, or an oversized multi-value array is rejected rather
    /// than driving the caller past the buffer end.
    pub fn decode_row(cur: &mut Buf<'_>, tag: &PropertyTag) -> Result<Self, DecodeError> {
        use PropertyType as T;
        let t = tag.property_type;
        // Multi-value-instance marker is informational; the underlying MV
        // payload uses the MV count form.
        if t.to_u16() & Self::MV_INSTANCE_MARKER != 0 {
            return Self::skip_multivalue(cur, t);
        }
        Ok(match t {
            T::PTYP_UNSPECIFIED | T::PTYP_NULL => Self::Null,
            T::PTYP_BOOLEAN => Self::Boolean(cur.take_u8()? != 0),
            T::PTYP_INTEGER16 => {
                let raw = cur.take_bytes(2)?;
                Self::Integer16(i16::from_le_bytes([raw[0], raw[1]]))
            }
            T::PTYP_INTEGER32 => {
                let raw = cur.take_bytes(4)?;
                Self::Integer32(i32::from_le_bytes([raw[0], raw[1], raw[2], raw[3]]))
            }
            T::PTYP_ERROR_CODE => Self::ErrorCode(cur.take_u32_le()?),
            T::PTYP_INTEGER64 => {
                let raw = cur.take_bytes(8)?;
                Self::Integer64(i64::from_le_bytes([
                    raw[0], raw[1], raw[2], raw[3], raw[4], raw[5], raw[6], raw[7],
                ]))
            }
            T::PTYP_FLOATING64 => Self::Floating64(f64::from_bits(cur.take_u64_le()?)),
            T::PTYP_FLOATING32 => Self::Floating32(f32::from_bits(cur.take_u32_le()?)),
            T::PTYP_CURRENCY => {
                let raw = cur.take_bytes(8)?;
                Self::Currency(i64::from_le_bytes([
                    raw[0], raw[1], raw[2], raw[3], raw[4], raw[5], raw[6], raw[7],
                ]))
            }
            T::PTYP_TIME | T::PTYP_FLOATING_TIME => Self::Time(cur.take_u64_le()?),
            T::PTYP_GUID => {
                let mut g = [0u8; 16];
                g.copy_from_slice(cur.take_bytes(16)?);
                Self::Guid(g)
            }
            T::PTYP_STRING8 => {
                let raw = Self::take_terminated_string8(cur)?;
                Self::String8(String::from_utf8_lossy(&raw).into_owned())
            }
            T::PTYP_STRING => Self::String(Self::take_terminated_utf16(cur)?),
            T::PTYP_BINARY => {
                let n = usize::from(cur.take_u16_le()?);
                Self::Binary(cur.take_bytes(n)?.to_vec())
            }
            // Plain multivalue types (PtypMultiple*, 0x1000): consume their
            // MV-count-prefixed payload so the cursor stays aligned. Without
            // this arm a PtypMultiple* tag hits `decode_opaque` which returns
            // an empty value but consumes zero bytes, leaving the MV payload
            // in the buffer and desyncing the next field/ROP.
            mv if Self::is_multivalue(mv) => Self::skip_multivalue(cur, mv)?,
            other => return Self::decode_opaque(cur, other),
        })
    }

    /// Read a NUL-terminated UTF-16 (PtypString) value in row form, returning
    /// the decoded `String` (without the terminating 0x0000).
    fn take_terminated_utf16(cur: &mut Buf<'_>) -> Result<String, DecodeError> {
        let mut units = Vec::new();
        loop {
            let u = cur.take_u16_le()?;
            if u == 0 {
                break;
            }
            units.push(u);
        }
        Ok(String::from_utf16_lossy(&units))
    }

    /// Read a NUL-terminated String8 value in row form, returning the raw
    /// bytes (without the terminating 0x00).
    fn take_terminated_string8(cur: &mut Buf<'_>) -> Result<Vec<u8>, DecodeError> {
        let mut bytes = Vec::new();
        loop {
            let b = cur.take_u8()?;
            if b == 0 {
                break;
            }
            bytes.push(b);
        }
        Ok(bytes)
    }

    /// ROP-buffer encoding (MS-OXCDATA section 2.11.4): the per-type bytes in
    /// the shape used inside `RopSetProperties`/`RopDeleteProperties`/
    /// `RopGetPropertiesSpecific` payloads. The difference from the row
    /// [`encode`] is the `PtypString`/`PtypString8` element: in a ROP buffer
    /// it is prefixed by a 2-byte count (UTF-16 code units for String, bytes
    /// for String8), while in table rows the same value is NUL-terminated
    /// without a prefix (cubic review #31).
    pub fn encode_rop_buffer(&self, out: &mut Vec<u8>) {
        match self {
            Self::Null => {}
            Self::Boolean(b) => out.push(u8::from(*b)),
            Self::Integer16(v) => out.extend_from_slice(&v.to_le_bytes()),
            Self::Integer32(v) => out.extend_from_slice(&v.to_le_bytes()),
            Self::Integer64(v) => out.extend_from_slice(&v.to_le_bytes()),
            Self::Floating32(v) => out.extend_from_slice(&v.to_le_bytes()),
            Self::Floating64(v) => out.extend_from_slice(&v.to_le_bytes()),
            Self::Currency(v) => out.extend_from_slice(&v.to_le_bytes()),
            Self::Time(v) => out.extend_from_slice(&v.to_le_bytes()),
            Self::Guid(g) => out.extend_from_slice(g),
            Self::ErrorCode(v) => out.extend_from_slice(&v.to_le_bytes()),
            Self::String(s) => {
                let max_units = (u16::MAX as usize) / 2;
                let units: Vec<u16> = s.encode_utf16().take(max_units).collect();
                let count =
                    u16::try_from(units.len()).expect("take capped at u16::MAX/2 code units");
                out.extend_from_slice(&count.to_le_bytes());
                for u in &units {
                    out.extend_from_slice(&u.to_le_bytes());
                }
                out.extend_from_slice(&0u16.to_le_bytes()); // terminating NUL
            }
            Self::String8(s) => {
                let max = u16::MAX as usize;
                let bytes = s.as_bytes();
                let take = bytes.len().min(max);
                let count = u16::try_from(take).unwrap_or(u16::MAX);
                out.extend_from_slice(&count.to_le_bytes());
                out.extend_from_slice(&bytes[..take]);
                out.push(0); // terminating NUL
            }
            Self::Binary(b) => {
                let max = u16::MAX as usize;
                let take = b.len().min(max);
                out.extend_from_slice(&u16::try_from(take).unwrap_or(u16::MAX).to_le_bytes());
                out.extend_from_slice(&b[..take]);
            }
            Self::Opaque { bytes, .. } => {
                out.extend_from_slice(bytes);
            }
        }
    }

    /// The PropertyType this value serialises as.
    pub const fn property_type(&self) -> PropertyType {
        match self {
            Self::Null => PropertyType::PTYP_NULL,
            Self::Boolean(_) => PropertyType::PTYP_BOOLEAN,
            Self::Integer16(_) => PropertyType::PTYP_INTEGER16,
            Self::Integer32(_) => PropertyType::PTYP_INTEGER32,
            Self::Integer64(_) => PropertyType::PTYP_INTEGER64,
            Self::Floating32(_) => PropertyType::PTYP_FLOATING32,
            Self::Floating64(_) => PropertyType::PTYP_FLOATING64,
            Self::Currency(_) => PropertyType::PTYP_CURRENCY,
            Self::Time(_) => PropertyType::PTYP_TIME,
            Self::Guid(_) => PropertyType::PTYP_GUID,
            Self::ErrorCode(_) => PropertyType::PTYP_ERROR_CODE,
            Self::String(_) => PropertyType::PTYP_STRING,
            Self::String8(_) => PropertyType::PTYP_STRING8,
            Self::Binary(_) => PropertyType::PTYP_BINARY,
            Self::Opaque { property_type, .. } => *property_type,
        }
    }
}

/// A `(PropertyTag, PropertyValue)` pair as emitted in a row or a
/// `GetPropertiesSpecific` response.
#[derive(Debug, Clone, PartialEq)]
pub struct PropertyRowEntry {
    pub tag: PropertyTag,
    pub value: PropertyValue,
}

impl PropertyRowEntry {
    pub fn encode(&self, out: &mut Vec<u8>) {
        self.tag.encode(out);
        self.value.encode(out);
    }
}

/// `TaggedPropertyValue` (MS-OXCDATA §2.11.4): a self-describing
/// `(PropertyTag, PropertyValue)` pair that carries its own tag inline. This
/// is the array element of the `RopSetProperties` request `PropertyValues`
/// field — unlike the implicit-tag rows of `RopQueryRows`, each entry
/// names the property it sets so the server can apply them out of column-set
/// order.
#[derive(Debug, Clone, PartialEq)]
pub struct TaggedPropertyValue {
    pub tag: PropertyTag,
    pub value: PropertyValue,
}

impl TaggedPropertyValue {
    /// Decode a single `TaggedPropertyValue`: the 4-byte `PropertyTag` then
    /// the typed `PropertyValue` for that tag (MS-OXCDATA §2.11.4).
    pub fn decode(cur: &mut Buf<'_>) -> Result<Self, DecodeError> {
        let tag = PropertyTag::decode(cur)?;
        let value = PropertyValue::decode(cur, &tag)?;
        Ok(Self { tag, value })
    }

    pub fn encode(&self, out: &mut Vec<u8>) {
        self.tag.encode(out);
        self.value.encode(out);
    }

    /// ROP-buffer encoding (MS-OXCDATA §2.11.4 / §2.11.2.1): the tag followed
    /// by the value in its count-prefixed form (`PtypString`/`PtypString8`
    /// carry a 2-byte char/byte count, then the payload, then the NUL
    /// terminator). This is the shape used inside the `RopSetProperties` /
    /// `RopDeleteProperties` request `PropertyValues` array — distinct from
    /// the NUL-terminated-without-prefix row form emitted by `encode`
    /// (table rows, `RopQueryRows`). Added because the plain `encode` used
    /// the row form, which a future re-encode of a decoded SetProperties
    /// payload would silently shrink (cubic review #31).
    pub fn encode_rop_buffer(&self, out: &mut Vec<u8>) {
        self.tag.encode(out);
        self.value.encode_rop_buffer(out);
    }
}

/// `PropertyProblem` (MS-OXCDATA §2.7): the per-property error block returned
/// by `RopSetProperties` / `RopDeleteProperties` / `RopCopyTo`. `index` is the
/// 0-based position of the offending entry in the request's property array,
/// `tag` echoes the property, and `error_code` is the MAPI HRESULT for that
/// property alone (the ROP-level `ReturnValue` reports the aggregate).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PropertyProblem {
    pub index: u16,
    pub tag: PropertyTag,
    pub error_code: u32,
}

impl PropertyProblem {
    pub const SIZE: usize = 2 + 4 + 4;

    pub fn encode(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&self.index.to_le_bytes());
        self.tag.encode(out);
        out.extend_from_slice(&self.error_code.to_le_bytes());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn property_type_roundtrips() {
        for v in [
            0x0000u16, 0x0001, 0x0002, 0x0003, 0x0004, 0x0005, 0x0006, 0x0007, 0x000A, 0x000B,
            0x0014, 0x001E, 0x001F, 0x0040, 0x0048, 0x00FB, 0x00FD, 0x00FE, 0x0102, 0x1002, 0x1003,
            0x1004, 0x1005, 0x1006, 0x1007, 0x1014, 0x101E, 0x101F, 0x1040, 0x1048, 0x1102, 0x2002,
        ] {
            let t = PropertyType::from_u16(v);
            assert_eq!(t.to_u16(), v, "v={v:#06x}");
        }
    }

    #[test]
    fn named_property_flag_detected() {
        let t = PropertyTag::new(PropertyType::PTYP_STRING, 0x8010);
        assert!(t.is_named());
        let t = PropertyTag::new(PropertyType::PTYP_STRING, 0x0010);
        assert!(!t.is_named());
    }

    #[test]
    fn property_tag_roundtrip() {
        let t = PropertyTag::new(PropertyType::PTYP_INTEGER32, 0x0100);
        let mut buf = Vec::new();
        t.encode(&mut buf);
        let mut cur = Buf::new(&buf);
        let got = PropertyTag::decode(&mut cur).expect("decode");
        assert_eq!(got, t);
        assert_eq!(cur.remaining(), 0);
    }

    #[test]
    fn scalar_value_encoding_matches_sizes() {
        let mut b = Vec::new();
        PropertyValue::Boolean(true).encode(&mut b);
        assert_eq!(b, [1]);

        let mut b = Vec::new();
        PropertyValue::Integer32(-5).encode(&mut b);
        assert_eq!(b, (-5i32).to_le_bytes());

        let mut b = Vec::new();
        PropertyValue::Time(0x01FFFFFFFF).encode(&mut b);
        assert_eq!(b, 0x01FFFFFFFFu64.to_le_bytes());
    }

    #[test]
    fn string_value_includes_terminator() {
        // Per MS-OXCDATA §2.11.2.1 a PtypString PropertyValue is the UTF-16LE
        // code units INCLUDING the 0x0000 terminator, with NO length prefix.
        let mut b = Vec::new();
        PropertyValue::String("AB".into()).encode(&mut b);
        // 'A', 'B', then single terminating NUL (0x0000).
        assert_eq!(u16::from_le_bytes([b[0], b[1]]), b'A' as u16);
        assert_eq!(u16::from_le_bytes([b[2], b[3]]), b'B' as u16);
        assert_eq!(u16::from_le_bytes([b[4], b[5]]), 0);
        assert_eq!(b.len(), 6);
    }

    #[test]
    fn string8_value_includes_terminator() {
        // PtypString8: bytes + single trailing 0x00, no length prefix.
        let mut b = Vec::new();
        PropertyValue::String8("AB".into()).encode(&mut b);
        assert_eq!(b, vec![b'A', b'B', 0x00]);
    }

    #[test]
    fn binary_value_length_prefixed() {
        let mut b = Vec::new();
        PropertyValue::Binary(vec![0xDE, 0xAD, 0xBE, 0xEF]).encode(&mut b);
        assert_eq!(u16::from_le_bytes([b[0], b[1]]), 4);
        assert_eq!(&b[2..], &[0xDE, 0xAD, 0xBE, 0xEF]);
    }

    #[test]
    fn floating_time_is_8_bytes() {
        // PidTagAutoForwarded / PtypFloatingTime (0x000D) is an 8-byte IEEE
        // double used as a fractional-day time; the size table regrouped it
        // from 4 to 8 bytes so multivalue sizing matched the single-element
        // width (qodo #6 / cubic #3).
        assert_eq!(PropertyType::PTYP_FLOATING_TIME.fixed_size(), Some(8));
    }

    #[test]
    fn floating_time_decodes_as_time_variant() {
        // PTYP_FLOATING_TIME carries an unsigned 64-bit payload the same way
        // PTYP_TIME does; collapsing it into the Time variant (instead of an
        // untyped Opaque) keeps the value typed for callers. Encode a u64,
        // decode under the FLOATING_TIME tag, expect Time.
        let raw: u64 = 132555555550000000;
        let tag = PropertyTag::new(PropertyType::PTYP_FLOATING_TIME, 0x3000);
        let le = raw.to_le_bytes();
        let mut cur = Buf::new(&le);
        let pv = PropertyValue::decode(&mut cur, &tag).expect("decode");
        assert_eq!(pv, PropertyValue::Time(raw));
        assert_eq!(cur.remaining(), 0);
    }

    #[test]
    fn integer16_negative_decodes_signed() {
        // PtypInteger16 (0x0002) is signed; the read must bit-reinterpret the
        // little-endian u16 into i16 (a naive i16::try_from(u16) rejects
        // values 0x8000?0xFFFF and would have lost negatives). Encode -1 and
        // round-trip.
        let tag = PropertyTag::new(PropertyType::PTYP_INTEGER16, 0x3001);
        let le: [u8; 2] = 0xFFFFu16.to_le_bytes();
        let mut cur = Buf::new(&le);
        let pv = PropertyValue::decode(&mut cur, &tag).expect("decode");
        assert_eq!(pv, PropertyValue::Integer16(-1));
        assert_eq!(cur.remaining(), 0);
    }

    #[test]
    fn integer64_negative_decodes_signed() {
        // PtypInteger64 (0x0014) signed: encode -7 as u64 LE and decode.
        let tag = PropertyTag::new(PropertyType::PTYP_INTEGER64, 0x3002);
        let raw: i64 = -7;
        let le = raw.to_le_bytes();
        let mut cur = Buf::new(&le);
        let pv = PropertyValue::decode(&mut cur, &tag).expect("decode");
        assert_eq!(pv, PropertyValue::Integer64(-7));
    }

    #[test]
    fn currency_negative_decodes_signed() {
        // PtypCurrency (0x0006) signed 64-bit (cents). Encode a negative.
        let tag = PropertyTag::new(PropertyType::PTYP_CURRENCY, 0x3003);
        let raw: i64 = -123456;
        let le = raw.to_le_bytes();
        let mut cur = Buf::new(&le);
        let pv = PropertyValue::decode(&mut cur, &tag).expect("decode");
        assert_eq!(pv, PropertyValue::Currency(-123456));
    }

    #[test]
    fn multivalue_opaque_captures_consumed_bytes() {
        // An MV element decodes to Opaque carrying the *exact* consumed span
        // (u32 element count + per-element encodings), not an empty blob, so a
        // future re-encode round-trips byte-for-byte and the chain cursor stays
        // aligned (cubic #15). Build an MV String8 (PTYP_MV_STRING8 = 0x101E):
        // a 4-byte LE element count (1) followed by one NUL-terminated String8
        // element ("XY" + 0x00). Total 7 bytes; the decoder captures all 7.
        let tag = PropertyTag::new(PropertyType::from_u16(0x101E), 0x3004);
        let bytes: Vec<u8> = vec![0x01, 0x00, 0x00, 0x00, b'X', b'Y', 0x00];
        assert_eq!(bytes.len(), 7);
        let mut cur = Buf::new(&bytes);
        let pv = PropertyValue::decode(&mut cur, &tag).expect("decode");
        match pv {
            PropertyValue::Opaque {
                property_type,
                bytes: got,
            } => {
                assert_eq!(property_type, PropertyType::from_u16(0x101E));
                assert_eq!(got, bytes, "Opaque must capture the full consumed span");
            }
            other => panic!("expected Opaque, got {other:?}"),
        }
        assert_eq!(cur.remaining(), 0);
    }

    #[test]
    fn encode_rop_buffer_string_is_count_prefixed() {
        // ROP-buffer form: 2-byte UTF-16 code-unit count, then the units,
        // then the 0x0000 terminator (cubic review #31). The row form omits
        // the count prefix; this asserts they differ and that the buffer
        // form decodes back via a manual prefix-stripping reader.
        let mut row = Vec::new();
        let mut rop = Vec::new();
        PropertyValue::String("A".into()).encode(&mut row);
        PropertyValue::String("A".into()).encode_rop_buffer(&mut rop);
        // "A" = 0x0041 (1 UTF-16 unit). row = 41 00 00 00 ; rop = 01 00 41 00 00 00
        assert_eq!(row, vec![0x41, 0x00, 0x00, 0x00]);
        assert_eq!(rop, vec![0x01, 0x00, 0x41, 0x00, 0x00, 0x00]);
    }

    #[test]
    fn encode_rop_buffer_string8_is_count_prefixed() {
        let mut row = Vec::new();
        let mut rop = Vec::new();
        PropertyValue::String8("AB".into()).encode(&mut row);
        PropertyValue::String8("AB".into()).encode_rop_buffer(&mut rop);
        // row = 41 42 00 ; rop = 02 00 41 42 00
        assert_eq!(row, vec![b'A', b'B', 0x00]);
        assert_eq!(rop, vec![0x02, 0x00, b'A', b'B', 0x00]);
    }

    #[test]
    fn encode_rop_buffer_binary_matches_row_form() {
        // Binary is count-prefixed in BOTH forms (the prefix is part of the
        // value itself), so the two encoders agree here.
        let mut row = Vec::new();
        let mut rop = Vec::new();
        PropertyValue::Binary(vec![0x01, 0x02]).encode(&mut row);
        PropertyValue::Binary(vec![0x01, 0x02]).encode_rop_buffer(&mut rop);
        assert_eq!(row, rop);
        assert_eq!(row, vec![0x02, 0x00, 0x01, 0x02]);
    }

    #[test]
    fn tagged_value_rop_buffer_roundtrips_through_rops_decoder() {
        // Sanity: TaggedPropertyValue::encode_rop_buffer produces a
        // count-prefixed String the existing (count-prefixed) decoder can
        // parse back exactly — the asymmetry that motivated the new encoder.
        use crate::mapi::rops as r;
        let tv = TaggedPropertyValue {
            tag: PropertyTag::new(PropertyType::PTYP_STRING, 0x0037),
            value: PropertyValue::String("Hello".into()),
        };
        let mut out = Vec::new();
        tv.encode_rop_buffer(&mut out);
        let mut cur = r::Buf::new(&out);
        let got = TaggedPropertyValue::decode(&mut cur).expect("decode");
        assert_eq!(got, tv);
        assert_eq!(cur.remaining(), 0);
    }

    proptest::proptest! {
        #[test]
        fn property_tag_roundtrip_prop(pt in 0u16..=0x1FFFu16, id in 0u16..=0xFFFFu16) {
            let t = PropertyTag {
                property_type: PropertyType::from_u16(pt),
                property_id: id,
            };
            let mut buf = Vec::new();
            t.encode(&mut buf);
            let mut cur = Buf::new(&buf);
            let got = PropertyTag::decode(&mut cur).expect("decode");
            proptest::prop_assert_eq!(got, t);
            proptest::prop_assert_eq!(cur.remaining(), 0);
        }

        #[test]
        fn integer_value_roundtrips(i in -10_000_000i32..10_000_000i32) {
            let v = PropertyValue::Integer32(i);
            assert_eq!(v.property_type(), PropertyType::PTYP_INTEGER32);
        }

        #[test]
        fn time_value_roundtrips(t in 0u64..u64::MAX) {
            let v = PropertyValue::Time(t);
            assert_eq!(v.property_type(), PropertyType::PTYP_TIME);
            let mut buf = Vec::new();
            v.encode(&mut buf);
            proptest::prop_assert_eq!(buf, t.to_le_bytes());
        }
    }
}
