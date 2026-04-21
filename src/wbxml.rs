// src/wbxml.rs
use crate::util::xml_escape_text;
use anyhow::{Result, anyhow};
use base64::Engine;

const SWITCH_PAGE: u8 = 0x00;
const END: u8 = 0x01;
const ENTITY: u8 = 0x02;
const STR_I: u8 = 0x03;
const LITERAL: u8 = 0x04;
const STR_T: u8 = 0x83;
const OPAQUE: u8 = 0xC3;

static TAG_TO_NAME: phf::Map<[u8; 2], &'static str> = phf::phf_map! {
    [0u8, 0x05u8] => "Sync",
    [0u8, 0x06u8] => "Responses",
    [0u8, 0x07u8] => "Add",
    [0u8, 0x08u8] => "Change",
    [0u8, 0x09u8] => "Delete",
    [0u8, 0x0Au8] => "Fetch",
    [0u8, 0x0Bu8] => "SyncKey",
    [0u8, 0x0Cu8] => "ClientId",
    [0u8, 0x0Du8] => "ServerId",
    [0u8, 0x0Eu8] => "Status",
    [0u8, 0x0Fu8] => "Collection",
    [0u8, 0x10u8] => "Class",
    [0u8, 0x12u8] => "CollectionId",
    [0u8, 0x13u8] => "GetChanges",
    [0u8, 0x14u8] => "MoreAvailable",
    [0u8, 0x15u8] => "WindowSize",
    [0u8, 0x16u8] => "Commands",
    [0u8, 0x17u8] => "Options",
    [0u8, 0x18u8] => "FilterType",
    [0u8, 0x1Bu8] => "Conflict",
    [0u8, 0x1Cu8] => "Collections",
    [0u8, 0x1Du8] => "ApplicationData",
    [0u8, 0x1Eu8] => "DeletesAsMoves",
    [0u8, 0x20u8] => "Supported",
    [0u8, 0x21u8] => "SoftDelete",
    [0u8, 0x22u8] => "MIMESupport",
    [0u8, 0x23u8] => "MIMETruncation",
    [0u8, 0x24u8] => "Wait",
    [0u8, 0x25u8] => "Limit",
    [0u8, 0x26u8] => "Partial",
    [0u8, 0x27u8] => "ConversationMode",
    [0u8, 0x28u8] => "MaxItems",
    [0u8, 0x29u8] => "HeartbeatInterval",
    [1u8, 0x05u8] => "Contacts:Anniversary",
    [1u8, 0x06u8] => "Contacts:AssistantName",
    [1u8, 0x07u8] => "Contacts:AssistantPhoneNumber",
    [1u8, 0x08u8] => "Contacts:Birthday",
    [1u8, 0x13u8] => "Contacts:BusinessPhoneNumber",
    [1u8, 0x19u8] => "Contacts:CompanyName",
    [1u8, 0x1Bu8] => "Contacts:Email1Address",
    [1u8, 0x1Cu8] => "Contacts:Email2Address",
    [1u8, 0x1Du8] => "Contacts:Email3Address",
    [1u8, 0x1Fu8] => "Contacts:FirstName",
    [1u8, 0x21u8] => "Contacts:HomeCity",
    [1u8, 0x22u8] => "Contacts:HomeCountry",
    [1u8, 0x26u8] => "Contacts:HomePhoneNumber",
    [1u8, 0x29u8] => "Contacts:LastName",
    [1u8, 0x2Bu8] => "Contacts:MobilePhoneNumber",
    [1u8, 0x2Fu8] => "Contacts:Suffix",
    [1u8, 0x30u8] => "Contacts:Title",
    [1u8, 0x33u8] => "Contacts:JobTitle",
    [1u8, 0x35u8] => "Contacts:MiddleName",
    [1u8, 0x37u8] => "Contacts:NickName",
    [1u8, 0x39u8] => "Contacts:OfficeLocation",
    [1u8, 0x45u8] => "Contacts:WebPage",
    [1u8, 0x47u8] => "Contacts:YomiCompanyName",
    [1u8, 0x48u8] => "Contacts:YomiFirstName",
    [1u8, 0x49u8] => "Contacts:YomiLastName",
    [2u8, 0x05u8] => "Email:Attachment",
    [2u8, 0x06u8] => "Email:Attachments",
    [2u8, 0x07u8] => "Email:AttName",
    [2u8, 0x08u8] => "Email:AttSize",
    [2u8, 0x0Cu8] => "Email:Body",
    [2u8, 0x0Eu8] => "Email:DateReceived",
    [2u8, 0x11u8] => "Email:DisplayTo",
    [2u8, 0x14u8] => "Email:Subject",
    [2u8, 0x15u8] => "Email:Read",
    [2u8, 0x16u8] => "Email:To",
    [2u8, 0x17u8] => "Email:Cc",
    [2u8, 0x18u8] => "Email:From",
    [2u8, 0x19u8] => "Email:Reply-To",
    [2u8, 0x1Au8] => "Email:AllDayEvent",
    [2u8, 0x1Bu8] => "Email:Categories",
    [2u8, 0x1Cu8] => "Email:Category",
    [2u8, 0x1Du8] => "Email:DtStamp",
    [2u8, 0x1Eu8] => "Email:EndTime",
    [2u8, 0x1Fu8] => "Email:InstanceType",
    [2u8, 0x20u8] => "Email:BusyStatus",
    [2u8, 0x24u8] => "Email:Location",
    [2u8, 0x25u8] => "Email:MeetingRequest",
    [2u8, 0x26u8] => "Email:Organizer",
    [2u8, 0x28u8] => "Email:Recurrence",
    [2u8, 0x2Bu8] => "Email:Reminder",
    [2u8, 0x2Cu8] => "Email:RequiredAttendees",
    [2u8, 0x2Du8] => "Email:OptionalAttendees",
    [2u8, 0x2Eu8] => "Email:ResourceAttendees",
    [2u8, 0x2Fu8] => "Email:ResponseRequested",
    [2u8, 0x30u8] => "Email:Sensitivity",
    [2u8, 0x31u8] => "Email:StartTime",
    [2u8, 0x32u8] => "Email:Timezone",
    [2u8, 0x33u8] => "Email:GlobalObjId",
    [2u8, 0x34u8] => "Email:ThreadTopic",
    [2u8, 0x39u8] => "Email:InternetCPID",
    [2u8, 0x3Au8] => "Email:Flag",
    [2u8, 0x3Bu8] => "Email:FlagStatus",
    [2u8, 0x3Cu8] => "Email:ContentClass",
    [2u8, 0x3Du8] => "Email:FlagType",
    [2u8, 0x3Eu8] => "Email:CompleteTime",
    [2u8, 0x40u8] => "Email:DisallowNewTimeProposal",
    [4u8, 0x05u8] => "Calendar:Timezone",
    [4u8, 0x06u8] => "Calendar:AllDayEvent",
    [4u8, 0x07u8] => "Calendar:Attendees",
    [4u8, 0x08u8] => "Calendar:Attendee",
    [4u8, 0x09u8] => "Calendar:Email",
    [4u8, 0x0Au8] => "Calendar:Name",
    [4u8, 0x0Bu8] => "Calendar:Body",
    [4u8, 0x0Du8] => "Calendar:BusyStatus",
    [4u8, 0x0Eu8] => "Calendar:Categories",
    [4u8, 0x0Fu8] => "Calendar:Category",
    [4u8, 0x11u8] => "Calendar:DtStamp",
    [4u8, 0x12u8] => "Calendar:EndTime",
    [4u8, 0x13u8] => "Calendar:Exception",
    [4u8, 0x14u8] => "Calendar:Exceptions",
    [4u8, 0x15u8] => "Calendar:Deleted",
    [4u8, 0x16u8] => "Calendar:ExceptionStartTime",
    [4u8, 0x17u8] => "Calendar:Location",
    [4u8, 0x18u8] => "Calendar:MeetingStatus",
    [4u8, 0x19u8] => "Calendar:OrganizerEmail",
    [4u8, 0x1Au8] => "Calendar:OrganizerName",
    [4u8, 0x1Bu8] => "Calendar:Recurrence",
    [4u8, 0x1Cu8] => "Calendar:Type",
    [4u8, 0x1Du8] => "Calendar:Until",
    [4u8, 0x1Eu8] => "Calendar:Occurrences",
    [4u8, 0x1Fu8] => "Calendar:Interval",
    [4u8, 0x20u8] => "Calendar:DayOfWeek",
    [4u8, 0x21u8] => "Calendar:DayOfMonth",
    [4u8, 0x22u8] => "Calendar:WeekOfMonth",
    [4u8, 0x23u8] => "Calendar:MonthOfYear",
    [4u8, 0x24u8] => "Calendar:Reminder",
    [4u8, 0x25u8] => "Calendar:Sensitivity",
    [4u8, 0x26u8] => "Calendar:Subject",
    [4u8, 0x27u8] => "Calendar:StartTime",
    [4u8, 0x28u8] => "Calendar:UID",
    [4u8, 0x29u8] => "Calendar:AttendeeStatus",
    [4u8, 0x2Au8] => "Calendar:AttendeeType",
    [4u8, 0x33u8] => "Calendar:DisallowNewTimeProposal",
    [4u8, 0x34u8] => "Calendar:ResponseRequested",
    [4u8, 0x35u8] => "Calendar:AppointmentReplyTime",
    [4u8, 0x36u8] => "Calendar:ResponseType",
    [4u8, 0x37u8] => "Calendar:CalendarType",
    [4u8, 0x38u8] => "Calendar:IsLeapMonth",
    [4u8, 0x39u8] => "Calendar:FirstDayOfWeek",
    [4u8, 0x3Au8] => "Calendar:OnlineMeetingConfLink",
    [4u8, 0x3Bu8] => "Calendar:OnlineMeetingExternalLink",
    [4u8, 0x3Cu8] => "Calendar:ClientUid",
    [4u8, 0x3Du8] => "Calendar:StartTimeZoneId",
    [4u8, 0x3Eu8] => "Calendar:StartTimeZone",
    [4u8, 0x3Fu8] => "Calendar:EndTimeZone",
    [4u8, 0x40u8] => "Calendar:EndTimeZoneId",
    [5u8, 0x05u8] => "MoveItems",
    [5u8, 0x06u8] => "Move",
    [5u8, 0x07u8] => "SrcMsgId",
    [5u8, 0x08u8] => "SrcFldId",
    [5u8, 0x09u8] => "DstFldId",
    [5u8, 0x0Au8] => "MoveResponse",
    [5u8, 0x0Bu8] => "MoveStatus",
    [6u8, 0x05u8] => "GetItemEstimate",
    [6u8, 0x06u8] => "GIEVersion",
    [6u8, 0x07u8] => "GIECollections",
    [6u8, 0x08u8] => "GIECollection",
    [6u8, 0x09u8] => "GIEClass",
    [6u8, 0x0Au8] => "GIECollectionId",
    [6u8, 0x0Bu8] => "DateTime",
    [6u8, 0x0Cu8] => "Estimate",
    [6u8, 0x0Du8] => "Response",
    [6u8, 0x0Eu8] => "Status",
    [7u8, 0x07u8] => "DisplayName",
    [7u8, 0x08u8] => "ServerId",
    [7u8, 0x09u8] => "ParentId",
    [7u8, 0x0Au8] => "Type",
    [7u8, 0x0Cu8] => "Status",
    [7u8, 0x0Eu8] => "Changes",
    [7u8, 0x0Fu8] => "Add",
    [7u8, 0x10u8] => "Delete",
    [7u8, 0x11u8] => "Update",
    [7u8, 0x12u8] => "SyncKey",
    [7u8, 0x13u8] => "FolderCreate",
    [7u8, 0x14u8] => "FolderDelete",
    [7u8, 0x15u8] => "FolderUpdate",
    [7u8, 0x16u8] => "FolderSync",
    [7u8, 0x17u8] => "Count",
    [8u8, 0x05u8] => "CalendarId",
    [8u8, 0x06u8] => "MeetingCollectionId",
    [8u8, 0x07u8] => "MeetingResponse",
    [8u8, 0x08u8] => "RequestId",
    [8u8, 0x09u8] => "Request",
    [8u8, 0x0Au8] => "Result",
    [8u8, 0x0Bu8] => "Status",
    [8u8, 0x0Cu8] => "UserResponse",
    [8u8, 0x0Eu8] => "InstanceId",
    [8u8, 0x0Fu8] => "LongId",
    [8u8, 0x10u8] => "ProposedStartTime",
    [8u8, 0x11u8] => "ProposedEndTime",
    [8u8, 0x12u8] => "SendResponse",
    [9u8, 0x08u8] => "Tasks:Complete",
    [9u8, 0x09u8] => "Tasks:DateCompleted",
    [9u8, 0x0Du8] => "Tasks:DueDate",
    [9u8, 0x0Fu8] => "Tasks:Importance",
    [9u8, 0x17u8] => "Tasks:StartDate",
    [9u8, 0x18u8] => "Tasks:Subject",
    [9u8, 0x19u8] => "Tasks:ReminderSet",
    [9u8, 0x1Au8] => "Tasks:ReminderTime",
    [9u8, 0x1Bu8] => "Tasks:Sensitivity",
    [9u8, 0x1Cu8] => "Tasks:Recurrence",
    [9u8, 0x1Du8] => "Tasks:Type",
    [9u8, 0x1Eu8] => "Tasks:Start",
    [9u8, 0x1Fu8] => "Tasks:Until",
    [9u8, 0x20u8] => "Tasks:Occurrences",
    [9u8, 0x21u8] => "Tasks:Interval",
    [9u8, 0x22u8] => "Tasks:DayOfWeek",
    [9u8, 0x23u8] => "Tasks:DayOfMonth",
    [9u8, 0x24u8] => "Tasks:WeekOfMonth",
    [9u8, 0x25u8] => "Tasks:MonthOfYear",
    [9u8, 0x26u8] => "Tasks:Regenerate",
    [9u8, 0x27u8] => "Tasks:DeadOccur",
    [9u8, 0x28u8] => "Tasks:Categories",
    [9u8, 0x29u8] => "Tasks:Category",
    [10u8, 0x05u8] => "ResolveRecipients",
    [10u8, 0x06u8] => "Response",
    [10u8, 0x07u8] => "Status",
    [10u8, 0x08u8] => "Type",
    [10u8, 0x09u8] => "Recipient",
    [10u8, 0x0Au8] => "DisplayName",
    [10u8, 0x0Bu8] => "EmailAddress",
    [10u8, 0x0Cu8] => "Certificates",
    [10u8, 0x0Du8] => "Certificate",
    [10u8, 0x0Eu8] => "MiniCertificate",
    [10u8, 0x0Fu8] => "Options",
    [10u8, 0x10u8] => "To",
    [10u8, 0x11u8] => "CertificateRetrieval",
    [10u8, 0x12u8] => "RecipientCount",
    [10u8, 0x13u8] => "MaxCertificates",
    [10u8, 0x14u8] => "MaxAmbiguousRecipients",
    [10u8, 0x15u8] => "CertificateCount",
    [10u8, 0x16u8] => "Availability",
    [10u8, 0x17u8] => "StartTime",
    [10u8, 0x18u8] => "EndTime",
    [10u8, 0x19u8] => "MergedFreeBusy",
    [10u8, 0x1Au8] => "Picture",
    [10u8, 0x1Bu8] => "MaxSize",
    [10u8, 0x1Cu8] => "Data",
    [10u8, 0x1Du8] => "MaxPictures",
    [11u8, 0x05u8] => "ValidateCert",
    [11u8, 0x06u8] => "Certificates",
    [11u8, 0x07u8] => "Certificate",
    [11u8, 0x08u8] => "CertificateChain",
    [11u8, 0x09u8] => "CheckCRL",
    [11u8, 0x0Au8] => "CertificateStatus",
    [11u8, 0x0Bu8] => "Status",
    [12u8, 0x05u8] => "Contacts2:CustomerId",
    [12u8, 0x06u8] => "Contacts2:GovernmentId",
    [12u8, 0x07u8] => "Contacts2:IMAddress",
    [12u8, 0x08u8] => "Contacts2:IMAddress2",
    [12u8, 0x09u8] => "Contacts2:IMAddress3",
    [12u8, 0x0Au8] => "Contacts2:ManagerName",
    [12u8, 0x0Bu8] => "Contacts2:CompanyMainPhone",
    [12u8, 0x0Cu8] => "Contacts2:AccountName",
    [12u8, 0x0Du8] => "Contacts2:MMS",
    [12u8, 0x0Eu8] => "Contacts2:NickName",
    [13u8, 0x05u8] => "Ping",
    [13u8, 0x07u8] => "Status",
    [13u8, 0x08u8] => "HeartbeatInterval",
    [13u8, 0x09u8] => "Folders",
    [13u8, 0x0Au8] => "Folder",
    [13u8, 0x0Bu8] => "Id",
    [13u8, 0x0Cu8] => "Class",
    [13u8, 0x0Du8] => "MaxFolders",
    [14u8, 0x05u8] => "Provision",
    [14u8, 0x06u8] => "Policies",
    [14u8, 0x07u8] => "Policy",
    [14u8, 0x08u8] => "PolicyType",
    [14u8, 0x09u8] => "PolicyKey",
    [14u8, 0x0Au8] => "Data",
    [14u8, 0x0Bu8] => "Status",
    [14u8, 0x0Cu8] => "RemoteWipe",
    [14u8, 0x0Du8] => "EASProvisionDoc",
    [14u8, 0x0Eu8] => "DevicePasswordEnabled",
    [14u8, 0x0Fu8] => "AlphanumericDevicePasswordRequired",
    [14u8, 0x10u8] => "RequireStorageCardEncryption",
    [14u8, 0x11u8] => "PasswordRecoveryEnabled",
    [14u8, 0x13u8] => "AttachmentsEnabled",
    [14u8, 0x14u8] => "MinDevicePasswordLength",
    [14u8, 0x15u8] => "MaxInactivityTimeDeviceLock",
    [14u8, 0x16u8] => "MaxDevicePasswordFailedAttempts",
    [14u8, 0x17u8] => "MaxAttachmentSize",
    [14u8, 0x18u8] => "AllowSimpleDevicePassword",
    [14u8, 0x19u8] => "DevicePasswordExpiration",
    [14u8, 0x1Au8] => "DevicePasswordHistory",
    [14u8, 0x1Bu8] => "AllowStorageCard",
    [14u8, 0x1Cu8] => "AllowCamera",
    [14u8, 0x1Du8] => "RequireDeviceEncryption",
    [14u8, 0x1Eu8] => "AllowUnsignedApplications",
    [14u8, 0x1Fu8] => "AllowUnsignedInstallationPackages",
    [14u8, 0x20u8] => "MinDevicePasswordComplexCharacters",
    [14u8, 0x21u8] => "AllowWifi",
    [14u8, 0x22u8] => "AllowTextMessaging",
    [14u8, 0x23u8] => "AllowPOPIMAPEmail",
    [14u8, 0x24u8] => "AllowBluetooth",
    [14u8, 0x25u8] => "AllowIrDA",
    [14u8, 0x26u8] => "RequireManualSyncWhenRoaming",
    [14u8, 0x27u8] => "AllowDesktopSync",
    [14u8, 0x28u8] => "MaxCalendarAgeFilter",
    [14u8, 0x29u8] => "AllowHTMLEmail",
    [14u8, 0x2Au8] => "MaxEmailAgeFilter",
    [14u8, 0x2Bu8] => "MaxEmailBodyTruncationSize",
    [14u8, 0x2Cu8] => "MaxEmailHTMLBodyTruncationSize",
    [14u8, 0x2Du8] => "RequireSignedSMIMEMessages",
    [14u8, 0x2Eu8] => "RequireEncryptedSMIMEMessages",
    [14u8, 0x2Fu8] => "RequireSignedSMIMEAlgorithm",
    [14u8, 0x30u8] => "RequireEncryptionSMIMEAlgorithm",
    [14u8, 0x31u8] => "AllowSMIMEEncryptionAlgorithmNegotiation",
    [14u8, 0x32u8] => "AllowSMIMESoftCerts",
    [14u8, 0x33u8] => "AllowBrowser",
    [14u8, 0x34u8] => "AllowConsumerEmail",
    [14u8, 0x35u8] => "AllowRemoteDesktop",
    [14u8, 0x36u8] => "AllowInternetSharing",
    [14u8, 0x37u8] => "UnapprovedInROMApplicationList",
    [14u8, 0x38u8] => "ApplicationName",
    [14u8, 0x39u8] => "ApprovedApplicationList",
    [14u8, 0x3Au8] => "Hash",
    [15u8, 0x05u8] => "Search",
    [15u8, 0x07u8] => "Store",
    [15u8, 0x08u8] => "Name",
    [15u8, 0x09u8] => "Query",
    [15u8, 0x0Au8] => "Options",
    [15u8, 0x0Bu8] => "Range",
    [15u8, 0x0Cu8] => "Status",
    [15u8, 0x0Du8] => "Response",
    [15u8, 0x0Eu8] => "Result",
    [15u8, 0x0Fu8] => "Properties",
    [15u8, 0x10u8] => "Total",
    [15u8, 0x11u8] => "EqualTo",
    [15u8, 0x12u8] => "Value",
    [15u8, 0x13u8] => "And",
    [15u8, 0x14u8] => "Or",
    [15u8, 0x15u8] => "FreeText",
    [15u8, 0x17u8] => "DeepTraversal",
    [15u8, 0x18u8] => "LongId",
    [15u8, 0x19u8] => "RebuildResults",
    [15u8, 0x1Au8] => "LeafName",
    [15u8, 0x1Bu8] => "Class",
    [15u8, 0x1Cu8] => "CollectionId",
    [15u8, 0x1Du8] => "QueryId",
    [15u8, 0x1Eu8] => "MaxResults",
    [16u8, 0x05u8] => "GAL:DisplayName",
    [16u8, 0x06u8] => "GAL:Phone",
    [16u8, 0x07u8] => "GAL:Office",
    [16u8, 0x08u8] => "GAL:Title",
    [16u8, 0x09u8] => "GAL:Company",
    [16u8, 0x0Au8] => "GAL:Alias",
    [16u8, 0x0Bu8] => "GAL:FirstName",
    [16u8, 0x0Cu8] => "GAL:LastName",
    [16u8, 0x0Du8] => "GAL:HomePhone",
    [16u8, 0x0Eu8] => "GAL:MobilePhone",
    [16u8, 0x0Fu8] => "GAL:EmailAddress",
    [16u8, 0x10u8] => "GAL:Picture",
    [16u8, 0x11u8] => "GAL:Status",
    [16u8, 0x12u8] => "GAL:Data",
    [17u8, 0x05u8] => "AirSyncBase:BodyPreference",
    [17u8, 0x06u8] => "AirSyncBase:Type",
    [17u8, 0x07u8] => "AirSyncBase:TruncationSize",
    [17u8, 0x08u8] => "AirSyncBase:AllOrNone",
    [17u8, 0x0Au8] => "AirSyncBase:Body",
    [17u8, 0x0Bu8] => "AirSyncBase:Data",
    [17u8, 0x0Cu8] => "AirSyncBase:EstimatedDataSize",
    [17u8, 0x0Du8] => "AirSyncBase:Truncated",
    [17u8, 0x0Eu8] => "AirSyncBase:Attachments",
    [17u8, 0x0Fu8] => "AirSyncBase:Attachment",
    [17u8, 0x10u8] => "AirSyncBase:DisplayName",
    [17u8, 0x11u8] => "AirSyncBase:FileReference",
    [17u8, 0x12u8] => "AirSyncBase:Method",
    [17u8, 0x13u8] => "AirSyncBase:ContentId",
    [17u8, 0x14u8] => "AirSyncBase:ContentLocation",
    [17u8, 0x15u8] => "AirSyncBase:IsInline",
    [17u8, 0x16u8] => "AirSyncBase:NativeBodyType",
    [17u8, 0x17u8] => "AirSyncBase:ContentType",
    [17u8, 0x18u8] => "AirSyncBase:Preview",
    [17u8, 0x19u8] => "AirSyncBase:BodyPartPreference",
    [17u8, 0x1Au8] => "AirSyncBase:BodyPart",
    [17u8, 0x1Bu8] => "AirSyncBase:Status",
    [17u8, 0x1Cu8] => "AirSyncBase:Add",
    [17u8, 0x1Du8] => "AirSyncBase:Delete",
    [17u8, 0x1Eu8] => "AirSyncBase:ClientId",
    [17u8, 0x1Fu8] => "AirSyncBase:Content",
    [17u8, 0x20u8] => "AirSyncBase:Location",
    [17u8, 0x21u8] => "AirSyncBase:Annotation",
    [17u8, 0x22u8] => "AirSyncBase:Street",
    [17u8, 0x23u8] => "AirSyncBase:City",
    [17u8, 0x24u8] => "AirSyncBase:State",
    [17u8, 0x25u8] => "AirSyncBase:Country",
    [17u8, 0x26u8] => "AirSyncBase:PostalCode",
    [17u8, 0x27u8] => "AirSyncBase:Latitude",
    [17u8, 0x28u8] => "AirSyncBase:Longitude",
    [17u8, 0x29u8] => "AirSyncBase:Accuracy",
    [17u8, 0x2Au8] => "AirSyncBase:Altitude",
    [17u8, 0x2Bu8] => "AirSyncBase:AltitudeAccuracy",
    [17u8, 0x2Cu8] => "AirSyncBase:LocationUri",
    [17u8, 0x2Du8] => "AirSyncBase:InstanceId",
    [18u8, 0x05u8] => "Settings",
    [18u8, 0x06u8] => "Status",
    [18u8, 0x07u8] => "Get",
    [18u8, 0x08u8] => "Set",
    [18u8, 0x09u8] => "Oof",
    [18u8, 0x0Au8] => "OofState",
    [18u8, 0x0Bu8] => "StartTime",
    [18u8, 0x0Cu8] => "EndTime",
    [18u8, 0x0Du8] => "OofMessage",
    [18u8, 0x0Eu8] => "AppliesToInternal",
    [18u8, 0x0Fu8] => "AppliesToExternalKnown",
    [18u8, 0x10u8] => "AppliesToExternalUnknown",
    [18u8, 0x11u8] => "Enabled",
    [18u8, 0x12u8] => "ReplyMessage",
    [18u8, 0x13u8] => "BodyType",
    [18u8, 0x14u8] => "DevicePassword",
    [18u8, 0x15u8] => "Password",
    [18u8, 0x16u8] => "DeviceInformation",
    [18u8, 0x17u8] => "Model",
    [18u8, 0x18u8] => "IMEI",
    [18u8, 0x19u8] => "FriendlyName",
    [18u8, 0x1Au8] => "OS",
    [18u8, 0x1Bu8] => "OSLanguage",
    [18u8, 0x1Cu8] => "PhoneNumber",
    [18u8, 0x1Du8] => "UserInformation",
    [18u8, 0x1Eu8] => "EmailAddresses",
    [18u8, 0x1Fu8] => "SMTPAddress",
    [18u8, 0x20u8] => "UserAgent",
    [18u8, 0x21u8] => "EnableOutboundSMS",
    [18u8, 0x22u8] => "MobileOperator",
    [18u8, 0x23u8] => "PrimarySmtpAddress",
    [18u8, 0x24u8] => "Accounts",
    [18u8, 0x25u8] => "Account",
    [18u8, 0x26u8] => "AccountId",
    [18u8, 0x27u8] => "AccountName",
    [18u8, 0x28u8] => "UserDisplayName",
    [18u8, 0x29u8] => "SendDisabled",
    [18u8, 0x2Bu8] => "RightsManagementInformation",
    [19u8, 0x05u8] => "DocumentLibrary:LinkId",
    [19u8, 0x06u8] => "DocumentLibrary:DisplayName",
    [19u8, 0x07u8] => "DocumentLibrary:IsFolder",
    [19u8, 0x08u8] => "DocumentLibrary:CreationDate",
    [19u8, 0x09u8] => "DocumentLibrary:LastModifiedDate",
    [19u8, 0x0Au8] => "DocumentLibrary:IsHidden",
    [19u8, 0x0Bu8] => "DocumentLibrary:ContentLength",
    [19u8, 0x0Cu8] => "DocumentLibrary:ContentType",
    [20u8, 0x05u8] => "ItemOperations",
    [20u8, 0x06u8] => "Fetch",
    [20u8, 0x07u8] => "Store",
    [20u8, 0x08u8] => "Options",
    [20u8, 0x09u8] => "Range",
    [20u8, 0x0Au8] => "Total",
    [20u8, 0x0Bu8] => "Properties",
    [20u8, 0x0Cu8] => "Data",
    [20u8, 0x0Du8] => "Status",
    [20u8, 0x0Eu8] => "Response",
    [20u8, 0x0Fu8] => "Version",
    [20u8, 0x10u8] => "Schema",
    [20u8, 0x11u8] => "Part",
    [20u8, 0x12u8] => "EmptyFolderContents",
    [20u8, 0x13u8] => "DeleteSubFolders",
    [20u8, 0x14u8] => "UserName",
    [20u8, 0x15u8] => "IOPassword",
    [20u8, 0x16u8] => "Move",
    [20u8, 0x17u8] => "DstFldId",
    [20u8, 0x18u8] => "ConversationId",
    [20u8, 0x19u8] => "MoveAlways",
    [21u8, 0x05u8] => "SendMail",
    [21u8, 0x06u8] => "SmartForward",
    [21u8, 0x07u8] => "SmartReply",
    [21u8, 0x08u8] => "SaveInSentItems",
    [21u8, 0x09u8] => "ReplaceMime",
    [21u8, 0x0Bu8] => "Mime",
    [21u8, 0x0Cu8] => "ClientId",
    [21u8, 0x0Du8] => "Status",
    [21u8, 0x0Eu8] => "AccountId",
    [21u8, 0x0Fu8] => "Forwardees",
    [21u8, 0x10u8] => "Forwardee",
    [21u8, 0x11u8] => "ForwardeeName",
    [21u8, 0x12u8] => "ForwardeeEmail",
    [22u8, 0x05u8] => "Email2:UmCallerId",
    [22u8, 0x06u8] => "Email2:UmUserNotes",
    [22u8, 0x07u8] => "Email2:UmAttDuration",
    [22u8, 0x08u8] => "Email2:UmAttOrder",
    [22u8, 0x09u8] => "Email2:ConversationId",
    [22u8, 0x0Au8] => "Email2:ConversationIndex",
    [22u8, 0x0Bu8] => "Email2:LastVerbExecuted",
    [22u8, 0x0Cu8] => "Email2:LastVerbExecutionTime",
    [22u8, 0x0Du8] => "Email2:ReceivedAsBcc",
    [22u8, 0x0Eu8] => "Email2:Sender",
    [22u8, 0x0Fu8] => "Email2:CalendarType",
    [22u8, 0x10u8] => "Email2:IsLeapMonth",
    [22u8, 0x11u8] => "Email2:AccountId",
    [22u8, 0x12u8] => "Email2:FirstDayOfWeek",
    [22u8, 0x13u8] => "Email2:MeetingMessageType",
    [23u8, 0x05u8] => "Notes:Subject",
    [23u8, 0x06u8] => "Notes:MessageClass",
    [23u8, 0x07u8] => "Notes:LastModifiedDate",
    [23u8, 0x08u8] => "Notes:Categories",
    [23u8, 0x09u8] => "Notes:Category",
    [23u8, 0x0Bu8] => "Notes:Body",
    [24u8, 0x05u8] => "RightsManagement:RightsManagementSupport",
    [24u8, 0x06u8] => "RightsManagement:RightsManagementTemplates",
    [24u8, 0x07u8] => "RightsManagement:RightsManagementTemplate",
    [24u8, 0x08u8] => "RightsManagement:RightsManagementLicense",
    [24u8, 0x09u8] => "RightsManagement:EditAllowed",
    [24u8, 0x0Au8] => "RightsManagement:ReplyAllowed",
    [24u8, 0x0Bu8] => "RightsManagement:ReplyAllAllowed",
    [24u8, 0x0Cu8] => "RightsManagement:ForwardAllowed",
    [24u8, 0x0Du8] => "RightsManagement:ModifyRecipientsAllowed",
    [24u8, 0x0Eu8] => "RightsManagement:ExtractAllowed",
    [24u8, 0x0Fu8] => "RightsManagement:PrintAllowed",
    [24u8, 0x10u8] => "RightsManagement:ExportAllowed",
    [24u8, 0x11u8] => "RightsManagement:ProgrammaticAccessAllowed",
    [24u8, 0x12u8] => "RightsManagement:RMOwner",
    [24u8, 0x13u8] => "RightsManagement:ContentExpiryDate",
    [24u8, 0x14u8] => "RightsManagement:ContentExpiryDateString",
    [24u8, 0x15u8] => "RightsManagement:ContentExpiryInterval",
    [24u8, 0x16u8] => "RightsManagement:ContentExpiryIntervalType",
    [24u8, 0x17u8] => "RightsManagement:TemplateID",
    [24u8, 0x18u8] => "RightsManagement:TemplateName",
    [24u8, 0x19u8] => "RightsManagement:TemplateDescription",
    [24u8, 0x1Au8] => "RightsManagement:ContentOwner",
    [24u8, 0x1Bu8] => "RightsManagement:RemoveRightsManagementDistribution",
};

static NAME_TO_TAG: phf::Map<&'static str, [u8; 2]> = phf::phf_map! {
    "Sync" => [0u8, 0x05u8],
    "Responses" => [0u8, 0x06u8],
    "Change" => [0u8, 0x08u8],
    "Collection" => [0u8, 0x0Fu8],
    "GetChanges" => [0u8, 0x13u8],
    "MoreAvailable" => [0u8, 0x14u8],
    "WindowSize" => [0u8, 0x15u8],
    "Commands" => [0u8, 0x16u8],
    "FilterType" => [0u8, 0x18u8],
    "Conflict" => [0u8, 0x1Bu8],
    "Collections" => [0u8, 0x1Cu8],
    "ApplicationData" => [0u8, 0x1Du8],
    "DeletesAsMoves" => [0u8, 0x1Eu8],
    "Supported" => [0u8, 0x20u8],
    "SoftDelete" => [0u8, 0x21u8],
    "MIMESupport" => [0u8, 0x22u8],
    "MIMETruncation" => [0u8, 0x23u8],
    "Wait" => [0u8, 0x24u8],
    "Limit" => [0u8, 0x25u8],
    "Partial" => [0u8, 0x26u8],
    "ConversationMode" => [0u8, 0x27u8],
    "MaxItems" => [0u8, 0x28u8],
    "Contacts:Anniversary" => [1u8, 0x05u8],
    "Contacts:AssistantName" => [1u8, 0x06u8],
    "Contacts:AssistantPhoneNumber" => [1u8, 0x07u8],
    "Contacts:Birthday" => [1u8, 0x08u8],
    "Contacts:BusinessPhoneNumber" => [1u8, 0x13u8],
    "Contacts:CompanyName" => [1u8, 0x19u8],
    "Contacts:Email1Address" => [1u8, 0x1Bu8],
    "Contacts:Email2Address" => [1u8, 0x1Cu8],
    "Contacts:Email3Address" => [1u8, 0x1Du8],
    "Contacts:FirstName" => [1u8, 0x1Fu8],
    "Contacts:HomeCity" => [1u8, 0x21u8],
    "Contacts:HomeCountry" => [1u8, 0x22u8],
    "Contacts:HomePhoneNumber" => [1u8, 0x26u8],
    "Contacts:LastName" => [1u8, 0x29u8],
    "Contacts:MobilePhoneNumber" => [1u8, 0x2Bu8],
    "Contacts:Suffix" => [1u8, 0x2Fu8],
    "Contacts:Title" => [1u8, 0x30u8],
    "Contacts:JobTitle" => [1u8, 0x33u8],
    "Contacts:MiddleName" => [1u8, 0x35u8],
    "Contacts:NickName" => [1u8, 0x37u8],
    "Contacts:OfficeLocation" => [1u8, 0x39u8],
    "Contacts:WebPage" => [1u8, 0x45u8],
    "Contacts:YomiCompanyName" => [1u8, 0x47u8],
    "Contacts:YomiFirstName" => [1u8, 0x48u8],
    "Contacts:YomiLastName" => [1u8, 0x49u8],
    "Email:Attachment" => [2u8, 0x05u8],
    "Email:Attachments" => [2u8, 0x06u8],
    "Email:AttName" => [2u8, 0x07u8],
    "Email:AttSize" => [2u8, 0x08u8],
    "Email:Body" => [2u8, 0x0Cu8],
    "Email:DateReceived" => [2u8, 0x0Eu8],
    "Email:DisplayTo" => [2u8, 0x11u8],
    "Email:Subject" => [2u8, 0x14u8],
    "Email:Read" => [2u8, 0x15u8],
    "Email:To" => [2u8, 0x16u8],
    "Email:Cc" => [2u8, 0x17u8],
    "Email:From" => [2u8, 0x18u8],
    "Email:Reply-To" => [2u8, 0x19u8],
    "Email:AllDayEvent" => [2u8, 0x1Au8],
    "Email:Categories" => [2u8, 0x1Bu8],
    "Email:Category" => [2u8, 0x1Cu8],
    "Email:DtStamp" => [2u8, 0x1Du8],
    "Email:EndTime" => [2u8, 0x1Eu8],
    "Email:InstanceType" => [2u8, 0x1Fu8],
    "Email:BusyStatus" => [2u8, 0x20u8],
    "Email:Location" => [2u8, 0x24u8],
    "Email:MeetingRequest" => [2u8, 0x25u8],
    "Email:Organizer" => [2u8, 0x26u8],
    "Email:Recurrence" => [2u8, 0x28u8],
    "Email:Reminder" => [2u8, 0x2Bu8],
    "Email:RequiredAttendees" => [2u8, 0x2Cu8],
    "Email:OptionalAttendees" => [2u8, 0x2Du8],
    "Email:ResourceAttendees" => [2u8, 0x2Eu8],
    "Email:ResponseRequested" => [2u8, 0x2Fu8],
    "Email:Sensitivity" => [2u8, 0x30u8],
    "Email:StartTime" => [2u8, 0x31u8],
    "Email:Timezone" => [2u8, 0x32u8],
    "Email:GlobalObjId" => [2u8, 0x33u8],
    "Email:ThreadTopic" => [2u8, 0x34u8],
    "Email:InternetCPID" => [2u8, 0x39u8],
    "Email:Flag" => [2u8, 0x3Au8],
    "Email:FlagStatus" => [2u8, 0x3Bu8],
    "Email:ContentClass" => [2u8, 0x3Cu8],
    "Email:FlagType" => [2u8, 0x3Du8],
    "Email:CompleteTime" => [2u8, 0x3Eu8],
    "Email:DisallowNewTimeProposal" => [2u8, 0x40u8],
    "Calendar:Timezone" => [4u8, 0x05u8],
    "Calendar:AllDayEvent" => [4u8, 0x06u8],
    "Calendar:Attendees" => [4u8, 0x07u8],
    "Calendar:Attendee" => [4u8, 0x08u8],
    "Calendar:Email" => [4u8, 0x09u8],
    "Calendar:Name" => [4u8, 0x0Au8],
    "Calendar:Body" => [4u8, 0x0Bu8],
    "Calendar:BusyStatus" => [4u8, 0x0Du8],
    "Calendar:Categories" => [4u8, 0x0Eu8],
    "Calendar:Category" => [4u8, 0x0Fu8],
    "Calendar:DtStamp" => [4u8, 0x11u8],
    "Calendar:EndTime" => [4u8, 0x12u8],
    "Calendar:Exception" => [4u8, 0x13u8],
    "Calendar:Exceptions" => [4u8, 0x14u8],
    "Calendar:Deleted" => [4u8, 0x15u8],
    "Calendar:ExceptionStartTime" => [4u8, 0x16u8],
    "Calendar:Location" => [4u8, 0x17u8],
    "Calendar:MeetingStatus" => [4u8, 0x18u8],
    "Calendar:OrganizerEmail" => [4u8, 0x19u8],
    "Calendar:OrganizerName" => [4u8, 0x1Au8],
    "Calendar:Recurrence" => [4u8, 0x1Bu8],
    "Calendar:Type" => [4u8, 0x1Cu8],
    "Calendar:Until" => [4u8, 0x1Du8],
    "Calendar:Occurrences" => [4u8, 0x1Eu8],
    "Calendar:Interval" => [4u8, 0x1Fu8],
    "Calendar:DayOfWeek" => [4u8, 0x20u8],
    "Calendar:DayOfMonth" => [4u8, 0x21u8],
    "Calendar:WeekOfMonth" => [4u8, 0x22u8],
    "Calendar:MonthOfYear" => [4u8, 0x23u8],
    "Calendar:Reminder" => [4u8, 0x24u8],
    "Calendar:Sensitivity" => [4u8, 0x25u8],
    "Calendar:Subject" => [4u8, 0x26u8],
    "Calendar:StartTime" => [4u8, 0x27u8],
    "Calendar:UID" => [4u8, 0x28u8],
    "Calendar:AttendeeStatus" => [4u8, 0x29u8],
    "Calendar:AttendeeType" => [4u8, 0x2Au8],
    "Calendar:DisallowNewTimeProposal" => [4u8, 0x33u8],
    "Calendar:ResponseRequested" => [4u8, 0x34u8],
    "Calendar:AppointmentReplyTime" => [4u8, 0x35u8],
    "Calendar:ResponseType" => [4u8, 0x36u8],
    "Calendar:CalendarType" => [4u8, 0x37u8],
    "Calendar:IsLeapMonth" => [4u8, 0x38u8],
    "Calendar:FirstDayOfWeek" => [4u8, 0x39u8],
    "Calendar:OnlineMeetingConfLink" => [4u8, 0x3Au8],
    "Calendar:OnlineMeetingExternalLink" => [4u8, 0x3Bu8],
    "Calendar:ClientUid" => [4u8, 0x3Cu8],
    "Calendar:StartTimeZoneId" => [4u8, 0x3Du8],
    "Calendar:StartTimeZone" => [4u8, 0x3Eu8],
    "Calendar:EndTimeZone" => [4u8, 0x3Fu8],
    "Calendar:EndTimeZoneId" => [4u8, 0x40u8],
    "MoveItems" => [5u8, 0x05u8],
    "SrcMsgId" => [5u8, 0x07u8],
    "SrcFldId" => [5u8, 0x08u8],
    "MoveResponse" => [5u8, 0x0Au8],
    "MoveStatus" => [5u8, 0x0Bu8],
    "GetItemEstimate" => [6u8, 0x05u8],
    "GIEVersion" => [6u8, 0x06u8],
    "GIECollections" => [6u8, 0x07u8],
    "GIECollection" => [6u8, 0x08u8],
    "GIEClass" => [6u8, 0x09u8],
    "GIECollectionId" => [6u8, 0x0Au8],
    "DateTime" => [6u8, 0x0Bu8],
    "Estimate" => [6u8, 0x0Cu8],
    "ServerId" => [7u8, 0x08u8],
    "ParentId" => [7u8, 0x09u8],
    "Changes" => [7u8, 0x0Eu8],
    "Add" => [7u8, 0x0Fu8],
    "Delete" => [7u8, 0x10u8],
    "Update" => [7u8, 0x11u8],
    "SyncKey" => [7u8, 0x12u8],
    "FolderCreate" => [7u8, 0x13u8],
    "FolderDelete" => [7u8, 0x14u8],
    "FolderUpdate" => [7u8, 0x15u8],
    "FolderSync" => [7u8, 0x16u8],
    "Count" => [7u8, 0x17u8],
    "CalendarId" => [8u8, 0x05u8],
    "MeetingCollectionId" => [8u8, 0x06u8],
    "MeetingResponse" => [8u8, 0x07u8],
    "RequestId" => [8u8, 0x08u8],
    "Request" => [8u8, 0x09u8],
    "UserResponse" => [8u8, 0x0Cu8],
    "InstanceId" => [8u8, 0x0Eu8],
    "ProposedStartTime" => [8u8, 0x10u8],
    "ProposedEndTime" => [8u8, 0x11u8],
    "SendResponse" => [8u8, 0x12u8],
    "Tasks:Complete" => [9u8, 0x08u8],
    "Tasks:DateCompleted" => [9u8, 0x09u8],
    "Tasks:DueDate" => [9u8, 0x0Du8],
    "Tasks:Importance" => [9u8, 0x0Fu8],
    "Tasks:StartDate" => [9u8, 0x17u8],
    "Tasks:Subject" => [9u8, 0x18u8],
    "Tasks:ReminderSet" => [9u8, 0x19u8],
    "Tasks:ReminderTime" => [9u8, 0x1Au8],
    "Tasks:Sensitivity" => [9u8, 0x1Bu8],
    "Tasks:Recurrence" => [9u8, 0x1Cu8],
    "Tasks:Type" => [9u8, 0x1Du8],
    "Tasks:Start" => [9u8, 0x1Eu8],
    "Tasks:Until" => [9u8, 0x1Fu8],
    "Tasks:Occurrences" => [9u8, 0x20u8],
    "Tasks:Interval" => [9u8, 0x21u8],
    "Tasks:DayOfWeek" => [9u8, 0x22u8],
    "Tasks:DayOfMonth" => [9u8, 0x23u8],
    "Tasks:WeekOfMonth" => [9u8, 0x24u8],
    "Tasks:MonthOfYear" => [9u8, 0x25u8],
    "Tasks:Regenerate" => [9u8, 0x26u8],
    "Tasks:DeadOccur" => [9u8, 0x27u8],
    "Tasks:Categories" => [9u8, 0x28u8],
    "Tasks:Category" => [9u8, 0x29u8],
    "ResolveRecipients" => [10u8, 0x05u8],
    "Type" => [10u8, 0x08u8],
    "Recipient" => [10u8, 0x09u8],
    "DisplayName" => [10u8, 0x0Au8],
    "EmailAddress" => [10u8, 0x0Bu8],
    "MiniCertificate" => [10u8, 0x0Eu8],
    "To" => [10u8, 0x10u8],
    "CertificateRetrieval" => [10u8, 0x11u8],
    "RecipientCount" => [10u8, 0x12u8],
    "MaxCertificates" => [10u8, 0x13u8],
    "MaxAmbiguousRecipients" => [10u8, 0x14u8],
    "CertificateCount" => [10u8, 0x15u8],
    "Availability" => [10u8, 0x16u8],
    "MergedFreeBusy" => [10u8, 0x19u8],
    "Picture" => [10u8, 0x1Au8],
    "MaxSize" => [10u8, 0x1Bu8],
    "MaxPictures" => [10u8, 0x1Du8],
    "ValidateCert" => [11u8, 0x05u8],
    "Certificates" => [11u8, 0x06u8],
    "Certificate" => [11u8, 0x07u8],
    "CertificateChain" => [11u8, 0x08u8],
    "CheckCRL" => [11u8, 0x09u8],
    "CertificateStatus" => [11u8, 0x0Au8],
    "Contacts2:CustomerId" => [12u8, 0x05u8],
    "Contacts2:GovernmentId" => [12u8, 0x06u8],
    "Contacts2:IMAddress" => [12u8, 0x07u8],
    "Contacts2:IMAddress2" => [12u8, 0x08u8],
    "Contacts2:IMAddress3" => [12u8, 0x09u8],
    "Contacts2:ManagerName" => [12u8, 0x0Au8],
    "Contacts2:CompanyMainPhone" => [12u8, 0x0Bu8],
    "Contacts2:AccountName" => [12u8, 0x0Cu8],
    "Contacts2:MMS" => [12u8, 0x0Du8],
    "Contacts2:NickName" => [12u8, 0x0Eu8],
    "Ping" => [13u8, 0x05u8],
    "HeartbeatInterval" => [13u8, 0x08u8],
    "Folders" => [13u8, 0x09u8],
    "Folder" => [13u8, 0x0Au8],
    "Id" => [13u8, 0x0Bu8],
    "MaxFolders" => [13u8, 0x0Du8],
    "Provision" => [14u8, 0x05u8],
    "Policies" => [14u8, 0x06u8],
    "Policy" => [14u8, 0x07u8],
    "PolicyType" => [14u8, 0x08u8],
    "PolicyKey" => [14u8, 0x09u8],
    "RemoteWipe" => [14u8, 0x0Cu8],
    "EASProvisionDoc" => [14u8, 0x0Du8],
    "DevicePasswordEnabled" => [14u8, 0x0Eu8],
    "AlphanumericDevicePasswordRequired" => [14u8, 0x0Fu8],
    "RequireStorageCardEncryption" => [14u8, 0x10u8],
    "PasswordRecoveryEnabled" => [14u8, 0x11u8],
    "AttachmentsEnabled" => [14u8, 0x13u8],
    "MinDevicePasswordLength" => [14u8, 0x14u8],
    "MaxInactivityTimeDeviceLock" => [14u8, 0x15u8],
    "MaxDevicePasswordFailedAttempts" => [14u8, 0x16u8],
    "MaxAttachmentSize" => [14u8, 0x17u8],
    "AllowSimpleDevicePassword" => [14u8, 0x18u8],
    "DevicePasswordExpiration" => [14u8, 0x19u8],
    "DevicePasswordHistory" => [14u8, 0x1Au8],
    "AllowStorageCard" => [14u8, 0x1Bu8],
    "AllowCamera" => [14u8, 0x1Cu8],
    "RequireDeviceEncryption" => [14u8, 0x1Du8],
    "AllowUnsignedApplications" => [14u8, 0x1Eu8],
    "AllowUnsignedInstallationPackages" => [14u8, 0x1Fu8],
    "MinDevicePasswordComplexCharacters" => [14u8, 0x20u8],
    "AllowWifi" => [14u8, 0x21u8],
    "AllowTextMessaging" => [14u8, 0x22u8],
    "AllowPOPIMAPEmail" => [14u8, 0x23u8],
    "AllowBluetooth" => [14u8, 0x24u8],
    "AllowIrDA" => [14u8, 0x25u8],
    "RequireManualSyncWhenRoaming" => [14u8, 0x26u8],
    "AllowDesktopSync" => [14u8, 0x27u8],
    "MaxCalendarAgeFilter" => [14u8, 0x28u8],
    "AllowHTMLEmail" => [14u8, 0x29u8],
    "MaxEmailAgeFilter" => [14u8, 0x2Au8],
    "MaxEmailBodyTruncationSize" => [14u8, 0x2Bu8],
    "MaxEmailHTMLBodyTruncationSize" => [14u8, 0x2Cu8],
    "RequireSignedSMIMEMessages" => [14u8, 0x2Du8],
    "RequireEncryptedSMIMEMessages" => [14u8, 0x2Eu8],
    "RequireSignedSMIMEAlgorithm" => [14u8, 0x2Fu8],
    "RequireEncryptionSMIMEAlgorithm" => [14u8, 0x30u8],
    "AllowSMIMEEncryptionAlgorithmNegotiation" => [14u8, 0x31u8],
    "AllowSMIMESoftCerts" => [14u8, 0x32u8],
    "AllowBrowser" => [14u8, 0x33u8],
    "AllowConsumerEmail" => [14u8, 0x34u8],
    "AllowRemoteDesktop" => [14u8, 0x35u8],
    "AllowInternetSharing" => [14u8, 0x36u8],
    "UnapprovedInROMApplicationList" => [14u8, 0x37u8],
    "ApplicationName" => [14u8, 0x38u8],
    "ApprovedApplicationList" => [14u8, 0x39u8],
    "Hash" => [14u8, 0x3Au8],
    "Search" => [15u8, 0x05u8],
    "Name" => [15u8, 0x08u8],
    "Query" => [15u8, 0x09u8],
    "Result" => [15u8, 0x0Eu8],
    "EqualTo" => [15u8, 0x11u8],
    "Value" => [15u8, 0x12u8],
    "And" => [15u8, 0x13u8],
    "Or" => [15u8, 0x14u8],
    "FreeText" => [15u8, 0x15u8],
    "DeepTraversal" => [15u8, 0x17u8],
    "LongId" => [15u8, 0x18u8],
    "RebuildResults" => [15u8, 0x19u8],
    "LeafName" => [15u8, 0x1Au8],
    "Class" => [15u8, 0x1Bu8],
    "CollectionId" => [15u8, 0x1Cu8],
    "QueryId" => [15u8, 0x1Du8],
    "MaxResults" => [15u8, 0x1Eu8],
    "GAL:DisplayName" => [16u8, 0x05u8],
    "GAL:Phone" => [16u8, 0x06u8],
    "GAL:Office" => [16u8, 0x07u8],
    "GAL:Title" => [16u8, 0x08u8],
    "GAL:Company" => [16u8, 0x09u8],
    "GAL:Alias" => [16u8, 0x0Au8],
    "GAL:FirstName" => [16u8, 0x0Bu8],
    "GAL:LastName" => [16u8, 0x0Cu8],
    "GAL:HomePhone" => [16u8, 0x0Du8],
    "GAL:MobilePhone" => [16u8, 0x0Eu8],
    "GAL:EmailAddress" => [16u8, 0x0Fu8],
    "GAL:Picture" => [16u8, 0x10u8],
    "GAL:Status" => [16u8, 0x11u8],
    "GAL:Data" => [16u8, 0x12u8],
    "AirSyncBase:BodyPreference" => [17u8, 0x05u8],
    "AirSyncBase:Type" => [17u8, 0x06u8],
    "AirSyncBase:TruncationSize" => [17u8, 0x07u8],
    "AirSyncBase:AllOrNone" => [17u8, 0x08u8],
    "AirSyncBase:Body" => [17u8, 0x0Au8],
    "AirSyncBase:Data" => [17u8, 0x0Bu8],
    "AirSyncBase:EstimatedDataSize" => [17u8, 0x0Cu8],
    "AirSyncBase:Truncated" => [17u8, 0x0Du8],
    "AirSyncBase:Attachments" => [17u8, 0x0Eu8],
    "AirSyncBase:Attachment" => [17u8, 0x0Fu8],
    "AirSyncBase:DisplayName" => [17u8, 0x10u8],
    "AirSyncBase:FileReference" => [17u8, 0x11u8],
    "AirSyncBase:Method" => [17u8, 0x12u8],
    "AirSyncBase:ContentId" => [17u8, 0x13u8],
    "AirSyncBase:ContentLocation" => [17u8, 0x14u8],
    "AirSyncBase:IsInline" => [17u8, 0x15u8],
    "AirSyncBase:NativeBodyType" => [17u8, 0x16u8],
    "AirSyncBase:ContentType" => [17u8, 0x17u8],
    "AirSyncBase:Preview" => [17u8, 0x18u8],
    "AirSyncBase:BodyPartPreference" => [17u8, 0x19u8],
    "AirSyncBase:BodyPart" => [17u8, 0x1Au8],
    "AirSyncBase:Status" => [17u8, 0x1Bu8],
    "AirSyncBase:Add" => [17u8, 0x1Cu8],
    "AirSyncBase:Delete" => [17u8, 0x1Du8],
    "AirSyncBase:ClientId" => [17u8, 0x1Eu8],
    "AirSyncBase:Content" => [17u8, 0x1Fu8],
    "AirSyncBase:Location" => [17u8, 0x20u8],
    "AirSyncBase:Annotation" => [17u8, 0x21u8],
    "AirSyncBase:Street" => [17u8, 0x22u8],
    "AirSyncBase:City" => [17u8, 0x23u8],
    "AirSyncBase:State" => [17u8, 0x24u8],
    "AirSyncBase:Country" => [17u8, 0x25u8],
    "AirSyncBase:PostalCode" => [17u8, 0x26u8],
    "AirSyncBase:Latitude" => [17u8, 0x27u8],
    "AirSyncBase:Longitude" => [17u8, 0x28u8],
    "AirSyncBase:Accuracy" => [17u8, 0x29u8],
    "AirSyncBase:Altitude" => [17u8, 0x2Au8],
    "AirSyncBase:AltitudeAccuracy" => [17u8, 0x2Bu8],
    "AirSyncBase:LocationUri" => [17u8, 0x2Cu8],
    "AirSyncBase:InstanceId" => [17u8, 0x2Du8],
    "Settings" => [18u8, 0x05u8],
    "Get" => [18u8, 0x07u8],
    "Set" => [18u8, 0x08u8],
    "Oof" => [18u8, 0x09u8],
    "OofState" => [18u8, 0x0Au8],
    "StartTime" => [18u8, 0x0Bu8],
    "EndTime" => [18u8, 0x0Cu8],
    "OofMessage" => [18u8, 0x0Du8],
    "AppliesToInternal" => [18u8, 0x0Eu8],
    "AppliesToExternalKnown" => [18u8, 0x0Fu8],
    "AppliesToExternalUnknown" => [18u8, 0x10u8],
    "Enabled" => [18u8, 0x11u8],
    "ReplyMessage" => [18u8, 0x12u8],
    "BodyType" => [18u8, 0x13u8],
    "DevicePassword" => [18u8, 0x14u8],
    "Password" => [18u8, 0x15u8],
    "DeviceInformation" => [18u8, 0x16u8],
    "Model" => [18u8, 0x17u8],
    "IMEI" => [18u8, 0x18u8],
    "FriendlyName" => [18u8, 0x19u8],
    "OS" => [18u8, 0x1Au8],
    "OSLanguage" => [18u8, 0x1Bu8],
    "PhoneNumber" => [18u8, 0x1Cu8],
    "UserInformation" => [18u8, 0x1Du8],
    "EmailAddresses" => [18u8, 0x1Eu8],
    "SMTPAddress" => [18u8, 0x1Fu8],
    "UserAgent" => [18u8, 0x20u8],
    "EnableOutboundSMS" => [18u8, 0x21u8],
    "MobileOperator" => [18u8, 0x22u8],
    "PrimarySmtpAddress" => [18u8, 0x23u8],
    "Accounts" => [18u8, 0x24u8],
    "Account" => [18u8, 0x25u8],
    "AccountName" => [18u8, 0x27u8],
    "UserDisplayName" => [18u8, 0x28u8],
    "SendDisabled" => [18u8, 0x29u8],
    "RightsManagementInformation" => [18u8, 0x2Bu8],
    "DocumentLibrary:LinkId" => [19u8, 0x05u8],
    "DocumentLibrary:DisplayName" => [19u8, 0x06u8],
    "DocumentLibrary:IsFolder" => [19u8, 0x07u8],
    "DocumentLibrary:CreationDate" => [19u8, 0x08u8],
    "DocumentLibrary:LastModifiedDate" => [19u8, 0x09u8],
    "DocumentLibrary:IsHidden" => [19u8, 0x0Au8],
    "DocumentLibrary:ContentLength" => [19u8, 0x0Bu8],
    "DocumentLibrary:ContentType" => [19u8, 0x0Cu8],
    "ItemOperations" => [20u8, 0x05u8],
    "Fetch" => [20u8, 0x06u8],
    "Store" => [20u8, 0x07u8],
    "Options" => [20u8, 0x08u8],
    "Range" => [20u8, 0x09u8],
    "Total" => [20u8, 0x0Au8],
    "Properties" => [20u8, 0x0Bu8],
    "Data" => [20u8, 0x0Cu8],
    "Response" => [20u8, 0x0Eu8],
    "Version" => [20u8, 0x0Fu8],
    "Schema" => [20u8, 0x10u8],
    "Part" => [20u8, 0x11u8],
    "EmptyFolderContents" => [20u8, 0x12u8],
    "DeleteSubFolders" => [20u8, 0x13u8],
    "UserName" => [20u8, 0x14u8],
    "IOPassword" => [20u8, 0x15u8],
    "Move" => [20u8, 0x16u8],
    "DstFldId" => [20u8, 0x17u8],
    "ConversationId" => [20u8, 0x18u8],
    "MoveAlways" => [20u8, 0x19u8],
    "SendMail" => [21u8, 0x05u8],
    "SmartForward" => [21u8, 0x06u8],
    "SmartReply" => [21u8, 0x07u8],
    "SaveInSentItems" => [21u8, 0x08u8],
    "ReplaceMime" => [21u8, 0x09u8],
    "Mime" => [21u8, 0x0Bu8],
    "ClientId" => [21u8, 0x0Cu8],
    "Status" => [21u8, 0x0Du8],
    "AccountId" => [21u8, 0x0Eu8],
    "Forwardees" => [21u8, 0x0Fu8],
    "Forwardee" => [21u8, 0x10u8],
    "ForwardeeName" => [21u8, 0x11u8],
    "ForwardeeEmail" => [21u8, 0x12u8],
    "Email2:UmCallerId" => [22u8, 0x05u8],
    "Email2:UmUserNotes" => [22u8, 0x06u8],
    "Email2:UmAttDuration" => [22u8, 0x07u8],
    "Email2:UmAttOrder" => [22u8, 0x08u8],
    "Email2:ConversationId" => [22u8, 0x09u8],
    "Email2:ConversationIndex" => [22u8, 0x0Au8],
    "Email2:LastVerbExecuted" => [22u8, 0x0Bu8],
    "Email2:LastVerbExecutionTime" => [22u8, 0x0Cu8],
    "Email2:ReceivedAsBcc" => [22u8, 0x0Du8],
    "Email2:Sender" => [22u8, 0x0Eu8],
    "Email2:CalendarType" => [22u8, 0x0Fu8],
    "Email2:IsLeapMonth" => [22u8, 0x10u8],
    "Email2:AccountId" => [22u8, 0x11u8],
    "Email2:FirstDayOfWeek" => [22u8, 0x12u8],
    "Email2:MeetingMessageType" => [22u8, 0x13u8],
    "Notes:Subject" => [23u8, 0x05u8],
    "Notes:MessageClass" => [23u8, 0x06u8],
    "Notes:LastModifiedDate" => [23u8, 0x07u8],
    "Notes:Categories" => [23u8, 0x08u8],
    "Notes:Category" => [23u8, 0x09u8],
    "Notes:Body" => [23u8, 0x0Bu8],
    "RightsManagement:RightsManagementSupport" => [24u8, 0x05u8],
    "RightsManagement:RightsManagementTemplates" => [24u8, 0x06u8],
    "RightsManagement:RightsManagementTemplate" => [24u8, 0x07u8],
    "RightsManagement:RightsManagementLicense" => [24u8, 0x08u8],
    "RightsManagement:EditAllowed" => [24u8, 0x09u8],
    "RightsManagement:ReplyAllowed" => [24u8, 0x0Au8],
    "RightsManagement:ReplyAllAllowed" => [24u8, 0x0Bu8],
    "RightsManagement:ForwardAllowed" => [24u8, 0x0Cu8],
    "RightsManagement:ModifyRecipientsAllowed" => [24u8, 0x0Du8],
    "RightsManagement:ExtractAllowed" => [24u8, 0x0Eu8],
    "RightsManagement:PrintAllowed" => [24u8, 0x0Fu8],
    "RightsManagement:ExportAllowed" => [24u8, 0x10u8],
    "RightsManagement:ProgrammaticAccessAllowed" => [24u8, 0x11u8],
    "RightsManagement:RMOwner" => [24u8, 0x12u8],
    "RightsManagement:ContentExpiryDate" => [24u8, 0x13u8],
    "RightsManagement:ContentExpiryDateString" => [24u8, 0x14u8],
    "RightsManagement:ContentExpiryInterval" => [24u8, 0x15u8],
    "RightsManagement:ContentExpiryIntervalType" => [24u8, 0x16u8],
    "RightsManagement:TemplateID" => [24u8, 0x17u8],
    "RightsManagement:TemplateName" => [24u8, 0x18u8],
    "RightsManagement:TemplateDescription" => [24u8, 0x19u8],
    "RightsManagement:ContentOwner" => [24u8, 0x1Au8],
    "RightsManagement:RemoveRightsManagementDistribution" => [24u8, 0x1Bu8],
};

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
            if pair[0] == cp {
                return Some((pair[0], pair[1]));
            }
        } else {
            return Some((pair[0], pair[1]));
        }
    }

    for (&pair, &name) in TAG_TO_NAME.entries() {
        let local = if let Some(p) = name.rfind(':') {
            &name[p + 1..]
        } else {
            name
        };
        if local == qualified_or_local {
            if let Some(ocp) = override_cp {
                if pair[0] == ocp {
                    return Some((pair[0], pair[1]));
                }
            } else {
                return Some((pair[0], pair[1]));
            }
        }
    }
    None
}

pub struct Wbxml;

impl Default for Wbxml {
    fn default() -> Self {
        Self::new()
    }
}

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
        let _version = *bytes
            .get(pos)
            .ok_or_else(|| anyhow!("Missing WBXML version"))?;
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
                        if let Some(name) = TAG_TO_NAME.get(&[current_code_page, tag_id]) {
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
        let mut prefix_ns_stack: Vec<std::collections::HashMap<String, Option<u8>>> = Vec::new();

        let mut reader = quick_xml::Reader::from_str(xml);
        reader.config_mut().trim_text(true);
        let mut event_buf = Vec::new();

        loop {
            match reader.read_event_into(&mut event_buf) {
                Ok(quick_xml::events::Event::Start(ref e)) => {
                    let mut new_prefixes: std::collections::HashMap<String, Option<u8>> =
                        std::collections::HashMap::new();
                    for attr in e.attributes().flatten() {
                        let key_bytes = attr.key.as_ref();
                        if key_bytes.starts_with(b"xmlns:") && key_bytes.len() > 6 {
                            let prefix = String::from_utf8_lossy(&key_bytes[6..]);
                            if let Ok(val) = attr.decode_and_unescape_value(reader.decoder()) {
                                let cp = namespace_to_code_page(val.as_ref());
                                new_prefixes.insert(prefix.into_owned(), cp);
                            }
                        }
                    }
                    prefix_ns_stack.push(new_prefixes);

                    let ns_cp = extract_xmlns_cp(e, &reader);
                    ns_stack.push(ns_cp);

                    let qname = e.name();
                    let full_name = String::from_utf8_lossy(qname.as_ref());
                    let (local_name, effective_cp) = if let Some(pos) = full_name.find(':') {
                        let prefix = &full_name[..pos];
                        let local = &full_name[pos + 1..];
                        let prefix_cp = prefix_ns_stack
                            .iter()
                            .rev()
                            .find_map(|map| map.get(prefix).copied().flatten());
                        (
                            local,
                            prefix_cp
                                .or(ns_cp)
                                .or_else(|| ns_stack.iter().rev().find_map(|&x| x)),
                        )
                    } else {
                        (
                            &*full_name,
                            ns_cp.or_else(|| ns_stack.iter().rev().find_map(|&x| x)),
                        )
                    };

                    self.encode_open_tag(
                        &mut buf,
                        &mut current_code_page,
                        local_name,
                        effective_cp,
                        true,
                    )?;
                }
                Ok(quick_xml::events::Event::Empty(ref e)) => {
                    let mut new_prefixes: std::collections::HashMap<String, Option<u8>> =
                        std::collections::HashMap::new();
                    for attr in e.attributes().flatten() {
                        let key_bytes = attr.key.as_ref();
                        if key_bytes.starts_with(b"xmlns:") && key_bytes.len() > 6 {
                            let prefix = String::from_utf8_lossy(&key_bytes[6..]);
                            if let Ok(val) = attr.decode_and_unescape_value(reader.decoder()) {
                                let cp = namespace_to_code_page(val.as_ref());
                                new_prefixes.insert(prefix.into_owned(), cp);
                            }
                        }
                    }
                    prefix_ns_stack.push(new_prefixes);

                    let ns_cp = extract_xmlns_cp(e, &reader);

                    let qname = e.name();
                    let full_name = String::from_utf8_lossy(qname.as_ref());
                    let (local_name, effective_cp) = if let Some(pos) = full_name.find(':') {
                        let prefix = &full_name[..pos];
                        let local = &full_name[pos + 1..];
                        let prefix_cp = prefix_ns_stack
                            .iter()
                            .rev()
                            .find_map(|map| map.get(prefix).copied().flatten());
                        (
                            local,
                            prefix_cp
                                .or(ns_cp)
                                .or_else(|| ns_stack.iter().rev().find_map(|&x| x)),
                        )
                    } else {
                        (
                            &*full_name,
                            ns_cp.or_else(|| ns_stack.iter().rev().find_map(|&x| x)),
                        )
                    };

                    self.encode_open_tag(
                        &mut buf,
                        &mut current_code_page,
                        local_name,
                        effective_cp,
                        false,
                    )?;

                    prefix_ns_stack.pop();
                }
                Ok(quick_xml::events::Event::Text(ref e)) => {
                    let txt = e
                        .decode()
                        .map_err(|e| anyhow!("XML decode error: {e}"))?
                        .into_owned();
                    if !txt.is_empty() {
                        buf.push(STR_I);
                        buf.extend_from_slice(txt.as_bytes());
                        buf.push(0x00);
                    }
                }
                Ok(quick_xml::events::Event::End(_)) => {
                    ns_stack.pop();
                    prefix_ns_stack.pop();
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
        Err(anyhow!("WBXML encode: unknown tag '{}'", name_str))
    }
}

fn extract_xmlns_cp<'a, R: std::io::BufRead>(
    e: &quick_xml::events::BytesStart<'a>,
    reader: &quick_xml::Reader<R>,
) -> Option<u8> {
    for attr in e.attributes().flatten() {
        if attr.key.as_ref() == b"xmlns"
            && let Ok(val) = attr.decode_and_unescape_value(reader.decoder())
        {
            if let Some(cp) = namespace_to_code_page(val.as_ref()) {
                return Some(cp);
            }
            let with_colon = format!("{}:", val);
            if let Some(cp) = namespace_to_code_page(&with_colon) {
                return Some(cp);
            }
        }
    }
    None
}
