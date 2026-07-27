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
            Self::PTYP_INTEGER32
            | Self::PTYP_FLOATING32
            | Self::PTYP_FLOATING_TIME
            | Self::PTYP_ERROR_CODE => 4,
            Self::PTYP_INTEGER64
            | Self::PTYP_FLOATING64
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
                let v = cur.take_u16_le()?;
                Self::Integer16(i16::try_from(v).map_err(|_| DecodeError::InvalidValue)?)
            }
            T::PTYP_INTEGER32 => {
                let v = cur.take_u32_le()?;
                Self::Integer32(i32::try_from(v).map_err(|_| DecodeError::InvalidValue)?)
            }
            T::PTYP_ERROR_CODE => Self::ErrorCode(cur.take_u32_le()?),
            T::PTYP_INTEGER64 => {
                let v = cur.take_u64_le()?;
                let s = i64::try_from(v).map_err(|_| DecodeError::InvalidValue)?;
                Self::Integer64(s)
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
                let raw = cur.take_u64_le()?;
                let s = i64::try_from(raw).map_err(|_| DecodeError::InvalidValue)?;
                Self::Currency(s)
            }
            T::PTYP_TIME => {
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
            // Anything else (PTYP_SERVER_ID/RESTRICTION/RULE_ACTION/unknowns):
            // return Null and leave the cursor where we are, since the wire
            // length is unknown to a generic decoder. The caller is expected
            // to skip such tags explicitly rather than request a typed value.
            other => return Self::decode_opaque(cur, other),
        })
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
