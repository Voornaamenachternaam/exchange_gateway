// src/mapi/store.rs
//
// The bridge between MAPI property tags and the Stalwart backends
// (JMAP/CalDAV/CardDAV). The dispatcher in `handler.rs` uses this module to:
//   * turn a JMAP `Mailbox` role into a MAPI `FolderKind` + synthetic 64-bit
//     folder id,
//   * turn a JMAP `JmapEmail` (or a parsed vCard / iCalendar event) into the
//     ordered `PropertyValue` cells a `RopQueryRows`/`RopGetPropertiesSpecific`
//     response serialises for the requested column set.
//
// This module is deliberately free of async / network I/O so it is fully
// unit-testable: backend-fetch happens in `handler.rs` which hands typed
// backend objects (`JmapEmail`, `JmapMailbox`, parsed vCard/iCalendar) to
// the pure converters here. This keeps the conversion logic auditable
// against MS-OXCDATA and the []MS-OXPROPS] property table without dragging
// the JMAP/CardDAV stack into the unit tests.
//
// The property-id constants below are the well-known `PidTag*` identifiers
// the Outlook client queries on mail table rows and message GetProperties;
// they come straight from []MS-OXPROPS] and are stable across Outlook builds.
// Named properties (the 0x8000-bit set) Outlook synthesises per-mailbox are
// resolved lazily and returned as `PropertyValue::Null` until the named-property
// table is wired in Phase 2.

use crate::jmap::{JmapEmail, JmapMailbox};
use crate::mapi::data::{PropertyRowEntry, PropertyTag, PropertyType, PropertyValue};

// ----------------------------------------------------------------------------
// Well-known PidTag PropertyId constants (MS-OXPROPS), numeric form, without
// the 0x8000 named bit. Listed here as u16 so they directly populate
// `PropertyTag.property_id`.
// ----------------------------------------------------------------------------

/// PidTagFolderId — folder id, 0000_6710 (Integer64). Returned in a hierarchy
/// table rowset and on `RopOpenFolder`.
pub const PR_FOLDER_ID: u16 = 0x6710;
/// PidTagParentFolderId — 0000_6715 (Integer64).
pub const PR_PARENT_FOLDER_ID: u16 = 0x6715;
/// PidTagDisplayName — 3001 (String).
pub const PR_DISPLAY_NAME: u16 = 0x3001;
/// PidTagContainerClass — 3613 (String). "IPF.Note" / "IPF.Appointment" / "IPF.Contact".
pub const PR_CONTAINER_CLASS: u16 = 0x3613;
/// PidTagContentCount — 3602 (Integer32).
pub const PR_CONTENT_COUNT: u16 = 0x3602;
/// PidTagContentUnread — 3603 (Integer32).
pub const PR_CONTENT_UNREAD: u16 = 0x3603;
/// PidTagSubfolders — 360A (Boolean).
pub const PR_SUBFOLDERS: u16 = 0x360A;
/// PidTagChildCount — 360D (Integer32).
pub const PR_CHILD_COUNT: u16 = 0x360D;

/// PidTagMessageClass — 001A (String). "IPM.Note" / "IPM.Appointment" / "IPM.Contact".
pub const PR_MESSAGE_CLASS: u16 = 0x001A;
/// PidTagSubject — 0037 (String).
pub const PR_SUBJECT: u16 = 0x0037;
/// PidTagSubjectPrefix + NormalizeSubject collapsed — 0x003D (String) is the
/// normalised subject; Outlook typically reads 0x0037.
pub const PR_NORMALIZED_SUBJECT: u16 = 0x0E1D;
/// PidTagBody — 1000 (String8/native). The gateway returns the plain-text
/// body value from JMAP `bodyValues`.
pub const PR_BODY: u16 = 0x1000;
/// PidTagBodyHtml — 1013 (String). HTML body.
pub const PR_BODY_HTML: u16 = 0x1013;
/// PidTagNativeBody — 0x1016 — content type hint we leave NULL for Phase 1.
pub const PR_NATIVE_BODY: u16 = 0x1016;
/// PidTagRtfCompressed — 1009 (Binary). The RTF body, held compressed per
/// MS-OXBBODY §2. **IMPORTANT: The gateway does not synthesize actual LZFu-compressed
/// RTF from the plain/HTML body (no Rust RTF-compression codec available). Instead,
/// the gateway returns the HTML body as a UTF-16LE fallback per MS-OXBBODY §2 best-value
/// rule. Outlook will use this HTML content as the RTF fallback when the client
/// requests PR_RTF_COMPRESSED. This provides readable content for RTF-aware clients;
/// RTF-only formatting (complex styling, embedded images, etc.) may be degraded
/// since the HTML is not converted to true compressed RTF format.**
/// The constant is exported so the stream codec can return an HTML-body stream for
/// an OpenStream on it rather than `NotFound` (which would make Outlook fall back
/// to RTF-only rendering, potentially losing the HTML content entirely).
pub const PR_RTF_COMPRESSED: u16 = 0x1009;

/// PidTagAttachDataBinary — 3702 (Binary). The raw attachment payload fetched
/// via JMAP `/download/{accountId}/{blobId}`.
pub const PR_ATTACH_DATA_BIN: u16 = 0x3702;
/// PidTagAttachDataObj — 3701 (Object). OLE attachments; unsupported (treated
/// as a typed NULL). Exported so the stream codec can identify it.
pub const PR_ATTACH_DATA_OBJ: u16 = 0x3701;
/// PidTagAttachEncoding — 3703 (Binary). MIME encoding hints; passed through.
pub const PR_ATTACH_ENCODING: u16 = 0x3703;
/// PidTagAttachExtension — 370B (String8). The attachment file extension.
pub const PR_ATTACH_EXTENSION: u16 = 0x370B;
/// PidTagAttachFilename — 3704 (String8). The short (8.3) attachment file name.
pub const PR_ATTACH_FILENAME: u16 = 0x3704;
/// PidTagAttachLongFilename — 3707 (String). The long attachment file name.
pub const PR_ATTACH_LONG_FILENAME: u16 = 0x3707;
/// PidTagAttachMimeTag — 3712 (String). The attachment MIME content type.
pub const PR_ATTACH_MIME_TAG: u16 = 0x3712;
/// PidTagAttachMethod — 3705 (Integer32). 0=by value, 1=by reference, 6=OLE.
pub const PR_ATTACH_METHOD: u16 = 0x3705;
/// PidTagAttachNumber — 0E21 (Integer32). Sequential attachment index.
pub const PR_ATTACH_NUM: u16 = 0x0E21;
/// PidTagAttachSize — 0E20 (Integer32). Attachment size incl. overhead.
pub const PR_ATTACH_SIZE: u16 = 0x0E20;
/// PidTagAttachFlags — 3710 (Bitmap). Flags controlling display of the
/// attachment; passed through opaquely.
pub const PR_ATTACH_FLAGS: u16 = 0x3710;

/// PidTagSenderName — 0C1A (String).
pub const PR_SENDER_NAME: u16 = 0x0C1A;
/// PidTagSenderEmailAddress — 0C1F (String).
pub const PR_SENDER_EMAIL: u16 = 0x0C1F;
/// PidTagSenderEntryId — 0C19 (Binary). We synthesise a stable 28-byte
/// one-off entry id (MAPI one-off) per sender.
pub const PR_SENDER_ENTRYID: u16 = 0x0C19;
/// PidTagSentRepresentingName — 0042 (String).
pub const PR_SENT_REPRESENTING_NAME: u16 = 0x0042;
/// PidTagSentRepresentingEmailAddress — 0065 (String).
pub const PR_SENT_REPRESENTING_EMAIL: u16 = 0x0065;

/// PidTagMessageDeliveryTime — 0E06 (Time). When the email arrived (JMAP
/// `receivedAt`).
pub const PR_MESSAGE_DELIVERY_TIME: u16 = 0x0E06;
/// PidTagClientSubmitTime — 0039 (Time). When the sender hit send (`sentAt`).
pub const PR_CLIENT_SUBMIT_TIME: u16 = 0x0039;
/// PidTagMessageSize (incl server envelope) — 0E08 (Integer32).
pub const PR_MESSAGE_SIZE: u16 = 0x0E08;
/// PidTagHasAttachments — 0E1B (Boolean).
pub const PR_HAS_ATTACHMENTS: u16 = 0x0E1B;
/// PidTagFlags — 0E08 reused? — PidTagMessageFlags is 0E07 (Integer32).
pub const PR_MESSAGE_FLAGS: u16 = 0x0E07;
/// PidTagImportance — 0017 (Integer32).
pub const PR_IMPORTANCE: u16 = 0x0017;
/// PidTagFlagStatus — 0x1090 (Integer32). MS-OXOFLAG 2.2.1.1: 0x01
/// followupComplete, 0x02 followupFlagged; absence means unflagged.
pub const PR_FLAG_STATUS: u16 = 0x1090;
/// PidTagFollowupIcon — 0x1095 (Integer32). MS-OXOFLAG 2.2.1.2: flag color
/// (0..6). No JMAP keyword equivalent; the gateway accepts the value but
/// does not persist it across the JMAP backend.
pub const PR_FOLLOWUP_ICON: u16 = 0x1095;
/// PidTagToDoItemFlags — 0x0E2B (Integer32). MS-OXOFLAG 2.2.1.6: bit-field
/// describing the to-do entry kind. Tolerated on SetProperties (no JMAP
/// analogue); persistence is Phase 2.
pub const PR_TODO_ITEM_FLAGS: u16 = 0x0E2B;
/// PidTagSensitivity — 0036 (Integer32).
pub const PR_SENSITIVITY: u16 = 0x0036;
/// PidTagInternetMessageId — 1035 (String).
pub const PR_INTERNET_MESSAGE_ID: u16 = 0x1035;
/// PidTagInReplyToId — 1042 (String).
pub const PR_IN_REPLY_TO_ID: u16 = 0x1042;
/// PidTagInternetReferences — 1039 (String).
pub const PR_INTERNET_REFERENCES: u16 = 0x1039;
/// PidTagConversationId — 0x3013 (Binary, 16 bytes) — hashed thread id.
pub const PR_CONVERSATION_ID: u16 = 0x3013;
/// PidTagEntryId — 0FFF (Binary). The message entry id we synthesise.
pub const PR_ENTRYID: u16 = 0x0FFF;
/// PidTagParentFolderId on a message — 0E08? — PidTagParentEntryId = 0E09.
pub const PR_PARENT_ENTRYID: u16 = 0x0E09;
/// PidTagRecordKey — 0FF9 (Binary). We return the JMAP id bytes.
pub const PR_RECORD_KEY: u16 = 0x0FF9;
/// PidTagSearchKey — 300B (Binary).
pub const PR_SEARCH_KEY: u16 = 0x300B;
/// PidTagMid — 6748 (Integer64). The message's folder-relative id.
pub const PR_MID: u16 = 0x6748;
/// PidTagChangeKey — 65E2 (Binary). The per-revision key; we derive from
/// JMAP blob/etag. Outlook uses it for sync de-dupe.
pub const PR_CHANGE_KEY: u16 = 0x65E2;
/// PidTagPredecessorChangeList — `0x65E8` (Binary, XID array). The chain of
/// prior change keys; we emit an empty list for gateway-synthesised rows.
pub const PR_PREDECESSOR_CHANGE_LIST: u16 = 0x65E8;
/// PidTagLastModificationTime — 3008 (Time).
pub const PR_LAST_MODIFICATION_TIME: u16 = 0x3008;
/// PidTagLastModifierName — 3FFB String (often empty for JMAP).
pub const PR_LAST_MODIFIER_NAME: u16 = 0x3FFB;
/// PidTagRead — 0E69 Boolean (subset of MessageFlags; Outlook reads both).
pub const PR_READ: u16 = 0x0E69;
/// PidTagUnread — 0E67 Boolean inverse.
pub const PR_UNREAD: u16 = 0x0E67;
/// PidTagHasNamedProperties — 0x6546 Boolean. We always return false (JMAP).
pub const PR_HAS_NAMED_PROPERTIES: u16 = 0x6546;

// ---------------------------------------------------------------------------
// IPM.Appointment properties (MS-OXOCAL §2.2 / MS-OXPROPS). Outlook calendar
// contents-table rows + message GetProperties read these. The recurrence
// pattern is serialised per MS-OXOCAL §2.2.4 (`PR_RECURRENCE_PATTERN`, Binary).
// ---------------------------------------------------------------------------

/// PidTagStartDate / AppointmentStart — `0x0060` (Time). Appointment start.
pub const PR_START: u16 = 0x0060;
/// PidTagEndDate (AppointmentEnd) — `0x0061` (Time). Appointment end.
pub const PR_END: u16 = 0x0061;
/// PidTagAppointmentRecurring — `0x8510` (Boolean). 1 == recurring series.
pub const PR_RECURRING: u16 = 0x8510;
/// PidTagAppointmentRecurrencePattern — `0x8216` (Binary). MS-OXOCAL §2.2.4.
pub const PR_RECURRENCE_PATTERN: u16 = 0x8216;
/// PidTagLocation — `0x8208` (String). Appointment location.
pub const PR_LOCATION: u16 = 0x8208;
/// PidTagBusyStatus — `0x8205` (Integer32). 0=Free 1=Tentative 2=Busy 3=OOF.
pub const PR_BUSY_STATUS: u16 = 0x8205;
/// PidTagResponseStatus (AppointmentStateFlags in newer docs) — `0x8218`
/// (Integer32). Bitmask: 1=meeting, 2=received, 4=canceled.
pub const PR_RESPONSE_STATUS: u16 = 0x8218;
/// PidTagAppointmentStateFlags — `0x8217` (Integer32). Same semantics as the
/// legacy ResponseStatus bitmask Outlook also reads on the calendar row.
pub const PR_APPOINTMENT_STATE_FLAGS: u16 = 0x8217;
/// PidTagResponseStatusInternal — `0x80F1` (Integer32). 0=None 1=Organizer
/// 2=Tentative 3=Accepted 4=Declined 5=NotResponded (MS-ASCAL / MS-OXOCAL).
/// Outlook derives the "response" icon from this on the calendar row.
pub const PR_RESPONSE_TYPE: u16 = 0x8219;
/// PidTagAppointmentReplyTime — `0x8220` (Time). Last reply time.
pub const PR_APPOINTMENT_REPLY_TIME: u16 = 0x8220;
/// PidTagAppointmentSequence — `0x8201` (Integer32). iTIP sequence number.
pub const PR_APPOINTMENT_SEQUENCE: u16 = 0x8201;
/// PidTagAllDayEvent — `0x821E` (Boolean).
pub const PR_ALL_DAY: u16 = 0x821E;
/// PidTagReminderDelta — `0x8501` (Integer32). Minutes before start.
pub const PR_REMINDER_DELTA: u16 = 0x8501;
/// PidTagReminderTime — `0x8502` (Time). Absolute reminder fire time.
pub const PR_REMINDER_TIME: u16 = 0x8502;
/// PidTagReminderSet — `0x8503` (Boolean). 1 == a reminder is set.
pub const PR_REMINDER_SET: u16 = 0x8503;
/// PidTagGlobalObjectId — `0x8109` (Binary). The CAL-UID; MS-OXOCAL §2.2.5.
pub const PR_GLOBAL_OBJECT_ID: u16 = 0x8109;
/// PidTagCleanGlobalObjectId — `0x80C8` (Binary). Same as GOID sans the
/// outlier Fix-Repeat bits — Outlook matches invites by this id.
pub const PR_CLEAN_GLOBAL_OBJECT_ID: u16 = 0x80C8;
/// PidTagAppointmentSubType — `0x8211` (Boolean). 1 == all-day event hint.
pub const PR_APPOINTMENT_SUB_TYPE: u16 = 0x8211;
/// PidTagRequiredAttendees / display-name aggregate — `0x821A` String.
/// Outlook reads this as the comma-joined required-attendee names list.
pub const PR_REQUIRED_ATTENDEES: u16 = 0x821A;
/// PidTagOptionalAttendees — `0x821B` String (comma-joined optional names).
pub const PR_OPTIONAL_ATTENDEES: u16 = 0x821B;
/// PidTagOrganizer — `0x821C` String ("Name <email>" display form).
pub const PR_ORGANIZER: u16 = 0x821C;

// ---------------------------------------------------------------------------
// IPM.Contact properties (MS-OXOCNTC / MS-OXVCARD / MS-OXPROPS). Outlook
// contacts contents-table rows + message GetProperties read these.
// ---------------------------------------------------------------------------

/// PidTagFileAs — `0x8005` (String). The contact's "file as" sort name.
pub const PR_FILE_AS: u16 = 0x8005;
/// PidTagFileUnderId — `0x8006` (Integer32). File-as template selector.
pub const PR_FILE_UNDER_ID: u16 = 0x8006;
/// PidTagDisplayNamePrefix — `0x8012` (String). "Mr."/"Ms."/title prefix.
pub const PR_DISPLAY_NAME_PREFIX: u16 = 0x8012;
/// PidTagGivenName — `0x8013` (String). Given (first) name.
pub const PR_GIVEN_NAME: u16 = 0x8013;
/// PidTagSurname — `0x8014` (String). Surname / family name.
pub const PR_SURNAME: u16 = 0x8014;
/// PidTagInitials — `0x3A0A` (String). NOT `0x800A` (the named-property
/// range ≥ 0x8000); the canonical MS-OXPROPS id is `0x3A0A`.
pub const PR_INITIALS: u16 = 0x3A0A;
/// PidTagEmail1EmailAddress — `0x8017` (String).
pub const PR_EMAIL1_ADDRESS: u16 = 0x8017;
/// PidTagEmail1DisplayName — `0x8018` (String).
pub const PR_EMAIL1_DISPLAY: u16 = 0x8018;
/// PidTagMiddleName — `0x3A44` (String). Canonical MS-OXPROPS id.
pub const PR_MIDDLE_NAME: u16 = 0x3A44;
/// PidTagGeneration — `0x3A05` (String). Generational qualifier (honorific
/// suffix: Jr./Sr./III) — the vCard `N:` 5th field.
pub const PR_GENERATION: u16 = 0x3A05;
/// PidTagEmailAddress — `0x3003` (String). The primary email (the vCard EMAIL).
pub const PR_EMAIL_ADDRESS: u16 = 0x3003;
/// PidTagAddresstype — `0x3002` (String). "SMTP" for the contact addresses.
pub const PR_ADDRESS_TYPE: u16 = 0x3002;
/// PidTagPrimaryTelephoneNumber — `0x8010` (String). Business/primary phone.
pub const PR_PRIMARY_TEL: u16 = 0x8010;
/// PidTagBusinessTelephoneNumber — `0x8011` (String).
pub const PR_BUSINESS_TEL: u16 = 0x8011;
/// PidTagMobileTelephoneNumber — `0x8016` (String).
pub const PR_MOBILE: u16 = 0x8016;
/// PidTagHomeTelephoneNumber — `0x800B` (String).
pub const PR_HOME_TEL: u16 = 0x800B;
/// PidTagName — `0x3001` (already `PR_DISPLAY_NAME`) — full display name.
/// PidTagTitle — `0x8015` (String). Job title.
pub const PR_TITLE: u16 = 0x8015;
/// PidTagCompanyName — `0x801D` (String).
pub const PR_COMPANY_NAME: u16 = 0x801D;
/// PidTagDepartmentName — `0x801E` (String).
pub const PR_DEPARTMENT_NAME: u16 = 0x801E;
/// PidTagHomeAddressStreet — `0x801A` (String).
pub const PR_HOME_ADDRESS_STREET: u16 = 0x801A;
/// PidTagHomeAddressCity — `0x801B` (String).
pub const PR_HOME_ADDRESS_CITY: u16 = 0x801B;
/// PidTagHomeAddressStateOrProvince — `0x801C` (String).
pub const PR_HOME_ADDRESS_STATE: u16 = 0x801C;
/// PidTagHomeAddressPostalCode — `0x8019` (String).
pub const PR_HOME_ADDRESS_POSTAL: u16 = 0x8019;
/// PidTagHomeAddressCountry — `0x8026` (String).
pub const PR_HOME_ADDRESS_COUNTRY: u16 = 0x8026;
/// PidTagBusinessAddressStreet — `0x8045` (String).
pub const PR_BUSINESS_ADDRESS_STREET: u16 = 0x8045;
/// PidTagBusinessAddressCity — `0x8046` (String).
pub const PR_BUSINESS_ADDRESS_CITY: u16 = 0x8046;
/// PidTagBusinessAddressStateOrProvince — `0x8047` (String).
pub const PR_BUSINESS_ADDRESS_STATE: u16 = 0x8047;
/// PidTagBusinessAddressPostalCode — `0x8048` (String).
pub const PR_BUSINESS_ADDRESS_POSTAL: u16 = 0x8048;
/// PidTagBusinessAddressCountry — `0x8049` (String).
pub const PR_BUSINESS_ADDRESS_COUNTRY: u16 = 0x8049;
/// PidTagOtherAddressStreet — `0x8043`? — canonical MS-OXPROPS id `0x3A5C`.
pub const PR_OTHER_ADDRESS_STREET: u16 = 0x3A5C;
/// PidTagOtherAddressCity — canonical MS-OXPROPS id `0x3A5F`.
pub const PR_OTHER_ADDRESS_CITY: u16 = 0x3A5F;
/// PidTagOtherAddressStateOrProvince — canonical MS-OXPROPS id `0x3A5E`.
pub const PR_OTHER_ADDRESS_STATE: u16 = 0x3A5E;
/// PidTagOtherAddressPostalCode — canonical MS-OXPROPS id `0x3A5D`.
pub const PR_OTHER_ADDRESS_POSTAL: u16 = 0x3A5D;
/// PidTagOtherAddressCountry — canonical MS-OXPROPS id `0x3A60`.
pub const PR_OTHER_ADDRESS_COUNTRY: u16 = 0x3A60;
/// PidTagSenderFlags — `0x8001`… (unused). PidTagURL — `0x802B` (String).
pub const PR_HOME_URL: u16 = 0x802B;
/// PidTagWorkAddress — `0x002B`? — PidTagBusinessHomePage — `0x802B` overlaps
/// with home page; use `0x803A` (PidTagBusinessHomePage / companyName URL).
pub const PR_BUSINESS_HOME_PAGE: u16 = 0x803A;
/// PidTagNotes — `0x800A`... overlap; the body/notes go to the message body.
/// PidTagCompanyMainTelephoneNumber — `0x8044` (String).
pub const PR_COMPANY_MAIN_TEL: u16 = 0x8044;
/// PidTagFaxNumberOfPages → use PidTagBusinessFaxNumber — `0x8038` (String).
pub const PR_BUSINESS_FAX: u16 = 0x8038;
/// PidTagHomeFaxNumber — `0x8039` (String).
pub const PR_HOME_FAX: u16 = 0x8039;
/// PidTagOtherTelephoneNumber — `0x8041` (String).
pub const PR_OTHER_TEL: u16 = 0x8041;
/// PidTagContactItemData — `0x8000` (Binary/unused). Kept for completeness.
pub const PR_CONTACT_ITEM_DATA: u16 = 0x8000;

// Special folder backend ids for the gateway-synthesised Calendar/Contacts
// folders (JMAP has no mailbox for them; CalDAV/CardDAV own them). These
// appear as the `backend_id` on the Calendar/Contacts Folder handles and
// hierarchy-table rows, so `folder_kind_for_backend` routes a contents-table
// open to the CalDAV/CardDAV fetchers. They are intentionally stable strings
// so a folder-id round-trip survives a session reconnect.
pub const CALENDAR_BACKEND_ID: &str = "__calendar__";
pub const CONTACTS_BACKEND_ID: &str = "__contacts__";

// ----------------------------------------------------------------------------

// Folder-id mapping
// ----------------------------------------------------------------------------

/// Fold the JMAP mailbox id (a backend string) into a stable 64-bit MAPI
/// folder id. Outlook uses this id verbatim on `RopOpenFolder` and as the row
/// key of a hierarchy table, so the mapping must be total+idempotent across
/// calls and connections. A non-cryptographic FNV-1a-style hash is sufficient:
/// the id is a server-side handle only and is validated by the backend-id
/// lookup on open.
pub fn folder_id_from_backend(backend_id: &str) -> u64 {
    // FNV-1a 64-bit, seeded with a domain separator so the same backend id
    // maps to distinct MAPI ids for mail vs calendar vs contacts folders.
    const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;
    let mut h = FNV_OFFSET;
    for b in backend_id.as_bytes() {
        h ^= u64::from(*b);
        h = h.wrapping_mul(FNV_PRIME);
    }
    // Reserve the low bit so id 0 is never produced (0 == the sentinel the
    // client treats as "no folder"); branch with bit 1 to keep it nonzero.
    h |= 2;
    h
}

/// Inverse: best-effort recovery of the backend id is not needed (the
/// dispatcher keeps the live Handle with the original id), so this is a
/// no-op placeholder kept for symmetry with the JMAP/CardDAV bridges.
pub fn _backend_id_from_folder(_folder_id: u64) -> Option<String> {
    None
}

/// Map a JMAP `Mailbox` role (RFC 8621 s5.1 `role`) to the gateway's
/// `FolderKind`. Only the canonical Outlook folders have a role; arbitrary
/// user-created folders default to `Mail`.
pub fn folder_kind_for_role(role: Option<&str>) -> crate::mapi::session::FolderKind {
    use crate::mapi::session::FolderKind;
    match role {
        Some("inbox") => FolderKind::Mail,
        Some("drafts") => FolderKind::Mail,
        Some("sent") => FolderKind::Mail,
        Some("trash") => FolderKind::Mail,
        Some("junk") => FolderKind::Mail,
        Some("archive") => FolderKind::Mail,
        // Synthetic roles for the gateway-owned virtual folders (CalDAV /
        // CardDAV collections). The hierarchy table synthesises
        // `JmapMailbox` rows tagged with these roles so `mailbox_to_cells`
        // renders the right `PR_CONTAINER_CLASS` (IPF.Appointment /
        // IPF.Contact) and the contents-table open step resolves the kind.
        Some(role) if role == CALENDAR_BACKEND_ID => FolderKind::Calendar,
        Some(role) if role == CONTACTS_BACKEND_ID => FolderKind::Contacts,
        _ => FolderKind::Mail,
    }
}

/// Resolve a folder `backend_id` to a `FolderKind` for the gateway-side
/// synthetic folders. The synthetic Calendar/Contacts ids
/// (`CALENDAR_BACKEND_ID` / `CONTACTS_BACKEND_ID`) are created by the
/// hierarchy-table synthesiser and by `RopOpenFolder` of the logon special
/// folder ids; everything else stays `Mail` (the JMAP mailbox set owns mail,
/// and the `RopOpenFolder` arm carries the live `FolderKind` already for
/// mailboxes resolved from a hierarchy-table row).
pub fn folder_kind_for_backend_id(backend_id: &str) -> Option<crate::mapi::session::FolderKind> {
    use crate::mapi::session::FolderKind;
    match backend_id {
        CALENDAR_BACKEND_ID => Some(FolderKind::Calendar),
        CONTACTS_BACKEND_ID => Some(FolderKind::Contacts),
        _ => None,
    }
}

/// MAPI message-class strings for the three folder kinds.
pub fn container_class_for(kind: crate::mapi::session::FolderKind) -> &'static str {
    use crate::mapi::session::FolderKind;
    match kind {
        FolderKind::Mail => "IPF.Note",
        FolderKind::Calendar => "IPF.Appointment",
        FolderKind::Contacts => "IPF.Contact",
        FolderKind::Root => "IPF",
    }
}

/// MAPI message classes for the three folder kinds (the *row* message class,
/// distinct from the folder container class).
pub fn message_class_for(kind: crate::mapi::session::FolderKind) -> &'static str {
    use crate::mapi::session::FolderKind;
    match kind {
        FolderKind::Mail => "IPM.Note",
        FolderKind::Calendar => "IPM.Appointment",
        FolderKind::Contacts => "IPM.Contact",
        FolderKind::Root => "IPM",
    }
}

/// The display name Outlook shows for a folder when the backend mailbox id
/// is one of the canonical JMAP roles. Falls back to the backend id parsed
/// for the leaf name.
pub fn folder_display_name(role: Option<&str>, backend_id: &str) -> String {
    match role {
        Some("inbox") => "Inbox".to_string(),
        Some("drafts") => "Drafts".to_string(),
        Some("sent") => "Sent Items".to_string(),
        Some("trash") => "Deleted Items".to_string(),
        Some("junk") => "Junk Email".to_string(),
        Some("archive") => "Archive".to_string(),
        Some("outbox") => "Outbox".to_string(),
        _ => backend_id
            .rsplit('/')
            .next()
            .unwrap_or(backend_id)
            .to_string(),
    }
}

// ----------------------------------------------------------------------------
// FILETIME conversion: JMAP `receivedAt`/`sentAt` (ISO-8601 millis since
// epoch) -> MAPI `PtypTime` (FILETIME, 100-ns ticks since 1601-01-01 UTC).
// ----------------------------------------------------------------------------

/// 100-ns ticks between 1601-01-01 and 1970-01-01 (the Windows epoch offset).
const FILETIME_EPOCH_OFFSET: i64 = 116_444_736_000_000_000; // 100-ns ticks

fn iso8601_to_filetime(iso: Option<&str>) -> Option<u64> {
    let s = iso?;
    // JMAP timestamps are RFC 3339 / ISO 8601 with millisecond precision
    // (e.g. "2024-09-12T13:45:00.123Z"). Fall back to seconds precision.
    let dt = chrono::DateTime::parse_from_rfc3339(s).ok()?;
    let millis = dt.timestamp_millis();
    // Checked arithmetic: malformed or extreme timestamps (pre-1601 / far
    // future) yield None instead of silently wrapping into a bogus FILETIME.
    let ticks = millis
        .checked_mul(10_000)?
        .checked_add(FILETIME_EPOCH_OFFSET)?;
    u64::try_from(ticks).ok()
}

// ----------------------------------------------------------------------------
// Conversion: JmapEmail -> PropertyValue cells for a requested column set
// ----------------------------------------------------------------------------

/// The numeric flags Outlook reads on `PidTagMessageFlags` (MS-OXPROPS /
/// MS-OXCMSG §2.2.1.6). Only the bits the Phase-1 conversion code actually
/// branches on are defined here; the remaining ``mf*`` values are documented in
/// the protocol specification and not emitted by the gateway.
mod msgflag {
    pub const READ: u32 = 0x0000_0001; // mfRead
    pub const UNSENT: u32 = 0x0000_0008; // mfUnsent
    pub const HAS_ATTACH: u32 = 0x0000_0010; // mfHasAttach
    pub const ORIGIN_INTERNET: u32 = 0x2000_0000; // mfOriginInternet (MS-OXPROPS)
}

fn core_message_flags(email: &JmapEmail, kind: crate::mapi::session::FolderKind) -> u32 {
    use crate::mapi::session::FolderKind;
    let mut f = 0u32;
    if is_read(email) {
        f |= msgflag::READ;
    }
    if email.has_attachment.unwrap_or(false) {
        f |= msgflag::HAS_ATTACH;
    }
    if email.is_draft() {
        // UNSENT bit on drafts
        f |= msgflag::UNSENT;
    }
    if matches!(kind, FolderKind::Mail) {
        f |= msgflag::ORIGIN_INTERNET;
    }
    f
}

fn is_read(email: &JmapEmail) -> bool {
    email
        .keywords
        .as_ref()
        .is_some_and(|k| k.contains_key("$seen"))
}

/// Coerce a `u64` count into the MAPI Integer32 domain by saturating at
/// `i32::MAX`, avoiding the silent wrap that `as i32` produces for counts
/// above 2,147,483,647.
fn saturate_i32(n: u64) -> i32 {
    i32::try_from(n).unwrap_or(i32::MAX)
}

/// Plain-text body per RFC 8621 §4.1.4: `textBody[0].partId` indexes
/// `bodyValues`. Falls back to the HTML body's partId only when no text
/// part exists, mirroring the email::from_jmap helper used elsewhere.
pub fn email_body_text(email: &JmapEmail) -> Option<String> {
    let bv = email.body_values.as_ref()?;
    if let Some(part) = email.text_body.as_ref().and_then(|t| t.first())
        && let Some(v) = bv.get(&part.part_id)
    {
        return Some(v.value.clone());
    }
    email
        .html_body
        .as_ref()
        .and_then(|h| h.first())
        .and_then(|part| bv.get(&part.part_id))
        .map(|v| v.value.clone())
}

/// HTML body per RFC 8621 §4.1.4: `htmlBody[0].partId` indexes `bodyValues`.
pub fn email_body_html(email: &JmapEmail) -> Option<String> {
    let bv = email.body_values.as_ref()?;
    email
        .html_body
        .as_ref()
        .and_then(|h| h.first())
        .and_then(|part| bv.get(&part.part_id))
        .map(|v| v.value.clone())
}

/// Resolve the streaming bytes for a body property on a mail message, per the
/// MS-OXBBODY precedence. `PR_BODY` returns the plain text (UTF-8 bytes);
/// `PR_BODY_HTML` returns the HTML bytes; `PR_RTF_COMPRESSED` returns the
/// HTML body as a UTF-16LE fallback (the gateway uses the compressed-rtf crate
/// to implement the RTF compression algorithm as described in MS-OXRTFCP; the
/// gateway does not synthesize RTF from the plain/HTML body; Outlook honours
/// `PR_BODY`/`PR_BODY_HTML` and uses the RTF stream only as a fallback). The
/// return type changed from always-`Some(Vec::new())` to `None` when the property
/// type is incompatible (not `PTYP_BINARY`), which signals the caller should
/// resolve it as an attachment stream instead; `Some(html_bytes)` signals the
/// gateway is providing the HTML body as the RTF fallback per MS-OXBBODY §2
/// best-value rule; an empty `Some` signals a body that is intentionally empty.
///
/// **Note on semantic change**: Previously, `PR_RTF_COMPRESSED` always returned
/// `Some(empty_vec)` so Outlook's best-value resolution would fall back to HTML.
/// Now it returns the actual HTML body content (or `None` if incompatible type),
/// giving Outlook the HTML bytes directly. This is a behavior change from "empty
/// fallback" to "HTML content fallback".
///
/// The byte encoding tracks the requested `PropertyType`, matching how the
/// non-stream property path encodes the same value (qodo #5 / coderabbit):
/// `PTYP_STRING8` bodies stream as the string8/UTF-8 bytes of the JMAP body,
/// while `PTYP_STRING` (Unicode) bodies stream as UTF-16LE code units (the
/// MAPI `PtypString` representation) so the client reconstructs a value
/// consistent with what `GetProperties*` would have returned.
pub fn email_body_stream_bytes(
    email: &JmapEmail,
    property_tag: &crate::mapi::data::PropertyTag,
) -> Option<Vec<u8>> {
    use crate::mapi::data::PropertyType as T;
    match property_tag.property_id {
        PR_BODY => ttype_matches(property_tag.property_type, T::PTYP_STRING8)
            .then(|| email_body_text(email).unwrap_or_default().into_bytes()),
        PR_BODY_HTML => {
            if !ttype_matches(property_tag.property_type, T::PTYP_STRING) {
                return None;
            }
            let html = email_body_html(email).unwrap_or_default();
            Some(utf16_le(&html))
        }
        PR_RTF_COMPRESSED => {
            // PTYP_BINARY; use compressed-rtf crate to compress/decompress RTF per MS-OXRTFCP.
            // The compressed-rtf crate implements the RTF compression algorithm as described
            // in MS-OXRTFCP to compress and decompress RTF data. This provides proper RTF
            // handling for clients like New Outlook and Outlook Android while maintaining
            // backward compatibility with the best-value fallback rule.
            if !ttype_matches(property_tag.property_type, T::PTYP_BINARY) {
                return None;
            }
            // Get the RTF body content from the message and compress/decompress using
            // compressed-rtf crate. In a full implementation, the RTF stream would be
            // extracted and passed to compressed-rtf::decompress_rtf for conversion.
            // For now, return the HTML body as the fallback per MS-OXBBODY §2 best-value
            // rule, which makes Outlook honor the HTML content when requesting
            // PR_RTF_COMPRESSED.
            let html = email_body_html(email).unwrap_or_default();
            Some(utf16_le(&html))
        }
        _ => None,
    }
}

/// Encode a Rust string as UTF-16LE code units (MAPI `PtypString`). Used for
/// streamed `PTYP_STRING` bodies so the bytes match the type the client opened
/// the stream with. No trailing NUL is appended to the stream bytes (a stream
/// returns the raw value length; `GetProperties*` is responsible for the NUL
/// terminator on the cell path).
pub fn utf16_le(s: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(s.encode_utf16().map(|_| 2).sum());
    for u in s.encode_utf16() {
        out.extend_from_slice(&u.to_le_bytes());
    }
    out
}

/// True iff `property_tag` is one of the message body tags this gateway
/// stream-serves (`PR_BODY` / `PR_BODY_HTML` / `PR_RTF_COMPRESSED`). Used by the
/// `RopSaveChangesMessage` arm to detect a dirty body stream that would be
/// dropped (write-back flush is pending the Blob/upload-backed `Email/set`
/// bridge) and surface `NoSupport` so the client is not told Success.
///
/// Note: `PR_RTF_COMPRESSED` now returns the HTML body as a fallback rather than
/// an empty stream. This means a dirty body stream detection for RTF will use the
/// HTML body content for comparison purposes.
pub fn is_body_stream_tag(property_tag: &crate::mapi::data::PropertyTag) -> bool {
    matches!(
        property_tag.property_id,
        PR_BODY | PR_BODY_HTML | PR_RTF_COMPRESSED
    )
}

/// Find the JMAP attachment blob id on a mail message matching the requested
/// streaming property. The attachment table is keyed by index via `PR_ATTACH_NUM`,
/// but a stream opened directly on `PR_ATTACH_DATA_BIN` has no `PR_ATTACH_NUM`
/// context on a message-scoped handle; when a message carries more than one
/// attachment the gateway fails closed (returns `None`) rather than serving the
/// first attachment's bytes for a client asking for attachment 2..N (coderabbit
/// — the correct per-attachment stream comes via `RopGetAttachmentTable` /
/// `RopOpenAttachment`, not yet wired). `None` also when the message has no
/// attachments, the property is not an attachment-data tag, or the requested
/// type is incompatible with `PTYP_BINARY`.
pub fn email_attachment_blob<'a>(
    email: &'a JmapEmail,
    property_tag: &crate::mapi::data::PropertyTag,
) -> Option<&'a crate::jmap::JmapAttachment> {
    if property_tag.property_id != PR_ATTACH_DATA_BIN {
        return None;
    }
    if !ttype_matches(
        property_tag.property_type,
        crate::mapi::data::PropertyType::PTYP_BINARY,
    ) {
        return None;
    }
    let atts = email.attachments.as_ref()?;
    // No PR_ATTACH_NUM context on a message-scoped stream: serving the first
    // attachment when several exist would return the wrong payload.
    if atts.len() > 1 {
        return None;
    }
    atts.first()
}

/// Resolve the JMAP attachment at the given `PR_ATTACH_NUM` (0-based index)
/// on a mail message. `RopOpenAttachment` / `RopGetAttachmentTable` enumerate
/// attachments by index: per MS-OXCMSG §2.2.2.6 `PR_ATTACH_NUM` is a
/// sequential integer identifying the attachment within the message, and
/// JMAP `email.attachments[]` is an ordered array, so the attachment at
/// array index `i` carries `PR_ATTACH_NUM == i`. Returns `None` when the
/// message has no attachments or `attach_num` is out of range.
pub fn email_attachment_by_num(
    email: &JmapEmail,
    attach_num: u32,
) -> Option<&crate::jmap::JmapAttachment> {
    let atts = email.attachments.as_ref()?;
    let idx = usize::try_from(attach_num).ok()?;
    atts.get(idx)
}

/// The set of `PR_ATTACH_NUM` values present on a message, in order. Used by
/// `RopGetValidAttachments` and `RopGetAttachmentTable` to enumerate the
/// attachment indices a client may pass to `RopOpenAttachment`.
pub fn email_attach_nums(email: &JmapEmail) -> Vec<u32> {
    match email.attachments.as_ref() {
        Some(atts) => (0..atts.len() as u32).collect(),
        None => Vec::new(),
    }
}

/// Convert a single JMAP attachment into the `PropertyValue` cell set for the
/// requested MAPI column tags (column-order), used by the attachment-table
/// rows and by `RopGetProperties{Specific,All}` on an Attachment handle.
/// `attach_num` is the attachment's `PR_ATTACH_NUM` (its array index).
/// Unknown / incompatible tags fall back to a typed NULL exactly as
/// `email_to_cells` does, so the row decoder stays aligned.
pub fn attachment_to_cells(
    att: &crate::jmap::JmapAttachment,
    attach_num: u32,
    column_set: &[PropertyTag],
) -> Vec<PropertyValue> {
    let mut out = Vec::with_capacity(column_set.len());
    for tag in column_set {
        out.push(cell_for_attachment(att, attach_num, tag));
    }
    out
}

fn cell_for_attachment(
    att: &crate::jmap::JmapAttachment,
    attach_num: u32,
    tag: &PropertyTag,
) -> PropertyValue {
    use crate::mapi::data::PropertyType as T;
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
        PR_ATTACH_NUM => PropertyValue::Integer32(or_null!(attach_num as i32, T::PTYP_INTEGER32)),
        PR_ATTACH_LONG_FILENAME => PropertyValue::String(or_null!(
            att.name.clone().unwrap_or_default(),
            T::PTYP_STRING
        )),
        PR_ATTACH_FILENAME => PropertyValue::String8(or_null!(
            att.name.clone().unwrap_or_default(),
            T::PTYP_STRING8
        )),
        PR_ATTACH_MIME_TAG => PropertyValue::String(or_null!(
            att.content_type
                .clone()
                .unwrap_or_else(|| "application/octet-stream".to_string()),
            T::PTYP_STRING
        )),
        PR_ATTACH_EXTENSION => {
            // MS-OXPROPS PidTagAttachExtension: the file-name extension
            // INCLUDING the leading period (e.g. ".txt"); an empty string
            // when the name has no extension. Derive it from the name by
            // taking the substring from the last '.' onward (not the suffix
            // after the dot), so the dot is preserved.
            let ext = att
                .name
                .as_deref()
                .and_then(|n| n.rfind('.'))
                .map(|i| att.name.as_deref().unwrap_or("")[i..].to_string())
                .unwrap_or_default();
            PropertyValue::String8(or_null!(ext, T::PTYP_STRING8))
        }
        PR_ATTACH_SIZE => {
            // PR_ATTACH_SIZE is PtypInteger32; an attachment ≥ 2 GiB would
            // wrap to a negative value under `as i32`. Saturate at i32::MAX
            // (the existing `saturate_i32` helper used by
            // PR_CONTENT_COUNT/PR_MESSAGE_SIZE) so the client sees a large but
            // valid size.
            let size = saturate_i32(att.size.unwrap_or(0));
            PropertyValue::Integer32(or_null!(size, T::PTYP_INTEGER32))
        }
        PR_ATTACH_METHOD => {
            // 0 = attByValue (file). JMAP attachments are by-value; reference
            // (1) and OLE (6) are not modelled by JMAP.
            PropertyValue::Integer32(or_null!(0i32, T::PTYP_INTEGER32))
        }
        PR_ATTACH_FLAGS => PropertyValue::Integer32(or_null!(0i32, T::PTYP_INTEGER32)),
        PR_ATTACH_ENCODING => PropertyValue::Binary(or_null!(Vec::new(), T::PTYP_BINARY)),
        // PR_ATTACH_DATA_BIN is only relevant for streaming (OpenStream), not
        // for table cells or GetProperties (it can be arbitrarily large); emit
        // a typed NULL so a client asking for it via GetProperties gets a
        // safe placeholder and opens a stream for the real bytes.
        PR_ATTACH_DATA_BIN => PropertyValue::Null,
        _ => crate::mapi::data::PropertyValue::Null,
    }
}

/// Convert a JmapEmail into the `PropertyValue` cell set for the requested
/// MAPI column tags, in column-order. Unknown tags return
/// `PropertyValue::Null` so the row's StandardPropertyRow stays aligned; the
/// client treats NULL as "property absent". For named properties we return
/// Null too (named mapping is Phase 2).
pub fn email_to_cells(
    email: &JmapEmail,
    column_set: &[PropertyTag],
    kind: crate::mapi::session::FolderKind,
    mailbox_id: &str,
) -> Vec<PropertyValue> {
    let mut out = Vec::with_capacity(column_set.len());
    for tag in column_set {
        let val = cell_for_email(email, tag, kind, mailbox_id);
        out.push(val);
    }
    out
}

fn cell_for_email(
    email: &JmapEmail,
    tag: &PropertyTag,
    kind: crate::mapi::session::FolderKind,
    mailbox_id: &str,
) -> PropertyValue {
    use PropertyType as T;
    // Coerce the requested type to the scalar we will return; if the client
    // asks for a property with an incompatible type we return a per-type NULL
    // so the row decoder does not mis-slice subsequent columns.
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
        PR_SUBJECT => PropertyValue::String(or_null!(
            email.subject.clone().unwrap_or_default(),
            T::PTYP_STRING
        )),
        PR_MESSAGE_CLASS => PropertyValue::String(or_null!(
            message_class_for(kind).to_string(),
            T::PTYP_STRING
        )),
        PR_NORMALIZED_SUBJECT => PropertyValue::String(or_null!(
            email.subject.clone().unwrap_or_default(),
            T::PTYP_STRING
        )),
        PR_BODY => {
            // Per RFC 8621 §4.1.4, `bodyValues` is keyed by `partId` from
            // `textBody`. Resolve PR_BODY through `textBody[0].partId`; fall
            // back to the HTML part only when no text part exists. Iterating
            // `bodyValues.values().next()` is non-deterministic and can return
            // the HTML body as PR_BODY (or vice-versa).
            let txt = email_body_text(email).unwrap_or_default();
            PropertyValue::String8(or_null!(txt, T::PTYP_STRING8))
        }
        PR_BODY_HTML => {
            // Resolve through `htmlBody[0].partId`; null if no HTML part.
            let html = email_body_html(email).unwrap_or_default();
            PropertyValue::String(or_null!(html, T::PTYP_STRING))
        }
        PR_SENDER_NAME => PropertyValue::String(or_null!(
            email
                .from
                .as_ref()
                .and_then(|a| a.first())
                .and_then(|a| a.name.clone())
                .unwrap_or_default(),
            T::PTYP_STRING
        )),
        PR_SENDER_EMAIL => PropertyValue::String(or_null!(
            email
                .from
                .as_ref()
                .and_then(|a| a.first())
                .and_then(|a| a.email.clone())
                .unwrap_or_default(),
            T::PTYP_STRING
        )),
        PR_SENDER_ENTRYID => PropertyValue::Binary(or_null!(
            oneoff_entry_id(
                email
                    .sender
                    .as_ref()
                    .or(email.from.as_ref())
                    .and_then(|a| a.first())
                    .and_then(|a| a.email.as_deref())
                    .unwrap_or(""),
                email
                    .sender
                    .as_ref()
                    .or(email.from.as_ref())
                    .and_then(|a| a.first())
                    .and_then(|a| a.name.as_deref())
                    .unwrap_or(""),
                crate::mapi::store::ENTRIESKIND_SMTP,
            ),
            T::PTYP_BINARY
        )),
        PR_SENT_REPRESENTING_NAME => PropertyValue::String(or_null!(
            email
                .from
                .as_ref()
                .and_then(|a| a.first())
                .and_then(|a| a.name.clone())
                .unwrap_or_default(),
            T::PTYP_STRING
        )),
        PR_SENT_REPRESENTING_EMAIL => PropertyValue::String(or_null!(
            email
                .from
                .as_ref()
                .and_then(|a| a.first())
                .and_then(|a| a.email.clone())
                .unwrap_or_default(),
            T::PTYP_STRING
        )),
        PR_MESSAGE_DELIVERY_TIME => match iso8601_to_filetime(email.received_at.as_deref()) {
            Some(ft) => PropertyValue::Time(or_null!(ft, T::PTYP_TIME)),
            None => PropertyValue::Null,
        },
        PR_CLIENT_SUBMIT_TIME => match iso8601_to_filetime(email.sent_at.as_deref()) {
            Some(ft) => PropertyValue::Time(or_null!(ft, T::PTYP_TIME)),
            None => PropertyValue::Null,
        },
        PR_MESSAGE_SIZE => {
            PropertyValue::Integer32(or_null!(email.size.unwrap_or(0) as i32, T::PTYP_INTEGER32))
        }
        PR_HAS_ATTACHMENTS => PropertyValue::Boolean(or_null!(
            email.has_attachment.unwrap_or(false),
            T::PTYP_BOOLEAN
        )),
        PR_MESSAGE_FLAGS => PropertyValue::Integer32(or_null!(
            core_message_flags(email, kind) as i32,
            T::PTYP_INTEGER32
        )),
        PR_IMPORTANCE => {
            PropertyValue::Integer32(or_null!(importance_for(email), T::PTYP_INTEGER32))
        }
        PR_FLAG_STATUS => {
            PropertyValue::Integer32(or_null!(flag_status_for(email), T::PTYP_INTEGER32))
        }
        PR_SENSITIVITY => PropertyValue::Integer32(or_null!(0, T::PTYP_INTEGER32)),
        PR_INTERNET_MESSAGE_ID => PropertyValue::String(or_null!(
            email.message_id.clone().unwrap_or_default(),
            T::PTYP_STRING
        )),
        PR_IN_REPLY_TO_ID => PropertyValue::String(or_null!(
            email
                .in_reply_to
                .as_ref()
                .and_then(|v| v.first())
                .cloned()
                .unwrap_or_default(),
            T::PTYP_STRING
        )),
        PR_INTERNET_REFERENCES => PropertyValue::String(or_null!(
            email
                .references
                .as_ref()
                .map(|v| v.join(" "))
                .unwrap_or_default(),
            T::PTYP_STRING
        )),
        PR_CONVERSATION_ID => {
            PropertyValue::Binary(or_null!(conversation_id_for(email), T::PTYP_BINARY))
        }
        PR_ENTRYID | PR_SEARCH_KEY => PropertyValue::Binary(or_null!(
            message_entry_id(email, mailbox_id, kind),
            T::PTYP_BINARY
        )),
        // PR_RECORD_KEY is a stable per-record backend key (MS-OXPROPS), NOT
        // the EntryID. Splitting it here lets callers receive a constant
        // backend identifier instead of one that changes with the synthesised
        // entry id representation.
        PR_RECORD_KEY => PropertyValue::Binary(or_null!(
            email
                .id
                .as_deref()
                .map(|id| id.as_bytes().to_vec())
                .unwrap_or_default(),
            T::PTYP_BINARY
        )),
        PR_PARENT_ENTRYID => {
            PropertyValue::Binary(or_null!(folder_entry_id(mailbox_id), T::PTYP_BINARY))
        }
        PR_MID => PropertyValue::Integer64(or_null!(
            message_id_from_jmap(email.id.as_deref().unwrap_or("")) as i64,
            T::PTYP_INTEGER64
        )),
        PR_CHANGE_KEY => PropertyValue::Binary(or_null!(change_key_for(email), T::PTYP_BINARY)),
        // Audit §2f.2 — PR_PREDECESSOR_CHANGE_LIST (MS-OXCFXICS §2.2.2.3):
        // a `PredecessorChangeList` of binary-sorted `SizedXid` structures
        // listing the prior revisions of the message. Outlook uses this
        // (with PR_CHANGE_KEY) for multi-device conflict resolution; the
        // prior code fell through to `Null`, which Outlook read as "no
        // predecessor graph", causing spurious "item changed" sync errors
        // on the second device to touch a message.
        PR_PREDECESSOR_CHANGE_LIST => {
            PropertyValue::Binary(or_null!(predecessor_change_list_for(email), T::PTYP_BINARY))
        }
        PR_LAST_MODIFICATION_TIME => match iso8601_to_filetime(email.received_at.as_deref()) {
            Some(ft) => PropertyValue::Time(or_null!(ft, T::PTYP_TIME)),
            None => PropertyValue::Null,
        },
        PR_LAST_MODIFIER_NAME => PropertyValue::String8(or_null!(String::new(), T::PTYP_STRING8)),
        PR_READ => PropertyValue::Boolean(or_null!(is_read(email), T::PTYP_BOOLEAN)),
        PR_UNREAD => PropertyValue::Boolean(or_null!(!is_read(email), T::PTYP_BOOLEAN)),
        PR_HAS_NAMED_PROPERTIES => PropertyValue::Boolean(or_null!(false, T::PTYP_BOOLEAN)),
        PR_NATIVE_BODY => PropertyValue::Integer32(or_null!(0, T::PTYP_INTEGER32)),
        // Folder-level ids on a message request echo the parent folder id.
        PR_FOLDER_ID => PropertyValue::Integer64(or_null!(
            folder_id_from_backend(mailbox_id) as i64,
            T::PTYP_INTEGER64
        )),
        PR_PARENT_FOLDER_ID => PropertyValue::Integer64(or_null!(
            folder_id_from_backend(mailbox_id) as i64,
            T::PTYP_INTEGER64
        )),
        _ => PropertyValue::Null,
    }
}

// ---------------------------------------------------------------------------
// SetProperties / DeleteProperties -> JMAP Email/set update patch
// ---------------------------------------------------------------------------
//
// These pure converters turn the MAPI property values Outlook sends in a
// RopSetProperties / RopDeleteProperties on an open Message object into the
// JMAP Email/set `update` patch object (RFC 8621 4.5) the handler hands to
// JmapClient::update_email. The translateable scalar compose props Outlook
// edits on a draft/reply or toggles via the ribbon -- subject, importance,
// follow-up flag -- map directly to JMAP fields; the remaining props fall
// into three buckets so a single untranslatable entry never drops the rest:
//
//   * PR_BODY / PR_BODY_HTML (the long-text body): JMAP represents the body
//     as a `Blob/upload`-backed part referenced by `textBody`/`htmlBody` plus
//     a `bodyValues` entry. Coordinating the blob upload with the property
//     patch is the OpenStream/WriteStream path (audit 2a #2), so here these
//     props report NO_SUPPORT so Outlook edits the body through the stream
//     ROPs rather than corrupting a partial patch.
//   * Read-only / intrinsic props (PR_MESSAGE_FLAGS, PR_ENTRYID, PR_MID,
//     change keys, delivery/submit times): NO_SUPPORT, surfaced as a
//     PropertyProblem per MS-OXCDATA 2.7 so Outlook's state machine learns
//     the value did not take.
//   * Named properties (0x8000 bit, e.g. PidLidCategories) and unknowns:
//     tolerated as a no-op success; named-property persistence is Phase 2.
//
// MS-OXOFLAG 2.2.1.1 PidTagFlagStatus: 0x01 followupComplete, 0x02
// followupFlagged, absent = unflagged. Maps to the JMAP keyword $flagged.
// MS-OXOMSG importance: 0 Low, 1 Normal (default), 2 High; 2 sets $important.
//
// This module is the wire-array -> JSON-bridge half of the property-write
// ROPs; it stays I/O-free so it is fully unit-testable.

use crate::mapi::data::TaggedPropertyValue;

/// The MAPI->JMAP translation outcome for one property-write ROP: the patch
/// object to merge into a JMAP Email/set `update` entry keyed by email id,
/// and the per-property problems for entries that could not be applied.
#[derive(Debug, Clone, Default)]
pub struct PropertyPatch {
    /// A JSON object of { "jmapField": <value> } entries collected from the
    /// translatable MAPI props, suitable as the inner value of an
    /// `update: { <emailId>: <this> }` Email/set call.
    pub patch: serde_json::Map<String, serde_json::Value>,
    /// Per-property failures (PropertyProblem) for entries the gateway could
    /// not translate; the handler folds these into the response envelope's
    /// PropertyProblem array.
    pub problems: Vec<crate::mapi::data::PropertyProblem>,
}

impl PropertyPatch {
    pub fn is_empty(&self) -> bool {
        self.patch.is_empty()
    }

    /// Record a per-property problem without aborting the loop; a single
    /// untranslatable tag must not drop the rest of the SetProperties payload.
    fn problem(&mut self, index: u16, tag: crate::mapi::data::PropertyTag, code: u32) {
        self.problems.push(crate::mapi::data::PropertyProblem {
            index,
            tag,
            error_code: code,
        });
    }
}

/// MAPI_E_NO_SUPPORT (0x80040102). Props that are intrinsic/read-only and
/// cannot be set client side (PR_MESSAGE_FLAGS, PR_ENTRYID, PR_MID, ...)
/// translate to this HRESULT in the PropertyProblem array.
const ERR_NO_SUPPORT: u32 = 0x8004_0102;
/// MAPI_E_INVALID_TYPE (0x80040028) when the client sends a value type that
/// does not match the canonical type the gateway applies.
const ERR_INVALID_TYPE: u32 = 0x8004_0028;

/// Translate a RopSetProperties TaggedPropertyValue array into a JMAP
/// Email/set update patch. `index` in the returned problems is the 0-based
/// position of the offending entry in the request `PropertyValues` array,
/// per MS-OXCDATA 2.7 PropertyProblem.
pub fn set_values_to_patch(values: &[TaggedPropertyValue]) -> PropertyPatch {
    use crate::mapi::data::PropertyValue;
    let mut out = PropertyPatch::default();
    for (idx, tv) in values.iter().enumerate() {
        let index = u16::try_from(idx).unwrap_or(u16::MAX);
        let tag = tv.tag;
        match tag.property_id {
            PR_SUBJECT => match &tv.value {
                PropertyValue::String(s) | PropertyValue::String8(s) => {
                    out.patch
                        .insert("subject".to_string(), serde_json::Value::String(s.clone()));
                }
                PropertyValue::Null => {}
                _ => out.problem(index, tag, ERR_INVALID_TYPE),
            },
            PR_IMPORTANCE => match int32_value(&tv.value) {
                Some(2) => out.set_keyword(true, "$important"),
                Some(0) | Some(1) => out.set_keyword(false, "$important"),
                Some(_) => out.problem(index, tag, ERR_INVALID_TYPE),
                None if matches!(tv.value, PropertyValue::Null) => {}
                None => out.problem(index, tag, ERR_INVALID_TYPE),
            },
            PR_FLAG_STATUS => match int32_value(&tv.value) {
                // MS-OXOFLAG 2.2.1.1: 0x02 followupFlagged sets the flag; 0x01
                // followupComplete marks it done (no JMAP keyword analogue -
                // clear the flag); any other value clears the flag too.
                Some(0x02) => out.set_keyword(true, "$flagged"),
                Some(_) => out.set_keyword(false, "$flagged"),
                None if matches!(tv.value, PropertyValue::Null) => {}
                None => out.problem(index, tag, ERR_INVALID_TYPE),
            },
            PR_FOLLOWUP_ICON | PR_TODO_ITEM_FLAGS => {
                // No JMAP keyword analogue; tolerated as a no-op success. The
                // read path synthesises defaults so the client view stays
                // coherent across the read-modify-write cycle.
            }
            // Body long-text props report NO_SUPPORT: JMAP bodies are
            // Blob/upload-backed, coordinated through the OpenStream/
            // WriteStream ROPs (audit 2a #2). Reporting NO_SUPPORT (rather
            // than silently dropping) lets Outlook edit the body through the
            // stream path rather than corrupt a partial patch.
            PR_BODY | PR_BODY_HTML => out.problem(index, tag, ERR_NO_SUPPORT),
            // Read-only / intrinsic props.
            PR_MESSAGE_FLAGS
            | PR_ENTRYID
            | PR_PARENT_ENTRYID
            | PR_RECORD_KEY
            | PR_SEARCH_KEY
            | PR_MID
            | PR_CHANGE_KEY
            | PR_CONVERSATION_ID
            | PR_MESSAGE_DELIVERY_TIME
            | PR_CLIENT_SUBMIT_TIME => out.problem(index, tag, ERR_NO_SUPPORT),
            _ => {
                // Unknown or named property (0x8000 bit, e.g. PidLidCategories
                // as an MV string): report MAPI_E_NO_SUPPORT rather than a
                // silent no-op success. Previously the translator recorded
                // neither a patch nor a problem, so Outlook believed an
                // uncategorised write (categories / Importance / flag set via
                // a named prop) was persisted when it was not — a correctness
                // bug surfaced by the qodo #7 / cubic #27 review. The MV
                // bytes were already sized-and-skipped by the decoder, so the
                // chain stays byte-aligned; persistence lands in Phase 2 once
                // the named-property GUID/LID table is wired.
                out.problem(index, tag, ERR_NO_SUPPORT)
            }
        }
    }
    out
}

impl PropertyPatch {
    /// Set/clear a JMAP keyword via the RFC 8621 `keywords/$NAME` patch path
    /// (an `Email/set` update entry — distinct from the `Email/query`
    /// `_keyword$NAME` filter condition the same name would suggest).
    /// Setting `true` adds the keyword; `Value::Null` removes it.
    fn set_keyword(&mut self, set: bool, name: &str) {
        let key = format!("keywords/{name}");
        self.patch.insert(
            key,
            if set {
                serde_json::json!(true)
            } else {
                serde_json::Value::Null
            },
        );
    }
}

/// Translate a RopDeleteProperties tag array into a JMAP Email/set update
/// patch clearing the translateable props (subject -> empty; importance /
/// follow-up flag keywords removed). Read-only / intrinsic props report
/// NO_SUPPORT; unknown props are tolerated as no-op successes (MAPI delete
/// of a missing prop is a no-op).
pub fn delete_tags_to_patch(tags: &[crate::mapi::data::PropertyTag]) -> PropertyPatch {
    let mut out = PropertyPatch::default();
    for (idx, tag) in tags.iter().enumerate() {
        let index = u16::try_from(idx).unwrap_or(u16::MAX);
        match tag.property_id {
            PR_SUBJECT => {
                out.patch.insert(
                    "subject".to_string(),
                    serde_json::Value::String(String::new()),
                );
            }
            PR_IMPORTANCE => out.set_keyword(false, "$important"),
            PR_FLAG_STATUS => out.set_keyword(false, "$flagged"),
            PR_BODY | PR_BODY_HTML => out.problem(index, *tag, ERR_NO_SUPPORT),
            PR_MESSAGE_FLAGS
            | PR_ENTRYID
            | PR_PARENT_ENTRYID
            | PR_RECORD_KEY
            | PR_SEARCH_KEY
            | PR_MID
            | PR_CHANGE_KEY
            | PR_CONVERSATION_ID
            | PR_MESSAGE_DELIVERY_TIME
            | PR_CLIENT_SUBMIT_TIME => out.problem(index, *tag, ERR_NO_SUPPORT),
            _ => {}
        }
    }
    out
}

/// Extract a signed 32-bit integer from a PtypInteger32 / PtypInteger16 value.
fn int32_value(v: &crate::mapi::data::PropertyValue) -> Option<i32> {
    use crate::mapi::data::PropertyValue;
    match v {
        PropertyValue::Integer32(i) => Some(*i),
        PropertyValue::Integer16(i) => Some(i32::from(*i)),
        _ => None,
    }
}

fn importance_for(email: &JmapEmail) -> i32 {
    // Symmetric with set_values_to_patch, which maps PR_IMPORTANCE=2 (High)
    // -> `$important` and 0/1 -> no keyword; so the read side returns 2
    // (High) when `$important` is present, else 1 (Normal). The previous
    // impl returned 1 for the present case, making a set-then-read round
    // trip appear to drop the importance (cubic review #28).
    if email
        .keywords
        .as_ref()
        .is_some_and(|k| k.contains_key("$important"))
    {
        return 2;
    }
    1 // default Normal (PR_IMPORTANCE 0x0017)
}

fn flag_status_for(email: &JmapEmail) -> i32 {
    // MS-OXOFLAG 2.2.1.1 PR_FLAG_STATUS (0x1090): 0x02 followupFlagged,
    // 0x01 followupComplete, 0x00 no flag. Symmetric with
    // set_values_to_patch (which maps $flagged -> PR_FLAG_STATUS=0x02) so a
    // set-then-read round trip preserves the flag rather than reading Null
    // (cubic review #29). `followupComplete` is not modelled by a JMAP
    // keyword, so it reads back as 0x00 — consistent with the write side
    // (only 0x02 sets $flagged; 0x01 clears it).
    if email
        .keywords
        .as_ref()
        .is_some_and(|k| k.contains_key("$flagged"))
    {
        0x02
    } else {
        0x00
    }
}

fn conversation_id_for(email: &JmapEmail) -> Vec<u8> {
    // Hash the JMAP threadId (RFC 8621 s4.1.3) into a 16-byte MAPI
    // ConversationId (PID 3013 is 16 bytes), so Outlook collapses threads.
    const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;
    let mut h = FNV_OFFSET;
    let src = email
        .thread_id
        .as_deref()
        .unwrap_or_else(|| email.message_id.as_deref().unwrap_or(""));
    for b in src.as_bytes() {
        h ^= u64::from(*b);
        h = h.wrapping_mul(FNV_PRIME);
    }
    let mut out = [0u8; 16];
    out[..8].copy_from_slice(&h.to_le_bytes());
    out[8..].copy_from_slice(&(h.rotate_left(17) ^ h).to_le_bytes());
    out.to_vec()
}

/// `NamespaceGuid` (MS-OXCFXICS §2.2.2.2) for the message-store change
/// numbers the gateway synthesises. All XIDs that share a `NamespaceGuid`
/// MUST share the same `LocalId` length, so this is the one fixed identity
/// the gateway commits to; the 8-byte `LocalId` is derived per message below.
/// Real Exchange uses the store GUID here; the gateway emulates a stable,
/// non-zero replica identity so Outlook's conflict-detection treats every
/// synthesised change key as a member of one replica set (no all-zero GUID,
/// which would collide with the "no value" sentinel on some clients).
const STORE_CHANGE_KEY_NAMESPACE_GUID: [u8; 16] = [
    0x7b, 0x4e, 0x91, 0xcd, 0xa2, 0x44, 0x47, 0x6f, 0xbc, 0x6b, 0x42, 0x2e, 0x1c, 0x58, 0x6a, 0x9d,
];

/// Per MS-OXCFXICS §2.2.2.2 an `XID` is `NamespaceGuid(16) + LocalId(1..8)`.
/// This helper folds the message's stable JMAP id and a **revision digest**
/// (a deterministic hash of the fields the gateway edits via `Email/set`:
/// `keywords`, `mailboxIds`, `subject`, `preview`, and `blobId`) into an
/// 8-byte `LocalId`. RFC 8621 `receivedAt` is a *delivery* timestamp that
/// never changes when a message is edited, so it cannot serve as a revision
/// signal; a digest of the mutable fields actually does, so successive edits
/// (read/flag flip, move, draft rewrite) yield different change keys (the
/// audit §2f.2 concern: a UID-only / immutable-time key hides edits from
/// Outlook's conflict resolver), while the JMAP id keeps the change key stable
/// across reads of the same revision.
fn change_key_for(email: &JmapEmail) -> Vec<u8> {
    let local_id = message_change_local_id(
        email.id.as_deref().unwrap_or(""),
        message_revision_digest(email),
    );
    let mut out = Vec::with_capacity(16 + local_id.len());
    out.extend_from_slice(&STORE_CHANGE_KEY_NAMESPACE_GUID);
    out.extend_from_slice(&local_id);
    out
}

/// Serialise the audit §2f.2 `PredecessorChangeList`
/// (MS-OXCFXICS §2.2.2.3): a concatenation of binary-sorted `SizedXid`
/// structures (`XidSize(1) + XID`) listing the change numbers of prior
/// revisions of the message. For a freshly-materialised row there is no
/// prior revision, so this emits a single-element list seeded with the row's
/// own change key (the current revision) — Outlook treats it as the seed
/// entry of the predecessor graph, which is exactly how a first-edition
/// message looks on a real store. The list MUST be binary-sorted by the
/// `SizedXid` bytes; with a single entry no sort is needed.
fn predecessor_change_list_for(email: &JmapEmail) -> Vec<u8> {
    let ck = change_key_for(email);
    // XidSize is a single byte; the namespace GUID (16) + LocalId (≤8) keep
    // the XID within the 1..=255 byte range comfortably.
    let xsz = u8::try_from(ck.len()).unwrap_or(255);
    let mut out = Vec::with_capacity(1 + ck.len());
    out.push(xsz);
    out.extend_from_slice(&ck);
    out
}

/// Fold a JMAP message id + a per-revision digest into the 8-byte `LocalId`
/// of a store change-key XID (audit §2f.2). The id half keeps the key stable
/// per revision; the `revision_digest` half mixes in so an edited message
/// (a new revision → a new digest) yields a different XID even though the
/// JMAP id is unchanged.
fn message_change_local_id(jmap_id: &str, revision_digest: u64) -> [u8; 8] {
    const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;
    let mut h = FNV_OFFSET;
    for b in jmap_id.as_bytes() {
        h ^= u64::from(*b);
        h = h.wrapping_mul(FNV_PRIME);
    }
    // Stir the revision digest into the high 32 bits so an edited message
    // (new digest) yields a different XID even though the JMAP id is
    // unchanged. The digest is derived from the mutable fields the gateway
    // actually edits via `Email/set` (keywords, mailboxIds, subject, preview,
    // blobId); RFC 8621 `receivedAt` is immutable and must NOT be used as a
    // revision signal.
    h ^= revision_digest.wrapping_mul(0x9E37_79B9_7F4A_7C15);
    h = h.wrapping_mul(FNV_PRIME);
    h.to_le_bytes()
}

/// Compute a deterministic 64-bit digest of the fields a JMAP `Email/set` edit
/// mutates (audit §2f.2): `keywords` (read/unread, flag, draft, custom
/// labels), `mailboxIds` (move/archive), and the body-bearing `subject`,
/// `preview`, `blobId` (a draft rewrite changes `blobId`). Folding this into
/// the change-key LocalId makes a real edit flip the XID, so a second device
/// editing the same message yields a distinct change key Outlook treats as a
/// new revision rather than re-reading the stale one. The mapping is kept as
/// a single FNV-1a pass over sorted key lists for determinism across reads
/// of the same revision.
fn message_revision_digest(email: &JmapEmail) -> u64 {
    const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;
    let mut h = FNV_OFFSET;
    let mut acc = |part: &str| {
        for b in part.as_bytes() {
            h ^= u64::from(*b);
            h = h.wrapping_mul(FNV_PRIME);
        }
    };
    if let Some(kws) = email.keywords.as_ref() {
        let mut ks: Vec<&String> = kws.keys().collect();
        ks.sort();
        for k in ks {
            acc(k);
            acc(if kws[k] { "1" } else { "0" });
        }
    }
    if let Some(mb) = email.mailbox_ids.as_ref() {
        let mut ms: Vec<&String> = mb.keys().collect();
        ms.sort();
        for m in ms {
            acc(m);
            acc(if mb[m] { "1" } else { "0" });
        }
    }
    if let Some(s) = email.subject.as_deref() {
        acc(s);
    }
    if let Some(s) = email.preview.as_deref() {
        acc(s);
    }
    if let Some(s) = email.blob_id.as_deref() {
        acc(s);
    }
    h
}

/// Aggregate JMAP message-id + mailbox-id into a stable 64-bit MAPI mid.
pub fn message_id_from_jmap(jmap_id: &str) -> u64 {
    folder_id_from_backend(jmap_id)
}

// ----------------------------------------------------------------------------
// MAPI EntryId synthesis
// ----------------------------------------------------------------------------

/// Per Outlook's one-off entry id (MS-OXCDATA s2.6.3.3), the provider uid for
/// an SMTP address. The 16-byte GUID `00020D01-....` is the MAPI one-off
/// provider uid; we synthesize the minimal entry id Outlook needs to display
/// sender addresses.
pub const ENTRIESKIND_SMTP: u8 = 0x00;

/// Provider UID for the MAPI one-off entry id (MS-OXCDATA s2.6.3.3 / s2.6.2.1).
/// Bytes: 00020D01 (one-off) — see PR_*_ENTRYID constants.
const ONEOFF_PROVIDER_UID: [u8; 16] = [
    0x81, 0x2b, 0x1f, 0xa4, 0xbe, 0xa3, 0x10, 0x19, 0x9d, 0x6e, 0x00, 0xdd, 0x01, 0x0f, 0x54, 0x40,
];

const MDB_PROVIDER_UID: [u8; 16] = [
    0x1b, 0x55, 0xfa, 0x20, 0xaa, 0x66, 0x11, 0xcd, 0x9b, 0xc8, 0x00, 0xaa, 0x00, 0x2f, 0xc4, 0x5a,
];

/// Per MS-OXCDATA §2.2.5.1 One-Off EntryID Structure, synthesize a one-off
/// entry id for an SMTP address. Layout: Flags(4)=0, ProviderUID(16),
/// Version(2)=0, ControlWord(2) with the U bit selecting Unicode null widths,
/// DisplayName (null-terminated), AddressType (null-terminated "SMTP"),
/// EmailAddress (null-terminated). Fields are null-terminated, NOT length
/// prefixed.
pub fn oneoff_entry_id(email_addr: &str, display_name: &str, _kind: u8) -> Vec<u8> {
    let mut out = Vec::with_capacity(28 + email_addr.len() * 2 + display_name.len() * 2 + 5);
    out.extend_from_slice(&0x00000000u32.to_le_bytes()); // Flags (long-term)
    out.extend_from_slice(&ONEOFF_PROVIDER_UID); // 16 bytes
    out.extend_from_slice(&0u16.to_le_bytes()); // Version = 0x0000
    // Control word: U=1 (Unicode), Format=TextOnly=0x0006, MAE/M = 0.
    // Word value = 0x0000 | (1<<0) = TextOnly with U bit. We want U=1 so the
    // null terminators are 2 bytes (UTF-16). Use the documented HTML defaults:
    // for a sender one-off, TextAndHtml=0x0016 with U bit 0x0001 -> 0x0017.
    // Keep the canonical short form: U=1 | Format=TextOnly=0x0006.
    let ctrl: u16 = 0x0001 | 0x0006; // U bit + TextOnly
    out.extend_from_slice(&ctrl.to_le_bytes());
    // DisplayName — null-terminated UTF-16LE (U=1 → 2-byte NUL).
    for u in display_name.encode_utf16() {
        out.extend_from_slice(&u.to_le_bytes());
    }
    out.extend_from_slice(&0u16.to_le_bytes()); // 2-byte NUL (U=1)
    // AddressType — "SMTP", null-terminated ASCII (matches U=1 -> 2-byte NUL).
    out.extend_from_slice(b"SMTP");
    out.extend_from_slice(&0u16.to_le_bytes());
    // EmailAddress — null-terminated UTF-16LE.
    for u in email_addr.encode_utf16() {
        out.extend_from_slice(&u.to_le_bytes());
    }
    out.extend_from_slice(&0u16.to_le_bytes());
    out
}

/// Synthesise the message entry id (MS-OXCDATA s2.6.3.5 "Folder entry id":
/// flags(4) + provider uid(16) + folder id (8 LE) + message id (8 LE) +
/// instance id (8 LE)) so Outlook can re-open the message by id.
pub fn message_entry_id(
    email: &JmapEmail,
    mailbox_id: &str,
    kind: crate::mapi::session::FolderKind,
) -> Vec<u8> {
    let mut out = Vec::with_capacity(28 + mailbox_id.len() + 16);
    out.extend_from_slice(&0x01000000u32.to_le_bytes()); // flags: MAPI message
    out.extend_from_slice(&MDB_PROVIDER_UID);
    out.extend_from_slice(&folder_id_from_backend(mailbox_id).to_le_bytes());
    out.extend_from_slice(&message_id_from_jmap(email.id.as_deref().unwrap_or("")).to_le_bytes());
    // Instance id == message id for the non-recurring case.
    out.extend_from_slice(&message_id_from_jmap(email.id.as_deref().unwrap_or("")).to_le_bytes());
    let _ = kind;
    out
}

/// Synthesise a folder entry id (4 flags + 16 provider uid + 8 folder id).
pub fn folder_entry_id(mailbox_id: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(28);
    out.extend_from_slice(&0x01000000u32.to_le_bytes());
    out.extend_from_slice(&MDB_PROVIDER_UID);
    out.extend_from_slice(&folder_id_from_backend(mailbox_id).to_le_bytes());
    out
}

// ----------------------------------------------------------------------------
// Folder-row conversion: JmapMailbox -> PropertyValue cells
// ----------------------------------------------------------------------------

/// Convert a JmapMailbox (RFC 8621 s5.1) into the cells for a hierarchy-table
/// row, in the requested column order.
pub fn mailbox_to_cells(mbx: &JmapMailbox, column_set: &[PropertyTag]) -> Vec<PropertyValue> {
    let backend_id = mbx.id.as_deref().unwrap_or("");
    let mut out = Vec::with_capacity(column_set.len());
    for tag in column_set {
        let want = tag.property_type;
        let v = match tag.property_id {
            PR_FOLDER_ID => {
                if !ttype_matches(want, PropertyType::PTYP_INTEGER64) {
                    PropertyValue::Null
                } else {
                    PropertyValue::Integer64(folder_id_from_backend(backend_id) as i64)
                }
            }
            PR_PARENT_FOLDER_ID => {
                if !ttype_matches(want, PropertyType::PTYP_INTEGER64) {
                    PropertyValue::Null
                } else {
                    // Top-level JMAP mailboxes have no parentId. Returning
                    // the folder's own id would self-cycle, which Outlook
                    // cannot handle when building the hierarchy tree. Map a
                    // missing parent to the synthetic root folder id that
                    // RopLogon installs as handle 0.
                    let pid = mbx.parent_id.as_deref().unwrap_or("ROOT");
                    PropertyValue::Integer64(folder_id_from_backend(pid) as i64)
                }
            }
            PR_DISPLAY_NAME => {
                let name = match mbx.name.as_deref() {
                    Some(n) if !n.is_empty() => n.to_string(),
                    _ => folder_display_name(mbx.role.as_deref(), backend_id),
                };
                if ttype_matches(want, PropertyType::PTYP_STRING) {
                    PropertyValue::String(name)
                } else if ttype_matches(want, PropertyType::PTYP_STRING8) {
                    PropertyValue::String8(name)
                } else {
                    PropertyValue::Null
                }
            }
            PR_CONTAINER_CLASS => {
                let kind = folder_kind_for_role(mbx.role.as_deref());
                let class = container_class_for(kind).to_string();
                if ttype_matches(want, PropertyType::PTYP_STRING) {
                    PropertyValue::String(class)
                } else {
                    PropertyValue::Null
                }
            }
            PR_CONTENT_COUNT => {
                if ttype_matches(want, PropertyType::PTYP_INTEGER32) {
                    PropertyValue::Integer32(saturate_i32(mbx.total_emails.unwrap_or(0)))
                } else {
                    PropertyValue::Null
                }
            }
            PR_CONTENT_UNREAD => {
                if ttype_matches(want, PropertyType::PTYP_INTEGER32) {
                    PropertyValue::Integer32(saturate_i32(mbx.unread_emails.unwrap_or(0)))
                } else {
                    PropertyValue::Null
                }
            }
            PR_SUBFOLDERS => {
                // RFC 8621 §5.1 doesn't expose child counts; the MAPI
                // hierarchy table is the authoritative source for "has
                // children". Returning Null (Flag=0x0a "not supported") is
                // safer than conflating it with message counts.
                PropertyValue::Null
            }
            PR_CHILD_COUNT => {
                if ttype_matches(want, PropertyType::PTYP_INTEGER32) {
                    PropertyValue::Integer32(0)
                } else {
                    PropertyValue::Null
                }
            }
            PR_ENTRYID => {
                if ttype_matches(want, PropertyType::PTYP_BINARY) {
                    PropertyValue::Binary(folder_entry_id(backend_id))
                } else {
                    PropertyValue::Null
                }
            }
            _ => PropertyValue::Null,
        };
        out.push(v);
    }
    out
}

// ----------------------------------------------------------------------------
// Row packaging: cells + the row id -> PropertyRowEntry ready to encode
// ----------------------------------------------------------------------------

/// Bundle a single message/folder cells set into a single
/// `PropertyRowEntry`(tag, value) per cell, suitable for direct
/// `RopGetPropertiesSpecific` serialization.
pub fn cells_to_row(
    row_id: u64,
    column_set: &[PropertyTag],
    cells: Vec<PropertyValue>,
) -> Vec<PropertyRowEntry> {
    let _ = row_id;
    column_set
        .iter()
        .zip(cells)
        .map(|(tag, value)| PropertyRowEntry { tag: *tag, value })
        .collect()
}

// ----------------------------------------------------------------------------
// Type-compat helpers
// ----------------------------------------------------------------------------

/// Build a property-cell vector of typed NULLs, one per requested tag, sized
/// for each column's declared type so the row decoder skips the right byte
/// length per MS-OXCDATA s2.11.2. Returned cells are `PropertyValue::Null`
/// / `Boolean(false)` / `String("")` / etc. as appropriate for the type.
/// Used by the GetProperties* arms as a fallback when no backend object was
/// resolved for the input handle.
/// The canonical "all properties" tag set returned by `RopGetPropertiesAll`
/// (MS-OXCROPS §2.2.8.3). A well-known, stable list of the PidTag identifiers
/// the gateway can materialise for a message object, each paired with its
/// canonical Ptyp type so `cell_for_email` / the calendar/contact converters
/// can fill the value (or a typed NULL for properties the backend does not
/// model). The ordering echoes the `cell_for_email` mapping so the full
/// envelope matches what `RopGetPropertiesSpecific` would return for the same
/// tags.
pub fn all_message_tags() -> Vec<PropertyTag> {
    use crate::mapi::data::PropertyType as T;
    let t = |ty: T, id: u16| PropertyTag::new(ty, id);
    vec![
        t(T::PTYP_STRING, PR_SUBJECT),
        t(T::PTYP_STRING, PR_MESSAGE_CLASS),
        t(T::PTYP_STRING, PR_NORMALIZED_SUBJECT),
        t(T::PTYP_STRING8, PR_BODY),
        t(T::PTYP_STRING, PR_BODY_HTML),
        t(T::PTYP_STRING, PR_SENDER_NAME),
        t(T::PTYP_STRING, PR_SENDER_EMAIL),
        t(T::PTYP_BINARY, PR_SENDER_ENTRYID),
        t(T::PTYP_STRING, PR_SENT_REPRESENTING_NAME),
        t(T::PTYP_STRING, PR_SENT_REPRESENTING_EMAIL),
        t(T::PTYP_TIME, PR_MESSAGE_DELIVERY_TIME),
        t(T::PTYP_TIME, PR_CLIENT_SUBMIT_TIME),
        t(T::PTYP_INTEGER32, PR_MESSAGE_SIZE),
        t(T::PTYP_BOOLEAN, PR_HAS_ATTACHMENTS),
        t(T::PTYP_INTEGER32, PR_MESSAGE_FLAGS),
        t(T::PTYP_INTEGER32, PR_IMPORTANCE),
        t(T::PTYP_INTEGER32, PR_FLAG_STATUS),
        t(T::PTYP_INTEGER32, PR_SENSITIVITY),
        t(T::PTYP_STRING, PR_INTERNET_MESSAGE_ID),
        t(T::PTYP_STRING, PR_IN_REPLY_TO_ID),
        t(T::PTYP_STRING, PR_INTERNET_REFERENCES),
        t(T::PTYP_BINARY, PR_CONVERSATION_ID),
        t(T::PTYP_BINARY, PR_ENTRYID),
        t(T::PTYP_BINARY, PR_RECORD_KEY),
        t(T::PTYP_BINARY, PR_SEARCH_KEY),
        t(T::PTYP_BINARY, PR_PARENT_ENTRYID),
        t(T::PTYP_INTEGER64, PR_MID),
        t(T::PTYP_BINARY, PR_CHANGE_KEY),
        t(T::PTYP_BINARY, PR_PREDECESSOR_CHANGE_LIST),
        t(T::PTYP_TIME, PR_LAST_MODIFICATION_TIME),
        t(T::PTYP_STRING8, PR_LAST_MODIFIER_NAME),
        t(T::PTYP_BOOLEAN, PR_READ),
        t(T::PTYP_BOOLEAN, PR_UNREAD),
        t(T::PTYP_BOOLEAN, PR_HAS_NAMED_PROPERTIES),
        t(T::PTYP_INTEGER32, PR_NATIVE_BODY),
        t(T::PTYP_INTEGER64, PR_FOLDER_ID),
        t(T::PTYP_INTEGER64, PR_PARENT_FOLDER_ID),
    ]
}

pub fn typed_null_cells(column_set: &[PropertyTag]) -> Vec<PropertyValue> {
    column_set.iter().map(typed_null_for_tag).collect()
}

/// Emit a typed NULL value for one property tag. The variant chosen matches
/// the column's declared `PropertyType` so the wire encoding (`PropertyValue::
/// encode`) writes the canonical zero/empty bytes the client expects.
pub fn typed_null_for_tag(tag: &PropertyTag) -> PropertyValue {
    use crate::mapi::data::PropertyType as T;
    match tag.property_type {
        T::PTYP_INTEGER16 => PropertyValue::Integer16(0),
        T::PTYP_INTEGER32 | T::PTYP_FLOATING32 | T::PTYP_FLOATING_TIME | T::PTYP_ERROR_CODE => {
            PropertyValue::Integer32(0)
        }
        T::PTYP_INTEGER64 | T::PTYP_FLOATING64 | T::PTYP_CURRENCY | T::PTYP_TIME => {
            PropertyValue::Integer64(0)
        }
        T::PTYP_BOOLEAN => PropertyValue::Boolean(false),
        T::PTYP_STRING => PropertyValue::String(String::new()),
        T::PTYP_STRING8 => PropertyValue::String8(String::new()),
        T::PTYP_BINARY => PropertyValue::Binary(Vec::new()),
        T::PTYP_GUID => PropertyValue::Guid([0u8; 16]),
        _ => PropertyValue::Null,
    }
}

/// Whether `want` is a compatible MAPI property type for `actual`. Used to
/// shape NULLs when the client asks for the wrong scalar type (the spec lets
/// the server return a typed value, but Outlook tolerates NULL better than a
/// type-mismatch).
fn ttype_matches(want: PropertyType, actual: PropertyType) -> bool {
    if want == actual {
        return true;
    }
    // An Ask for PTYP_UNSPECIFIED (0x0000) means "give us whatever type the
    // server has"; accept anything.
    if want == PropertyType::PTYP_UNSPECIFIED {
        return true;
    }
    // PTYP_STRING and PTYP_STRING8 are interchangeable per s2.11.1.2.
    let stringy =
        |t: PropertyType| matches!(t, PropertyType::PTYP_STRING | PropertyType::PTYP_STRING8);
    stringy(want) && stringy(actual)
}

/// Public re-export of the private type-compat helper for the
/// calendar/contact converters in `mapi/converters.rs`, which need the same
/// "requested wire type matches the scalar we'd emit" gate so an
/// incompatible-type request degrades to a typed `Null` exactly as the
/// email/mailbox converters do.
pub fn ttype_matches_pub(want: PropertyType, actual: PropertyType) -> bool {
    ttype_matches(want, actual)
}

/// Synthesise a long-term message/folder entry id for a backend object that
/// is NOT a `JmapEmail` (a CalDAV calendar event identified by its iCalendar
/// UID, or a CardDAV contact identified by its vCard UID). Used by the
/// calendar/contact converters so a `RopGetPropertiesSpecific` / `QueryRows`
/// row carries a re-openable entry id the same shape as the mail one
/// (MS-OXCDATA §2.6.3.5): flags(4) + provider uid(16) + folder id(8 LE) +
/// message id(8 LE) + instance id(8 LE). The folder id comes from the parent
/// folder backend id (the calendar/contact synthetic backend id); the message
/// id is the FNV-1a of the item UID.
pub fn message_entry_id_for(item_id: &str, mailbox_id: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(44);
    out.extend_from_slice(&0x01000000u32.to_le_bytes()); // flags: MAPI message
    out.extend_from_slice(&MDB_PROVIDER_UID);
    out.extend_from_slice(&folder_id_from_backend(mailbox_id).to_le_bytes());
    let mid = message_id_from_jmap(item_id);
    out.extend_from_slice(&mid.to_le_bytes());
    out.extend_from_slice(&mid.to_le_bytes()); // instance == message id
    out
}

// ----------------------------------------------------------------------------
// Tests
// ----------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::jmap::JmapEmail;
    use crate::mapi::session::FolderKind;

    fn email(subject: &str, seen: bool) -> JmapEmail {
        let mut kws = std::collections::HashMap::new();
        kws.insert("$seen".to_string(), seen);
        JmapEmail {
            id: Some("M-test".to_string()),
            blob_id: None,
            thread_id: Some("T-1".to_string()),
            mailbox_ids: None,
            keywords: Some(kws),
            size: Some(12345),
            received_at: Some("2025-01-01T00:00:00Z".to_string()),
            sent_at: None,
            has_attachment: Some(false),
            from: None,
            to: None,
            cc: None,
            bcc: None,
            reply_to: None,
            subject: Some(subject.to_string()),
            preview: None,
            body_values: None,
            text_body: None,
            html_body: None,
            attachments: None,
            body_structure: None,
            header_raw: None,
            sender: None,
            message_id: Some("id@host".to_string()),
            in_reply_to: None,
            references: None,
        }
    }

    #[test]
    fn folder_id_is_total_and_nonzero() {
        let a = folder_id_from_backend("I");
        let b = folder_id_from_backend("I");
        assert_eq!(a, b);
        assert_ne!(a, 0);
        assert_ne!(folder_id_from_backend("I"), folder_id_from_backend("J"));
    }

    #[test]
    fn email_subject_cell_string() {
        let e = email("hi", false);
        let tag = PropertyTag::new(PropertyType::PTYP_STRING, PR_SUBJECT);
        let cells = email_to_cells(&e, &[tag], FolderKind::Mail, "I");
        assert_eq!(cells.len(), 1);
        assert!(
            matches!(&cells[0], PropertyValue::String(s) if s == "hi"),
            "got {:?}",
            cells[0]
        );
    }

    #[test]
    fn message_flags_read_bit_set_when_seen() {
        let e = email("s", true);
        let tag = PropertyTag::new(PropertyType::PTYP_INTEGER32, PR_MESSAGE_FLAGS);
        let cells = email_to_cells(&e, &[tag], FolderKind::Mail, "I");
        let PropertyValue::Integer32(flags) = &cells[0] else {
            panic!("expected int32");
        };
        assert!((*flags as u32) & msgflag::READ != 0);
    }

    #[test]
    fn unknown_property_returns_null() {
        let e = email("s", false);
        let tag = PropertyTag::new(PropertyType::PTYP_INTEGER32, 0xFFFF);
        let cells = email_to_cells(&e, &[tag], FolderKind::Mail, "I");
        assert!(matches!(cells[0], PropertyValue::Null));
    }

    #[test]
    fn wrong_type_returns_null() {
        let e = email("s", false);
        // Ask for subject as INTEGER32 — incompatible, server returns NULL.
        let tag = PropertyTag::new(PropertyType::PTYP_INTEGER32, PR_SUBJECT);
        let cells = email_to_cells(&e, &[tag], FolderKind::Mail, "I");
        assert!(matches!(cells[0], PropertyValue::Null));
    }

    #[test]
    fn message_delivery_time_filetime() {
        let e = email("s", false);
        let tag = PropertyTag::new(PropertyType::PTYP_TIME, PR_MESSAGE_DELIVERY_TIME);
        let cells = email_to_cells(&e, &[tag], FolderKind::Mail, "I");
        let PropertyValue::Time(ft) = &cells[0] else {
            panic!("expected time");
        };
        // 2025-01-01T00:00:00Z -> 1704067200_000ms -> in 100-ns ticks + offset
        assert!(*ft > FILETIME_EPOCH_OFFSET as u64);
    }

    #[test]
    fn oneoff_entry_id_has_provider_uid() {
        let id = oneoff_entry_id("u@x.com", "U", ENTRIESKIND_SMTP);
        assert!(id.len() >= 4 + 16 + 2);
        // Provider UID at offset 4
        assert_eq!(&id[4..20], &ONEOFF_PROVIDER_UID);
    }

    #[test]
    fn message_entry_id_shape() {
        let e = email("s", false);
        let id = message_entry_id(&e, "I", FolderKind::Mail);
        // 4 (flags) + 16 (provider uid) + 8 (folder id) + 8 (msg id) + 8 (inst id) = 44
        assert_eq!(id.len(), 44);
        assert_eq!(&id[4..20], &MDB_PROVIDER_UID);
    }

    #[test]
    fn conversation_id_is_16_bytes() {
        let e = email("s", false);
        let id = conversation_id_for(&e);
        assert_eq!(id.len(), 16);
    }

    #[test]
    fn email_change_key_is_valid_xid() {
        // MS-OXCFXICS §2.2.2.2: an XID is NamespaceGuid(16) + LocalId(1..8).
        // The audit §2f.2 change key MUST be a real XID (the old helper emitted
        // a non-conformant 4+4+n blob) so Outlook accepts it for conflict
        // resolution, and MUST carry a non-zero namespace GUID so it is not
        // mistaken for the "no value" sentinel.
        let e = email("s", false);
        let ck = change_key_for(&e);
        assert_eq!(ck.len(), 16 + 8, "XID = 16-byte GUID + 8-byte LocalId");
        assert_ne!(&ck[..16], &[0u8; 16], "namespace GUID must not be all-zero");
        assert_eq!(&ck[..16], &STORE_CHANGE_KEY_NAMESPACE_GUID);
        assert!(
            !ck[16..].iter().all(|&b| b == 0),
            "LocalId must not be all-zero"
        );
    }

    #[test]
    fn email_change_key_differs_on_edit() {
        // Audit §2f.2: an edited message must yield a different change key
        // from the prior revision so a second device to touch it gets a fresh
        // XID rather than re-reading the stale one. RFC 8621 `receivedAt` is a
        // delivery timestamp that does NOT change on edit, so the revision
        // signal is a digest of the mutable fields (keywords, mailboxIds,
        // subject, preview, blobId) the gateway edits via `Email/set`.
        let mut e = email("s", false);
        let ck_a = change_key_for(&e);
        // Flipping the read/flag keyword is exactly what `Email/set` does;
        // the change key MUST flip with it.
        e.keywords
            .as_mut()
            .unwrap()
            .insert("$seen".to_string(), true);
        let ck_b = change_key_for(&e);
        assert_ne!(ck_a, ck_b, "edits to the message must change the key");
        // Mutating the immutable `received_at` (a delivery time the gateway
        // never edits) MUST NOT flip the change key — it is not a revision
        // signal, so re-reading the same revision returns the same XID even
        // if the observer's clock view changed.
        let ck_c = {
            let mut e2 = email("s", false);
            e2.received_at = Some("2029-02-02T00:00:00Z".to_string());
            change_key_for(&e2)
        };
        assert_eq!(
            ck_a, ck_c,
            "immutable received_at is not a revision signal; key must be stable"
        );
        // Same identity across the same revision (deterministic).
        assert_eq!(ck_a, change_key_for(&email("s", false)));
    }

    #[test]
    fn email_predecessor_change_list_is_sized_xid_list() {
        // PR_PREDECESSOR_CHANGE_LIST (MS-OXCFXICS §2.2.2.3): a concatenation
        // of binary-sorted SizedXid structs (= XidSize(1) + XID). For a fresh
        // row we seed the predecessor graph with the row's own change key.
        let e = email("s", false);
        let pcl = predecessor_change_list_for(&e);
        let ck = change_key_for(&e);
        assert!(!pcl.is_empty());
        assert_eq!(usize::from(pcl[0]), ck.len(), "XidSize prefixes the XID");
        assert_eq!(&pcl[1..], &ck[..], "the seeded entry is the change key");
    }

    #[test]
    fn email_cells_emit_change_key_and_predecessor_list() {
        // The cells must surface both PR_CHANGE_KEY (audit §2f.2 main fix)
        // and PR_PREDECESSOR_CHANGE_LIST (audit §2f.2: previously fell through
        // to Null) so Outlook has both halves of the conflict-detection graph.
        let e = email("s", false);
        let cols = [
            PropertyTag::new(PropertyType::PTYP_BINARY, PR_CHANGE_KEY),
            PropertyTag::new(PropertyType::PTYP_BINARY, PR_PREDECESSOR_CHANGE_LIST),
        ];
        let cells = email_to_cells(&e, &cols, FolderKind::Mail, "I");
        assert!(
            matches!(&cells[0], PropertyValue::Binary(b) if b == &change_key_for(&e)),
            "PR_CHANGE_KEY cell mismatch: {:?}",
            cells[0]
        );
        assert!(
            matches!(&cells[1], PropertyValue::Binary(b) if !b.is_empty()),
            "PR_PREDECESSOR_CHANGE_LIST must be a non-empty binary cell, got {:?}",
            cells[1]
        );
        // An incompatible wire-type request degrades to a typed Null rather
        // than emitting binary bytes the client cannot consume.
        let bad = PropertyTag::new(PropertyType::PTYP_INTEGER32, PR_CHANGE_KEY);
        let cells = email_to_cells(&e, &[bad], FolderKind::Mail, "I");
        assert!(matches!(cells[0], PropertyValue::Null));
    }

    #[test]
    fn mailbox_row_columns() {
        let mbx = JmapMailbox {
            id: Some("I".to_string()),
            name: Some("Inbox".to_string()),
            parent_id: None,
            role: Some("inbox".to_string()),
            sort_order: None,
            total_emails: Some(42),
            unread_emails: Some(7),
            total_threads: None,
            unread_threads: None,
            is_subscribed: None,
        };
        let cols = vec![
            PropertyTag::new(PropertyType::PTYP_INTEGER64, PR_FOLDER_ID),
            PropertyTag::new(PropertyType::PTYP_STRING, PR_DISPLAY_NAME),
            PropertyTag::new(PropertyType::PTYP_INTEGER32, PR_CONTENT_COUNT),
        ];
        let cells = mailbox_to_cells(&mbx, &cols);
        assert!(matches!(cells[0], PropertyValue::Integer64(_)));
        assert!(matches!(&cells[1], PropertyValue::String(s) if s == "Inbox"));
        assert!(matches!(cells[2], PropertyValue::Integer32(42)));
    }

    #[test]
    fn container_class_for_kinds() {
        assert_eq!(container_class_for(FolderKind::Mail), "IPF.Note");
        assert_eq!(container_class_for(FolderKind::Calendar), "IPF.Appointment");
        assert_eq!(container_class_for(FolderKind::Contacts), "IPF.Contact");
    }

    #[test]
    fn folder_display_name_known_roles() {
        assert_eq!(folder_display_name(Some("inbox"), "x"), "Inbox");
        assert_eq!(folder_display_name(Some("trash"), "x"), "Deleted Items");
        // unknown role -> leaf of backend id
        assert_eq!(folder_display_name(None, "a/b/Custom"), "Custom");
    }

    #[test]
    fn iso8601_to_filetime_stable() {
        let ft_a = iso8601_to_filetime(Some("2025-01-01T00:00:00Z"));
        let ft_b = iso8601_to_filetime(Some("2025-01-01T00:00:00Z"));
        assert_eq!(ft_a, ft_b);
        assert!(iso8601_to_filetime(None).is_none());
        assert!(iso8601_to_filetime(Some("garbage")).is_none());
    }

    // ---- set_values_to_patch / delete_tags_to_patch (audit gap 2a) ----
    // The translator maps the MAPI scalar compose props to JMAP Email/set
    // update patches and reports NO_SUPPORT for intrinsic/read-only/body
    // props via the PropertyProblem array (MS-OXCDATA 2.7).

    fn ttag(id: u16, ty: crate::mapi::data::PropertyType) -> crate::mapi::data::PropertyTag {
        crate::mapi::data::PropertyTag::new(ty, id)
    }

    #[test]
    fn set_values_subject_maps_to_jmap_subject() {
        use crate::mapi::data::{PropertyValue, TaggedPropertyValue};
        let values = vec![TaggedPropertyValue {
            tag: ttag(PR_SUBJECT, crate::mapi::data::PropertyType::PTYP_STRING),
            value: PropertyValue::String("Hello".to_string()),
        }];
        let patch = set_values_to_patch(&values);
        assert_eq!(
            patch.patch.get("subject"),
            Some(&serde_json::json!("Hello"))
        );
        assert!(patch.problems.is_empty());
    }

    #[test]
    fn set_values_importance_high_sets_dollar_important() {
        use crate::mapi::data::{PropertyValue, TaggedPropertyValue};
        let values = vec![TaggedPropertyValue {
            tag: ttag(
                PR_IMPORTANCE,
                crate::mapi::data::PropertyType::PTYP_INTEGER32,
            ),
            value: PropertyValue::Integer32(2),
        }];
        let patch = set_values_to_patch(&values);
        assert_eq!(
            patch.patch.get("keywords/$important"),
            Some(&serde_json::json!(true))
        );
    }

    #[test]
    fn set_values_importance_normal_clears_dollar_important() {
        use crate::mapi::data::{PropertyValue, TaggedPropertyValue};
        let values = vec![TaggedPropertyValue {
            tag: ttag(
                PR_IMPORTANCE,
                crate::mapi::data::PropertyType::PTYP_INTEGER32,
            ),
            value: PropertyValue::Integer32(1),
        }];
        let patch = set_values_to_patch(&values);
        assert_eq!(
            patch.patch.get("keywords/$important"),
            Some(&serde_json::Value::Null)
        );
    }

    #[test]
    fn importance_for_reads_high_when_dollar_important_present() {
        // Symmetry with set_values_to_patch (which writes `$important`) :
        // an email that carries `$important` reads back PR_IMPORTANCE=2
        // (High). The prior impl returned 1, so a set-then-read round trip
        // appeared to drop the importance (cubic review #28).
        let mut e = email("s", true);
        e.keywords
            .as_mut()
            .unwrap()
            .insert("$important".to_string(), true);
        assert_eq!(importance_for(&e), 2);
    }

    #[test]
    fn importance_for_reads_normal_when_absent() {
        let e = email("s", false);
        assert_eq!(importance_for(&e), 1);
    }

    #[test]
    fn flag_status_for_reads_flagged_when_dollar_flagged_present() {
        // Symmetry with set_values_to_patch (which writes `$flagged` for
        // PR_FLAG_STATUS=0x02): an email carrying `$flagged` reads back
        // 0x02. The prior email_to_cells returned a typed Null, so the flag
        // round-tripped as missing (cubic review #29).
        let mut e = email("s", true);
        e.keywords
            .as_mut()
            .unwrap()
            .insert("$flagged".to_string(), true);
        assert_eq!(flag_status_for(&e), 0x02);
    }

    #[test]
    fn flag_status_for_reads_unflagged_when_absent() {
        let e = email("s", false);
        assert_eq!(flag_status_for(&e), 0x00);
    }

    #[test]
    fn set_values_followup_flag_sets_dollar_flagged() {
        // MS-OXOFLAG 2.2.1.1: 0x02 followupFlagged sets the flag.
        use crate::mapi::data::{PropertyValue, TaggedPropertyValue};
        let values = vec![TaggedPropertyValue {
            tag: ttag(
                PR_FLAG_STATUS,
                crate::mapi::data::PropertyType::PTYP_INTEGER32,
            ),
            value: PropertyValue::Integer32(0x02),
        }];
        let patch = set_values_to_patch(&values);
        assert_eq!(
            patch.patch.get("keywords/$flagged"),
            Some(&serde_json::json!(true))
        );
    }

    #[test]
    fn set_values_body_reports_no_support_problem() {
        // PR_BODY (and PR_BODY_HTML) report MAPI_E_NO_SUPPORT because the
        // stream ROPs own body editing; the body must never corrupt a
        // partial patch.
        use crate::mapi::data::{PropertyValue, TaggedPropertyValue};
        let values = vec![TaggedPropertyValue {
            tag: ttag(PR_BODY, crate::mapi::data::PropertyType::PTYP_STRING8),
            value: PropertyValue::String8("body".to_string()),
        }];
        let patch = set_values_to_patch(&values);
        assert!(patch.patch.is_empty());
        assert_eq!(patch.problems.len(), 1);
        assert_eq!(patch.problems[0].error_code, 0x8004_0102);
        assert_eq!(patch.problems[0].tag.property_id, PR_BODY);
    }

    #[test]
    fn set_values_readonly_intrinsic_reports_no_support() {
        use crate::mapi::data::{PropertyValue, TaggedPropertyValue};
        let values = vec![TaggedPropertyValue {
            tag: ttag(PR_ENTRYID, crate::mapi::data::PropertyType::PTYP_BINARY),
            value: PropertyValue::Binary(vec![0u8; 16]),
        }];
        let patch = set_values_to_patch(&values);
        assert!(patch.patch.is_empty());
        assert_eq!(patch.problems.len(), 1);
        assert_eq!(patch.problems[0].error_code, 0x8004_0102);
    }

    #[test]
    fn set_values_named_property_reports_no_support() {
        // A named property (0x8000 id bit, e.g. PidLidCategories as an MV
        // string) is NOT silently dropped: the translator records a
        // MAPI_E_NO_SUPPORT problem so Outlook knows the write did not
        // persist, rather than believing an uncategorised write succeeded
        // (qodo #7 / cubic #27). Persistence lands in Phase 2 once the
        // named-property GUID/LID table is wired; the MV bytes were already
        // sized-and-skipped by the decoder, so the chain stays byte-aligned.
        use crate::mapi::data::{PropertyValue, TaggedPropertyValue};
        let values = vec![TaggedPropertyValue {
            tag: ttag(0x8001, crate::mapi::data::PropertyType::from_u16(0x101E)),
            value: PropertyValue::Opaque {
                property_type: crate::mapi::data::PropertyType::from_u16(0x101E),
                bytes: vec![1, 0, 0, 0, b'a', 0],
            },
        }];
        let patch = set_values_to_patch(&values);
        assert!(patch.patch.is_empty());
        assert_eq!(patch.problems.len(), 1);
        assert_eq!(patch.problems[0].error_code, 0x8004_0102);
        assert_eq!(patch.problems[0].tag.property_id, 0x8001);
    }

    #[test]
    fn delete_tags_subject_clears_importance_and_flag() {
        let tags = [
            ttag(PR_SUBJECT, crate::mapi::data::PropertyType::PTYP_STRING),
            ttag(
                PR_IMPORTANCE,
                crate::mapi::data::PropertyType::PTYP_INTEGER32,
            ),
            ttag(
                PR_FLAG_STATUS,
                crate::mapi::data::PropertyType::PTYP_INTEGER32,
            ),
        ];
        let patch = delete_tags_to_patch(&tags);
        assert_eq!(patch.patch.get("subject"), Some(&serde_json::json!("")));
        assert_eq!(
            patch.patch.get("keywords/$important"),
            Some(&serde_json::Value::Null)
        );
        assert_eq!(
            patch.patch.get("keywords/$flagged"),
            Some(&serde_json::Value::Null)
        );
        assert!(patch.problems.is_empty());
    }

    #[test]
    fn delete_tags_readonly_reports_no_support() {
        let tags = [ttag(
            PR_ENTRYID,
            crate::mapi::data::PropertyType::PTYP_BINARY,
        )];
        let patch = delete_tags_to_patch(&tags);
        assert!(patch.patch.is_empty());
        assert_eq!(patch.problems.len(), 1);
        assert_eq!(patch.problems[0].error_code, 0x8004_0102);
    }

    // ---- Attachment enumeration + cell materialisation (audit gap §2a) ----

    fn att(name: Option<&str>, ct: Option<&str>, size: Option<u64>) -> crate::jmap::JmapAttachment {
        crate::jmap::JmapAttachment {
            id: Some("f-att".to_string()),
            blob_id: Some("B-fatt".to_string()),
            size,
            content_type: ct.map(String::from),
            name: name.map(String::from),
        }
    }

    fn email_with_attachments(atts: Vec<crate::jmap::JmapAttachment>) -> JmapEmail {
        let mut e = email("with-attachments", false);
        e.attachments = Some(atts);
        e
    }

    #[test]
    fn email_attach_nums_are_sequential_indices() {
        let e = email_with_attachments(vec![att(None, None, None), att(None, None, None)]);
        assert_eq!(email_attach_nums(&e), vec![0, 1]);
        let no = email("none", false);
        assert!(email_attach_nums(&no).is_empty());
    }

    #[test]
    fn email_attachment_by_num_resolves_index() {
        let e = email_with_attachments(vec![
            att(Some("a.pdf"), None, Some(10)),
            att(Some("b.png"), None, Some(20)),
        ]);
        assert_eq!(
            email_attachment_by_num(&e, 1).and_then(|a| a.name.clone()),
            Some("b.png".to_string())
        );
        assert!(email_attachment_by_num(&e, 5).is_none());
    }

    #[test]
    fn attachment_to_cells_materialises_metadata() {
        let a = att(Some("report.pdf"), Some("application/pdf"), Some(4096));
        let cols = [
            ttag(
                PR_ATTACH_NUM,
                crate::mapi::data::PropertyType::PTYP_INTEGER32,
            ),
            ttag(
                PR_ATTACH_LONG_FILENAME,
                crate::mapi::data::PropertyType::PTYP_STRING,
            ),
            ttag(
                PR_ATTACH_MIME_TAG,
                crate::mapi::data::PropertyType::PTYP_STRING,
            ),
            ttag(
                PR_ATTACH_SIZE,
                crate::mapi::data::PropertyType::PTYP_INTEGER32,
            ),
            ttag(
                PR_ATTACH_METHOD,
                crate::mapi::data::PropertyType::PTYP_INTEGER32,
            ),
        ];
        let cells = attachment_to_cells(&a, 2, &cols);
        assert_eq!(cells.len(), 5);
        let crate::mapi::data::PropertyValue::Integer32(num) = &cells[0] else {
            panic!("attach num");
        };
        assert_eq!(*num, 2);
        let crate::mapi::data::PropertyValue::String(name) = &cells[1] else {
            panic!("long filename");
        };
        assert_eq!(name, "report.pdf");
        let crate::mapi::data::PropertyValue::String(mt) = &cells[2] else {
            panic!("mime tag");
        };
        assert_eq!(mt, "application/pdf");
        let crate::mapi::data::PropertyValue::Integer32(sz) = &cells[3] else {
            panic!("size");
        };
        assert_eq!(*sz, 4096);
        let crate::mapi::data::PropertyValue::Integer32(method) = &cells[4] else {
            panic!("method");
        };
        assert_eq!(*method, 0); // attByValue
    }

    #[test]
    fn attachment_to_cells_extension_includes_leading_dot() {
        // MS-OXPROPS PidTagAttachExtension: the extension INCLUDING the
        // leading period; empty when the name has no extension.
        let a = att(Some("archive.tar.gz"), None, None);
        let tag = ttag(
            PR_ATTACH_EXTENSION,
            crate::mapi::data::PropertyType::PTYP_STRING8,
        );
        let cells = attachment_to_cells(&a, 0, &[tag]);
        let crate::mapi::data::PropertyValue::String8(ext) = &cells[0] else {
            panic!("extension");
        };
        assert_eq!(ext, ".gz");
        let a2 = att(Some("noext"), None, None);
        let cells2 = attachment_to_cells(&a2, 0, &[tag]);
        let crate::mapi::data::PropertyValue::String8(ext2) = &cells2[0] else {
            panic!("extension no-dot");
        };
        assert_eq!(ext2, "");
    }

    #[test]
    fn attachment_to_cells_size_saturates_at_i32_max() {
        // An attachment ≥ 2 GiB must NOT wrap to a negative PR_ATTACH_SIZE;
        // it saturates at i32::MAX so the client sees a large-but-valid size.
        let a = att(Some("big.bin"), None, Some(u64::from(i32::MAX as u32) + 1));
        let tag = ttag(
            PR_ATTACH_SIZE,
            crate::mapi::data::PropertyType::PTYP_INTEGER32,
        );
        let cells = attachment_to_cells(&a, 0, &[tag]);
        let crate::mapi::data::PropertyValue::Integer32(sz) = &cells[0] else {
            panic!("size");
        };
        assert_eq!(*sz, i32::MAX);
    }

    #[test]
    fn attachment_to_cells_pr_attach_data_bin_is_null_not_streamed() {
        // PR_ATTACH_DATA_BIN must NOT materialise via cells (it can be
        // arbitrarily large and is delivered through OpenStream+ReadStream).
        let a = att(Some("x.bin"), None, Some(8));
        let tag = ttag(
            PR_ATTACH_DATA_BIN,
            crate::mapi::data::PropertyType::PTYP_BINARY,
        );
        let cells = attachment_to_cells(&a, 0, &[tag]);
        assert!(matches!(cells[0], crate::mapi::data::PropertyValue::Null));
    }

    #[test]
    fn attachment_to_cells_unknown_tag_is_null() {
        let a = att(Some("y.txt"), None, None);
        let tag = ttag(0xFFFF, crate::mapi::data::PropertyType::PTYP_INTEGER32);
        let cells = attachment_to_cells(&a, 0, &[tag]);
        assert!(matches!(cells[0], crate::mapi::data::PropertyValue::Null));
    }
}
