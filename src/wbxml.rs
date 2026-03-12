use lazy_static::lazy_static;
use std::collections::HashMap;

const TAG_SWITCH_PAGE: u8 = 0x00;
const TAG_END: u8 = 0x01;
const TAG_STR_I: u8 = 0x03;
const TAG_OPAQUE: u8 = 0xC3;

const MAX_DECODE_DEPTH: usize = 256;

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
const CP_FIND: u8 = 25;

#[derive(Debug, Clone)]
struct Tag {
    name: &'static str,
    _has_content: bool,
}

lazy_static! {
    /// Maps ActiveSync‑style namespace prefixes to WBXML code pages.
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
        m.insert("Find", CP_FIND);
        m
    };

    /// Maps (code page, token) to the corresponding tag name.
    static ref TAG_MAP: HashMap<(u8, u8), Tag> = {
        let mut m = HashMap::new();
        macro_rules! add {
            ($page:expr, $token:expr, $name:expr) => {
                m.insert(($page, $token), Tag { name: $name, _has_content: true });
            };
        }

        // AirSync (0) – MS-ASWBXML §2.1.2.1.1
        add!(CP_AIRSYNC, 0x05, "Sync");
        add!(CP_AIRSYNC, 0x06, "Responses");
        add!(CP_AIRSYNC, 0x07, "Add");
        add!(CP_AIRSYNC, 0x08, "Change");
        add!(CP_AIRSYNC, 0x09, "Delete");
        add!(CP_AIRSYNC, 0x0A, "Fetch");
        add!(CP_AIRSYNC, 0x0B, "SyncKey");
        add!(CP_AIRSYNC, 0x0C, "ClientId");
        add!(CP_AIRSYNC, 0x0D, "ServerId");
        add!(CP_AIRSYNC, 0x0E, "Status");
        add!(CP_AIRSYNC, 0x0F, "Collection");
        add!(CP_AIRSYNC, 0x10, "Class");
        add!(CP_AIRSYNC, 0x12, "CollectionId");
        add!(CP_AIRSYNC, 0x13, "GetChanges");
        add!(CP_AIRSYNC, 0x14, "MoreAvailable");
        add!(CP_AIRSYNC, 0x15, "WindowSize");
        add!(CP_AIRSYNC, 0x16, "Commands");
        add!(CP_AIRSYNC, 0x17, "Options");
        add!(CP_AIRSYNC, 0x18, "FilterType");
        add!(CP_AIRSYNC, 0x19, "Truncation");
        add!(CP_AIRSYNC, 0x1B, "Conflict");
        add!(CP_AIRSYNC, 0x1C, "Collections");
        add!(CP_AIRSYNC, 0x1D, "ApplicationData");
        add!(CP_AIRSYNC, 0x1E, "DeletesAsMoves");
        add!(CP_AIRSYNC, 0x20, "Supported");
        add!(CP_AIRSYNC, 0x21, "SoftDelete");
        add!(CP_AIRSYNC, 0x22, "MIMESupport");
        add!(CP_AIRSYNC, 0x23, "MIMETruncation");
        add!(CP_AIRSYNC, 0x24, "Wait");
        add!(CP_AIRSYNC, 0x25, "Limit");
        add!(CP_AIRSYNC, 0x26, "Partial");
        add!(CP_AIRSYNC, 0x27, "ConversationMode");
        add!(CP_AIRSYNC, 0x28, "MaxItems");
        add!(CP_AIRSYNC, 0x29, "HeartbeatInterval");

        // Contacts (1) – MS-ASWBXML §2.1.2.1.2
        add!(CP_CONTACTS, 0x05, "Anniversary");
        add!(CP_CONTACTS, 0x06, "AssistantName");
        add!(CP_CONTACTS, 0x07, "AssistantPhoneNumber");
        add!(CP_CONTACTS, 0x08, "Birthday");
        add!(CP_CONTACTS, 0x09, "Body");
        add!(CP_CONTACTS, 0x0A, "BodySize");
        add!(CP_CONTACTS, 0x0B, "BodyTruncated");
        add!(CP_CONTACTS, 0x0C, "Business2PhoneNumber");
        add!(CP_CONTACTS, 0x0D, "BusinessAddressCity");
        add!(CP_CONTACTS, 0x0E, "BusinessAddressCountry");
        add!(CP_CONTACTS, 0x0F, "BusinessAddressPostalCode");
        add!(CP_CONTACTS, 0x10, "BusinessAddressState");
        add!(CP_CONTACTS, 0x11, "BusinessAddressStreet");
        add!(CP_CONTACTS, 0x12, "BusinessFaxNumber");
        add!(CP_CONTACTS, 0x13, "BusinessPhoneNumber");
        add!(CP_CONTACTS, 0x14, "CarPhoneNumber");
        add!(CP_CONTACTS, 0x15, "Categories");
        add!(CP_CONTACTS, 0x16, "Category");
        add!(CP_CONTACTS, 0x17, "Children");
        add!(CP_CONTACTS, 0x18, "Child");
        add!(CP_CONTACTS, 0x19, "CompanyName");
        add!(CP_CONTACTS, 0x1A, "Department");
        add!(CP_CONTACTS, 0x1B, "Email1Address");
        add!(CP_CONTACTS, 0x1C, "Email2Address");
        add!(CP_CONTACTS, 0x1D, "Email3Address");
        add!(CP_CONTACTS, 0x1E, "FileAs");
        add!(CP_CONTACTS, 0x1F, "FirstName");
        add!(CP_CONTACTS, 0x20, "Home2PhoneNumber");
        add!(CP_CONTACTS, 0x21, "HomeAddressCity");
        add!(CP_CONTACTS, 0x22, "HomeAddressCountry");
        add!(CP_CONTACTS, 0x23, "HomeAddressPostalCode");
        add!(CP_CONTACTS, 0x24, "HomeAddressState");
        add!(CP_CONTACTS, 0x25, "HomeAddressStreet");
        add!(CP_CONTACTS, 0x26, "HomeFaxNumber");
        add!(CP_CONTACTS, 0x27, "HomePhoneNumber");
        add!(CP_CONTACTS, 0x28, "JobTitle");
        add!(CP_CONTACTS, 0x29, "LastName");
        add!(CP_CONTACTS, 0x2A, "MiddleName");
        add!(CP_CONTACTS, 0x2B, "MobilePhoneNumber");
        add!(CP_CONTACTS, 0x2C, "OfficeLocation");
        add!(CP_CONTACTS, 0x2F, "PagerNumber");
        add!(CP_CONTACTS, 0x31, "Spouse");
        add!(CP_CONTACTS, 0x32, "Suffix");
        add!(CP_CONTACTS, 0x33, "Title");
        add!(CP_CONTACTS, 0x34, "WebPage");
        add!(CP_CONTACTS, 0x35, "YomiCompanyName");
        add!(CP_CONTACTS, 0x36, "YomiFirstName");
        add!(CP_CONTACTS, 0x37, "YomiLastName");
        add!(CP_CONTACTS, 0x3C, "Picture");
        add!(CP_CONTACTS, 0x3D, "Alias");
        add!(CP_CONTACTS, 0x3E, "WeightedRank");

        // Email (2) – MS-ASWBXML §2.1.2.1.3
        add!(CP_EMAIL, 0x05, "Attachment");
        add!(CP_EMAIL, 0x06, "Attachments");
        add!(CP_EMAIL, 0x07, "AttName");
        add!(CP_EMAIL, 0x08, "AttSize");
        add!(CP_EMAIL, 0x09, "Att0Id");
        add!(CP_EMAIL, 0x0A, "AttMethod");
        // 0x0B unused
        add!(CP_EMAIL, 0x0C, "Body");
        add!(CP_EMAIL, 0x0D, "BodySize");
        add!(CP_EMAIL, 0x0E, "BodyTruncated");
        add!(CP_EMAIL, 0x0F, "DateReceived");
        add!(CP_EMAIL, 0x10, "DisplayName");
        add!(CP_EMAIL, 0x11, "DisplayTo");
        add!(CP_EMAIL, 0x12, "Importance");
        add!(CP_EMAIL, 0x13, "MessageClass");
        add!(CP_EMAIL, 0x14, "Subject");
        add!(CP_EMAIL, 0x15, "Read");
        add!(CP_EMAIL, 0x16, "To");
        add!(CP_EMAIL, 0x17, "Cc");
        add!(CP_EMAIL, 0x18, "From");
        add!(CP_EMAIL, 0x19, "ReplyTo");
        add!(CP_EMAIL, 0x1A, "AllDayEvent");
        add!(CP_EMAIL, 0x1B, "Categories");
        add!(CP_EMAIL, 0x1C, "Category");
        add!(CP_EMAIL, 0x1D, "DtStamp");
        add!(CP_EMAIL, 0x1E, "EndTime");
        add!(CP_EMAIL, 0x1F, "InstanceType");
        add!(CP_EMAIL, 0x20, "BusyStatus");
        add!(CP_EMAIL, 0x21, "Location");
        add!(CP_EMAIL, 0x22, "MeetingRequest");
        add!(CP_EMAIL, 0x23, "Organizer");
        add!(CP_EMAIL, 0x24, "RecurrenceId");
        add!(CP_EMAIL, 0x25, "Reminder");
        add!(CP_EMAIL, 0x26, "ResponseRequested");
        add!(CP_EMAIL, 0x27, "Recurrences");
        add!(CP_EMAIL, 0x28, "Recurrence");
        add!(CP_EMAIL, 0x29, "Type");
        add!(CP_EMAIL, 0x2A, "Until");
        add!(CP_EMAIL, 0x2B, "Occurrences");
        add!(CP_EMAIL, 0x2C, "Interval");
        add!(CP_EMAIL, 0x2D, "DayOfWeek");
        add!(CP_EMAIL, 0x2E, "DayOfMonth");
        add!(CP_EMAIL, 0x2F, "WeekOfMonth");
        add!(CP_EMAIL, 0x30, "MonthOfYear");
        add!(CP_EMAIL, 0x31, "StartTime");
        add!(CP_EMAIL, 0x32, "Sensitivity");
        add!(CP_EMAIL, 0x33, "TimeZone");
        add!(CP_EMAIL, 0x34, "GlobalObjId");
        add!(CP_EMAIL, 0x35, "ThreadTopic");
        add!(CP_EMAIL, 0x36, "MIMEData");
        add!(CP_EMAIL, 0x37, "MIMETruncated");
        add!(CP_EMAIL, 0x38, "MIMESize");
        add!(CP_EMAIL, 0x39, "InternetCPID");
        add!(CP_EMAIL, 0x3A, "Flag");
        add!(CP_EMAIL, 0x3B, "Status");
        add!(CP_EMAIL, 0x3C, "ContentClass");
        add!(CP_EMAIL, 0x3D, "FlagType");
        add!(CP_EMAIL, 0x3E, "CompleteTime");
        add!(CP_EMAIL, 0x3F, "DisallowNewTimeProposal");

        // Calendar (4) – MS-ASWBXML §2.1.2.1.5
        add!(CP_CALENDAR, 0x05, "TimeZone");
        add!(CP_CALENDAR, 0x06, "AllDayEvent");
        add!(CP_CALENDAR, 0x07, "Attendees");
        add!(CP_CALENDAR, 0x08, "Attendee");
        add!(CP_CALENDAR, 0x09, "Email");
        add!(CP_CALENDAR, 0x0A, "Name");
        add!(CP_CALENDAR, 0x0D, "BusyStatus");
        add!(CP_CALENDAR, 0x0E, "Categories");
        add!(CP_CALENDAR, 0x0F, "Category");
        add!(CP_CALENDAR, 0x11, "DtStamp");
        add!(CP_CALENDAR, 0x12, "EndTime");
        add!(CP_CALENDAR, 0x13, "Exception");
        add!(CP_CALENDAR, 0x14, "Exceptions");
        add!(CP_CALENDAR, 0x15, "Deleted");
        add!(CP_CALENDAR, 0x16, "ExceptionStartTime");
        add!(CP_CALENDAR, 0x17, "Location");
        add!(CP_CALENDAR, 0x18, "MeetingStatus");
        add!(CP_CALENDAR, 0x19, "OrganizerEmail");
        add!(CP_CALENDAR, 0x1A, "OrganizerName");
        add!(CP_CALENDAR, 0x1B, "Recurrence");
        add!(CP_CALENDAR, 0x1C, "Type");
        add!(CP_CALENDAR, 0x1D, "Until");
        add!(CP_CALENDAR, 0x1E, "Occurrences");
        add!(CP_CALENDAR, 0x1F, "Interval");
        add!(CP_CALENDAR, 0x20, "DayOfWeek");
        add!(CP_CALENDAR, 0x21, "DayOfMonth");
        add!(CP_CALENDAR, 0x22, "WeekOfMonth");
        add!(CP_CALENDAR, 0x23, "MonthOfYear");
        add!(CP_CALENDAR, 0x24, "Reminder");
        add!(CP_CALENDAR, 0x25, "Sensitivity");
        add!(CP_CALENDAR, 0x26, "Subject");
        add!(CP_CALENDAR, 0x27, "StartTime");
        add!(CP_CALENDAR, 0x28, "UID");
        add!(CP_CALENDAR, 0x29, "AttendeeStatus");
        add!(CP_CALENDAR, 0x2A, "AttendeeType");
        add!(CP_CALENDAR, 0x33, "DisallowNewTimeProposal");
        add!(CP_CALENDAR, 0x34, "ResponseRequested");
        add!(CP_CALENDAR, 0x35, "AppointmentReplyTime");
        add!(CP_CALENDAR, 0x36, "ResponseType");
        add!(CP_CALENDAR, 0x37, "CalendarType");
        add!(CP_CALENDAR, 0x38, "IsLeapMonth");
        add!(CP_CALENDAR, 0x39, "FirstDayOfWeek");
        add!(CP_CALENDAR, 0x3A, "OnlineMeetingConfLink");
        add!(CP_CALENDAR, 0x3B, "OnlineMeetingExternalLink");
        add!(CP_CALENDAR, 0x3C, "ClientUid");

        // Move (5) – MS-ASWBXML §2.1.2.1.6
        add!(CP_MOVE, 0x05, "MoveItems");
        add!(CP_MOVE, 0x06, "Move");
        add!(CP_MOVE, 0x07, "SrcMsgId");
        add!(CP_MOVE, 0x08, "SrcFldId");
        add!(CP_MOVE, 0x09, "DstFldId");
        add!(CP_MOVE, 0x0A, "Response");
        add!(CP_MOVE, 0x0B, "Status");
        add!(CP_MOVE, 0x0C, "DstMsgId");

        // GetItemEstimate (6) – MS-ASWBXML §2.1.2.1.7
        add!(CP_GETITEMESTIMATE, 0x05, "GetItemEstimate");
        add!(CP_GETITEMESTIMATE, 0x06, "Version");
        add!(CP_GETITEMESTIMATE, 0x07, "Collections");
        add!(CP_GETITEMESTIMATE, 0x08, "Collection");
        add!(CP_GETITEMESTIMATE, 0x09, "Class");
        add!(CP_GETITEMESTIMATE, 0x0A, "CollectionId");
        add!(CP_GETITEMESTIMATE, 0x0B, "DateTime");
        add!(CP_GETITEMESTIMATE, 0x0C, "Estimate");
        add!(CP_GETITEMESTIMATE, 0x0D, "Response");
        add!(CP_GETITEMESTIMATE, 0x0E, "Status");

        // FolderHierarchy (7) – MS-ASWBXML §2.1.2.1.8
        add!(CP_FOLDERHIERARCHY, 0x05, "Folders");
        add!(CP_FOLDERHIERARCHY, 0x06, "Folder");
        add!(CP_FOLDERHIERARCHY, 0x07, "DisplayName");
        add!(CP_FOLDERHIERARCHY, 0x08, "ServerId");
        add!(CP_FOLDERHIERARCHY, 0x09, "ParentId");
        add!(CP_FOLDERHIERARCHY, 0x0A, "Type");
        add!(CP_FOLDERHIERARCHY, 0x0C, "Status");
        add!(CP_FOLDERHIERARCHY, 0x0D, "ContentClass");
        add!(CP_FOLDERHIERARCHY, 0x0E, "Changes");
        add!(CP_FOLDERHIERARCHY, 0x0F, "Add");
        add!(CP_FOLDERHIERARCHY, 0x10, "Delete");
        add!(CP_FOLDERHIERARCHY, 0x11, "Update");
        add!(CP_FOLDERHIERARCHY, 0x12, "SyncKey");
        add!(CP_FOLDERHIERARCHY, 0x13, "FolderCreate");
        add!(CP_FOLDERHIERARCHY, 0x14, "FolderDelete");
        add!(CP_FOLDERHIERARCHY, 0x15, "FolderUpdate");
        add!(CP_FOLDERHIERARCHY, 0x16, "FolderSync");
        add!(CP_FOLDERHIERARCHY, 0x17, "Count");

        // MeetingResponse (8) – MS-ASWBXML §2.1.2.1.9
        add!(CP_MEETINGRESPONSE, 0x05, "CalendarId");
        add!(CP_MEETINGRESPONSE, 0x06, "CollectionId");
        add!(CP_MEETINGRESPONSE, 0x07, "MeetingResponse");
        add!(CP_MEETINGRESPONSE, 0x08, "RequestId");
        add!(CP_MEETINGRESPONSE, 0x09, "Request");
        add!(CP_MEETINGRESPONSE, 0x0A, "Result");
        add!(CP_MEETINGRESPONSE, 0x0B, "Status");
        add!(CP_MEETINGRESPONSE, 0x0C, "UserResponse");
        add!(CP_MEETINGRESPONSE, 0x0E, "InstanceId");
        add!(CP_MEETINGRESPONSE, 0x10, "ProposedStartTime");
        add!(CP_MEETINGRESPONSE, 0x11, "ProposedEndTime");
        add!(CP_MEETINGRESPONSE, 0x12, "SendResponse");

        // Tasks (9) – MS-ASWBXML §2.1.2.1.10
        add!(CP_TASKS, 0x08, "Categories");
        add!(CP_TASKS, 0x09, "Category");
        add!(CP_TASKS, 0x0A, "Complete");
        add!(CP_TASKS, 0x0B, "DateCompleted");
        add!(CP_TASKS, 0x0C, "DueDate");
        add!(CP_TASKS, 0x0D, "UtcDueDate");
        add!(CP_TASKS, 0x0E, "Importance");
        add!(CP_TASKS, 0x0F, "Recurrence");
        add!(CP_TASKS, 0x10, "Type");
        add!(CP_TASKS, 0x11, "Start");
        add!(CP_TASKS, 0x12, "Until");
        add!(CP_TASKS, 0x13, "Occurrences");
        add!(CP_TASKS, 0x14, "Interval");
        add!(CP_TASKS, 0x15, "DayOfMonth");
        add!(CP_TASKS, 0x16, "DayOfWeek");
        add!(CP_TASKS, 0x17, "WeekOfMonth");
        add!(CP_TASKS, 0x18, "MonthOfYear");
        add!(CP_TASKS, 0x19, "Regenerate");
        add!(CP_TASKS, 0x1A, "DeadOccur");
        add!(CP_TASKS, 0x1B, "ReminderSet");
        add!(CP_TASKS, 0x1C, "ReminderTime");
        add!(CP_TASKS, 0x1D, "Sensitivity");
        add!(CP_TASKS, 0x1E, "StartDate");
        add!(CP_TASKS, 0x1F, "UtcStartDate");
        add!(CP_TASKS, 0x20, "Subject");
        add!(CP_TASKS, 0x22, "OrdinalDate");
        add!(CP_TASKS, 0x23, "SubOrdinalDate");
        add!(CP_TASKS, 0x24, "CalendarType");
        add!(CP_TASKS, 0x25, "IsLeapMonth");
        add!(CP_TASKS, 0x26, "FirstDayOfWeek");

        // ResolveRecipients (10) – MS-ASWBXML §2.1.2.1.11
        add!(CP_RESOLVERECIPIENTS, 0x05, "ResolveRecipients");
        add!(CP_RESOLVERECIPIENTS, 0x06, "Response");
        add!(CP_RESOLVERECIPIENTS, 0x07, "Status");
        add!(CP_RESOLVERECIPIENTS, 0x08, "Type");
        add!(CP_RESOLVERECIPIENTS, 0x09, "Recipient");
        add!(CP_RESOLVERECIPIENTS, 0x0A, "DisplayName");
        add!(CP_RESOLVERECIPIENTS, 0x0B, "EmailAddress");
        add!(CP_RESOLVERECIPIENTS, 0x0C, "Certificates");
        add!(CP_RESOLVERECIPIENTS, 0x0D, "Certificate");
        add!(CP_RESOLVERECIPIENTS, 0x0E, "MiniCertificate");
        add!(CP_RESOLVERECIPIENTS, 0x0F, "Options");
        add!(CP_RESOLVERECIPIENTS, 0x10, "To");
        add!(CP_RESOLVERECIPIENTS, 0x11, "CertificateRetrieval");
        add!(CP_RESOLVERECIPIENTS, 0x12, "RecipientCount");
        add!(CP_RESOLVERECIPIENTS, 0x13, "MaxCertificates");
        add!(CP_RESOLVERECIPIENTS, 0x14, "MaxAmbiguousRecipients");
        add!(CP_RESOLVERECIPIENTS, 0x15, "CertificateCount");
        add!(CP_RESOLVERECIPIENTS, 0x16, "Availability");
        add!(CP_RESOLVERECIPIENTS, 0x17, "StartTime");
        add!(CP_RESOLVERECIPIENTS, 0x18, "EndTime");
        add!(CP_RESOLVERECIPIENTS, 0x19, "MergedFreeBusy");
        add!(CP_RESOLVERECIPIENTS, 0x1A, "Picture");
        add!(CP_RESOLVERECIPIENTS, 0x1B, "MaxSize");
        add!(CP_RESOLVERECIPIENTS, 0x1C, "Data");
        add!(CP_RESOLVERECIPIENTS, 0x1D, "MaxPictures");

        // ValidateCert (11) – MS-ASWBXML §2.1.2.1.12
        add!(CP_VALIDATECERT, 0x05, "ValidateCert");
        add!(CP_VALIDATECERT, 0x06, "Certificates");
        add!(CP_VALIDATECERT, 0x07, "Certificate");
        add!(CP_VALIDATECERT, 0x08, "CertificateChain");
        add!(CP_VALIDATECERT, 0x09, "CheckCRL");
        add!(CP_VALIDATECERT, 0x0A, "Status");

        // Contacts2 (12) – MS-ASWBXML §2.1.2.1.13
        add!(CP_CONTACTS2, 0x05, "CustomerId");
        add!(CP_CONTACTS2, 0x06, "GovernmentId");
        add!(CP_CONTACTS2, 0x07, "IMAddress");
        add!(CP_CONTACTS2, 0x08, "IMAddress2");
        add!(CP_CONTACTS2, 0x09, "IMAddress3");
        add!(CP_CONTACTS2, 0x0A, "ManagerName");
        add!(CP_CONTACTS2, 0x0B, "CompanyMainPhone");
        add!(CP_CONTACTS2, 0x0C, "AccountName");
        add!(CP_CONTACTS2, 0x0D, "NickName");
        add!(CP_CONTACTS2, 0x0E, "MMS");

        // Ping (13) – MS-ASWBXML §2.1.2.1.14
        add!(CP_PING, 0x05, "Ping");
        add!(CP_PING, 0x07, "Status");
        add!(CP_PING, 0x08, "HeartbeatInterval");
        add!(CP_PING, 0x09, "Folders");
        add!(CP_PING, 0x0A, "Folder");
        add!(CP_PING, 0x0B, "Id");
        add!(CP_PING, 0x0C, "Class");
        add!(CP_PING, 0x0D, "MaxFolders");

        // Provision (14) – MS-ASWBXML §2.1.2.1.15
        add!(CP_PROVISION, 0x05, "Provision");
        add!(CP_PROVISION, 0x06, "Policies");
        add!(CP_PROVISION, 0x07, "Policy");
        add!(CP_PROVISION, 0x08, "PolicyType");
        add!(CP_PROVISION, 0x09, "PolicyKey");
        add!(CP_PROVISION, 0x0A, "Data");
        add!(CP_PROVISION, 0x0B, "Status");
        add!(CP_PROVISION, 0x0C, "RemoteWipe");
        add!(CP_PROVISION, 0x0D, "EASProvisionDoc");
        add!(CP_PROVISION, 0x0E, "DevicePasswordEnabled");
        add!(CP_PROVISION, 0x0F, "AlphanumericDevicePasswordRequired");
        add!(CP_PROVISION, 0x10, "RequireStorageCardEncryption");
        add!(CP_PROVISION, 0x11, "PasswordRecoveryEnabled");
        add!(CP_PROVISION, 0x13, "AttachmentsEnabled");
        add!(CP_PROVISION, 0x14, "MinDevicePasswordLength");
        add!(CP_PROVISION, 0x15, "MaxInactivityTimeDeviceLock");
        add!(CP_PROVISION, 0x16, "MaxDevicePasswordFailedAttempts");
        add!(CP_PROVISION, 0x17, "MaxAttachmentSize");
        add!(CP_PROVISION, 0x18, "AllowSimpleDevicePassword");
        add!(CP_PROVISION, 0x19, "DevicePasswordExpiration");
        add!(CP_PROVISION, 0x1A, "DevicePasswordHistory");
        add!(CP_PROVISION, 0x1B, "AllowStorageCard");
        add!(CP_PROVISION, 0x1C, "AllowCamera");
        add!(CP_PROVISION, 0x1D, "RequireDeviceEncryption");
        add!(CP_PROVISION, 0x1E, "AllowUnsignedApplications");
        add!(CP_PROVISION, 0x1F, "AllowUnsignedInstallationPackages");
        add!(CP_PROVISION, 0x20, "MinDevicePasswordComplexCharacters");
        add!(CP_PROVISION, 0x21, "AllowWiFi");
        add!(CP_PROVISION, 0x22, "AllowTextMessaging");
        add!(CP_PROVISION, 0x23, "AllowPOPIMAPEmail");
        add!(CP_PROVISION, 0x24, "AllowBluetooth");
        add!(CP_PROVISION, 0x25, "AllowIrDA");
        add!(CP_PROVISION, 0x26, "RequireManualSyncWhenRoaming");
        add!(CP_PROVISION, 0x27, "AllowDesktopSync");
        add!(CP_PROVISION, 0x28, "MaxCalendarAgeFilter");
        add!(CP_PROVISION, 0x29, "AllowHTMLEmail");
        add!(CP_PROVISION, 0x2A, "MaxEmailAgeFilter");
        add!(CP_PROVISION, 0x2B, "MaxEmailBodyTruncationSize");
        add!(CP_PROVISION, 0x2C, "MaxEmailHTMLBodyTruncationSize");
        add!(CP_PROVISION, 0x2D, "RequireSignedSMIMEMessages");
        add!(CP_PROVISION, 0x2E, "RequireEncryptedSMIMEMessages");
        add!(CP_PROVISION, 0x2F, "RequireSignedSMIMEAlgorithm");
        add!(CP_PROVISION, 0x30, "RequireEncryptionSMIMEAlgorithm");
        add!(CP_PROVISION, 0x31, "AllowSMIMEEncryptionAlgorithmNegotiation");
        add!(CP_PROVISION, 0x32, "AllowSMIMESoftCerts");
        add!(CP_PROVISION, 0x33, "AllowBrowser");
        add!(CP_PROVISION, 0x34, "AllowConsumerEmail");
        add!(CP_PROVISION, 0x35, "AllowRemoteDesktop");
        add!(CP_PROVISION, 0x36, "AllowInternetSharing");
        add!(CP_PROVISION, 0x37, "UnapprovedInROMApplicationList");
        add!(CP_PROVISION, 0x38, "ApplicationName");
        add!(CP_PROVISION, 0x39, "ApprovedApplicationList");
        add!(CP_PROVISION, 0x3A, "Hash");

        // Search (15) – MS-ASWBXML §2.1.2.1.16
        add!(CP_SEARCH, 0x05, "Search");
        add!(CP_SEARCH, 0x07, "Store");
        add!(CP_SEARCH, 0x08, "Name");
        add!(CP_SEARCH, 0x09, "Query");
        add!(CP_SEARCH, 0x0A, "Options");
        add!(CP_SEARCH, 0x0B, "Range");
        add!(CP_SEARCH, 0x0C, "Status");
        add!(CP_SEARCH, 0x0D, "Response");
        add!(CP_SEARCH, 0x0E, "Result");
        add!(CP_SEARCH, 0x0F, "Properties");
        add!(CP_SEARCH, 0x10, "Total");
        add!(CP_SEARCH, 0x11, "EqualTo");
        add!(CP_SEARCH, 0x12, "Value");
        add!(CP_SEARCH, 0x13, "And");
        add!(CP_SEARCH, 0x14, "Or");
        add!(CP_SEARCH, 0x15, "FreeText");
        add!(CP_SEARCH, 0x17, "DeepTraversal");
        add!(CP_SEARCH, 0x18, "LongId");
        add!(CP_SEARCH, 0x19, "RebuildResults");
        add!(CP_SEARCH, 0x1A, "LessThan");
        add!(CP_SEARCH, 0x1B, "GreaterThan");
        add!(CP_SEARCH, 0x1E, "UserName");
        add!(CP_SEARCH, 0x1F, "Password");
        add!(CP_SEARCH, 0x20, "ConversationId");
        add!(CP_SEARCH, 0x21, "Picture");
        add!(CP_SEARCH, 0x22, "MaxSize");
        add!(CP_SEARCH, 0x23, "MaxPictures");

        // GAL (16) – MS-ASWBXML §2.1.2.1.17
        add!(CP_GAL, 0x05, "DisplayName");
        add!(CP_GAL, 0x06, "Phone");
        add!(CP_GAL, 0x07, "Office");
        add!(CP_GAL, 0x08, "Title");
        add!(CP_GAL, 0x09, "Company");
        add!(CP_GAL, 0x0A, "Alias");
        add!(CP_GAL, 0x0B, "FirstName");
        add!(CP_GAL, 0x0C, "LastName");
        add!(CP_GAL, 0x0D, "HomePhone");
        add!(CP_GAL, 0x0E, "MobilePhone");
        add!(CP_GAL, 0x0F, "EmailAddress");
        add!(CP_GAL, 0x10, "Picture");
        add!(CP_GAL, 0x11, "Status");
        add!(CP_GAL, 0x12, "Data");

        // AirSyncBase (17) – MS-ASWBXML §2.1.2.1.18
        add!(CP_AIRSYNCBASE, 0x05, "BodyPreference");
        add!(CP_AIRSYNCBASE, 0x06, "Type");
        add!(CP_AIRSYNCBASE, 0x07, "TruncationSize");
        add!(CP_AIRSYNCBASE, 0x08, "AllOrNone");
        add!(CP_AIRSYNCBASE, 0x0A, "Body");
        add!(CP_AIRSYNCBASE, 0x0B, "Data");
        add!(CP_AIRSYNCBASE, 0x0C, "EstimatedDataSize");
        add!(CP_AIRSYNCBASE, 0x0D, "Truncated");
        add!(CP_AIRSYNCBASE, 0x0E, "Attachments");
        add!(CP_AIRSYNCBASE, 0x0F, "Attachment");
        add!(CP_AIRSYNCBASE, 0x10, "DisplayName");
        add!(CP_AIRSYNCBASE, 0x11, "FileReference");
        add!(CP_AIRSYNCBASE, 0x12, "Method");
        add!(CP_AIRSYNCBASE, 0x13, "ContentId");
        add!(CP_AIRSYNCBASE, 0x14, "ContentLocation");
        add!(CP_AIRSYNCBASE, 0x15, "IsInline");
        add!(CP_AIRSYNCBASE, 0x16, "NativeBodyType");
        add!(CP_AIRSYNCBASE, 0x17, "ContentType");
        add!(CP_AIRSYNCBASE, 0x18, "Preview");
        add!(CP_AIRSYNCBASE, 0x19, "BodyPartPreference");
        add!(CP_AIRSYNCBASE, 0x1A, "BodyPart");
        add!(CP_AIRSYNCBASE, 0x1B, "Status");
        add!(CP_AIRSYNCBASE, 0x1C, "Add");
        add!(CP_AIRSYNCBASE, 0x1D, "Delete");
        add!(CP_AIRSYNCBASE, 0x1E, "ClientId");
        add!(CP_AIRSYNCBASE, 0x1F, "Content");
        add!(CP_AIRSYNCBASE, 0x20, "Location");
        add!(CP_AIRSYNCBASE, 0x21, "Annotation");
        add!(CP_AIRSYNCBASE, 0x22, "Street");
        add!(CP_AIRSYNCBASE, 0x23, "City");
        add!(CP_AIRSYNCBASE, 0x24, "State");
        add!(CP_AIRSYNCBASE, 0x25, "Country");
        add!(CP_AIRSYNCBASE, 0x26, "PostalCode");
        add!(CP_AIRSYNCBASE, 0x27, "Latitude");
        add!(CP_AIRSYNCBASE, 0x28, "Longitude");
        add!(CP_AIRSYNCBASE, 0x29, "Accuracy");
        add!(CP_AIRSYNCBASE, 0x2A, "Altitude");
        add!(CP_AIRSYNCBASE, 0x2B, "AltitudeAccuracy");

        // Settings (18) – MS-ASWBXML §2.1.2.1.19
        add!(CP_SETTINGS, 0x05, "Settings");
        add!(CP_SETTINGS, 0x06, "Status");
        add!(CP_SETTINGS, 0x07, "Get");
        add!(CP_SETTINGS, 0x08, "Set");
        add!(CP_SETTINGS, 0x09, "Oof");
        add!(CP_SETTINGS, 0x0A, "OofState");
        add!(CP_SETTINGS, 0x0B, "StartTime");
        add!(CP_SETTINGS, 0x0C, "EndTime");
        add!(CP_SETTINGS, 0x0D, "OofMessage");
        add!(CP_SETTINGS, 0x0E, "AppliesToInternal");
        add!(CP_SETTINGS, 0x0F, "AppliesToExternalKnown");
        add!(CP_SETTINGS, 0x10, "AppliesToExternalUnknown");
        add!(CP_SETTINGS, 0x11, "Enabled");
        add!(CP_SETTINGS, 0x12, "ReplyMessage");
        add!(CP_SETTINGS, 0x13, "BodyType");
        add!(CP_SETTINGS, 0x14, "Password");
        add!(CP_SETTINGS, 0x15, "DevicePassword");
        add!(CP_SETTINGS, 0x16, "DeviceInformation");
        add!(CP_SETTINGS, 0x17, "Model");
        add!(CP_SETTINGS, 0x18, "IMEI");
        add!(CP_SETTINGS, 0x19, "FriendlyName");
        add!(CP_SETTINGS, 0x1A, "OS");
        add!(CP_SETTINGS, 0x1B, "OSLanguage");
        add!(CP_SETTINGS, 0x1C, "PhoneNumber");
        add!(CP_SETTINGS, 0x1D, "UserInformation");
        add!(CP_SETTINGS, 0x1E, "EmailAddresses");
        add!(CP_SETTINGS, 0x1F, "SmtpAddress");
        add!(CP_SETTINGS, 0x20, "UserAgent");
        add!(CP_SETTINGS, 0x21, "EnableOutboundSMS");
        add!(CP_SETTINGS, 0x22, "MobileOperator");
        add!(CP_SETTINGS, 0x23, "PrimarySmtpAddress");
        add!(CP_SETTINGS, 0x24, "Accounts");
        add!(CP_SETTINGS, 0x25, "Account");
        add!(CP_SETTINGS, 0x26, "AccountId");
        add!(CP_SETTINGS, 0x27, "AccountName");
        add!(CP_SETTINGS, 0x28, "UserDisplayName");
        add!(CP_SETTINGS, 0x29, "SendDisabled");
        add!(CP_SETTINGS, 0x2B, "RightsManagementInformation");

        // DocumentLibrary (19) – MS-ASWBXML §2.1.2.1.20
        add!(CP_DOCUMENTLIBRARY, 0x05, "LinkId");
        add!(CP_DOCUMENTLIBRARY, 0x06, "DisplayName");
        add!(CP_DOCUMENTLIBRARY, 0x07, "IsFolder");
        add!(CP_DOCUMENTLIBRARY, 0x09, "CreationDate");
        add!(CP_DOCUMENTLIBRARY, 0x0A, "LastModifiedDate");
        add!(CP_DOCUMENTLIBRARY, 0x0B, "IsHidden");
        add!(CP_DOCUMENTLIBRARY, 0x0C, "ContentLength");
        add!(CP_DOCUMENTLIBRARY, 0x0D, "ContentType");

        // ItemOperations (20) – MS-ASWBXML §2.1.2.1.21
        add!(CP_ITEMOPERATIONS, 0x05, "ItemOperations");
        add!(CP_ITEMOPERATIONS, 0x06, "Fetch");
        add!(CP_ITEMOPERATIONS, 0x07, "Store");
        add!(CP_ITEMOPERATIONS, 0x08, "Options");
        add!(CP_ITEMOPERATIONS, 0x09, "Range");
        add!(CP_ITEMOPERATIONS, 0x0A, "Total");
        add!(CP_ITEMOPERATIONS, 0x0B, "Properties");
        add!(CP_ITEMOPERATIONS, 0x0C, "Data");
        add!(CP_ITEMOPERATIONS, 0x0D, "Status");
        add!(CP_ITEMOPERATIONS, 0x0E, "Response");
        add!(CP_ITEMOPERATIONS, 0x0F, "Version");
        add!(CP_ITEMOPERATIONS, 0x10, "Schema");
        add!(CP_ITEMOPERATIONS, 0x11, "Part");
        add!(CP_ITEMOPERATIONS, 0x12, "EmptyFolderContents");
        add!(CP_ITEMOPERATIONS, 0x13, "DeleteSubFolders");
        add!(CP_ITEMOPERATIONS, 0x14, "UserName");
        add!(CP_ITEMOPERATIONS, 0x15, "Password");
        add!(CP_ITEMOPERATIONS, 0x16, "Move");
        add!(CP_ITEMOPERATIONS, 0x17, "DstFldId");
        add!(CP_ITEMOPERATIONS, 0x18, "ConversationId");
        add!(CP_ITEMOPERATIONS, 0x19, "MoveAlways");

        // ComposeMail (21) – MS-ASWBXML §2.1.2.1.22
        add!(CP_COMPOSEMAIL, 0x05, "SendMail");
        add!(CP_COMPOSEMAIL, 0x06, "SmartForward");
        add!(CP_COMPOSEMAIL, 0x07, "SmartReply");
        add!(CP_COMPOSEMAIL, 0x08, "SaveInSentItems");
        add!(CP_COMPOSEMAIL, 0x09, "ReplaceMime");
        add!(CP_COMPOSEMAIL, 0x0B, "Source");
        add!(CP_COMPOSEMAIL, 0x0C, "FolderId");
        add!(CP_COMPOSEMAIL, 0x0D, "ItemId");
        add!(CP_COMPOSEMAIL, 0x0E, "LongId");
        add!(CP_COMPOSEMAIL, 0x0F, "InstanceId");
        add!(CP_COMPOSEMAIL, 0x10, "Mime");
        add!(CP_COMPOSEMAIL, 0x11, "ClientId");
        add!(CP_COMPOSEMAIL, 0x12, "Status");
        add!(CP_COMPOSEMAIL, 0x13, "AccountId");
        add!(CP_COMPOSEMAIL, 0x15, "Forwardees");
        add!(CP_COMPOSEMAIL, 0x16, "Forwardee");
        add!(CP_COMPOSEMAIL, 0x17, "Name");
        add!(CP_COMPOSEMAIL, 0x18, "Email");

        // Email2 (22) – MS-ASWBXML §2.1.2.1.23
        add!(CP_EMAIL2, 0x05, "UmCallerID");
        add!(CP_EMAIL2, 0x06, "UmUserNotes");
        add!(CP_EMAIL2, 0x07, "UmAttDuration");
        add!(CP_EMAIL2, 0x08, "UmAttOrder");
        add!(CP_EMAIL2, 0x09, "ConversationId");
        add!(CP_EMAIL2, 0x0A, "ConversationIndex");
        add!(CP_EMAIL2, 0x0B, "LastVerbExecuted");
        add!(CP_EMAIL2, 0x0C, "LastVerbExecutionTime");
        add!(CP_EMAIL2, 0x0D, "ReceivedAsBcc");
        add!(CP_EMAIL2, 0x0E, "Sender");
        add!(CP_EMAIL2, 0x0F, "CalendarType");
        add!(CP_EMAIL2, 0x10, "IsLeapMonth");
        add!(CP_EMAIL2, 0x11, "AccountId");
        add!(CP_EMAIL2, 0x12, "FirstDayOfWeek");
        add!(CP_EMAIL2, 0x13, "MeetingMessageType");
        add!(CP_EMAIL2, 0x15, "IsDraft");
        add!(CP_EMAIL2, 0x16, "Bcc");
        add!(CP_EMAIL2, 0x17, "Send");

        // Notes (23) – MS-ASWBXML §2.1.2.1.24
        add!(CP_NOTES, 0x05, "Subject");
        add!(CP_NOTES, 0x06, "MessageClass");
        add!(CP_NOTES, 0x07, "LastModifiedDate");
        add!(CP_NOTES, 0x08, "Categories");
        add!(CP_NOTES, 0x09, "Category");

        // RightsManagement (24) – MS-ASWBXML §2.1.2.1.25
        add!(CP_RIGHTSMANAGEMENT, 0x05, "RightsManagementSupport");
        add!(CP_RIGHTSMANAGEMENT, 0x06, "RightsManagementTemplates");
        add!(CP_RIGHTSMANAGEMENT, 0x07, "RightsManagementTemplate");
        add!(CP_RIGHTSMANAGEMENT, 0x08, "RightsManagementLicense");
        add!(CP_RIGHTSMANAGEMENT, 0x09, "EditAllowed");
        add!(CP_RIGHTSMANAGEMENT, 0x0A, "ReplyAllowed");
        add!(CP_RIGHTSMANAGEMENT, 0x0B, "ReplyAllAllowed");
        add!(CP_RIGHTSMANAGEMENT, 0x0C, "ForwardAllowed");
        add!(CP_RIGHTSMANAGEMENT, 0x0D, "ModifyRecipientsAllowed");
        add!(CP_RIGHTSMANAGEMENT, 0x0E, "ExtractAllowed");
        add!(CP_RIGHTSMANAGEMENT, 0x0F, "PrintAllowed");
        add!(CP_RIGHTSMANAGEMENT, 0x10, "ExportAllowed");
        add!(CP_RIGHTSMANAGEMENT, 0x11, "ProgrammaticAccessAllowed");
        add!(CP_RIGHTSMANAGEMENT, 0x12, "Owner");
        add!(CP_RIGHTSMANAGEMENT, 0x13, "ContentExpiryDate");
        add!(CP_RIGHTSMANAGEMENT, 0x14, "TemplateID");
        add!(CP_RIGHTSMANAGEMENT, 0x15, "TemplateName");
        add!(CP_RIGHTSMANAGEMENT, 0x16, "TemplateDescription");
        add!(CP_RIGHTSMANAGEMENT, 0x17, "ContentOwner");
        add!(CP_RIGHTSMANAGEMENT, 0x18, "RemoveRightsManagementProtection");

        // Find (25) – MS-ASWBXML §2.1.2.1.26
        add!(CP_FIND, 0x05, "Find");
        add!(CP_FIND, 0x06, "SearchId");
        add!(CP_FIND, 0x07, "ExecuteSearch");
        add!(CP_FIND, 0x08, "MailBoxSearchCriterion");
        add!(CP_FIND, 0x09, "Query");
        add!(CP_FIND, 0x0A, "Status");
        add!(CP_FIND, 0x0B, "FreeText");
        add!(CP_FIND, 0x0C, "Options");
        add!(CP_FIND, 0x0D, "Range");
        add!(CP_FIND, 0x0E, "DeepTraversal");
        add!(CP_FIND, 0x11, "Response");
        add!(CP_FIND, 0x12, "Result");
        add!(CP_FIND, 0x13, "Properties");
        add!(CP_FIND, 0x14, "Preview");
        add!(CP_FIND, 0x15, "HasAttachments");

        m
    };

    /// Reverse mapping: tag name -> list of (page, token) where it appears.
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

    fn read_mb_u_int32(data: &[u8], pos: &mut usize) -> Result<usize, String> {
        let mut val: usize = 0;
        loop {
            if *pos >= data.len() {
                return Err("Unexpected end reading mb_u_int32".into());
            }
            let byte = data[*pos];
    /// Skip over a WBXML attribute section.  Per the WBXML spec the attribute
    /// list is a sequence of ATTRSTART / ATTRVALUE tokens terminated by an END
    /// (0x01) token.  Each token may carry inline data (strings, opaque, etc.)
    /// that must be consumed to keep `pos` in sync.
    fn skip_wbxml_attributes(data: &[u8], pos: &mut usize) -> Result<(), String> {
        const MAX_DEPTH: usize = 64;
        let mut depth: usize = 1;
        loop {
            if *pos >= data.len() {
                return Err("Unexpected end while skipping attributes".into());
            }
            let t = data[*pos];
            *pos += 1;
            match t {
                // END — attribute list finished
                0x01 => {
                    depth -= 1;
                    if depth == 0 {
                        return Ok(());
                    }
                }
                // SWITCH_PAGE — consume one page-number byte
                0x00 => {
                    if *pos >= data.len() {
                        return Err("Unexpected end after SWITCH_PAGE in attributes".into());
                    }
                    *pos += 1;
                }
                // STR_I — inline NUL-terminated string
                0x03 => {
                    while *pos < data.len() && data[*pos] != 0 {
                        *pos += 1;
                    }
                    if *pos >= data.len() {
                        return Err("Unexpected end in inline string in attributes".into());
                    }
                    *pos += 1; // skip NUL
                }
                // OPAQUE — length-prefixed binary blob
                0xC3 => {
                    let len = read_mb_u_int32(data, pos)?;
                    let end = pos
                        .checked_add(len)
                        .ok_or_else(|| "Opaque overflow in attributes".to_string())?;
                    if end > data.len() {
                        return Err("Opaque overflow in attributes".into());
                    }
                    *pos = end;
                }
                // LITERAL (0x04) is not used in ActiveSync, but if present we must consume its index.
                0x04 => {
                    let _ = read_mb_u_int32(data, pos)?;
                }
                // PI (processing instruction) — treat like a start tag, increase depth
                0x43 => {
                    depth += 1;
                    if depth > MAX_DEPTH {
                        return Err("Exceeded maximum nesting depth in WBXML attributes".into());
                    }
                }
                // Attribute start/value tokens can be skipped as single-byte markers.
                // Tokens with additional payload must be rejected or consumed explicitly.
                0x02 | 0x44 | 0x84 | 0xC4 | 0x40..=0x42 | 0x80..=0x83 | 0xC0..=0xC2 => {
                    return Err(format!(
                        "Unsupported WBXML attribute token used in ActiveSync: 0x{:02X}",
                        t
                    ));
                }
                _ => {}
            }
        }
    }
    }

    // Parse public ID as mb_u_int32 (per WBXML spec, section 5.4)
    let mut pos = 1;
    let publicid = read_mb_u_int32(data, &mut pos)?;

    // When publicid is 0 the actual identifier is stored as a string table index
    // encoded as an additional mb_u_int32 that must be consumed before charset.
    if publicid == 0 {
        let _ = read_mb_u_int32(data, &mut pos)?;
    }

    // Read charset as mb_u_int32 (per WBXML spec); for ActiveSync this is always
    // 0x6A (UTF-8) which fits in a single byte.
    let charset = read_mb_u_int32(data, &mut pos)?;
    if charset != 0x6A {
        return Err("Invalid WBXML header".into());
    }

    // Read string table length (mb_u_int32)
    let strtbl_len = read_mb_u_int32(data, &mut pos)?;

    // Skip the string table (must be empty per spec, but we just advance pos)
    // We do not need its contents because ActiveSync does not use string table references.
    let strtbl_end = pos
        .checked_add(strtbl_len)
        .ok_or_else(|| "String table end overflow".to_string())?;
    if strtbl_end > data.len() {
        return Err("String table exceeds data length".into());
    }
    pos = strtbl_end;

    let mut current_page = 0;
    let mut xml = String::new();
    let mut stack: Vec<String> = Vec::new();
    let mut pending_tag: Option<String> = None;

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
            } else {
                return Err("Unexpected END token".into());
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
            let text = String::from_utf8(str_buf)
                .map_err(|_| "Invalid UTF-8 in inline string".to_string())?;
            xml.push_str(
                &text
                    .replace('&', "&amp;")
                    .replace('<', "&lt;")
                    .replace('>', "&gt;"),
            );
            continue;
        }

        if token == TAG_OPAQUE {
            let len = read_mb_u_int32(data, &mut pos)?;
            let end = pos
                .checked_add(len)
                .ok_or_else(|| "Opaque overflow".to_string())?;
            if end > data.len() {
                return Err("Opaque overflow".into());
            }
            let content = &data[pos..end];
            pos = end;

            if let Some(tag) = pending_tag.take() {
                xml.push('>');
                stack.push(tag);
            }
            let encoded =
                base64::Engine::encode(&base64::engine::general_purpose::STANDARD, content);
            xml.push_str(&encoded);
            continue;
        }

        // Reject tokens that are not used by ActiveSync per spec (string table refs,
        // entities, extensions, processing instructions, LITERAL tokens).
        if matches!(token, 0x02 | 0x04 | 0x44 | 0x84 | 0xC4 | 0x40..=0x43 | 0x80..=0x83 | 0xC0..=0xC2) {
            return Err(format!("Unsupported WBXML token used in ActiveSync: 0x{:02X}", token));
        }

        let has_content = (token & 0x40) != 0;
        let has_attrs = (token & 0x80) != 0;
        let token_id = token & 0x3F;
        if has_attrs {
            skip_wbxml_attributes(data, &mut pos)?;
        }

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
            return Err(format!(
                "Unknown WBXML token: page {}, token 0x{:02X}",
                current_page, token_id
            ));
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
    // <strtbl_len: mb_u_int32> = 0 (string table not used)
    // <string table> (empty)
    // <WBXML body>
    let mut reader = quick_xml::Reader::from_str(xml);
    reader.config_mut().trim_text(false);
    let mut buf = Vec::new();

    // Build WBXML body.
    let mut body: Vec<u8> = Vec::new();
    let mut current_page: u8 = 0;

    // Tracks the effective XML namespace (WBXML code page) at each nesting level
    // so that unprefixed child tags inherit their parent's namespace scope.
    let mut scope_page_stack: Vec<u8> = Vec::new();

    loop {
        buf.clear();
        match reader.read_event_into(&mut buf) {
            Ok(quick_xml::events::Event::Start(ref e)) => {
                let full_name = String::from_utf8_lossy(e.name().as_ref()).to_string();
                let (prefix, local) = split_prefix(&full_name);
                // Determine the effective page: explicit prefix wins, otherwise
                // inherit the parent's scope page.
lazy_static! {
    /// Maps ActiveSync namespace strings (e.g., "AirSync") to WBXML code pages.
    static ref NAMESPACE_STRING_TO_PAGE: HashMap<&'static str, u8> = {
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
        m.insert("Find", CP_FIND);
        m
    };

    /// Maps (code page, token) to the corresponding tag name.
    static ref TAG_MAP: HashMap<(u8, u8), Tag> = {
        let mut m = HashMap::new();
        macro_rules! add {
            ($page:expr, $token:expr, $name:expr) => {
                m.insert(($page, $token), Tag { name: $name, _has_content: true });
            };
        }

        // AirSync (0) – MS-ASWBXML §2.1.2.1.1
        add!(CP_AIRSYNC, 0x05, "Sync");
        add!(CP_AIRSYNC, 0x06, "Responses");
        add!(CP_AIRSYNC, 0x07, "Add");
        add!(CP_AIRSYNC, 0x08, "Change");
        add!(CP_AIRSYNC, 0x09, "Delete");
        add!(CP_AIRSYNC, 0x0A, "Fetch");
        add!(CP_AIRSYNC, 0x0B, "SyncKey");
        add!(CP_AIRSYNC, 0x0C, "ClientId");
        add!(CP_AIRSYNC, 0x0D, "ServerId");
        add!(CP_AIRSYNC, 0x0E, "Status");
        add!(CP_AIRSYNC, 0x0F, "Collection");
        add!(CP_AIRSYNC, 0x10, "Class");
        add!(CP_AIRSYNC, 0x12, "CollectionId");
        add!(CP_AIRSYNC, 0x13, "GetChanges");
        add!(CP_AIRSYNC, 0x14, "MoreAvailable");
        add!(CP_AIRSYNC, 0x15, "WindowSize");
        add!(CP_AIRSYNC, 0x16, "Commands");
        add!(CP_AIRSYNC, 0x17, "Options");
        add!(CP_AIRSYNC, 0x18, "FilterType");
        add!(CP_AIRSYNC, 0x19, "Truncation");
        add!(CP_AIRSYNC, 0x1B, "Conflict");
        add!(CP_AIRSYNC, 0x1C, "Collections");
        add!(CP_AIRSYNC, 0x1D, "ApplicationData");
        add!(CP_AIRSYNC, 0x1E, "DeletesAsMoves");
        add!(CP_AIRSYNC, 0x20, "Supported");
        add!(CP_AIRSYNC, 0x21, "SoftDelete");
        add!(CP_AIRSYNC, 0x22, "MIMESupport");
        add!(CP_AIRSYNC, 0x23, "MIMETruncation");
        add!(CP_AIRSYNC, 0x24, "Wait");
        add!(CP_AIRSYNC, 0x25, "Limit");
        add!(CP_AIRSYNC, 0x26, "Partial");
        add!(CP_AIRSYNC, 0x27, "ConversationMode");
        add!(CP_AIRSYNC, 0x28, "MaxItems");
        add!(CP_AIRSYNC, 0x29, "HeartbeatInterval");

        // Contacts (1) – MS-ASWBXML §2.1.2.1.2
        add!(CP_CONTACTS, 0x05, "Anniversary");
        add!(CP_CONTACTS, 0x06, "AssistantName");
        add!(CP_CONTACTS, 0x07, "AssistantPhoneNumber");
        add!(CP_CONTACTS, 0x08, "Birthday");
        add!(CP_CONTACTS, 0x09, "Body");
        add!(CP_CONTACTS, 0x0A, "BodySize");
        add!(CP_CONTACTS, 0x0B, "BodyTruncated");
        add!(CP_CONTACTS, 0x0C, "Business2PhoneNumber");
        add!(CP_CONTACTS, 0x0D, "BusinessAddressCity");
        add!(CP_CONTACTS, 0x0E, "BusinessAddressCountry");
        add!(CP_CONTACTS, 0x0F, "BusinessAddressPostalCode");
        add!(CP_CONTACTS, 0x10, "BusinessAddressState");
        add!(CP_CONTACTS, 0x11, "BusinessAddressStreet");
        add!(CP_CONTACTS, 0x12, "BusinessFaxNumber");
        add!(CP_CONTACTS, 0x13, "BusinessPhoneNumber");
        add!(CP_CONTACTS, 0x14, "CarPhoneNumber");
        add!(CP_CONTACTS, 0x15, "Categories");
        add!(CP_CONTACTS, 0x16, "Category");
        add!(CP_CONTACTS, 0x17, "Children");
        add!(CP_CONTACTS, 0x18, "Child");
        add!(CP_CONTACTS, 0x19, "CompanyName");
        add!(CP_CONTACTS, 0x1A, "Department");
        add!(CP_CONTACTS, 0x1B, "Email1Address");
        add!(CP_CONTACTS, 0x1C, "Email2Address");
        add!(CP_CONTACTS, 0x1D, "Email3Address");
        add!(CP_CONTACTS, 0x1E, "FileAs");
        add!(CP_CONTACTS, 0x1F, "FirstName");
        add!(CP_CONTACTS, 0x20, "Home2PhoneNumber");
        add!(CP_CONTACTS, 0x21, "HomeAddressCity");
        add!(CP_CONTACTS, 0x22, "HomeAddressCountry");
        add!(CP_CONTACTS, 0x23, "HomeAddressPostalCode");
        add!(CP_CONTACTS, 0x24, "HomeAddressState");
        add!(CP_CONTACTS, 0x25, "HomeAddressStreet");
        add!(CP_CONTACTS, 0x26, "HomeFaxNumber");
        add!(CP_CONTACTS, 0x27, "HomePhoneNumber");
        add!(CP_CONTACTS, 0x28, "JobTitle");
        add!(CP_CONTACTS, 0x29, "LastName");
        add!(CP_CONTACTS, 0x2A, "MiddleName");
        add!(CP_CONTACTS, 0x2B, "MobilePhoneNumber");
        add!(CP_CONTACTS, 0x2C, "OfficeLocation");
        add!(CP_CONTACTS, 0x2F, "PagerNumber");
        add!(CP_CONTACTS, 0x31, "Spouse");
        add!(CP_CONTACTS, 0x32, "Suffix");
        add!(CP_CONTACTS, 0x33, "Title");
        add!(CP_CONTACTS, 0x34, "WebPage");
        add!(CP_CONTACTS, 0x35, "YomiCompanyName");
        add!(CP_CONTACTS, 0x36, "YomiFirstName");
        add!(CP_CONTACTS, 0x37, "YomiLastName");
        add!(CP_CONTACTS, 0x3C, "Picture");
        add!(CP_CONTACTS, 0x3D, "Alias");
        add!(CP_CONTACTS, 0x3E, "WeightedRank");

        // Email (2) – MS-ASWBXML §2.1.2.1.3
        add!(CP_EMAIL, 0x05, "Attachment");
        add!(CP_EMAIL, 0x06, "Attachments");
        add!(CP_EMAIL, 0x07, "AttName");
        add!(CP_EMAIL, 0x08, "AttSize");
        add!(CP_EMAIL, 0x09, "Att0Id");
        add!(CP_EMAIL, 0x0A, "AttMethod");
        // 0x0B unused
        add!(CP_EMAIL, 0x0C, "Body");
        add!(CP_EMAIL, 0x0D, "BodySize");
        add!(CP_EMAIL, 0x0E, "BodyTruncated");
        add!(CP_EMAIL, 0x0F, "DateReceived");
        add!(CP_EMAIL, 0x10, "DisplayName");
        add!(CP_EMAIL, 0x11, "DisplayTo");
        add!(CP_EMAIL, 0x12, "Importance");
        add!(CP_EMAIL, 0x13, "MessageClass");
        add!(CP_EMAIL, 0x14, "Subject");
        add!(CP_EMAIL, 0x15, "Read");
        add!(CP_EMAIL, 0x16, "To");
        add!(CP_EMAIL, 0x17, "Cc");
        add!(CP_EMAIL, 0x18, "From");
        add!(CP_EMAIL, 0x19, "ReplyTo");
        add!(CP_EMAIL, 0x1A, "AllDayEvent");
        add!(CP_EMAIL, 0x1B, "Categories");
        add!(CP_EMAIL, 0x1C, "Category");
        add!(CP_EMAIL, 0x1D, "DtStamp");
        add!(CP_EMAIL, 0x1E, "EndTime");
        add!(CP_EMAIL, 0x1F, "InstanceType");
        add!(CP_EMAIL, 0x20, "BusyStatus");
        add!(CP_EMAIL, 0x21, "Location");
        add!(CP_EMAIL, 0x22, "MeetingRequest");
        add!(CP_EMAIL, 0x23, "Organizer");
        add!(CP_EMAIL, 0x24, "RecurrenceId");
        add!(CP_EMAIL, 0x25, "Reminder");
        add!(CP_EMAIL, 0x26, "ResponseRequested");
        add!(CP_EMAIL, 0x27, "Recurrences");
        add!(CP_EMAIL, 0x28, "Recurrence");
        add!(CP_EMAIL, 0x29, "Type");
        add!(CP_EMAIL, 0x2A, "Until");
        add!(CP_EMAIL, 0x2B, "Occurrences");
        add!(CP_EMAIL, 0x2C, "Interval");
        add!(CP_EMAIL, 0x2D, "DayOfWeek");
        add!(CP_EMAIL, 0x2E, "DayOfMonth");
        add!(CP_EMAIL, 0x2F, "WeekOfMonth");
        add!(CP_EMAIL, 0x30, "MonthOfYear");
        add!(CP_EMAIL, 0x31, "StartTime");
        add!(CP_EMAIL, 0x32, "Sensitivity");
        add!(CP_EMAIL, 0x33, "TimeZone");
        add!(CP_EMAIL, 0x34, "GlobalObjId");
        add!(CP_EMAIL, 0x35, "ThreadTopic");
        add!(CP_EMAIL, 0x36, "MIMEData");
        add!(CP_EMAIL, 0x37, "MIMETruncated");
        add!(CP_EMAIL, 0x38, "MIMESize");
        add!(CP_EMAIL, 0x39, "InternetCPID");
        add!(CP_EMAIL, 0x3A, "Flag");
        add!(CP_EMAIL, 0x3B, "Status");
        add!(CP_EMAIL, 0x3C, "ContentClass");
        add!(CP_EMAIL, 0x3D, "FlagType");
        add!(CP_EMAIL, 0x3E, "CompleteTime");
        add!(CP_EMAIL, 0x3F, "DisallowNewTimeProposal");

        // Calendar (4) – MS-ASWBXML §2.1.2.1.5
        add!(CP_CALENDAR, 0x05, "TimeZone");
        add!(CP_CALENDAR, 0x06, "AllDayEvent");
        add!(CP_CALENDAR, 0x07, "Attendees");
        add!(CP_CALENDAR, 0x08, "Attendee");
        add!(CP_CALENDAR, 0x09, "Email");
        add!(CP_CALENDAR, 0x0A, "Name");
        add!(CP_CALENDAR, 0x0D, "BusyStatus");
        add!(CP_CALENDAR, 0x0E, "Categories");
        add!(CP_CALENDAR, 0x0F, "Category");
        add!(CP_CALENDAR, 0x11, "DtStamp");
        add!(CP_CALENDAR, 0x12, "EndTime");
        add!(CP_CALENDAR, 0x13, "Exception");
        add!(CP_CALENDAR, 0x14, "Exceptions");
        add!(CP_CALENDAR, 0x15, "Deleted");
        add!(CP_CALENDAR, 0x16, "ExceptionStartTime");
        add!(CP_CALENDAR, 0x17, "Location");
        add!(CP_CALENDAR, 0x18, "MeetingStatus");
        add!(CP_CALENDAR, 0x19, "OrganizerEmail");
        add!(CP_CALENDAR, 0x1A, "OrganizerName");
        add!(CP_CALENDAR, 0x1B, "Recurrence");
        add!(CP_CALENDAR, 0x1C, "Type");
        add!(CP_CALENDAR, 0x1D, "Until");
        add!(CP_CALENDAR, 0x1E, "Occurrences");
        add!(CP_CALENDAR, 0x1F, "Interval");
        add!(CP_CALENDAR, 0x20, "DayOfWeek");
        add!(CP_CALENDAR, 0x21, "DayOfMonth");
        add!(CP_CALENDAR, 0x22, "WeekOfMonth");
        add!(CP_CALENDAR, 0x23, "MonthOfYear");
        add!(CP_CALENDAR, 0x24, "Reminder");
        add!(CP_CALENDAR, 0x25, "Sensitivity");
        add!(CP_CALENDAR, 0x26, "Subject");
        add!(CP_CALENDAR, 0x27, "StartTime");
        add!(CP_CALENDAR, 0x28, "UID");
        add!(CP_CALENDAR, 0x29, "AttendeeStatus");
        add!(CP_CALENDAR, 0x2A, "AttendeeType");
        add!(CP_CALENDAR, 0x33, "DisallowNewTimeProposal");
        add!(CP_CALENDAR, 0x34, "ResponseRequested");
        add!(CP_CALENDAR, 0x35, "AppointmentReplyTime");
        add!(CP_CALENDAR, 0x36, "ResponseType");
        add!(CP_CALENDAR, 0x37, "CalendarType");
        add!(CP_CALENDAR, 0x38, "IsLeapMonth");
        add!(CP_CALENDAR, 0x39, "FirstDayOfWeek");
        add!(CP_CALENDAR, 0x3A, "OnlineMeetingConfLink");
        add!(CP_CALENDAR, 0x3B, "OnlineMeetingExternalLink");
        add!(CP_CALENDAR, 0x3C, "ClientUid");

        // Move (5) – MS-ASWBXML §2.1.2.1.6
        add!(CP_MOVE, 0x05, "MoveItems");
        add!(CP_MOVE, 0x06, "Move");
        add!(CP_MOVE, 0x07, "SrcMsgId");
        add!(CP_MOVE, 0x08, "SrcFldId");
        add!(CP_MOVE, 0x09, "DstFldId");
        add!(CP_MOVE, 0x0A, "Response");
        add!(CP_MOVE, 0x0B, "Status");
        add!(CP_MOVE, 0x0C, "DstMsgId");

        // GetItemEstimate (6) – MS-ASWBXML §2.1.2.1.7
        add!(CP_GETITEMESTIMATE, 0x05, "GetItemEstimate");
        add!(CP_GETITEMESTIMATE, 0x06, "Version");
        add!(CP_GETITEMESTIMATE, 0x07, "Collections");
        add!(CP_GETITEMESTIMATE, 0x08, "Collection");
        add!(CP_GETITEMESTIMATE, 0x09, "Class");
        add!(CP_GETITEMESTIMATE, 0x0A, "CollectionId");
        add!(CP_GETITEMESTIMATE, 0x0B, "DateTime");
        add!(CP_GETITEMESTIMATE, 0x0C, "Estimate");
        add!(CP_GETITEMESTIMATE, 0x0D, "Response");
        add!(CP_GETITEMESTIMATE, 0x0E, "Status");

        // FolderHierarchy (7) – MS-ASWBXML §2.1.2.1.8
        add!(CP_FOLDERHIERARCHY, 0x05, "Folders");
        add!(CP_FOLDERHIERARCHY, 0x06, "Folder");
        add!(CP_FOLDERHIERARCHY, 0x07, "DisplayName");
        add!(CP_FOLDERHIERARCHY, 0x08, "ServerId");
        add!(CP_FOLDERHIERARCHY, 0x09, "ParentId");
        add!(CP_FOLDERHIERARCHY, 0x0A, "Type");
        add!(CP_FOLDERHIERARCHY, 0x0C, "Status");
        add!(CP_FOLDERHIERARCHY, 0x0D, "ContentClass");
        add!(CP_FOLDERHIERARCHY, 0x0E, "Changes");
        add!(CP_FOLDERHIERARCHY, 0x0F, "Add");
        add!(CP_FOLDERHIERARCHY, 0x10, "Delete");
        add!(CP_FOLDERHIERARCHY, 0x11, "Update");
        add!(CP_FOLDERHIERARCHY, 0x12, "SyncKey");
        add!(CP_FOLDERHIERARCHY, 0x13, "FolderCreate");
        add!(CP_FOLDERHIERARCHY, 0x14, "FolderDelete");
        add!(CP_FOLDERHIERARCHY, 0x15, "FolderUpdate");
        add!(CP_FOLDERHIERARCHY, 0x16, "FolderSync");
        add!(CP_FOLDERHIERARCHY, 0x17, "Count");

        // MeetingResponse (8) – MS-ASWBXML §2.1.2.1.9
        add!(CP_MEETINGRESPONSE, 0x05, "CalendarId");
        add!(CP_MEETINGRESPONSE, 0x06, "CollectionId");
        add!(CP_MEETINGRESPONSE, 0x07, "MeetingResponse");
        add!(CP_MEETINGRESPONSE, 0x08, "RequestId");
        add!(CP_MEETINGRESPONSE, 0x09, "Request");
        add!(CP_MEETINGRESPONSE, 0x0A, "Result");
        add!(CP_MEETINGRESPONSE, 0x0B, "Status");
        add!(CP_MEETINGRESPONSE, 0x0C, "UserResponse");
        add!(CP_MEETINGRESPONSE, 0x0E, "InstanceId");
        add!(CP_MEETINGRESPONSE, 0x10, "ProposedStartTime");
        add!(CP_MEETINGRESPONSE, 0x11, "ProposedEndTime");
        add!(CP_MEETINGRESPONSE, 0x12, "SendResponse");

        // Tasks (9) – MS-ASWBXML §2.1.2.1.10
        add!(CP_TASKS, 0x08, "Categories");
        add!(CP_TASKS, 0x09, "Category");
        add!(CP_TASKS, 0x0A, "Complete");
        add!(CP_TASKS, 0x0B, "DateCompleted");
        add!(CP_TASKS, 0x0C, "DueDate");
        add!(CP_TASKS, 0x0D, "UtcDueDate");
        add!(CP_TASKS, 0x0E, "Importance");
        add!(CP_TASKS, 0x0F, "Recurrence");
        add!(CP_TASKS, 0x10, "Type");
        add!(CP_TASKS, 0x11, "Start");
        add!(CP_TASKS, 0x12, "Until");
        add!(CP_TASKS, 0x13, "Occurrences");
        add!(CP_TASKS, 0x14, "Interval");
        add!(CP_TASKS, 0x15, "DayOfMonth");
        add!(CP_TASKS, 0x16, "DayOfWeek");
        add!(CP_TASKS, 0x17, "WeekOfMonth");
        add!(CP_TASKS, 0x18, "MonthOfYear");
        add!(CP_TASKS, 0x19, "Regenerate");
        add!(CP_TASKS, 0x1A, "DeadOccur");
        add!(CP_TASKS, 0x1B, "ReminderSet");
        add!(CP_TASKS, 0x1C, "ReminderTime");
        add!(CP_TASKS, 0x1D, "Sensitivity");
        add!(CP_TASKS, 0x1E, "StartDate");
        add!(CP_TASKS, 0x1F, "UtcStartDate");
        add!(CP_TASKS, 0x20, "Subject");
        add!(CP_TASKS, 0x22, "OrdinalDate");
        add!(CP_TASKS, 0x23, "SubOrdinalDate");
        add!(CP_TASKS, 0x24, "CalendarType");
        add!(CP_TASKS, 0x25, "IsLeapMonth");
        add!(CP_TASKS, 0x26, "FirstDayOfWeek");

        // ResolveRecipients (10) – MS-ASWBXML §2.1.2.1.11
        add!(CP_RESOLVERECIPIENTS, 0x05, "ResolveRecipients");
        add!(CP_RESOLVERECIPIENTS, 0x06, "Response");
        add!(CP_RESOLVERECIPIENTS, 0x07, "Status");
        add!(CP_RESOLVERECIPIENTS, 0x08, "Type");
        add!(CP_RESOLVERECIPIENTS, 0x09, "Recipient");
        add!(CP_RESOLVERECIPIENTS, 0x0A, "DisplayName");
        add!(CP_RESOLVERECIPIENTS, 0x0B, "EmailAddress");
        add!(CP_RESOLVERECIPIENTS, 0x0C, "Certificates");
        add!(CP_RESOLVERECIPIENTS, 0x0D, "Certificate");
        add!(CP_RESOLVERECIPIENTS, 0x0E, "MiniCertificate");
        add!(CP_RESOLVERECIPIENTS, 0x0F, "Options");
        add!(CP_RESOLVERECIPIENTS, 0x10, "To");
        add!(CP_RESOLVERECIPIENTS, 0x11, "CertificateRetrieval");
        add!(CP_RESOLVERECIPIENTS, 0x12, "RecipientCount");
        add!(CP_RESOLVERECIPIENTS, 0x13, "MaxCertificates");
        add!(CP_RESOLVERECIPIENTS, 0x14, "MaxAmbiguousRecipients");
        add!(CP_RESOLVERECIPIENTS, 0x15, "CertificateCount");
        add!(CP_RESOLVERECIPIENTS, 0x16, "Availability");
        add!(CP_RESOLVERECIPIENTS, 0x17, "StartTime");
        add!(CP_RESOLVERECIPIENTS, 0x18, "EndTime");
        add!(CP_RESOLVERECIPIENTS, 0x19, "MergedFreeBusy");
        add!(CP_RESOLVERECIPIENTS, 0x1A, "Picture");
        add!(CP_RESOLVERECIPIENTS, 0x1B, "MaxSize");
        add!(CP_RESOLVERECIPIENTS, 0x1C, "Data");
        add!(CP_RESOLVERECIPIENTS, 0x1D, "MaxPictures");

        // ValidateCert (11) – MS-ASWBXML §2.1.2.1.12
        add!(CP_VALIDATECERT, 0x05, "ValidateCert");
        add!(CP_VALIDATECERT, 0x06, "Certificates");
        add!(CP_VALIDATECERT, 0x07, "Certificate");
        add!(CP_VALIDATECERT, 0x08, "CertificateChain");
        add!(CP_VALIDATECERT, 0x09, "CheckCRL");
        add!(CP_VALIDATECERT, 0x0A, "Status");

        // Contacts2 (12) – MS-ASWBXML §2.1.2.1.13
        add!(CP_CONTACTS2, 0x05, "CustomerId");
        add!(CP_CONTACTS2, 0x06, "GovernmentId");
        add!(CP_CONTACTS2, 0x07, "IMAddress");
        add!(CP_CONTACTS2, 0x08, "IMAddress2");
        add!(CP_CONTACTS2, 0x09, "IMAddress3");
        add!(CP_CONTACTS2, 0x0A, "ManagerName");
        add!(CP_CONTACTS2, 0x0B, "CompanyMainPhone");
        add!(CP_CONTACTS2, 0x0C, "AccountName");
        add!(CP_CONTACTS2, 0x0D, "NickName");
        add!(CP_CONTACTS2, 0x0E, "MMS");

        // Ping (13) – MS-ASWBXML §2.1.2.1.14
        add!(CP_PING, 0x05, "Ping");
        add!(CP_PING, 0x07, "Status");
        add!(CP_PING, 0x08, "HeartbeatInterval");
        add!(CP_PING, 0x09, "Folders");
        add!(CP_PING, 0x0A, "Folder");
        add!(CP_PING, 0x0B, "Id");
        add!(CP_PING, 0x0C, "Class");
        add!(CP_PING, 0x0D, "MaxFolders");

        // Provision (14) – MS-ASWBXML §2.1.2.1.15
        add!(CP_PROVISION, 0x05, "Provision");
        add!(CP_PROVISION, 0x06, "Policies");
        add!(CP_PROVISION, 0x07, "Policy");
        add!(CP_PROVISION, 0x08, "PolicyType");
        add!(CP_PROVISION, 0x09, "PolicyKey");
        add!(CP_PROVISION, 0x0A, "Data");
        add!(CP_PROVISION, 0x0B, "Status");
        add!(CP_PROVISION, 0x0C, "RemoteWipe");
        add!(CP_PROVISION, 0x0D, "EASProvisionDoc");
        add!(CP_PROVISION, 0x0E, "DevicePasswordEnabled");
        add!(CP_PROVISION, 0x0F, "AlphanumericDevicePasswordRequired");
        add!(CP_PROVISION, 0x10, "RequireStorageCardEncryption");
        add!(CP_PROVISION, 0x11, "PasswordRecoveryEnabled");
        add!(CP_PROVISION, 0x13, "AttachmentsEnabled");
        add!(CP_PROVISION, 0x14, "MinDevicePasswordLength");
        add!(CP_PROVISION, 0x15, "MaxInactivityTimeDeviceLock");
        add!(CP_PROVISION, 0x16, "MaxDevicePasswordFailedAttempts");
        add!(CP_PROVISION, 0x17, "MaxAttachmentSize");
        add!(CP_PROVISION, 0x18, "AllowSimpleDevicePassword");
        add!(CP_PROVISION, 0x19, "DevicePasswordExpiration");
        add!(CP_PROVISION, 0x1A, "DevicePasswordHistory");
        add!(CP_PROVISION, 0x1B, "AllowStorageCard");
        add!(CP_PROVISION, 0x1C, "AllowCamera");
        add!(CP_PROVISION, 0x1D, "RequireDeviceEncryption");
        add!(CP_PROVISION, 0x1E, "AllowUnsignedApplications");
        add!(CP_PROVISION, 0x1F, "AllowUnsignedInstallationPackages");
        add!(CP_PROVISION, 0x20, "MinDevicePasswordComplexCharacters");
        add!(CP_PROVISION, 0x21, "AllowWiFi");
        add!(CP_PROVISION, 0x22, "AllowTextMessaging");
        add!(CP_PROVISION, 0x23, "AllowPOPIMAPEmail");
        add!(CP_PROVISION, 0x24, "AllowBluetooth");
        add!(CP_PROVISION, 0x25, "AllowIrDA");
        add!(CP_PROVISION, 0x26, "RequireManualSyncWhenRoaming");
        add!(CP_PROVISION, 0x27, "AllowDesktopSync");
        add!(CP_PROVISION, 0x28, "MaxCalendarAgeFilter");
        add!(CP_PROVISION, 0x29, "AllowHTMLEmail");
        add!(CP_PROVISION, 0x2A, "MaxEmailAgeFilter");
        add!(CP_PROVISION, 0x2B, "MaxEmailBodyTruncationSize");
        add!(CP_PROVISION, 0x2C, "MaxEmailHTMLBodyTruncationSize");
        add!(CP_PROVISION, 0x2D, "RequireSignedSMIMEMessages");
        add!(CP_PROVISION, 0x2E, "RequireEncryptedSMIMEMessages");
        add!(CP_PROVISION, 0x2F, "RequireSignedSMIMEAlgorithm");
        add!(CP_PROVISION, 0x30, "RequireEncryptionSMIMEAlgorithm");
        add!(CP_PROVISION, 0x31, "AllowSMIMEEncryptionAlgorithmNegotiation");
        add!(CP_PROVISION, 0x32, "AllowSMIMESoftCerts");
        add!(CP_PROVISION, 0x33, "AllowBrowser");
        add!(CP_PROVISION, 0x34, "AllowConsumerEmail");
        add!(CP_PROVISION, 0x35, "AllowRemoteDesktop");
        add!(CP_PROVISION, 0x36, "AllowInternetSharing");
        add!(CP_PROVISION, 0x37, "UnapprovedInROMApplicationList");
        add!(CP_PROVISION, 0x38, "ApplicationName");
        add!(CP_PROVISION, 0x39, "ApprovedApplicationList");
        add!(CP_PROVISION, 0x3A, "Hash");

        // Search (15) – MS-ASWBXML §2.1.2.1.16
        add!(CP_SEARCH, 0x05, "Search");
        add!(CP_SEARCH, 0x07, "Store");
        add!(CP_SEARCH, 0x08, "Name");
        add!(CP_SEARCH, 0x09, "Query");
        add!(CP_SEARCH, 0x0A, "Options");
        add!(CP_SEARCH, 0x0B, "Range");
        add!(CP_SEARCH, 0x0C, "Status");
        add!(CP_SEARCH, 0x0D, "Response");
        add!(CP_SEARCH, 0x0E, "Result");
        add!(CP_SEARCH, 0x0F, "Properties");
        add!(CP_SEARCH, 0x10, "Total");
        add!(CP_SEARCH, 0x11, "EqualTo");
        add!(CP_SEARCH, 0x12, "Value");
        add!(CP_SEARCH, 0x13, "And");
        add!(CP_SEARCH, 0x14, "Or");
        add!(CP_SEARCH, 0x15, "FreeText");
        add!(CP_SEARCH, 0x17, "DeepTraversal");
        add!(CP_SEARCH, 0x18, "LongId");
        add!(CP_SEARCH, 0x19, "RebuildResults");
        add!(CP_SEARCH, 0x1A, "LessThan");
        add!(CP_SEARCH, 0x1B, "GreaterThan");
        add!(CP_SEARCH, 0x1E, "UserName");
        add!(CP_SEARCH, 0x1F, "Password");
        add!(CP_SEARCH, 0x20, "ConversationId");
        add!(CP_SEARCH, 0x21, "Picture");
        add!(CP_SEARCH, 0x22, "MaxSize");
        add!(CP_SEARCH, 0x23, "MaxPictures");

        // GAL (16) – MS-ASWBXML §2.1.2.1.17
        add!(CP_GAL, 0x05, "DisplayName");
        add!(CP_GAL, 0x06, "Phone");
        add!(CP_GAL, 0x07, "Office");
        add!(CP_GAL, 0x08, "Title");
        add!(CP_GAL, 0x09, "Company");
        add!(CP_GAL, 0x0A, "Alias");
        add!(CP_GAL, 0x0B, "FirstName");
        add!(CP_GAL, 0x0C, "LastName");
        add!(CP_GAL, 0x0D, "HomePhone");
        add!(CP_GAL, 0x0E, "MobilePhone");
        add!(CP_GAL, 0x0F, "EmailAddress");
        add!(CP_GAL, 0x10, "Picture");
        add!(CP_GAL, 0x11, "Status");
        add!(CP_GAL, 0x12, "Data");

        // AirSyncBase (17) – MS-ASWBXML §2.1.2.1.18
        add!(CP_AIRSYNCBASE, 0x05, "BodyPreference");
        add!(CP_AIRSYNCBASE, 0x06, "Type");
        add!(CP_AIRSYNCBASE, 0x07, "TruncationSize");
        add!(CP_AIRSYNCBASE, 0x08, "AllOrNone");
        add!(CP_AIRSYNCBASE, 0x0A, "Body");
        add!(CP_AIRSYNCBASE, 0x0B, "Data");
        add!(CP_AIRSYNCBASE, 0x0C, "EstimatedDataSize");
        add!(CP_AIRSYNCBASE, 0x0D, "Truncated");
        add!(CP_AIRSYNCBASE, 0x0E, "Attachments");
        add!(CP_AIRSYNCBASE, 0x0F, "Attachment");
        add!(CP_AIRSYNCBASE, 0x10, "DisplayName");
        add!(CP_AIRSYNCBASE, 0x11, "FileReference");
        add!(CP_AIRSYNCBASE, 0x12, "Method");
        add!(CP_AIRSYNCBASE, 0x13, "ContentId");
        add!(CP_AIRSYNCBASE, 0x14, "ContentLocation");
        add!(CP_AIRSYNCBASE, 0x15, "IsInline");
        add!(CP_AIRSYNCBASE, 0x16, "NativeBodyType");
        add!(CP_AIRSYNCBASE, 0x17, "ContentType");
        add!(CP_AIRSYNCBASE, 0x18, "Preview");
        add!(CP_AIRSYNCBASE, 0x19, "BodyPartPreference");
        add!(CP_AIRSYNCBASE, 0x1A, "BodyPart");
        add!(CP_AIRSYNCBASE, 0x1B, "Status");
        add!(CP_AIRSYNCBASE, 0x1C, "Add");
        add!(CP_AIRSYNCBASE, 0x1D, "Delete");
        add!(CP_AIRSYNCBASE, 0x1E, "ClientId");
        add!(CP_AIRSYNCBASE, 0x1F, "Content");
        add!(CP_AIRSYNCBASE, 0x20, "Location");
        add!(CP_AIRSYNCBASE, 0x21, "Annotation");
        add!(CP_AIRSYNCBASE, 0x22, "Street");
        add!(CP_AIRSYNCBASE, 0x23, "City");
        add!(CP_AIRSYNCBASE, 0x24, "State");
        add!(CP_AIRSYNCBASE, 0x25, "Country");
        add!(CP_AIRSYNCBASE, 0x26, "PostalCode");
        add!(CP_AIRSYNCBASE, 0x27, "Latitude");
        add!(CP_AIRSYNCBASE, 0x28, "Longitude");
        add!(CP_AIRSYNCBASE, 0x29, "Accuracy");
        add!(CP_AIRSYNCBASE, 0x2A, "Altitude");
        add!(CP_AIRSYNCBASE, 0x2B, "AltitudeAccuracy");

        // Settings (18) – MS-ASWBXML §2.1.2.1.19
        add!(CP_SETTINGS, 0x05, "Settings");
        add!(CP_SETTINGS, 0x06, "Status");
        add!(CP_SETTINGS, 0x07, "Get");
        add!(CP_SETTINGS, 0x08, "Set");
        add!(CP_SETTINGS, 0x09, "Oof");
        add!(CP_SETTINGS, 0x0A, "OofState");
        add!(CP_SETTINGS, 0x0B, "StartTime");
        add!(CP_SETTINGS, 0x0C, "EndTime");
        add!(CP_SETTINGS, 0x0D, "OofMessage");
        add!(CP_SETTINGS, 0x0E, "AppliesToInternal");
        add!(CP_SETTINGS, 0x0F, "AppliesToExternalKnown");
        add!(CP_SETTINGS, 0x10, "AppliesToExternalUnknown");
        add!(CP_SETTINGS, 0x11, "Enabled");
        add!(CP_SETTINGS, 0x12, "ReplyMessage");
        add!(CP_SETTINGS, 0x13, "BodyType");
        add!(CP_SETTINGS, 0x14, "Password");
        add!(CP_SETTINGS, 0x15, "DevicePassword");
        add!(CP_SETTINGS, 0x16, "DeviceInformation");
        add!(CP_SETTINGS, 0x17, "Model");
        add!(CP_SETTINGS, 0x18, "IMEI");
        add!(CP_SETTINGS, 0x19, "FriendlyName");
        add!(CP_SETTINGS, 0x1A, "OS");
        add!(CP_SETTINGS, 0x1B, "OSLanguage");
        add!(CP_SETTINGS, 0x1C, "PhoneNumber");
        add!(CP_SETTINGS, 0x1D, "UserInformation");
        add!(CP_SETTINGS, 0x1E, "EmailAddresses");
        add!(CP_SETTINGS, 0x1F, "SmtpAddress");
        add!(CP_SETTINGS, 0x20, "UserAgent");
        add!(CP_SETTINGS, 0x21, "EnableOutboundSMS");
        add!(CP_SETTINGS, 0x22, "MobileOperator");
        add!(CP_SETTINGS, 0x23, "PrimarySmtpAddress");
        add!(CP_SETTINGS, 0x24, "Accounts");
        add!(CP_SETTINGS, 0x25, "Account");
        add!(CP_SETTINGS, 0x26, "AccountId");
        add!(CP_SETTINGS, 0x27, "AccountName");
        add!(CP_SETTINGS, 0x28, "UserDisplayName");
        add!(CP_SETTINGS, 0x29, "SendDisabled");
        add!(CP_SETTINGS, 0x2B, "RightsManagementInformation");

        // DocumentLibrary (19) – MS-ASWBXML §2.1.2.1.20
        add!(CP_DOCUMENTLIBRARY, 0x05, "LinkId");
        add!(CP_DOCUMENTLIBRARY, 0x06, "DisplayName");
        add!(CP_DOCUMENTLIBRARY, 0x07, "IsFolder");
        add!(CP_DOCUMENTLIBRARY, 0x09, "CreationDate");
        add!(CP_DOCUMENTLIBRARY, 0x0A, "LastModifiedDate");
        add!(CP_DOCUMENTLIBRARY, 0x0B, "IsHidden");
        add!(CP_DOCUMENTLIBRARY, 0x0C, "ContentLength");
        add!(CP_DOCUMENTLIBRARY, 0x0D, "ContentType");

        // ItemOperations (20) – MS-ASWBXML §2.1.2.1.21
        add!(CP_ITEMOPERATIONS, 0x05, "ItemOperations");
        add!(CP_ITEMOPERATIONS, 0x06, "Fetch");
        add!(CP_ITEMOPERATIONS, 0x07, "Store");
        add!(CP_ITEMOPERATIONS, 0x08, "Options");
        add!(CP_ITEMOPERATIONS, 0x09, "Range");
        add!(CP_ITEMOPERATIONS, 0x0A, "Total");
        add!(CP_ITEMOPERATIONS, 0x0B, "Properties");
        add!(CP_ITEMOPERATIONS, 0x0C, "Data");
        add!(CP_ITEMOPERATIONS, 0x0D, "Status");
        add!(CP_ITEMOPERATIONS, 0x0E, "Response");
        add!(CP_ITEMOPERATIONS, 0x0F, "Version");
        add!(CP_ITEMOPERATIONS, 0x10, "Schema");
        add!(CP_ITEMOPERATIONS, 0x11, "Part");
        add!(CP_ITEMOPERATIONS, 0x12, "EmptyFolderContents");
        add!(CP_ITEMOPERATIONS, 0x13, "DeleteSubFolders");
        add!(CP_ITEMOPERATIONS, 0x14, "UserName");
        add!(CP_ITEMOPERATIONS, 0x15, "Password");
        add!(CP_ITEMOPERATIONS, 0x16, "Move");
        add!(CP_ITEMOPERATIONS, 0x17, "DstFldId");
        add!(CP_ITEMOPERATIONS, 0x18, "ConversationId");
        add!(CP_ITEMOPERATIONS, 0x19, "MoveAlways");

        // ComposeMail (21) – MS-ASWBXML §2.1.2.1.22
        add!(CP_COMPOSEMAIL, 0x05, "SendMail");
        add!(CP_COMPOSEMAIL, 0x06, "SmartForward");
        add!(CP_COMPOSEMAIL, 0x07, "SmartReply");
        add!(CP_COMPOSEMAIL, 0x08, "SaveInSentItems");
        add!(CP_COMPOSEMAIL, 0x09, "ReplaceMime");
        add!(CP_COMPOSEMAIL, 0x0B, "Source");
        add!(CP_COMPOSEMAIL, 0x0C, "FolderId");
        add!(CP_COMPOSEMAIL, 0x0D, "ItemId");
        add!(CP_COMPOSEMAIL, 0x0E, "LongId");
        add!(CP_COMPOSEMAIL, 0x0F, "InstanceId");
        add!(CP_COMPOSEMAIL, 0x10, "Mime");
        add!(CP_COMPOSEMAIL, 0x11, "ClientId");
        add!(CP_COMPOSEMAIL, 0x12, "Status");
        add!(CP_COMPOSEMAIL, 0x13, "AccountId");
        add!(CP_COMPOSEMAIL, 0x15, "Forwardees");
        add!(CP_COMPOSEMAIL, 0x16, "Forwardee");
        add!(CP_COMPOSEMAIL, 0x17, "Name");
        add!(CP_COMPOSEMAIL, 0x18, "Email");

        // Email2 (22) – MS-ASWBXML §2.1.2.1.23
        add!(CP_EMAIL2, 0x05, "UmCallerID");
        add!(CP_EMAIL2, 0x06, "UmUserNotes");
        add!(CP_EMAIL2, 0x07, "UmAttDuration");
        add!(CP_EMAIL2, 0x08, "UmAttOrder");
        add!(CP_EMAIL2, 0x09, "ConversationId");
        add!(CP_EMAIL2, 0x0A, "ConversationIndex");
        add!(CP_EMAIL2, 0x0B, "LastVerbExecuted");
        add!(CP_EMAIL2, 0x0C, "LastVerbExecutionTime");
        add!(CP_EMAIL2, 0x0D, "ReceivedAsBcc");
        add!(CP_EMAIL2, 0x0E, "Sender");
        add!(CP_EMAIL2, 0x0F, "CalendarType");
        add!(CP_EMAIL2, 0x10, "IsLeapMonth");
        add!(CP_EMAIL2, 0x11, "AccountId");
        add!(CP_EMAIL2, 0x12, "FirstDayOfWeek");
        add!(CP_EMAIL2, 0x13, "MeetingMessageType");
        add!(CP_EMAIL2, 0x15, "IsDraft");
        add!(CP_EMAIL2, 0x16, "Bcc");
        add!(CP_EMAIL2, 0x17, "Send");

        // Notes (23) – MS-ASWBXML §2.1.2.1.24
        add!(CP_NOTES, 0x05, "Subject");
        add!(CP_NOTES, 0x06, "MessageClass");
        add!(CP_NOTES, 0x07, "LastModifiedDate");
        add!(CP_NOTES, 0x08, "Categories");
        add!(CP_NOTES, 0x09, "Category");

        // RightsManagement (24) – MS-ASWBXML §2.1.2.1.25
        add!(CP_RIGHTSMANAGEMENT, 0x05, "RightsManagementSupport");
        add!(CP_RIGHTSMANAGEMENT, 0x06, "RightsManagementTemplates");
        add!(CP_RIGHTSMANAGEMENT, 0x07, "RightsManagementTemplate");
        add!(CP_RIGHTSMANAGEMENT, 0x08, "RightsManagementLicense");
        add!(CP_RIGHTSMANAGEMENT, 0x09, "EditAllowed");
        add!(CP_RIGHTSMANAGEMENT, 0x0A, "ReplyAllowed");
        add!(CP_RIGHTSMANAGEMENT, 0x0B, "ReplyAllAllowed");
        add!(CP_RIGHTSMANAGEMENT, 0x0C, "ForwardAllowed");
        add!(CP_RIGHTSMANAGEMENT, 0x0D, "ModifyRecipientsAllowed");
        add!(CP_RIGHTSMANAGEMENT, 0x0E, "ExtractAllowed");
        add!(CP_RIGHTSMANAGEMENT, 0x0F, "PrintAllowed");
        add!(CP_RIGHTSMANAGEMENT, 0x10, "ExportAllowed");
        add!(CP_RIGHTSMANAGEMENT, 0x11, "ProgrammaticAccessAllowed");
        add!(CP_RIGHTSMANAGEMENT, 0x12, "Owner");
        add!(CP_RIGHTSMANAGEMENT, 0x13, "ContentExpiryDate");
        add!(CP_RIGHTSMANAGEMENT, 0x14, "TemplateID");
        add!(CP_RIGHTSMANAGEMENT, 0x15, "TemplateName");
        add!(CP_RIGHTSMANAGEMENT, 0x16, "TemplateDescription");
        add!(CP_RIGHTSMANAGEMENT, 0x17, "ContentOwner");
        add!(CP_RIGHTSMANAGEMENT, 0x18, "RemoveRightsManagementProtection");

        // Find (25) – MS-ASWBXML §2.1.2.1.26
        add!(CP_FIND, 0x05, "Find");
        add!(CP_FIND, 0x06, "SearchId");
        add!(CP_FIND, 0x07, "ExecuteSearch");
        add!(CP_FIND, 0x08, "MailBoxSearchCriterion");
        add!(CP_FIND, 0x09, "Query");
        add!(CP_FIND, 0x0A, "Status");
        add!(CP_FIND, 0x0B, "FreeText");
        add!(CP_FIND, 0x0C, "Options");
        add!(CP_FIND, 0x0D, "Range");
        add!(CP_FIND, 0x0E, "DeepTraversal");
        add!(CP_FIND, 0x11, "Response");
        add!(CP_FIND, 0x12, "Result");
        add!(CP_FIND, 0x13, "Properties");
        add!(CP_FIND, 0x14, "Preview");
        add!(CP_FIND, 0x15, "HasAttachments");

        m
    };

    /// Reverse mapping: tag name -> list of (page, token) where it appears.
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

    fn read_mb_u_int32(data: &[u8], pos: &mut usize) -> Result<usize, String> {
        let mut val: usize = 0;
        loop {
            if *pos >= data.len() {
                return Err("Unexpected end reading mb_u_int32".into());
            }
            let byte = data[*pos];
            *pos += 1;
            val = val
                .checked_mul(1 << 7)
                .and_then(|v| v.checked_add((byte & 0x7F) as usize))
                .ok_or_else(|| "mb_u_int32 overflow".to_string())?;
            if (byte & 0x80) == 0 {
                break;
            }
        }
        Ok(val)
    }

    /// Skip over a WBXML attribute section.  Per the WBXML spec the attribute
    /// list is a sequence of ATTRSTART / ATTRVALUE tokens terminated by an END
    /// (0x01) token.  Each token may carry inline data (strings, opaque, etc.)
    /// that must be consumed to keep `pos` in sync.
    fn skip_wbxml_attributes(data: &[u8], pos: &mut usize) -> Result<(), String> {
        const MAX_DEPTH: usize = 64;
        let mut depth: usize = 1;
        loop {
            if *pos >= data.len() {
                return Err("Unexpected end while skipping attributes".into());
            }
            let t = data[*pos];
            *pos += 1;
            match t {
                // END — attribute list finished
                0x01 => {
                    depth -= 1;
                    if depth == 0 {
                        return Ok(());
                    }
                }
                // SWITCH_PAGE — consume one page-number byte
                0x00 => {
                    if *pos >= data.len() {
                        return Err("Unexpected end after SWITCH_PAGE in attributes".into());
                    }
                    *pos += 1;
                }
                // STR_I — inline NUL-terminated string
                0x03 => {
                    while *pos < data.len() && data[*pos] != 0 {
                        *pos += 1;
                    }
                    if *pos >= data.len() {
                        return Err("Unexpected end in inline string in attributes".into());
                    }
                    *pos += 1; // skip NUL
                }
                // OPAQUE — length-prefixed binary blob
                0xC3 => {
                    let len = read_mb_u_int32(data, pos)?;
                    let end = pos
                        .checked_add(len)
                        .ok_or_else(|| "Opaque overflow in attributes".to_string())?;
                    if end > data.len() {
                        return Err("Opaque overflow in attributes".into());
                    }
                    *pos = end;
                }
                // LITERAL (0x04) is not used in ActiveSync, but if present we must consume its index.
                0x04 => {
                    let _ = read_mb_u_int32(data, pos)?;
                }
                // PI (processing instruction) — treat like a start tag, increase depth
                0x43 => {
                    depth += 1;
                    if depth > MAX_DEPTH {
                        return Err("Exceeded maximum nesting depth in WBXML attributes".into());
                    }
                }
                // All remaining tokens are either unsupported (EXT, ENTITY) or
                // are attribute start/value tokens that we just skip without extra data.
                // According to spec, attributes are not used, so we just ignore them.
                _ => {}
            }
        }
    }

    // Parse public ID as mb_u_int32 (per WBXML spec, section 5.4)
    let mut pos = 1;
    let publicid = read_mb_u_int32(data, &mut pos)?;

    // When publicid is 0 the actual identifier is stored as a string table index
    // encoded as an additional mb_u_int32 that must be consumed before charset.
    if publicid == 0 {
        let _ = read_mb_u_int32(data, &mut pos)?;
    }

    // Read charset as mb_u_int32 (per WBXML spec); for ActiveSync this is always
    // 0x6A (UTF-8) which fits in a single byte.
    let charset = read_mb_u_int32(data, &mut pos)?;
    if charset != 0x6A {
        return Err("Invalid WBXML header".into());
    }

    // Read string table length (mb_u_int32)
    let strtbl_len = read_mb_u_int32(data, &mut pos)?;

    // Skip the string table (must be empty per spec, but we just advance pos)
    // We do not need its contents because ActiveSync does not use string table references.
    let strtbl_end = pos
        .checked_add(strtbl_len)
        .ok_or_else(|| "String table end overflow".to_string())?;
    if strtbl_end > data.len() {
        return Err("String table exceeds data length".into());
    }
    pos = strtbl_end;

    let mut current_page = 0;
    let mut xml = String::new();
    let mut stack: Vec<String> = Vec::new();
    let mut pending_tag: Option<String> = None;

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
            let text = String::from_utf8(str_buf)
                .map_err(|_| "Invalid UTF-8 in inline string".to_string())?;
            xml.push_str(
                &text
                    .replace('&', "&amp;")
                    .replace('<', "&lt;")
                    .replace('>', "&gt;"),
            );
            continue;
        }

        if token == TAG_OPAQUE {
            let len = read_mb_u_int32(data, &mut pos)?;
            let end = pos
                .checked_add(len)
                .ok_or_else(|| "Opaque overflow".to_string())?;
            if end > data.len() {
                return Err("Opaque overflow".into());
            }
            let content = &data[pos..end];
            pos = end;

            if let Some(tag) = pending_tag.take() {
                xml.push('>');
                stack.push(tag);
            }
            let encoded =
                base64::Engine::encode(&base64::engine::general_purpose::STANDARD, content);
            xml.push_str(&encoded);
            continue;
        }

        // Reject tokens that are not used by ActiveSync per spec (string table refs,
        // entities, extensions, processing instructions, LITERAL tokens).
        if matches!(token, 0x02 | 0x04 | 0x44 | 0x84 | 0xC4 | 0x40..=0x43 | 0x80..=0x83 | 0xC0..=0xC2) {
            return Err(format!("Unsupported WBXML token used in ActiveSync: 0x{:02X}", token));
        }

        let has_content = (token & 0x40) != 0;
        let has_attrs = (token & 0x80) != 0;
        let token_id = token & 0x3F;
        if has_attrs {
            skip_wbxml_attributes(data, &mut pos)?;
        }

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
            return Err(format!(
                "Unknown WBXML token: page {}, token 0x{:02X}",
                current_page, token_id
            ));
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
    // <strtbl_len: mb_u_int32> = 0 (string table not used)
    // <string table> (empty)
    // <WBXML body>
    let mut reader = quick_xml::Reader::from_str(xml);
    reader.config_mut().trim_text(false);
    reader.config_mut().expand_empty_elements(true); // Treat empty elements as start/end for consistent stack management
    reader.config_mut().trim_text(true); // Trim text nodes

    let mut buf = Vec::new();

    // Build WBXML body.
    let mut body: Vec<u8> = Vec::new();
    let mut current_page: u8 = 0;
    let mut page_stack: Vec<u8> = Vec::new(); // Stack of current code pages (for default namespace or inherited)

    loop {
        buf.clear();
        match reader.read_namespaced_event(&mut buf) {
            Ok((namespace_uri_opt, quick_xml::events::Event::Start(ref e))) => {
                let local_name = String::from_utf8_lossy(e.local_name().as_ref()).to_string();
                let mut resolved_page_from_ns: Option<u8> = None;

                if let Some(uri_bytes) = namespace_uri_opt {
                    if let Ok(uri_str) = std::str::from_utf8(uri_bytes) {
                        resolved_page_from_ns = NAMESPACE_STRING_TO_PAGE.get(uri_str).copied();
                    }
                }

                let effective_page = resolved_page_from_ns.or_else(|| page_stack.last().copied());

                if let Some(page) = effective_page {
                    if !encode_tag(&mut body, &local_name, &mut current_page, true, Some(page)) {
                        return Err(format!("Unknown tag '{}' or ambiguous namespace for page {}", local_name, page));
                    }
                    page_stack.push(page);
                } else {
                    // No namespace URI, no parent default, try to find in any page
                    if !encode_tag(&mut body, &local_name, &mut current_page, true, None) {
                        return Err(format!("Unknown tag '{}' without explicit or inherited namespace", local_name));
                    }
                    page_stack.push(current_page); // Push the current_page as the effective page
                }
            }
            Ok((namespace_uri_opt, quick_xml::events::Event::Empty(ref e))) => {
                let local_name = String::from_utf8_lossy(e.local_name().as_ref()).to_string();
                let mut resolved_page_from_ns: Option<u8> = None;

                if let Some(uri_bytes) = namespace_uri_opt {
                    if let Ok(uri_str) = std::str::from_utf8(uri_bytes) {
                        resolved_page_from_ns = NAMESPACE_STRING_TO_PAGE.get(uri_str).copied();
                    }
                }

                let effective_page = resolved_page_from_ns.or_else(|| page_stack.last().copied());

                if let Some(page) = effective_page {
                    if !encode_tag(&mut body, &local_name, &mut current_page, false, Some(page)) {
                        return Err(format!("Unknown tag '{}' or ambiguous namespace for page {}", local_name, page));
                    }
                } else {
                    // No namespace URI, no parent default, try to find in any page
                    if !encode_tag(&mut body, &local_name, &mut current_page, false, None) {
                        return Err(format!("Unknown tag '{}' without explicit or inherited namespace", local_name));
                    }
                }
                // Empty tags do not push to the stack
            }
            Ok((_, quick_xml::events::Event::End(_))) => {
                body.push(TAG_END);
                page_stack.pop(); // Pop the scope for this element
            }
            Ok((_, quick_xml::events::Event::Text(ref e))) => {
                let text_str = std::str::from_utf8(e.as_ref())
                    .map_err(|_| "Invalid UTF-8 in XML text node".to_string())?;
                let t = quick_xml::escape::unescape(text_str)
                    .map_err(|e| format!("XML text unescape error: {}", e))?;
                if !t.trim().is_empty() {
                    body.push(TAG_STR_I);
                    body.extend(t.as_bytes());
                    body.push(0x00);
                }
            }
            Ok((_, quick_xml::events::Event::CData(ref e))) => {
                let raw = e.as_ref();
                if raw.contains(&0u8) {
                    return Err("CData content contains NUL byte, cannot encode as WBXML inline string".to_string());
                }
                body.push(TAG_STR_I);
                body.extend_from_slice(raw);
                body.push(0x00);
            }
            Ok(quick_xml::events::Event::Eof) => break,
            Ok(_) => {} // Ignore other event types
            Err(e) => return Err(format!("XML parsing error: {}", e)),
        }
    }

    let mut output = vec![0x03, 0x01, 0x6A];
    write_mb_u_int32(&mut output, 0); // string table length = 0
    output.extend_from_slice(&body);
    Ok(output)
}

/// Write a multi-byte integer in WBXML format (7-bit groups, big-endian).
fn write_mb_u_int32(out: &mut Vec<u8>, mut v: usize) {
    if v == 0 {
        out.push(0);
        return;
    }
    let mut bytes = Vec::new();
    while v > 0 {
        bytes.push((v & 0x7F) as u8);
        v >>= 7;
    }
    for (i, b) in bytes.iter().enumerate().rev() {
        let mut byte = *b;
        if i != 0 {
            byte |= 0x80;
        }
        out.push(byte);
    }
}

/// Encode a single tag. Returns `true` on success, `false` if the tag is unknown
/// or the prefix cannot be resolved to a unique page.
fn encode_tag(
    output: &mut Vec<u8>,
    name: &str,
    current_page: &mut u8,
    has_content: bool,
    target_page: Option<u8>,
) -> bool {
    let entries = match NAME_MAP.get(name) {
        Some(e) => e,
        None => return false,
    };

    let (page, token) = if let Some(explicit_page) = target_page {
        match entries.iter().find(|(p, _)| *p == explicit_page) {
            Some(entry) => entry,
            None => return false,
        }
    } else {
        if let Some(entry) = entries.iter().find(|(p, _)| *p == *current_page) {
            entry
        } else if entries.len() == 1 {
            &entries[0]
        } else {
            return false;
        }
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
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Test round-trip encoding/decoding of a simple known message.
    #[test]
    fn round_trip_basic_sync() {
        let xml = r#"<Sync xmlns="AirSync"><Collections><Collection><SyncKey>1</SyncKey></Collection></Collections></Sync>"#;
        let encoded = encode(xml).expect("encode failed");
        let decoded = decode(&encoded).expect("decode failed");
        // After round-trip, namespaces are stripped (prefixes become default ns)
        assert!(decoded.contains("<Sync>"));
        assert!(decoded.contains("<Collections>"));
        assert!(decoded.contains("<Collection>"));
        assert!(decoded.contains("<SyncKey>1</SyncKey>"));
    }

    /// Test that a tag with a known prefix is encoded with correct page switch.
    #[test]
    fn known_prefix_encoding() {
        let xml = "<Calendar:Recurrence><AirSyncBase:Type>2</AirSyncBase:Type></Calendar:Recurrence>";
        let encoded = encode(xml).expect("encode should succeed");
        let decoded = decode(&encoded).expect("round-trip decode should succeed");
        assert!(decoded.contains("<Recurrence>"));
        assert!(decoded.contains("<Type>"));
    }

    /// Test that an unknown tag results in an error.
    #[test]
    fn unknown_tag_returns_error() {
        let xml = "<Unknown:Tag>test</Unknown:Tag>";
        assert!(encode(xml).is_err());
    }

    /// Test handling of CData.
    #[test]
    fn cdata_encoding() {
        let xml = "<Sync><![CDATA[<hello>]]></Sync>";
        let encoded = encode(xml).expect("encode failed");
        let decoded = decode(&encoded).expect("decode failed");
        assert!(decoded.contains("<Sync>"));
        assert!(decoded.contains("&lt;hello&gt;"));
    }

    /// Test that string table references are rejected.
    #[test]
    fn strt_token_rejected() {
        // Manually construct a WBXML with STR_T token (0x83) referencing offset 0 in an empty string table.
        // This should be rejected.
        let wbxml = vec![
            0x03, 0x01, 0x6A, 0x00, // header + zero string table length
            0x83, 0x00, // STR_T offset 0 (but no string table)
        ];
        let result = decode(&wbxml);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("Unsupported WBXML token"));
    }

    /// Test that LITERAL tokens are rejected.
    #[test]
    fn literal_token_rejected() {
        let wbxml = vec![
            0x03, 0x01, 0x6A, 0x00, // header
            0x04, 0x00, // LITERAL token with offset 0 (no string table)
        ];
        let result = decode(&wbxml);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("Unsupported WBXML token"));
    }
}
                let scope_page = if prefix.is_some() {
                    resolved_prefix_page
                } else {
                    scope_page_stack.last().copied()
                };
                if !encode_tag(&mut body, local, &mut current_page, true, scope_page) {
                    return Err(format!("Unknown tag or ambiguous namespace: {}", full_name));
                }
                // Push the resolved scope page. For unknown-prefix tags (which are already
                // rejected), this would not be reached.
                let fallback = scope_page_stack.last().copied().unwrap_or(current_page);
                scope_page_stack.push(scope_page.unwrap_or(fallback));
            }
            Ok(quick_xml::events::Event::Empty(ref e)) => {
                let full_name = String::from_utf8_lossy(e.name().as_ref()).to_string();
                let (prefix, local) = split_prefix(&full_name);
                let resolved_prefix_page = prefix.and_then(|p| PREFIX_TO_PAGE.get(p).copied());
                let scope_page = if prefix.is_some() {
                    resolved_prefix_page
                } else {
                    scope_page_stack.last().copied()
                };
                if !encode_tag(&mut body, local, &mut current_page, false, scope_page) {
                    return Err(format!("Unknown tag or ambiguous namespace: {}", full_name));
                }
            }
            Ok(quick_xml::events::Event::End(_)) => {
                body.push(TAG_END);
                scope_page_stack.pop();
            }
            Ok(quick_xml::events::Event::Text(ref e)) => {
                let text_str = std::str::from_utf8(e.as_ref())
                    .map_err(|_| "Invalid UTF-8 in XML text node".to_string())?;
                let t = quick_xml::escape::unescape(text_str)
                    .map_err(|e| format!("XML text unescape error: {}", e))?;
                if !t.trim().is_empty() {
                    body.push(TAG_STR_I);
                    body.extend(t.as_bytes());
                    body.push(0x00);
                }
            }
            }
            Ok(quick_xml::events::Event::Eof) => break,
            Ok(quick_xml::events::Event::CData(ref e)) => {
                let raw = e.as_ref();
                if raw.contains(&0u8) {
                    return Err("CData content contains NUL byte, cannot encode as WBXML inline string".to_string());
                }
                body.push(TAG_STR_I);
                body.extend_from_slice(raw);
                body.push(0x00);
            }
            Ok(_) => {}
            Err(e) => return Err(format!("XML parsing error: {}", e)),
        }
    }

    let mut output = vec![0x03, 0x01, 0x6A];
    write_mb_u_int32(&mut output, 0); // string table length = 0
    output.extend_from_slice(&body);
    Ok(output)
}

/// Write a multi-byte integer in WBXML format (7-bit groups, big-endian).
fn write_mb_u_int32(out: &mut Vec<u8>, mut v: usize) {
    if v == 0 {
        out.push(0);
        return;
    }
    let mut bytes = Vec::new();
    while v > 0 {
        bytes.push((v & 0x7F) as u8);
        v >>= 7;
    }
    for (i, b) in bytes.iter().enumerate().rev() {
        let mut byte = *b;
        if i != 0 {
            byte |= 0x80;
        }
        out.push(byte);
    }
}

/// Split a possibly-prefixed tag name (e.g. "Calendar:Type") into an optional
/// prefix and the local name. Returns `(None, name)` when there is no prefix.
fn split_prefix(name: &str) -> (Option<&str>, &str) {
    match name.split_once(':') {
        Some((prefix, local)) if !prefix.is_empty() && !local.is_empty() => (Some(prefix), local),
        _ => (None, name),
    }
}

/// Encode a single tag. Returns `true` on success, `false` if the tag is unknown
/// or the prefix cannot be resolved to a unique page.
fn encode_tag(
    output: &mut Vec<u8>,
    name: &str,
    current_page: &mut u8,
    has_content: bool,
    target_page: Option<u8>,
) -> bool {
    let entries = match NAME_MAP.get(name) {
        Some(e) => e,
        None => return false,
    };
    // When a namespace prefix resolved to a specific code page, use that page
    // for disambiguation. Otherwise prefer the entry on the current page to
    // avoid unnecessary page switches.
    let (page, token) = if let Some(tp) = target_page {
        match entries.iter().find(|(p, _)| *p == tp) {
            Some(entry) => entry,
            None => return false,
        }
    } else {
        if let Some(entry) = entries.iter().find(|(p, _)| *p == *current_page) {
            entry
        } else if entries.len() == 1 {
            &entries[0]
        } else {
            return false;
        }
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
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Test round-trip encoding/decoding of a simple known message.
    #[test]
    fn round_trip_basic_sync() {
        let xml = r#"<Sync xmlns="AirSync"><Collections><Collection><SyncKey>1</SyncKey></Collection></Collections></Sync>"#;
        let encoded = encode(xml).expect("encode failed");
        let decoded = decode(&encoded).expect("decode failed");
        // After round-trip, namespaces are stripped (prefixes become default ns)
        assert!(decoded.contains("<Sync>"));
        assert!(decoded.contains("<Collections>"));
        assert!(decoded.contains("<Collection>"));
        assert!(decoded.contains("<SyncKey>1</SyncKey>"));
    }

    /// Test that a tag with a known prefix is encoded with correct page switch.
    #[test]
    fn known_prefix_encoding() {
        let xml = "<Calendar:Recurrence><AirSyncBase:Type>2</AirSyncBase:Type></Calendar:Recurrence>";
        let encoded = encode(xml).expect("encode should succeed");
        let decoded = decode(&encoded).expect("round-trip decode should succeed");
        assert!(decoded.contains("<Recurrence>"));
        assert!(decoded.contains("<Type>"));
    }

    /// Test that an unknown tag results in an error.
    #[test]
    fn unknown_tag_returns_error() {
        let xml = "<Unknown:Tag>test</Unknown:Tag>";
        assert!(encode(xml).is_err());
    }

    /// Test handling of CData.
    #[test]
    fn cdata_encoding() {
        let xml = "<Sync><![CDATA[<hello>]]></Sync>";
        let encoded = encode(xml).expect("encode failed");
        let decoded = decode(&encoded).expect("decode failed");
        assert!(decoded.contains("<Sync>"));
        assert!(decoded.contains("&lt;hello&gt;"));
    }

    /// Test that string table references are rejected.
    #[test]
    fn strt_token_rejected() {
        // Manually construct a WBXML with STR_T token (0x83) referencing offset 0 in an empty string table.
        // This should be rejected.
        let wbxml = vec![
            0x03, 0x01, 0x6A, 0x00, // header + zero string table length
            0x83, 0x00, // STR_T offset 0 (but no string table)
        ];
        let result = decode(&wbxml);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("Unsupported WBXML token"));
    }

    /// Test that LITERAL tokens are rejected.
    #[test]
    fn literal_token_rejected() {
        let wbxml = vec![
            0x03, 0x01, 0x6A, 0x00, // header
            0x04, 0x00, // LITERAL token with offset 0 (no string table)
        ];
        let result = decode(&wbxml);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("Unsupported WBXML token"));
    }
}
