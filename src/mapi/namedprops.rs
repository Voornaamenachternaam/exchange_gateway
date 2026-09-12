// src/mapi/namedprops.rs
//
// The named-property table backing `RopGetPropertyIdsFromNames` (0x56),
// `RopGetNamesFromPropertyIds` (0x55) and `RopQueryNamedProperties` (0x5F).
//
// MS-OXPROPS divides MAPI properties into two id spaces:
//   * `PidTag*`  — 0x0001..0x7FFF (fixed, well-known, single-byte id).
//   * `PidLid*`  — 0x8000..0x8FFF (named, keyed by a (property-set GUID,
//                    LID/name) pair).
//
// A named property's *property id* is simply its LID: the 0x8000 bit IS part
// of the LID's value (e.g. `PidLidCategories` = 0x8524), so there is no
// separate "0x8000 range assignment" step — the LID *is* the id. This module
// therefore only needs to remember which `(GUID, LID)` pairs are known and
// map them in both directions, mirroring what real Exchange/Outlook retains in
// the on-wire named-property table (MS-OXCROPS §2.2.19 / §2.2.20).
//
// GUIDs are stored in on-wire byte order: Data1/Data2/Data3 little-endian,
// then the final 8 bytes verbatim (exactly the bytes Outlook sends in the
// `Guid` field of a `RopGetPropertyIdsFromNames` request).

/// PS_PUBLIC_STRINGS   `{00020329-0000-0000-C000-000000000046}`
pub const PS_PUBLIC_STRINGS: [u8; 16] =
    [0x29, 0x03, 0x02, 0x00, 0x00, 0x00, 0x00, 0x00, 0xC0, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x46];

/// PSETID_Common       `{00062008-0000-0000-C000-000000000046}`
pub const PSETID_COMMON: [u8; 16] =
    [0x08, 0x20, 0x06, 0x00, 0x00, 0x00, 0x00, 0x00, 0xC0, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x46];

/// PSETID_Address      `{00062004-0000-0000-C000-000000000046}`
pub const PSETID_ADDRESS: [u8; 16] =
    [0x04, 0x20, 0x06, 0x00, 0x00, 0x00, 0x00, 0x00, 0xC0, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x46];

/// PSETID_Appointment  `{00062002-0000-0000-C000-000000000046}`
pub const PSETID_APPOINTMENT: [u8; 16] =
    [0x02, 0x20, 0x06, 0x00, 0x00, 0x00, 0x00, 0x00, 0xC0, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x46];

/// PSETID_Task         `{00062003-0000-0000-C000-000000000046}`
pub const PSETID_TASK: [u8; 16] =
    [0x03, 0x20, 0x06, 0x00, 0x00, 0x00, 0x00, 0x00, 0xC0, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x46];

/// PSETID_Log          `{0006200A-0000-0000-C000-000000000046}`
pub const PSETID_LOG: [u8; 16] =
    [0x0A, 0x20, 0x06, 0x00, 0x00, 0x00, 0x00, 0x00, 0xC0, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x46];

/// PSETID_Note         `{0006200E-0000-0000-C000-000000000046}`
pub const PSETID_NOTE: [u8; 16] =
    [0x0E, 0x20, 0x06, 0x00, 0x00, 0x00, 0x00, 0x00, 0xC0, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x46];

/// PSETID_Meeting      `{6ED8DA90-450B-101B-98DA-00AA003F1305}`
pub const PSETID_MEETING: [u8; 16] =
    [0x90, 0xDA, 0xD8, 0x6E, 0x0B, 0x45, 0x1B, 0x10, 0x98, 0xDA, 0x00, 0xAA, 0x00, 0x3F, 0x13, 0x05];

/// PSETID_Report       `{00062013-0000-0000-C000-000000000046}`
pub const PSETID_REPORT: [u8; 16] =
    [0x13, 0x20, 0x06, 0x00, 0x00, 0x00, 0x00, 0x00, 0xC0, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x46];

/// One entry in the gateways well-known named-property table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NamedPropEntry {
    /// Property-set GUID, on-wire byte order.
    pub guid: [u8; 16],
    /// The property's 16-bit *named-property id* (always has the 0x8000 bit
    /// set). This is the value exchanged on the wire as `PropertyId`.
    pub property_id: u16,
    /// The property's canonical MS-OXPROPS LID. For LIDs already in the
    /// 0x8000..0xFFFF range this equals `property_id`; for LIDs below 0x8000
    /// (e.g. `PidLidMeetingType` = 0x00000026) the named-property id is
    /// `0x8000 | LID`.
    pub lid: u32,
}

/// The well-known named properties the two target clients rely on. The list
/// captures the categories/flag/task-status properties Outlook synthesises on
/// first connect (audit gap #5): without them those properties read as Null
/// and do not round-trip. `guid`, `lid` and `property_id` are the canonical
/// MS-OXPROPS values.
const KNOWN_NAMED_PROPS: &[NamedPropEntry] = &[
    // PSETID_Common — reminders, classification, follow-up flags.
    NamedPropEntry { guid: PSETID_COMMON, property_id: 0x8501, lid: 0x00008501 }, // PidLidReminderDelta
    NamedPropEntry { guid: PSETID_COMMON, property_id: 0x8502, lid: 0x00008502 }, // PidLidReminderTime
    NamedPropEntry { guid: PSETID_COMMON, property_id: 0x8503, lid: 0x00008503 }, // PidLidReminderSet
    NamedPropEntry { guid: PSETID_COMMON, property_id: 0x8530, lid: 0x00008530 }, // PidLidFlagRequest
    NamedPropEntry { guid: PSETID_COMMON, property_id: 0x85B6, lid: 0x000085B6 }, // PidLidClassification
    // PS_PUBLIC_STRINGS — keyword/category strings.
    NamedPropEntry { guid: PS_PUBLIC_STRINGS, property_id: 0x9000, lid: 0x00009000 }, // PidLidCategory (categories keyword)
    // PSETID_Address — name/contact identity.
    NamedPropEntry { guid: PSETID_ADDRESS, property_id: 0x802E, lid: 0x0000802E }, // PidLidYomiCompanyName
    // PSETID_Appointment — calendar identity.
    NamedPropEntry { guid: PSETID_APPOINTMENT, property_id: 0x8205, lid: 0x00008205 }, // PidLidBusyStatus
    NamedPropEntry { guid: PSETID_APPOINTMENT, property_id: 0x8208, lid: 0x00008208 }, // PidLidLocation
    NamedPropEntry { guid: PSETID_APPOINTMENT, property_id: 0x820D, lid: 0x0000820D }, // PidLidAppointmentStartWhole
    NamedPropEntry { guid: PSETID_APPOINTMENT, property_id: 0x820E, lid: 0x0000820E }, // PidLidAppointmentEndWhole
    NamedPropEntry { guid: PSETID_APPOINTMENT, property_id: 0x8238, lid: 0x00008238 }, // PidLidAllAttendeesString
    // PSETID_Task — task status/date round-trip.
    NamedPropEntry { guid: PSETID_TASK, property_id: 0x8101, lid: 0x00008101 }, // PidLidTaskStatus
    NamedPropEntry { guid: PSETID_TASK, property_id: 0x8102, lid: 0x00008102 }, // PidLidPercentComplete
    NamedPropEntry { guid: PSETID_TASK, property_id: 0x8104, lid: 0x00008104 }, // PidLidTaskStartDate
    NamedPropEntry { guid: PSETID_TASK, property_id: 0x8105, lid: 0x00008105 }, // PidLidTaskDueDate
    NamedPropEntry { guid: PSETID_TASK, property_id: 0x811C, lid: 0x0000811C }, // PidLidTaskComplete
    // PSETID_Meeting — meeting-request type (LID below 0x8000: named id = 0x8000|LID).
    NamedPropEntry { guid: PSETID_MEETING, property_id: 0x8026, lid: 0x00000026 }, // PidLidMeetingType
];

/// Resolve a `(guid, kind, lid/name)` request triple to a property id.
/// Returns `Some(id)` for a known property and `None` (encoded as id `0` by
/// the caller, per MS-OXCROPS §2.2.20.2) for an unknown one.
pub fn property_id_for_name(guid: &[u8; 16], name: &NamedPropertyName) -> Option<u16> {
    match name {
        NamedPropertyName::Lid(lid) => {
            KNOWN_NAMED_PROPS
                .iter()
                .find(|e| e.guid == *guid && e.lid == *lid)
                .map(|e| e.property_id)
        }
        // String-named properties are not in the well-known table; returning
        // None maps to the id-0 "not found" sentinel (not an error).
        NamedPropertyName::String(_) => None,
    }
}

/// Resolve a property id back to its `(guid, lid)` pair. Returns `None` for an
/// id outside the known named set.
pub fn name_for_property_id(property_id: u16) -> Option<NamedPropEntry> {
    KNOWN_NAMED_PROPS
        .iter()
        .find(|e| e.property_id == property_id)
        .copied()
}

/// The full list of known named-property ids, for `RopQueryNamedProperties`.
pub fn all_named_property_ids() -> Vec<u16> {
    KNOWN_NAMED_PROPS
        .iter()
        .map(|e| e.property_id)
        .collect()
}

/// Re-export the name kinds used by the ROP codecs so callers need no
/// additional import path.
pub use crate::mapi::rops::NamedPropertyName;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn categories_lid_round_trips() {
        let lid = NamedPropertyName::Lid(0x00009000);
        let id = property_id_for_name(&PS_PUBLIC_STRINGS, &lid).unwrap();
        assert_eq!(id, 0x9000);
        let back = name_for_property_id(id).unwrap();
        assert_eq!(back.property_id, 0x9000);
        assert_eq!(back.guid, PS_PUBLIC_STRINGS);
    }

    #[test]
    fn meeting_type_lid_below_named_range_maps_to_0x8000_or_lid() {
        let lid = NamedPropertyName::Lid(0x00000026);
        let id = property_id_for_name(&PSETID_MEETING, &lid).unwrap();
        assert_eq!(id, 0x8026);
        let back = name_for_property_id(id).unwrap();
        assert_eq!(back.guid, PSETID_MEETING);
        assert_eq!(back.lid, 0x00000026);
    }

    #[test]
    fn common_guid_is_little_endian_wire_bytes() {
        // PSETID_Common {00062008-0000-0000-C000-000000000046}: Data1=0x00062008
        // little-endian => 08 20 06 00.
        assert_eq!(
            &PSETID_COMMON[..4],
            &[0x08, 0x20, 0x06, 0x00],
            "PSETID_COMMON Data1 must be little-endian"
        );
        // Independent wire-byte check for a {000620xx} set: PSETID_Task
        // {00062003-...} => Data1=0x00062003 => 03 20 06 00.
        assert_eq!(&PSETID_TASK[..4], &[0x03, 0x20, 0x06, 0x00]);
    }

    #[test]
    fn unknown_string_name_is_not_found() {
        let name = NamedPropertyName::String("com.example.nothere".to_string());
        assert!(property_id_for_name(&PS_PUBLIC_STRINGS, &name).is_none());
    }

    #[test]
    fn unknown_lid_is_not_found() {
        let lid = NamedPropertyName::Lid(0x9999);
        assert!(property_id_for_name(&PSETID_COMMON, &lid).is_none());
    }

    #[test]
    fn all_ids_are_in_named_range() {
        for id in all_named_property_ids() {
            assert!(id & 0x8000 != 0, "{id:#x} must be in the named range");
        }
    }
}
