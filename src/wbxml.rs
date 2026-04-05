// src/wbxml.rs
use anyhow::{anyhow, Result};
use std::collections::HashMap;
use std::sync::LazyLock;

const SWITCH_PAGE: u8 = 0x00;
const END: u8 = 0x01;
const ENTITY: u8 = 0x02;
const STR_I: u8 = 0x03;
const LITERAL: u8 = 0x04;
const STR_T: u8 = 0x83;
const OPAQUE: u8 = 0xC3;

static TAG_TO_NAME: LazyLock<HashMap<(u8, u8), &'static str>> = LazyLock::new(build_tag_to_name);
static NAME_TO_TAG: LazyLock<HashMap<&'static str, (u8, u8)>> = LazyLock::new(|| {
    TAG_TO_NAME.iter().map(|(&k, &v)| (v, k)).collect()
});

fn namespace_to_code_page(ns: &str) -> Option<u8> {
    match ns {
        "AirSync:" => Some(0),
        "Contacts:" => Some(1),
        "Email:" => Some(2),
        "Calendar:" => Some(4),
        "Move:" | "MoveItems:" => Some(5),
        "GetItemEstimate:" => Some(6),
        "FolderHierarchy:" => Some(7),
        "MeetingResponse:" => Some(8),
        "Tasks:" => Some(9),
        "ResolveRecipients:" => Some(10),
        "ValidateCert:" => Some(11),
        "Contacts2:" => Some(12),
        "Ping:" => Some(13),
        "Provision:" => Some(14),
        "Search:" => Some(15),
        "GAL:" => Some(16),
        "AirSyncBase:" => Some(17),
        "Settings:" => Some(18),
        "DocumentLibrary:" => Some(19),
        "ItemOperations:" => Some(20),
        "ComposeMail:" => Some(21),
        "Email2:" => Some(22),
        "Notes:" => Some(23),
        "RightsManagement:" => Some(24),
        _ => None,
    }
}

fn find_encode_tag(qualified_or_local: &str, override_cp: Option<u8>) -> Option<(u8, u8)> {
    if let Some(&pair) = NAME_TO_TAG.get(qualified_or_local) {
        if let Some(cp) = override_cp {
            if pair.0 == cp {
                return Some(pair);
            }
        } else {
            return Some(pair);
        }
    }
    for (name, &(cp, id)) in NAME_TO_TAG.iter() {
        let local = if let Some(p) = name.rfind(':') { &name[p + 1..] } else { name };
        if local == qualified_or_local {
            if let Some(ocp) = override_cp {
                if cp == ocp { return Some((cp, id)); }
            } else {
                return Some((cp, id));
            }
        }
    }
    None
}

#[rustfmt::skip]
fn build_tag_to_name() -> HashMap<(u8, u8), &'static str> {
    let mut m: HashMap<(u8, u8), &'static str> = HashMap::new();


    m.insert((0, 0x05), "Sync");
    m.insert((0, 0x06), "Responses");
    m.insert((0, 0x07), "Add");
    m.insert((0, 0x08), "Change");
    m.insert((0, 0x09), "Delete");
    m.insert((0, 0x0A), "Fetch");
    m.insert((0, 0x0B), "SyncKey");
    m.insert((0, 0x0C), "ClientId");
    m.insert((0, 0x0D), "ServerId");
    m.insert((0, 0x0E), "Status");
    m.insert((0, 0x0F), "Collection");
    m.insert((0, 0x10), "Class");
    m.insert((0, 0x12), "CollectionId");
    m.insert((0, 0x13), "GetChanges");
    m.insert((0, 0x14), "MoreAvailable");
    m.insert((0, 0x15), "WindowSize");
    m.insert((0, 0x16), "Commands");
    m.insert((0, 0x17), "Options");
    m.insert((0, 0x18), "FilterType");
    m.insert((0, 0x1B), "Conflict");
    m.insert((0, 0x1C), "Collections");
    m.insert((0, 0x1D), "ApplicationData");
    m.insert((0, 0x1E), "DeletesAsMoves");
    m.insert((0, 0x20), "Supported");
    m.insert((0, 0x21), "SoftDelete");
    m.insert((0, 0x22), "MIMESupport");
    m.insert((0, 0x23), "MIMETruncation");
    m.insert((0, 0x24), "Wait");
    m.insert((0, 0x25), "Limit");
    m.insert((0, 0x26), "Partial");
    m.insert((0, 0x27), "ConversationMode");
    m.insert((0, 0x28), "MaxItems");
    m.insert((0, 0x29), "HeartbeatInterval");


    m.insert((1, 0x05), "Contacts:Anniversary");
    m.insert((1, 0x06), "Contacts:AssistantName");
    m.insert((1, 0x07), "Contacts:AssistantPhoneNumber");
    m.insert((1, 0x08), "Contacts:Birthday");
    m.insert((1, 0x13), "Contacts:BusinessPhoneNumber");
    m.insert((1, 0x19), "Contacts:CompanyName");
    m.insert((1, 0x1B), "Contacts:Email1Address");
    m.insert((1, 0x1C), "Contacts:Email2Address");
    m.insert((1, 0x1D), "Contacts:Email3Address");
    m.insert((1, 0x1F), "Contacts:FirstName");
    m.insert((1, 0x21), "Contacts:HomeCity");
    m.insert((1, 0x22), "Contacts:HomeCountry");
    m.insert((1, 0x26), "Contacts:HomePhoneNumber");
    m.insert((1, 0x29), "Contacts:LastName");
    m.insert((1, 0x2B), "Contacts:MobilePhoneNumber");
    m.insert((1, 0x2F), "Contacts:Suffix");
    m.insert((1, 0x30), "Contacts:Title");
    m.insert((1, 0x33), "Contacts:JobTitle");
    m.insert((1, 0x35), "Contacts:MiddleName");
    m.insert((1, 0x37), "Contacts:NickName");
    m.insert((1, 0x39), "Contacts:OfficeLocation");
    m.insert((1, 0x45), "Contacts:WebPage");
    m.insert((1, 0x47), "Contacts:YomiCompanyName");
    m.insert((1, 0x48), "Contacts:YomiFirstName");
    m.insert((1, 0x49), "Contacts:YomiLastName");


    m.insert((2, 0x05), "Email:Attachment");
    m.insert((2, 0x06), "Email:Attachments");
    m.insert((2, 0x07), "Email:AttName");
    m.insert((2, 0x08), "Email:AttSize");
    m.insert((2, 0x0C), "Email:Body");
    m.insert((2, 0x0E), "Email:DateReceived");
    m.insert((2, 0x11), "Email:DisplayTo");
    m.insert((2, 0x14), "Email:Subject");
    m.insert((2, 0x15), "Email:Read");
    m.insert((2, 0x16), "Email:To");
    m.insert((2, 0x17), "Email:Cc");
    m.insert((2, 0x18), "Email:From");
    m.insert((2, 0x19), "Email:Reply-To");
    m.insert((2, 0x1A), "Email:AllDayEvent");
    m.insert((2, 0x1B), "Email:Categories");
    m.insert((2, 0x1C), "Email:Category");
    m.insert((2, 0x1D), "Email:DtStamp");
    m.insert((2, 0x1E), "Email:EndTime");
    m.insert((2, 0x1F), "Email:InstanceType");
    m.insert((2, 0x20), "Email:BusyStatus");
    m.insert((2, 0x24), "Email:Location");
    m.insert((2, 0x25), "Email:MeetingRequest");
    m.insert((2, 0x26), "Email:Organizer");
    m.insert((2, 0x28), "Email:Recurrence");
    m.insert((2, 0x2B), "Email:Reminder");
    m.insert((2, 0x2C), "Email:RequiredAttendees");
    m.insert((2, 0x2D), "Email:OptionalAttendees");
    m.insert((2, 0x2E), "Email:ResourceAttendees");
    m.insert((2, 0x2F), "Email:ResponseRequested");
    m.insert((2, 0x30), "Email:Sensitivity");
    m.insert((2, 0x31), "Email:StartTime");
    m.insert((2, 0x32), "Email:Timezone");
    m.insert((2, 0x33), "Email:GlobalObjId");
    m.insert((2, 0x34), "Email:ThreadTopic");
    m.insert((2, 0x39), "Email:InternetCPID");
    m.insert((2, 0x3A), "Email:Flag");
    m.insert((2, 0x3B), "Email:FlagStatus");
    m.insert((2, 0x3C), "Email:ContentClass");
    m.insert((2, 0x3D), "Email:FlagType");
    m.insert((2, 0x3E), "Email:CompleteTime");
    m.insert((2, 0x40), "Email:DisallowNewTimeProposal");


    m.insert((4, 0x05), "Calendar:Timezone");
    m.insert((4, 0x06), "Calendar:AllDayEvent");
    m.insert((4, 0x07), "Calendar:Attendees");
    m.insert((4, 0x08), "Calendar:Attendee");
    m.insert((4, 0x09), "Calendar:Email");
    m.insert((4, 0x0A), "Calendar:Name");
    m.insert((4, 0x0D), "Calendar:BusyStatus");
    m.insert((4, 0x0E), "Calendar:Categories");
    m.insert((4, 0x0F), "Calendar:Category");
    m.insert((4, 0x11), "Calendar:DtStamp");
    m.insert((4, 0x12), "Calendar:EndTime");
    m.insert((4, 0x13), "Calendar:Exception");
    m.insert((4, 0x14), "Calendar:Exceptions");
    m.insert((4, 0x15), "Calendar:Deleted");
    m.insert((4, 0x16), "Calendar:ExceptionStartTime");
    m.insert((4, 0x17), "Calendar:Location");
    m.insert((4, 0x18), "Calendar:MeetingStatus");
    m.insert((4, 0x19), "Calendar:OrganizerEmail");
    m.insert((4, 0x1A), "Calendar:OrganizerName");
    m.insert((4, 0x1B), "Calendar:Recurrence");
    m.insert((4, 0x1C), "Calendar:Type");
    m.insert((4, 0x1D), "Calendar:Until");
    m.insert((4, 0x1E), "Calendar:Occurrences");
    m.insert((4, 0x1F), "Calendar:Interval");
    m.insert((4, 0x20), "Calendar:DayOfWeek");
    m.insert((4, 0x21), "Calendar:DayOfMonth");
    m.insert((4, 0x22), "Calendar:WeekOfMonth");
    m.insert((4, 0x23), "Calendar:MonthOfYear");
    m.insert((4, 0x24), "Calendar:Reminder");
    m.insert((4, 0x25), "Calendar:Sensitivity");
    m.insert((4, 0x26), "Calendar:Subject");
    m.insert((4, 0x27), "Calendar:StartTime");
    m.insert((4, 0x28), "Calendar:UID");
    m.insert((4, 0x29), "Calendar:AttendeeStatus");
    m.insert((4, 0x2A), "Calendar:AttendeeType");
    m.insert((4, 0x33), "Calendar:DisallowNewTimeProposal");
    m.insert((4, 0x34), "Calendar:ResponseRequested");
    m.insert((4, 0x35), "Calendar:AppointmentReplyTime");
    m.insert((4, 0x36), "Calendar:ResponseType");
    m.insert((4, 0x37), "Calendar:CalendarType");
    m.insert((4, 0x38), "Calendar:IsLeapMonth");
    m.insert((4, 0x39), "Calendar:FirstDayOfWeek");
    m.insert((4, 0x3A), "Calendar:OnlineMeetingConfLink");
    m.insert((4, 0x3B), "Calendar:OnlineMeetingExternalLink");
    m.insert((4, 0x3C), "Calendar:ClientUid");


    m.insert((5, 0x05), "MoveItems");
    m.insert((5, 0x06), "Move");
    m.insert((5, 0x07), "SrcMsgId");
    m.insert((5, 0x08), "SrcFldId");
    m.insert((5, 0x09), "DstFldId");
    m.insert((5, 0x0A), "MoveResponse");
    m.insert((5, 0x0B), "MoveStatus");


    m.insert((6, 0x05), "GetItemEstimate");
    m.insert((6, 0x06), "GIEVersion");
    m.insert((6, 0x07), "GIECollections");
    m.insert((6, 0x08), "GIECollection");
    m.insert((6, 0x09), "GIEClass");
    m.insert((6, 0x0A), "GIECollectionId");
    m.insert((6, 0x0B), "DateTime");
    m.insert((6, 0x0C), "Estimate");
    m.insert((6, 0x0D), "Response");
    m.insert((6, 0x0E), "Status");


    m.insert((7, 0x07), "DisplayName");
    m.insert((7, 0x08), "ServerId");
    m.insert((7, 0x09), "ParentId");
    m.insert((7, 0x0A), "Type");
    m.insert((7, 0x0C), "Status");
    m.insert((7, 0x0E), "Changes");
    m.insert((7, 0x0F), "Add");
    m.insert((7, 0x10), "Delete");
    m.insert((7, 0x11), "Update");
    m.insert((7, 0x12), "SyncKey");
    m.insert((7, 0x13), "FolderCreate");
    m.insert((7, 0x14), "FolderDelete");
    m.insert((7, 0x15), "FolderUpdate");
    m.insert((7, 0x16), "FolderSync");
    m.insert((7, 0x17), "Count");


    m.insert((8, 0x05), "CalendarId");
    m.insert((8, 0x06), "MeetingCollectionId");
    m.insert((8, 0x07), "MeetingResponse");
    m.insert((8, 0x08), "RequestId");
    m.insert((8, 0x09), "Request");
    m.insert((8, 0x0A), "Result");
    m.insert((8, 0x0B), "Status");
    m.insert((8, 0x0C), "UserResponse");
    m.insert((8, 0x0E), "InstanceId");
    m.insert((8, 0x10), "ProposedStartTime");
    m.insert((8, 0x11), "ProposedEndTime");
    m.insert((8, 0x12), "SendResponse");


    m.insert((9, 0x08), "Tasks:Complete");
    m.insert((9, 0x09), "Tasks:DateCompleted");
    m.insert((9, 0x0D), "Tasks:DueDate");
    m.insert((9, 0x0F), "Tasks:Importance");
    m.insert((9, 0x17), "Tasks:StartDate");
    m.insert((9, 0x18), "Tasks:Subject");
    m.insert((9, 0x19), "Tasks:ReminderSet");
    m.insert((9, 0x1A), "Tasks:ReminderTime");
    m.insert((9, 0x1B), "Tasks:Sensitivity");
    m.insert((9, 0x1C), "Tasks:Recurrence");
    m.insert((9, 0x1D), "Tasks:Type");
    m.insert((9, 0x1E), "Tasks:Start");
    m.insert((9, 0x1F), "Tasks:Until");
    m.insert((9, 0x20), "Tasks:Occurrences");
    m.insert((9, 0x21), "Tasks:Interval");
    m.insert((9, 0x22), "Tasks:DayOfWeek");
    m.insert((9, 0x23), "Tasks:DayOfMonth");
    m.insert((9, 0x24), "Tasks:WeekOfMonth");
    m.insert((9, 0x25), "Tasks:MonthOfYear");
    m.insert((9, 0x26), "Tasks:Regenerate");
    m.insert((9, 0x27), "Tasks:DeadOccur");
    m.insert((9, 0x28), "Tasks:Categories");
    m.insert((9, 0x29), "Tasks:Category");


    m.insert((10, 0x05), "ResolveRecipients");
    m.insert((10, 0x06), "Response");
    m.insert((10, 0x07), "Status");
    m.insert((10, 0x08), "Type");
    m.insert((10, 0x09), "Recipient");
    m.insert((10, 0x0A), "DisplayName");
    m.insert((10, 0x0B), "EmailAddress");
    m.insert((10, 0x0C), "Certificates");
    m.insert((10, 0x0D), "Certificate");
    m.insert((10, 0x0E), "MiniCertificate");
    m.insert((10, 0x0F), "Options");
    m.insert((10, 0x10), "To");
    m.insert((10, 0x11), "CertificateRetrieval");
    m.insert((10, 0x12), "RecipientCount");
    m.insert((10, 0x13), "MaxCertificates");
    m.insert((10, 0x14), "MaxAmbiguousRecipients");
    m.insert((10, 0x15), "CertificateCount");
    m.insert((10, 0x16), "Availability");
    m.insert((10, 0x17), "StartTime");
    m.insert((10, 0x18), "EndTime");
    m.insert((10, 0x19), "MergedFreeBusy");
    m.insert((10, 0x1A), "Picture");
    m.insert((10, 0x1B), "MaxSize");
    m.insert((10, 0x1C), "Data");
    m.insert((10, 0x1D), "MaxPictures");


    m.insert((11, 0x05), "ValidateCert");
    m.insert((11, 0x06), "Certificates");
    m.insert((11, 0x07), "Certificate");
    m.insert((11, 0x08), "CertificateChain");
    m.insert((11, 0x09), "CheckCRL");
    m.insert((11, 0x0A), "CertificateStatus");
    m.insert((11, 0x0B), "Status");


    m.insert((12, 0x05), "Contacts2:CustomerId");
    m.insert((12, 0x06), "Contacts2:GovernmentId");
    m.insert((12, 0x07), "Contacts2:IMAddress");
    m.insert((12, 0x08), "Contacts2:IMAddress2");
    m.insert((12, 0x09), "Contacts2:IMAddress3");
    m.insert((12, 0x0A), "Contacts2:ManagerName");
    m.insert((12, 0x0B), "Contacts2:CompanyMainPhone");
    m.insert((12, 0x0C), "Contacts2:AccountName");
    m.insert((12, 0x0D), "Contacts2:MMS");
    m.insert((12, 0x0E), "Contacts2:NickName");


    m.insert((13, 0x05), "Ping");
    m.insert((13, 0x07), "Status");
    m.insert((13, 0x08), "HeartbeatInterval");
    m.insert((13, 0x09), "Folders");
    m.insert((13, 0x0A), "Folder");
    m.insert((13, 0x0B), "Id");
    m.insert((13, 0x0C), "Class");
    m.insert((13, 0x0D), "MaxFolders");


    m.insert((14, 0x05), "Provision");
    m.insert((14, 0x06), "Policies");
    m.insert((14, 0x07), "Policy");
    m.insert((14, 0x08), "PolicyType");
    m.insert((14, 0x09), "PolicyKey");
    m.insert((14, 0x0A), "Data");
    m.insert((14, 0x0B), "Status");
    m.insert((14, 0x0C), "RemoteWipe");
    m.insert((14, 0x0D), "EASProvisionDoc");
    m.insert((14, 0x0E), "DevicePasswordEnabled");
    m.insert((14, 0x0F), "AlphanumericDevicePasswordRequired");
    m.insert((14, 0x10), "RequireStorageCardEncryption");
    m.insert((14, 0x11), "PasswordRecoveryEnabled");
    m.insert((14, 0x13), "AttachmentsEnabled");
    m.insert((14, 0x14), "MinDevicePasswordLength");
    m.insert((14, 0x15), "MaxInactivityTimeDeviceLock");
    m.insert((14, 0x16), "MaxDevicePasswordFailedAttempts");
    m.insert((14, 0x17), "MaxAttachmentSize");
    m.insert((14, 0x18), "AllowSimpleDevicePassword");
    m.insert((14, 0x19), "DevicePasswordExpiration");
    m.insert((14, 0x1A), "DevicePasswordHistory");
    m.insert((14, 0x1B), "AllowStorageCard");
    m.insert((14, 0x1C), "AllowCamera");
    m.insert((14, 0x1D), "RequireDeviceEncryption");
    m.insert((14, 0x1E), "AllowUnsignedApplications");
    m.insert((14, 0x1F), "AllowUnsignedInstallationPackages");
    m.insert((14, 0x20), "MinDevicePasswordComplexCharacters");
    m.insert((14, 0x21), "AllowWifi");
    m.insert((14, 0x22), "AllowTextMessaging");
    m.insert((14, 0x23), "AllowPOPIMAPEmail");
    m.insert((14, 0x24), "AllowBluetooth");
    m.insert((14, 0x25), "AllowIrDA");
    m.insert((14, 0x26), "RequireManualSyncWhenRoaming");
    m.insert((14, 0x27), "AllowDesktopSync");
    m.insert((14, 0x28), "MaxCalendarAgeFilter");
    m.insert((14, 0x29), "AllowHTMLEmail");
    m.insert((14, 0x2A), "MaxEmailAgeFilter");
    m.insert((14, 0x2B), "MaxEmailBodyTruncationSize");
    m.insert((14, 0x2C), "MaxEmailHTMLBodyTruncationSize");
    m.insert((14, 0x2D), "RequireSignedSMIMEMessages");
    m.insert((14, 0x2E), "RequireEncryptedSMIMEMessages");
    m.insert((14, 0x2F), "RequireSignedSMIMEAlgorithm");
    m.insert((14, 0x30), "RequireEncryptionSMIMEAlgorithm");
    m.insert((14, 0x31), "AllowSMIMEEncryptionAlgorithmNegotiation");
    m.insert((14, 0x32), "AllowSMIMESoftCerts");
    m.insert((14, 0x33), "AllowBrowser");
    m.insert((14, 0x34), "AllowConsumerEmail");
    m.insert((14, 0x35), "AllowRemoteDesktop");
    m.insert((14, 0x36), "AllowInternetSharing");
    m.insert((14, 0x37), "UnapprovedInROMApplicationList");
    m.insert((14, 0x38), "ApplicationName");
    m.insert((14, 0x39), "ApprovedApplicationList");
    m.insert((14, 0x3A), "Hash");


    m.insert((15, 0x05), "Search");
    m.insert((15, 0x07), "Store");
    m.insert((15, 0x08), "Name");
    m.insert((15, 0x09), "Query");
    m.insert((15, 0x0A), "Options");
    m.insert((15, 0x0B), "Range");
    m.insert((15, 0x0C), "Status");
    m.insert((15, 0x0D), "Response");
    m.insert((15, 0x0E), "Result");
    m.insert((15, 0x0F), "Properties");
    m.insert((15, 0x10), "Total");
    m.insert((15, 0x11), "EqualTo");
    m.insert((15, 0x12), "Value");
    m.insert((15, 0x13), "And");
    m.insert((15, 0x14), "Or");
    m.insert((15, 0x15), "FreeText");
    m.insert((15, 0x17), "DeepTraversal");
    m.insert((15, 0x18), "LongId");
    m.insert((15, 0x19), "RebuildResults");
    m.insert((15, 0x1A), "LeafName");
    m.insert((15, 0x1B), "Class");
    m.insert((15, 0x1C), "CollectionId");
    m.insert((15, 0x1D), "QueryId");
    m.insert((15, 0x1E), "MaxResults");


    m.insert((16, 0x05), "GAL:DisplayName");
    m.insert((16, 0x06), "GAL:Phone");
    m.insert((16, 0x07), "GAL:Office");
    m.insert((16, 0x08), "GAL:Title");
    m.insert((16, 0x09), "GAL:Company");
    m.insert((16, 0x0A), "GAL:Alias");
    m.insert((16, 0x0B), "GAL:FirstName");
    m.insert((16, 0x0C), "GAL:LastName");
    m.insert((16, 0x0D), "GAL:HomePhone");
    m.insert((16, 0x0E), "GAL:MobilePhone");
    m.insert((16, 0x0F), "GAL:EmailAddress");
    m.insert((16, 0x10), "GAL:Picture");
    m.insert((16, 0x11), "GAL:Status");
    m.insert((16, 0x12), "GAL:Data");


    m.insert((17, 0x05), "AirSyncBase:BodyPreference");
    m.insert((17, 0x06), "AirSyncBase:Type");
    m.insert((17, 0x07), "AirSyncBase:TruncationSize");
    m.insert((17, 0x08), "AirSyncBase:AllOrNone");
    m.insert((17, 0x0A), "AirSyncBase:Body");
    m.insert((17, 0x0B), "AirSyncBase:Data");
    m.insert((17, 0x0C), "AirSyncBase:EstimatedDataSize");
    m.insert((17, 0x0D), "AirSyncBase:Truncated");
    m.insert((17, 0x0E), "AirSyncBase:Attachments");
    m.insert((17, 0x0F), "AirSyncBase:Attachment");
    m.insert((17, 0x10), "AirSyncBase:DisplayName");
    m.insert((17, 0x11), "AirSyncBase:FileReference");
    m.insert((17, 0x12), "AirSyncBase:Method");
    m.insert((17, 0x13), "AirSyncBase:ContentId");
    m.insert((17, 0x14), "AirSyncBase:ContentLocation");
    m.insert((17, 0x15), "AirSyncBase:IsInline");
    m.insert((17, 0x16), "AirSyncBase:NativeBodyType");
    m.insert((17, 0x17), "AirSyncBase:ContentType");
    m.insert((17, 0x18), "AirSyncBase:Preview");
    m.insert((17, 0x19), "AirSyncBase:BodyPartPreference");
    m.insert((17, 0x1A), "AirSyncBase:BodyPart");
    m.insert((17, 0x1B), "AirSyncBase:Status");
    m.insert((17, 0x1C), "AirSyncBase:Add");
    m.insert((17, 0x1D), "AirSyncBase:Delete");
    m.insert((17, 0x1E), "AirSyncBase:ClientId");
    m.insert((17, 0x1F), "AirSyncBase:Content");
    m.insert((17, 0x20), "AirSyncBase:Location");
    m.insert((17, 0x21), "AirSyncBase:Annotation");
    m.insert((17, 0x22), "AirSyncBase:Street");
    m.insert((17, 0x23), "AirSyncBase:City");
    m.insert((17, 0x24), "AirSyncBase:State");
    m.insert((17, 0x25), "AirSyncBase:Country");
    m.insert((17, 0x26), "AirSyncBase:PostalCode");
    m.insert((17, 0x27), "AirSyncBase:Latitude");
    m.insert((17, 0x28), "AirSyncBase:Longitude");
    m.insert((17, 0x29), "AirSyncBase:Accuracy");
    m.insert((17, 0x2A), "AirSyncBase:Altitude");
    m.insert((17, 0x2B), "AirSyncBase:AltitudeAccuracy");
    m.insert((17, 0x2C), "AirSyncBase:LocationUri");
    m.insert((17, 0x2D), "AirSyncBase:InstanceId");


    m.insert((18, 0x05), "Settings");
    m.insert((18, 0x06), "Status");
    m.insert((18, 0x07), "Get");
    m.insert((18, 0x08), "Set");
    m.insert((18, 0x09), "Oof");
    m.insert((18, 0x0A), "OofState");
    m.insert((18, 0x0B), "StartTime");
    m.insert((18, 0x0C), "EndTime");
    m.insert((18, 0x0D), "OofMessage");
    m.insert((18, 0x0E), "AppliesToInternal");
    m.insert((18, 0x0F), "AppliesToExternalKnown");
    m.insert((18, 0x10), "AppliesToExternalUnknown");
    m.insert((18, 0x11), "Enabled");
    m.insert((18, 0x12), "ReplyMessage");
    m.insert((18, 0x13), "BodyType");
    m.insert((18, 0x14), "DevicePassword");
    m.insert((18, 0x15), "Password");
    m.insert((18, 0x16), "DeviceInformation");
    m.insert((18, 0x17), "Model");
    m.insert((18, 0x18), "IMEI");
    m.insert((18, 0x19), "FriendlyName");
    m.insert((18, 0x1A), "OS");
    m.insert((18, 0x1B), "OSLanguage");
    m.insert((18, 0x1C), "PhoneNumber");
    m.insert((18, 0x1D), "UserInformation");
    m.insert((18, 0x1E), "EmailAddresses");
    m.insert((18, 0x1F), "SMTPAddress");
    m.insert((18, 0x20), "UserAgent");
    m.insert((18, 0x21), "EnableOutboundSMS");
    m.insert((18, 0x22), "MobileOperator");
    m.insert((18, 0x23), "PrimarySmtpAddress");
    m.insert((18, 0x24), "Accounts");
    m.insert((18, 0x25), "Account");
    m.insert((18, 0x26), "AccountId");
    m.insert((18, 0x27), "AccountName");
    m.insert((18, 0x28), "UserDisplayName");
    m.insert((18, 0x29), "SendDisabled");
    m.insert((18, 0x2B), "RightsManagementInformation");


    m.insert((19, 0x05), "DocumentLibrary:LinkId");
    m.insert((19, 0x06), "DocumentLibrary:DisplayName");
    m.insert((19, 0x07), "DocumentLibrary:IsFolder");
    m.insert((19, 0x08), "DocumentLibrary:CreationDate");
    m.insert((19, 0x09), "DocumentLibrary:LastModifiedDate");
    m.insert((19, 0x0A), "DocumentLibrary:IsHidden");
    m.insert((19, 0x0B), "DocumentLibrary:ContentLength");
    m.insert((19, 0x0C), "DocumentLibrary:ContentType");


    m.insert((20, 0x05), "ItemOperations");
    m.insert((20, 0x06), "Fetch");
    m.insert((20, 0x07), "Store");
    m.insert((20, 0x08), "Options");
    m.insert((20, 0x09), "Range");
    m.insert((20, 0x0A), "Total");
    m.insert((20, 0x0B), "Properties");
    m.insert((20, 0x0C), "Data");
    m.insert((20, 0x0D), "Status");
    m.insert((20, 0x0E), "Response");
    m.insert((20, 0x0F), "Version");
    m.insert((20, 0x10), "Schema");
    m.insert((20, 0x11), "Part");
    m.insert((20, 0x12), "EmptyFolderContents");
    m.insert((20, 0x13), "DeleteSubFolders");
    m.insert((20, 0x14), "UserName");
    m.insert((20, 0x15), "IOPassword");
    m.insert((20, 0x16), "Move");
    m.insert((20, 0x17), "DstFldId");
    m.insert((20, 0x18), "ConversationId");
    m.insert((20, 0x19), "MoveAlways");


    m.insert((21, 0x05), "SendMail");
    m.insert((21, 0x06), "SmartForward");
    m.insert((21, 0x07), "SmartReply");
    m.insert((21, 0x08), "SaveInSentItems");
    m.insert((21, 0x09), "ReplaceMime");
    m.insert((21, 0x0B), "Mime");
    m.insert((21, 0x0C), "ClientId");
    m.insert((21, 0x0D), "Status");
    m.insert((21, 0x0E), "AccountId");
    m.insert((21, 0x0F), "Forwardees");
    m.insert((21, 0x10), "Forwardee");
    m.insert((21, 0x11), "ForwardeeName");
    m.insert((21, 0x12), "ForwardeeEmail");


    m.insert((22, 0x05), "Email2:UmCallerId");
    m.insert((22, 0x06), "Email2:UmUserNotes");
    m.insert((22, 0x07), "Email2:UmAttDuration");
    m.insert((22, 0x08), "Email2:UmAttOrder");
    m.insert((22, 0x09), "Email2:ConversationId");
    m.insert((22, 0x0A), "Email2:ConversationIndex");
    m.insert((22, 0x0B), "Email2:LastVerbExecuted");
    m.insert((22, 0x0C), "Email2:LastVerbExecutionTime");
    m.insert((22, 0x0D), "Email2:ReceivedAsBcc");
    m.insert((22, 0x0E), "Email2:Sender");
    m.insert((22, 0x0F), "Email2:CalendarType");
    m.insert((22, 0x10), "Email2:IsLeapMonth");
    m.insert((22, 0x11), "Email2:AccountId");
    m.insert((22, 0x12), "Email2:FirstDayOfWeek");
    m.insert((22, 0x13), "Email2:MeetingMessageType");


    m.insert((23, 0x05), "Notes:Subject");
    m.insert((23, 0x06), "Notes:MessageClass");
    m.insert((23, 0x07), "Notes:LastModifiedDate");
    m.insert((23, 0x08), "Notes:Categories");
    m.insert((23, 0x09), "Notes:Category");
    m.insert((23, 0x0B), "Notes:Body");


    m.insert((24, 0x05), "RightsManagement:RightsManagementSupport");
    m.insert((24, 0x06), "RightsManagement:RightsManagementTemplates");
    m.insert((24, 0x07), "RightsManagement:RightsManagementTemplate");
    m.insert((24, 0x08), "RightsManagement:RightsManagementLicense");
    m.insert((24, 0x09), "RightsManagement:EditAllowed");
    m.insert((24, 0x0A), "RightsManagement:ReplyAllowed");
    m.insert((24, 0x0B), "RightsManagement:ReplyAllAllowed");
    m.insert((24, 0x0C), "RightsManagement:ForwardAllowed");
    m.insert((24, 0x0D), "RightsManagement:ModifyRecipientsAllowed");
    m.insert((24, 0x0E), "RightsManagement:ExtractAllowed");
    m.insert((24, 0x0F), "RightsManagement:PrintAllowed");
    m.insert((24, 0x10), "RightsManagement:ExportAllowed");
    m.insert((24, 0x11), "RightsManagement:ProgrammaticAccessAllowed");
    m.insert((24, 0x12), "RightsManagement:RMOwner");
    m.insert((24, 0x13), "RightsManagement:ContentExpiryDate");
    m.insert((24, 0x14), "RightsManagement:ContentExpiryDateString");
    m.insert((24, 0x15), "RightsManagement:ContentExpiryInterval");
    m.insert((24, 0x16), "RightsManagement:ContentExpiryIntervalType");
    m.insert((24, 0x17), "RightsManagement:TemplateID");
    m.insert((24, 0x18), "RightsManagement:TemplateName");
    m.insert((24, 0x19), "RightsManagement:TemplateDescription");
    m.insert((24, 0x1A), "RightsManagement:ContentOwner");
    m.insert((24, 0x1B), "RightsManagement:RemoveRightsManagementDistribution");

    m
}

pub struct Wbxml;

impl Wbxml {
    pub fn new() -> Self {
        Wbxml
    }

    fn read_mb_uint(bytes: &[u8], pos: &mut usize) -> Result<u32> {
        let mut result: u32 = 0;
        let mut count = 0;
        loop {
            if *pos >= bytes.len() {
                return Err(anyhow!("Truncated WBXML mb_u_int32"));
            }
            let b = bytes[*pos];
            *pos += 1;
            result = (result << 7) | u32::from(b & 0x7F);
            count += 1;
            if (b & 0x80) == 0 {
                break;
            }
            if count > 5 {
                return Err(anyhow!("WBXML mb_u_int32 too large"));
            }
        }
        Ok(result)
    }

    fn read_inline_str(bytes: &[u8], pos: &mut usize) -> Result<String> {
        let start = *pos;
        while *pos < bytes.len() && bytes[*pos] != 0 {
            *pos += 1;
        }
        if *pos >= bytes.len() {
            return Err(anyhow!("Unterminated STR_I string"));
        }
        let s = String::from_utf8(bytes[start..*pos].to_vec())?;
        *pos += 1;
        Ok(s)
    }

    pub fn decode(&self, bytes: &[u8]) -> Result<String> {
        if bytes.is_empty() {
            return Err(anyhow!("Empty WBXML payload"));
        }
        if bytes[0] == b'<' {
            return Ok(String::from_utf8(bytes.to_vec())?);
        }

        let mut pos = 0usize;
        let _version = *bytes.get(pos).ok_or_else(|| anyhow!("Missing WBXML version"))?;
        pos += 1;
        let _public_id = Self::read_mb_uint(bytes, &mut pos)?;
        let _charset = Self::read_mb_uint(bytes, &mut pos)?;
        let str_table_len = usize::try_from(Self::read_mb_uint(bytes, &mut pos)?)
            .map_err(|_| anyhow!("Invalid WBXML string table length"))?;
        if pos + str_table_len > bytes.len() {
            return Err(anyhow!("WBXML string table exceeds payload"));
        }
        let string_table = &bytes[pos..pos + str_table_len];
        pos += str_table_len;

        let mut current_code_page = 0u8;
        let mut xml_stack: Vec<String> = Vec::new();
        let mut output = String::from("<?xml version=\"1.0\" encoding=\"utf-8\"?>\n");

        while pos < bytes.len() {
            let token = bytes[pos];
            pos += 1;

            match token {
                SWITCH_PAGE => {
                    if pos >= bytes.len() {
                        return Err(anyhow!("WBXML SWITCH_PAGE missing code page"));
                    }
                    current_code_page = bytes[pos];
                    pos += 1;
                }
                END => {
                    if let Some(tag) = xml_stack.pop() {
                        output.push_str(&format!("</{tag}>"));
                    }
                }
                STR_I => {
                    let content = Self::read_inline_str(bytes, &mut pos)?;
                    output.push_str(&xml_escape_text(&content));
                }
                STR_T => {
                    let offset = usize::try_from(Self::read_mb_uint(bytes, &mut pos)?)
                        .map_err(|_| anyhow!("Invalid STR_T offset"))?;
                    if offset >= string_table.len() {
                        return Err(anyhow!("STR_T offset outside string table"));
                    }
                    let mut end = offset;
                    while end < string_table.len() && string_table[end] != 0 {
                        end += 1;
                    }
                    let content = String::from_utf8(string_table[offset..end].to_vec())?;
                    output.push_str(&xml_escape_text(&content));
                }
                ENTITY => {
                    let ent = Self::read_mb_uint(bytes, &mut pos)?;
                    output.push_str(&format!("&#{ent};"));
                }
                OPAQUE => {
                    let len = usize::try_from(Self::read_mb_uint(bytes, &mut pos)?)
                        .map_err(|_| anyhow!("Invalid OPAQUE length"))?;
                    if pos + len > bytes.len() {
                        return Err(anyhow!("OPAQUE data exceeds payload"));
                    }
                    let opaque = &bytes[pos..pos + len];
                    pos += len;
                    output.push_str(&base64::engine::general_purpose::STANDARD.encode(opaque));
                }
                LITERAL => {
                    return Err(anyhow!("LITERAL token unsupported in this profile"));
                }
                _ => {
                    if token >= 0x05 {
                        let has_content = (token & 0x40) != 0;
                        let tag_id = token & 0x3F;
                        if let Some(name) = TAG_TO_NAME.get(&(current_code_page, tag_id)) {
                            output.push_str(&format!("<{name}>"));
                            if has_content {
                                xml_stack.push(name.to_string());
                            } else {
                                output.push_str(&format!("</{name}>"));
                            }
                        } else {
                            tracing::trace!(
                                "WBXML decode: unknown tag cp={} id=0x{:02x}",
                                current_code_page,
                                tag_id
                            );
                            if has_content {
                                let placeholder =
                                    format!("_unknown_cp{current_code_page}_{tag_id:02x}");
                                output.push_str(&format!("<{placeholder}>"));
                                xml_stack.push(placeholder);
                            }
                        }
                    }
                }
            }
        }

        while let Some(tag) = xml_stack.pop() {
            output.push_str(&format!("</{tag}>"));
        }

        Ok(output)
    }

    pub fn encode(&self, xml: &str) -> Result<Vec<u8>> {

        let mut buf: Vec<u8> = vec![0x03, 0x01, 0x6A, 0x00];
        let mut current_code_page = 0u8;
        let mut ns_stack: Vec<Option<u8>> = Vec::new();

        let mut reader = quick_xml::Reader::from_str(xml);
        reader.config_mut().trim_text(true);
        let mut event_buf = Vec::new();

        loop {
            match reader.read_event_into(&mut event_buf) {
                Ok(quick_xml::events::Event::Start(ref e)) => {
                    let ns_cp = extract_xmlns_cp(e);
                    ns_stack.push(ns_cp);
                    let effective_cp = ns_cp.or_else(|| ns_stack.iter().rev().find_map(|&x| x));
                    let name_raw = e.name().local_name();
                    let name_str = std::str::from_utf8(name_raw.as_ref())?;
                    self.encode_open_tag(&mut buf, &mut current_code_page, name_str, effective_cp, true)?;
                }
                Ok(quick_xml::events::Event::Empty(ref e)) => {
                    let ns_cp = extract_xmlns_cp(e);
                    let effective_cp = ns_cp.or_else(|| ns_stack.last().copied().flatten());
                    let name_raw = e.name().local_name();
                    let name_str = std::str::from_utf8(name_raw.as_ref())?;
                    self.encode_open_tag(&mut buf, &mut current_code_page, name_str, effective_cp, false)?;
                }
                Ok(quick_xml::events::Event::Text(ref e)) => {
                    let txt = e.decode().map_err(|e| anyhow!("XML decode error: {e}"))?.into_owned();
                    if !txt.is_empty() {
                        buf.push(STR_I);
                        buf.extend_from_slice(txt.as_bytes());
                        buf.push(0x00);
                    }
                }
                Ok(quick_xml::events::Event::End(_)) => {
                    ns_stack.pop();
                    buf.push(END);
                }
                Ok(quick_xml::events::Event::Eof) => break,
                Err(e) => return Err(anyhow!("XML encode error: {e:?}")),
                _ => {}
            }
            event_buf.clear();
        }

        Ok(buf)
    }

    fn encode_open_tag(
        &self,
        buf: &mut Vec<u8>,
        current_cp: &mut u8,
        name_str: &str,
        hint_cp: Option<u8>,
        has_content: bool,
    ) -> Result<()> {
        if let Some((cp, tag_id)) = find_encode_tag(name_str, hint_cp) {
            if cp != *current_cp {
                buf.push(SWITCH_PAGE);
                buf.push(cp);
                *current_cp = cp;
            }
            let token = if has_content { tag_id | 0x40 } else { tag_id };
            buf.push(token);
            return Ok(());
        }
        return Err(anyhow!("WBXML encode: unknown tag '{}'", name_str));
    }
}

fn extract_xmlns_cp<'a>(e: &quick_xml::events::BytesStart<'a>) -> Option<u8> {
    for attr in e.attributes().flatten() {
        let key = std::str::from_utf8(attr.key.as_ref()).unwrap_or("");
        if key == "xmlns" || key.starts_with("xmlns:") {
            if let Ok(val) = attr.decode_and_unescape_value(quick_xml::encoding::Decoder::utf8()) {
                if let Some(cp) = namespace_to_code_page(val.as_ref()) {
                    return Some(cp);
                }
                let with_colon = format!("{}:", val);
                if let Some(cp) = namespace_to_code_page(&with_colon) {
                    return Some(cp);
                }
            }
        }
    }
    None
}

fn xml_escape_text(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

#[cfg(test)]
mod tests {
    use super::Wbxml;

    #[test]
    fn round_trip_sync_with_namespaces() {
        let codec = Wbxml::new();
        let xml = r#"<?xml version="1.0" encoding="utf-8"?><Sync xmlns="AirSync:"><Collections><Collection><SyncKey>1</SyncKey><CollectionId>1</CollectionId><Commands></Commands></Collection></Collections></Sync>"#;
        let wb = codec.encode(xml).expect("encode");
        let dec = codec.decode(&wb).expect("decode");
        assert!(dec.contains("<Sync>"), "decoded: {dec}");
        assert!(dec.contains("<SyncKey>1</SyncKey>"), "decoded: {dec}");
    }

    #[test]
    fn round_trip_folder_sync() {
        let codec = Wbxml::new();
        let folder = r#"<FolderSync xmlns="FolderHierarchy:"><Status>1</Status><SyncKey>1</SyncKey><Changes><Count>1</Count></Changes></FolderSync>"#;
        let wb_folder = codec.encode(folder).expect("encode folder");
        let folder_dec = codec.decode(&wb_folder).expect("decode folder");
        assert!(folder_dec.contains("<FolderSync>"), "decoded: {folder_dec}");
    }

    #[test]
    fn round_trip_provision() {
        let codec = Wbxml::new();
        let prov = r#"<Provision xmlns="Provision:"><Status>1</Status><Policies><Policy><PolicyType>MS-EAS-Provisioning-WBXML</PolicyType><PolicyKey>1</PolicyKey></Policy></Policies></Provision>"#;
        let wb_prov = codec.encode(prov).expect("encode provision");
        let prov_dec = codec.decode(&wb_prov).expect("decode provision");
        assert!(prov_dec.contains("<Provision>"), "decoded: {prov_dec}");
    }

    #[test]
    fn meeting_response_code_page_correct() {
        let codec = Wbxml::new();
        let xml = r#"<MeetingResponse xmlns="MeetingResponse:"><Request><RequestId>abc</RequestId><UserResponse>1</UserResponse></Request></MeetingResponse>"#;
        let wb = codec.encode(xml).expect("encode");
        let dec = codec.decode(&wb).expect("decode");
        assert!(dec.contains("<MeetingResponse>"), "decoded: {dec}");
        assert!(dec.contains("<UserResponse>1</UserResponse>"), "decoded: {dec}");
        assert!(dec.contains("<RequestId>abc</RequestId>"), "decoded: {dec}");
    }

    #[test]
    fn settings_user_information_round_trip() {
        let codec = Wbxml::new();
        let xml = r#"<Settings xmlns="Settings:"><Status>1</Status><UserInformation><Status>1</Status><Get><EmailAddresses><SMTPAddress>user@example.com</SMTPAddress><PrimarySmtpAddress>user@example.com</PrimarySmtpAddress></EmailAddresses></Get></UserInformation></Settings>"#;
        let wb = codec.encode(xml).expect("encode settings");
        let dec = codec.decode(&wb).expect("decode settings");
        assert!(dec.contains("<Settings>"), "decoded: {dec}");
        assert!(dec.contains("<UserInformation>"), "decoded: {dec}");
        assert!(dec.contains("<EmailAddresses>"), "decoded: {dec}");
    }

    #[test]
    fn search_code_page_round_trip() {
        let codec = Wbxml::new();
        let xml = r#"<Search xmlns="Search:"><Status>1</Status><Response><Store><Status>1</Status><Total>0</Total></Store></Response></Search>"#;
        let wb = codec.encode(xml).expect("encode search");
        let dec = codec.decode(&wb).expect("decode search");
        assert!(dec.contains("<Search>"), "decoded: {dec}");
        assert!(dec.contains("<Total>0</Total>"), "decoded: {dec}");
    }

    #[test]
    fn item_operations_code_page_round_trip() {
        let codec = Wbxml::new();
        let xml = r#"<ItemOperations xmlns="ItemOperations:"><Status>1</Status><Response><Fetch><Store>Mailbox</Store><Status>1</Status></Fetch></Response></ItemOperations>"#;
        let wb = codec.encode(xml).expect("encode itemops");
        let dec = codec.decode(&wb).expect("decode itemops");
        assert!(dec.contains("<ItemOperations>"), "decoded: {dec}");
        assert!(dec.contains("<Response>"), "decoded: {dec}");
    }

    #[test]
    fn ping_max_folders_encodes() {
        let codec = Wbxml::new();
        let xml = r#"<Ping xmlns="Ping:"><Status>6</Status><MaxFolders>200</MaxFolders></Ping>"#;
        let wb = codec.encode(xml).expect("encode ping");
        let dec = codec.decode(&wb).expect("decode ping");
        assert!(dec.contains("<MaxFolders>200</MaxFolders>"), "decoded: {dec}");
    }

    #[test]
    fn plain_xml_passthrough() {
        let codec = Wbxml::new();
        let xml = b"<?xml version=\"1.0\"?><Sync/>";
        let dec = codec.decode(xml).expect("passthrough");
        assert!(dec.starts_with("<?xml"));
    }

    #[test]
    fn calendar_namespace_round_trip() {
        let codec = Wbxml::new();
        let xml = r#"<Sync xmlns="AirSync:" xmlns:Calendar="Calendar:"><Collections><Collection><SyncKey>0</SyncKey><CollectionId>1</CollectionId><Commands><Add><ClientId>c1</ClientId><ApplicationData><Calendar:Subject>Test</Calendar:Subject><Calendar:StartTime>2026-03-22T09:00:00Z</Calendar:StartTime><Calendar:EndTime>2026-03-22T10:00:00Z</Calendar:EndTime></ApplicationData></Add></Commands></Collection></Collections></Sync>"#;
        let wb = codec.encode(xml).expect("encode");
        let dec = codec.decode(&wb).expect("decode");
        assert!(dec.contains("<Calendar:Subject>Test</Calendar:Subject>"), "decoded: {dec}");
    }
}

