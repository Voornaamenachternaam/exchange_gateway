// src/wbxml.rs
use base64::{Engine as _, engine::general_purpose};
use quick_xml::{Reader, events::Event};
use std::collections::HashMap;
use std::sync::LazyLock;

// ── WBXML 1.3 global token constants (MS-ASWBXML §1.6) ───────────────────────
const SWITCH_PAGE: u8 = 0x00;
const END: u8 = 0x01;
const STR_I: u8 = 0x03;
const OPAQUE: u8 = 0xC3;

// ── EAS code-page identifiers (MS-ASWBXML §2.1.2.1, v20250520) ───────────────
const PAGE_AIRSYNC: u8 = 0x00; //  0 – AirSync
const PAGE_CALENDAR: u8 = 0x04; //  4 – Calendar
const PAGE_FOLDERHIERARCHY: u8 = 0x07; //  7 – FolderHierarchy
const PAGE_MEETINGRESPONSE: u8 = 0x08; //  8 – MeetingResponse
const PAGE_PING: u8 = 0x0D; // 13 – Ping
const PAGE_PROVISION: u8 = 0x0E; // 14 – Provision
const PAGE_AIRSYNCBASE: u8 = 0x11; // 17 – AirSyncBase
const PAGE_SETTINGS: u8 = 0x12; // 18 – Settings
const PAGE_COMPOSEMAIL: u8 = 0x15; // 21 – ComposeMail (SendMail)

// ── Forward map: (page, token_byte) → (ns_prefix, xml_element_name) ──────────
//
// All token values are taken verbatim from MS-ASWBXML v20250520 tables.
// Only pages actually used by this gateway are populated; unknown tokens are
// handled gracefully at runtime.
static TAG_MAP: LazyLock<HashMap<(u8, u8), (&'static str, &'static str)>> = LazyLock::new(|| {
    let mut m: HashMap<(u8, u8), (&'static str, &'static str)> = HashMap::new();

    // ── Page 0: AirSync (MS-ASWBXML §2.1.2.1.1) ─────────────────────────
    m.insert((PAGE_AIRSYNC, 0x05), ("AirSync", "Sync"));
    m.insert((PAGE_AIRSYNC, 0x06), ("AirSync", "Responses"));
    m.insert((PAGE_AIRSYNC, 0x07), ("AirSync", "Add"));
    m.insert((PAGE_AIRSYNC, 0x08), ("AirSync", "Change"));
    m.insert((PAGE_AIRSYNC, 0x09), ("AirSync", "Delete"));
    m.insert((PAGE_AIRSYNC, 0x0A), ("AirSync", "Fetch"));
    m.insert((PAGE_AIRSYNC, 0x0B), ("AirSync", "SyncKey"));
    m.insert((PAGE_AIRSYNC, 0x0C), ("AirSync", "ClientId"));
    m.insert((PAGE_AIRSYNC, 0x0D), ("AirSync", "ServerId"));
    m.insert((PAGE_AIRSYNC, 0x0E), ("AirSync", "Status"));
    m.insert((PAGE_AIRSYNC, 0x0F), ("AirSync", "Collection"));
    m.insert((PAGE_AIRSYNC, 0x10), ("AirSync", "Class"));
    m.insert((PAGE_AIRSYNC, 0x12), ("AirSync", "CollectionId"));
    m.insert((PAGE_AIRSYNC, 0x13), ("AirSync", "GetChanges"));
    m.insert((PAGE_AIRSYNC, 0x14), ("AirSync", "MoreAvailable"));
    m.insert((PAGE_AIRSYNC, 0x15), ("AirSync", "WindowSize"));
    m.insert((PAGE_AIRSYNC, 0x16), ("AirSync", "Commands"));
    m.insert((PAGE_AIRSYNC, 0x17), ("AirSync", "Options"));
    m.insert((PAGE_AIRSYNC, 0x18), ("AirSync", "FilterType"));
    m.insert((PAGE_AIRSYNC, 0x19), ("AirSync", "Truncation"));
    m.insert((PAGE_AIRSYNC, 0x1B), ("AirSync", "Conflict"));
    m.insert((PAGE_AIRSYNC, 0x1C), ("AirSync", "Collections"));
    m.insert((PAGE_AIRSYNC, 0x1D), ("AirSync", "ApplicationData"));
    m.insert((PAGE_AIRSYNC, 0x1E), ("AirSync", "DeletesAsMoves"));
    m.insert((PAGE_AIRSYNC, 0x20), ("AirSync", "Supported"));
    m.insert((PAGE_AIRSYNC, 0x21), ("AirSync", "SoftDelete"));
    m.insert((PAGE_AIRSYNC, 0x22), ("AirSync", "MIMESupport"));
    m.insert((PAGE_AIRSYNC, 0x23), ("AirSync", "MIMETruncation"));
    m.insert((PAGE_AIRSYNC, 0x24), ("AirSync", "Wait"));
    m.insert((PAGE_AIRSYNC, 0x25), ("AirSync", "Limit"));
    m.insert((PAGE_AIRSYNC, 0x26), ("AirSync", "Partial"));
    m.insert((PAGE_AIRSYNC, 0x27), ("AirSync", "ConversationMode"));
    m.insert((PAGE_AIRSYNC, 0x28), ("AirSync", "MaxItems"));
    m.insert((PAGE_AIRSYNC, 0x29), ("AirSync", "HeartbeatInterval"));

    // ── Page 4: Calendar (MS-ASWBXML §2.1.2.1.5) ────────────────────────
    m.insert((PAGE_CALENDAR, 0x05), ("Calendar", "Timezone"));
    m.insert((PAGE_CALENDAR, 0x06), ("Calendar", "AllDayEvent"));
    m.insert((PAGE_CALENDAR, 0x07), ("Calendar", "Attendees"));
    m.insert((PAGE_CALENDAR, 0x08), ("Calendar", "Attendee"));
    m.insert((PAGE_CALENDAR, 0x09), ("Calendar", "Email"));
    m.insert((PAGE_CALENDAR, 0x0A), ("Calendar", "Name"));
    m.insert((PAGE_CALENDAR, 0x0B), ("Calendar", "Body")); // legacy 2.5
    m.insert((PAGE_CALENDAR, 0x0C), ("Calendar", "BodyTruncated")); // legacy 2.5
    m.insert((PAGE_CALENDAR, 0x0D), ("Calendar", "BusyStatus"));
    m.insert((PAGE_CALENDAR, 0x0E), ("Calendar", "Categories"));
    m.insert((PAGE_CALENDAR, 0x0F), ("Calendar", "Category"));
    m.insert((PAGE_CALENDAR, 0x11), ("Calendar", "DtStamp"));
    m.insert((PAGE_CALENDAR, 0x12), ("Calendar", "EndTime"));
    m.insert((PAGE_CALENDAR, 0x13), ("Calendar", "Exception"));
    m.insert((PAGE_CALENDAR, 0x14), ("Calendar", "Exceptions"));
    m.insert((PAGE_CALENDAR, 0x15), ("Calendar", "Deleted"));
    m.insert((PAGE_CALENDAR, 0x16), ("Calendar", "ExceptionStartTime"));
    m.insert((PAGE_CALENDAR, 0x17), ("Calendar", "Location"));
    m.insert((PAGE_CALENDAR, 0x18), ("Calendar", "MeetingStatus"));
    m.insert((PAGE_CALENDAR, 0x19), ("Calendar", "OrganizerEmail"));
    m.insert((PAGE_CALENDAR, 0x1A), ("Calendar", "OrganizerName"));
    m.insert((PAGE_CALENDAR, 0x1B), ("Calendar", "Recurrence"));
    m.insert((PAGE_CALENDAR, 0x1C), ("Calendar", "Type"));
    m.insert((PAGE_CALENDAR, 0x1D), ("Calendar", "Until"));
    m.insert((PAGE_CALENDAR, 0x1E), ("Calendar", "Occurrences"));
    m.insert((PAGE_CALENDAR, 0x1F), ("Calendar", "Interval"));
    m.insert((PAGE_CALENDAR, 0x20), ("Calendar", "DayOfWeek"));
    m.insert((PAGE_CALENDAR, 0x21), ("Calendar", "DayOfMonth"));
    m.insert((PAGE_CALENDAR, 0x22), ("Calendar", "WeekOfMonth"));
    m.insert((PAGE_CALENDAR, 0x23), ("Calendar", "MonthOfYear"));
    m.insert((PAGE_CALENDAR, 0x24), ("Calendar", "Reminder"));
    m.insert((PAGE_CALENDAR, 0x25), ("Calendar", "Sensitivity"));
    m.insert((PAGE_CALENDAR, 0x26), ("Calendar", "Subject"));
    m.insert((PAGE_CALENDAR, 0x27), ("Calendar", "StartTime"));
    m.insert((PAGE_CALENDAR, 0x28), ("Calendar", "UID"));
    m.insert((PAGE_CALENDAR, 0x29), ("Calendar", "AttendeeStatus"));
    m.insert((PAGE_CALENDAR, 0x2A), ("Calendar", "AttendeeType"));
    m.insert(
        (PAGE_CALENDAR, 0x33),
        ("Calendar", "DisallowNewTimeProposal"),
    );
    m.insert((PAGE_CALENDAR, 0x34), ("Calendar", "ResponseRequested"));
    m.insert((PAGE_CALENDAR, 0x35), ("Calendar", "AppointmentReplyTime"));
    m.insert((PAGE_CALENDAR, 0x36), ("Calendar", "ResponseType"));
    m.insert((PAGE_CALENDAR, 0x37), ("Calendar", "CalendarType"));
    m.insert((PAGE_CALENDAR, 0x38), ("Calendar", "IsLeapMonth"));
    m.insert((PAGE_CALENDAR, 0x39), ("Calendar", "FirstDayOfWeek"));
    m.insert((PAGE_CALENDAR, 0x3A), ("Calendar", "OnlineMeetingConfLink"));
    m.insert(
        (PAGE_CALENDAR, 0x3B),
        ("Calendar", "OnlineMeetingExternalLink"),
    );
    m.insert((PAGE_CALENDAR, 0x3C), ("Calendar", "ClientUid"));

    // ── Page 7: FolderHierarchy (MS-ASWBXML §2.1.2.1.8) ─────────────────
    m.insert((PAGE_FOLDERHIERARCHY, 0x05), ("FolderHierarchy", "Folders"));
    m.insert((PAGE_FOLDERHIERARCHY, 0x06), ("FolderHierarchy", "Folder"));
    m.insert(
        (PAGE_FOLDERHIERARCHY, 0x07),
        ("FolderHierarchy", "DisplayName"),
    );
    m.insert(
        (PAGE_FOLDERHIERARCHY, 0x08),
        ("FolderHierarchy", "ServerId"),
    );
    m.insert(
        (PAGE_FOLDERHIERARCHY, 0x09),
        ("FolderHierarchy", "ParentId"),
    );
    m.insert((PAGE_FOLDERHIERARCHY, 0x0A), ("FolderHierarchy", "Type"));
    m.insert((PAGE_FOLDERHIERARCHY, 0x0C), ("FolderHierarchy", "Status"));
    m.insert((PAGE_FOLDERHIERARCHY, 0x0E), ("FolderHierarchy", "Changes"));
    m.insert((PAGE_FOLDERHIERARCHY, 0x0F), ("FolderHierarchy", "Add"));
    m.insert((PAGE_FOLDERHIERARCHY, 0x10), ("FolderHierarchy", "Delete"));
    m.insert((PAGE_FOLDERHIERARCHY, 0x11), ("FolderHierarchy", "Update"));
    m.insert((PAGE_FOLDERHIERARCHY, 0x12), ("FolderHierarchy", "SyncKey"));
    m.insert(
        (PAGE_FOLDERHIERARCHY, 0x13),
        ("FolderHierarchy", "FolderCreate"),
    );
    m.insert(
        (PAGE_FOLDERHIERARCHY, 0x14),
        ("FolderHierarchy", "FolderDelete"),
    );
    m.insert(
        (PAGE_FOLDERHIERARCHY, 0x15),
        ("FolderHierarchy", "FolderUpdate"),
    );
    m.insert(
        (PAGE_FOLDERHIERARCHY, 0x16),
        ("FolderHierarchy", "FolderSync"),
    );
    m.insert((PAGE_FOLDERHIERARCHY, 0x17), ("FolderHierarchy", "Count"));

    // ── Page 8: MeetingResponse (MS-ASWBXML §2.1.2.1.9) ─────────────────
    m.insert(
        (PAGE_MEETINGRESPONSE, 0x05),
        ("MeetingResponse", "CalendarId"),
    );
    m.insert(
        (PAGE_MEETINGRESPONSE, 0x06),
        ("MeetingResponse", "CollectionId"),
    );
    m.insert(
        (PAGE_MEETINGRESPONSE, 0x07),
        ("MeetingResponse", "MeetingResponse"),
    );
    m.insert(
        (PAGE_MEETINGRESPONSE, 0x08),
        ("MeetingResponse", "RequestId"),
    );
    m.insert((PAGE_MEETINGRESPONSE, 0x09), ("MeetingResponse", "Request"));
    m.insert((PAGE_MEETINGRESPONSE, 0x0A), ("MeetingResponse", "Result"));
    m.insert((PAGE_MEETINGRESPONSE, 0x0B), ("MeetingResponse", "Status"));
    m.insert(
        (PAGE_MEETINGRESPONSE, 0x0C),
        ("MeetingResponse", "UserResponse"),
    );
    m.insert(
        (PAGE_MEETINGRESPONSE, 0x0E),
        ("MeetingResponse", "InstanceId"),
    );
    m.insert(
        (PAGE_MEETINGRESPONSE, 0x10),
        ("MeetingResponse", "ProposedStartTime"),
    );
    m.insert(
        (PAGE_MEETINGRESPONSE, 0x11),
        ("MeetingResponse", "ProposedEndTime"),
    );
    m.insert(
        (PAGE_MEETINGRESPONSE, 0x12),
        ("MeetingResponse", "SendResponse"),
    );

    // ── Page 13 (0x0D): Ping (MS-ASWBXML §2.1.2.1.14) ───────────────────
    m.insert((PAGE_PING, 0x05), ("Ping", "Ping"));
    m.insert((PAGE_PING, 0x07), ("Ping", "Status"));
    m.insert((PAGE_PING, 0x08), ("Ping", "HeartbeatInterval"));
    m.insert((PAGE_PING, 0x09), ("Ping", "Folders"));
    m.insert((PAGE_PING, 0x0A), ("Ping", "Folder"));
    m.insert((PAGE_PING, 0x0B), ("Ping", "Id"));
    m.insert((PAGE_PING, 0x0C), ("Ping", "Class"));
    m.insert((PAGE_PING, 0x0D), ("Ping", "MaxFolders"));

    // ── Page 14 (0x0E): Provision (MS-ASWBXML §2.1.2.1.15) ──────────────
    m.insert((PAGE_PROVISION, 0x05), ("Provision", "Provision"));
    m.insert((PAGE_PROVISION, 0x06), ("Provision", "Policies"));
    m.insert((PAGE_PROVISION, 0x07), ("Provision", "Policy"));
    m.insert((PAGE_PROVISION, 0x08), ("Provision", "PolicyType"));
    m.insert((PAGE_PROVISION, 0x09), ("Provision", "PolicyKey"));
    m.insert((PAGE_PROVISION, 0x0A), ("Provision", "Data"));
    m.insert((PAGE_PROVISION, 0x0B), ("Provision", "Status"));
    m.insert((PAGE_PROVISION, 0x0C), ("Provision", "RemoteWipe"));
    m.insert((PAGE_PROVISION, 0x0D), ("Provision", "EASProvisionDoc"));
    m.insert(
        (PAGE_PROVISION, 0x0E),
        ("Provision", "DevicePasswordEnabled"),
    );
    m.insert(
        (PAGE_PROVISION, 0x0F),
        ("Provision", "AlphanumericDevicePasswordRequired"),
    );
    m.insert(
        (PAGE_PROVISION, 0x10),
        ("Provision", "RequireStorageCardEncryption"),
    );
    m.insert(
        (PAGE_PROVISION, 0x11),
        ("Provision", "PasswordRecoveryEnabled"),
    );
    m.insert((PAGE_PROVISION, 0x13), ("Provision", "AttachmentsEnabled"));
    m.insert(
        (PAGE_PROVISION, 0x14),
        ("Provision", "MinDevicePasswordLength"),
    );
    m.insert(
        (PAGE_PROVISION, 0x15),
        ("Provision", "MaxInactivityTimeDeviceLock"),
    );
    m.insert(
        (PAGE_PROVISION, 0x16),
        ("Provision", "MaxDevicePasswordFailedAttempts"),
    );
    m.insert((PAGE_PROVISION, 0x17), ("Provision", "MaxAttachmentSize"));
    m.insert(
        (PAGE_PROVISION, 0x18),
        ("Provision", "AllowSimpleDevicePassword"),
    );
    m.insert(
        (PAGE_PROVISION, 0x19),
        ("Provision", "DevicePasswordExpiration"),
    );
    m.insert(
        (PAGE_PROVISION, 0x1A),
        ("Provision", "DevicePasswordHistory"),
    );
    m.insert((PAGE_PROVISION, 0x1B), ("Provision", "AllowStorageCard"));
    m.insert((PAGE_PROVISION, 0x1C), ("Provision", "AllowCamera"));
    m.insert(
        (PAGE_PROVISION, 0x1D),
        ("Provision", "RequireDeviceEncryption"),
    );
    m.insert(
        (PAGE_PROVISION, 0x1E),
        ("Provision", "AllowUnsignedApplications"),
    );
    m.insert(
        (PAGE_PROVISION, 0x1F),
        ("Provision", "AllowUnsignedInstallationPackages"),
    );
    m.insert(
        (PAGE_PROVISION, 0x20),
        ("Provision", "MinDevicePasswordComplexCharacters"),
    );
    m.insert((PAGE_PROVISION, 0x21), ("Provision", "AllowWiFi"));
    m.insert((PAGE_PROVISION, 0x22), ("Provision", "AllowTextMessaging"));
    m.insert((PAGE_PROVISION, 0x23), ("Provision", "AllowPOPIMAPEmail"));
    m.insert((PAGE_PROVISION, 0x24), ("Provision", "AllowBluetooth"));
    m.insert((PAGE_PROVISION, 0x25), ("Provision", "AllowIrDA"));
    m.insert(
        (PAGE_PROVISION, 0x26),
        ("Provision", "RequireManualSyncWhenRoaming"),
    );
    m.insert((PAGE_PROVISION, 0x27), ("Provision", "AllowDesktopSync"));
    m.insert(
        (PAGE_PROVISION, 0x28),
        ("Provision", "MaxCalendarAgeFilter"),
    );
    m.insert((PAGE_PROVISION, 0x29), ("Provision", "AllowHTMLEmail"));
    m.insert((PAGE_PROVISION, 0x2A), ("Provision", "MaxEmailAgeFilter"));
    m.insert(
        (PAGE_PROVISION, 0x2B),
        ("Provision", "MaxEmailBodyTruncationSize"),
    );
    m.insert(
        (PAGE_PROVISION, 0x2C),
        ("Provision", "MaxEmailHTMLBodyTruncationSize"),
    );
    m.insert(
        (PAGE_PROVISION, 0x2D),
        ("Provision", "RequireSignedSMIMEMessages"),
    );
    m.insert(
        (PAGE_PROVISION, 0x2E),
        ("Provision", "RequireEncryptedSMIMEMessages"),
    );
    m.insert(
        (PAGE_PROVISION, 0x2F),
        ("Provision", "RequireSignedSMIMEAlgorithm"),
    );
    m.insert(
        (PAGE_PROVISION, 0x30),
        ("Provision", "RequireEncryptionSMIMEAlgorithm"),
    );
    m.insert(
        (PAGE_PROVISION, 0x31),
        ("Provision", "AllowSMIMEEncryptionAlgorithmNegotiation"),
    );
    m.insert((PAGE_PROVISION, 0x32), ("Provision", "AllowSMIMESoftCerts"));
    m.insert((PAGE_PROVISION, 0x33), ("Provision", "AllowBrowser"));
    m.insert((PAGE_PROVISION, 0x34), ("Provision", "AllowConsumerEmail"));
    m.insert((PAGE_PROVISION, 0x35), ("Provision", "AllowRemoteDesktop"));
    m.insert(
        (PAGE_PROVISION, 0x36),
        ("Provision", "AllowInternetSharing"),
    );
    m.insert(
        (PAGE_PROVISION, 0x37),
        ("Provision", "UnapprovedInROMApplicationList"),
    );
    m.insert((PAGE_PROVISION, 0x38), ("Provision", "ApplicationName"));
    m.insert(
        (PAGE_PROVISION, 0x39),
        ("Provision", "ApprovedApplicationList"),
    );
    m.insert((PAGE_PROVISION, 0x3A), ("Provision", "Hash"));
    m.insert(
        (PAGE_PROVISION, 0x3B),
        ("Provision", "AccountOnlyRemoteWipe"),
    );

    // ── Page 17 (0x11): AirSyncBase (MS-ASWBXML §2.1.2.1.18) ────────────
    m.insert((PAGE_AIRSYNCBASE, 0x05), ("AirSyncBase", "BodyPreference"));
    m.insert((PAGE_AIRSYNCBASE, 0x06), ("AirSyncBase", "Type"));
    m.insert((PAGE_AIRSYNCBASE, 0x07), ("AirSyncBase", "TruncationSize"));
    m.insert((PAGE_AIRSYNCBASE, 0x08), ("AirSyncBase", "AllOrNone"));
    m.insert((PAGE_AIRSYNCBASE, 0x0A), ("AirSyncBase", "Body"));
    m.insert((PAGE_AIRSYNCBASE, 0x0B), ("AirSyncBase", "Data"));
    m.insert(
        (PAGE_AIRSYNCBASE, 0x0C),
        ("AirSyncBase", "EstimatedDataSize"),
    );
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
    m.insert(
        (PAGE_AIRSYNCBASE, 0x19),
        ("AirSyncBase", "BodyPartPreference"),
    );
    m.insert((PAGE_AIRSYNCBASE, 0x1A), ("AirSyncBase", "BodyPart"));
    m.insert((PAGE_AIRSYNCBASE, 0x1B), ("AirSyncBase", "Status"));
    m.insert((PAGE_AIRSYNCBASE, 0x1C), ("AirSyncBase", "Add"));
    m.insert((PAGE_AIRSYNCBASE, 0x1D), ("AirSyncBase", "Delete"));
    m.insert((PAGE_AIRSYNCBASE, 0x1E), ("AirSyncBase", "ClientId"));
    m.insert((PAGE_AIRSYNCBASE, 0x1F), ("AirSyncBase", "Content"));
    m.insert((PAGE_AIRSYNCBASE, 0x20), ("AirSyncBase", "Location"));
    m.insert((PAGE_AIRSYNCBASE, 0x21), ("AirSyncBase", "Annotation"));
    m.insert((PAGE_AIRSYNCBASE, 0x22), ("AirSyncBase", "Street"));
    m.insert((PAGE_AIRSYNCBASE, 0x23), ("AirSyncBase", "City"));
    m.insert((PAGE_AIRSYNCBASE, 0x24), ("AirSyncBase", "State"));
    m.insert((PAGE_AIRSYNCBASE, 0x25), ("AirSyncBase", "Country"));
    m.insert((PAGE_AIRSYNCBASE, 0x26), ("AirSyncBase", "PostalCode"));
    m.insert((PAGE_AIRSYNCBASE, 0x27), ("AirSyncBase", "Latitude"));
    m.insert((PAGE_AIRSYNCBASE, 0x28), ("AirSyncBase", "Longitude"));
    m.insert((PAGE_AIRSYNCBASE, 0x29), ("AirSyncBase", "Accuracy"));
    m.insert((PAGE_AIRSYNCBASE, 0x2A), ("AirSyncBase", "Altitude"));
    m.insert(
        (PAGE_AIRSYNCBASE, 0x2B),
        ("AirSyncBase", "AltitudeAccuracy"),
    );
    m.insert((PAGE_AIRSYNCBASE, 0x2C), ("AirSyncBase", "LocationUri"));
    m.insert((PAGE_AIRSYNCBASE, 0x2D), ("AirSyncBase", "InstanceId"));

    // ── Page 18 (0x12): Settings (MS-ASWBXML §2.1.2.1.19) ───────────────
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
    m.insert(
        (PAGE_SETTINGS, 0x0F),
        ("Settings", "AppliesToExternalKnown"),
    );
    m.insert(
        (PAGE_SETTINGS, 0x10),
        ("Settings", "AppliesToExternalUnknown"),
    );
    m.insert((PAGE_SETTINGS, 0x11), ("Settings", "Enabled"));
    m.insert((PAGE_SETTINGS, 0x12), ("Settings", "ReplyMessage"));
    m.insert((PAGE_SETTINGS, 0x13), ("Settings", "BodyType"));
    m.insert((PAGE_SETTINGS, 0x14), ("Settings", "DevicePassword"));
    m.insert((PAGE_SETTINGS, 0x15), ("Settings", "Password"));
    m.insert((PAGE_SETTINGS, 0x16), ("Settings", "DeviceInformation"));
    m.insert((PAGE_SETTINGS, 0x17), ("Settings", "Model"));
    m.insert((PAGE_SETTINGS, 0x18), ("Settings", "IMEI"));
    m.insert((PAGE_SETTINGS, 0x19), ("Settings", "FriendlyName"));
    m.insert((PAGE_SETTINGS, 0x1A), ("Settings", "OS"));
    m.insert((PAGE_SETTINGS, 0x1B), ("Settings", "OSLanguage"));
    m.insert((PAGE_SETTINGS, 0x1C), ("Settings", "PhoneNumber"));
    m.insert((PAGE_SETTINGS, 0x1D), ("Settings", "UserInformation"));
    m.insert((PAGE_SETTINGS, 0x1E), ("Settings", "EmailAddresses"));
    m.insert((PAGE_SETTINGS, 0x1F), ("Settings", "SMTPAddress"));
    m.insert((PAGE_SETTINGS, 0x20), ("Settings", "UserAgent"));
    m.insert((PAGE_SETTINGS, 0x21), ("Settings", "EnableOutboundSMS"));
    m.insert((PAGE_SETTINGS, 0x22), ("Settings", "MobileOperator"));
    m.insert((PAGE_SETTINGS, 0x23), ("Settings", "PrimarySmtpAddress"));
    m.insert((PAGE_SETTINGS, 0x24), ("Settings", "Accounts"));
    m.insert((PAGE_SETTINGS, 0x25), ("Settings", "Account"));
    m.insert((PAGE_SETTINGS, 0x26), ("Settings", "AccountId"));
    m.insert((PAGE_SETTINGS, 0x27), ("Settings", "AccountName"));
    m.insert((PAGE_SETTINGS, 0x28), ("Settings", "UserDisplayName"));
    m.insert((PAGE_SETTINGS, 0x29), ("Settings", "SendDisabled"));
    m.insert(
        (PAGE_SETTINGS, 0x2B),
        ("Settings", "RightsManagementInformation"),
    );

    // ── Page 21 (0x15): ComposeMail (MS-ASWBXML §2.1.2.1.22) ────────────
    m.insert((PAGE_COMPOSEMAIL, 0x05), ("ComposeMail", "SendMail"));
    m.insert((PAGE_COMPOSEMAIL, 0x06), ("ComposeMail", "SmartForward"));
    m.insert((PAGE_COMPOSEMAIL, 0x07), ("ComposeMail", "SmartReply"));
    m.insert((PAGE_COMPOSEMAIL, 0x08), ("ComposeMail", "SaveInSentItems"));
    m.insert((PAGE_COMPOSEMAIL, 0x09), ("ComposeMail", "ReplaceMime"));
    m.insert((PAGE_COMPOSEMAIL, 0x0B), ("ComposeMail", "Source"));
    m.insert((PAGE_COMPOSEMAIL, 0x0C), ("ComposeMail", "FolderId"));
    m.insert((PAGE_COMPOSEMAIL, 0x0D), ("ComposeMail", "ItemId"));
    m.insert((PAGE_COMPOSEMAIL, 0x0E), ("ComposeMail", "LongId"));
    m.insert((PAGE_COMPOSEMAIL, 0x0F), ("ComposeMail", "InstanceId"));
    m.insert((PAGE_COMPOSEMAIL, 0x10), ("ComposeMail", "Mime"));
    m.insert((PAGE_COMPOSEMAIL, 0x11), ("ComposeMail", "ClientId"));
    m.insert((PAGE_COMPOSEMAIL, 0x12), ("ComposeMail", "Status"));
    m.insert((PAGE_COMPOSEMAIL, 0x13), ("ComposeMail", "AccountId"));
    m.insert((PAGE_COMPOSEMAIL, 0x15), ("ComposeMail", "Forwardees"));
    m.insert((PAGE_COMPOSEMAIL, 0x16), ("ComposeMail", "Forwardee"));
    m.insert((PAGE_COMPOSEMAIL, 0x17), ("ComposeMail", "Name"));
    m.insert((PAGE_COMPOSEMAIL, 0x18), ("ComposeMail", "Email"));

    m
});

// ── Reverse map: (ns_prefix, xml_element_name) → (page, token_byte) ──────────
//
// For AirSync-page elements, active_sync.rs emits them WITHOUT a namespace
// prefix (using a default-namespace declaration instead).  We therefore
// register every AirSync element under BOTH ("AirSync", name) AND ("", name).
//
// For Calendar elements, active_sync.rs uses the shortened names "Start" and
// "End" (pre-EAS-14 style). We register alias entries so the encoder can
// still map them to the correct spec tokens StartTime (0x27) / EndTime (0x12).
static REV_TAG_MAP: LazyLock<HashMap<(&'static str, &'static str), (u8, u8)>> =
    LazyLock::new(|| {
        let mut m: HashMap<(&'static str, &'static str), (u8, u8)> = HashMap::new();

        // Populate from forward map.
        for ((page, token), (ns, name)) in TAG_MAP.iter() {
            m.insert((*ns, *name), (*page, *token));
            // Allow no-prefix lookup for AirSync page elements.
            if *page == PAGE_AIRSYNC {
                m.entry(("", *name)).or_insert((*page, *token));
            }
        }

        // Compatibility aliases: active_sync.rs uses "Start"/"End" instead of
        // the spec-correct "StartTime"/"EndTime" for Calendar elements.  Map
        // these aliases so the encoder emits the correct token bytes rather
        // than emitting the unknown-tag sentinel (0xFF).
        m.entry(("Calendar", "Start"))
            .or_insert((PAGE_CALENDAR, 0x27)); // → StartTime
        m.entry(("Calendar", "End"))
            .or_insert((PAGE_CALENDAR, 0x12)); // → EndTime

        m
    });

// ─────────────────────────────────────────────────────────────────────────────
// Public API
// ─────────────────────────────────────────────────────────────────────────────

/// Decodes an EAS WBXML byte stream into a UTF-8 XML string.
///
/// Parses the variable-length WBXML 1.x header so it is compatible with every
/// header length a real EAS device or the `encode` function can produce.
pub fn decode(input: &[u8]) -> Result<String, anyhow::Error> {
    if input.len() < 4 {
        return Err(anyhow::anyhow!("Input too short to be valid WBXML"));
    }

    // ── Parse variable-length WBXML header ───────────────────────────────────
    let mut pos = 0usize;

    // Version byte (1 byte).
    pos += 1;

    // Public-identifier (multibyte int).
    // A value of 0 means the actual id follows as a string-table reference.
    let (pubid, n) = read_multibyte_int(&input[pos..])?;
    pos += n;
    if pubid == 0 {
        let (_, m) = read_multibyte_int(&input[pos..])?;
        pos += m;
    }

    // Charset (multibyte int).
    let (_, n) = read_multibyte_int(&input[pos..])?;
    pos += n;

    // String-table: length (multibyte int) then the raw bytes.
    let (strtbl_len, n) = read_multibyte_int(&input[pos..])?;
    pos += n;
    if pos + strtbl_len > input.len() {
        return Err(anyhow::anyhow!(
            "WBXML string-table length {} extends beyond input",
            strtbl_len
        ));
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

    // Tolerate malformed input by auto-closing any unclosed tags.
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
/// Header written: `03 01 6A 00` — WBXML 1.3, unknown public-id, UTF-8,
/// empty string table.
///
/// `Event::Start` and `Event::Empty` are handled in **separate** match arms
/// so that each branch inspects only the event it owns; the previous combined
/// arm incorrectly consumed an extra token from the reader on every open tag.
///
/// Text content is passed through `quick_xml::escape::unescape()` (the
/// module-level function, valid for quick-xml ≥ 0.31) so that XML entities
/// such as `&amp;` are converted back to their raw characters before being
/// embedded in the WBXML inline-string payload.
pub fn encode(xml: &str) -> Result<Vec<u8>, anyhow::Error> {
    // WBXML 1.3 header: version=0x03  pubid=0x01  charset=0x6A(UTF-8)  strtbl_len=0x00
    let mut output: Vec<u8> = vec![0x03, 0x01, 0x6A, 0x00];
    let mut reader = Reader::from_str(xml);
    let mut current_page: u8 = 0xFF; // sentinel — no page emitted yet
    let mut buf = Vec::new();

    loop {
        buf.clear();
        match reader.read_event_into(&mut buf) {
            // ── Opening tag with child content ────────────────────────────────
            Ok(Event::Start(ref e)) => {
                let tag_byte = resolve_tag(e, &mut output, &mut current_page);
                output.push(tag_byte | 0x40); // content bit set
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
            // Fix E0599: use the module-level `quick_xml::escape::unescape()`
            // function instead of the non-existent `BytesText::unescape()`.
            Ok(Event::Text(ref t)) => {
                let raw = std::str::from_utf8(t.as_ref()).unwrap_or("");
                // unescape converts XML entities (&amp; → &, &lt; → <, …)
                let unescaped =
                    quick_xml::escape::unescape(raw).unwrap_or(std::borrow::Cow::Borrowed(raw));
                let bytes = unescaped.as_bytes();
                if !bytes.is_empty() {
                    output.push(STR_I);
                    output.extend_from_slice(bytes);
                    output.push(0x00); // inline-string null terminator
                }
            }

            // ── Skip XML declaration, processing instructions, comments ────────
            Ok(Event::Decl(_)) | Ok(Event::PI(_)) | Ok(Event::Comment(_)) => {}

            Ok(Event::Eof) => break,

            Err(e) => {
                return Err(anyhow::anyhow!(
                    "XML parse error during WBXML encode: {}",
                    e
                ));
            }

            _ => {}
        }
    }

    Ok(output)
}

// ─────────────────────────────────────────────────────────────────────────────
// Private helpers
// ─────────────────────────────────────────────────────────────────────────────

/// Resolves the EAS code page and token byte for the given element, emits a
/// `SWITCH_PAGE` instruction when the page changes, and returns the raw token
/// byte (content flag NOT set — callers apply that themselves).
///
/// Fix E0716: the `QName` returned by `e.name()` is a temporary; we bind it
/// to a local variable so it lives long enough for `as_ref()` to borrow it.
fn resolve_tag(
    e: &quick_xml::events::BytesStart<'_>,
    output: &mut Vec<u8>,
    current_page: &mut u8,
) -> u8 {
    // Bind the QName to extend its lifetime past the `from_utf8` borrow.
    let qname = e.name();
    let full_name = std::str::from_utf8(qname.as_ref()).unwrap_or("");

    let (ns, local) = match full_name.split_once(':') {
        Some((n, l)) => (n, l),
        None => ("", full_name),
    };

    let page: u8 = match ns {
        "AirSync" => PAGE_AIRSYNC,
        "Calendar" => PAGE_CALENDAR,
        "FolderHierarchy" => PAGE_FOLDERHIERARCHY,
        "MeetingResponse" => PAGE_MEETINGRESPONSE,
        "Ping" => PAGE_PING,
        "Provision" => PAGE_PROVISION,
        "AirSyncBase" => PAGE_AIRSYNCBASE,
        "Settings" => PAGE_SETTINGS,
        "ComposeMail" => PAGE_COMPOSEMAIL,
        // No prefix → default namespace; assume AirSync (all un-prefixed
        // elements in active_sync.rs response XML belong to this page).
        _ => PAGE_AIRSYNC,
    };

    if page != *current_page {
        output.push(SWITCH_PAGE);
        output.push(page);
        *current_page = page;
    }

    // ("", local) covers un-prefixed AirSync elements; ("ns", local) covers
    // all explicitly-prefixed elements and the Calendar aliases.
    match REV_TAG_MAP.get(&(ns, local)) {
        Some((_, t)) => *t,
        None => {
            tracing::warn!(
                "No WBXML token for element '{}:{}' (page {:#04x}); emitting 0xFF",
                ns,
                local,
                page
            );
            0xFF
        }
    }
}

/// Reads a WBXML multi-byte integer (big-endian, 7 bits per byte,
/// MSB = continuation flag).  Returns `(value, bytes_consumed)`.
fn read_multibyte_int(buf: &[u8]) -> Result<(usize, usize), anyhow::Error> {
    let mut result: usize = 0;
    let mut count = 0usize;
    loop {
        if count >= buf.len() {
            return Err(anyhow::anyhow!(
                "Unexpected end of input while reading WBXML multibyte integer"
            ));
        }
        let byte = buf[count];
        count += 1;
        result = (result << 7) | ((byte & 0x7F) as usize);
        if byte & 0x80 == 0 {
            break;
        }
        // WBXML multibyte ints are at most 5 bytes (35 usable bits for a
        // 32-bit value); reject pathological input early.
        if count > 5 {
            return Err(anyhow::anyhow!("WBXML multibyte integer overflow"));
        }
    }
    Ok((result, count))
}
