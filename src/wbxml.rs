use lazy_static::lazy_static;
use std::collections::HashMap;

const TAG_SWITCH_PAGE: u8 = 0x00;
const TAG_END: u8 = 0x01;
const TAG_STR_I: u8 = 0x03;
const TAG_OPAQUE: u8 = 0xC3;

const MAX_DECODE_DEPTH: usize = 256;

/// Validate that `name` is a legal XML element name (simplified XML 1.0 Name production).
/// Rejects empty strings and strings containing characters that could break XML structure.
fn is_valid_xml_name(name: &str) -> bool {
    let mut chars = name.chars();
    match chars.next() {
        None => return false,
        Some(c) if !(c.is_ascii_alphabetic() || c == '_' || c == ':' || !c.is_ascii()) => {
            return false;
        }
        _ => {}
    }
    chars.all(|c| {
        c.is_ascii_alphanumeric()
            || c == '_'
            || c == ':'
            || c == '-'
            || c == '.'
            || !c.is_ascii()
    })
}

const CP_AIRSYNC: u8 = 0;
const CP_CONTACTS: u8 = 1;
const CP_EMAIL: u8 = 2;
const CP_CALENDAR: u8 = 4;
const CP_MOVE: u8 = 5;
const CP_GETITEMESTIMATE: u8 = 6;
const CP_FOLDERHIERARCHY: u8 = 7;
const CP_MEETINGRESPONSE: u8 = 8;
const CP_TASKS: u8 = 9;
const CP_RESOLVERECIPIENTS: u8 = 10;
const CP_VALIDATECERT: u8 = 11;
const CP_CONTACTS2: u8 = 12;
const CP_PING: u8 = 13;
const CP_PROVISION: u8 = 14;
const CP_SEARCH: u8 = 15;
const CP_GAL: u8 = 16;
const CP_AIRSYNCBASE: u8 = 17;
const CP_SETTINGS: u8 = 18;
const CP_DOCUMENTLIBRARY: u8 = 19;
const CP_ITEMOPERATIONS: u8 = 20;
const CP_COMPOSEMAIL: u8 = 21;
const CP_EMAIL2: u8 = 22;
const CP_NOTES: u8 = 23;
const CP_RIGHTSMANAGEMENT: u8 = 24;

#[derive(Debug, Clone)]
struct Tag {
    name: &'static str,
    _has_content: bool,
}

lazy_static! {
    /// Maps ActiveSync-style namespace prefixes to WBXML code pages.
    static ref PREFIX_TO_PAGE: HashMap<&'static str, u8> = {
        let mut m = HashMap::new();
        m.insert("AirSync", CP_AIRSYNC);
        m.insert("Contacts", CP_CONTACTS);
        m.insert("Email", CP_EMAIL);
        m.insert("Calendar", CP_CALENDAR);
        m.insert("Move", CP_MOVE);
        m.insert("GetItemEstimate", CP_GETITEMESTIMATE);
        m.insert("FolderHierarchy", CP_FOLDERHIERARCHY);
        m.insert("MeetingResponse", CP_MEETINGRESPONSE);
        m.insert("Tasks", CP_TASKS);
        m.insert("ResolveRecipients", CP_RESOLVERECIPIENTS);
        m.insert("ValidateCert", CP_VALIDATECERT);
        m.insert("Contacts2", CP_CONTACTS2);
        m.insert("Ping", CP_PING);
        m.insert("Provision", CP_PROVISION);
        m.insert("Search", CP_SEARCH);
        m.insert("GAL", CP_GAL);
        m.insert("AirSyncBase", CP_AIRSYNCBASE);
        m.insert("Settings", CP_SETTINGS);
        m.insert("DocumentLibrary", CP_DOCUMENTLIBRARY);
        m.insert("ItemOperations", CP_ITEMOPERATIONS);
        m.insert("ComposeMail", CP_COMPOSEMAIL);
        m.insert("Email2", CP_EMAIL2);
        m.insert("Notes", CP_NOTES);
        m.insert("RightsManagement", CP_RIGHTSMANAGEMENT);
        m
    };

    static ref TAG_MAP: HashMap<(u8, u8), Tag> = {
        let mut m = HashMap::new();
        macro_rules! add {
            ($page:expr, $token:expr, $name:expr, $content:expr) => {
                m.insert(($page, $token), Tag { name: $name, _has_content: $content });
            };
        }
        // AirSync (0) – per MS-ASWBXML spec
        add!(CP_AIRSYNC, 0x05, "Sync", true);
        add!(CP_AIRSYNC, 0x06, "Responses", true);
        add!(CP_AIRSYNC, 0x07, "Add", true);
        add!(CP_AIRSYNC, 0x08, "Change", true);
        add!(CP_AIRSYNC, 0x09, "Delete", true);
        add!(CP_AIRSYNC, 0x0A, "Fetch", true);
        add!(CP_AIRSYNC, 0x0B, "SyncKey", true);
        add!(CP_AIRSYNC, 0x0C, "ClientId", true);
        add!(CP_AIRSYNC, 0x0D, "ServerId", true);
        add!(CP_AIRSYNC, 0x0E, "Status", true);
        add!(CP_AIRSYNC, 0x0F, "Collection", true);
        add!(CP_AIRSYNC, 0x10, "Class", true);
        add!(CP_AIRSYNC, 0x12, "CollectionId", true);
        add!(CP_AIRSYNC, 0x13, "GetChanges", true);
        add!(CP_AIRSYNC, 0x14, "MoreAvailable", true);
        add!(CP_AIRSYNC, 0x15, "WindowSize", true);
        add!(CP_AIRSYNC, 0x16, "Commands", true);
        add!(CP_AIRSYNC, 0x17, "Options", true);
        add!(CP_AIRSYNC, 0x18, "FilterType", true);
        add!(CP_AIRSYNC, 0x19, "Truncation", true);
        add!(CP_AIRSYNC, 0x1B, "Conflict", true);
        add!(CP_AIRSYNC, 0x1C, "Collections", true);
        add!(CP_AIRSYNC, 0x1D, "ApplicationData", true);
        add!(CP_AIRSYNC, 0x1E, "DeletesAsMoves", true);
        add!(CP_AIRSYNC, 0x20, "Supported", true);
        add!(CP_AIRSYNC, 0x21, "SoftDelete", true);
        add!(CP_AIRSYNC, 0x22, "MIMESupport", true);
        add!(CP_AIRSYNC, 0x23, "MIMETruncation", true);
        add!(CP_AIRSYNC, 0x24, "Wait", true);
        add!(CP_AIRSYNC, 0x25, "Limit", true);
        add!(CP_AIRSYNC, 0x26, "Partial", true);
        add!(CP_AIRSYNC, 0x27, "ConversationMode", true);
        add!(CP_AIRSYNC, 0x28, "MaxItems", true);
        add!(CP_AIRSYNC, 0x29, "HeartbeatInterval", true);

        // Calendar (4) – per MS-ASWBXML spec
        add!(CP_CALENDAR, 0x05, "Timezone", true);
        add!(CP_CALENDAR, 0x06, "AllDayEvent", true);
        add!(CP_CALENDAR, 0x07, "Attendees", true);
        add!(CP_CALENDAR, 0x08, "Attendee", true);
        add!(CP_CALENDAR, 0x09, "Email", true);
        add!(CP_CALENDAR, 0x0A, "Name", true);
        add!(CP_CALENDAR, 0x0D, "BusyStatus", true);
        add!(CP_CALENDAR, 0x0E, "Categories", true);
        add!(CP_CALENDAR, 0x0F, "Category", true);
        add!(CP_CALENDAR, 0x11, "DtStamp", true);
        add!(CP_CALENDAR, 0x12, "EndTime", true);
        add!(CP_CALENDAR, 0x13, "Exception", true);
        add!(CP_CALENDAR, 0x14, "Exceptions", true);
        add!(CP_CALENDAR, 0x15, "Deleted", true);
        add!(CP_CALENDAR, 0x16, "ExceptionStartTime", true);
        add!(CP_CALENDAR, 0x17, "Location", true);
        add!(CP_CALENDAR, 0x18, "MeetingStatus", true);
        add!(CP_CALENDAR, 0x19, "OrganizerEmail", true);
        add!(CP_CALENDAR, 0x1A, "OrganizerName", true);
        add!(CP_CALENDAR, 0x1B, "Recurrence", true);
        add!(CP_CALENDAR, 0x1C, "Type", true);
        add!(CP_CALENDAR, 0x1D, "Until", true);
        add!(CP_CALENDAR, 0x1E, "Occurrences", true);
        add!(CP_CALENDAR, 0x1F, "Interval", true);
        add!(CP_CALENDAR, 0x20, "DayOfWeek", true);
        add!(CP_CALENDAR, 0x21, "DayOfMonth", true);
        add!(CP_CALENDAR, 0x22, "WeekOfMonth", true);
        add!(CP_CALENDAR, 0x23, "MonthOfYear", true);
        add!(CP_CALENDAR, 0x24, "Reminder", true);
        add!(CP_CALENDAR, 0x25, "Sensitivity", true);
        add!(CP_CALENDAR, 0x26, "Subject", true);
        add!(CP_CALENDAR, 0x27, "StartTime", true);
        add!(CP_CALENDAR, 0x28, "UID", true);
        add!(CP_CALENDAR, 0x29, "AttendeeStatus", true);
        add!(CP_CALENDAR, 0x2A, "AttendeeType", true);
        add!(CP_CALENDAR, 0x33, "DisallowNewTimeProposal", true);
        add!(CP_CALENDAR, 0x34, "ResponseRequested", true);

        // AirSyncBase (17) – per MS-ASWBXML spec
        add!(CP_AIRSYNCBASE, 0x05, "BodyPreference", true);
        add!(CP_AIRSYNCBASE, 0x06, "Type", true);
        add!(CP_AIRSYNCBASE, 0x07, "TruncationSize", true);
        add!(CP_AIRSYNCBASE, 0x08, "AllOrNone", true);
        add!(CP_AIRSYNCBASE, 0x0A, "Body", true);
        add!(CP_AIRSYNCBASE, 0x0B, "Data", true);
        add!(CP_AIRSYNCBASE, 0x0C, "EstimatedDataSize", true);
        add!(CP_AIRSYNCBASE, 0x0D, "Truncated", true);
        add!(CP_AIRSYNCBASE, 0x0E, "Attachments", true);
        add!(CP_AIRSYNCBASE, 0x0F, "Attachment", true);
        add!(CP_AIRSYNCBASE, 0x10, "DisplayName", true);
        add!(CP_AIRSYNCBASE, 0x11, "FileReference", true);
        add!(CP_AIRSYNCBASE, 0x12, "Method", true);
        add!(CP_AIRSYNCBASE, 0x13, "ContentId", true);
        add!(CP_AIRSYNCBASE, 0x14, "ContentLocation", true);
        add!(CP_AIRSYNCBASE, 0x15, "IsInline", true);
        add!(CP_AIRSYNCBASE, 0x16, "NativeBodyType", true);
        add!(CP_AIRSYNCBASE, 0x17, "ContentType", true);
        add!(CP_AIRSYNCBASE, 0x18, "Preview", true);
        add!(CP_AIRSYNCBASE, 0x19, "BodyPartPreference", true);
        add!(CP_AIRSYNCBASE, 0x1A, "BodyPart", true);
        add!(CP_AIRSYNCBASE, 0x1B, "Status", true);

        // Settings (18) – per MS-ASCMD / MS-ASWBXML spec
        add!(CP_SETTINGS, 0x05, "Settings", true);
        add!(CP_SETTINGS, 0x06, "Status", true);
        add!(CP_SETTINGS, 0x07, "Get", true);
        add!(CP_SETTINGS, 0x08, "Set", true);
        add!(CP_SETTINGS, 0x09, "Oof", true);
        add!(CP_SETTINGS, 0x0A, "OofState", true);
        add!(CP_SETTINGS, 0x0B, "StartTime", true);
        add!(CP_SETTINGS, 0x0C, "EndTime", true);
        add!(CP_SETTINGS, 0x0D, "OofMessage", true);
        add!(CP_SETTINGS, 0x0E, "AppliesToInternal", true);
        add!(CP_SETTINGS, 0x0F, "AppliesToExternalKnown", true);
        add!(CP_SETTINGS, 0x10, "AppliesToExternalUnknown", true);
        add!(CP_SETTINGS, 0x11, "Enabled", true);
        add!(CP_SETTINGS, 0x12, "ReplyMessage", true);
        add!(CP_SETTINGS, 0x13, "BodyType", true);
        add!(CP_SETTINGS, 0x14, "Password", true);
        add!(CP_SETTINGS, 0x15, "DevicePassword", true);
        add!(CP_SETTINGS, 0x16, "DeviceInformation", true);
        add!(CP_SETTINGS, 0x17, "Model", true);
        add!(CP_SETTINGS, 0x18, "IMEI", true);
        add!(CP_SETTINGS, 0x19, "FriendlyName", true);
        add!(CP_SETTINGS, 0x1A, "OS", true);
        add!(CP_SETTINGS, 0x1B, "OSLanguage", true);
        add!(CP_SETTINGS, 0x1C, "PhoneNumber", true);
        add!(CP_SETTINGS, 0x1D, "UserInformation", true);
        add!(CP_SETTINGS, 0x1E, "EmailAddresses", true);
        add!(CP_SETTINGS, 0x1F, "SmtpAddress", true);
        add!(CP_SETTINGS, 0x20, "UserAgent", true);
        add!(CP_SETTINGS, 0x21, "EnableOutboundSMS", true);
        add!(CP_SETTINGS, 0x22, "MobileOperator", true);

        // ItemOperations (20) – per MS-ASCMD / MS-ASWBXML spec
        add!(CP_ITEMOPERATIONS, 0x05, "ItemOperations", true);
        add!(CP_ITEMOPERATIONS, 0x06, "Fetch", true);
        add!(CP_ITEMOPERATIONS, 0x07, "Store", true);
        add!(CP_ITEMOPERATIONS, 0x08, "Options", true);
        add!(CP_ITEMOPERATIONS, 0x09, "Range", true);
        add!(CP_ITEMOPERATIONS, 0x0A, "Total", true);
        add!(CP_ITEMOPERATIONS, 0x0B, "Properties", true);
        add!(CP_ITEMOPERATIONS, 0x0C, "Data", true);
        add!(CP_ITEMOPERATIONS, 0x0D, "Status", true);
        add!(CP_ITEMOPERATIONS, 0x0E, "Response", true);
        add!(CP_ITEMOPERATIONS, 0x0F, "Move", true);
        add!(CP_ITEMOPERATIONS, 0x10, "DstFldId", true);
        add!(CP_ITEMOPERATIONS, 0x11, "ConversationId", true);
        add!(CP_ITEMOPERATIONS, 0x12, "MoveAlways", true);

        // Search (15) – per MS-ASCMD / MS-ASWBXML spec
        add!(CP_SEARCH, 0x05, "Search", true);
        add!(CP_SEARCH, 0x07, "Store", true);
        add!(CP_SEARCH, 0x08, "Name", true);
        add!(CP_SEARCH, 0x09, "Query", true);
        add!(CP_SEARCH, 0x0A, "Options", true);
        add!(CP_SEARCH, 0x0B, "Range", true);
        add!(CP_SEARCH, 0x0C, "Status", true);
        add!(CP_SEARCH, 0x0D, "Response", true);
        add!(CP_SEARCH, 0x0E, "Result", true);
        add!(CP_SEARCH, 0x0F, "Properties", true);
        add!(CP_SEARCH, 0x10, "Total", true);
        add!(CP_SEARCH, 0x11, "EqualTo", true);
        add!(CP_SEARCH, 0x12, "Value", true);
        add!(CP_SEARCH, 0x13, "And", true);
        add!(CP_SEARCH, 0x14, "Or", true);
        add!(CP_SEARCH, 0x15, "FreeText", true);
        add!(CP_SEARCH, 0x17, "DeepTraversal", true);
        add!(CP_SEARCH, 0x18, "LongId", true);
        add!(CP_SEARCH, 0x19, "RebuildResults", true);
        add!(CP_SEARCH, 0x1A, "LessThan", true);
        add!(CP_SEARCH, 0x1B, "GreaterThan", true);
        add!(CP_SEARCH, 0x1E, "UserName", true);
        add!(CP_SEARCH, 0x1F, "Password", true);
        add!(CP_SEARCH, 0x20, "ConversationId", true);
        add!(CP_SEARCH, 0x21, "Picture", true);
        add!(CP_SEARCH, 0x22, "MaxSize", true);
        add!(CP_SEARCH, 0x23, "MaxPictures", true);

        // Provision (14) – per MS-ASPROV / MS-ASWBXML spec
        add!(CP_PROVISION, 0x05, "Provision", true);
        add!(CP_PROVISION, 0x06, "Policies", true);
        add!(CP_PROVISION, 0x07, "Policy", true);
        add!(CP_PROVISION, 0x08, "PolicyType", true);
        add!(CP_PROVISION, 0x09, "PolicyKey", true);
        add!(CP_PROVISION, 0x0A, "Data", true);
        add!(CP_PROVISION, 0x0B, "Status", true);
        add!(CP_PROVISION, 0x0C, "RemoteWipe", true);
        add!(CP_PROVISION, 0x0D, "EASProvisionDoc", true);
        add!(CP_PROVISION, 0x0E, "DevicePasswordEnabled", true);
        add!(CP_PROVISION, 0x0F, "AlphanumericDevicePasswordRequired", true);
        add!(CP_PROVISION, 0x10, "RequireStorageCardEncryption", true);
        add!(CP_PROVISION, 0x11, "PasswordRecoveryEnabled", true);
        add!(CP_PROVISION, 0x13, "AttachmentsEnabled", true);
        add!(CP_PROVISION, 0x14, "MinDevicePasswordLength", true);
        add!(CP_PROVISION, 0x15, "MaxInactivityTimeDeviceLock", true);
        add!(CP_PROVISION, 0x16, "MaxDevicePasswordFailedAttempts", true);
        add!(CP_PROVISION, 0x17, "MaxAttachmentSize", true);
        add!(CP_PROVISION, 0x18, "AllowSimpleDevicePassword", true);
        add!(CP_PROVISION, 0x19, "DevicePasswordExpiration", true);
        add!(CP_PROVISION, 0x1A, "DevicePasswordHistory", true);
        add!(CP_PROVISION, 0x1B, "AllowStorageCard", true);
        add!(CP_PROVISION, 0x1C, "AllowCamera", true);
        add!(CP_PROVISION, 0x1D, "RequireDeviceEncryption", true);
        add!(CP_PROVISION, 0x1E, "AllowUnsignedApplications", true);
        add!(CP_PROVISION, 0x1F, "AllowUnsignedInstallationPackages", true);
        add!(CP_PROVISION, 0x20, "MinDevicePasswordComplexCharacters", true);
        add!(CP_PROVISION, 0x21, "AllowWiFi", true);
        add!(CP_PROVISION, 0x22, "AllowTextMessaging", true);
        add!(CP_PROVISION, 0x23, "AllowPOPIMAPEmail", true);
        add!(CP_PROVISION, 0x24, "AllowBluetooth", true);
        add!(CP_PROVISION, 0x25, "AllowIrDA", true);
        add!(CP_PROVISION, 0x26, "RequireManualSyncWhenRoaming", true);
        add!(CP_PROVISION, 0x27, "AllowDesktopSync", true);
        add!(CP_PROVISION, 0x28, "MaxCalendarAgeFilter", true);
        add!(CP_PROVISION, 0x29, "AllowHTMLEmail", true);
        add!(CP_PROVISION, 0x2A, "MaxEmailAgeFilter", true);
        add!(CP_PROVISION, 0x2B, "MaxEmailBodyTruncationSize", true);
        add!(CP_PROVISION, 0x2C, "MaxEmailHTMLBodyTruncationSize", true);
        add!(CP_PROVISION, 0x2D, "RequireSignedSMIMEMessages", true);
        add!(CP_PROVISION, 0x2E, "RequireEncryptedSMIMEMessages", true);
        add!(CP_PROVISION, 0x2F, "RequireSignedSMIMEAlgorithm", true);
        add!(CP_PROVISION, 0x30, "RequireEncryptionSMIMEAlgorithm", true);
        add!(CP_PROVISION, 0x31, "AllowSMIMEEncryptionAlgorithmNegotiation", true);
        add!(CP_PROVISION, 0x32, "AllowSMIMESoftCerts", true);
        add!(CP_PROVISION, 0x33, "AllowBrowser", true);
        add!(CP_PROVISION, 0x34, "AllowConsumerEmail", true);
        add!(CP_PROVISION, 0x35, "AllowRemoteDesktop", true);
        add!(CP_PROVISION, 0x36, "AllowInternetSharing", true);
        add!(CP_PROVISION, 0x37, "UnapprovedInROMApplicationList", true);
        add!(CP_PROVISION, 0x38, "ApplicationName", true);
        add!(CP_PROVISION, 0x39, "ApprovedApplicationList", true);
        add!(CP_PROVISION, 0x3A, "Hash", true);

        // Contacts (1) – per MS-ASCNTC / MS-ASWBXML spec
        add!(CP_CONTACTS, 0x05, "Anniversary", true);
        add!(CP_CONTACTS, 0x06, "AssistantName", true);
        add!(CP_CONTACTS, 0x07, "AssistantPhoneNumber", true);
        add!(CP_CONTACTS, 0x08, "Birthday", true);
        add!(CP_CONTACTS, 0x0C, "Business2PhoneNumber", true);
        add!(CP_CONTACTS, 0x0D, "BusinessCity", true);
        add!(CP_CONTACTS, 0x0E, "BusinessCountry", true);
        add!(CP_CONTACTS, 0x0F, "BusinessPostalCode", true);
        add!(CP_CONTACTS, 0x10, "BusinessState", true);
        add!(CP_CONTACTS, 0x11, "BusinessStreet", true);
        add!(CP_CONTACTS, 0x12, "BusinessFaxNumber", true);
        add!(CP_CONTACTS, 0x13, "BusinessPhoneNumber", true);
        add!(CP_CONTACTS, 0x17, "CompanyName", true);
        add!(CP_CONTACTS, 0x19, "Department", true);
        add!(CP_CONTACTS, 0x1A, "Email1Address", true);
        add!(CP_CONTACTS, 0x1B, "Email2Address", true);
        add!(CP_CONTACTS, 0x1C, "Email3Address", true);
        add!(CP_CONTACTS, 0x1D, "FileAs", true);
        add!(CP_CONTACTS, 0x1F, "FirstName", true);
        add!(CP_CONTACTS, 0x20, "Home2PhoneNumber", true);
        add!(CP_CONTACTS, 0x21, "HomeCity", true);
        add!(CP_CONTACTS, 0x22, "HomeCountry", true);
        add!(CP_CONTACTS, 0x23, "HomePostalCode", true);
        add!(CP_CONTACTS, 0x24, "HomeState", true);
        add!(CP_CONTACTS, 0x25, "HomeStreet", true);
        add!(CP_CONTACTS, 0x26, "HomeFaxNumber", true);
        add!(CP_CONTACTS, 0x27, "HomePhoneNumber", true);
        add!(CP_CONTACTS, 0x28, "JobTitle", true);
        add!(CP_CONTACTS, 0x29, "LastName", true);
        add!(CP_CONTACTS, 0x2A, "MiddleName", true);
        add!(CP_CONTACTS, 0x2B, "MobilePhoneNumber", true);
        add!(CP_CONTACTS, 0x2C, "OfficeLocation", true);
        add!(CP_CONTACTS, 0x2F, "PagerNumber", true);
        add!(CP_CONTACTS, 0x31, "Spouse", true);
        add!(CP_CONTACTS, 0x32, "Suffix", true);
        add!(CP_CONTACTS, 0x33, "Title", true);
        add!(CP_CONTACTS, 0x34, "WebPage", true);
        add!(CP_CONTACTS, 0x35, "YomiCompanyName", true);
        add!(CP_CONTACTS, 0x36, "YomiFirstName", true);
        add!(CP_CONTACTS, 0x37, "YomiLastName", true);
        add!(CP_CONTACTS, 0x3C, "Picture", true);
        add!(CP_CONTACTS, 0x3D, "Alias", true);
        add!(CP_CONTACTS, 0x3E, "WeightedRank", true);

        // Email (2) – per MS-ASEMAIL / MS-ASWBXML spec
        add!(CP_EMAIL, 0x05, "Attachment", true);
        add!(CP_EMAIL, 0x06, "Attachments", true);
        add!(CP_EMAIL, 0x07, "AttName", true);
        add!(CP_EMAIL, 0x08, "AttSize", true);
        add!(CP_EMAIL, 0x09, "Att0Id", true);
        add!(CP_EMAIL, 0x0A, "AttMethod", true);
        // 0x0B is not assigned in the spec
        add!(CP_EMAIL, 0x0C, "Body", true);
        add!(CP_EMAIL, 0x0D, "BodySize", true);
        add!(CP_EMAIL, 0x0E, "BodyTruncated", true);
        add!(CP_EMAIL, 0x0F, "DateReceived", true);
        add!(CP_EMAIL, 0x10, "DisplayName", true);
        add!(CP_EMAIL, 0x11, "DisplayTo", true);
        add!(CP_EMAIL, 0x12, "Importance", true);
        add!(CP_EMAIL, 0x13, "MessageClass", true);
        add!(CP_EMAIL, 0x14, "Subject", true);
        add!(CP_EMAIL, 0x15, "Read", true);
        add!(CP_EMAIL, 0x16, "To", true);
        add!(CP_EMAIL, 0x17, "Cc", true);
        add!(CP_EMAIL, 0x18, "From", true);
        add!(CP_EMAIL, 0x19, "ReplyTo", true);
        add!(CP_EMAIL, 0x1A, "AllDayEvent", true);
        add!(CP_EMAIL, 0x1B, "Categories", true);
        add!(CP_EMAIL, 0x1C, "Category", true);
        add!(CP_EMAIL, 0x1D, "DtStamp", true);
        add!(CP_EMAIL, 0x1E, "EndTime", true);
        add!(CP_EMAIL, 0x1F, "InstanceType", true);
        add!(CP_EMAIL, 0x20, "BusyStatus", true);
        add!(CP_EMAIL, 0x21, "Location", true);
        add!(CP_EMAIL, 0x22, "MeetingRequest", true);
        add!(CP_EMAIL, 0x23, "Organizer", true);
        add!(CP_EMAIL, 0x24, "RecurrenceId", true);
        add!(CP_EMAIL, 0x25, "Reminder", true);
        add!(CP_EMAIL, 0x26, "ResponseRequested", true);
        add!(CP_EMAIL, 0x27, "Recurrences", true);
        add!(CP_EMAIL, 0x28, "Recurrence", true);
        add!(CP_EMAIL, 0x29, "Type", true);
        add!(CP_EMAIL, 0x2A, "Until", true);
        add!(CP_EMAIL, 0x2B, "Occurrences", true);
        add!(CP_EMAIL, 0x2C, "Interval", true);
        add!(CP_EMAIL, 0x2D, "DayOfWeek", true);
        add!(CP_EMAIL, 0x2E, "DayOfMonth", true);
        add!(CP_EMAIL, 0x2F, "WeekOfMonth", true);
        add!(CP_EMAIL, 0x30, "MonthOfYear", true);
        add!(CP_EMAIL, 0x31, "StartTime", true);
        add!(CP_EMAIL, 0x32, "Sensitivity", true);
        add!(CP_EMAIL, 0x33, "TimeZone", true);
        add!(CP_EMAIL, 0x34, "GlobalObjId", true);
        add!(CP_EMAIL, 0x35, "ThreadTopic", true);
        add!(CP_EMAIL, 0x36, "MIMEData", true);
        add!(CP_EMAIL, 0x37, "MIMETruncated", true);
        add!(CP_EMAIL, 0x38, "MIMESize", true);
        add!(CP_EMAIL, 0x39, "InternetCPID", true);
        add!(CP_EMAIL, 0x3A, "Flag", true);
        add!(CP_EMAIL, 0x3B, "Status", true);
        add!(CP_EMAIL, 0x3C, "ContentClass", true);
        add!(CP_EMAIL, 0x3D, "FlagType", true);
        add!(CP_EMAIL, 0x3E, "CompleteTime", true);
        add!(CP_EMAIL, 0x3F, "DisallowNewTimeProposal", true);

        // Move (5) – per MS-ASCMD / MS-ASWBXML spec
        add!(CP_MOVE, 0x05, "MoveItems", true);
        add!(CP_MOVE, 0x06, "Move", true);
        add!(CP_MOVE, 0x07, "SrcMsgId", true);
        add!(CP_MOVE, 0x08, "SrcFldId", true);
        add!(CP_MOVE, 0x09, "DstFldId", true);
        add!(CP_MOVE, 0x0A, "Response", true);
        add!(CP_MOVE, 0x0B, "Status", true);
        add!(CP_MOVE, 0x0C, "DstMsgId", true);

        // GetItemEstimate (6) – per MS-ASCMD / MS-ASWBXML spec
        add!(CP_GETITEMESTIMATE, 0x05, "GetItemEstimate", true);
        add!(CP_GETITEMESTIMATE, 0x06, "Version", true);
        add!(CP_GETITEMESTIMATE, 0x07, "Collections", true);
        add!(CP_GETITEMESTIMATE, 0x08, "Collection", true);
        add!(CP_GETITEMESTIMATE, 0x09, "Class", true);
        add!(CP_GETITEMESTIMATE, 0x0A, "CollectionId", true);
        add!(CP_GETITEMESTIMATE, 0x0B, "DateTime", true);
        add!(CP_GETITEMESTIMATE, 0x0C, "Estimate", true);
        add!(CP_GETITEMESTIMATE, 0x0D, "Response", true);
        add!(CP_GETITEMESTIMATE, 0x0E, "Status", true);

        // FolderHierarchy (7) – per MS-ASCMD / MS-ASWBXML spec
        add!(CP_FOLDERHIERARCHY, 0x05, "Folders", true);
        add!(CP_FOLDERHIERARCHY, 0x06, "Folder", true);
        add!(CP_FOLDERHIERARCHY, 0x07, "DisplayName", true);
        add!(CP_FOLDERHIERARCHY, 0x08, "ServerId", true);
        add!(CP_FOLDERHIERARCHY, 0x09, "ParentId", true);
        add!(CP_FOLDERHIERARCHY, 0x0A, "Type", true);
        add!(CP_FOLDERHIERARCHY, 0x0C, "Status", true);
        add!(CP_FOLDERHIERARCHY, 0x0D, "ContentClass", true);
        add!(CP_FOLDERHIERARCHY, 0x0E, "Changes", true);
        add!(CP_FOLDERHIERARCHY, 0x0F, "Add", true);
        add!(CP_FOLDERHIERARCHY, 0x10, "Delete", true);
        add!(CP_FOLDERHIERARCHY, 0x11, "Update", true);
        add!(CP_FOLDERHIERARCHY, 0x12, "SyncKey", true);
        add!(CP_FOLDERHIERARCHY, 0x13, "FolderCreate", true);
        add!(CP_FOLDERHIERARCHY, 0x14, "FolderDelete", true);
        add!(CP_FOLDERHIERARCHY, 0x15, "FolderUpdate", true);
        add!(CP_FOLDERHIERARCHY, 0x16, "FolderSync", true);
        add!(CP_FOLDERHIERARCHY, 0x17, "Count", true);

        // MeetingResponse (8) – per MS-ASCMD / MS-ASWBXML spec
        add!(CP_MEETINGRESPONSE, 0x05, "CalendarId", true);
        add!(CP_MEETINGRESPONSE, 0x06, "CollectionId", true);
        add!(CP_MEETINGRESPONSE, 0x07, "MeetingResponse", true);
        add!(CP_MEETINGRESPONSE, 0x08, "RequestId", true);
        add!(CP_MEETINGRESPONSE, 0x09, "Request", true);
        add!(CP_MEETINGRESPONSE, 0x0A, "Result", true);
        add!(CP_MEETINGRESPONSE, 0x0B, "Status", true);
        add!(CP_MEETINGRESPONSE, 0x0C, "UserResponse", true);
        add!(CP_MEETINGRESPONSE, 0x0E, "InstanceId", true);

        // Tasks (9) – per MS-ASTASK / MS-ASWBXML spec
        add!(CP_TASKS, 0x08, "Categories", true);
        add!(CP_TASKS, 0x09, "Category", true);
        add!(CP_TASKS, 0x0A, "Complete", true);
        add!(CP_TASKS, 0x0B, "DateCompleted", true);
        add!(CP_TASKS, 0x0C, "DueDate", true);
        add!(CP_TASKS, 0x0D, "UtcDueDate", true);
        add!(CP_TASKS, 0x0E, "Importance", true);
        add!(CP_TASKS, 0x0F, "Recurrence", true);
        add!(CP_TASKS, 0x10, "Type", true);
        add!(CP_TASKS, 0x11, "Start", true);
        add!(CP_TASKS, 0x12, "Until", true);
        add!(CP_TASKS, 0x13, "Occurrences", true);
        add!(CP_TASKS, 0x14, "Interval", true);
        add!(CP_TASKS, 0x15, "DayOfMonth", true);
        add!(CP_TASKS, 0x16, "DayOfWeek", true);
        add!(CP_TASKS, 0x17, "WeekOfMonth", true);
        add!(CP_TASKS, 0x18, "MonthOfYear", true);
        add!(CP_TASKS, 0x19, "Regenerate", true);
        add!(CP_TASKS, 0x1A, "DeadOccur", true);
        add!(CP_TASKS, 0x1B, "ReminderSet", true);
        add!(CP_TASKS, 0x1C, "ReminderTime", true);
        add!(CP_TASKS, 0x1D, "Sensitivity", true);
        add!(CP_TASKS, 0x1E, "StartDate", true);
        add!(CP_TASKS, 0x1F, "UtcStartDate", true);
        add!(CP_TASKS, 0x20, "Subject", true);
        add!(CP_TASKS, 0x22, "OrdinalDate", true);
        add!(CP_TASKS, 0x23, "SubOrdinalDate", true);
        add!(CP_TASKS, 0x24, "CalendarType", true);
        add!(CP_TASKS, 0x25, "IsLeapMonth", true);
        add!(CP_TASKS, 0x26, "FirstDayOfWeek", true);

        // ResolveRecipients (10) – per MS-ASCMD / MS-ASWBXML spec
        add!(CP_RESOLVERECIPIENTS, 0x05, "ResolveRecipients", true);
        add!(CP_RESOLVERECIPIENTS, 0x06, "Response", true);
        add!(CP_RESOLVERECIPIENTS, 0x07, "Status", true);
        add!(CP_RESOLVERECIPIENTS, 0x08, "Type", true);
        add!(CP_RESOLVERECIPIENTS, 0x09, "Recipient", true);
        add!(CP_RESOLVERECIPIENTS, 0x0A, "DisplayName", true);
        add!(CP_RESOLVERECIPIENTS, 0x0B, "EmailAddress", true);
        add!(CP_RESOLVERECIPIENTS, 0x0C, "Certificates", true);
        add!(CP_RESOLVERECIPIENTS, 0x0D, "Certificate", true);
        add!(CP_RESOLVERECIPIENTS, 0x0E, "MiniCertificate", true);
        add!(CP_RESOLVERECIPIENTS, 0x0F, "Options", true);
        add!(CP_RESOLVERECIPIENTS, 0x10, "To", true);
        add!(CP_RESOLVERECIPIENTS, 0x11, "CertificateRetrieval", true);
        add!(CP_RESOLVERECIPIENTS, 0x12, "RecipientCount", true);
        add!(CP_RESOLVERECIPIENTS, 0x13, "MaxCertificates", true);
        add!(CP_RESOLVERECIPIENTS, 0x14, "MaxAmbiguousRecipients", true);
        add!(CP_RESOLVERECIPIENTS, 0x15, "CertificateCount", true);
        add!(CP_RESOLVERECIPIENTS, 0x16, "Availability", true);
        add!(CP_RESOLVERECIPIENTS, 0x17, "StartTime", true);
        add!(CP_RESOLVERECIPIENTS, 0x18, "EndTime", true);
        add!(CP_RESOLVERECIPIENTS, 0x19, "MergedFreeBusy", true);
        add!(CP_RESOLVERECIPIENTS, 0x1A, "Picture", true);
        add!(CP_RESOLVERECIPIENTS, 0x1B, "MaxSize", true);
        add!(CP_RESOLVERECIPIENTS, 0x1C, "Data", true);
        add!(CP_RESOLVERECIPIENTS, 0x1D, "MaxPictures", true);

        // ValidateCert (11) – per MS-ASCMD / MS-ASWBXML spec
        add!(CP_VALIDATECERT, 0x05, "ValidateCert", true);
        add!(CP_VALIDATECERT, 0x06, "Certificates", true);
        add!(CP_VALIDATECERT, 0x07, "Certificate", true);
        add!(CP_VALIDATECERT, 0x08, "CertificateChain", true);
        add!(CP_VALIDATECERT, 0x09, "CheckCRL", true);
        add!(CP_VALIDATECERT, 0x0A, "Status", true);

        // Contacts2 (12) – per MS-ASCNTC / MS-ASWBXML spec
        add!(CP_CONTACTS2, 0x05, "CustomerId", true);
        add!(CP_CONTACTS2, 0x06, "GovernmentId", true);
        add!(CP_CONTACTS2, 0x07, "IMAddress", true);
        add!(CP_CONTACTS2, 0x08, "IMAddress2", true);
        add!(CP_CONTACTS2, 0x09, "IMAddress3", true);
        add!(CP_CONTACTS2, 0x0A, "ManagerName", true);
        add!(CP_CONTACTS2, 0x0B, "CompanyMainPhone", true);
        add!(CP_CONTACTS2, 0x0C, "AccountName", true);
        add!(CP_CONTACTS2, 0x0D, "NickName", true);
        add!(CP_CONTACTS2, 0x0E, "MMS", true);

        // Ping (13) – per MS-ASWBXML spec
        add!(CP_PING, 0x05, "Ping", true);
        add!(CP_PING, 0x07, "Status", true);
        add!(CP_PING, 0x08, "HeartbeatInterval", true);
        add!(CP_PING, 0x09, "Folders", true);
        add!(CP_PING, 0x0A, "Folder", true);
        add!(CP_PING, 0x0B, "Id", true);
        add!(CP_PING, 0x0C, "Class", true);
        add!(CP_PING, 0x0D, "MaxFolders", true);

        // GAL (16) – per MS-ASWBXML spec
        add!(CP_GAL, 0x05, "DisplayName", true);
        add!(CP_GAL, 0x06, "Phone", true);
        add!(CP_GAL, 0x07, "Office", true);
        add!(CP_GAL, 0x08, "Title", true);
        add!(CP_GAL, 0x09, "Company", true);
        add!(CP_GAL, 0x0A, "Alias", true);
        add!(CP_GAL, 0x0B, "FirstName", true);
        add!(CP_GAL, 0x0C, "LastName", true);
        add!(CP_GAL, 0x0D, "HomePhone", true);
        add!(CP_GAL, 0x0E, "MobilePhone", true);
        add!(CP_GAL, 0x0F, "EmailAddress", true);

        // DocumentLibrary (19) – per MS-ASWBXML spec
        add!(CP_DOCUMENTLIBRARY, 0x05, "LinkId", true);
        add!(CP_DOCUMENTLIBRARY, 0x06, "DisplayName", true);
        add!(CP_DOCUMENTLIBRARY, 0x07, "IsFolder", true);
        add!(CP_DOCUMENTLIBRARY, 0x09, "CreationDate", true);
        add!(CP_DOCUMENTLIBRARY, 0x0A, "LastModifiedDate", true);
        add!(CP_DOCUMENTLIBRARY, 0x0B, "IsHidden", true);
        add!(CP_DOCUMENTLIBRARY, 0x0C, "ContentLength", true);
        add!(CP_DOCUMENTLIBRARY, 0x0D, "ContentType", true);

        // ComposeMail (21) – per MS-ASCMD / MS-ASWBXML spec
        add!(CP_COMPOSEMAIL, 0x05, "SendMail", true);
        add!(CP_COMPOSEMAIL, 0x06, "SmartForward", true);
        add!(CP_COMPOSEMAIL, 0x07, "SmartReply", true);
        add!(CP_COMPOSEMAIL, 0x08, "SaveInSentItems", true);
        add!(CP_COMPOSEMAIL, 0x09, "ReplaceMime", true);
        add!(CP_COMPOSEMAIL, 0x0B, "Source", true);
        add!(CP_COMPOSEMAIL, 0x0C, "FolderId", true);
        add!(CP_COMPOSEMAIL, 0x0D, "ItemId", true);
        add!(CP_COMPOSEMAIL, 0x0E, "LongId", true);
        add!(CP_COMPOSEMAIL, 0x0F, "InstanceId", true);
        add!(CP_COMPOSEMAIL, 0x10, "Mime", true);
        add!(CP_COMPOSEMAIL, 0x11, "ClientId", true);
        add!(CP_COMPOSEMAIL, 0x12, "Status", true);
        add!(CP_COMPOSEMAIL, 0x13, "AccountId", true);

        // Email2 (22) – per MS-ASEMAIL / MS-ASWBXML spec
        add!(CP_EMAIL2, 0x05, "UmCallerID", true);
        add!(CP_EMAIL2, 0x06, "UmUserNotes", true);
        add!(CP_EMAIL2, 0x07, "UmAttDuration", true);
        add!(CP_EMAIL2, 0x08, "UmAttOrder", true);
        add!(CP_EMAIL2, 0x09, "ConversationId", true);
        add!(CP_EMAIL2, 0x0A, "ConversationIndex", true);
        add!(CP_EMAIL2, 0x0B, "LastVerbExecuted", true);
        add!(CP_EMAIL2, 0x0C, "LastVerbExecutionTime", true);
        add!(CP_EMAIL2, 0x0D, "ReceivedAsBcc", true);
        add!(CP_EMAIL2, 0x0E, "Sender", true);
        add!(CP_EMAIL2, 0x0F, "CalendarType", true);
        add!(CP_EMAIL2, 0x10, "IsLeapMonth", true);
        add!(CP_EMAIL2, 0x11, "AccountId", true);
        add!(CP_EMAIL2, 0x12, "FirstDayOfWeek", true);
        add!(CP_EMAIL2, 0x13, "MeetingMessageType", true);

        // Notes (23) – per MS-ASWBXML spec
        add!(CP_NOTES, 0x05, "Subject", true);
        add!(CP_NOTES, 0x06, "MessageClass", true);
        add!(CP_NOTES, 0x07, "LastModifiedDate", true);
        add!(CP_NOTES, 0x08, "Categories", true);
        add!(CP_NOTES, 0x09, "Category", true);

        // RightsManagement (24) – per MS-ASWBXML spec
        add!(CP_RIGHTSMANAGEMENT, 0x05, "RightsManagementSupport", true);
        add!(CP_RIGHTSMANAGEMENT, 0x06, "RightsManagementTemplates", true);
        add!(CP_RIGHTSMANAGEMENT, 0x07, "RightsManagementTemplate", true);
        add!(CP_RIGHTSMANAGEMENT, 0x08, "RightsManagementLicense", true);
        add!(CP_RIGHTSMANAGEMENT, 0x09, "EditAllowed", true);
        add!(CP_RIGHTSMANAGEMENT, 0x0A, "ReplyAllowed", true);
        add!(CP_RIGHTSMANAGEMENT, 0x0B, "ReplyAllAllowed", true);
        add!(CP_RIGHTSMANAGEMENT, 0x0C, "ForwardAllowed", true);
        add!(CP_RIGHTSMANAGEMENT, 0x0D, "ModifyRecipientsAllowed", true);
        add!(CP_RIGHTSMANAGEMENT, 0x0E, "ExtractAllowed", true);
        add!(CP_RIGHTSMANAGEMENT, 0x0F, "PrintAllowed", true);
        add!(CP_RIGHTSMANAGEMENT, 0x10, "ExportAllowed", true);
        add!(CP_RIGHTSMANAGEMENT, 0x11, "ProgrammaticAccessAllowed", true);
        add!(CP_RIGHTSMANAGEMENT, 0x12, "Owner", true);
        add!(CP_RIGHTSMANAGEMENT, 0x13, "ContentExpiryDate", true);
        add!(CP_RIGHTSMANAGEMENT, 0x14, "TemplateID", true);
        add!(CP_RIGHTSMANAGEMENT, 0x15, "TemplateName", true);
        add!(CP_RIGHTSMANAGEMENT, 0x16, "TemplateDescription", true);
        add!(CP_RIGHTSMANAGEMENT, 0x17, "ContentOwner", true);
        add!(CP_RIGHTSMANAGEMENT, 0x18, "RemoveRightsManagementProtection", true);

        m
    };

    static ref NAME_MAP: HashMap<&'static str, Vec<(u8, u8)>> = {
        let mut m: HashMap<&'static str, Vec<(u8, u8)>> = HashMap::new();
        for ((page, token), tag) in TAG_MAP.iter() {
            m.entry(tag.name).or_default().push((*page, *token));
        }
        for entries in m.values_mut() {
            entries.sort();
        }
        m
    };
}

pub fn decode(data: &[u8]) -> Result<String, String> {
    if data.len() < 4 {
        return Err("Data too short".into());
    }
    if data[0] != 0x03 {
        return Err("Invalid WBXML header".into());
    }

    // Parse public ID as mb_u_int32 (per WBXML spec, section 5.4)
    let mut pos = 1;
    let mut publicid: u32 = 0;
    loop {
        if pos >= data.len() {
            return Err("Unexpected end reading public ID".into());
        }
        let byte = data[pos];
        pos += 1;
        publicid = (publicid << 7) | (byte & 0x7F) as u32;
        if (byte & 0x80) == 0 {
            break;
        }
    }

    // When publicid is 0 the actual identifier is stored as a string table index
    // encoded as an additional mb_u_int32 that must be consumed before charset.
    if publicid == 0 {
        loop {
            if pos >= data.len() {
                return Err("Unexpected end reading public ID string table index".into());
            }
            let byte = data[pos];
            pos += 1;
            if (byte & 0x80) == 0 {
                break;
            }
        }
    }

    // Read charset as mb_u_int32 (per WBXML spec); for ActiveSync this is always
    // 0x6A (UTF-8) which fits in a single byte.
    let mut charset: u32 = 0;
    loop {
        if pos >= data.len() {
            return Err("Unexpected end reading charset".into());
        }
        let byte = data[pos];
        pos += 1;
        charset = (charset << 7) | (byte & 0x7F) as u32;
        if (byte & 0x80) == 0 {
            break;
        }
    }
    if charset != 0x6A {
        return Err("Invalid WBXML header".into());
    }

    // Read string table length (mb_u_int32)
    let mut strtbl_len: usize = 0;
    loop {
        if pos >= data.len() {
            return Err("Unexpected end reading string table length".into());
        }
        let byte = data[pos];
        pos += 1;
        strtbl_len = strtbl_len
            .checked_shl(7)
            .and_then(|v| v.checked_add((byte & 0x7F) as usize))
            .ok_or_else(|| "String table length overflow".to_string())?;
        if (byte & 0x80) == 0 {
            break;
        }
    }

    // Read string table
    let strtbl_start = pos;
    let strtbl_end = strtbl_start.checked_add(strtbl_len)
        .ok_or_else(|| "String table end overflow".to_string())?;
    if strtbl_end > data.len() {
        return Err("String table exceeds data length".into());
    }
    let strtbl = &data[strtbl_start..strtbl_end];
    pos = strtbl_end;

    let mut current_page = 0;
    let mut xml = String::new();
    let mut stack: Vec<String> = Vec::new();
    let mut pending_tag: Option<String> = None;

    fn read_mb_u_int32(data: &[u8], pos: &mut usize) -> Result<usize, String> {
        let mut val: usize = 0;
        loop {
            if *pos >= data.len() {
                return Err("Unexpected end reading mb_u_int32".into());
            }
            let byte = data[*pos];
            *pos += 1;
            val = val
                .checked_shl(7)
                .and_then(|v| v.checked_add((byte & 0x7F) as usize))
                .ok_or_else(|| "mb_u_int32 overflow".to_string())?;
            if (byte & 0x80) == 0 {
                break;
            }
        }
        Ok(val)
    }

    fn read_strtbl_string(strtbl: &[u8], offset: usize) -> Result<String, String> {
        if offset >= strtbl.len() {
            return Err("String table offset out of bounds".into());
        }
        let mut end = offset;
        while end < strtbl.len() && strtbl[end] != 0 {
            end += 1;
        }
        let s = String::from_utf8_lossy(&strtbl[offset..end]).to_string();
        Ok(s)
    }

    while pos < data.len() {
        let token = data[pos];
        pos += 1;

        if token == TAG_SWITCH_PAGE {
            if pos >= data.len() {
                return Err("Unexpected end".into());
            }
            current_page = data[pos];
            pos += 1;
            continue;
        }

        if token == TAG_END {
            if pending_tag.is_some() {
                xml.push_str("/>");
                pending_tag = None;
            } else if let Some(tag) = stack.pop() {
                xml.push_str(&format!("</{}>", tag));
            }
            continue;
        }

        if token == TAG_STR_I {
            let mut str_buf = Vec::new();
            while pos < data.len() && data[pos] != 0 {
                str_buf.push(data[pos]);
                pos += 1;
            }
            if pos < data.len() {
                pos += 1;
            } else {
                return Err("Unexpected end in inline string".into());
            }

            if let Some(tag) = pending_tag.take() {
                xml.push('>');
                stack.push(tag);
            }
            let text = String::from_utf8_lossy(&str_buf);
            xml.push_str(
                &text
                    .replace('&', "&amp;")
                    .replace('<', "&lt;")
                    .replace('>', "&gt;"),
            );
            continue;
        }

        if token == TAG_OPAQUE {
            let mut len: usize = 0;
            loop {
                if pos >= data.len() {
                    return Err("Unexpected end opaque".into());
                }
                let byte = data[pos];
                pos += 1;
                len = len
                    .checked_shl(7)
                    .and_then(|v| v.checked_add((byte & 0x7F) as usize))
                    .ok_or_else(|| "Opaque length overflow".to_string())?;
                if (byte & 0x80) == 0 {
                    break;
                }
            }
            let end = pos.checked_add(len).ok_or_else(|| "Opaque overflow".to_string())?;
            if end > data.len() {
                return Err("Opaque overflow".into());
            }
            let content = &data[pos..end];
            pos = end;

            if let Some(tag) = pending_tag.take() {
                xml.push('>');
                stack.push(tag);
            }
            let encoded = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, content);
            xml.push_str(&encoded);
            continue;
        }

        // Handle STR_T (0x83) — string table reference
        if token == 0x83 {
            let offset = read_mb_u_int32(data, &mut pos)?;
            let text = read_strtbl_string(strtbl, offset)?;

            if let Some(tag) = pending_tag.take() {
                xml.push('>');
                stack.push(tag);
            }
            xml.push_str(
                &text
                    .replace('&', "&amp;")
                    .replace('<', "&lt;")
                    .replace('>', "&gt;"),
            );
            continue;
        }

        // Handle LITERAL (0x04), LITERAL_C (0x44), LITERAL_A (0x84), LITERAL_AC (0xC4) tokens
        if token == 0x04 || token == 0x44 || token == 0x84 || token == 0xC4 {
            let has_content = (token & 0x40) != 0;
            let has_attrs = (token & 0x80) != 0;
            let offset = read_mb_u_int32(data, &mut pos)?;
            let tag_name = read_strtbl_string(strtbl, offset)?;

            if !is_valid_xml_name(&tag_name) {
                return Err(format!(
                    "Invalid LITERAL tag name in string table: {:?}",
                    tag_name
                ));
            }

            // Skip attributes if present (consume until END token).
            // Per WBXML spec, attributes are a flat sequence of ATTRSTART/ATTRVALUE
            // tokens terminated by END (0x01). ActiveSync doesn't use attributes, so
            // we just consume them to keep the parse position correct.
            if has_attrs {
                loop {
                    if pos >= data.len() {
                        return Err("Unexpected end while skipping LITERAL attributes".into());
                    }
                    let attr_token = data[pos];
                    pos += 1;
                    if attr_token == TAG_END {
                        break;
                    } else if attr_token == TAG_STR_I {
                        // Skip inline string
                        while pos < data.len() && data[pos] != 0 {
                            pos += 1;
                        }
                        if pos < data.len() {
                            pos += 1;
                        } else {
                            return Err("Unexpected end in attribute inline string".into());
                        }
                    } else if attr_token == 0x83 {
                        // STR_T — skip mb_u_int32 index
                        let _ = read_mb_u_int32(data, &mut pos)?;
                    } else if attr_token == TAG_OPAQUE {
                        // Skip opaque data
                        let len = read_mb_u_int32(data, &mut pos)?;
                        let end = pos.checked_add(len).ok_or_else(|| "Opaque overflow in attrs".to_string())?;
                        if end > data.len() {
                            return Err("Opaque overflow in attrs".into());
                        }
                        pos = end;
                    } else if attr_token == TAG_SWITCH_PAGE {
                        if pos >= data.len() {
                            return Err("Unexpected end in attribute page switch".into());
                        }
                        pos += 1;
                    }
                    // Other attribute tokens (ATTRSTART/ATTRVALUE) are single bytes — already consumed
                }
            }

            if pending_tag.is_some() {
                xml.push('>');
                stack.push(pending_tag.take().unwrap());
            }

            if has_content {
                if stack.len() >= MAX_DECODE_DEPTH {
                    return Err(format!(
                        "WBXML nesting depth exceeds maximum of {}",
                        MAX_DECODE_DEPTH
                    ));
                }
                pending_tag = Some(tag_name.clone());
                xml.push_str(&format!("<{}", tag_name));
            } else {
                xml.push_str(&format!("<{}/>", tag_name));
            }
            continue;
        }

        let has_content = (token & 0x40) != 0;
        let token_id = token & 0x3F;

        if let Some(tag_def) = TAG_MAP.get(&(current_page, token_id)) {
            if pending_tag.is_some() {
                xml.push('>');
                stack.push(pending_tag.take().unwrap());
            }

            if has_content {
                if stack.len() >= MAX_DECODE_DEPTH {
                    return Err(format!(
                        "WBXML nesting depth exceeds maximum of {}",
                        MAX_DECODE_DEPTH
                    ));
                }
                pending_tag = Some(tag_def.name.to_string());
                xml.push_str(&format!("<{}", tag_def.name));
            } else {
                xml.push_str(&format!("<{}/>", tag_def.name));
            }
        } else {
            // Unknown tag: preserve structure with a deterministic placeholder.
            let placeholder = format!("Unknown_{}_{:02X}", current_page, token_id);
            if pending_tag.is_some() {
                xml.push('>');
                stack.push(pending_tag.take().unwrap());
            }
            if has_content {
                if stack.len() >= MAX_DECODE_DEPTH {
                    return Err(format!(
                        "WBXML nesting depth exceeds maximum of {}",
                        MAX_DECODE_DEPTH
                    ));
                }
                pending_tag = Some(placeholder.clone());
                xml.push_str(&format!("<{}", placeholder));
            } else {
                xml.push_str(&format!("<{}/>", placeholder));
            }
        }
    }
    if pending_tag.is_some() || !stack.is_empty() {
        return Err("Unexpected end: unclosed tag(s)".into());
    }
    Ok(xml)
}

pub fn encode(xml: &str) -> Result<Vec<u8>, String> {
    // WBXML header:
    // 0x03 = WBXML version 1.3
    // 0x01 = Public ID (unknown/opaque, matches prior behavior)
    // 0x6A = Charset UTF-8
    // <strtbl_len: mb_u_int32>
    // <string table bytes>
    // <WBXML body>
    let mut reader = quick_xml::Reader::from_str(xml);
    reader.config_mut().trim_text(true);
    let mut buf = Vec::new();

    // Build WBXML body separately so we can prefix the final output with a string table.
    let mut body: Vec<u8> = Vec::new();
    let mut current_page: u8 = 0;

    // String table for LITERAL tags (used when a tag isn't present in NAME_MAP).
    let mut strtbl: Vec<u8> = Vec::new();
    let mut strtbl_index: HashMap<String, usize> = HashMap::new();

    fn write_mb_u_int32(out: &mut Vec<u8>, mut v: usize) {
        // WBXML mb_u_int32: 7-bit groups, big-endian, high bit indicates continuation.
        let mut bytes = [0u8; 10];
        let mut n = 0usize;
        loop {
            bytes[n] = (v & 0x7F) as u8;
            n += 1;
            v >>= 7;
            if v == 0 {
                break;
            }
        }
        for i in (0..n).rev() {
            let mut b = bytes[i];
            if i != 0 {
                b |= 0x80;
            }
            out.push(b);
        }
    }

    fn strtbl_offset(name: &str, idx: &mut HashMap<String, usize>, table: &mut Vec<u8>) -> usize {
        if let Some(&off) = idx.get(name) {
            return off;
        }
        let off = table.len();
        table.extend_from_slice(name.as_bytes());
        table.push(0x00);
        idx.insert(name.to_string(), off);
        off
    }

    fn encode_literal_tag(
        out: &mut Vec<u8>,
        name: &str,
        has_content: bool,
        idx: &mut HashMap<String, usize>,
        table: &mut Vec<u8>,
    ) {
        // WBXML LITERAL (0x04) / LITERAL_C (0x44): followed by string table index (mb_u_int32)
        let token = if has_content { 0x44 } else { 0x04 };
        out.push(token);
        let off = strtbl_offset(name, idx, table);
        write_mb_u_int32(out, off);
    }

    loop {
        buf.clear();
        match reader.read_event_into(&mut buf) {
            Ok(quick_xml::events::Event::Start(ref e)) => {
                let full_name = String::from_utf8_lossy(e.name().as_ref()).to_string();
                let (prefix, local) = split_prefix(&full_name);
                let target_page = prefix.and_then(|p| PREFIX_TO_PAGE.get(p).copied());
                if !encode_tag(&mut body, local, &mut current_page, true, target_page) {
                    encode_literal_tag(&mut body, local, true, &mut strtbl_index, &mut strtbl);
                }
            }
            Ok(quick_xml::events::Event::Empty(ref e)) => {
                let full_name = String::from_utf8_lossy(e.name().as_ref()).to_string();
                let (prefix, local) = split_prefix(&full_name);
                let target_page = prefix.and_then(|p| PREFIX_TO_PAGE.get(p).copied());
                if !encode_tag(&mut body, local, &mut current_page, false, target_page) {
                    encode_literal_tag(&mut body, local, false, &mut strtbl_index, &mut strtbl);
                }
            }
            Ok(quick_xml::events::Event::End(_)) => {
                body.push(TAG_END);
            }
            Ok(quick_xml::events::Event::Text(ref e)) => {
                body.push(TAG_STR_I);
                let text_str = std::str::from_utf8(e.as_ref())
                    .map_err(|_| "Invalid UTF-8 in XML text node".to_string())?;
                let t = quick_xml::escape::unescape(text_str)
                    .map_err(|e| format!("XML text unescape error: {}", e))?;
                body.extend(t.as_bytes());
                body.push(0x00);
            }
            Ok(quick_xml::events::Event::Eof) => break,
            Ok(_) => {}
            Err(e) => return Err(format!("XML parsing error: {}", e)),
        }
    }

    let mut output = vec![0x03, 0x01, 0x6A];
    write_mb_u_int32(&mut output, strtbl.len());
    output.extend_from_slice(&strtbl);
    output.extend_from_slice(&body);
    Ok(output)
}

/// Split a possibly-prefixed tag name (e.g. "Calendar:Type") into an optional
/// prefix and the local name. Returns `(None, name)` when there is no prefix.
fn split_prefix(name: &str) -> (Option<&str>, &str) {
    match name.rsplit_once(':') {
        Some((prefix, local)) if !prefix.is_empty() && !local.is_empty() => (Some(prefix), local),
        _ => (None, name),
    }
}

fn encode_tag(
    output: &mut Vec<u8>,
    name: &str,
    current_page: &mut u8,
    has_content: bool,
    target_page: Option<u8>,
) -> bool {
    if let Some(entries) = NAME_MAP.get(name) {
        // When a namespace prefix resolved to a specific code page, use that page
        // for disambiguation. Otherwise prefer the entry on the current page to
        // avoid unnecessary page switches.
        let (page, token) = if let Some(tp) = target_page {
            match entries.iter().find(|(p, _)| *p == tp) {
                Some(entry) => entry,
                None => return false,
            }
        } else {
            entries
                .iter()
                .find(|(p, _)| *p == *current_page)
                .unwrap_or(&entries[0])
        };
        if *page != *current_page {
            output.push(TAG_SWITCH_PAGE);
            output.push(*page);
            *current_page = *page;
        }
        let mut final_token = *token;
        if has_content {
            final_token |= 0x40;
        }
        output.push(final_token);
        true
    } else {
        false
    }
}
