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
    PR_APPOINTMENT_STATE_FLAGS, PR_APPOINTMENT_SUB_TYPE, PR_BUSINESS_ADDRESS_CITY,
    PR_BUSINESS_ADDRESS_COUNTRY, PR_BUSINESS_ADDRESS_POSTAL, PR_BUSINESS_ADDRESS_STATE,
    PR_BUSINESS_ADDRESS_STREET, PR_BUSINESS_FAX, PR_BUSINESS_HOME_PAGE, PR_BUSINESS_TEL,
    PR_BUSY_STATUS, PR_CHANGE_KEY, PR_CLEAN_GLOBAL_OBJECT_ID, PR_COMPANY_MAIN_TEL, PR_COMPANY_NAME,
    PR_DISPLAY_NAME, PR_DISPLAY_NAME_PREFIX, PR_EMAIL_ADDRESS, PR_EMAIL1_ADDRESS,
    PR_EMAIL1_DISPLAY, PR_END, PR_FILE_AS, PR_GENERATION, PR_GIVEN_NAME, PR_GLOBAL_OBJECT_ID,
    PR_HOME_ADDRESS_CITY, PR_HOME_ADDRESS_COUNTRY, PR_HOME_ADDRESS_POSTAL, PR_HOME_ADDRESS_STATE,
    PR_HOME_ADDRESS_STREET, PR_HOME_FAX, PR_HOME_TEL, PR_HOME_URL, PR_INITIALS, PR_LOCATION,
    PR_MESSAGE_CLASS, PR_MIDDLE_NAME, PR_MOBILE, PR_NORMALIZED_SUBJECT, PR_ORGANIZER,
    PR_OTHER_ADDRESS_CITY, PR_OTHER_ADDRESS_COUNTRY, PR_OTHER_ADDRESS_POSTAL,
    PR_OTHER_ADDRESS_STATE, PR_OTHER_ADDRESS_STREET, PR_OTHER_TEL, PR_PREDECESSOR_CHANGE_LIST,
    PR_PRIMARY_TEL, PR_RECORD_KEY, PR_RECURRENCE_PATTERN, PR_RECURRING, PR_REMINDER_DELTA,
    PR_REMINDER_SET, PR_REMINDER_TIME, PR_REQUIRED_ATTENDEES, PR_RESPONSE_STATUS, PR_RESPONSE_TYPE,
    PR_SEARCH_KEY, PR_START, PR_SUBJECT, PR_SURNAME, PR_TITLE,
};
use chrono::Datelike;

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

fn calendar_cell_for(item: &CalendarItem, tag: &PropertyTag, mailbox_id: &str) -> PropertyValue {
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
        PR_APPOINTMENT_SUB_TYPE => {
            // PidTagAppointmentSubType: 1 == all-day event hint (fSubType).
            PropertyValue::Boolean(or_null!(item.all_day, T::PTYP_BOOLEAN))
        }
        PR_RECURRING => PropertyValue::Boolean(or_null!(item.rrule.is_some(), T::PTYP_BOOLEAN)),
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
        PR_APPOINTMENT_REPLY_TIME => match item.appointment_reply_time.and_then(dt_to_filetime) {
            Some(ft) => PropertyValue::Time(or_null!(ft, T::PTYP_TIME)),
            None => PropertyValue::Null,
        },
        PR_APPOINTMENT_SEQUENCE => PropertyValue::Integer32(or_null!(0, T::PTYP_INTEGER32)),
        PR_REMINDER_SET => {
            PropertyValue::Boolean(or_null!(item.reminder.is_some(), T::PTYP_BOOLEAN))
        }
        PR_REMINDER_DELTA => {
            PropertyValue::Integer32(or_null!(item.reminder.unwrap_or(0), T::PTYP_INTEGER32))
        }
        PR_REMINDER_TIME => PropertyValue::Null,
        PR_GLOBAL_OBJECT_ID | PR_CLEAN_GLOBAL_OBJECT_ID => {
            if ttype_matches(want, T::PTYP_BINARY) {
                PropertyValue::Binary(global_object_id(item))
            } else {
                PropertyValue::Null
            }
        }
        PR_PREDECESSOR_CHANGE_LIST => {
            if ttype_matches(want, T::PTYP_BINARY) {
                // Empty XID list (no predecessor change keys) — Outlook reads
                // this as the row having no prior revisions.
                PropertyValue::Binary(Vec::new())
            } else {
                PropertyValue::Null
            }
        }
        PR_ORGANIZER => PropertyValue::String(or_null!(organizer_display(item), T::PTYP_STRING)),
        PR_REQUIRED_ATTENDEES => {
            PropertyValue::String(or_null!(attendees_display(item, false), T::PTYP_STRING))
        }
        PR_RECORD_KEY | PR_SEARCH_KEY => {
            if ttype_matches(want, T::PTYP_BINARY) {
                PropertyValue::Binary(item.uid.as_bytes().to_vec())
            } else {
                PropertyValue::Null
            }
        }
        PR_CHANGE_KEY => PropertyValue::Binary(or_null!(change_key(item), T::PTYP_BINARY)),
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
        store::PR_HAS_ATTACHMENTS => PropertyValue::Boolean(or_null!(false, T::PTYP_BOOLEAN)),
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

/// MS-OXOCAL §2.2.5 GlobalObjectId / CleanGlobalObjectId binary layout.
///
/// The canonical structure (per MS-OXOCAL §2.2.5.1 "ByteArrayStructure"):
///   ByteArrayID            (16 bytes) — the fixed XTMA meeting-namespace id
///     `04 00 00 00 82 00 E0 00 74 C5 B7 10 1A 82 E0 08`
///   Year                   (2 bytes LE) — start year
///   Month                  (1 byte)     — start month
///   Day                    (1 byte)     — start day
///   CreationTime           (8 bytes)    — FILETIME of creation (item start)
///   Reserved               (8 bytes)    — zero
///   Size                   (4 bytes LE) — byte length of the trailing Data
///   Data  [Size]           — the iCalendar UID (per MS-OXCICAL the UID maps
///                            into the GlobalObjectId Data for invite matching)
/// followed by a single terminating NUL. Outlook matches meeting invites by
/// equality of this id (GlobalObjectId == CleanGlobalObjectId modulo a
/// trailing suffix), so we serialise the full documented structure keyed off
/// the event's iCalendar UID and start date.
fn global_object_id(item: &CalendarItem) -> Vec<u8> {
    // The fixed 16-byte meeting-namespace ByteArrayID (MS-OXOCAL §2.2.5.2).
    const BYTE_ARRAY_ID: [u8; 16] = [
        0x04, 0x00, 0x00, 0x00, 0x82, 0x00, 0xE0, 0x00, 0x74, 0xC5, 0xB7, 0x10, 0x1A, 0x82, 0xE0,
        0x08,
    ];
    let start = item.start;
    let (year, month, day) = (
        (start.year() as u16).to_le_bytes(),
        start.month() as u8,
        start.day() as u8,
    );
    let creation_time = dt_to_filetime(start).unwrap_or(0).to_le_bytes();
    let data = item.uid.as_bytes();
    let size = data.len() as u32;
    let mut out = Vec::with_capacity(BYTE_ARRAY_ID.len() + 2 + 1 + 1 + 8 + 8 + 4 + data.len() + 1);
    out.extend_from_slice(&BYTE_ARRAY_ID);
    out.extend_from_slice(&year);
    out.push(month);
    out.push(day);
    out.extend_from_slice(&creation_time);
    out.extend_from_slice(&[0u8; 8]); // Reserved
    out.extend_from_slice(&size.to_le_bytes());
    out.extend_from_slice(data);
    out.push(0); // terminating NUL (matches Exchange's Data form)
    out
}

/// A change-sensitive change key. Per MS-OXCDATA §2.12.2 the change key is a
/// GUID-bearing byte sequence Outlook diffs for conflict detection — a *new*
/// key must be produced whenever the item is edited. Mixing `item.dtstamp`
/// (which CalDAV bumps on every mutation) on top of the stable UID means an
/// edited appointment yields a different key from the cached predecessor, so
/// Outlook's stale-row + conflict-detection path fires. The leading 16 bytes
/// are the provider GUID placeholder.
fn change_key(item: &CalendarItem) -> Vec<u8> {
    let dt = item
        .dtstamp
        .and_then(dt_to_filetime)
        .unwrap_or(0)
        .to_le_bytes();
    let mut out = Vec::with_capacity(16 + dt.len() + item.uid.len());
    out.extend_from_slice(&[0x01; 16]); // provider GUID placeholder
    out.extend_from_slice(&dt);
    out.extend_from_slice(item.uid.as_bytes());
    out
}

/// Serialise the MS-OXOCAL §2.2.4 AppointmentRecurrencePattern binary blob.
///
/// Outlook reads the recurrence *pattern* off this blob and expands
/// occurrences itself; the blob must be a faithful-enough encoding that
/// Outlook recognises the recurrence kind. We decode the iCalendar `RRULE`
/// for the leading `FREQ=` and the `INTERVAL=`/`COUNT=`/`UNTIL=`/`BYDAY=`
/// and emit the matching MS-OXOCAL RecurrencePattern structure:
///
///   RecurrenceType(4 LE)       — 0=daily,1=weekly,2=monthly,3=yearly
///   PatternType(4 LE)         — 0=Interval,1=Pattern,2=DayOfMonthMonthly*
///   CalendarType(4 LE)        — 0=default
///   FirstDateTime(8 LE)        — 0
///   Interval(4 LE)             — n
///   WeekIndex(4 LE)            — 0
///   FirstDOW(4 LE)             — 0 (Sun)
///   OuterDuration(4 LE)        — 0
///   AdditionalFlags(4 LE)      — 0x01 END_DATE bit set when UNTIL present
///   PatternSpecific(8 LE)      — 0
///   EndTime(8 LE)              — UNTIL FILETIME (0 when unbounded)
///   OccurrenceCount(4 LE)      — RRULE COUNT (0 when UNTIL-bounded/unbounded)
///   ModifiedInstanceCount(4 LE) — 0 (modified exception blocks not modelled)
///   DeletedInstanceCount(4 LE)  — count of deleted occurrences
///   DeletedInstanceDates[Count](8 LE each) — FILETIME of each EXDATE
///
/// We emit the blob Outlook accepts for a RECURRING appointment. The
/// recurrence-type + interval let Outlook expand DAILY/WEEKLY/MONTHLY/
/// YEARLY series; the END_DATE flag + EndTime bound a UNTIL= series, the
/// OccurrenceCount bounds a COUNT= series, and the deleted-instance DATE
/// array conveys EXDATE deletions so the blob stays structurally consist
/// (Outlook decoder always reads exactly DeletedInstanceCount FILETIMEs
/// after the count).
fn recurrence_pattern_bytes(item: &CalendarItem, rrule: &str) -> Vec<u8> {
    let freq = parse_freq(rrule);
    let interval = parse_interval(rrule).unwrap_or(1).max(1);
    let count = parse_count(rrule).unwrap_or(0);
    let until_ft = parse_until(rrule).and_then(dt_to_filetime).unwrap_or(0);
    let pattern_type = match freq {
        Freq::Daily => 0u32,
        Freq::Weekly => 0u32,
        Freq::Monthly => 2u32, // DayOfMonth monthly
        Freq::Yearly => 2u32,
    };

    // Deleted occurrences: EXDATE values (item.exdates) plus any exception
    // flagged `deleted`. Each is serialised as its original-start FILETIME in
    // the DeletedInstanceDates[DeletedInstanceCount] array (MS-OXOCAL §2.2.4)
    // so the blob is structurally consistent — Outlook's decoder reads exactly
    // `DeletedInstanceCount` 8-byte FILETIMEs after the counts.
    let deleted_dates: Vec<u64> = item
        .exdates
        .iter()
        .filter_map(|d| dt_to_filetime(*d))
        .chain(
            item.exceptions
                .iter()
                .filter(|e| e.deleted)
                .filter_map(|e| dt_to_filetime(e.exception_start)),
        )
        .collect();
    // Modified exceptions (RECURRENCE-ID without deleted) require a full
    // per-exception property block which is not yet modelled on the read path
    // — keep ModifiedInstanceCount = 0 so the blob carries no modified blocks,
    // which is structurally valid (Outlook expands the master pattern only).
    let modified = 0u32;

    // END_DATE flag (bit 0x01) in AdditionalFlags: tell Outlook the series is
    // bounded by EndTime (the UNTIL FILETIME). Also set bit 0x02
    // (regenerating) only when COUNT terminates the series; we map COUNT via
    // the OccurrenceCount field below instead of an additional flag, so it
    // stays clear of the END_DATE bit. With the now-fixed UNTIL parser the
    // bit is set exactly when a real bound is present.
    let end_date_flag: u32 = (until_ft != 0) as u32;

    let mut out = Vec::with_capacity(64 + deleted_dates.len() * 8);
    out.extend_from_slice(&freq.as_recurrence_type().to_le_bytes()); // RecurrenceType
    out.extend_from_slice(&pattern_type.to_le_bytes()); // PatternType
    out.extend_from_slice(&0u32.to_le_bytes()); // CalendarType
    out.extend_from_slice(&0u64.to_le_bytes()); // FirstDateTime
    out.extend_from_slice(&interval.to_le_bytes()); // Interval
    out.extend_from_slice(&0u32.to_le_bytes()); // WeekIndex/Instance
    out.extend_from_slice(&0u32.to_le_bytes()); // FirstDOW
    out.extend_from_slice(&0u32.to_le_bytes()); // OuterDuration
    out.extend_from_slice(&end_date_flag.to_le_bytes()); // AdditionalFlags
    out.extend_from_slice(&0u64.to_le_bytes()); // PatternSpecific
    out.extend_from_slice(&until_ft.to_le_bytes()); // EndTime (until as FT; 0 if none)
    out.extend_from_slice(&count.to_le_bytes()); // OccurrenceCount (RRULE COUNT)
    out.extend_from_slice(&modified.to_le_bytes()); // ModifiedInstanceCount
    out.extend_from_slice(&(deleted_dates.len() as u32).to_le_bytes()); // DeletedInstanceCount
    // DeletedInstanceDates — one 8-byte FILETIME per deleted occurrence,
    // required whenever DeletedInstanceCount > 0 (otherwise the blob is
    // structurally inconsistent and Outlook ignores the recurrence).
    for ft in &deleted_dates {
        out.extend_from_slice(&ft.to_le_bytes());
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

/// Parse the iCalendar `COUNT=` bound (the number of occurrences including
/// the first). Used to terminate the series via the recurrence blob's
/// OccurrenceCount field rather than an UNTIL date.
fn parse_count(rrule: &str) -> Option<u32> {
    for part in rrule.split(';') {
        if let Some(v) = part.strip_prefix("COUNT=") {
            return v.parse().ok().filter(|n: &u32| *n > 0);
        }
    }
    None
}

fn parse_until(rrule: &str) -> Option<chrono::DateTime<chrono::Utc>> {
    for part in rrule.split(';') {
        if let Some(v) = part.strip_prefix("UNTIL=") {
            // iCalendar UNTIL is a basic-form UTC datetime ("20251231T235959Z")
            // or a date-only ("20251231"), NOT RFC3339 (the parser rejected the
            // compact form, dropping the end bound). Try the compact UTC form,
            // then date-only (resolved to end-of-day UTC), then RFC3339 as a
            // permissive fallback.
            let v = v.trim();
            if let Ok(dt) = chrono::NaiveDateTime::parse_from_str(v, "%Y%m%dT%H%M%SZ") {
                return Some(dt.and_utc());
            }
            if let Ok(d) = chrono::NaiveDate::parse_from_str(v, "%Y%m%d") {
                // A date-only UNTIL bound means "no occurrence starting after
                // this date"; resolve it to the start of the next UTC day so
                // the bound includes the whole UNTIL day.
                return Some(d.succ_opt()?.and_hms_opt(0, 0, 0)?.and_utc());
            }
            if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(v) {
                return Some(dt.with_timezone(&chrono::Utc));
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
    let change_key = contact_change_key(&parsed.uid(), vcard);
    let mut out = Vec::with_capacity(column_set.len());
    for tag in column_set {
        out.push(contact_cell_for(&parsed, tag, mailbox_id, &change_key));
    }
    out
}

/// A change-sensitive change key for a contact: mixes a stable digest of the
/// *raw vCard text* on top of the (immutable) vCard UID, so an edit to any
/// field produces a different `PR_CHANGE_KEY`. Outlook diffs change keys to
/// detect stale rows + conflicts, so a UID-only key would hide edits. The
/// leading 16 bytes are the provider GUID placeholder, per MS-OXCDATA §2.12.2.
fn contact_change_key(uid: &str, vcard: &str) -> Vec<u8> {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    // Folded-line whitespace is normalised so a repackaged-but-equal vCard
    // (different folding) hashes the same; the leading BEGIN/END lines are
    // excluded so an identical body under different protocol noise stays equal.
    let body = vcard
        .lines()
        .filter(|l| !l.starts_with("BEGIN:") && !l.starts_with("END:"))
        .map(|l| l.trim())
        .collect::<Vec<_>>()
        .join("\n");
    let mut h = DefaultHasher::new();
    body.hash(&mut h);
    let digest = h.finish().to_le_bytes();
    let mut out = Vec::with_capacity(16 + digest.len() + uid.len());
    out.extend_from_slice(&[0x01; 16]); // provider GUID placeholder
    out.extend_from_slice(&digest);
    out.extend_from_slice(uid.as_bytes());
    out
}

fn contact_cell_for(
    c: &ParsedVcard,
    tag: &PropertyTag,
    mailbox_id: &str,
    change_key: &[u8],
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
        PR_DISPLAY_NAME => PropertyValue::String(or_null!(display_name(), T::PTYP_STRING)),
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
        PR_MIDDLE_NAME => PropertyValue::String(or_null!(
            c.middle_name().unwrap_or_default(),
            T::PTYP_STRING
        )),
        PR_GENERATION => {
            PropertyValue::String(or_null!(c.generation().unwrap_or_default(), T::PTYP_STRING))
        }
        PR_TITLE => PropertyValue::String(or_null!(c.title().unwrap_or_default(), T::PTYP_STRING)),
        PR_COMPANY_NAME => {
            PropertyValue::String(or_null!(c.company().unwrap_or_default(), T::PTYP_STRING))
        }
        PR_EMAIL_ADDRESS | PR_EMAIL1_ADDRESS => PropertyValue::String(or_null!(
            c.primary_email().unwrap_or_default(),
            T::PTYP_STRING
        )),
        PR_EMAIL1_DISPLAY => PropertyValue::String(or_null!(display_name(), T::PTYP_STRING)),
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
        PR_BUSINESS_FAX => PropertyValue::String(or_null!(
            c.tel("FAX,WORK").unwrap_or_default(),
            T::PTYP_STRING
        )),
        PR_HOME_FAX => PropertyValue::String(or_null!(
            c.tel("FAX,HOME").unwrap_or_default(),
            T::PTYP_STRING
        )),
        PR_COMPANY_MAIN_TEL => {
            PropertyValue::String(or_null!(c.tel("WORK").unwrap_or_default(), T::PTYP_STRING))
        }
        PR_HOME_ADDRESS_STREET => {
            PropertyValue::String(or_null!(c.adr("HOME").street.clone(), T::PTYP_STRING))
        }
        PR_HOME_ADDRESS_CITY => {
            PropertyValue::String(or_null!(c.adr("HOME").locality.clone(), T::PTYP_STRING))
        }
        PR_HOME_ADDRESS_STATE => {
            PropertyValue::String(or_null!(c.adr("HOME").region.clone(), T::PTYP_STRING))
        }
        PR_HOME_ADDRESS_POSTAL => {
            PropertyValue::String(or_null!(c.adr("HOME").postal.clone(), T::PTYP_STRING))
        }
        PR_HOME_ADDRESS_COUNTRY => {
            PropertyValue::String(or_null!(c.adr("HOME").country.clone(), T::PTYP_STRING))
        }
        PR_BUSINESS_ADDRESS_STREET => {
            PropertyValue::String(or_null!(c.adr("WORK").street.clone(), T::PTYP_STRING))
        }
        PR_BUSINESS_ADDRESS_CITY => {
            PropertyValue::String(or_null!(c.adr("WORK").locality.clone(), T::PTYP_STRING))
        }
        PR_BUSINESS_ADDRESS_STATE => {
            PropertyValue::String(or_null!(c.adr("WORK").region.clone(), T::PTYP_STRING))
        }
        PR_BUSINESS_ADDRESS_POSTAL => {
            PropertyValue::String(or_null!(c.adr("WORK").postal.clone(), T::PTYP_STRING))
        }
        PR_BUSINESS_ADDRESS_COUNTRY => {
            PropertyValue::String(or_null!(c.adr("WORK").country.clone(), T::PTYP_STRING))
        }
        PR_OTHER_ADDRESS_STREET => {
            PropertyValue::String(or_null!(c.adr("OTHER").street.clone(), T::PTYP_STRING))
        }
        PR_OTHER_ADDRESS_CITY => {
            PropertyValue::String(or_null!(c.adr("OTHER").locality.clone(), T::PTYP_STRING))
        }
        PR_OTHER_ADDRESS_STATE => {
            PropertyValue::String(or_null!(c.adr("OTHER").region.clone(), T::PTYP_STRING))
        }
        PR_OTHER_ADDRESS_POSTAL => {
            PropertyValue::String(or_null!(c.adr("OTHER").postal.clone(), T::PTYP_STRING))
        }
        PR_OTHER_ADDRESS_COUNTRY => {
            PropertyValue::String(or_null!(c.adr("OTHER").country.clone(), T::PTYP_STRING))
        }
        PR_BUSINESS_HOME_PAGE => {
            PropertyValue::String(or_null!(c.url().unwrap_or_default(), T::PTYP_STRING))
        }
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
        store::PR_CHANGE_KEY => {
            PropertyValue::Binary(or_null!(change_key.to_vec(), T::PTYP_BINARY))
        }
        PR_PREDECESSOR_CHANGE_LIST => {
            if ttype_matches(want, T::PTYP_BINARY) {
                // Empty XID list — no predecessor change keys.
                PropertyValue::Binary(Vec::new())
            } else {
                PropertyValue::Null
            }
        }
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
    org: Option<String>,  // First component (company). Additional ignored.
    unit: Option<String>, // Second ORG component (department/unit).
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
    /// N: Additional (middle) name component — index 2.
    fn middle_name(&self) -> Option<String> {
        self.n_
            .as_ref()
            .and_then(|n| n.get(2).filter(|s| !s.is_empty()).cloned())
    }
    /// N: honorific suffix (generation qualifier: Jr./Sr./III) — index 4,
    /// mapped to PidTagGeneration.
    fn generation(&self) -> Option<String> {
        self.n_
            .as_ref()
            .and_then(|n| n.get(4).filter(|s| !s.is_empty()).cloned())
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
            self.primary_email()
                .or_else(|| self.fn_.clone())
                .unwrap_or_default()
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
        self.fn_
            .clone()
            .or(self.primary_email())
            .unwrap_or_default()
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

    /// CR6: PR_CHANGE_KEY must differ when the vCard body changes, so Outlook
    /// detects edits (a UID-only key would hide them).
    #[test]
    fn contact_change_key_differs_on_body_edit() {
        let v1 = "BEGIN:VCARD\nVERSION:3.0\nFN:Jane Doe\nEMAIL:jane@example.com\nUID:urn:uuid:xyz\nEND:VCARD";
        let v2 = "BEGIN:VCARD\nVERSION:3.0\nFN:Jane Doe\nEMAIL:jane@other.com\nUID:urn:uuid:xyz\nEND:VCARD";
        let cs = vec![
            ttag(store::PR_CHANGE_KEY, PropertyType::PTYP_BINARY),
            ttag(store::PR_RECORD_KEY, PropertyType::PTYP_BINARY),
        ];
        let c1 = contact_to_cells(v1, &cs, store::CONTACTS_BACKEND_ID);
        let c2 = contact_to_cells(v2, &cs, store::CONTACTS_BACKEND_ID);
        let (PropertyValue::Binary(k1), PropertyValue::Binary(k2)) = (&c1[1], &c2[1]) else {
            panic!("expected binary record key");
        };
        // Change key must change with the body ...
        assert_ne!(c1[0], c2[0], "change key should differ when body edits");
        // ... but the record key (stable UID) stays the same.
        assert_eq!(k1, k2, "record key should be stable across edits");
    }

    /// Q3/CR1: a compact-basic UNTIL bound rounds through the recurrence
    /// blob as a real EndTime + the END_DATE flag (AdditionalFlags bit 0x01),
    /// and parse_until now resolves compact "20251231T235959Z".
    #[test]
    fn recurrence_until_set_end_date_flag_and_endtime() {
        let mut ev = sample_event();
        ev.rrule = Some("FREQ=DAILY;INTERVAL=1;UNTIL=20251231T235959Z".into());
        let cs = vec![ttag(PR_RECURRENCE_PATTERN, PropertyType::PTYP_BINARY)];
        let cells = calendar_to_cells(&ev, &cs, store::CALENDAR_BACKEND_ID);
        let PropertyValue::Binary(b) = &cells[0] else {
            panic!("expected binary recurrence pattern");
        };
        // Layout: RecType4 Pat4 Cal4 First8 Interval4 Week4 FirstDOW4 Outer4
        // Flags4 PatternSpecific8 EndTime8 OccCount4 ModCount4 DelCount4
        // => AdditionalFlags at 36, EndTime at 48, OccCount 56, ModCount 60,
        // DelCount 64.
        let flags = u32::from_le_bytes(b[36..40].try_into().unwrap());
        assert_eq!(flags & 0x01, 0x01, "END_DATE flag must be set for UNTIL");
        let end_time = u64::from_le_bytes(b[48..56].try_into().unwrap());
        assert_ne!(end_time, 0, "EndTime must be non-zero for UNTIL");
        let until_ft = parse_until(ev.rrule.as_deref().unwrap())
            .and_then(dt_to_filetime)
            .unwrap();
        assert_eq!(end_time, until_ft);
        let count = u32::from_le_bytes(b[56..60].try_into().unwrap());
        assert_eq!(count, 0);
        let mod_count = u32::from_le_bytes(b[60..64].try_into().unwrap());
        assert_eq!(mod_count, 0);
        let del_count = u32::from_le_bytes(b[64..68].try_into().unwrap());
        assert_eq!(del_count, 0);
        assert_eq!(b.len(), 68);
    }

    /// CR1: a COUNT=... series leaves EndTime 0 and stamps OccurrenceCount.
    #[test]
    fn recurrence_count_stamps_occurrence_count() {
        let mut ev = sample_event();
        ev.rrule = Some("FREQ=DAILY;INTERVAL=1;COUNT=5".into());
        let cs = vec![ttag(PR_RECURRENCE_PATTERN, PropertyType::PTYP_BINARY)];
        let cells = calendar_to_cells(&ev, &cs, store::CALENDAR_BACKEND_ID);
        let PropertyValue::Binary(b) = &cells[0] else {
            panic!("expected binary recurrence pattern");
        };
        let flags = u32::from_le_bytes(b[36..40].try_into().unwrap());
        assert_eq!(flags & 0x01, 0, "no END_DATE flag for COUNT series");
        let end_time = u64::from_le_bytes(b[48..56].try_into().unwrap());
        assert_eq!(end_time, 0, "EndTime must be 0 for COUNT series");
        let count = u32::from_le_bytes(b[56..60].try_into().unwrap());
        assert_eq!(count, 5);
    }

    /// Q2: EXDATE deletions are serialised into the DeletedInstanceDates
    /// array, keeping the blob structurally consistent (the decoder reads
    /// exactly DeletedInstanceCount FILETIMEs after the count).
    #[test]
    fn recurrence_exdates_serialised_as_deleted_instances() {
        use chrono::TimeZone;
        let mut ev = sample_event();
        ev.rrule = Some("FREQ=DAILY;INTERVAL=1".into());
        ev.exdates = vec![
            chrono::Utc.with_ymd_and_hms(2025, 1, 2, 9, 0, 0).unwrap(),
            chrono::Utc.with_ymd_and_hms(2025, 1, 3, 9, 0, 0).unwrap(),
        ];
        let cs = vec![ttag(PR_RECURRENCE_PATTERN, PropertyType::PTYP_BINARY)];
        let cells = calendar_to_cells(&ev, &cs, store::CALENDAR_BACKEND_ID);
        let PropertyValue::Binary(b) = &cells[0] else {
            panic!("expected binary recurrence pattern");
        };
        let del_count = u32::from_le_bytes(b[64..68].try_into().unwrap());
        assert_eq!(del_count, 2);
        // DeletedInstanceDates follow at offset 68, one 8-byte FT each.
        assert_eq!(b.len(), 68 + (2 * 8));
        let ft0 = u64::from_le_bytes(b[68..76].try_into().unwrap());
        let ft1 = u64::from_le_bytes(b[76..84].try_into().unwrap());
        assert_ne!(ft0, 0);
        assert_ne!(ft1, 0);
        assert_ne!(ft0, ft1);
    }

    /// CR5: the GlobalObjectId now carries the full MS-OXOCAL ByteArrayID
    /// structure, starting with the fixed meeting-namespace id.
    #[test]
    fn global_object_id_has_full_byte_array_id() {
        let ev = sample_event();
        let cs = vec![ttag(PR_GLOBAL_OBJECT_ID, PropertyType::PTYP_BINARY)];
        let cells = calendar_to_cells(&ev, &cs, store::CALENDAR_BACKEND_ID);
        let PropertyValue::Binary(b) = &cells[0] else {
            panic!("expected binary global object id");
        };
        // Fixed 16-byte ByteArrayID per MS-OXOCAL §2.2.5.2.
        const EXPECTED: [u8; 16] = [
            0x04, 0x00, 0x00, 0x00, 0x82, 0x00, 0xE0, 0x00, 0x74, 0xC5, 0xB7, 0x10, 0x1A, 0x82,
            0xE0, 0x08,
        ];
        assert_eq!(&b[..16], &EXPECTED);
        // Year(2) Month(1) Day(1) CreationTime(8) Reserved(8) Size(4) Data[n]
        // NUL — Size is at 16+2+1+1+8+8 = 36, Data follows.
        assert!(b.len() >= 41);
        let size = u32::from_le_bytes(b[36..40].try_into().unwrap()) as usize;
        assert_eq!(size, sample_event().uid.len());
        assert_eq!(&b[40..40 + size], sample_event().uid.as_bytes());
        assert_eq!(b[b.len() - 1], 0);
    }
}
