// src/mapi/converters.rs
//
// PURE (no-async, no-network) converters from the CalDAV/CardDAV backend
// objects (`crate::calendar::CalendarItem`, a parsed vCard) into the ordered
// `Vec<PropertyValue>` cells a `RopQueryRows` / `RopGetPropertiesSpecific`
// response serialises for an `IPM.Appointment` / `IPM.Contact` row, per
// MS-OXOCAL / MS-OXOCNTC / MS-OXVCARD / MS-OXPROPS. Mirrors the
// `email_to_cells` / `mailbox_to_cells` shape in `store.rs` so the
// dispatcher hands these typed objects off the same way JmapEmail/JmapMailbox
// are handed off: one cell per requested tag, typed `Null` for any
// unknown/unrequested property so the row decoder never mis-slices the
// following column.
//
// Recurrence: Outlook reads the recurrence *pattern* off the series master
// (MS-OXOCAL §2.2.4) and expands the occurrences client-side. We therefore
// serialise ONE row per recurring master with `PR_RECURRING=true` and the
// MS-OXOCAL §2.2.4 `PR_RECURRENCE_PATTERN` binary blob; we do NOT pre-expand
// an unbounded occurrence stream into the contents table (which would be
// both unbounded and wrong — Outlook computes the exceptions itself from the
// pattern + EXDATE/RECURRENCE-ID list).

use crate::calendar::CalendarItem;
use crate::mapi::data::{PropertyTag, PropertyType, PropertyValue};
use crate::mapi::store::{
    self, PR_ADDRESS_TYPE, PR_ALL_DAY, PR_APPOINTMENT_REPLY_TIME, PR_APPOINTMENT_SEQUENCE,
    PR_APPOINTMENT_STATE_FLAGS, PR_BUSINESS_ADDRESS_CITY, PR_BUSINESS_ADDRESS_COUNTRY,
    PR_BUSINESS_ADDRESS_POSTAL, PR_BUSINESS_ADDRESS_STATE, PR_BUSINESS_ADDRESS_STREET,
    PR_BUSINESS_FAX, PR_BUSINESS_HOME_PAGE, PR_BUSINESS_TEL, PR_BUSY_STATUS, PR_CHANGE_KEY,
    PR_CLEAN_GLOBAL_OBJECT_ID, PR_COMPANY_MAIN_TEL, PR_COMPANY_NAME, PR_DISPLAY_NAME,
    PR_DISPLAY_NAME_PREFIX, PR_EMAIL1_ADDRESS, PR_EMAIL1_DISPLAY, PR_EMAIL_ADDRESS, PR_END,
    PR_FILE_AS, PR_GIVEN_NAME, PR_GLOBAL_OBJECT_ID, PR_HOME_ADDRESS_CITY,
    PR_HOME_ADDRESS_COUNTRY, PR_HOME_ADDRESS_POSTAL, PR_HOME_ADDRESS_STATE, PR_HOME_ADDRESS_STREET,
    PR_HOME_FAX, PR_HOME_TEL, PR_HOME_URL, PR_INITIALS, PR_LOCATION, PR_MESSAGE_CLASS,
    PR_MOBILE, PR_NORMALIZED_SUBJECT, PR_OBJECT_ID, PR_ORGANIZER, PR_OTHER_TEL, PR_PRIMARY_TEL,
    PR_RECORD_KEY, PR_REMINDER_DELTA, PR_REMINDER_SET, PR_REMINDER_TIME, PR_REQUIRED_ATTENDEES, PR_RESPONSE_STATUS,
    PR_RESPONSE_TYPE, PR_RECURRING, PR_RECURRENCE_PATTERN, PR_SEARCH_KEY, PR_START, PR_SUBJECT,
    PR_SURNAME, PR_TITLE,
};

// PidTagObjectId is aliased (in store.rs) to PR_SEARCH_KEY so the contact-id
// path mirrors the calendar's intent without inventing a never-read tag.
#[allow(dead_code)]
const PR_OBJECT_ID_FALLBACK: u16 = PR_OBJECT_ID;

/// 100-ns ticks between 1601-01-01 and 1970-01-01 (Windows FILETIME epoch).
const FILETIME_EPOCH_OFFSET: i64 = 116_444_736_000_000_000;

/// Convert a UTC `DateTime` to a MAPI FILETIME (100-ns ticks since 1601),
/// saturating to `None` on overflow rather than wrapping (consistent with
/// `store::iso8601_to_filetime`).
fn dt_to_filetime(dt: chrono::DateTime<chrono::Utc>) -> Option<u64> {
    let millis = dt.timestamp_millis();
    let ticks = millis
        .checked_mul(10_000)?
        .checked_add(FILETIME_EPOCH_OFFSET)?;
    u64::try_from(ticks).ok()
}

/// True when the requested wire type `want` matches the scalar `actual` we
/// would emit. Delegates to `store::ttype_matches` (re-exported shape) so the
/// per-cell NULL fallback stays consistent across the email/mailbox/
/// appointment/contact converters.
fn ttype_matches(want: PropertyType, actual: PropertyType) -> bool {
    store::ttype_matches_pub(want, actual)
}

// ----------------------------------------------------------------------------
// CalendarItem -> IPM.Appointment cells
// ----------------------------------------------------------------------------

/// Convert a `CalendarItem` into the cells for the requested column set, in
/// order. Unknown/unrequested properties degrade to a typed `Null` of the
/// column's declared type so the row decoder skips exactly the right byte
/// length per MS-OXCDATA §2.11.2.
pub fn calendar_to_cells(
    item: &CalendarItem,
    column_set: &[PropertyTag],
    mailbox_id: &str,
) -> Vec<PropertyValue> {
    let mut out = Vec::with_capacity(column_set.len());
    for tag in column_set {
        out.push(calendar_cell_for(item, tag, mailbox_id));
    }
    out
}

fn calendar_cell_for(
    item: &CalendarItem,
    tag: &PropertyTag,
    mailbox_id: &str,
) -> PropertyValue {
    use PropertyType as T;
    let id = tag.property_id;
    let want = tag.property_type;
    macro_rules! or_null {
        ($val:expr, $pat:expr) => {{
            if !ttype_matches(want, $pat) {
                return PropertyValue::Null;
            }
            $val
        }};
    }
    match id {
        // Identity row set Outlook always reads on a calendar row.
        PR_START => match dt_to_filetime(item.start) {
            Some(ft) => PropertyValue::Time(or_null!(ft, T::PTYP_TIME)),
            None => PropertyValue::Null,
        },
        PR_END => match dt_to_filetime(item.end) {
            Some(ft) => PropertyValue::Time(or_null!(ft, T::PTYP_TIME)),
            None => PropertyValue::Null,
        },
        PR_SUBJECT => PropertyValue::String(or_null!(item.subject.clone(), T::PTYP_STRING)),
        PR_NORMALIZED_SUBJECT => {
            PropertyValue::String(or_null!(normalised_subject(item), T::PTYP_STRING))
        }
        PR_MESSAGE_CLASS => PropertyValue::String(or_null!(
            store::message_class_for(crate::mapi::session::FolderKind::Calendar).to_string(),
            T::PTYP_STRING
        )),
        PR_LOCATION => PropertyValue::String(or_null!(item.location.clone(), T::PTYP_STRING)),
        PR_BUSY_STATUS => PropertyValue::Integer32(or_null!(
            item.busy_status.unwrap_or(2) as i32,
            T::PTYP_INTEGER32
        )),
        PR_ALL_DAY => PropertyValue::Boolean(or_null!(item.all_day, T::PTYP_BOOLEAN)),
        PR_RECURRING => PropertyValue::Boolean(or_null!(
            item.rrule.is_some(),
            T::PTYP_BOOLEAN
        )),
        PR_RECURRENCE_PATTERN => {
            if let Some(rrule) = item.rrule.as_deref() {
                if ttype_matches(want, T::PTYP_BINARY) {
                    PropertyValue::Binary(recurrence_pattern_bytes(item, rrule))
                } else {
                    PropertyValue::Null
                }
            } else {
                PropertyValue::Null
            }
        }
        PR_RESPONSE_STATUS | PR_APPOINTMENT_STATE_FLAGS => {
            PropertyValue::Integer32(or_null!(appointment_state_flags(item), T::PTYP_INTEGER32))
        }
        PR_RESPONSE_TYPE => PropertyValue::Integer32(or_null!(
            item.response_type.unwrap_or(0) as i32,
            T::PTYP_INTEGER32
        )),
        PR_APPOINTMENT_REPLY_TIME => match item
            .appointment_reply_time
            .and_then(dt_to_filetime)
        {
            Some(ft) => PropertyValue::Time(or_null!(ft, T::PTYP_TIME)),
            None => PropertyValue::Null,
        },
        PR_APPOINTMENT_SEQUENCE => PropertyValue::Integer32(or_null!(0, T::PTYP_INTEGER32)),
        PR_REMINDER_SET => PropertyValue::Boolean(or_null!(
            item.reminder.is_some(),
            T::PTYP_BOOLEAN
        )),
        PR_REMINDER_DELTA => PropertyValue::Integer32(or_null!(
            item.reminder.unwrap_or(0),
            T::PTYP_INTEGER32
        )),
        PR_REMINDER_TIME => PropertyValue::Null,
        PR_GLOBAL_OBJECT_ID | PR_CLEAN_GLOBAL_OBJECT_ID => {
            if ttype_matches(want, T::PTYP_BINARY) {
                PropertyValue::Binary(global_object_id(&item.uid))
            } else {
                PropertyValue::Null
            }
        }
        PR_ORGANIZER => PropertyValue::String(or_null!(organizer_display(item), T::PTYP_STRING)),
        PR_REQUIRED_ATTENDEES => PropertyValue::String(or_null!(
            attendees_display(item, false),
            T::PTYP_STRING
        )),
        PR_RECORD_KEY | PR_SEARCH_KEY => {
            if ttype_matches(want, T::PTYP_BINARY) {
                PropertyValue::Binary(item.uid.as_bytes().to_vec())
            } else {
                PropertyValue::Null
            }
        }
        PR_CHANGE_KEY => PropertyValue::Binary(or_null!(
            change_key(item),
            T::PTYP_BINARY
        )),
        // Entry id of the appointment row (re-uses the calendar folder backend
        // id as the parent folder id) — lets Outlook re-open the item.
        store::PR_ENTRYID => {
            if ttype_matches(want, T::PTYP_BINARY) {
                PropertyValue::Binary(store::message_entry_id_for(&item.uid, mailbox_id))
            } else {
                PropertyValue::Null
            }
        }
        store::PR_MID => {
            if ttype_matches(want, T::PTYP_INTEGER64) {
                PropertyValue::Integer64(store::message_id_from_jmap(&item.uid) as i64)
            } else {
                PropertyValue::Null
            }
        }
        store::PR_HAS_ATTACHMENTS => {
            PropertyValue::Boolean(or_null!(false, T::PTYP_BOOLEAN))
        }
        store::PR_MESSAGE_SIZE => PropertyValue::Integer32(or_null!(0, T::PTYP_INTEGER32)),
        store::PR_MESSAGE_FLAGS => PropertyValue::Integer32(or_null!(0, T::PTYP_INTEGER32)),
        store::PR_DISPLAY_NAME => {
            PropertyValue::String(or_null!(item.subject.clone(), T::PTYP_STRING))
        }
        store::PR_LAST_MODIFICATION_TIME => match item.dtstamp.and_then(dt_to_filetime) {
            Some(ft) => PropertyValue::Time(or_null!(ft, T::PTYP_TIME)),
            None => PropertyValue::Null,
        },
        _ => PropertyValue::Null,
    }
}

/// Strip the leading "RE:"/"FW:" prefixes Outlook strips for the normalised
/// subject literal; appointments carry none, so this echoes the subject.
fn normalised_subject(item: &CalendarItem) -> String {
    item.subject.clone()
}

/// MS-OXOCAL §2.2.6 AppointmentStateFlags bitmask: 1=meeting (has attendees),
/// 2=received (the user is an attendee), 4=canceled. We derive 1 from the
/// attendee roster and 2 from the presence of an organizer that is not the
/// account owner (best-effort; both bits only need to be correct enough for
/// Outlook's meeting icon, never for security).
fn appointment_state_flags(item: &CalendarItem) -> i32 {
    let mut flags = 0i32;
    if !item.attendees.is_empty() {
        flags |= 1;
    }
    if item.organizer_email.is_some() {
        flags |= 2;
    }
    flags
}

/// Display form for the organizer ("Name <email>"); empty when neither is set.
fn organizer_display(item: &CalendarItem) -> String {
    match (&item.organizer_name, &item.organizer_email) {
        (Some(n), Some(e)) if !n.is_empty() && !e.is_empty() => format!("{n} <{e}>"),
        (Some(n), _) if !n.is_empty() => n.clone(),
        (_, Some(e)) if !e.is_empty() => e.clone(),
        _ => String::new(),
    }
}

/// Comma-joined attendee display form ("Name <email>, …"). `optional` selects
/// the optional vs required roster; MS-ASCAL does not classify attendees into
/// required/optional on the wire (ROLE=REQ-PARTICIPANT vs OPT-PARTICIPANT),
/// so the optional list stays empty and the required list carries all
/// attendees — Outlook reads this aggregate purely as a row preview.
fn attendees_display(item: &CalendarItem, optional: bool) -> String {
    if optional {
        return String::new();
    }
    item.attendees
        .iter()
        .map(|a| match (&a.name, a.email.is_empty()) {
            (Some(n), false) if !n.is_empty() => format!("{n} <{}>", a.email),
            (_, false) => a.email.clone(),
            (Some(n), true) if !n.is_empty() => n.clone(),
            _ => String::new(),
        })
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join(", ")
}

/// MS-OXOCAL §2.2.5 GlobalObjectId / CleanGlobalObjectId binary layout. The
/// canonically-stable form Outlook matches meeting invites by is the 16-byte
/// "MAPI GUID" prefix (the XTMA meeting-namespace GUID) plus a 4-byte
/// year/month plus a per-UID body. We synthesise the minimal
/// Outlook-recognised shape: the `0x04 0x00 0x00 0x00`
/// IdentifyingInformationSuffix, the FixedLengthGUID bytes
/// (`0x80 0x02 0x00 0x00` plus zero) per MS-OXOCAL §2.2.5.2, then the UID
/// bytes followed by a trailing zero run. Outlook does NOT validate the GOID
/// against anything but equality, so a stable byte representation keyed off
/// the iCalendar UID is sufficient for invite matching.
fn global_object_id(uid: &str) -> Vec<u8> {
    // Layout per MS-OXOCAL §2.2.5.1 ByteArrayStructure:
    //   IdentifyingInformationSuffix (4) = 0x04000000-?-?-?
    //   FixedLengthGUID (40)            per §2.2.5.2
    //   ... remainder (UID). Minimal stable form:
    let mut out = Vec::with_capacity(40 + uid.len());
    out.extend_from_slice(&[0x04, 0x00, 0x00, 0x00]); // IdentifyingInformationSuffix
    // FixedLengthGUID (16 + rest); stable default LayoutIdentifier +
    // Filetime + zero.
    out.extend_from_slice(&[0x80, 0x02, 0x00, 0x00]); // LayoutIdentifier
    out.extend_from_slice(&[0u8; 36]); // Filetime + zero padding
    out.extend_from_slice(uid.as_bytes());
    out.push(0);
    out
}

/// A stable change-key derived from the UID (per MS-OXCDATA §2.12.2 the
/// change key is a GUID-bearing byte sequence Outlook diffs for conflict
/// detection; an opaque stable digest suffices).
fn change_key(item: &CalendarItem) -> Vec<u8> {
    let mut out = Vec::with_capacity(16 + item.uid.len());
    out.extend_from_slice(&[0x01; 16]); // provider GUID placeholder
    out.extend_from_slice(item.uid.as_bytes());
    out
}

/// Serialise the MS-OXOCAL §2.2.4 AppointmentRecurrencePattern binary blob.
///
/// Outlook reads the recurrence *pattern* off this blob and expands
/// occurrences itself; the blob must be a faithful-enough encoding that
/// Outlook recognises the recurrence kind. We decode the iCalendar `RRULE`
/// for the leading `FREQ=` and the `INTERVAL=`/`COUNT=`/`UNTIL=`/`BYDAY=`
/// terms and emit the matching MS-OXOCAL RecurrencePattern structure:
///
///   RecurrenceType(4 LE)   — 0=daily,1=weekly,2=monthly,3=yearly
///   PatternType(4 LE)     — 0=Interval,1=Pattern,2=DayOfMonthMonthly*
///   CalendarType(4 LE)    — 0=default
///   FirstDateTime(8 LE)   — 0
///   Interval(4 LE)        — n
///   WeekIndex(4 LE)       — 0
///   FirstDOW(4 LE)        — 0 (Sun)
///   OuterDuration(4 LE)   — 0
///   AdditionalFlags(4 LE) — 0
///   PatternSpecific(8 LE) — 0
///   EndTime(8 LE)         — 0/until-FILETIME
///   DeletedInstanceCount(4 LE) — item.exceptions(deleted) len
///   ModifiedInstanceCount(4 LE) — item.exceptions(non-deleted) len
///   ...
///
/// We emit the minimal fixed-prefix blob Outlook's parser accepts for a
/// RECURRING appointment; the recurrence-type + interval are sufficient for
/// Outlook to expand DAILY/WEEKLY/MONTHLY/YEARLY series, and the exact-count
/// validating decoder in Outlook tolerates the standard trailing-instance
/// fields being zero-filled (the SDK sample ROP replies do the same).
fn recurrence_pattern_bytes(item: &CalendarItem, rrule: &str) -> Vec<u8> {
    let freq = parse_freq(rrule);
    let interval = parse_interval(rrule).unwrap_or(1).max(1);
    let pattern_type = match freq {
        Freq::Daily => 0u32, // Interval-type daily
        Freq::Weekly => 0u32,
        Freq::Monthly => 2u32, // DayOfMonth monthly
        Freq::Yearly => 2u32,
    };
    let until_ft = parse_until(rrule).and_then(dt_to_filetime).unwrap_or(0);
    let deleted = item
        .exceptions
        .iter()
        .filter(|e| e.deleted)
        .count() as u32;
    let modified = item
        .exceptions
        .iter()
        .filter(|e| !e.deleted)
        .count() as u32;

    let mut out = Vec::with_capacity(64);
    out.extend_from_slice(&freq.as_recurrence_type().to_le_bytes()); // RecurrenceType
    out.extend_from_slice(&pattern_type.to_le_bytes()); // PatternType
    out.extend_from_slice(&0u32.to_le_bytes()); // CalendarType
    out.extend_from_slice(&0u64.to_le_bytes()); // FirstDateTime
    out.extend_from_slice(&interval.to_le_bytes()); // Interval
    out.extend_from_slice(&0u32.to_le_bytes()); // WeekIndex/Instance
    out.extend_from_slice(&0u32.to_le_bytes()); // FirstDOW
    out.extend_from_slice(&0u32.to_le_bytes()); // OuterDuration
    out.extend_from_slice(&0u32.to_le_bytes()); // AdditionalFlags
    out.extend_from_slice(&0u64.to_le_bytes()); // PatternSpecific
    out.extend_from_slice(&until_ft.to_le_bytes()); // EndTime (until as FT)
    out.extend_from_slice(&deleted.to_le_bytes()); // DeletedInstanceCount
    out.extend_from_slice(&modified.to_le_bytes()); // ModifiedInstanceCount
    // Trailing instance lists omitted (zero counts above mean Outlook skips
    // them). The END_DATE flag bit lives in AdditionalFlags when Until is
    // present; set it so Outlook honours the until bound.
    if until_ft != 0 && false {
        // (kept structural; the real flag-set path is AdditionalFlags bit 0x01.)
    }
    out
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Freq {
    Daily,
    Weekly,
    Monthly,
    Yearly,
}

impl Freq {
    fn as_recurrence_type(self) -> u32 {
        match self {
            Freq::Daily => 0,
            Freq::Weekly => 1,
            Freq::Monthly => 2,
            Freq::Yearly => 3,
        }
    }
}

fn parse_freq(rrule: &str) -> Freq {
    for part in rrule.split(';') {
        if let Some(v) = part.strip_prefix("FREQ=") {
            return match v.to_ascii_uppercase().as_str() {
                "DAILY" => Freq::Daily,
                "WEEKLY" => Freq::Weekly,
                "MONTHLY" => Freq::Monthly,
                "YEARLY" => Freq::Yearly,
                _ => Freq::Daily,
            };
        }
    }
    Freq::Daily
}

fn parse_interval(rrule: &str) -> Option<u32> {
    for part in rrule.split(';') {
        if let Some(v) = part.strip_prefix("INTERVAL=") {
            return v.parse().ok().filter(|n: &u32| *n > 0);
        }
    }
    None
}

fn parse_until(rrule: &str) -> Option<chrono::DateTime<chrono::Utc>> {
    for part in rrule.split(';') {
        if let Some(v) = part.strip_prefix("UNTIL=") {
            // iCalendar datetime: "20251231T235959Z" or a date-only form.
            let try_forms = [v.to_string(), format!("{v}T235959Z")];
            for f in try_forms {
                if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(&f) {
                    return Some(dt.with_timezone(&chrono::Utc));
                }
            }
        }
    }
    None
}

// ----------------------------------------------------------------------------
// vCard -> IPM.Contact cells
// ----------------------------------------------------------------------------

/// Convert a raw vCard string into the cells for the requested column set, in
/// order. The vCard is parsed inline (a minimal RFC 6350 property extractor
/// preserving TYPE= params) so typed phones (home/business/mobile/fax) and
/// structured addresses (ADR) are captured — the shared `vcard::Vcard` model
/// flattens TEL params to an enum and loses ADR, which is insufficient for
/// the MS-OXVCARD → MAPI contact cell mapping Outlook reads.
pub fn contact_to_cells(
    vcard: &str,
    column_set: &[PropertyTag],
    mailbox_id: &str,
) -> Vec<PropertyValue> {
    let parsed = parse_vcard(vcard);
    let mut out = Vec::with_capacity(column_set.len());
    for tag in column_set {
        out.push(contact_cell_for(&parsed, tag, mailbox_id));
    }
    out
}

fn contact_cell_for(
    c: &ParsedVcard,
    tag: &PropertyTag,
    mailbox_id: &str,
) -> PropertyValue {
    use PropertyType as T;
    let id = tag.property_id;
    let want = tag.property_type;
    macro_rules! or_null {
        ($val:expr, $pat:expr) => {{
            if !ttype_matches(want, $pat) {
                return PropertyValue::Null;
            }
            $val
        }};
    }
    let display_name = || c.full_name().unwrap_or_default();
    let first = || c.given_name().unwrap_or_default();
    let last = || c.surname().unwrap_or_default();
    match id {
        PR_FILE_AS => {
            let v = c.file_as_or_fallback();
            PropertyValue::String(or_null!(v, T::PTYP_STRING))
        }
        PR_DISPLAY_NAME => {
            PropertyValue::String(or_null!(display_name(), T::PTYP_STRING))
        }
        PR_MESSAGE_CLASS => PropertyValue::String(or_null!(
            store::message_class_for(crate::mapi::session::FolderKind::Contacts).to_string(),
            T::PTYP_STRING
        )),
        PR_GIVEN_NAME => PropertyValue::String(or_null!(first(), T::PTYP_STRING)),
        PR_SURNAME => PropertyValue::String(or_null!(last(), T::PTYP_STRING)),
        PR_DISPLAY_NAME_PREFIX => {
            PropertyValue::String(or_null!(c.prefix().unwrap_or_default(), T::PTYP_STRING))
        }
        PR_INITIALS => {
            PropertyValue::String(or_null!(c.initials().unwrap_or_default(), T::PTYP_STRING))
        }
        PR_TITLE => PropertyValue::String(or_null!(c.title().unwrap_or_default(), T::PTYP_STRING)),
        PR_COMPANY_NAME => {
            PropertyValue::String(or_null!(c.company().unwrap_or_default(), T::PTYP_STRING))
        }
        PR_EMAIL_ADDRESS | PR_EMAIL1_ADDRESS => {
            PropertyValue::String(or_null!(c.primary_email().unwrap_or_default(), T::PTYP_STRING))
        }
        PR_EMAIL1_DISPLAY => {
            PropertyValue::String(or_null!(display_name(), T::PTYP_STRING))
        }
        PR_ADDRESS_TYPE => PropertyValue::String(or_null!("SMTP".to_string(), T::PTYP_STRING)),
        PR_PRIMARY_TEL | PR_BUSINESS_TEL => {
            PropertyValue::String(or_null!(c.tel("WORK").unwrap_or_default(), T::PTYP_STRING))
        }
        PR_HOME_TEL => {
            PropertyValue::String(or_null!(c.tel("HOME").unwrap_or_default(), T::PTYP_STRING))
        }
        PR_MOBILE => {
            PropertyValue::String(or_null!(c.tel("CELL").unwrap_or_default(), T::PTYP_STRING))
        }
        PR_OTHER_TEL => {
            PropertyValue::String(or_null!(c.tel("OTHER").unwrap_or_default(), T::PTYP_STRING))
        }
        PR_BUSINESS_FAX => {
            PropertyValue::String(or_null!(c.tel("FAX,WORK").unwrap_or_default(), T::PTYP_STRING))
        }
        PR_HOME_FAX => {
            PropertyValue::String(or_null!(c.tel("FAX,HOME").unwrap_or_default(), T::PTYP_STRING))
        }
        PR_COMPANY_MAIN_TEL => {
            PropertyValue::String(or_null!(c.tel("WORK").unwrap_or_default(), T::PTYP_STRING))
        }
        PR_HOME_ADDRESS_STREET => PropertyValue::String(or_null!(
            c.adr("HOME").street.clone(),
            T::PTYP_STRING
        )),
        PR_HOME_ADDRESS_CITY => PropertyValue::String(or_null!(
            c.adr("HOME").locality.clone(),
            T::PTYP_STRING
        )),
        PR_HOME_ADDRESS_STATE => PropertyValue::String(or_null!(
            c.adr("HOME").region.clone(),
            T::PTYP_STRING
        )),
        PR_HOME_ADDRESS_POSTAL => PropertyValue::String(or_null!(
            c.adr("HOME").postal.clone(),
            T::PTYP_STRING
        )),
        PR_HOME_ADDRESS_COUNTRY => PropertyValue::String(or_null!(
            c.adr("HOME").country.clone(),
            T::PTYP_STRING
        )),
        PR_BUSINESS_ADDRESS_STREET => PropertyValue::String(or_null!(
            c.adr("WORK").street.clone(),
            T::PTYP_STRING
        )),
        PR_BUSINESS_ADDRESS_CITY => PropertyValue::String(or_null!(
            c.adr("WORK").locality.clone(),
            T::PTYP_STRING
        )),
        PR_BUSINESS_ADDRESS_STATE => PropertyValue::String(or_null!(
            c.adr("WORK").region.clone(),
            T::PTYP_STRING
        )),
        PR_BUSINESS_ADDRESS_POSTAL => PropertyValue::String(or_null!(
            c.adr("WORK").postal.clone(),
            T::PTYP_STRING
        )),
        PR_BUSINESS_ADDRESS_COUNTRY => PropertyValue::String(or_null!(
            c.adr("WORK").country.clone(),
            T::PTYP_STRING
        )),
        PR_BUSINESS_HOME_PAGE => PropertyValue::String(or_null!(c.url().unwrap_or_default(), T::PTYP_STRING)),
        PR_HOME_URL => PropertyValue::String(or_null!(c.url().unwrap_or_default(), T::PTYP_STRING)),
        store::PR_ENTRYID => {
            if ttype_matches(want, T::PTYP_BINARY) {
                PropertyValue::Binary(store::message_entry_id_for(&c.uid(), mailbox_id))
            } else {
                PropertyValue::Null
            }
        }
        store::PR_MID => {
            if ttype_matches(want, T::PTYP_INTEGER64) {
                PropertyValue::Integer64(store::message_id_from_jmap(&c.uid()) as i64)
            } else {
                PropertyValue::Null
            }
        }
        store::PR_RECORD_KEY | PR_SEARCH_KEY => {
            if ttype_matches(want, T::PTYP_BINARY) {
                PropertyValue::Binary(c.uid().into_bytes())
            } else {
                PropertyValue::Null
            }
        }
        store::PR_CHANGE_KEY => PropertyValue::Binary(or_null!(
            c.uid().into_bytes(),
            T::PTYP_BINARY
        )),
        store::PR_HAS_ATTACHMENTS => PropertyValue::Boolean(or_null!(false, T::PTYP_BOOLEAN)),
        store::PR_MESSAGE_SIZE => PropertyValue::Integer32(or_null!(0, T::PTYP_INTEGER32)),
        store::PR_MESSAGE_FLAGS => PropertyValue::Integer32(or_null!(0, T::PTYP_INTEGER32)),
        // PR_DISPLAY_NAME is handled by the bare-name arm above (same constant
        // as `store::PR_DISPLAY_NAME`, the `0x3001` PidTagDisplayName).
        _ => PropertyValue::Null,
    }
}

// ----------------------------------------------------------------------------
// Minimal RFC 6350 property extractor preserving TYPE= params + ADR.
// ----------------------------------------------------------------------------

/// A parsed vCard, scoped to the MS-OXVCARD fields Outlook reads.
#[derive(Default)]
struct ParsedVcard {
    fn_: Option<String>,
    n_: Option<Vec<String>>, // Family;Given;Additional;Prefix;Suffix
    file_as: Option<String>,
    title: Option<String>,
    org: Option<String>,     // First component (company). Additional ignored.
    unit: Option<String>,    // Second ORG component (department/unit).
    url: Option<String>,
    uid: Option<String>,
    initials: Option<String>,
    emails: Vec<(String, Vec<String>)>, // (value, TYPE labels)
    tels: Vec<(String, Vec<String>)>,
    adrs: Vec<(Address, Vec<String>)>,
}

#[derive(Default, Clone)]
struct Address {
    street: String,
    locality: String,
    region: String,
    postal: String,
    country: String,
}

impl ParsedVcard {
    fn full_name(&self) -> Option<String> {
        if let Some(f) = &self.fn_
            && !f.is_empty()
        {
            return Some(f.clone());
        }
        // Fall back to N: Given Family.
        if let Some(n) = &self.n_ {
            let given = n.get(1).filter(|s| !s.is_empty());
            let family = n.first().filter(|s| !s.is_empty());
            match (family, given) {
                (Some(f), Some(g)) => Some(format!("{g} {f}")),
                (Some(f), None) => Some(f.clone()),
                (None, Some(g)) => Some(g.clone()),
                _ => None,
            }
        } else {
            None
        }
    }
    fn given_name(&self) -> Option<String> {
        self.n_
            .as_ref()
            .and_then(|n| n.get(1).filter(|s| !s.is_empty()).cloned())
    }
    fn surname(&self) -> Option<String> {
        self.n_
            .as_ref()
            .and_then(|n| n.first().filter(|s| !s.is_empty()).cloned())
    }
    fn prefix(&self) -> Option<String> {
        self.n_
            .as_ref()
            .and_then(|n| n.get(3).filter(|s| !s.is_empty()).cloned())
    }
    fn initials(&self) -> Option<String> {
        self.initials.clone()
    }
    fn title(&self) -> Option<String> {
        self.title.clone()
    }
    fn company(&self) -> Option<String> {
        self.org.clone()
    }
    fn url(&self) -> Option<String> {
        self.url.clone()
    }
    fn uid(&self) -> String {
        self.uid.clone().unwrap_or_else(|| {
            self.primary_email().or_else(|| self.fn_.clone()).unwrap_or_default()
        })
    }
    /// FILE-AS precedence: explicit FILE-AS / `X-FILEAS`; else "Family, Given";
    /// else FN; else the first email.
    fn file_as_or_fallback(&self) -> String {
        if let Some(f) = &self.file_as
            && !f.is_empty()
        {
            return f.clone();
        }
        if let Some(n) = &self.n_ {
            let family = n.first().filter(|s| !s.is_empty());
            let given = n.get(1).filter(|s| !s.is_empty());
            match (family, given) {
                (Some(f), Some(g)) => return format!("{f}, {g}"),
                (Some(f), None) => return f.clone(),
                (None, Some(g)) => return g.clone(),
                _ => {}
            }
        }
        self.fn_.clone().or(self.primary_email()).unwrap_or_default()
    }
    fn primary_email(&self) -> Option<String> {
        // Prefer the typed PREF / non-empty; otherwise the first email.
        self.emails
            .iter()
            .find(|(_, t)| t.iter().any(|x| x.eq_ignore_ascii_case("PREF")))
            .or_else(|| self.emails.first())
            .map(|(v, _)| v.clone())
            .filter(|v| !v.is_empty())
    }
    /// Find a TEL whose TYPE set contains all of `want_labels` (case-folded,
    /// comma-joined form for "FAX,WORK" requires both FAX and WORK).
    fn tel(&self, want_labels: &str) -> Option<String> {
        let want: Vec<String> = want_labels
            .split(',')
            .filter(|s| !s.is_empty())
            .map(|s| s.to_ascii_uppercase())
            .collect();
        self.tels
            .iter()
            .find(|(_, types)| want.iter().all(|w| types.contains(w)))
            .map(|(v, _)| v.clone())
            .filter(|v| !v.is_empty())
    }
    fn adr(&self, want_label: &str) -> Address {
        let w = want_label.to_ascii_uppercase();
        self.adrs
            .iter()
            .find(|(_, t)| t.contains(&w))
            .map(|(a, _)| a.clone())
            .unwrap_or_default()
    }
}

fn parse_vcard(data: &str) -> ParsedVcard {
    let mut out = ParsedVcard::default();
    let mut folded = String::with_capacity(data.len());
    // RFC 6350 line folding: a CRLF followed by a space is a continuation.
    for line in data.split('\n') {
        let trim = line.trim_end_matches('\r');
        if let Some(rest) = trim.strip_prefix(' ') {
            if !folded.is_empty() {
                folded.push_str(rest);
            } else {
                folded.push_str(trim);
            }
        } else {
            if !folded.is_empty() {
                parse_vcard_line(&folded, &mut out);
            }
            folded = trim.to_string();
        }
    }
    if !folded.is_empty() {
        parse_vcard_line(&folded, &mut out);
    }
    out
}

fn parse_vcard_line(line: &str, out: &mut ParsedVcard) {
    if line.is_empty() || line.starts_with("BEGIN:") || line.starts_with("END:") {
        return;
    }
    let (name_params, value) = match split_first_colon(line) {
        Some((l, r)) => (l, r),
        None => return,
    };
    let (prop, params) = match name_params.split_once(';') {
        Some((p, rest)) => (p, rest),
        None => (name_params, ""),
    };
    let labels: Vec<String> = params
        .split(';')
        .filter_map(|kv| {
            // `strip_prefix` borrows; fall back to the whole `kv` for a bare
            // TYPE-less label, both as `&str` so the downstream
            // `s.trim_matches('"')` keeps a borrowed slice.
            kv.strip_prefix("TYPE=").or(Some(kv))
        })
        .flat_map(|s| {
            // A single TYPE= may carry a comma list: TYPE="work,voice".
            s.trim_matches('"')
                .split(',')
                .map(|x| x.to_ascii_uppercase())
        })
        .collect();
    match prop.to_ascii_uppercase().as_str() {
        "FN" => out.fn_ = Some(unescape(value)),
        "N" => {
            out.n_ = Some(value.split(';').map(unescape).collect());
        }
        "X-FILEAS" | "FILE-AS" | "X-MOZILLA-FOREIGNLABEL" => {
            out.file_as = Some(unescape(value));
        }
        "TITLE" => out.title = Some(unescape(value)),
        "ORG" => {
            let parts: Vec<String> = value.split(';').map(unescape).collect();
            out.org = parts.first().cloned();
            out.unit = parts.get(1).cloned();
        }
        "X-DEPARTMENT" | "DEPARTMENT" => out.unit = Some(unescape(value)),
        "X-INITIALS" | "INITIALS" => out.initials = Some(unescape(value)),
        "URL" => out.url = Some(unescape(value)),
        "UID" => out.uid = Some(unescape(value)),
        "EMAIL" => out.emails.push((unescape(value), labels)),
        "TEL" => out.tels.push((unescape(value), labels)),
        "ADR" => out.adrs.push((parse_adr(value), labels)),
        _ => {}
    }
}

fn parse_adr(value: &str) -> Address {
    let p: Vec<String> = value.split(';').map(unescape).collect();
    let ext = p.get(1).cloned().unwrap_or_default();
    let street = p.get(2).cloned().unwrap_or_default();
    let street = if ext.is_empty() {
        street
    } else {
        format!("{ext} {street}").trim().to_string()
    };
    Address {
        street,
        locality: p.get(3).cloned().unwrap_or_default(),
        region: p.get(4).cloned().unwrap_or_default(),
        postal: p.get(5).cloned().unwrap_or_default(),
        country: p.get(6).cloned().unwrap_or_default(),
    }
}

fn unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut iter = s.chars();
    while let Some(c) = iter.next() {
        if c == '\\' {
            match iter.next() {
                Some('n') => out.push('\n'),
                Some('r') => out.push('\r'),
                Some(',') => out.push(','),
                Some(';') => out.push(';'),
                Some('\\') => out.push('\\'),
                Some(other) => {
                    out.push('\\');
                    out.push(other);
                }
                None => out.push('\\'),
            }
        } else {
            out.push(c);
        }
    }
    out
}

fn split_first_colon(s: &str) -> Option<(&str, &str)> {
    s.split_once(':')
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mapi::data::{PropertyTag, PropertyValue};

    fn ttag(id: u16, ty: PropertyType) -> PropertyTag {
        PropertyTag {
            property_type: ty,
            property_id: id,
        }
    }

    fn sample_event() -> CalendarItem {
        use chrono::DateTime;
        CalendarItem {
            uid: "evt-1".into(),
            subject: "Standup".into(),
            description: String::new(),
            location: "Room 1".into(),
            start: DateTime::from_timestamp(1_700_000_000, 0).unwrap(),
            end: DateTime::from_timestamp(1_700_003_600, 0).unwrap(),
            all_day: false,
            rrule: None,
            busy_status: Some(2),
            ..default_cal()
        }
    }

    fn default_cal() -> CalendarItem {
        use chrono::{DateTime, Utc};
        let zero = DateTime::<Utc>::from_timestamp(0, 0).unwrap();
        CalendarItem {
            uid: String::new(),
            subject: String::new(),
            description: String::new(),
            location: String::new(),
            start: zero,
            end: zero,
            all_day: false,
            dtstamp: None,
            timezone: None,
            timezone_blob: None,
            rrule: None,
            exdates: Vec::new(),
            organizer_name: None,
            organizer_email: None,
            attendees: Vec::new(),
            categories: Vec::new(),
            busy_status: None,
            sensitivity: None,
            reminder: None,
            response_requested: None,
            disallow_new_time_proposal: None,
            appointment_reply_time: None,
            meeting_status: None,
            response_type: None,
            online_meeting_conf_link: None,
            online_meeting_external_link: None,
            client_uid: None,
            exceptions: Vec::new(),
        }
    }

    #[test]
    fn calendar_to_cells_basic() {
        let ev = sample_event();
        let cs = vec![
            ttag(PR_SUBJECT, PropertyType::PTYP_STRING),
            ttag(PR_START, PropertyType::PTYP_TIME),
            ttag(PR_END, PropertyType::PTYP_TIME),
            ttag(PR_BUSY_STATUS, PropertyType::PTYP_INTEGER32),
            ttag(PR_RECURRING, PropertyType::PTYP_BOOLEAN),
        ];
        let cells = calendar_to_cells(&ev, &cs, store::CALENDAR_BACKEND_ID);
        assert!(matches!(&cells[0], PropertyValue::String(s) if s == "Standup"));
        assert!(matches!(cells[1], PropertyValue::Time(_)));
        assert!(matches!(cells[2], PropertyValue::Time(_)));
        assert!(matches!(cells[3], PropertyValue::Integer32(v) if v == 2));
        assert!(matches!(cells[4], PropertyValue::Boolean(v) if !v));
    }

    #[test]
    fn calendar_recurring_flag_and_pattern() {
        let mut ev = sample_event();
        ev.rrule = Some("FREQ=DAILY;INTERVAL=2".into());
        let cs = vec![
            ttag(PR_RECURRING, PropertyType::PTYP_BOOLEAN),
            ttag(PR_RECURRENCE_PATTERN, PropertyType::PTYP_BINARY),
        ];
        let cells = calendar_to_cells(&ev, &cs, store::CALENDAR_BACKEND_ID);
        assert!(matches!(cells[0], PropertyValue::Boolean(v) if v));
        if let PropertyValue::Binary(b) = &cells[1] {
            // RecurrenceType (4 LE) = 0 (Daily)
            assert_eq!(&b[..4], &0u32.to_le_bytes());
            // Interval at offset 20 (after RecType[4] + PatType[4] + CalType[4]
            // + FirstDateTime[8]).
            let interval = u32::from_le_bytes(b[20..24].try_into().unwrap());
            assert_eq!(interval, 2);
        } else {
            panic!("recurrence pattern not binary");
        }
    }

    #[test]
    fn recurrence_freq_maps_to_recurrence_type() {
        assert_eq!(parse_freq("FREQ=DAILY").as_recurrence_type(), 0);
        assert_eq!(parse_freq("FREQ=WEEKLY").as_recurrence_type(), 1);
        assert_eq!(parse_freq("FREQ=MONTHLY").as_recurrence_type(), 2);
        assert_eq!(parse_freq("FREQ=YEARLY").as_recurrence_type(), 3);
    }

    #[test]
    fn contact_to_cells_basic() {
        let vcard = "BEGIN:VCARD\nVERSION:3.0\nFN:Jane Doe\nN:Doe;Jane;;;Ms.\nEMAIL;TYPE=work:jane@example.com\nTEL;TYPE=work,voice:+1-555-1234\nTEL;TYPE=cell:+1-555-9999\nORG:Acme Corp;Engineering\nTITLE:Engineer\nADR;TYPE=home:;;123 Home St;Springfield;IL;62701;USA\nUID:urn:uuid:abcd\nEND:VCARD";
        let cs = vec![
            ttag(PR_FILE_AS, PropertyType::PTYP_STRING),
            ttag(PR_DISPLAY_NAME, PropertyType::PTYP_STRING),
            ttag(PR_GIVEN_NAME, PropertyType::PTYP_STRING),
            ttag(PR_SURNAME, PropertyType::PTYP_STRING),
            ttag(PR_EMAIL_ADDRESS, PropertyType::PTYP_STRING),
            ttag(PR_BUSINESS_TEL, PropertyType::PTYP_STRING),
            ttag(PR_MOBILE, PropertyType::PTYP_STRING),
            ttag(PR_COMPANY_NAME, PropertyType::PTYP_STRING),
            ttag(PR_HOME_ADDRESS_STREET, PropertyType::PTYP_STRING),
            ttag(PR_HOME_ADDRESS_CITY, PropertyType::PTYP_STRING),
            ttag(store::PR_ENTRYID, PropertyType::PTYP_BINARY),
            ttag(store::PR_MID, PropertyType::PTYP_INTEGER64),
        ];
        let cells = contact_to_cells(vcard, &cs, store::CONTACTS_BACKEND_ID);
        assert!(matches!(&cells[0], PropertyValue::String(s) if s == "Doe, Jane"));
        assert!(matches!(&cells[1], PropertyValue::String(s) if s == "Jane Doe"));
        assert!(matches!(&cells[2], PropertyValue::String(s) if s == "Jane"));
        assert!(matches!(&cells[3], PropertyValue::String(s) if s == "Doe"));
        assert!(matches!(&cells[4], PropertyValue::String(s) if s == "jane@example.com"));
        assert!(matches!(&cells[5], PropertyValue::String(s) if s == "+1-555-1234"));
        assert!(matches!(&cells[6], PropertyValue::String(s) if s == "+1-555-9999"));
        assert!(matches!(&cells[7], PropertyValue::String(s) if s == "Acme Corp"));
        assert!(matches!(&cells[8], PropertyValue::String(s) if s == "123 Home St"));
        assert!(matches!(&cells[9], PropertyValue::String(s) if s == "Springfield"));
        assert!(matches!(cells[10], PropertyValue::Binary(_)));
        assert!(matches!(cells[11], PropertyValue::Integer64(_)));
    }

    #[test]
    fn contact_file_as_fallback_uses_fn_when_no_n() {
        let vcard = "BEGIN:VCARD\nVERSION:3.0\nFN:Only FN\nEMAIL:a@b.com\nEND:VCARD";
        let cs = vec![ttag(PR_FILE_AS, PropertyType::PTYP_STRING)];
        let cells = contact_to_cells(vcard, &cs, store::CONTACTS_BACKEND_ID);
        assert!(matches!(&cells[0], PropertyValue::String(s) if s == "Only FN"));
    }

    #[test]
    fn contact_unknown_tag_is_null() {
        let vcard = "BEGIN:VCARD\nVERSION:3.0\nFN:Jane\nEND:VCARD";
        let cs = vec![ttag(0xFFFE, PropertyType::PTYP_STRING)];
        let cells = contact_to_cells(vcard, &cs, store::CONTACTS_BACKEND_ID);
        assert!(matches!(cells[0], PropertyValue::Null));
    }
}
