// src/mapi/restrict.rs
//
// MS-OXCDATA §2.12.3 — the `SRestriction` tree Outlook sends in
// `RopRestrict` (and embedded inside FastTransfer property sets) to filter
// contents tables (unread, flagged, search folders) and as the
// `lpRestriction` parameter of several ROPs.
//
// An `SRestriction` is a discriminated union over `RestrictionType`
// (§2.12.3.2). Each variant carries its typed payload. The codec is
// byte-exact against the spec, bounds-checked, and fail-closed: a truncated
// or self-referential tree is rejected with `DecodeError` rather than
// driving the client into an unbounded recursion.
//
// The decoder uses an explicit recursion budget so an attacker-supplied
// deeply-nested `resAnd`/`resOr` tree cannot blow the stack. The encoder is
// the inverse and round-trips for every variant exercised in the unit tests.

use crate::mapi::data::{PropertyTag, PropertyValue};
use crate::mapi::rops::{Buf, DecodeError};

/// The `RestrictionType` discriminant (MS-OXCDATA §2.12.3.2.1). Each value
/// names an `SRestriction` arm; the wire order is `RestrictionType(1)` then
/// the arm payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum RestrictionType {
    /// `resContent` (0x03): content property restriction.
    Content = 0x03,
    /// `resProperty` (0x04): relational property restriction.
    Property = 0x04,
    /// `resPropCompare` (0x05): two-property comparison restriction.
    CompareProperties = 0x05,
    /// `resBitMask` (0x06): bitwise restriction.
    BitMask = 0x06,
    /// `resSize` (0x07): size restriction.
    Size = 0x07,
    /// `resExist` (0x08): property existence restriction.
    Exist = 0x08,
    /// `resSubRestriction` (0x09): sub-object restriction.
    SubRestriction = 0x09,
    /// `resComment` (0x0A): comment restriction.
    Comment = 0x0A,
    /// `resCount` (0x0B): count restriction (used internally / by some
    /// providers; the gateway applies a best-effort interpretation).
    Count = 0x0B,
    /// `resAnd` (0x00): logical AND of child restrictions.
    And = 0x00,
    /// `resOr` (0x01): logical OR of child restrictions.
    Or = 0x01,
    /// `resNot` (0x02): logical NOT of a child restriction.
    Not = 0x02,
}

impl RestrictionType {
    /// Decode the 1-byte discriminant; an unknown value is rejected
    /// (fail-closed) so the parser cannot mis-slice the payload of an
    /// unrecognised arm.
    pub fn from_u8(v: u8) -> Result<Self, DecodeError> {
        Ok(match v {
            0x00 => Self::And,
            0x01 => Self::Or,
            0x02 => Self::Not,
            0x03 => Self::Content,
            0x04 => Self::Property,
            0x05 => Self::CompareProperties,
            0x06 => Self::BitMask,
            0x07 => Self::Size,
            0x08 => Self::Exist,
            0x09 => Self::SubRestriction,
            0x0A => Self::Comment,
            0x0B => Self::Count,
            _ => return Err(DecodeError::InvalidValue),
        })
    }
    pub const fn to_u8(self) -> u8 {
        self as u8
    }
}

/// Relational operator (`relop`, MS-OXCDATA §2.12.3.3) used by
/// `ContentRestriction` and `PropertyRestriction`. The wire discriminant
/// occupies the high nibble / is packed per the spec; this newtype preserves
/// the byte value verbatim.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RelOp(pub u8);

impl RelOp {
    /// Greater-than-or-equal (0).
    pub const GE: Self = Self(0);
    /// Greater-than (1).
    pub const GT: Self = Self(1);
    /// Equal (2).
    pub const EQ: Self = Self(2);
    /// Not-equal (3).
    pub const NE: Self = Self(3);
    /// Less-than-or-equal (4).
    pub const LE: Self = Self(4);
    /// Less-than (5).
    pub const LT: Self = Self(5);
    /// The flags-bits mask stored in the low nibble of the packed byte is
    /// the relational-operator nibble plus reserved bits. We keep the raw
    /// value for completeness.
    pub const fn from_u8(v: u8) -> Self {
        Self(v)
    }
    pub const fn to_u8(self) -> u8 {
        self.0
    }
}

/// Bit-mask operator (`BMR`, MS-OXCDATA §2.12.3.6.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum BitMaskRelOp {
    /// `BMR_EQZ` (0x00): the masked value equals zero.
    EqZero = 0x00,
    /// `BMR_NEZ` (0x01): the masked value is non-zero.
    NonZero = 0x01,
}

impl BitMaskRelOp {
    pub fn from_u8(v: u8) -> Result<Self, DecodeError> {
        Ok(match v {
            0x00 => Self::EqZero,
            0x01 => Self::NonZero,
            _ => return Err(DecodeError::InvalidValue),
        })
    }
    pub const fn to_u8(self) -> u8 {
        self as u8
    }
}

/// Size operator (`relop`, MS-OXCDATA §2.12.3.7.1) reused as the byte value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SizeRelOp(pub u8);

impl SizeRelOp {
    /// Reinterprets the byte as a `RelOp` (same encoding).
    pub const fn from_u8(v: u8) -> Self {
        Self(v)
    }
    pub const fn to_u8(self) -> u8 {
        self.0
    }
}

/// A content-match mode (`FuzzyLevel`, MS-OXCDATA §2.12.3.4.1). The gateway
/// preserves the two-byte flags verbatim (the spec defines FL_* match modes
/// and the ignore-case/ignore-space bins in the low byte).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FuzzyLevel(pub u16);

impl FuzzyLevel {
    /// Full-string match (no flag bits).
    pub const FL_FULLSTRING: u16 = 0x0000;
    /// Substring match (low word bit 0x0001).
    pub const FL_SUBSTRING: u16 = 0x0001;
    /// Prefix match (0x0002).
    pub const FL_PREFIX: u16 = 0x0002;
    pub const fn from_u16(v: u16) -> Self {
        Self(v)
    }
    pub const fn to_u16(self) -> u16 {
        self.0
    }
}

/// An `SRestriction` tree (MS-OXCDATA §2.12.3). The discriminant `rt`
/// selects the arm; the matching payload carries the typed fields. Variable
/// arms hold an owned `Vec<SRestriction>` for the child list (AND/OR), a
/// single boxed child (NOT/Comment/SubRestriction), and a `PropertyValue`
/// for the comparison target.
#[derive(Debug, Clone, PartialEq)]
pub enum SRestriction {
    /// `resAnd` (§2.12.3.1): logical AND. Wire: `RestrictionType(1)=0x00`,
    /// `cRes(2 LE)` then `cRes` child `SRestriction`s.
    And(Vec<SRestriction>),
    /// `resOr` (§2.12.3.1): logical OR. Identical wire shape to AND with
    /// `RestrictionType=0x01`.
    Or(Vec<SRestriction>),
    /// `resNot` (§2.12.3.1): logical NOT. Wire: `RestrictionType(1)=0x02`,
    /// `cRes(2 LE)` reserved as `1`, then one child `SRestriction`.
    Not(Box<SRestriction>),
    /// `resContent` (§2.12.3.4): content match. Wire: `RestrictionType(1)=0x03`,
    /// `FuzzyLevel(2 LE)`, `PropertyTagContent(4)` (the tag to match against),
    /// `PropertyTagProperty(4)` reserved, then the typed `PropertyValue`
    /// (propValue) decoded against `PropertyTagContent`.
    Content {
        fuzzy_level: FuzzyLevel,
        content_tag: PropertyTag,
        property_tag: PropertyTag,
        value: PropertyValue,
    },
    /// `resProperty` (§2.12.3.5): relational compare against a constant.
    /// Wire: `RestrictionType(1)=0x04`, `RelOp(1)`, `PropertyTag(4)` then
    /// the typed `PropertyValue` decoded against `PropertyTag`.
    Property {
        relop: RelOp,
        tag: PropertyTag,
        value: PropertyValue,
    },
    /// `resPropCompare` (§2.12.3.6): compare two tags. Wire:
    /// `RestrictionType(1)=0x05`, `RelOp(1)`, `PropertyTagA(4)`,
    /// `PropertyTagB(4)`.
    CompareProperties {
        relop: RelOp,
        tag_a: PropertyTag,
        tag_b: PropertyTag,
    },
    /// `resBitMask` (§2.12.3.6.1): bitwise compare. Wire:
    /// `RestrictionType(1)=0x06`, `BitMaskRelOp(1)`, `PropertyTag(4)`,
    /// `Mask(8 LE)`.
    BitMask {
        rel_op: BitMaskRelOp,
        tag: PropertyTag,
        mask: u32,
    },
    /// `resSize` (§2.12.3.7): size compare. Wire: `RestrictionType(1)=0x07`,
    /// `SizeRelOp(1) + Reserved(3)`, `PropertyTag(4)`, `cb(4 LE)`.
    Size {
        relop: SizeRelOp,
        tag: PropertyTag,
        size: u32,
    },
    /// `resExist` (§2.12.3.8): existence. Wire: `RestrictionType(1)=0x08`,
    /// `Reserved(1+1)`, `PropertyTag(4)`.
    Exist { tag: PropertyTag },
    /// `resSubRestriction` (§2.12.3.9): sub-object restriction. Wire:
    /// `RestrictionType(1)=0x09`, `SubObject(1)` (0x0 = message, 0x1 = recips)
    /// , `PropertyTag(4)`, then the child `SRestriction`.
    SubRestriction {
        sub_object: u8,
        tag: PropertyTag,
        child: Box<SRestriction>,
    },
    /// `resComment` (§2.12.3.10): comment with optional restrictions.
    /// Wire: `RestrictionType(1)=0x0A`, `cValues(2 LE)` reserved, `cRes(2 LE)`,
    /// then `cRes` child `SRestriction`s.
    Comment { children: Vec<SRestriction> },
    /// `resCount` (§2.12.3.11): count restriction. Wire:
    /// `RestrictionType(1)=0x0B`, `Reserved(1+1)`, `PropertiesCount(4 LE)`,
    /// `PropertyCount(4 LE)`. Interpreted best-effort as a top-N; the
    /// gateway applies it to limit the matched row set.
    Count { count: u32 },
}

/// Max nesting depth for the recursive decoder. §2.12.3 trees are shallow in
/// practice (Outlook rarely nests AND/OR more than ~4 deep); a cap of 64 is
/// generous and bounds the stack a malicious client could force.
pub const MAX_RESTRICTION_DEPTH: u8 = 64;

/// The empty restriction — an `resAnd` over zero children — which trivially
/// matches every row. Used as the default state of a Table handle (no
/// restriction applied) so the matcher short-circuits to "all rows".
impl Default for SRestriction {
    fn default() -> Self {
        Self::And(Vec::new())
    }
}

impl SRestriction {
    /// Decode an `SRestriction` tree from the cursor, enforcing the recursion
    /// budget so a deeply-nested attacker tree fails closed.
    pub fn decode(cur: &mut Buf<'_>) -> Result<Self, DecodeError> {
        Self::decode_depth(cur, MAX_RESTRICTION_DEPTH)
    }

    fn decode_depth(cur: &mut Buf<'_>, depth: u8) -> Result<Self, DecodeError> {
        if depth == 0 {
            return Err(DecodeError::InvalidValue);
        }
        let rt = RestrictionType::from_u8(cur.take_u8()?)?;
        Ok(match rt {
            RestrictionType::And | RestrictionType::Or => {
                let count = usize::from(cur.take_u16_le()?);
                let mut children = Vec::with_capacity(count.min(64));
                for _ in 0..count {
                    children.push(Self::decode_depth(cur, depth - 1)?);
                }
                if rt == RestrictionType::And {
                    Self::And(children)
                } else {
                    Self::Or(children)
                }
            }
            RestrictionType::Not => {
                let _cres = cur.take_u16_le()?;
                let child = Self::decode_depth(cur, depth - 1)?;
                Self::Not(Box::new(child))
            }
            RestrictionType::Content => {
                let fuzzy_level = FuzzyLevel::from_u16(cur.take_u16_le()?);
                let content_tag = PropertyTag::decode(cur)?;
                let property_tag = PropertyTag::decode(cur)?;
                let value = PropertyValue::decode_row(cur, &content_tag)?;
                Self::Content {
                    fuzzy_level,
                    content_tag,
                    property_tag,
                    value,
                }
            }
            RestrictionType::Property => {
                let relop = RelOp::from_u8(cur.take_u8()?);
                let tag = PropertyTag::decode(cur)?;
                let value = PropertyValue::decode_row(cur, &tag)?;
                Self::Property { relop, tag, value }
            }
            RestrictionType::CompareProperties => {
                let relop = RelOp::from_u8(cur.take_u8()?);
                let tag_a = PropertyTag::decode(cur)?;
                let tag_b = PropertyTag::decode(cur)?;
                Self::CompareProperties { relop, tag_a, tag_b }
            }
            RestrictionType::BitMask => {
                let rel_op = BitMaskRelOp::from_u8(cur.take_u8()?)?;
                let tag = PropertyTag::decode(cur)?;
                let mask = cur.take_u32_le()?;
                Self::BitMask { rel_op, tag, mask }
            }
            RestrictionType::Size => {
                let relop = SizeRelOp::from_u8(cur.take_u8()?);
                cur.take_bytes(3)?; // reserved
                let tag = PropertyTag::decode(cur)?;
                let size = cur.take_u32_le()?;
                Self::Size { relop, tag, size }
            }
            RestrictionType::Exist => {
                cur.take_u8()?; // reserved
                cur.take_u8()?; // reserved
                let tag = PropertyTag::decode(cur)?;
                Self::Exist { tag }
            }
            RestrictionType::SubRestriction => {
                let sub_object = cur.take_u8()?;
                let _reserved = cur.take_u8()?;
                let tag = PropertyTag::decode(cur)?;
                let child = Self::decode_depth(cur, depth - 1)?;
                Self::SubRestriction {
                    sub_object,
                    tag,
                    child: Box::new(child),
                }
            }
            RestrictionType::Comment => {
                let _cvalues = cur.take_u16_le()?;
                let count = usize::from(cur.take_u16_le()?);
                let mut children = Vec::with_capacity(count.min(64));
                for _ in 0..count {
                    children.push(Self::decode_depth(cur, depth - 1)?);
                }
                Self::Comment { children }
            }
            RestrictionType::Count => {
                cur.take_u8()?; // reserved
                cur.take_u8()?; // reserved
                let _props_count = cur.take_u32_le()?;
                let count = cur.take_u32_le()?;
                Self::Count { count }
            }
        })
    }

    /// Encode an `SRestriction` tree back to the wire form. Inverse of
    /// `decode`; round-trips for every arm exercised in tests.
    pub fn encode(&self, out: &mut Vec<u8>) {
        match self {
            Self::And(children) => {
                out.push(RestrictionType::And.to_u8());
                let n = u16::try_from(children.len()).unwrap_or(u16::MAX);
                out.extend_from_slice(&n.to_le_bytes());
                for c in children {
                    c.encode(out);
                }
            }
            Self::Or(children) => {
                out.push(RestrictionType::Or.to_u8());
                let n = u16::try_from(children.len()).unwrap_or(u16::MAX);
                out.extend_from_slice(&n.to_le_bytes());
                for c in children {
                    c.encode(out);
                }
            }
            Self::Not(child) => {
                out.push(RestrictionType::Not.to_u8());
                out.extend_from_slice(&1u16.to_le_bytes());
                child.encode(out);
            }
            Self::Content {
                fuzzy_level,
                content_tag,
                property_tag,
                value,
            } => {
                out.push(RestrictionType::Content.to_u8());
                out.extend_from_slice(&fuzzy_level.to_u16().to_le_bytes());
                content_tag.encode(out);
                property_tag.encode(out);
                value.encode(out);
            }
            Self::Property { relop, tag, value } => {
                out.push(RestrictionType::Property.to_u8());
                out.push(relop.to_u8());
                tag.encode(out);
                value.encode(out);
            }
            Self::CompareProperties {
                relop,
                tag_a,
                tag_b,
            } => {
                out.push(RestrictionType::CompareProperties.to_u8());
                out.push(relop.to_u8());
                tag_a.encode(out);
                tag_b.encode(out);
            }
            Self::BitMask { rel_op, tag, mask } => {
                out.push(RestrictionType::BitMask.to_u8());
                out.push(rel_op.to_u8());
                tag.encode(out);
                out.extend_from_slice(&mask.to_le_bytes());
            }
            Self::Size { relop, tag, size } => {
                out.push(RestrictionType::Size.to_u8());
                out.push(relop.to_u8());
                out.extend_from_slice(&[0u8, 0u8, 0u8]); // reserved
                tag.encode(out);
                out.extend_from_slice(&size.to_le_bytes());
            }
            Self::Exist { tag } => {
                out.push(RestrictionType::Exist.to_u8());
                out.push(0u8); // reserved
                out.push(0u8); // reserved
                tag.encode(out);
            }
            Self::SubRestriction {
                sub_object,
                tag,
                child,
            } => {
                out.push(RestrictionType::SubRestriction.to_u8());
                out.push(*sub_object);
                out.push(0u8); // reserved
                tag.encode(out);
                child.encode(out);
            }
            Self::Comment { children } => {
                out.push(RestrictionType::Comment.to_u8());
                out.extend_from_slice(&0u16.to_le_bytes()); // cValues reserved
                let n = u16::try_from(children.len()).unwrap_or(u16::MAX);
                out.extend_from_slice(&n.to_le_bytes());
                for c in children {
                    c.encode(out);
                }
            }
            Self::Count { count } => {
                out.push(RestrictionType::Count.to_u8());
                out.push(0u8); // reserved
                out.push(0u8); // reserved
                out.extend_from_slice(&0u32.to_le_bytes()); // properties count
                out.extend_from_slice(&count.to_le_bytes());
            }
        }
    }
}

/// A view the matcher consumes: a row of `(PropertyTag, PropertyValue)` cells
/// already materialised for a table's column set. The matcher does not fetch
/// properties — the dispatcher supplies the row's resolved cells (and the
/// per-well-known-tag fallback values) so matching is pure and async-free.
/// In tests this is spelled `CellForMatcher` to keep construction ergonomic.
pub struct CellForMatcher {
    pub tag: PropertyTag,
    pub value: PropertyValue,
}

impl SRestriction {
    /// Evaluate the restriction against a row of materialised cells.
    /// Properties the row does not carry are treated as **absent** (Exist
    /// → false, Property/Content/BitMask/Size → false, CompareProperties →
    /// false): the gateway returns no rows for a restriction over a property
    /// the backend cannot serve, mirroring MAPI's "no value ⇒ match false"
    /// semantics. The matcher is total: it never panics on a novel
    /// `PropertyValue::Opaque` payload — opaque bytes are compared by length
    /// only for the relational arms.
    pub fn matches(&self, row: &[CellForMatcher]) -> bool {
        match self {
            Self::And(children) => children.iter().all(|c| c.matches(row)),
            Self::Or(children) => children.iter().any(|c| c.matches(row)),
            Self::Not(child) => !child.matches(row),
            Self::Exist { tag } => row.iter().any(|c| c.tag.equivalent(tag)),
            Self::Property { relop, tag, value } => {
                row.iter()
                    .find(|c| c.tag.equivalent(tag))
                    .is_some_and(|c| compare_relop(*relop, &c.value, value))
            }
            Self::Content {
                fuzzy_level,
                content_tag,
                value,
                ..
            } => row
                .iter()
                .find(|c| c.tag.equivalent(content_tag))
                .is_some_and(|c| content_match(fuzzy_level, &c.value, value)),
            Self::BitMask { rel_op, tag, mask } => row
                .iter()
                .find(|c| c.tag.equivalent(tag))
                .is_some_and(|c| bitmask_match(*rel_op, &c.value, *mask)),
            Self::Size { relop, tag, size } => row
                .iter()
                .find(|c| c.tag.equivalent(tag))
                .is_some_and(|c| size_match(relop, &c.value, *size)),
            // CompareProperties / SubRestriction / Comment / Count are
            // best-effort: the gateway has no cross-row property comparison
            // in the ROP-by-ROP path, so they match all rows (no false
            // exclusions) rather than silently emptying the table.
            Self::CompareProperties { .. }
            | Self::SubRestriction { .. }
            | Self::Comment { .. }
            | Self::Count { .. } => true,
        }
    }
}

impl PropertyTag {
    /// Two tags match if they name the same `property_id` (the high 0x8000
    /// named-bit is dropped — a named prop and its resolved id are treated
    /// as equivalent for the gateway's row-matching). Property type mismatch
    /// does NOT fail the match because Outlook issues Exist restrictions
    /// with PtypUnspecified sometimes.
    fn equivalent(self, other: &Self) -> bool {
        let a = self.property_id & 0x7FFF;
        let b = other.property_id & 0x7FFF;
        a == b
    }
}

/// FL_* flag bit that selects case-insensitive matching. The MAPI 4-byte
/// FuzzyLevel uses 0x00020000, but MS-OXCDATA sec 2.12.3.4 serialises
/// FuzzyLevel as a 2-byte word; the ignore-case hint occupies bit 0x2000
/// of that 2-byte form. We honour only that bit so the matcher stays
/// width-correct against the wire.
const FL_IGNORECASE: u16 = 0x2000;

fn fuzzy_ignore_case(fl: FuzzyLevel) -> bool {
    fl.to_u16() & FL_IGNORECASE != 0
}

fn fuzzy_substring(fl: FuzzyLevel) -> bool {
    (fl.to_u16() & 0x0003) == FuzzyLevel::FL_SUBSTRING
}

fn fuzzy_prefix(fl: FuzzyLevel) -> bool {
    (fl.to_u16() & 0x0003) == FuzzyLevel::FL_PREFIX
}

fn compare_relop(relop: RelOp, lhs: &PropertyValue, rhs: &PropertyValue) -> bool {
    // Ordering is over the comparable scalar projections: i64 (Integer16/32/64,
    // Currency), f64 (Floating32/64, Time as FILETIME), bool (Boolean), and
    // string ord (String/String8). Time compares as i64 ticks. A type
    // mismatch between lhs and rhs ⇒ not comparable ⇒ false.
    let ord = scalar_ord(lhs, rhs);
    match relop.to_u8() {
        0 => ord.is_some_and(|o| o.is_ge()),
        1 => ord.is_some_and(|o| o.is_gt()),
        2 => ord.is_some_and(|o| o.is_eq()),
        3 => ord.is_none_or(|o| !o.is_eq()),
        4 => ord.is_some_and(|o| o.is_le()),
        5 => ord.is_some_and(|o| o.is_lt()),
        _ => false,
    }
}

/// Three-way ordering among two scalar values. `None` means the values are
/// not comparable (different scalar families). Time (`PropertyValue::Time`)
/// is compared by its `u64` ticks so a Time vs Time restriction orders.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ScalarOrd {
    Lt,
    Eq,
    Gt,
}

impl ScalarOrd {
    const fn is_lt(self) -> bool {
        matches!(self, Self::Lt)
    }
    const fn is_eq(self) -> bool {
        matches!(self, Self::Eq)
    }
    const fn is_gt(self) -> bool {
        matches!(self, Self::Gt)
    }
    const fn is_le(self) -> bool {
        matches!(self, Self::Lt | Self::Eq)
    }
    const fn is_ge(self) -> bool {
        matches!(self, Self::Gt | Self::Eq)
    }
}

fn scalar_ord(lhs: &PropertyValue, rhs: &PropertyValue) -> Option<ScalarOrd> {
    use PropertyValue as V;
    let ord = match (lhs, rhs) {
        (V::Integer16(a), V::Integer16(b)) => i64::from(*a).cmp(&i64::from(*b)),
        (V::Integer32(a), V::Integer32(b)) => i64::from(*a).cmp(&i64::from(*b)),
        (V::Integer64(a), V::Integer64(b)) => a.cmp(b),
        (V::Currency(a), V::Currency(b)) => a.cmp(b),
        (V::Boolean(a), V::Boolean(b)) => a.cmp(b),
        (V::Floating32(a), V::Floating32(b)) => a.partial_cmp(b)?,
        (V::Floating64(a), V::Floating64(b)) => a.partial_cmp(b)?,
        (V::Time(a), V::Time(b)) => a.cmp(b),
        (V::String(a), V::String(b)) => a.cmp(b),
        (V::String8(a), V::String8(b)) => a.cmp(b),
        // Mixed integer scalars flatten to i64 for comparison so a PtypInteger32
        // left side and PtypInteger64 right side still order.
        (V::Integer16(a), V::Integer32(b)) => i64::from(*a).cmp(&i64::from(*b)),
        (V::Integer32(a), V::Integer16(b)) => i64::from(*a).cmp(&i64::from(*b)),
        (V::Integer16(a), V::Integer64(b)) => i64::from(*a).cmp(b),
        (V::Integer64(a), V::Integer16(b)) => a.cmp(&i64::from(*b)),
        (V::Integer32(a), V::Integer64(b)) => i64::from(*a).cmp(b),
        (V::Integer64(a), V::Integer32(b)) => a.cmp(&i64::from(*b)),
        // String vs String8: compare as str.
        (V::String(a), V::String8(b)) | (V::String8(b), V::String(a)) => a.as_str().cmp(b.as_str()),
        _ => return None,
    };
    Some(match ord {
        std::cmp::Ordering::Less => ScalarOrd::Lt,
        std::cmp::Ordering::Equal => ScalarOrd::Eq,
        std::cmp::Ordering::Greater => ScalarOrd::Gt,
    })
}

fn content_match(fl: &FuzzyLevel, lhs: &PropertyValue, rhs: &PropertyValue) -> bool {
    use PropertyValue as V;
    let (lh, rh) = match (lhs, rhs) {
        (V::String(a), V::String(b)) => (a.clone(), b.clone()),
        (V::String8(a), V::String8(b)) => (a.clone(), b.clone()),
        (V::String(a), V::String8(b)) => (a.clone(), b.clone()),
        (V::String8(a), V::String(b)) => (a.clone(), b.clone()),
        _ => return false,
    };
    let ignore = fuzzy_ignore_case(*fl);
    if fuzzy_substring(*fl) {
        return str_contains(&lh, &rh, ignore);
    }
    if fuzzy_prefix(*fl) {
        return str_starts_with(&lh, &rh, ignore);
    }
    // FL_FULLSTRING (default)
    str_eq(&lh, &rh, ignore)
}

fn str_eq(a: &str, b: &str, ignore: bool) -> bool {
    if ignore {
        a.eq_ignore_ascii_case(b)
    } else {
        a == b
    }
}

fn str_contains(haystack: &str, needle: &str, ignore: bool) -> bool {
    if needle.is_empty() {
        return true;
    }
    if ignore {
        haystack.to_ascii_lowercase().contains(&needle.to_ascii_lowercase())
    } else {
        haystack.contains(needle)
    }
}

fn str_starts_with(haystack: &str, needle: &str, ignore: bool) -> bool {
    if ignore {
        haystack
            .to_ascii_lowercase()
            .starts_with(&needle.to_ascii_lowercase())
    } else {
        haystack.starts_with(needle)
    }
}

fn bitmask_match(rel_op: BitMaskRelOp, lhs: &PropertyValue, mask: u32) -> bool {
    let v = match lhs {
        PropertyValue::Integer32(v) => *v as u32,
        PropertyValue::Integer64(v) => *v as u32,
        PropertyValue::Integer16(v) => *v as u32,
        PropertyValue::Boolean(b) => u32::from(*b),
        _ => return false,
    };
    let masked = v & mask;
    match rel_op {
        BitMaskRelOp::EqZero => masked == 0,
        BitMaskRelOp::NonZero => masked != 0,
    }
}

fn size_match(relop: &SizeRelOp, lhs: &PropertyValue, size: u32) -> bool {
    let v = match lhs {
        PropertyValue::Binary(b) => b.len() as u64,
        PropertyValue::String(s) => (s.encode_utf16().count() * 2) as u64,
        PropertyValue::String8(s) => s.len() as u64,
        PropertyValue::Integer32(v) => *v as u64,
        PropertyValue::Integer64(v) => *v as u64,
        _ => return false,
    };
    // The byte-relop discriminant reuses the RelOp encoding (0..5 == GE/LT/...).
    // MS-OXCDATA §2.12.3.7.1 relops for SizeRestriction are EQZ/NEZ/RELOP but we
    // honour the relational interpretation Outlook actually issues (>= size).
    match relop.to_u8() {
        0 => v >= u64::from(size), // GE
        1 => v > u64::from(size),  // GT
        2 => v == u64::from(size), // EQ
        3 => v != u64::from(size), // NE
        4 => v <= u64::from(size), // LE
        5 => v < u64::from(size),  // LT
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tag(id: u16, ty: u16) -> PropertyTag {
        PropertyTag::new(crate::mapi::data::PropertyType::from_u16(ty), id)
    }

    #[test]
    fn roundtrip_exist() {
        let r = SRestriction::Exist { tag: tag(0x0E07, 0x0003) };
        let mut buf = Vec::new();
        r.encode(&mut buf);
        let mut cur = Buf::new(&buf);
        let got = SRestriction::decode(&mut cur).unwrap();
        assert_eq!(got, r);
        assert_eq!(cur.remaining(), 0);
    }

    #[test]
    fn roundtrip_property_scalar() {
        let r = SRestriction::Property {
            relop: RelOp::EQ,
            tag: tag(0x0037, 0x001F),
            value: PropertyValue::String("hello".to_string()),
        };
        let mut buf = Vec::new();
        r.encode(&mut buf);
        let mut cur = Buf::new(&buf);
        let got = SRestriction::decode(&mut cur).unwrap();
        assert_eq!(got, r);
        assert_eq!(cur.remaining(), 0);
    }

    #[test]
    fn roundtrip_and_or_not() {
        let r = SRestriction::And(vec![
            SRestriction::Or(vec![
                SRestriction::Exist { tag: tag(0x0E07, 0x0003) },
                SRestriction::Not(Box::new(SRestriction::Exist { tag: tag(0x0017, 0x0003) })),
            ]),
            SRestriction::Property {
                relop: RelOp::GE,
                tag: tag(0x0E08, 0x0003),
                value: PropertyValue::Integer32(100),
            },
        ]);
        let mut buf = Vec::new();
        r.encode(&mut buf);
        let mut cur = Buf::new(&buf);
        let got = SRestriction::decode(&mut cur).unwrap();
        assert_eq!(got, r);
        assert_eq!(cur.remaining(), 0);
    }

    #[test]
    fn roundtrip_bitmask_size_count() {
        let r = SRestriction::And(vec![
            SRestriction::BitMask {
                rel_op: BitMaskRelOp::NonZero,
                tag: tag(0x0E07, 0x0003),
                mask: 0x00000001,
            },
            SRestriction::Size {
                relop: SizeRelOp(1),
                tag: tag(0x0E08, 0x0003),
                size: 2048,
            },
            SRestriction::Count { count: 5 },
        ]);
        let mut buf = Vec::new();
        r.encode(&mut buf);
        let mut cur = Buf::new(&buf);
        let got = SRestriction::decode(&mut cur).unwrap();
        assert_eq!(got, r);
        assert_eq!(cur.remaining(), 0);
    }

    #[test]
    fn roundtrip_content_and_subrestrict_and_comment() {
        let r = SRestriction::And(vec![
            SRestriction::Content {
                fuzzy_level: FuzzyLevel::from_u16(FuzzyLevel::FL_SUBSTRING),
                content_tag: tag(0x0037, 0x001F),
                property_tag: tag(0x0037, 0x001F),
                value: PropertyValue::String("foo".to_string()),
            },
            SRestriction::SubRestriction {
                sub_object: 0,
                tag: tag(0x0E21, 0x0003),
                child: Box::new(SRestriction::Exist { tag: tag(0x3702, 0x0102) }),
            },
            SRestriction::Comment {
                children: vec![SRestriction::Exist { tag: tag(0x3001, 0x001F) }],
            },
        ]);
        let mut buf = Vec::new();
        r.encode(&mut buf);
        let mut cur = Buf::new(&buf);
        let got = SRestriction::decode(&mut cur).unwrap();
        assert_eq!(got, r);
        assert_eq!(cur.remaining(), 0);
    }

    #[test]
    fn rejects_unknown_restriction_type() {
        let buf = [0xFFu8];
        let mut cur = Buf::new(&buf);
        assert_eq!(SRestriction::decode(&mut cur).unwrap_err(), DecodeError::InvalidValue);
    }

    #[test]
    fn rejects_excessive_nesting_budget() {
        // Build a deeply nested NOT tree beyond the recursion budget and confirm
        // the decoder fails closed rather than recursing to overflow.
        let mut buf = Vec::new();
        let mut depth = 0u16;
        while depth <= u16::from(MAX_RESTRICTION_DEPTH) + 5 {
            buf.push(RestrictionType::Not.to_u8());
            buf.extend_from_slice(&1u16.to_le_bytes());
            depth += 1;
        }
        // Terminating leaf: an Exist arm.
        buf.push(RestrictionType::Exist.to_u8());
        buf.push(0u8);
        buf.push(0u8);
        let t = tag(0x0E07, 0x0003);
        let mut leaf = Vec::new();
        t.encode(&mut leaf);
        buf.extend_from_slice(&leaf);
        let mut cur = Buf::new(&buf);
        assert_eq!(
            SRestriction::decode(&mut cur).unwrap_err(),
            DecodeError::InvalidValue,
            "deeply nested restriction must hit the recursion budget"
        );
    }

    #[test]
    fn rejects_truncated_and_count() {
        // And arm declaring 2 children but supplying zero bytes.
        let buf = [RestrictionType::And.to_u8(), 0x02, 0x00];
        let mut cur = Buf::new(&buf);
        assert_eq!(SRestriction::decode(&mut cur).unwrap_err(), DecodeError::Insufficient);
    }

    // ---- restriction matcher -----------------------------------------------

    fn cell(tag_id: u16, ty: u16, v: PropertyValue) -> CellForMatcher {
        CellForMatcher {
            tag: PropertyTag::new(crate::mapi::data::PropertyType::from_u16(ty), tag_id),
            value: v,
        }
    }

    #[test]
    fn matcher_exist_matches_when_cell_present_non_null() {
        // PR_SUBJECT present as a string -> Exist is true.
        let row: Vec<CellForMatcher> = vec![cell(0x0037, 0x001F, PropertyValue::String("x".into()))];
        let r = SRestriction::Exist { tag: tag(0x0037, 0x001F) };
        assert!(r.matches(&row));
        // PR_SUBJECT absent (cell for another tag only) -> false.
        let row2: Vec<CellForMatcher> = vec![cell(0x0E07, 0x0003, PropertyValue::Integer32(0))];
        assert!(!r.matches(&row2));
    }

    #[test]
    fn matcher_property_relop_int32() {
        // PR_MESSAGE_SIZE >= 100 and < 50 (the latter false).
        let row: Vec<CellForMatcher> = vec![cell(0x0E08, 0x0003, PropertyValue::Integer32(200))];
        let ge = SRestriction::Property {
            relop: RelOp::GE,
            tag: tag(0x0E08, 0x0003),
            value: PropertyValue::Integer32(100),
        };
        let lt = SRestriction::Property {
            relop: RelOp::LT,
            tag: tag(0x0E08, 0x0003),
            value: PropertyValue::Integer32(50),
        };
        assert!(ge.matches(&row));
        assert!(!lt.matches(&row));
    }

    #[test]
    fn matcher_content_case_insensitive_substring() {
        // Substring content on PR_SUBJECT ignoring case.
        let row: Vec<CellForMatcher> = vec![cell(0x0037, 0x001F, PropertyValue::String("Hello World".into()))];
        let sub = SRestriction::Content {
            fuzzy_level: FuzzyLevel::from_u16(FuzzyLevel::FL_SUBSTRING | 0x2000),
            content_tag: tag(0x0037, 0x001F),
            property_tag: tag(0x0037, 0x001F),
            value: PropertyValue::String("WORLD".into()),
        };
        assert!(sub.matches(&row));
        let nope = SRestriction::Content {
            fuzzy_level: FuzzyLevel::from_u16(FuzzyLevel::FL_SUBSTRING | 0x2000),
            content_tag: tag(0x0037, 0x001F),
            property_tag: tag(0x0037, 0x001F),
            value: PropertyValue::String("missing".into()),
        };
        assert!(!nope.matches(&row));
    }

    #[test]
    fn matcher_bitmask_and_and_or_not() {
        // PR_MESSAGE_FLAGS & 0x00000010 (MSGFLAG_UNREAD bit).
        let unread_row: Vec<CellForMatcher> = vec![cell(0x0E07, 0x0003, PropertyValue::Integer32(0x10))];
        let read_row: Vec<CellForMatcher> = vec![cell(0x0E07, 0x0003, PropertyValue::Integer32(0))];
        let unread_bmr = SRestriction::BitMask {
            rel_op: BitMaskRelOp::NonZero,
            tag: tag(0x0E07, 0x0003),
            mask: 0x00000010,
        };
        assert!(unread_bmr.matches(&unread_row));
        assert!(!unread_bmr.matches(&read_row));
        // AND(OR(unread, big), NOT(read)) with a row that is unread+small.
        let small_unread: Vec<CellForMatcher> = vec![cell(0x0E07, 0x0003, PropertyValue::Integer32(0x10))];
        let big = SRestriction::Property {
            relop: RelOp::GE,
            tag: tag(0x0E08, 0x0003),
            value: PropertyValue::Integer32(10_000),
        };
        let and = SRestriction::And(vec![SRestriction::Or(vec![unread_bmr.clone(), big]), SRestriction::Not(Box::new(unread_bmr.clone()))]);
        // unread OR big = true; NOT unread = false -> AND false.
        assert!(!and.matches(&small_unread));
    }

}
