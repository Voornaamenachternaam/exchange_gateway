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
    [0x08, 0x00, 0x06, 0x20, 0x00, 0x00, 0x00, 0x00, 0xC0, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x46];

/// PSETID_Address      `{00062004-0000-0000-C000-000000000046}`
pub const PSETID_ADDRESS: [u8; 16] =
    [0x04, 0x00, 0x06, 0x20, 0x00, 0x00, 0x00, 0x00, 0xC0, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x46];

/// PSETID_Appointment  `{00062002-0000-0000-C000-000000000046}`
pub const PSETID_APPOINTMENT: [u8; 16] =
    [0x02, 0x00, 0x06, 0x20, 0x00, 0x00, 0x00, 0x00, 0xC0, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x46];

/// PSETID_Task         `{00062003-0000-0000-C000-000000000046}`
pub const PSETID_TASK: [u8; 16] =
    [0x03, 0x00, 0x06, 0x20, 0x00, 0x00, 0x00, 0x00, 0xC0, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x46];

/// PSETID_Log          `{0006200A-0000-0000-C000-000000000046}`
pub const PSETID_LOG: [u8; 16] =
    [0x0A, 0x00, 0x06, 0x20, 0x00, 0x00, 0x00, 0x00, 0xC0, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x46];

/// PSETID_Note         `{0006200E-0000-0000-C000-000000000046}`
pub const PSETID_NOTE: [u8; 16] =
    [0x0E, 0x00, 0x06, 0x20, 0x00, 0x00, 0x00, 0x00, 0xC0, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x46];

/// PSETID_Meeting      `{6ED8DA90-450B-101B-98DA-00AA003F1305}`
pub const PSETID_MEETING: [u8; 16] =
    [0x90, 0xDA, 0xD8, 0x6E, 0x0B, 0x45, 0x1B, 0x10, 0x98, 0xDA, 0x00, 0xAA, 0x00, 0x3F, 0x13, 0x05];

/// PSETID_Report       `{00062013-0000-0000-C000-000000000046}`
pub const PSETID_REPORT: [u8; 16] =
    [0x13, 0x00, 0x06, 0x20, 0x00, 0x00, 0x00, 0x00, 0xC0, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x46];

/// One entry in the gateways well-known named-property table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NamedPropEntry {
    /// Property-set GUID, on-wire byte order.
    pub guid: [u8; 16],
    /// The property id == the LID (already in the 0x8000..0x8FFF range).
    pub property_id: u16,
}

/// The well-known named properties the two target clients rely on. The list
/// captures the categories/flag/task-status properties Outlook synthesises on
/// first connect (audit gap #5): without them those properties read as Null
/// and do not round-trip. The entries are expressed as `(guid, lid)` pairs
/// where `lid` is the canonical MS-OXPROPS PidLid value (== the property id).
const KNOWN_NAMED_PROPS: &[NamedPropEntry] = &[
    // PSETID_Common — categories, flags, reminders, message identity.
    NamedPropEntry { guid: PSETID_COMMON, property_id: 0x8501 }, // PidLidReminderDelta
    NamedPropEntry { guid: PSETID_COMMON, property_id: 0x8502 }, // PidLidReminderTime
    NamedPropEntry { guid: PSETID_COMMON, property_id: 0x8503 }, // PidLidReminderSet
    NamedPropEntry { guid: PSETID_COMMON, property_id: 0x8524 }, // PidLidCategories
    NamedPropEntry { guid: PSETID_COMMON, property_id: 0x8570 }, // PidLidClassification
    NamedPropEntry { guid: PSETID_COMMON, property_id: 0x8581 }, // PidLidFlagRequest
    NamedPropEntry { guid: PSETID_COMMON, property_id: 0x8208 }, // PidLidLocation
    NamedPropEntry { guid: PSETID_COMMON, property_id: 0x811C }, // PidLidYomiCompanyName
    NamedPropEntry { guid: PSETID_COMMON, property_id: 0x8005 }, // PidLidMeetingType
    // PSETID_XExtendend-style task/flag status LIDs.
    NamedPropEntry { guid: PSETID_TASK, property_id: 0x8101 }, // PidLidTaskStatus
    NamedPropEntry { guid: PSETID_TASK, property_id: 0x8102 }, // PidLidPercentComplete
    NamedPropEntry { guid: PSETID_TASK, property_id: 0x8104 }, // PidLidTaskStartDate
    NamedPropEntry { guid: PSETID_TASK, property_id: 0x8105 }, // PidLidTaskDueDate
    NamedPropEntry { guid: PSETID_TASK, property_id: 0x811A }, // PidLidTaskComplete
    NamedPropEntry { guid: PSETID_APPOINTMENT, property_id: 0x8205 }, // PidLidAppointmentStartWhole
    NamedPropEntry { guid: PSETID_APPOINTMENT, property_id: 0x8206 }, // PidLidAppointmentEndWhole
    NamedPropEntry { guid: PSETID_APPOINTMENT, property_id: 0x8216 }, // PidLidAllAttendeesString
    NamedPropEntry { guid: PSETID_APPOINTMENT, property_id: 0x8214 }, // PidLidLocation (appt)
];

/// Resolve a `(guid, lid → name)` request triple to a property id. Returns
/// `Some(id)` for a known property and `None` (encoded as id `0` by the
/// caller, per MS-OXCROPS §2.2.20.2) for an unknown one.
pub fn property_id_for_name(guid: &[u8; 16], name: &NamedPropertyName) -> Option<u16> {
    match name {
        NamedPropertyName::Lid(lid) => {
            let lid16 = u16::try_from(*lid).ok()?;
            KNOWN_NAMED_PROPS
                .iter()
                .find(|e| e.guid == *guid && e.property_id == lid16)
                .map(|e| e.property_id)
        }
        // String-named properties: map a handful of public-string names to the
        // PS_PUBLIC_STRINGS set. Unsupported today (returns None → id 0), which
        // is the documented "not found" sentinel and not an error.
        NamedPropertyName::String(_) => None,
    }
}

/// Resolve a property id back to its `(guid, lid)` pair. Returns `None` for an
/// id outside the known named set (the caller then emits no `names` entry).
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
        let lid = NamedPropertyName::Lid(0x8524);
        let id = property_id_for_name(&PSETID_COMMON, &lid).unwrap();
        assert_eq!(id, 0x8524);
        let back = name_for_property_id(id).unwrap();
        assert_eq!(back.property_id, 0x8524);
        assert_eq!(back.guid, PSETID_COMMON);
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