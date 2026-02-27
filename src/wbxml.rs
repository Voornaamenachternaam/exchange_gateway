// src/wbxml.rs
use base64::{Engine as _, engine::general_purpose};
use quick_xml::{Reader, events::Event};
use std::collections::HashMap;
use std::sync::LazyLock;

// ── WBXML global token constants ─────────────────────────────────────────────
const SWITCH_PAGE: u8 = 0x00;
const END: u8 = 0x01;
const STR_I: u8 = 0x03;
const OPAQUE: u8 = 0xC3;

// ── EAS 14.1 code-page byte values (MS-ASWBXML) ──────────────────────────────
const PAGE_AIRSYNC: u8 = 0x00; //  0 – AirSync (Sync, Collections, …)
const PAGE_CALENDAR: u8 = 0x04; //  4 – Calendar
const PAGE_FOLDERHIERARCHY: u8 = 0x07; //  7 – FolderHierarchy (FolderSync, …)
const PAGE_MEETINGRESPONSE: u8 = 0x08; //  8 – MeetingResponse
const PAGE_PING: u8 = 0x0D; // 13 – Ping
const PAGE_PROVISION: u8 = 0x0E; // 14 – Provision
const PAGE_AIRSYNCBASE: u8 = 0x11; // 17 – AirSyncBase (Body, Data, …)
const PAGE_SETTINGS: u8 = 0x12; // 18 – Settings

// ── Forward map: (page, token) → (namespace-prefix, element-name) ─────────────
static TAG_MAP: LazyLock<HashMap<(u8, u8), (&'static str, &'static str)>> =
    LazyLock::new(|| {
        let mut m: HashMap<(u8, u8), (&'static str, &'static str)> = HashMap::new();

        // ── Page 0: AirSync ──────────────────────────────────────────────────
        m.insert((PAGE_AIRSYNC, 0x05), ("AirSync", "Sync"));
        m.insert((PAGE_AIRSYNC, 0x06), ("AirSync", "Responses"));
        m.insert((PAGE_AIRSYNC, 0x07), ("AirSync", "Add"));
        m.insert((PAGE_AIRSYNC, 0x08), ("AirSync", "Change"));
        m.insert((PAGE_AIRSYNC, 0x09), ("AirSync", "Delete"));
        m.insert((PAGE_AIRSYNC, 0x0A), ("AirSync", "Fetch"));
        m.insert((PAGE_AIRSYNC, 0x0B), ("AirSync", "SyncKey"));
        m.insert((PAGE_AIRSYNC, 0x0C), ("AirSync", "ClientId"));
        m.insert((PAGE_AIRSYNC, 0x0D), ("AirSync", "CollectionId"));
        m.insert((PAGE_AIRSYNC, 0x0E), ("AirSync", "GetChanges"));
        m.insert((PAGE_AIRSYNC, 0x0F), ("AirSync", "MoreAvailable"));
        m.insert((PAGE_AIRSYNC, 0x10), ("AirSync", "WindowSize"));
        m.insert((PAGE_AIRSYNC, 0x11), ("AirSync", "Commands"));
        m.insert((PAGE_AIRSYNC, 0x12), ("AirSync", "Status"));
        m.insert((PAGE_AIRSYNC, 0x13), ("AirSync", "Collection"));
        m.insert((PAGE_AIRSYNC, 0x14), ("AirSync", "Class"));
        m.insert((PAGE_AIRSYNC, 0x15), ("AirSync", "Version"));
        m.insert((PAGE_AIRSYNC, 0x16), ("AirSync", "Wait"));
        m.insert((PAGE_AIRSYNC, 0x17), ("AirSync", "ApplicationData"));
        m.insert((PAGE_AIRSYNC, 0x18), ("AirSync", "DeletesAsMoves"));
        m.insert((PAGE_AIRSYNC, 0x19), ("AirSync", "NotifyGuid"));
        m.insert((PAGE_AIRSYNC, 0x1A), ("AirSync", "Supported"));
        m.insert((PAGE_AIRSYNC, 0x1B), ("AirSync", "SoftDelete"));
        m.insert((PAGE_AIRSYNC, 0x1C), ("AirSync", "MIMESupport"));
        m.insert((PAGE_AIRSYNC, 0x1D), ("AirSync", "MIMETruncation"));
        m.insert((PAGE_AIRSYNC, 0x1E), ("AirSync", "ReplyTo"));
        m.insert((PAGE_AIRSYNC, 0x1F), ("AirSync", "ContentClass"));
        m.insert((PAGE_AIRSYNC, 0x20), ("AirSync", "Categories"));
        m.insert((PAGE_AIRSYNC, 0x21), ("AirSync", "Category"));
        m.insert((PAGE_AIRSYNC, 0x22), ("AirSync", "ServerId"));
        m.insert((PAGE_AIRSYNC, 0x23), ("AirSync", "Truncation"));
        m.insert((PAGE_AIRSYNC, 0x24), ("AirSync", "MaxItems"));
        m.insert((PAGE_AIRSYNC, 0x25), ("AirSync", "Options"));
        m.insert((PAGE_AIRSYNC, 0x26), ("AirSync", "FilterType"));

        // ── Page 4: Calendar (MS-ASCAL) ──────────────────────────────────────
        m.insert((PAGE_CALENDAR, 0x05), ("Calendar", "TimeZone"));
        m.insert((PAGE_CALENDAR, 0x06), ("Calendar", "AllDayEvent"));
        m.insert((PAGE_CALENDAR, 0x07), ("Calendar", "Attendees"));
        m.insert((PAGE_CALENDAR, 0x08), ("Calendar", "Attendee"));
        m.insert((PAGE_CALENDAR, 0x09), ("Calendar", "Email"));
        m.insert((PAGE_CALENDAR, 0x0A), ("Calendar", "FileAs")); // attendee name (EAS ≤ 2.5)
        m.insert((PAGE_CALENDAR, 0x0B), ("Calendar", "AttendeeStatus"));
        m.insert((PAGE_CALENDAR, 0x0C), ("Calendar", "AttendeeType"));
        m.insert((PAGE_CALENDAR, 0x0D), ("Calendar", "Body")); // legacy pre-12.0
        m.insert((PAGE_CALENDAR, 0x0E), ("Calendar", "BodyTruncated")); // legacy
        m.insert((PAGE_CALENDAR, 0x0F), ("Calendar", "BusyStatus"));
        m.insert((PAGE_CALENDAR, 0x10), ("Calendar", "Categories"));
        m.insert((PAGE_CALENDAR, 0x11), ("Calendar", "Category"));
        m.insert((PAGE_CALENDAR, 0x12), ("Calendar", "CompressedRTF"));
        m.insert((PAGE_CALENDAR, 0x13), ("Calendar", "DtStamp"));
        m.insert((PAGE_CALENDAR, 0x14), ("Calendar", "End")); // EndTime token, XML element <End>
        m.insert((PAGE_CALENDAR, 0x15), ("Calendar", "Exceptions"));
        m.insert((PAGE_CALENDAR, 0x16), ("Calendar", "Exception"));
        m.insert((PAGE_CALENDAR, 0x17), ("Calendar", "Deleted"));
        m.insert((PAGE_CALENDAR, 0x18), ("Calendar", "ExceptionStartTime"));
        m.insert((PAGE_CALENDAR, 0x19), ("Calendar", "Location"));
        m.insert((PAGE_CALENDAR, 0x1A), ("Calendar", "MeetingStatus"));
        m.insert((PAGE_CALENDAR, 0x1B), ("Calendar", "OrganizerEmail"));
        m.insert((PAGE_CALENDAR, 0x1C), ("Calendar", "OrganizerName"));
        m.insert((PAGE_CALENDAR, 0x1D), ("Calendar", "Recurrence"));
        m.insert((PAGE_CALENDAR, 0x1E), ("Calendar", "Type"));
        m.insert((PAGE_CALENDAR, 0x1F), ("Calendar", "Until"));
        m.insert((PAGE_CALENDAR, 0x20), ("Calendar", "Occurrences"));
        m.insert((PAGE_CALENDAR, 0x21), ("Calendar", "Interval"));
        m.insert((PAGE_CALENDAR, 0x22), ("Calendar", "DayOfWeek"));
        m.insert((PAGE_CALENDAR, 0x23), ("Calendar", "DayOfMonth"));
        m.insert((PAGE_CALENDAR, 0x24), ("Calendar", "WeekOfMonth"));
        m.insert((PAGE_CALENDAR, 0x25), ("Calendar", "MonthOfYear"));
        m.insert((PAGE_CALENDAR, 0x26), ("Calendar", "Sensitivity"));
        m.insert((PAGE_CALENDAR, 0x27), ("Calendar", "Subject"));
        m.insert((PAGE_CALENDAR, 0x28), ("Calendar", "Start")); // StartTime token, XML element <Start>
        m.insert((PAGE_CALENDAR, 0x29), ("Calendar", "UID"));
        m.insert((PAGE_CALENDAR, 0x2A), ("Calendar", "AttendeeHeartbeatInterval"));
        m.insert((PAGE_CALENDAR, 0x2B), ("Calendar", "Reminder"));
        m.insert((PAGE_CALENDAR, 0x2C), ("Calendar", "DisallowNewTimeProposal"));
        m.insert((PAGE_CALENDAR, 0x2D), ("Calendar", "ResponseRequested"));
        m.insert((PAGE_CALENDAR, 0x2E), ("Calendar", "AppointmentReplyTime"));
        m.insert((PAGE_CALENDAR, 0x2F), ("Calendar", "ResponseType"));
        m.insert((PAGE_CALENDAR, 0x30), ("Calendar", "CalendarType"));
        m.insert((PAGE_CALENDAR, 0x31), ("Calendar", "IsLeapMonth"));
        m.insert((PAGE_CALENDAR, 0x32), ("Calendar", "FirstDayOfWeek"));
        m.insert((PAGE_CALENDAR, 0x33), ("Calendar", "OnlineMeetingConfLink"));
        m.insert((PAGE_CALENDAR, 0x34), ("Calendar", "OnlineMeetingExternalLink"));
        m.insert((PAGE_CALENDAR, 0x35), ("Calendar", "Name"));

        // ── Page 7: FolderHierarchy ───────────────────────────────────────────
        m.insert((PAGE_FOLDERHIERARCHY, 0x05), ("FolderHierarchy", "Folders"));
        m.insert((PAGE_FOLDERHIERARCHY, 0x06), ("FolderHierarchy", "Folder"));
        m.insert((PAGE_FOLDERHIERARCHY, 0x07), ("FolderHierarchy", "DisplayName"));
        m.insert((PAGE_FOLDERHIERARCHY, 0x08), ("FolderHierarchy", "ServerId"));
        m.insert((PAGE_FOLDERHIERARCHY, 0x09), ("FolderHierarchy", "ParentId"));
        m.insert((PAGE_FOLDERHIERARCHY, 0x0A), ("FolderHierarchy", "Type"));
        m.insert((PAGE_FOLDERHIERARCHY, 0x0B), ("FolderHierarchy", "Response"));
        m.insert((PAGE_FOLDERHIERARCHY, 0x0C), ("FolderHierarchy", "Status"));
        m.insert((PAGE_FOLDERHIERARCHY, 0x0D), ("FolderHierarchy", "ContentClass"));
        m.insert((PAGE_FOLDERHIERARCHY, 0x0E), ("FolderHierarchy", "Changes"));
        m.insert((PAGE_FOLDERHIERARCHY, 0x0F), ("FolderHierarchy", "Add"));
        m.insert((PAGE_FOLDERHIERARCHY, 0x10), ("FolderHierarchy", "Remove"));
        m.insert((PAGE_FOLDERHIERARCHY, 0x11), ("FolderHierarchy", "Update"));
        m.insert((PAGE_FOLDERHIERARCHY, 0x12), ("FolderHierarchy", "SyncKey"));
        m.insert((PAGE_FOLDERHIERARCHY, 0x13), ("FolderHierarchy", "FolderCreate"));
        m.insert((PAGE_FOLDERHIERARCHY, 0x14), ("FolderHierarchy", "FolderDelete"));
        m.insert((PAGE_FOLDERHIERARCHY, 0x15), ("FolderHierarchy", "FolderUpdate"));
        m.insert((PAGE_FOLDERHIERARCHY, 0x16), ("FolderHierarchy", "FolderSync"));
        m.insert((PAGE_FOLDERHIERARCHY, 0x17), ("FolderHierarchy", "Count"));

        // ── Page 8: MeetingResponse ───────────────────────────────────────────
        m.insert((PAGE_MEETINGRESPONSE, 0x05), ("MeetingResponse", "MeetingResponse"));
        m.insert((PAGE_MEETINGRESPONSE, 0x06), ("MeetingResponse", "CalendarId"));
        m.insert((PAGE_MEETINGRESPONSE, 0x07), ("MeetingResponse", "CollectionId"));
        m.insert((PAGE_MEETINGRESPONSE, 0x08), ("MeetingResponse", "RequestId"));
        m.insert((PAGE_MEETINGRESPONSE, 0x09), ("MeetingResponse", "Request"));
        m.insert((PAGE_MEETINGRESPONSE, 0x0A), ("MeetingResponse", "Result"));
        m.insert((PAGE_MEETINGRESPONSE, 0x0B), ("MeetingResponse", "Status"));
        m.insert((PAGE_MEETINGRESPONSE, 0x0C), ("MeetingResponse", "UserResponse"));
        m.insert((PAGE_MEETINGRESPONSE, 0x0D), ("MeetingResponse", "Version"));
        m.insert((PAGE_MEETINGRESPONSE, 0x0E), ("MeetingResponse", "InstanceId"));

        // ── Page 13 (0x0D): Ping ─────────────────────────────────────────────
        m.insert((PAGE_PING, 0x05), ("Ping", "Ping"));
        m.insert((PAGE_PING, 0x07), ("Ping", "Status"));
        m.insert((PAGE_PING, 0x08), ("Ping", "HeartbeatInterval"));
        m.insert((PAGE_PING, 0x09), ("Ping", "Folders"));
        m.insert((PAGE_PING, 0x0A), ("Ping", "Folder"));
        m.insert((PAGE_PING, 0x0B), ("Ping", "Id"));
        m.insert((PAGE_PING, 0x0C), ("Ping", "Class"));
        m.insert((PAGE_PING, 0x0D), ("Ping", "MaxFolders"));

        // ── Page 14 (0x0E): Provision ────────────────────────────────────────
        m.insert((PAGE_PROVISION, 0x05), ("Provision", "Provision"));
        m.insert((PAGE_PROVISION, 0x06), ("Provision", "Policies"));
        m.insert((PAGE_PROVISION, 0x07), ("Provision", "Policy"));
        m.insert((PAGE_PROVISION, 0x08), ("Provision", "PolicyType"));
        m.insert((PAGE_PROVISION, 0x09), ("Provision", "PolicyKey"));
        m.insert((PAGE_PROVISION, 0x0A), ("Provision", "Data"));
        m.insert((PAGE_PROVISION, 0x0B), ("Provision", "Status"));
        m.insert((PAGE_PROVISION, 0x0C), ("Provision", "RemoteWipe"));
        m.insert((PAGE_PROVISION, 0x0D), ("Provision", "EASProvisionDoc"));
        m.insert((PAGE_PROVISION, 0x0E), ("Provision", "DevicePasswordEnabled"));
        m.insert((PAGE_PROVISION, 0x0F), ("Provision", "AlphanumericDevicePasswordRequired"));
        m.insert((PAGE_PROVISION, 0x10), ("Provision", "DeviceEncryptionEnabled"));
        m.insert((PAGE_PROVISION, 0x11), ("Provision", "PasswordRecoveryEnabled"));
        m.insert((PAGE_PROVISION, 0x13), ("Provision", "AttachmentsEnabled"));
        m.insert((PAGE_PROVISION, 0x14), ("Provision", "MinDevicePasswordLength"));
        m.insert((PAGE_PROVISION, 0x15), ("Provision", "MaxInactivityTimeDeviceLock"));
        m.insert((PAGE_PROVISION, 0x16), ("Provision", "MaxDevicePasswordFailedAttempts"));
        m.insert((PAGE_PROVISION, 0x17), ("Provision", "MaxAttachmentSize"));
        m.insert((PAGE_PROVISION, 0x18), ("Provision", "AllowSimpleDevicePassword"));
        m.insert((PAGE_PROVISION, 0x19), ("Provision", "DevicePasswordExpiration"));
        m.insert((PAGE_PROVISION, 0x1A), ("Provision", "DevicePasswordHistory"));
        m.insert((PAGE_PROVISION, 0x1B), ("Provision", "AllowStorageCard"));
        m.insert((PAGE_PROVISION, 0x1C), ("Provision", "AllowCamera"));
        m.insert((PAGE_PROVISION, 0x1D), ("Provision", "RequireDeviceEncryption"));
        m.insert((PAGE_PROVISION, 0x1E), ("Provision", "AllowUnsignedApplications"));
        m.insert((PAGE_PROVISION, 0x1F), ("Provision", "AllowUnsignedInstallationPackages"));
        m.insert((PAGE_PROVISION, 0x20), ("Provision", "MinDevicePasswordComplexCharacters"));
        m.insert((PAGE_PROVISION, 0x21), ("Provision", "AllowWiFi"));
        m.insert((PAGE_PROVISION, 0x22), ("Provision", "AllowTextMessaging"));
        m.insert((PAGE_PROVISION, 0x23), ("Provision", "AllowPOPIMAPEmail"));
        m.insert((PAGE_PROVISION, 0x24), ("Provision", "AllowBluetooth"));
        m.insert((PAGE_PROVISION, 0x25), ("Provision", "AllowIrDA"));
        m.insert((PAGE_PROVISION, 0x26), ("Provision", "RequireManualSyncWhenRoaming"));
        m.insert((PAGE_PROVISION, 0x27), ("Provision", "AllowDesktopSync"));
        m.insert((PAGE_PROVISION, 0x28), ("Provision", "MaxCalendarAgeFilter"));
        m.insert((PAGE_PROVISION, 0x29), ("Provision", "AllowHTMLEmail"));
        m.insert((PAGE_PROVISION, 0x2A), ("Provision", "MaxEmailAgeFilter"));
        m.insert((PAGE_PROVISION, 0x2B), ("Provision", "MaxEmailBodyTruncationSize"));
        m.insert((PAGE_PROVISION, 0x2C), ("Provision", "MaxEmailHTMLBodyTruncationSize"));
        m.insert((PAGE_PROVISION, 0x2D), ("Provision", "RequireSignedSMIMEMessages"));
        m.insert((PAGE_PROVISION, 0x2E), ("Provision", "RequireEncryptedSMIMEMessages"));
        m.insert((PAGE_PROVISION, 0x2F), ("Provision", "RequireSignedSMIMEAlgorithm"));
        m.insert((PAGE_PROVISION, 0x30), ("Provision", "RequireEncryptionSMIMEAlgorithm"));
        m.insert((PAGE_PROVISION, 0x31), ("Provision", "AllowSMIMEEncryptionAlgorithmNegotiation"));
        m.insert((PAGE_PROVISION, 0x32), ("Provision", "AllowSMIMESoftCerts"));
        m.insert((PAGE_PROVISION, 0x33), ("Provision", "AllowBrowser"));
        m.insert((PAGE_PROVISION, 0x34), ("Provision", "AllowConsumerEmail"));
        m.insert((PAGE_PROVISION, 0x35), ("Provision", "AllowRemoteDesktop"));
        m.insert((PAGE_PROVISION, 0x36), ("Provision", "AllowInternetSharing"));
        m.insert((PAGE_PROVISION, 0x37), ("Provision", "UnapprovedInROMApplicationList"));
        m.insert((PAGE_PROVISION, 0x38), ("Provision", "ApplicationName"));
        m.insert((PAGE_PROVISION, 0x39), ("Provision", "ApprovedApplicationList"));
        m.insert((PAGE_PROVISION, 0x3A), ("Provision", "Hash"));

        // ── Page 17 (0x11): AirSyncBase ──────────────────────────────────────
        m.insert((PAGE_AIRSYNCBASE, 0x05), ("AirSyncBase", "BodyPreference"));
        m.insert((PAGE_AIRSYNCBASE, 0x06), ("AirSyncBase", "Type"));
        m.insert((PAGE_AIRSYNCBASE, 0x07), ("AirSyncBase", "TruncationSize"));
        m.insert((PAGE_AIRSYNCBASE, 0x08), ("AirSyncBase", "AllOrNone"));
        // 0x09 is reserved in the spec
        m.insert((PAGE_AIRSYNCBASE, 0x0A), ("AirSyncBase", "Body"));
        m.insert((PAGE_AIRSYNCBASE, 0x0B), ("AirSyncBase", "Data"));
        m.insert((PAGE_AIRSYNCBASE, 0x0C), ("AirSyncBase", "EstimatedDataSize"));
        m.insert((PAGE_AIRSYNCBASE, 0x0D), ("AirSyncBase", "Truncated"));
        m.insert((PAGE_AIRSYNCBASE, 0x0E), ("AirSyncBase", "Attachments"));
        m.insert((PAGE_AIRSYNCBASE, 0x0F), ("AirSyncBase", "Attachment"));
        m.insert((PAGE_AIRSYNCBASE, 0x10), ("AirSyncBase", "DisplayName"));
        m.insert((PAGE_AIRSYNCBASE, 0x11), ("AirSyncBase", "FileReference"));
        m.insert((PAGE_AIRSYNCBASE, 0x12), ("AirSyncBase", "Method"));
        m.insert((PAGE_AIRSYNCBASE, 0x13), ("AirSyncBase", "ContentId"));
        m.insert((PAGE_AIRSYNCBASE, 0x14), ("AirSyncBase", "ContentLocation"));
        m.insert((PAGE_AIRSYNCBASE, 0x15), ("AirSyncBase", "IsInline"));
        m.insert((PAGE_AIRSYNCBASE, 0x16), ("AirSyncBase", "NativeBodyType"));
        m.insert((PAGE_AIRSYNCBASE, 0x17), ("AirSyncBase", "ContentType"));
        m.insert((PAGE_AIRSYNCBASE, 0x18), ("AirSyncBase", "Preview"));
        m.insert((PAGE_AIRSYNCBASE, 0x19), ("AirSyncBase", "BodyPartPreference"));
        m.insert((PAGE_AIRSYNCBASE, 0x1A), ("AirSyncBase", "BodyPart"));
        m.insert((PAGE_AIRSYNCBASE, 0x1B), ("AirSyncBase", "Status"));

        // ── Page 18 (0x12): Settings ─────────────────────────────────────────
        m.insert((PAGE_SETTINGS, 0x05), ("Settings", "Settings"));
        m.insert((PAGE_SETTINGS, 0x06), ("Settings", "Status"));
        m.insert((PAGE_SETTINGS, 0x07), ("Settings", "Get"));
        m.insert((PAGE_SETTINGS, 0x08), ("Settings", "Set"));
        m.insert((PAGE_SETTINGS, 0x09), ("Settings", "Oof"));
        m.insert((PAGE_SETTINGS, 0x0A), ("Settings", "OofState"));
        m.insert((PAGE_SETTINGS, 0x0B), ("Settings", "StartTime"));
        m.insert((PAGE_SETTINGS, 0x0C), ("Settings", "EndTime"));
        m.insert((PAGE_SETTINGS, 0x0D), ("Settings", "OofMessage"));
        m.insert((PAGE_SETTINGS, 0x0E), ("Settings", "AppliesToInternal"));
        m.insert((PAGE_SETTINGS, 0x0F), ("Settings", "AppliesToExternalKnown"));
        m.insert((PAGE_SETTINGS, 0x10), ("Settings", "AppliesToExternalUnknown"));
        m.insert((PAGE_SETTINGS, 0x11), ("Settings", "Enabled"));
        m.insert((PAGE_SETTINGS, 0x12), ("Settings", "ReplyMessage"));
        m.insert((PAGE_SETTINGS, 0x13), ("Settings", "BodyType"));
        m.insert((PAGE_SETTINGS, 0x14), ("Settings", "Data"));
        m.insert((PAGE_SETTINGS, 0x15), ("Settings", "DevicePassword"));
        m.insert((PAGE_SETTINGS, 0x16), ("Settings", "Password"));
        m.insert((PAGE_SETTINGS, 0x17), ("Settings", "DeviceInformation"));
        m.insert((PAGE_SETTINGS, 0x18), ("Settings", "Model"));
        m.insert((PAGE_SETTINGS, 0x19), ("Settings", "IMEI"));
        m.insert((PAGE_SETTINGS, 0x1A), ("Settings", "FriendlyName"));
        m.insert((PAGE_SETTINGS, 0x1B), ("Settings", "OS"));
        m.insert((PAGE_SETTINGS, 0x1C), ("Settings", "OSLanguage"));
        m.insert((PAGE_SETTINGS, 0x1D), ("Settings", "PhoneNumber"));
        m.insert((PAGE_SETTINGS, 0x1E), ("Settings", "UserInformation"));
        m.insert((PAGE_SETTINGS, 0x1F), ("Settings", "EmailAddresses"));
        m.insert((PAGE_SETTINGS, 0x20), ("Settings", "SmtpAddress"));
        m.insert((PAGE_SETTINGS, 0x21), ("Settings", "UserAgent"));
        m.insert((PAGE_SETTINGS, 0x22), ("Settings", "EnableOutboundSMS"));
        m.insert((PAGE_SETTINGS, 0x23), ("Settings", "MobileOperator"));
        m.insert((PAGE_SETTINGS, 0x24), ("Settings", "PrimarySmtpAddress"));
        m.insert((PAGE_SETTINGS, 0x25), ("Settings", "Accounts"));
        m.insert((PAGE_SETTINGS, 0x26), ("Settings", "Account"));
        m.insert((PAGE_SETTINGS, 0x27), ("Settings", "AccountId"));
        m.insert((PAGE_SETTINGS, 0x28), ("Settings", "AccountName"));
        m.insert((PAGE_SETTINGS, 0x29), ("Settings", "SendDisabled"));
        m.insert((PAGE_SETTINGS, 0x2B), ("Settings", "RightsManagementInformation"));

        m
    });

// ── Reverse map: (namespace-prefix, element-name) → (page, token) ─────────────
// For AirSync-page elements the response XML in active_sync.rs omits the
// "AirSync:" prefix (uses a default-namespace declaration instead), so we
// register every AirSync tag under both ("AirSync", name) and ("", name).
static REV_TAG_MAP: LazyLock<HashMap<(&'static str, &'static str), (u8, u8)>> =
    LazyLock::new(|| {
        let mut m: HashMap<(&'static str, &'static str), (u8, u8)> = HashMap::new();
        for ((page, token), (ns, name)) in TAG_MAP.iter() {
            m.insert((*ns, *name), (*page, *token));
            // Allow no-prefix lookup for AirSync page elements
            if *page == PAGE_AIRSYNC {
                m.entry(("", *name)).or_insert((*page, *token));
            }
        }
        m
    });

// ─────────────────────────────────────────────────────────────────────────────
// Public API
// ─────────────────────────────────────────────────────────────────────────────

/// Decodes an EAS WBXML byte stream into a UTF-8 XML string.
///
/// Properly parses the variable-length WBXML 1.3 header so it is compatible
/// with both the 4-byte standard header that real devices send **and** the
/// 6-byte header that our `encode` function produces.
pub fn decode(input: &[u8]) -> Result<String, anyhow::Error> {
    if input.len() < 4 {
        return Err(anyhow::anyhow!("Input too short to be valid WBXML"));
    }

    // ── Parse variable-length WBXML header ───────────────────────────────────
    let mut pos = 0usize;

    // version (1 byte)
    pos += 1;

    // public identifier (multibyte int)
    // If the value is 0 the actual id is a string-table reference (another int follows).
    let (pubid, n) = read_multibyte_int(&input[pos..])?;
    pos += n;
    if pubid == 0 {
        // string-table index for the public id – skip it
        let (_, m) = read_multibyte_int(&input[pos..])?;
        pos += m;
    }

    // charset (multibyte int)
    let (_, n) = read_multibyte_int(&input[pos..])?;
    pos += n;

    // string table: length (multibyte int) then the table bytes
    let (strtbl_len, n) = read_multibyte_int(&input[pos..])?;
    pos += n;
    if pos + strtbl_len > input.len() {
        return Err(anyhow::anyhow!("WBXML string table extends beyond input"));
    }
    pos += strtbl_len;

    // ── Decode body tokens ────────────────────────────────────────────────────
    let mut output = String::with_capacity(input.len() * 4);
    output.push_str(r#"<?xml version="1.0" encoding="utf-8"?>"#);

    let mut current_page = 0u8;
    let mut tag_stack: Vec<String> = Vec::new();

    while pos < input.len() {
        let token = input[pos];
        pos += 1;

        match token {
            SWITCH_PAGE => {
                if pos >= input.len() {
                    return Err(anyhow::anyhow!("Truncated WBXML after SWITCH_PAGE"));
                }
                current_page = input[pos];
                pos += 1;
            }

            END => {
                if let Some(tag) = tag_stack.pop() {
                    output.push_str("</");
                    output.push_str(&tag);
                    output.push('>');
                }
            }

            STR_I => {
                let start = pos;
                while pos < input.len() && input[pos] != 0x00 {
                    pos += 1;
                }
                let raw = String::from_utf8_lossy(&input[start..pos]).into_owned();
                output.push_str(&quick_xml::escape::escape(&raw));
                if pos < input.len() {
                    pos += 1; // consume null terminator
                }
            }

            OPAQUE => {
                let (len, bytes_read) = read_multibyte_int(&input[pos..])?;
                pos += bytes_read;
                if pos + len > input.len() {
                    return Err(anyhow::anyhow!(
                        "WBXML OPAQUE data length {} exceeds remaining input",
                        len
                    ));
                }
                let data = &input[pos..pos + len];
                if let Ok(s) = std::str::from_utf8(data) {
                    output.push_str(&quick_xml::escape::escape(s));
                } else {
                    // binary data – base64-encode it
                    output.push_str(&general_purpose::STANDARD.encode(data));
                }
                pos += len;
            }

            _ => {
                let has_content = (token & 0x40) != 0;
                let tag_byte = token & 0x3F;

                let full_name = match TAG_MAP.get(&(current_page, tag_byte)) {
                    Some((ns, name)) if !ns.is_empty() => format!("{}:{}", ns, name),
                    Some((_ns, name)) => (*name).to_string(),
                    None => {
                        tracing::warn!(
                            "Unknown WBXML token: page={:#04x} tag={:#04x}",
                            current_page,
                            tag_byte
                        );
                        format!("Unknown_{:02x}_{:02x}", current_page, tag_byte)
                    }
                };

                output.push('<');
                output.push_str(&full_name);
                if has_content {
                    output.push('>');
                    tag_stack.push(full_name);
                } else {
                    output.push_str("/>");
                }
            }
        }
    }

    // Close any tags left unclosed (malformed input tolerance)
    for tag in tag_stack.into_iter().rev() {
        tracing::warn!("WBXML decode: auto-closing unclosed tag <{}>", tag);
        output.push_str("</");
        output.push_str(&tag);
        output.push('>');
    }

    Ok(output)
}

/// Encodes an XML string into EAS WBXML bytes.
///
/// The WBXML header written is `03 01 6A 00` (version 1.3, unknown public-id,
/// UTF-8, empty string table).  XML declarations and processing instructions
/// are silently skipped.  `Event::Start` and `Event::Empty` are handled in
/// separate match arms – the previous combined arm incorrectly consumed an
/// extra token from the stream.
pub fn encode(xml: &str) -> Result<Vec<u8>, anyhow::Error> {
    // WBXML 1.3 header: version=0x03  pubid=0x01  charset=0x6A(UTF-8)  strtbl_len=0x00
    let mut output: Vec<u8> = vec![0x03, 0x01, 0x6A, 0x00];
    let mut reader = Reader::from_str(xml);
    let mut current_page: u8 = 0xFF; // sentinel: no page switched yet
    let mut buf = Vec::new();

    loop {
        buf.clear();
        match reader.read_event_into(&mut buf) {
            // ── Opening tag with content ──────────────────────────────────────
            Ok(Event::Start(ref e)) => {
                let tag_byte = resolve_tag(e, &mut output, &mut current_page);
                output.push(tag_byte | 0x40); // set content bit
            }

            // ── Self-closing / empty element ──────────────────────────────────
            Ok(Event::Empty(ref e)) => {
                let tag_byte = resolve_tag(e, &mut output, &mut current_page);
                output.push(tag_byte); // no content bit
            }

            // ── Closing tag ───────────────────────────────────────────────────
            Ok(Event::End(_)) => {
                output.push(END);
            }

            // ── Text content ──────────────────────────────────────────────────
            Ok(Event::Text(ref t)) => {
                // unescape() converts XML entities (&amp; → &, etc.) before
                // writing the raw bytes into the WBXML inline-string payload.
                let text = t.unescape().unwrap_or_default();
                let bytes = text.as_bytes();
                if !bytes.is_empty() {
                    output.push(STR_I);
                    output.extend_from_slice(bytes);
                    output.push(0x00); // null terminator
                }
            }

            // ── Skip XML declaration, PIs, comments ───────────────────────────
            Ok(Event::Decl(_)) | Ok(Event::PI(_)) | Ok(Event::Comment(_)) => {}

            Ok(Event::Eof) => break,

            Err(e) => {
                return Err(anyhow::anyhow!("XML parse error during WBXML encode: {}", e));
            }

            _ => {}
        }
    }

    Ok(output)
}

// ─────────────────────────────────────────────────────────────────────────────
// Private helpers
// ─────────────────────────────────────────────────────────────────────────────

/// Determines the code page and tag byte for a quick_xml element, emits a
/// `SWITCH_PAGE` token if the page has changed, and returns the raw tag byte
/// (without the content flag).
fn resolve_tag(
    e: &quick_xml::events::BytesStart<'_>,
    output: &mut Vec<u8>,
    current_page: &mut u8,
) -> u8 {
    let full_name = std::str::from_utf8(e.name().as_ref()).unwrap_or("");
    let (ns, local) = match full_name.split_once(':') {
        Some((n, l)) => (n, l),
        None => ("", full_name),
    };

    // Map namespace prefix → EAS code page
    let page: u8 = match ns {
        "AirSync" => PAGE_AIRSYNC,
        "Calendar" => PAGE_CALENDAR,
        "FolderHierarchy" => PAGE_FOLDERHIERARCHY,
        "MeetingResponse" => PAGE_MEETINGRESPONSE,
        "Ping" => PAGE_PING,
        "Provision" => PAGE_PROVISION,
        "AirSyncBase" => PAGE_AIRSYNCBASE,
        "Settings" => PAGE_SETTINGS,
        // No prefix → default namespace; assume AirSync (most response elements)
        _ => PAGE_AIRSYNC,
    };

    if page != *current_page {
        output.push(SWITCH_PAGE);
        output.push(page);
        *current_page = page;
    }

    // Look up tag token; ("", local) covers un-prefixed AirSync elements
    match REV_TAG_MAP.get(&(ns, local)) {
        Some((_, t)) => *t,
        None => {
            tracing::warn!(
                "No WBXML token for element '{}:{}' on page {:#04x}; using 0xFF",
                ns,
                local,
                page
            );
            0xFF
        }
    }
}

/// Reads a WBXML multi-byte integer (big-endian, 7 bits per byte, MSB=continuation).
/// Returns `(value, bytes_consumed)`.
fn read_multibyte_int(buf: &[u8]) -> Result<(usize, usize), anyhow::Error> {
    let mut result: usize = 0;
    let mut count = 0usize;
    loop {
        if count >= buf.len() {
            return Err(anyhow::anyhow!(
                "Unexpected end of input reading WBXML multibyte integer"
            ));
        }
        let byte = buf[count];
        count += 1;
        result = (result << 7) | ((byte & 0x7F) as usize);
        if byte & 0x80 == 0 {
            break;
        }
        if count > 5 {
            // WBXML multibyte ints are at most 5 bytes (35 usable bits)
            return Err(anyhow::anyhow!("WBXML multibyte integer overflow"));
        }
    }
    Ok((result, count))
}
