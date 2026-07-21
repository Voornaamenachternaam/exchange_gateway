// src/mapi/fxics.rs
//
// MS-OXCFXICS — the Fast Transfer Stream codec used by the
// RopFastTransferSource* family (MS-OXCROPS §2.2.12) to bulk-sync message and
// folder data to an Outlook client.
//
// Phase 0 provides:
//   * The complete marker table (§2.2.4.1.4), byte-exact, with the
//     start/end pairing the tokenizer uses to validate stream balance.
//   * A `TransferStream` tokenizer that walks a raw buffer of property
//     values + markers and emits a typed event sequence, fail-closed on
//     buffer overrun or unbalanced start/end markers.
//   * `IncrSyncState`/`IncrSyncChg`/`IncrSyncDel`/`IncrSyncRead`/`IncrSyncEnd`
//     builders for the ICS download case (Phase 1 will drive these off JMAP
//     `Email/changes`); Phase 0 establishes the serialisation surface and
//     the marker-balance validator so the producer is wire-correct.
//
// All decoding is bounds-checked; marker codes that do not match a table
// entry degrade to `Marker::Unknown(u32)` so the tokenizer stays total.

use crate::mapi::rops::DecodeError;

/// A Fast Transfer Stream marker (MS-OXCFXICS §2.2.4.1.4). Numeric value is a
/// 4-byte property-tag-shaped word: high word = family, low word = kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Marker {
    StartTopFld,
    StartSubFld,
    EndFolder,
    StartMessage,
    StartFaiMsg,
    EndMessage,
    StartEmbed,
    EndEmbed,
    StartRecip,
    EndToRecip,
    NewAttach,
    EndAttach,
    IncrSyncChg,
    IncrSyncChgPartial,
    IncrSyncDel,
    IncrSyncEnd,
    IncrSyncRead,
    IncrSyncStateBegin,
    IncrSyncStateEnd,
    IncrSyncProgressMode,
    IncrSyncProgressPerMsg,
    IncrSyncMessage,
    IncrSyncGroupInfo,
    FxErrorInfo,
    /// A 4-byte word that does not name a known marker. Carried verbatim so
    /// the tokenizer can still emit a deterministic event.
    Unknown(u32),
}

impl Marker {
    pub const fn value(self) -> u32 {
        match self {
            Self::StartTopFld => 0x40090003,
            Self::StartSubFld => 0x400A0003,
            Self::EndFolder => 0x400B0003,
            Self::StartMessage => 0x400C0003,
            Self::StartFaiMsg => 0x40100003,
            Self::EndMessage => 0x400D0003,
            Self::StartEmbed => 0x40010003,
            Self::EndEmbed => 0x40020003,
            Self::StartRecip => 0x40030003,
            Self::EndToRecip => 0x40040003,
            Self::NewAttach => 0x40000003,
            Self::EndAttach => 0x400E0003,
            Self::IncrSyncChg => 0x40120003,
            Self::IncrSyncChgPartial => 0x407D0003,
            Self::IncrSyncDel => 0x40130003,
            Self::IncrSyncEnd => 0x40140003,
            Self::IncrSyncRead => 0x402F0003,
            Self::IncrSyncStateBegin => 0x403A0003,
            Self::IncrSyncStateEnd => 0x403B0003,
            Self::IncrSyncProgressMode => 0x4074000B,
            Self::IncrSyncProgressPerMsg => 0x4075000B,
            Self::IncrSyncMessage => 0x40150003,
            Self::IncrSyncGroupInfo => 0x407B0102,
            Self::FxErrorInfo => 0x40180003,
            Self::Unknown(v) => v,
        }
    }

    pub const fn from_u32(v: u32) -> Self {
        match v {
            0x40090003 => Self::StartTopFld,
            0x400A0003 => Self::StartSubFld,
            0x400B0003 => Self::EndFolder,
            0x400C0003 => Self::StartMessage,
            0x40100003 => Self::StartFaiMsg,
            0x400D0003 => Self::EndMessage,
            0x40010003 => Self::StartEmbed,
            0x40020003 => Self::EndEmbed,
            0x40030003 => Self::StartRecip,
            0x40040003 => Self::EndToRecip,
            0x40000003 => Self::NewAttach,
            0x400E0003 => Self::EndAttach,
            0x40120003 => Self::IncrSyncChg,
            0x407D0003 => Self::IncrSyncChgPartial,
            0x40130003 => Self::IncrSyncDel,
            0x40140003 => Self::IncrSyncEnd,
            0x402F0003 => Self::IncrSyncRead,
            0x403A0003 => Self::IncrSyncStateBegin,
            0x403B0003 => Self::IncrSyncStateEnd,
            0x4074000B => Self::IncrSyncProgressMode,
            0x4075000B => Self::IncrSyncProgressPerMsg,
            0x40150003 => Self::IncrSyncMessage,
            0x407B0102 => Self::IncrSyncGroupInfo,
            0x40180003 => Self::FxErrorInfo,
            other => Self::Unknown(other),
        }
    }

    /// The matching end marker, if any.
    pub const fn end_marker(self) -> Option<Self> {
        Some(match self {
            Self::StartTopFld | Self::StartSubFld => Self::EndFolder,
            Self::StartMessage | Self::StartFaiMsg => Self::EndMessage,
            Self::StartEmbed => Self::EndEmbed,
            Self::StartRecip => Self::EndToRecip,
            Self::NewAttach => Self::EndAttach,
            Self::IncrSyncStateBegin => Self::IncrSyncStateEnd,
            _ => return None,
        })
    }
}

/// A typed event in a Fast Transfer Stream. Property values between markers
/// are carried as opaque `Vec<u8>` in Phase 0; the producer owns the typed
/// encoding via `data::PropertyValue` and serialises the bytes before pushing
/// them into the stream as `Property`.
#[derive(Debug, Clone, PartialEq)]
pub enum FxEvent {
    Marker(Marker),
    /// A property-tag-prefixed property-value blob, emitted verbatim so the
    /// producer controls the typed encoding.
    Property { tag: u32, bytes: Vec<u8> },
}

/// The balance-checked tokenizer. Reading the marker/code-point stream and
/// validating start/end pairing inhibits an attacker from sending a stream
/// that drives the Outlook client stack into unmatched-state code paths.
pub struct Tokenizer<'a> {
    buf: &'a [u8],
    pos: usize,
    /// LIFO stack of still-open markers awaiting their end marker.
    open: Vec<Marker>,
}

impl<'a> Tokenizer<'a> {
    pub fn new(buf: &'a [u8]) -> Self {
        Self {
            buf,
            pos: 0,
            open: Vec::new(),
        }
    }

    pub fn remaining(&self) -> usize {
        self.buf.len().saturating_sub(self.pos)
    }

    /// Read the next 4-byte LE word as a marker, or treat any other 4-byte
    /// word as a property. The MAPI/HTTP Fast Transfer Stream frames each
    /// item as a property tag (4 bytes); markers are a reserved subset of
    /// those tags.
    fn read_tag(&mut self) -> Result<u32, DecodeError> {
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

    /// Yield the next event, or `None` at end of stream.
    pub fn next_event(&mut self) -> Result<Option<FxEvent>, DecodeError> {
        if self.remaining() == 0 {
            return Ok(None);
        }
        let tag = self.read_tag()?;
        let marker = Marker::from_u32(tag);
        // `Marker::from_u32` is total: every known marker value maps to a
        // concrete variant, anything else maps to `Unknown(v)`. The earlier
        // additional `is_known_marker_value(tag)` re-check was dead logic
        // (it could never flip a `Unknown` back into a marker), so an
        // `Unknown(v)` here is unambiguously a property, not a marker.
        if !matches!(marker, Marker::Unknown(_)) {
            self.balance(marker)?;
            return Ok(Some(FxEvent::Marker(marker)));
        }
        // Property: the next two bytes give the value's count for variable
        // types; fixed-size types carry no count. Phase 0 determines the
        // length conservatively by PropertyType.fixed_size(); for variable
        // types it reads a 2-byte length prefix only if the high bit of the
        // type (0x1000… MV_INSTANCE) is unset and the type is variable.
        let property_type = crate::mapi::data::PropertyType::from_u16((tag & 0xFFFF) as u16);
        let bytes = self.read_property_payload(property_type)?;
        Ok(Some(FxEvent::Property { tag, bytes }))
    }

    /// Read the value payload for a property type in a FastTransfer stream
    /// (MS-OXCFXICS §2.2.4.1). For fixed-size scalar types, read the fixed
    /// number of bytes — note PtypBoolean is 2 bytes in FastTransfer (not 1).
    /// For variable-length types the outer `length` lexeme is a 4-byte
    /// PtypInteger32, EXCEPT for PtypBinary/PtypServerId (§2.2.4.1.1) which
    /// omits the outer length and serializes via their own 2-byte count per
    /// MS-OXCDATA §2.11. Bounded by the remaining buffer so an exaggerated
    /// length must fail closed.
    fn read_property_payload(&mut self, property_type: crate::mapi::data::PropertyType) -> Result<Vec<u8>, DecodeError> {
        use crate::mapi::data::PropertyType as Pt;
        // PtypBoolean is 2 bytes in FastTransfer per §2.2.4.1.3, not the 1
        // byte MS-OXCDATA fixed_size() returns for the property-row path.
        if property_type == Pt::PTYP_BOOLEAN {
            let bytes = self.buf.get(self.pos..self.pos + 2).ok_or(DecodeError::Insufficient)?;
            self.pos += 2;
            return Ok(bytes.to_vec());
        }
        if let Some(n) = property_type.fixed_size() {
            let bytes = self.buf.get(self.pos..self.pos + n).ok_or(DecodeError::Insufficient)?;
            self.pos += n;
            return Ok(bytes.to_vec());
        }
        // PtypBinary/PtypServerId carry NO outer length lexeme — they use the
        // MS-OXCDATA 2-byte count followed by the bytes.
        if property_type == Pt::PTYP_BINARY || property_type == Pt::PTYP_SERVER_ID {
            let count = self.read_u16_le()?;
            let count = usize::from(count);
            if count > self.remaining() {
                return Err(DecodeError::ExcessLength);
            }
            let bytes = self.buf.get(self.pos..self.pos + count).ok_or(DecodeError::Insufficient)?.to_vec();
            self.pos += count;
            return Ok(bytes);
        }
        // PtypString/PtypString8/PtypObject and code-page string variants:
        // 4-byte LE outer `length` lexeme then that many bytes.
        let count = self.read_u32_le()?;
        let count = usize::try_from(count).map_err(|_| DecodeError::ExcessLength)?;
        if count > self.remaining() {
            return Err(DecodeError::ExcessLength);
        }
        let bytes = self
            .buf
            .get(self.pos..self.pos + count)
            .ok_or(DecodeError::Insufficient)?
            .to_vec();
        self.pos += count;
        Ok(bytes)
    }

    fn read_u32_le(&mut self) -> Result<u32, DecodeError> {
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

    fn read_u16_le(&mut self) -> Result<u16, DecodeError> {
        if self.remaining() < 2 {
            return Err(DecodeError::Insufficient);
        }
        let v = u16::from_le_bytes([self.buf[self.pos], self.buf[self.pos + 1]]);
        self.pos += 2;
        Ok(v)
    }

    /// After exhausting the stream (`next_event` returning `Ok(None)`), call
    /// this to assert that no start marker was left open. A truncated stream
    /// (`StartMessage` then EOF; `StartTopFld` then `IncrSyncEnd`; etc.) is
    /// rejected with `DecodeError::InvalidValue` so Outlook cannot reach its
    /// "still inside a hierarchy/message" rollback path on attacker input.
    pub fn assert_complete(&self) -> Result<(), DecodeError> {
        if !self.open.is_empty() {
            return Err(DecodeError::InvalidValue);
        }
        Ok(())
    }

    /// Validate the start/end marker balance and record open markers.
    fn balance(&mut self, marker: Marker) -> Result<(), DecodeError> {
        // `IncrSyncEnd` is a standalone stream terminator (no matching start
        // marker); accept it with any balance state. The other "end" markers
        // pop their matching start marker from the open stack.
        if matches!(marker, Marker::IncrSyncEnd) {
            // A stray IncrSyncEnd mid-stream still leaves the stack balanced.
            return Ok(());
        }
        let is_end = matches!(
            marker,
            Marker::EndFolder
                | Marker::EndMessage
                | Marker::EndEmbed
                | Marker::EndToRecip
                | Marker::EndAttach
                | Marker::IncrSyncStateEnd
        );
        if is_end {
            let Some(open_marker) = self.open.last().copied() else {
                return Err(DecodeError::InvalidValue);
            };
            let wants = open_marker.end_marker();
            if wants != Some(marker) {
                return Err(DecodeError::InvalidValue);
            }
            self.open.pop();
        } else if marker.end_marker().is_some() {
            self.open.push(marker);
        }
        Ok(())
    }
}

/// Whether a 4-byte word names a known marker. Used only as an oracle in
/// the `marker_roundtrip` proptest (the live code uses `Marker::from_u32`,
/// which already returns a typed variant for every value in this set).
#[cfg(test)]
fn is_known_marker_value(v: u32) -> bool {
    matches!(
        v,
        0x40090003
            | 0x400A0003
            | 0x400B0003
            | 0x400C0003
            | 0x40100003
            | 0x400D0003
            | 0x40010003
            | 0x40020003
            | 0x40030003
            | 0x40040003
            | 0x40000003
            | 0x400E0003
            | 0x40120003
            | 0x407D0003
            | 0x40130003
            | 0x40140003
            | 0x402F0003
            | 0x403A0003
            | 0x403B0003
            | 0x4074000B
            | 0x4075000B
            | 0x40150003
            | 0x407B0102
            | 0x40180003
    )
}

/// A builder for an ICS download transfer stream. Phase 1 will feed this off
/// JMAP `Email/changes`; Phase 0 exposes the wire-correct construction API.
#[derive(Debug, Default)]
pub struct IcsStreamBuilder {
    out: Vec<u8>,
}

impl IcsStreamBuilder {
    pub fn new() -> Self {
        Self { out: Vec::new() }
    }
    pub fn push_marker(&mut self, m: Marker) {
        self.out.extend_from_slice(&m.value().to_le_bytes());
    }
    /// Push a `propValue` element whose wire format (MS-OXCFXICS §2.2.4.1)
    /// is `<tag(4)> <value>` for fixed types and PtypBinary/PtypServerId,
    /// or `<tag(4)> <length(4 LE)> <value>` for the other varPropType
    /// strings. The caller passes the *value bytes* (no length) and the
    /// tag's low 16 bits identify the property type, so this builder
    /// dispatches the length lexeme on the caller's behalf.
    pub fn push_property(&mut self, tag: u32, bytes: &[u8]) {
        self.out.extend_from_slice(&tag.to_le_bytes());
        let pt = crate::mapi::data::PropertyType::from_u16((tag & 0xFFFF) as u16);
        let needs_length = !matches!(pt, crate::mapi::data::PropertyType::PTYP_BINARY)
            && !matches!(pt, crate::mapi::data::PropertyType::PTYP_SERVER_ID)
            && pt.fixed_size().is_none();
        if needs_length {
            // bytes.len() fits in a usize on the target platform; cap to u32
            // to honour the wire width (any value above u32::MAX can never
            // legitimately transit a 4-byte length lexeme).
            let len = u32::try_from(bytes.len()).unwrap_or(u32::MAX);
            self.out.extend_from_slice(&len.to_le_bytes());
        }
        self.out.extend_from_slice(bytes);
    }
    /// Finalise the builder with `IncrSyncEnd` and return the buffer.
    pub fn finish(mut self) -> Vec<u8> {
        self.push_marker(Marker::IncrSyncEnd);
        self.out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn marker_table_roundtrips() {
        for v in [
            0x40090003u32, 0x400A0003, 0x400B0003, 0x400C0003, 0x40100003,
            0x400D0003, 0x40010003, 0x40020003, 0x40030003, 0x40040003,
            0x40000003, 0x400E0003, 0x40120003, 0x407D0003, 0x40130003,
            0x40140003, 0x402F0003, 0x403A0003, 0x403B0003, 0x4074000B,
            0x4075000B, 0x40150003, 0x407B0102, 0x40180003,
        ] {
            let m = Marker::from_u32(v);
            assert_eq!(m.value(), v, "v={v:#010x}");
        }
    }

    #[test]
    fn marker_end_pairing() {
        assert_eq!(Marker::StartTopFld.end_marker(), Some(Marker::EndFolder));
        assert_eq!(Marker::StartSubFld.end_marker(), Some(Marker::EndFolder));
        assert_eq!(Marker::StartMessage.end_marker(), Some(Marker::EndMessage));
        assert_eq!(Marker::StartFaiMsg.end_marker(), Some(Marker::EndMessage));
        assert_eq!(Marker::StartEmbed.end_marker(), Some(Marker::EndEmbed));
        assert_eq!(Marker::StartRecip.end_marker(), Some(Marker::EndToRecip));
        assert_eq!(Marker::NewAttach.end_marker(), Some(Marker::EndAttach));
        assert_eq!(
            Marker::IncrSyncStateBegin.end_marker(),
            Some(Marker::IncrSyncStateEnd)
        );
        assert_eq!(Marker::EndFolder.end_marker(), None);
        assert_eq!(Marker::IncrSyncChg.end_marker(), None);
    }

    #[test]
    fn tokenizer_balanced_folder_stream() {
        // StartTopFld, EndFolder.
        let mut buf = Vec::new();
        buf.extend_from_slice(&Marker::StartTopFld.value().to_le_bytes());
        buf.extend_from_slice(&Marker::EndFolder.value().to_le_bytes());
        let mut t = Tokenizer::new(&buf);
        assert!(matches!(t.next_event().unwrap(), Some(FxEvent::Marker(Marker::StartTopFld))));
        assert!(matches!(t.next_event().unwrap(), Some(FxEvent::Marker(Marker::EndFolder))));
        assert!(t.next_event().unwrap().is_none());
        assert!(t.open.is_empty());
    }

    #[test]
    fn tokenizer_rejects_unpaired_end() {
        let mut buf = Vec::new();
        buf.extend_from_slice(&Marker::EndMessage.value().to_le_bytes());
        let mut t = Tokenizer::new(&buf);
        assert_eq!(t.next_event().unwrap_err(), DecodeError::InvalidValue);
    }

    #[test]
    fn tokenizer_rejects_mismatched_end() {
        let mut buf = Vec::new();
        buf.extend_from_slice(&Marker::StartMessage.value().to_le_bytes());
        // StartMessage wants EndMessage; send EndEmbed instead.
        buf.extend_from_slice(&Marker::EndEmbed.value().to_le_bytes());
        let mut t = Tokenizer::new(&buf);
        t.next_event().unwrap(); // StartMessage
        assert_eq!(t.next_event().unwrap_err(), DecodeError::InvalidValue);
    }

    #[test]
    fn tokenizer_rejects_open_marker_eof() {
        // StartTopFld then EOF: balance stack still holds an open folder.
        let mut buf = Vec::new();
        buf.extend_from_slice(&Marker::StartTopFld.value().to_le_bytes());
        let mut t = Tokenizer::new(&buf);
        // Consume StartTopFld (push onto open stack), then EOF.
        assert!(matches!(
            t.next_event().unwrap(),
            Some(FxEvent::Marker(Marker::StartTopFld))
        ));
        assert!(t.next_event().unwrap().is_none());
        assert_eq!(
            t.assert_complete().unwrap_err(),
            DecodeError::InvalidValue,
            "truncated stream with an open start marker must fail-closed"
        );
    }

    #[test]
    fn tokenizer_emits_property_for_non_marker() {
        // A 4-byte word that is not a marker: read as a property property-type
        // 0x0003 (Integer32, fixed 4 bytes) with property id 0x0100.
        let tag = 0x0100_0003u32;
        let mut buf = Vec::new();
        buf.extend_from_slice(&tag.to_le_bytes());
        buf.extend_from_slice(&0x42u32.to_le_bytes());
        let mut t = Tokenizer::new(&buf);
        let ev = t.next_event().unwrap().expect("event");
        match ev {
            FxEvent::Property { tag: got, bytes } => {
                assert_eq!(got, tag);
                assert_eq!(bytes, 0x42u32.to_le_bytes());
            }
            _ => panic!("expected Property event"),
        }
    }

    #[test]
    fn ics_builder_finishes_with_end_marker() {
        let mut b = IcsStreamBuilder::new();
        b.push_marker(Marker::IncrSyncChg);
        b.push_marker(Marker::IncrSyncDel);
        let out = b.finish();
        // Last marker is IncrSyncEnd (0x40140003).
        let tail = &out[out.len() - 4..];
        assert_eq!(tail, 0x40140003u32.to_le_bytes());
    }

    proptest::proptest! {
        #[test]
        fn marker_roundtrip(v in 0u32..=0xFFFF_FFFFu32) {
            let m = Marker::from_u32(v);
            // From_u32 is total; only known values stay typed.
            if is_known_marker_value(v) {
                proptest::prop_assert!(!matches!(m, Marker::Unknown(_)));
                proptest::prop_assert_eq!(m.value(), v);
            } else {
                proptest::prop_assert!(matches!(m, Marker::Unknown(x) if x == v));
            }
        }

        #[test]
        fn balanced_top_folder_roundtrip(extra in 0u8..=3u8) {
            let mut b = IcsStreamBuilder::new();
            b.push_marker(Marker::StartTopFld);
            b.push_marker(Marker::EndFolder);
            let out = b.finish();
            let mut t = Tokenizer::new(&out);
            let mut depth = 0i32;
            while let Some(ev) = t.next_event().unwrap() {
                if let FxEvent::Marker(m) = ev {
                    if m.end_marker().is_some() { depth += 1; }
                    else if matches!(m, Marker::EndFolder | Marker::EndMessage | Marker::EndEmbed | Marker::EndToRecip | Marker::EndAttach | Marker::IncrSyncStateEnd) { depth -= 1; }
                }
            }
            // The builder's trailing IncrSyncEnd is a standalone terminator
            // (no matching start), so the net depth is 0.
            let _ = extra;
            proptest::prop_assert_eq!(depth, 0);
            proptest::prop_assert!(t.open.is_empty());
        }

        #[test]
        fn incr_sync_end_is_standalone(out_marker_count in 0u8..=4u8) {
            // A stream that is only IncrSyncEnd markers (no opens) must
            // tokenize without InvalidValue — the terminator is standalone.
            let mut buf = Vec::new();
            for _ in 0..out_marker_count {
                buf.extend_from_slice(&Marker::IncrSyncEnd.value().to_le_bytes());
            }
            let mut t = Tokenizer::new(&buf);
            let mut count = 0;
            while let Some(ev) = t.next_event().unwrap() {
                if matches!(ev, FxEvent::Marker(Marker::IncrSyncEnd)) {
                    count += 1;
                }
            }
            proptest::prop_assert_eq!(count, out_marker_count);
            proptest::prop_assert!(t.open.is_empty());
        }
    }
}
