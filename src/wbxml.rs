// src/wbxml.rs
use crate::util::{resolve_xml_reference_strict, xml_escape_text};
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
    // Code page 0: AirSync ([MS-ASWBXML] v20250520 §2.1.2.1.1)
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
    [0u8, 0x19u8] => "Truncation",
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
    // Code page 1: Contacts ([MS-ASWBXML] v20250520 §2.1.2.1.2)
    [1u8, 0x05u8] => "Contacts:Anniversary",
    [1u8, 0x06u8] => "Contacts:AssistantName",
    [1u8, 0x07u8] => "Contacts:AssistantPhoneNumber",
    [1u8, 0x08u8] => "Contacts:Birthday",
    [1u8, 0x09u8] => "Contacts:Body",
    [1u8, 0x0Au8] => "Contacts:BodySize",
    [1u8, 0x0Bu8] => "Contacts:BodyTruncated",
    [1u8, 0x0Cu8] => "Contacts:Business2PhoneNumber",
    [1u8, 0x0Du8] => "Contacts:BusinessAddressCity",
    [1u8, 0x0Eu8] => "Contacts:BusinessAddressCountry",
    [1u8, 0x0Fu8] => "Contacts:BusinessAddressPostalCode",
    [1u8, 0x10u8] => "Contacts:BusinessAddressState",
    [1u8, 0x11u8] => "Contacts:BusinessAddressStreet",
    [1u8, 0x12u8] => "Contacts:BusinessFaxNumber",
    [1u8, 0x13u8] => "Contacts:BusinessPhoneNumber",
    [1u8, 0x14u8] => "Contacts:CarPhoneNumber",
    [1u8, 0x15u8] => "Contacts:Categories",
    [1u8, 0x16u8] => "Contacts:Category",
    [1u8, 0x17u8] => "Contacts:Children",
    [1u8, 0x18u8] => "Contacts:Child",
    [1u8, 0x19u8] => "Contacts:CompanyName",
    [1u8, 0x1Au8] => "Contacts:Department",
    [1u8, 0x1Bu8] => "Contacts:Email1Address",
    [1u8, 0x1Cu8] => "Contacts:Email2Address",
    [1u8, 0x1Du8] => "Contacts:Email3Address",
    [1u8, 0x1Eu8] => "Contacts:FileAs",
    [1u8, 0x1Fu8] => "Contacts:FirstName",
    [1u8, 0x20u8] => "Contacts:Home2PhoneNumber",
    [1u8, 0x21u8] => "Contacts:HomeAddressCity",
    [1u8, 0x22u8] => "Contacts:HomeAddressCountry",
    [1u8, 0x23u8] => "Contacts:HomeAddressPostalCode",
    [1u8, 0x24u8] => "Contacts:HomeAddressState",
    [1u8, 0x25u8] => "Contacts:HomeAddressStreet",
    [1u8, 0x26u8] => "Contacts:HomeFaxNumber",
    [1u8, 0x27u8] => "Contacts:HomePhoneNumber",
    [1u8, 0x28u8] => "Contacts:JobTitle",
    [1u8, 0x29u8] => "Contacts:LastName",
    [1u8, 0x2Au8] => "Contacts:MiddleName",
    [1u8, 0x2Bu8] => "Contacts:MobilePhoneNumber",
    [1u8, 0x2Cu8] => "Contacts:OfficeLocation",
    [1u8, 0x2Du8] => "Contacts:OtherAddressCity",
    [1u8, 0x2Eu8] => "Contacts:OtherAddressCountry",
    [1u8, 0x2Fu8] => "Contacts:OtherAddressPostalCode",
    [1u8, 0x30u8] => "Contacts:OtherAddressState",
    [1u8, 0x31u8] => "Contacts:OtherAddressStreet",
    [1u8, 0x32u8] => "Contacts:PagerNumber",
    [1u8, 0x33u8] => "Contacts:RadioPhoneNumber",
    [1u8, 0x34u8] => "Contacts:Spouse",
    [1u8, 0x35u8] => "Contacts:Suffix",
    [1u8, 0x36u8] => "Contacts:Title",
    [1u8, 0x37u8] => "Contacts:WebPage",
    [1u8, 0x38u8] => "Contacts:YomiCompanyName",
    [1u8, 0x39u8] => "Contacts:YomiFirstName",
    [1u8, 0x3Au8] => "Contacts:YomiLastName",
    [1u8, 0x3Cu8] => "Contacts:Picture",
    [1u8, 0x3Du8] => "Contacts:Alias",
    [1u8, 0x3Eu8] => "Contacts:WeightedRank",
    // Code page 2: Email ([MS-ASWBXML] v20250520 §2.1.2.1.3)
    [2u8, 0x05u8] => "Email:Attachment",
    [2u8, 0x06u8] => "Email:Attachments",
    [2u8, 0x07u8] => "Email:AttName",
    [2u8, 0x08u8] => "Email:AttSize",
    [2u8, 0x09u8] => "Email:Att0id",
    [2u8, 0x0Au8] => "Email:AttMethod",
    [2u8, 0x0Cu8] => "Email:Body",
    [2u8, 0x0Du8] => "Email:BodySize",
    [2u8, 0x0Eu8] => "Email:BodyTruncated",
    [2u8, 0x0Fu8] => "Email:DateReceived",
    [2u8, 0x10u8] => "Email:DisplayName",
    [2u8, 0x11u8] => "Email:DisplayTo",
    [2u8, 0x12u8] => "Email:Importance",
    [2u8, 0x13u8] => "Email:MessageClass",
    [2u8, 0x14u8] => "Email:Subject",
    [2u8, 0x15u8] => "Email:Read",
    [2u8, 0x16u8] => "Email:To",
    [2u8, 0x17u8] => "Email:Cc",
    [2u8, 0x18u8] => "Email:From",
    [2u8, 0x19u8] => "Email:ReplyTo",
    [2u8, 0x1Au8] => "Email:AllDayEvent",
    [2u8, 0x1Bu8] => "Email:Categories",
    [2u8, 0x1Cu8] => "Email:Category",
    [2u8, 0x1Du8] => "Email:DtStamp",
    [2u8, 0x1Eu8] => "Email:EndTime",
    [2u8, 0x1Fu8] => "Email:InstanceType",
    [2u8, 0x20u8] => "Email:BusyStatus",
    [2u8, 0x21u8] => "Email:Location",
    [2u8, 0x22u8] => "Email:MeetingRequest",
    [2u8, 0x23u8] => "Email:Organizer",
    [2u8, 0x24u8] => "Email:RecurrenceId",
    [2u8, 0x25u8] => "Email:Reminder",
    [2u8, 0x26u8] => "Email:ResponseRequested",
    [2u8, 0x27u8] => "Email:Recurrences",
    [2u8, 0x28u8] => "Email:Recurrence",
    [2u8, 0x29u8] => "Email:Type",
    [2u8, 0x2Au8] => "Email:Until",
    [2u8, 0x2Bu8] => "Email:Occurrences",
    [2u8, 0x2Cu8] => "Email:Interval",
    [2u8, 0x2Du8] => "Email:DayOfWeek",
    [2u8, 0x2Eu8] => "Email:DayOfMonth",
    [2u8, 0x2Fu8] => "Email:WeekOfMonth",
    [2u8, 0x30u8] => "Email:MonthOfYear",
    [2u8, 0x31u8] => "Email:StartTime",
    [2u8, 0x32u8] => "Email:Sensitivity",
    [2u8, 0x33u8] => "Email:TimeZone",
    [2u8, 0x34u8] => "Email:GlobalObjId",
    [2u8, 0x35u8] => "Email:ThreadTopic",
    [2u8, 0x36u8] => "Email:MIMEData",
    [2u8, 0x37u8] => "Email:MIMETruncated",
    [2u8, 0x38u8] => "Email:MIMESize",
    [2u8, 0x39u8] => "Email:InternetCPID",
    [2u8, 0x3Au8] => "Email:Flag",
    [2u8, 0x3Bu8] => "Email:Status",
    [2u8, 0x3Cu8] => "Email:ContentClass",
    [2u8, 0x3Du8] => "Email:FlagType",
    [2u8, 0x3Eu8] => "Email:CompleteTime",
    [2u8, 0x3Fu8] => "Email:DisallowNewTimeProposal",
    // Code page 4: Calendar ([MS-ASWBXML] v20250520 §2.1.2.1.4)
    [4u8, 0x05u8] => "Calendar:Timezone",
    [4u8, 0x06u8] => "Calendar:AllDayEvent",
    [4u8, 0x07u8] => "Calendar:Attendees",
    [4u8, 0x08u8] => "Calendar:Attendee",
    [4u8, 0x09u8] => "Calendar:Email",
    [4u8, 0x0Au8] => "Calendar:Name",
    [4u8, 0x0Bu8] => "Calendar:Body",
    [4u8, 0x0Cu8] => "Calendar:BodyTruncated",
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
    // Code page 5: Move ([MS-ASWBXML] v20250520 §2.1.2.1.5)
    [5u8, 0x05u8] => "Move:MoveItems",
    [5u8, 0x06u8] => "Move:Move",
    [5u8, 0x07u8] => "Move:SrcMsgId",
    [5u8, 0x08u8] => "Move:SrcFldId",
    [5u8, 0x09u8] => "Move:DstFldId",
    [5u8, 0x0Au8] => "Move:Response",
    [5u8, 0x0Bu8] => "Move:Status",
    [5u8, 0x0Cu8] => "Move:DstMsgId",
    // Code page 6: GetItemEstimate ([MS-ASWBXML] v20250520 §2.1.2.1.6)
    [6u8, 0x05u8] => "GetItemEstimate:GetItemEstimate",
    [6u8, 0x07u8] => "GetItemEstimate:Collections",
    [6u8, 0x08u8] => "GetItemEstimate:Collection",
    [6u8, 0x09u8] => "GetItemEstimate:Class",
    [6u8, 0x0Au8] => "GetItemEstimate:CollectionId",
    [6u8, 0x0Cu8] => "GetItemEstimate:Estimate",
    [6u8, 0x0Du8] => "GetItemEstimate:Response",
    [6u8, 0x0Eu8] => "GetItemEstimate:Status",
    // Code page 7: FolderHierarchy ([MS-ASWBXML] v20250520 §2.1.2.1.7)
    [7u8, 0x05u8] => "FolderHierarchy:Folders",
    [7u8, 0x06u8] => "FolderHierarchy:Folder",
    [7u8, 0x07u8] => "FolderHierarchy:DisplayName",
    [7u8, 0x08u8] => "FolderHierarchy:ServerId",
    [7u8, 0x09u8] => "FolderHierarchy:ParentId",
    [7u8, 0x0Au8] => "FolderHierarchy:Type",
    [7u8, 0x0Cu8] => "FolderHierarchy:Status",
    [7u8, 0x0Eu8] => "FolderHierarchy:Changes",
    [7u8, 0x0Fu8] => "FolderHierarchy:Add",
    [7u8, 0x10u8] => "FolderHierarchy:Delete",
    [7u8, 0x11u8] => "FolderHierarchy:Update",
    [7u8, 0x12u8] => "FolderHierarchy:SyncKey",
    [7u8, 0x13u8] => "FolderHierarchy:FolderCreate",
    [7u8, 0x14u8] => "FolderHierarchy:FolderDelete",
    [7u8, 0x15u8] => "FolderHierarchy:FolderUpdate",
    [7u8, 0x16u8] => "FolderHierarchy:FolderSync",
    [7u8, 0x17u8] => "FolderHierarchy:Count",
    // Code page 8: MeetingResponse ([MS-ASWBXML] v20250520 §2.1.2.1.8)
    [8u8, 0x05u8] => "MeetingResponse:CalendarId",
    [8u8, 0x06u8] => "MeetingResponse:CollectionId",
    [8u8, 0x07u8] => "MeetingResponse:MeetingResponse",
    [8u8, 0x08u8] => "MeetingResponse:RequestId",
    [8u8, 0x09u8] => "MeetingResponse:Request",
    [8u8, 0x0Au8] => "MeetingResponse:Result",
    [8u8, 0x0Bu8] => "MeetingResponse:Status",
    [8u8, 0x0Cu8] => "MeetingResponse:UserResponse",
    [8u8, 0x0Eu8] => "MeetingResponse:InstanceId",
    [8u8, 0x10u8] => "MeetingResponse:ProposedStartTime",
    [8u8, 0x11u8] => "MeetingResponse:ProposedEndTime",
    [8u8, 0x12u8] => "MeetingResponse:SendResponse",
    // Code page 9: Tasks ([MS-ASWBXML] v20250520 §2.1.2.1.9)
    [9u8, 0x05u8] => "Tasks:Body",
    [9u8, 0x06u8] => "Tasks:BodySize",
    [9u8, 0x07u8] => "Tasks:BodyTruncated",
    [9u8, 0x08u8] => "Tasks:Categories",
    [9u8, 0x09u8] => "Tasks:Category",
    [9u8, 0x0Au8] => "Tasks:Complete",
    [9u8, 0x0Bu8] => "Tasks:DateCompleted",
    [9u8, 0x0Cu8] => "Tasks:DueDate",
    [9u8, 0x0Du8] => "Tasks:UtcDueDate",
    [9u8, 0x0Eu8] => "Tasks:Importance",
    [9u8, 0x0Fu8] => "Tasks:Recurrence",
    [9u8, 0x10u8] => "Tasks:Type",
    [9u8, 0x11u8] => "Tasks:Start",
    [9u8, 0x12u8] => "Tasks:Until",
    [9u8, 0x13u8] => "Tasks:Occurrences",
    [9u8, 0x14u8] => "Tasks:Interval",
    [9u8, 0x15u8] => "Tasks:DayOfMonth",
    [9u8, 0x16u8] => "Tasks:DayOfWeek",
    [9u8, 0x17u8] => "Tasks:WeekOfMonth",
    [9u8, 0x18u8] => "Tasks:MonthOfYear",
    [9u8, 0x19u8] => "Tasks:Regenerate",
    [9u8, 0x1Au8] => "Tasks:DeadOccur",
    [9u8, 0x1Bu8] => "Tasks:ReminderSet",
    [9u8, 0x1Cu8] => "Tasks:ReminderTime",
    [9u8, 0x1Du8] => "Tasks:Sensitivity",
    [9u8, 0x1Eu8] => "Tasks:StartDate",
    [9u8, 0x1Fu8] => "Tasks:UtcStartDate",
    [9u8, 0x20u8] => "Tasks:Subject",
    [9u8, 0x22u8] => "Tasks:OrdinalDate",
    [9u8, 0x23u8] => "Tasks:SubOrdinalDate",
    [9u8, 0x24u8] => "Tasks:CalendarType",
    [9u8, 0x25u8] => "Tasks:IsLeapMonth",
    [9u8, 0x26u8] => "Tasks:FirstDayOfWeek",
    // Code page 10: ResolveRecipients ([MS-ASWBXML] v20250520 §2.1.2.1.10)
    [10u8, 0x05u8] => "ResolveRecipients:ResolveRecipients",
    [10u8, 0x06u8] => "ResolveRecipients:Response",
    [10u8, 0x07u8] => "ResolveRecipients:Status",
    [10u8, 0x08u8] => "ResolveRecipients:Type",
    [10u8, 0x09u8] => "ResolveRecipients:Recipient",
    [10u8, 0x0Au8] => "ResolveRecipients:DisplayName",
    [10u8, 0x0Bu8] => "ResolveRecipients:EmailAddress",
    [10u8, 0x0Cu8] => "ResolveRecipients:Certificates",
    [10u8, 0x0Du8] => "ResolveRecipients:Certificate",
    [10u8, 0x0Eu8] => "ResolveRecipients:MiniCertificate",
    [10u8, 0x0Fu8] => "ResolveRecipients:Options",
    [10u8, 0x10u8] => "ResolveRecipients:To",
    [10u8, 0x11u8] => "ResolveRecipients:CertificateRetrieval",
    [10u8, 0x12u8] => "ResolveRecipients:RecipientCount",
    [10u8, 0x13u8] => "ResolveRecipients:MaxCertificates",
    [10u8, 0x14u8] => "ResolveRecipients:MaxAmbiguousRecipients",
    [10u8, 0x15u8] => "ResolveRecipients:CertificateCount",
    [10u8, 0x16u8] => "ResolveRecipients:Availability",
    [10u8, 0x17u8] => "ResolveRecipients:StartTime",
    [10u8, 0x18u8] => "ResolveRecipients:EndTime",
    [10u8, 0x19u8] => "ResolveRecipients:MergedFreeBusy",
    [10u8, 0x1Au8] => "ResolveRecipients:Picture",
    [10u8, 0x1Bu8] => "ResolveRecipients:MaxSize",
    [10u8, 0x1Cu8] => "ResolveRecipients:Data",
    [10u8, 0x1Du8] => "ResolveRecipients:MaxPictures",
    // Code page 11: ValidateCert ([MS-ASWBXML] v20250520 §2.1.2.1.11)
    [11u8, 0x05u8] => "ValidateCert:ValidateCert",
    [11u8, 0x06u8] => "ValidateCert:Certificates",
    [11u8, 0x07u8] => "ValidateCert:Certificate",
    [11u8, 0x08u8] => "ValidateCert:CertificateChain",
    [11u8, 0x09u8] => "ValidateCert:CheckCRL",
    [11u8, 0x0Au8] => "ValidateCert:Status",
    // Code page 12: Contacts2 ([MS-ASWBXML] v20250520 §2.1.2.1.12)
    [12u8, 0x05u8] => "Contacts2:CustomerId",
    [12u8, 0x06u8] => "Contacts2:GovernmentId",
    [12u8, 0x07u8] => "Contacts2:IMAddress",
    [12u8, 0x08u8] => "Contacts2:IMAddress2",
    [12u8, 0x09u8] => "Contacts2:IMAddress3",
    [12u8, 0x0Au8] => "Contacts2:ManagerName",
    [12u8, 0x0Bu8] => "Contacts2:CompanyMainPhone",
    [12u8, 0x0Cu8] => "Contacts2:AccountName",
    [12u8, 0x0Du8] => "Contacts2:NickName",
    [12u8, 0x0Eu8] => "Contacts2:MMS",
    // Code page 13: Ping ([MS-ASWBXML] v20250520 §2.1.2.1.13)
    [13u8, 0x05u8] => "Ping:Ping",
    [13u8, 0x07u8] => "Ping:Status",
    [13u8, 0x08u8] => "Ping:HeartbeatInterval",
    [13u8, 0x09u8] => "Ping:Folders",
    [13u8, 0x0Au8] => "Ping:Folder",
    [13u8, 0x0Bu8] => "Ping:Id",
    [13u8, 0x0Cu8] => "Ping:Class",
    [13u8, 0x0Du8] => "Ping:MaxFolders",
    // Code page 14: Provision ([MS-ASWBXML] v20250520 §2.1.2.1.14)
    [14u8, 0x05u8] => "Provision:Provision",
    [14u8, 0x06u8] => "Provision:Policies",
    [14u8, 0x07u8] => "Provision:Policy",
    [14u8, 0x08u8] => "Provision:PolicyType",
    [14u8, 0x09u8] => "Provision:PolicyKey",
    [14u8, 0x0Au8] => "Provision:Data",
    [14u8, 0x0Bu8] => "Provision:Status",
    [14u8, 0x0Cu8] => "Provision:RemoteWipe",
    [14u8, 0x0Du8] => "Provision:EASProvisionDoc",
    [14u8, 0x0Eu8] => "Provision:DevicePasswordEnabled",
    [14u8, 0x0Fu8] => "Provision:AlphanumericDevicePasswordRequired",
    [14u8, 0x10u8] => "Provision:RequireStorageCardEncryption",
    [14u8, 0x11u8] => "Provision:PasswordRecoveryEnabled",
    [14u8, 0x13u8] => "Provision:AttachmentsEnabled",
    [14u8, 0x14u8] => "Provision:MinDevicePasswordLength",
    [14u8, 0x15u8] => "Provision:MaxInactivityTimeDeviceLock",
    [14u8, 0x16u8] => "Provision:MaxDevicePasswordFailedAttempts",
    [14u8, 0x17u8] => "Provision:MaxAttachmentSize",
    [14u8, 0x18u8] => "Provision:AllowSimpleDevicePassword",
    [14u8, 0x19u8] => "Provision:DevicePasswordExpiration",
    [14u8, 0x1Au8] => "Provision:DevicePasswordHistory",
    [14u8, 0x1Bu8] => "Provision:AllowStorageCard",
    [14u8, 0x1Cu8] => "Provision:AllowCamera",
    [14u8, 0x1Du8] => "Provision:RequireDeviceEncryption",
    [14u8, 0x1Eu8] => "Provision:AllowUnsignedApplications",
    [14u8, 0x1Fu8] => "Provision:AllowUnsignedInstallationPackages",
    [14u8, 0x20u8] => "Provision:MinDevicePasswordComplexCharacters",
    [14u8, 0x21u8] => "Provision:AllowWiFi",
    [14u8, 0x22u8] => "Provision:AllowTextMessaging",
    [14u8, 0x23u8] => "Provision:AllowPOPIMAPEmail",
    [14u8, 0x24u8] => "Provision:AllowBluetooth",
    [14u8, 0x25u8] => "Provision:AllowIrDA",
    [14u8, 0x26u8] => "Provision:RequireManualSyncWhenRoaming",
    [14u8, 0x27u8] => "Provision:AllowDesktopSync",
    [14u8, 0x28u8] => "Provision:MaxCalendarAgeFilter",
    [14u8, 0x29u8] => "Provision:AllowHTMLEmail",
    [14u8, 0x2Au8] => "Provision:MaxEmailAgeFilter",
    [14u8, 0x2Bu8] => "Provision:MaxEmailBodyTruncationSize",
    [14u8, 0x2Cu8] => "Provision:MaxEmailHTMLBodyTruncationSize",
    [14u8, 0x2Du8] => "Provision:RequireSignedSMIMEMessages",
    [14u8, 0x2Eu8] => "Provision:RequireEncryptedSMIMEMessages",
    [14u8, 0x2Fu8] => "Provision:RequireSignedSMIMEAlgorithm",
    [14u8, 0x30u8] => "Provision:RequireEncryptionSMIMEAlgorithm",
    [14u8, 0x31u8] => "Provision:AllowSMIMEEncryptionAlgorithmNegotiation",
    [14u8, 0x32u8] => "Provision:AllowSMIMESoftCerts",
    [14u8, 0x33u8] => "Provision:AllowBrowser",
    [14u8, 0x34u8] => "Provision:AllowConsumerEmail",
    [14u8, 0x35u8] => "Provision:AllowRemoteDesktop",
    [14u8, 0x36u8] => "Provision:AllowInternetSharing",
    [14u8, 0x37u8] => "Provision:UnapprovedInROMApplicationList",
    [14u8, 0x38u8] => "Provision:ApplicationName",
    [14u8, 0x39u8] => "Provision:ApprovedApplicationList",
    [14u8, 0x3Au8] => "Provision:Hash",
    [14u8, 0x3Bu8] => "Provision:AccountOnlyRemoteWipe",
    // Code page 15: Search ([MS-ASWBXML] v20250520 §2.1.2.1.15)
    [15u8, 0x05u8] => "Search:Search",
    [15u8, 0x07u8] => "Search:Store",
    [15u8, 0x08u8] => "Search:Name",
    [15u8, 0x09u8] => "Search:Query",
    [15u8, 0x0Au8] => "Search:Options",
    [15u8, 0x0Bu8] => "Search:Range",
    [15u8, 0x0Cu8] => "Search:Status",
    [15u8, 0x0Du8] => "Search:Response",
    [15u8, 0x0Eu8] => "Search:Result",
    [15u8, 0x0Fu8] => "Search:Properties",
    [15u8, 0x10u8] => "Search:Total",
    [15u8, 0x11u8] => "Search:EqualTo",
    [15u8, 0x12u8] => "Search:Value",
    [15u8, 0x13u8] => "Search:And",
    [15u8, 0x14u8] => "Search:Or",
    [15u8, 0x15u8] => "Search:FreeText",
    [15u8, 0x17u8] => "Search:DeepTraversal",
    [15u8, 0x18u8] => "Search:LongId",
    [15u8, 0x19u8] => "Search:RebuildResults",
    [15u8, 0x1Au8] => "Search:LessThan",
    [15u8, 0x1Bu8] => "Search:GreaterThan",
    [15u8, 0x1Eu8] => "Search:UserName",
    [15u8, 0x1Fu8] => "Search:Password",
    [15u8, 0x20u8] => "Search:ConversationId",
    [15u8, 0x21u8] => "Search:Picture",
    [15u8, 0x22u8] => "Search:MaxSize",
    [15u8, 0x23u8] => "Search:MaxPictures",
    // Code page 16: GAL ([MS-ASWBXML] v20250520 §2.1.2.1.16)
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
    // Code page 17: AirSyncBase ([MS-ASWBXML] v20250520 §2.1.2.1.17)
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
    // Code page 18: Settings ([MS-ASWBXML] v20250520 §2.1.2.1.18)
    [18u8, 0x05u8] => "Settings:Settings",
    [18u8, 0x06u8] => "Settings:Status",
    [18u8, 0x07u8] => "Settings:Get",
    [18u8, 0x08u8] => "Settings:Set",
    [18u8, 0x09u8] => "Settings:Oof",
    [18u8, 0x0Au8] => "Settings:OofState",
    [18u8, 0x0Bu8] => "Settings:StartTime",
    [18u8, 0x0Cu8] => "Settings:EndTime",
    [18u8, 0x0Du8] => "Settings:OofMessage",
    [18u8, 0x0Eu8] => "Settings:AppliesToInternal",
    [18u8, 0x0Fu8] => "Settings:AppliesToExternalKnown",
    [18u8, 0x10u8] => "Settings:AppliesToExternalUnknown",
    [18u8, 0x11u8] => "Settings:Enabled",
    [18u8, 0x12u8] => "Settings:ReplyMessage",
    [18u8, 0x13u8] => "Settings:BodyType",
    [18u8, 0x14u8] => "Settings:DevicePassword",
    [18u8, 0x15u8] => "Settings:Password",
    [18u8, 0x16u8] => "Settings:DeviceInformation",
    [18u8, 0x17u8] => "Settings:Model",
    [18u8, 0x18u8] => "Settings:IMEI",
    [18u8, 0x19u8] => "Settings:FriendlyName",
    [18u8, 0x1Au8] => "Settings:OS",
    [18u8, 0x1Bu8] => "Settings:OSLanguage",
    [18u8, 0x1Cu8] => "Settings:PhoneNumber",
    [18u8, 0x1Du8] => "Settings:UserInformation",
    [18u8, 0x1Eu8] => "Settings:EmailAddresses",
    [18u8, 0x1Fu8] => "Settings:SMTPAddress",
    [18u8, 0x20u8] => "Settings:UserAgent",
    [18u8, 0x21u8] => "Settings:EnableOutboundSMS",
    [18u8, 0x22u8] => "Settings:MobileOperator",
    [18u8, 0x23u8] => "Settings:PrimarySmtpAddress",
    [18u8, 0x24u8] => "Settings:Accounts",
    [18u8, 0x25u8] => "Settings:Account",
    [18u8, 0x26u8] => "Settings:AccountId",
    [18u8, 0x27u8] => "Settings:AccountName",
    [18u8, 0x28u8] => "Settings:UserDisplayName",
    [18u8, 0x29u8] => "Settings:SendDisabled",
    [18u8, 0x2Bu8] => "Settings:RightsManagementInformation",
    // Code page 19: DocumentLibrary ([MS-ASWBXML] v20250520 §2.1.2.1.19)
    [19u8, 0x05u8] => "DocumentLibrary:LinkId",
    [19u8, 0x06u8] => "DocumentLibrary:DisplayName",
    [19u8, 0x07u8] => "DocumentLibrary:IsFolder",
    [19u8, 0x08u8] => "DocumentLibrary:CreationDate",
    [19u8, 0x09u8] => "DocumentLibrary:LastModifiedDate",
    [19u8, 0x0Au8] => "DocumentLibrary:IsHidden",
    [19u8, 0x0Bu8] => "DocumentLibrary:ContentLength",
    [19u8, 0x0Cu8] => "DocumentLibrary:ContentType",
    // Code page 20: ItemOperations ([MS-ASWBXML] v20250520 §2.1.2.1.20)
    [20u8, 0x05u8] => "ItemOperations:ItemOperations",
    [20u8, 0x06u8] => "ItemOperations:Fetch",
    [20u8, 0x07u8] => "ItemOperations:Store",
    [20u8, 0x08u8] => "ItemOperations:Options",
    [20u8, 0x09u8] => "ItemOperations:Range",
    [20u8, 0x0Au8] => "ItemOperations:Total",
    [20u8, 0x0Bu8] => "ItemOperations:Properties",
    [20u8, 0x0Cu8] => "ItemOperations:Data",
    [20u8, 0x0Du8] => "ItemOperations:Status",
    [20u8, 0x0Eu8] => "ItemOperations:Response",
    [20u8, 0x0Fu8] => "ItemOperations:Version",
    [20u8, 0x10u8] => "ItemOperations:Schema",
    [20u8, 0x11u8] => "ItemOperations:Part",
    [20u8, 0x12u8] => "ItemOperations:EmptyFolderContents",
    [20u8, 0x13u8] => "ItemOperations:DeleteSubFolders",
    [20u8, 0x14u8] => "ItemOperations:UserName",
    [20u8, 0x15u8] => "ItemOperations:Password",
    [20u8, 0x16u8] => "ItemOperations:Move",
    [20u8, 0x17u8] => "ItemOperations:DstFldId",
    [20u8, 0x18u8] => "ItemOperations:ConversationId",
    [20u8, 0x19u8] => "ItemOperations:MoveAlways",
    // Code page 21: ComposeMail ([MS-ASWBXML] v20250520 §2.1.2.1.21)
    [21u8, 0x05u8] => "ComposeMail:SendMail",
    [21u8, 0x06u8] => "ComposeMail:SmartForward",
    [21u8, 0x07u8] => "ComposeMail:SmartReply",
    [21u8, 0x08u8] => "ComposeMail:SaveInSentItems",
    [21u8, 0x09u8] => "ComposeMail:ReplaceMime",
    [21u8, 0x0Bu8] => "ComposeMail:Source",
    [21u8, 0x0Cu8] => "ComposeMail:FolderId",
    [21u8, 0x0Du8] => "ComposeMail:ItemId",
    [21u8, 0x0Eu8] => "ComposeMail:LongId",
    [21u8, 0x0Fu8] => "ComposeMail:InstanceId",
    [21u8, 0x10u8] => "ComposeMail:Mime",
    [21u8, 0x11u8] => "ComposeMail:ClientId",
    [21u8, 0x12u8] => "ComposeMail:Status",
    [21u8, 0x13u8] => "ComposeMail:AccountId",
    [21u8, 0x15u8] => "ComposeMail:Forwardees",
    [21u8, 0x16u8] => "ComposeMail:Forwardee",
    [21u8, 0x17u8] => "ComposeMail:Name",
    [21u8, 0x18u8] => "ComposeMail:Email",
    // Code page 22: Email2 ([MS-ASWBXML] v20250520 §2.1.2.1.22)
    [22u8, 0x05u8] => "Email2:UmCallerID",
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
    [22u8, 0x15u8] => "Email2:IsDraft",
    [22u8, 0x16u8] => "Email2:Bcc",
    [22u8, 0x17u8] => "Email2:Send",
    // Code page 23: Notes ([MS-ASWBXML] v20250520 §2.1.2.1.23)
    [23u8, 0x05u8] => "Notes:Subject",
    [23u8, 0x06u8] => "Notes:MessageClass",
    [23u8, 0x07u8] => "Notes:LastModifiedDate",
    [23u8, 0x08u8] => "Notes:Categories",
    [23u8, 0x09u8] => "Notes:Category",
    // Code page 24: RightsManagement ([MS-ASWBXML] v20250520 §2.1.2.1.24)
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
    [24u8, 0x12u8] => "RightsManagement:Owner",
    [24u8, 0x13u8] => "RightsManagement:ContentExpiryDate",
    [24u8, 0x14u8] => "RightsManagement:TemplateID",
    [24u8, 0x15u8] => "RightsManagement:TemplateName",
    [24u8, 0x16u8] => "RightsManagement:TemplateDescription",
    [24u8, 0x17u8] => "RightsManagement:ContentOwner",
    [24u8, 0x18u8] => "RightsManagement:RemoveRightsManagementProtection",
    // Code page 25: Find ([MS-ASWBXML] v20250520 §2.1.2.1.25)
    [25u8, 0x05u8] => "Find:Find",
    [25u8, 0x06u8] => "Find:SearchId",
    [25u8, 0x07u8] => "Find:ExecuteSearch",
    [25u8, 0x08u8] => "Find:MailBoxSearchCriterion",
    [25u8, 0x09u8] => "Find:Query",
    [25u8, 0x0Au8] => "Find:Status",
    [25u8, 0x0Bu8] => "Find:FreeText",
    [25u8, 0x0Cu8] => "Find:Options",
    [25u8, 0x0Du8] => "Find:Range",
    [25u8, 0x0Eu8] => "Find:DeepTraversal",
    [25u8, 0x11u8] => "Find:Response",
    [25u8, 0x12u8] => "Find:Result",
    [25u8, 0x13u8] => "Find:Properties",
    [25u8, 0x14u8] => "Find:Preview",
    [25u8, 0x15u8] => "Find:HasAttachments",
    [25u8, 0x16u8] => "Find:Total",
    [25u8, 0x17u8] => "Find:DisplayCc",
    [25u8, 0x18u8] => "Find:DisplayBcc",
    [25u8, 0x19u8] => "Find:GalSearchCriterion",
    [25u8, 0x20u8] => "Find:MaxPictures",
    [25u8, 0x21u8] => "Find:MaxSize",
    [25u8, 0x22u8] => "Find:Picture",
};

static NAME_TO_TAG: phf::Map<&'static str, [u8; 2]> = phf::phf_map! {
    "Sync" => [0u8, 0x05u8],
    "Responses" => [0u8, 0x06u8],
    "Add" => [0u8, 0x07u8],
    "Change" => [0u8, 0x08u8],
    "Delete" => [0u8, 0x09u8],
    "Fetch" => [0u8, 0x0Au8],
    "SyncKey" => [0u8, 0x0Bu8],
    "ClientId" => [0u8, 0x0Cu8],
    "ServerId" => [0u8, 0x0Du8],
    "Status" => [0u8, 0x0Eu8],
    "Collection" => [0u8, 0x0Fu8],
    "Class" => [0u8, 0x10u8],
    "CollectionId" => [0u8, 0x12u8],
    "GetChanges" => [0u8, 0x13u8],
    "MoreAvailable" => [0u8, 0x14u8],
    "WindowSize" => [0u8, 0x15u8],
    "Commands" => [0u8, 0x16u8],
    "Options" => [0u8, 0x17u8],
    "FilterType" => [0u8, 0x18u8],
    "Truncation" => [0u8, 0x19u8],
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
    "HeartbeatInterval" => [0u8, 0x29u8],
    "Contacts:Anniversary" => [1u8, 0x05u8],
    "Contacts:AssistantName" => [1u8, 0x06u8],
    "Contacts:AssistantPhoneNumber" => [1u8, 0x07u8],
    "Contacts:Birthday" => [1u8, 0x08u8],
    "Contacts:Body" => [1u8, 0x09u8],
    "Contacts:BodySize" => [1u8, 0x0Au8],
    "Contacts:BodyTruncated" => [1u8, 0x0Bu8],
    "Contacts:Business2PhoneNumber" => [1u8, 0x0Cu8],
    "Contacts:BusinessAddressCity" => [1u8, 0x0Du8],
    "Contacts:BusinessAddressCountry" => [1u8, 0x0Eu8],
    "Contacts:BusinessAddressPostalCode" => [1u8, 0x0Fu8],
    "Contacts:BusinessAddressState" => [1u8, 0x10u8],
    "Contacts:BusinessAddressStreet" => [1u8, 0x11u8],
    "Contacts:BusinessFaxNumber" => [1u8, 0x12u8],
    "Contacts:BusinessPhoneNumber" => [1u8, 0x13u8],
    "Contacts:CarPhoneNumber" => [1u8, 0x14u8],
    "Contacts:Categories" => [1u8, 0x15u8],
    "Contacts:Category" => [1u8, 0x16u8],
    "Contacts:Children" => [1u8, 0x17u8],
    "Contacts:Child" => [1u8, 0x18u8],
    "Contacts:CompanyName" => [1u8, 0x19u8],
    "Contacts:Department" => [1u8, 0x1Au8],
    "Contacts:Email1Address" => [1u8, 0x1Bu8],
    "Contacts:Email2Address" => [1u8, 0x1Cu8],
    "Contacts:Email3Address" => [1u8, 0x1Du8],
    "Contacts:FileAs" => [1u8, 0x1Eu8],
    "Contacts:FirstName" => [1u8, 0x1Fu8],
    "Contacts:Home2PhoneNumber" => [1u8, 0x20u8],
    "Contacts:HomeAddressCity" => [1u8, 0x21u8],
    "Contacts:HomeAddressCountry" => [1u8, 0x22u8],
    "Contacts:HomeAddressPostalCode" => [1u8, 0x23u8],
    "Contacts:HomeAddressState" => [1u8, 0x24u8],
    "Contacts:HomeAddressStreet" => [1u8, 0x25u8],
    "Contacts:HomeFaxNumber" => [1u8, 0x26u8],
    "Contacts:HomePhoneNumber" => [1u8, 0x27u8],
    "Contacts:JobTitle" => [1u8, 0x28u8],
    "Contacts:LastName" => [1u8, 0x29u8],
    "Contacts:MiddleName" => [1u8, 0x2Au8],
    "Contacts:MobilePhoneNumber" => [1u8, 0x2Bu8],
    "Contacts:OfficeLocation" => [1u8, 0x2Cu8],
    "Contacts:OtherAddressCity" => [1u8, 0x2Du8],
    "Contacts:OtherAddressCountry" => [1u8, 0x2Eu8],
    "Contacts:OtherAddressPostalCode" => [1u8, 0x2Fu8],
    "Contacts:OtherAddressState" => [1u8, 0x30u8],
    "Contacts:OtherAddressStreet" => [1u8, 0x31u8],
    "Contacts:PagerNumber" => [1u8, 0x32u8],
    "Contacts:RadioPhoneNumber" => [1u8, 0x33u8],
    "Contacts:Spouse" => [1u8, 0x34u8],
    "Contacts:Suffix" => [1u8, 0x35u8],
    "Contacts:Title" => [1u8, 0x36u8],
    "Contacts:WebPage" => [1u8, 0x37u8],
    "Contacts:YomiCompanyName" => [1u8, 0x38u8],
    "Contacts:YomiFirstName" => [1u8, 0x39u8],
    "Contacts:YomiLastName" => [1u8, 0x3Au8],
    "Contacts:Picture" => [1u8, 0x3Cu8],
    "Contacts:Alias" => [1u8, 0x3Du8],
    "Contacts:WeightedRank" => [1u8, 0x3Eu8],
    "Email:Attachment" => [2u8, 0x05u8],
    "Email:Attachments" => [2u8, 0x06u8],
    "Email:AttName" => [2u8, 0x07u8],
    "Email:AttSize" => [2u8, 0x08u8],
    "Email:Att0id" => [2u8, 0x09u8],
    "Email:AttMethod" => [2u8, 0x0Au8],
    "Email:Body" => [2u8, 0x0Cu8],
    "Email:BodySize" => [2u8, 0x0Du8],
    "Email:BodyTruncated" => [2u8, 0x0Eu8],
    "Email:DateReceived" => [2u8, 0x0Fu8],
    "Email:DisplayName" => [2u8, 0x10u8],
    "Email:DisplayTo" => [2u8, 0x11u8],
    "Email:Importance" => [2u8, 0x12u8],
    "Email:MessageClass" => [2u8, 0x13u8],
    "Email:Subject" => [2u8, 0x14u8],
    "Email:Read" => [2u8, 0x15u8],
    "Email:To" => [2u8, 0x16u8],
    "Email:Cc" => [2u8, 0x17u8],
    "Email:From" => [2u8, 0x18u8],
    "Email:ReplyTo" => [2u8, 0x19u8],
    "Email:AllDayEvent" => [2u8, 0x1Au8],
    "Email:Categories" => [2u8, 0x1Bu8],
    "Email:Category" => [2u8, 0x1Cu8],
    "Email:DtStamp" => [2u8, 0x1Du8],
    "Email:EndTime" => [2u8, 0x1Eu8],
    "Email:InstanceType" => [2u8, 0x1Fu8],
    "Email:BusyStatus" => [2u8, 0x20u8],
    "Email:Location" => [2u8, 0x21u8],
    "Email:MeetingRequest" => [2u8, 0x22u8],
    "Email:Organizer" => [2u8, 0x23u8],
    "Email:RecurrenceId" => [2u8, 0x24u8],
    "Email:Reminder" => [2u8, 0x25u8],
    "Email:ResponseRequested" => [2u8, 0x26u8],
    "Email:Recurrences" => [2u8, 0x27u8],
    "Email:Recurrence" => [2u8, 0x28u8],
    "Email:Type" => [2u8, 0x29u8],
    "Email:Until" => [2u8, 0x2Au8],
    "Email:Occurrences" => [2u8, 0x2Bu8],
    "Email:Interval" => [2u8, 0x2Cu8],
    "Email:DayOfWeek" => [2u8, 0x2Du8],
    "Email:DayOfMonth" => [2u8, 0x2Eu8],
    "Email:WeekOfMonth" => [2u8, 0x2Fu8],
    "Email:MonthOfYear" => [2u8, 0x30u8],
    "Email:StartTime" => [2u8, 0x31u8],
    "Email:Sensitivity" => [2u8, 0x32u8],
    "Email:TimeZone" => [2u8, 0x33u8],
    "Email:GlobalObjId" => [2u8, 0x34u8],
    "Email:ThreadTopic" => [2u8, 0x35u8],
    "Email:MIMEData" => [2u8, 0x36u8],
    "Email:MIMETruncated" => [2u8, 0x37u8],
    "Email:MIMESize" => [2u8, 0x38u8],
    "Email:InternetCPID" => [2u8, 0x39u8],
    "Email:Flag" => [2u8, 0x3Au8],
    "Email:Status" => [2u8, 0x3Bu8],
    "Email:ContentClass" => [2u8, 0x3Cu8],
    "Email:FlagType" => [2u8, 0x3Du8],
    "Email:CompleteTime" => [2u8, 0x3Eu8],
    "Email:DisallowNewTimeProposal" => [2u8, 0x3Fu8],
    "Calendar:Timezone" => [4u8, 0x05u8],
    "Calendar:AllDayEvent" => [4u8, 0x06u8],
    "Calendar:Attendees" => [4u8, 0x07u8],
    "Calendar:Attendee" => [4u8, 0x08u8],
    "Calendar:Email" => [4u8, 0x09u8],
    "Calendar:Name" => [4u8, 0x0Au8],
    "Calendar:Body" => [4u8, 0x0Bu8],
    "Calendar:BodyTruncated" => [4u8, 0x0Cu8],
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
    "Move:MoveItems" => [5u8, 0x05u8],
    "Move:Move" => [5u8, 0x06u8],
    "Move:SrcMsgId" => [5u8, 0x07u8],
    "Move:SrcFldId" => [5u8, 0x08u8],
    "Move:DstFldId" => [5u8, 0x09u8],
    "Move:Response" => [5u8, 0x0Au8],
    "Move:Status" => [5u8, 0x0Bu8],
    "Move:DstMsgId" => [5u8, 0x0Cu8],
    "GetItemEstimate:GetItemEstimate" => [6u8, 0x05u8],
    "GetItemEstimate:Collections" => [6u8, 0x07u8],
    "GetItemEstimate:Collection" => [6u8, 0x08u8],
    "GetItemEstimate:Class" => [6u8, 0x09u8],
    "GetItemEstimate:CollectionId" => [6u8, 0x0Au8],
    "GetItemEstimate:Estimate" => [6u8, 0x0Cu8],
    "GetItemEstimate:Response" => [6u8, 0x0Du8],
    "GetItemEstimate:Status" => [6u8, 0x0Eu8],
    "FolderHierarchy:Folders" => [7u8, 0x05u8],
    "FolderHierarchy:Folder" => [7u8, 0x06u8],
    "FolderHierarchy:DisplayName" => [7u8, 0x07u8],
    "FolderHierarchy:ServerId" => [7u8, 0x08u8],
    "FolderHierarchy:ParentId" => [7u8, 0x09u8],
    "FolderHierarchy:Type" => [7u8, 0x0Au8],
    "FolderHierarchy:Status" => [7u8, 0x0Cu8],
    "FolderHierarchy:Changes" => [7u8, 0x0Eu8],
    "FolderHierarchy:Add" => [7u8, 0x0Fu8],
    "FolderHierarchy:Delete" => [7u8, 0x10u8],
    "FolderHierarchy:Update" => [7u8, 0x11u8],
    "FolderHierarchy:SyncKey" => [7u8, 0x12u8],
    "FolderHierarchy:FolderCreate" => [7u8, 0x13u8],
    "FolderHierarchy:FolderDelete" => [7u8, 0x14u8],
    "FolderHierarchy:FolderUpdate" => [7u8, 0x15u8],
    "FolderHierarchy:FolderSync" => [7u8, 0x16u8],
    "FolderHierarchy:Count" => [7u8, 0x17u8],
    "MeetingResponse:CalendarId" => [8u8, 0x05u8],
    "MeetingResponse:CollectionId" => [8u8, 0x06u8],
    "MeetingResponse:MeetingResponse" => [8u8, 0x07u8],
    "MeetingResponse:RequestId" => [8u8, 0x08u8],
    "MeetingResponse:Request" => [8u8, 0x09u8],
    "MeetingResponse:Result" => [8u8, 0x0Au8],
    "MeetingResponse:Status" => [8u8, 0x0Bu8],
    "MeetingResponse:UserResponse" => [8u8, 0x0Cu8],
    "MeetingResponse:InstanceId" => [8u8, 0x0Eu8],
    "MeetingResponse:ProposedStartTime" => [8u8, 0x10u8],
    "MeetingResponse:ProposedEndTime" => [8u8, 0x11u8],
    "MeetingResponse:SendResponse" => [8u8, 0x12u8],
    "Tasks:Body" => [9u8, 0x05u8],
    "Tasks:BodySize" => [9u8, 0x06u8],
    "Tasks:BodyTruncated" => [9u8, 0x07u8],
    "Tasks:Categories" => [9u8, 0x08u8],
    "Tasks:Category" => [9u8, 0x09u8],
    "Tasks:Complete" => [9u8, 0x0Au8],
    "Tasks:DateCompleted" => [9u8, 0x0Bu8],
    "Tasks:DueDate" => [9u8, 0x0Cu8],
    "Tasks:UtcDueDate" => [9u8, 0x0Du8],
    "Tasks:Importance" => [9u8, 0x0Eu8],
    "Tasks:Recurrence" => [9u8, 0x0Fu8],
    "Tasks:Type" => [9u8, 0x10u8],
    "Tasks:Start" => [9u8, 0x11u8],
    "Tasks:Until" => [9u8, 0x12u8],
    "Tasks:Occurrences" => [9u8, 0x13u8],
    "Tasks:Interval" => [9u8, 0x14u8],
    "Tasks:DayOfMonth" => [9u8, 0x15u8],
    "Tasks:DayOfWeek" => [9u8, 0x16u8],
    "Tasks:WeekOfMonth" => [9u8, 0x17u8],
    "Tasks:MonthOfYear" => [9u8, 0x18u8],
    "Tasks:Regenerate" => [9u8, 0x19u8],
    "Tasks:DeadOccur" => [9u8, 0x1Au8],
    "Tasks:ReminderSet" => [9u8, 0x1Bu8],
    "Tasks:ReminderTime" => [9u8, 0x1Cu8],
    "Tasks:Sensitivity" => [9u8, 0x1Du8],
    "Tasks:StartDate" => [9u8, 0x1Eu8],
    "Tasks:UtcStartDate" => [9u8, 0x1Fu8],
    "Tasks:Subject" => [9u8, 0x20u8],
    "Tasks:OrdinalDate" => [9u8, 0x22u8],
    "Tasks:SubOrdinalDate" => [9u8, 0x23u8],
    "Tasks:CalendarType" => [9u8, 0x24u8],
    "Tasks:IsLeapMonth" => [9u8, 0x25u8],
    "Tasks:FirstDayOfWeek" => [9u8, 0x26u8],
    "ResolveRecipients:ResolveRecipients" => [10u8, 0x05u8],
    "ResolveRecipients:Response" => [10u8, 0x06u8],
    "ResolveRecipients:Status" => [10u8, 0x07u8],
    "ResolveRecipients:Type" => [10u8, 0x08u8],
    "ResolveRecipients:Recipient" => [10u8, 0x09u8],
    "ResolveRecipients:DisplayName" => [10u8, 0x0Au8],
    "ResolveRecipients:EmailAddress" => [10u8, 0x0Bu8],
    "ResolveRecipients:Certificates" => [10u8, 0x0Cu8],
    "ResolveRecipients:Certificate" => [10u8, 0x0Du8],
    "ResolveRecipients:MiniCertificate" => [10u8, 0x0Eu8],
    "ResolveRecipients:Options" => [10u8, 0x0Fu8],
    "ResolveRecipients:To" => [10u8, 0x10u8],
    "ResolveRecipients:CertificateRetrieval" => [10u8, 0x11u8],
    "ResolveRecipients:RecipientCount" => [10u8, 0x12u8],
    "ResolveRecipients:MaxCertificates" => [10u8, 0x13u8],
    "ResolveRecipients:MaxAmbiguousRecipients" => [10u8, 0x14u8],
    "ResolveRecipients:CertificateCount" => [10u8, 0x15u8],
    "ResolveRecipients:Availability" => [10u8, 0x16u8],
    "ResolveRecipients:StartTime" => [10u8, 0x17u8],
    "ResolveRecipients:EndTime" => [10u8, 0x18u8],
    "ResolveRecipients:MergedFreeBusy" => [10u8, 0x19u8],
    "ResolveRecipients:Picture" => [10u8, 0x1Au8],
    "ResolveRecipients:MaxSize" => [10u8, 0x1Bu8],
    "ResolveRecipients:Data" => [10u8, 0x1Cu8],
    "ResolveRecipients:MaxPictures" => [10u8, 0x1Du8],
    "ValidateCert:ValidateCert" => [11u8, 0x05u8],
    "ValidateCert:Certificates" => [11u8, 0x06u8],
    "ValidateCert:Certificate" => [11u8, 0x07u8],
    "ValidateCert:CertificateChain" => [11u8, 0x08u8],
    "ValidateCert:CheckCRL" => [11u8, 0x09u8],
    "ValidateCert:Status" => [11u8, 0x0Au8],
    "Contacts2:CustomerId" => [12u8, 0x05u8],
    "Contacts2:GovernmentId" => [12u8, 0x06u8],
    "Contacts2:IMAddress" => [12u8, 0x07u8],
    "Contacts2:IMAddress2" => [12u8, 0x08u8],
    "Contacts2:IMAddress3" => [12u8, 0x09u8],
    "Contacts2:ManagerName" => [12u8, 0x0Au8],
    "Contacts2:CompanyMainPhone" => [12u8, 0x0Bu8],
    "Contacts2:AccountName" => [12u8, 0x0Cu8],
    "Contacts2:NickName" => [12u8, 0x0Du8],
    "Contacts2:MMS" => [12u8, 0x0Eu8],
    "Ping:Ping" => [13u8, 0x05u8],
    "Ping:Status" => [13u8, 0x07u8],
    "Ping:HeartbeatInterval" => [13u8, 0x08u8],
    "Ping:Folders" => [13u8, 0x09u8],
    "Ping:Folder" => [13u8, 0x0Au8],
    "Ping:Id" => [13u8, 0x0Bu8],
    "Ping:Class" => [13u8, 0x0Cu8],
    "Ping:MaxFolders" => [13u8, 0x0Du8],
    "Provision:Provision" => [14u8, 0x05u8],
    "Provision:Policies" => [14u8, 0x06u8],
    "Provision:Policy" => [14u8, 0x07u8],
    "Provision:PolicyType" => [14u8, 0x08u8],
    "Provision:PolicyKey" => [14u8, 0x09u8],
    "Provision:Data" => [14u8, 0x0Au8],
    "Provision:Status" => [14u8, 0x0Bu8],
    "Provision:RemoteWipe" => [14u8, 0x0Cu8],
    "Provision:EASProvisionDoc" => [14u8, 0x0Du8],
    "Provision:DevicePasswordEnabled" => [14u8, 0x0Eu8],
    "Provision:AlphanumericDevicePasswordRequired" => [14u8, 0x0Fu8],
    "Provision:RequireStorageCardEncryption" => [14u8, 0x10u8],
    "Provision:PasswordRecoveryEnabled" => [14u8, 0x11u8],
    "Provision:AttachmentsEnabled" => [14u8, 0x13u8],
    "Provision:MinDevicePasswordLength" => [14u8, 0x14u8],
    "Provision:MaxInactivityTimeDeviceLock" => [14u8, 0x15u8],
    "Provision:MaxDevicePasswordFailedAttempts" => [14u8, 0x16u8],
    "Provision:MaxAttachmentSize" => [14u8, 0x17u8],
    "Provision:AllowSimpleDevicePassword" => [14u8, 0x18u8],
    "Provision:DevicePasswordExpiration" => [14u8, 0x19u8],
    "Provision:DevicePasswordHistory" => [14u8, 0x1Au8],
    "Provision:AllowStorageCard" => [14u8, 0x1Bu8],
    "Provision:AllowCamera" => [14u8, 0x1Cu8],
    "Provision:RequireDeviceEncryption" => [14u8, 0x1Du8],
    "Provision:AllowUnsignedApplications" => [14u8, 0x1Eu8],
    "Provision:AllowUnsignedInstallationPackages" => [14u8, 0x1Fu8],
    "Provision:MinDevicePasswordComplexCharacters" => [14u8, 0x20u8],
    "Provision:AllowWiFi" => [14u8, 0x21u8],
    "Provision:AllowTextMessaging" => [14u8, 0x22u8],
    "Provision:AllowPOPIMAPEmail" => [14u8, 0x23u8],
    "Provision:AllowBluetooth" => [14u8, 0x24u8],
    "Provision:AllowIrDA" => [14u8, 0x25u8],
    "Provision:RequireManualSyncWhenRoaming" => [14u8, 0x26u8],
    "Provision:AllowDesktopSync" => [14u8, 0x27u8],
    "Provision:MaxCalendarAgeFilter" => [14u8, 0x28u8],
    "Provision:AllowHTMLEmail" => [14u8, 0x29u8],
    "Provision:MaxEmailAgeFilter" => [14u8, 0x2Au8],
    "Provision:MaxEmailBodyTruncationSize" => [14u8, 0x2Bu8],
    "Provision:MaxEmailHTMLBodyTruncationSize" => [14u8, 0x2Cu8],
    "Provision:RequireSignedSMIMEMessages" => [14u8, 0x2Du8],
    "Provision:RequireEncryptedSMIMEMessages" => [14u8, 0x2Eu8],
    "Provision:RequireSignedSMIMEAlgorithm" => [14u8, 0x2Fu8],
    "Provision:RequireEncryptionSMIMEAlgorithm" => [14u8, 0x30u8],
    "Provision:AllowSMIMEEncryptionAlgorithmNegotiation" => [14u8, 0x31u8],
    "Provision:AllowSMIMESoftCerts" => [14u8, 0x32u8],
    "Provision:AllowBrowser" => [14u8, 0x33u8],
    "Provision:AllowConsumerEmail" => [14u8, 0x34u8],
    "Provision:AllowRemoteDesktop" => [14u8, 0x35u8],
    "Provision:AllowInternetSharing" => [14u8, 0x36u8],
    "Provision:UnapprovedInROMApplicationList" => [14u8, 0x37u8],
    "Provision:ApplicationName" => [14u8, 0x38u8],
    "Provision:ApprovedApplicationList" => [14u8, 0x39u8],
    "Provision:Hash" => [14u8, 0x3Au8],
    "Provision:AccountOnlyRemoteWipe" => [14u8, 0x3Bu8],
    "Search:Search" => [15u8, 0x05u8],
    "Search:Store" => [15u8, 0x07u8],
    "Search:Name" => [15u8, 0x08u8],
    "Search:Query" => [15u8, 0x09u8],
    "Search:Options" => [15u8, 0x0Au8],
    "Search:Range" => [15u8, 0x0Bu8],
    "Search:Status" => [15u8, 0x0Cu8],
    "Search:Response" => [15u8, 0x0Du8],
    "Search:Result" => [15u8, 0x0Eu8],
    "Search:Properties" => [15u8, 0x0Fu8],
    "Search:Total" => [15u8, 0x10u8],
    "Search:EqualTo" => [15u8, 0x11u8],
    "Search:Value" => [15u8, 0x12u8],
    "Search:And" => [15u8, 0x13u8],
    "Search:Or" => [15u8, 0x14u8],
    "Search:FreeText" => [15u8, 0x15u8],
    "Search:DeepTraversal" => [15u8, 0x17u8],
    "Search:LongId" => [15u8, 0x18u8],
    "Search:RebuildResults" => [15u8, 0x19u8],
    "Search:LessThan" => [15u8, 0x1Au8],
    "Search:GreaterThan" => [15u8, 0x1Bu8],
    "Search:UserName" => [15u8, 0x1Eu8],
    "Search:Password" => [15u8, 0x1Fu8],
    "Search:ConversationId" => [15u8, 0x20u8],
    "Search:Picture" => [15u8, 0x21u8],
    "Search:MaxSize" => [15u8, 0x22u8],
    "Search:MaxPictures" => [15u8, 0x23u8],
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
    "Settings:Settings" => [18u8, 0x05u8],
    "Settings:Status" => [18u8, 0x06u8],
    "Settings:Get" => [18u8, 0x07u8],
    "Settings:Set" => [18u8, 0x08u8],
    "Settings:Oof" => [18u8, 0x09u8],
    "Settings:OofState" => [18u8, 0x0Au8],
    "Settings:StartTime" => [18u8, 0x0Bu8],
    "Settings:EndTime" => [18u8, 0x0Cu8],
    "Settings:OofMessage" => [18u8, 0x0Du8],
    "Settings:AppliesToInternal" => [18u8, 0x0Eu8],
    "Settings:AppliesToExternalKnown" => [18u8, 0x0Fu8],
    "Settings:AppliesToExternalUnknown" => [18u8, 0x10u8],
    "Settings:Enabled" => [18u8, 0x11u8],
    "Settings:ReplyMessage" => [18u8, 0x12u8],
    "Settings:BodyType" => [18u8, 0x13u8],
    "Settings:DevicePassword" => [18u8, 0x14u8],
    "Settings:Password" => [18u8, 0x15u8],
    "Settings:DeviceInformation" => [18u8, 0x16u8],
    "Settings:Model" => [18u8, 0x17u8],
    "Settings:IMEI" => [18u8, 0x18u8],
    "Settings:FriendlyName" => [18u8, 0x19u8],
    "Settings:OS" => [18u8, 0x1Au8],
    "Settings:OSLanguage" => [18u8, 0x1Bu8],
    "Settings:PhoneNumber" => [18u8, 0x1Cu8],
    "Settings:UserInformation" => [18u8, 0x1Du8],
    "Settings:EmailAddresses" => [18u8, 0x1Eu8],
    "Settings:SMTPAddress" => [18u8, 0x1Fu8],
    "Settings:UserAgent" => [18u8, 0x20u8],
    "Settings:EnableOutboundSMS" => [18u8, 0x21u8],
    "Settings:MobileOperator" => [18u8, 0x22u8],
    "Settings:PrimarySmtpAddress" => [18u8, 0x23u8],
    "Settings:Accounts" => [18u8, 0x24u8],
    "Settings:Account" => [18u8, 0x25u8],
    "Settings:AccountId" => [18u8, 0x26u8],
    "Settings:AccountName" => [18u8, 0x27u8],
    "Settings:UserDisplayName" => [18u8, 0x28u8],
    "Settings:SendDisabled" => [18u8, 0x29u8],
    "Settings:RightsManagementInformation" => [18u8, 0x2Bu8],
    "DocumentLibrary:LinkId" => [19u8, 0x05u8],
    "DocumentLibrary:DisplayName" => [19u8, 0x06u8],
    "DocumentLibrary:IsFolder" => [19u8, 0x07u8],
    "DocumentLibrary:CreationDate" => [19u8, 0x08u8],
    "DocumentLibrary:LastModifiedDate" => [19u8, 0x09u8],
    "DocumentLibrary:IsHidden" => [19u8, 0x0Au8],
    "DocumentLibrary:ContentLength" => [19u8, 0x0Bu8],
    "DocumentLibrary:ContentType" => [19u8, 0x0Cu8],
    "ItemOperations:ItemOperations" => [20u8, 0x05u8],
    "ItemOperations:Fetch" => [20u8, 0x06u8],
    "ItemOperations:Store" => [20u8, 0x07u8],
    "ItemOperations:Options" => [20u8, 0x08u8],
    "ItemOperations:Range" => [20u8, 0x09u8],
    "ItemOperations:Total" => [20u8, 0x0Au8],
    "ItemOperations:Properties" => [20u8, 0x0Bu8],
    "ItemOperations:Data" => [20u8, 0x0Cu8],
    "ItemOperations:Status" => [20u8, 0x0Du8],
    "ItemOperations:Response" => [20u8, 0x0Eu8],
    "ItemOperations:Version" => [20u8, 0x0Fu8],
    "ItemOperations:Schema" => [20u8, 0x10u8],
    "ItemOperations:Part" => [20u8, 0x11u8],
    "ItemOperations:EmptyFolderContents" => [20u8, 0x12u8],
    "ItemOperations:DeleteSubFolders" => [20u8, 0x13u8],
    "ItemOperations:UserName" => [20u8, 0x14u8],
    "ItemOperations:Password" => [20u8, 0x15u8],
    "ItemOperations:Move" => [20u8, 0x16u8],
    "ItemOperations:DstFldId" => [20u8, 0x17u8],
    "ItemOperations:ConversationId" => [20u8, 0x18u8],
    "ItemOperations:MoveAlways" => [20u8, 0x19u8],
    "ComposeMail:SendMail" => [21u8, 0x05u8],
    "ComposeMail:SmartForward" => [21u8, 0x06u8],
    "ComposeMail:SmartReply" => [21u8, 0x07u8],
    "ComposeMail:SaveInSentItems" => [21u8, 0x08u8],
    "ComposeMail:ReplaceMime" => [21u8, 0x09u8],
    "ComposeMail:Source" => [21u8, 0x0Bu8],
    "ComposeMail:FolderId" => [21u8, 0x0Cu8],
    "ComposeMail:ItemId" => [21u8, 0x0Du8],
    "ComposeMail:LongId" => [21u8, 0x0Eu8],
    "ComposeMail:InstanceId" => [21u8, 0x0Fu8],
    "ComposeMail:Mime" => [21u8, 0x10u8],
    "ComposeMail:ClientId" => [21u8, 0x11u8],
    "ComposeMail:Status" => [21u8, 0x12u8],
    "ComposeMail:AccountId" => [21u8, 0x13u8],
    "ComposeMail:Forwardees" => [21u8, 0x15u8],
    "ComposeMail:Forwardee" => [21u8, 0x16u8],
    "ComposeMail:Name" => [21u8, 0x17u8],
    "ComposeMail:Email" => [21u8, 0x18u8],
    "Email2:UmCallerID" => [22u8, 0x05u8],
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
    "Email2:IsDraft" => [22u8, 0x15u8],
    "Email2:Bcc" => [22u8, 0x16u8],
    "Email2:Send" => [22u8, 0x17u8],
    "Notes:Subject" => [23u8, 0x05u8],
    "Notes:MessageClass" => [23u8, 0x06u8],
    "Notes:LastModifiedDate" => [23u8, 0x07u8],
    "Notes:Categories" => [23u8, 0x08u8],
    "Notes:Category" => [23u8, 0x09u8],
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
    "RightsManagement:Owner" => [24u8, 0x12u8],
    "RightsManagement:ContentExpiryDate" => [24u8, 0x13u8],
    "RightsManagement:TemplateID" => [24u8, 0x14u8],
    "RightsManagement:TemplateName" => [24u8, 0x15u8],
    "RightsManagement:TemplateDescription" => [24u8, 0x16u8],
    "RightsManagement:ContentOwner" => [24u8, 0x17u8],
    "RightsManagement:RemoveRightsManagementProtection" => [24u8, 0x18u8],
    "Find:Find" => [25u8, 0x05u8],
    "Find:SearchId" => [25u8, 0x06u8],
    "Find:ExecuteSearch" => [25u8, 0x07u8],
    "Find:MailBoxSearchCriterion" => [25u8, 0x08u8],
    "Find:Query" => [25u8, 0x09u8],
    "Find:Status" => [25u8, 0x0Au8],
    "Find:FreeText" => [25u8, 0x0Bu8],
    "Find:Options" => [25u8, 0x0Cu8],
    "Find:Range" => [25u8, 0x0Du8],
    "Find:DeepTraversal" => [25u8, 0x0Eu8],
    "Find:Response" => [25u8, 0x11u8],
    "Find:Result" => [25u8, 0x12u8],
    "Find:Properties" => [25u8, 0x13u8],
    "Find:Preview" => [25u8, 0x14u8],
    "Find:HasAttachments" => [25u8, 0x15u8],
    "Find:Total" => [25u8, 0x16u8],
    "Find:DisplayCc" => [25u8, 0x17u8],
    "Find:DisplayBcc" => [25u8, 0x18u8],
    "Find:GalSearchCriterion" => [25u8, 0x19u8],
    "Find:MaxPictures" => [25u8, 0x20u8],
    "Find:MaxSize" => [25u8, 0x21u8],
    "Find:Picture" => [25u8, 0x22u8],
};

/// Map a namespace URI (the `xmlns`/`xmlns:*` value) to its [MS-ASWBXML]
/// code page. Code pages ARE namespaces: page 0 is AirSync, 1 Contacts, …
/// Returns `None` for any URI this profile does not support.
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
        "Find:" => Some(25),
        _ => None,
    }
}

/// Resolve a namespace URI to its WBXML code page. [MS-ASWBXML] §3 writes the
/// namespaces of its worked example without the trailing colon
/// (`xmlns="AirSync"`, `xmlns:airsyncbase="AirSyncBase"`), while the command
/// reference sections use the colon form ("AirSyncBase:"); both resolve to the
/// same code page.
fn namespace_uri_to_code_page(uri: &str) -> Option<u8> {
    namespace_to_code_page(uri).or_else(|| {
        let with_colon = format!("{uri}:");
        namespace_to_code_page(&with_colon)
    })
}

/// Resolve an element name to its (code page, token) pair for encoding.
/// A qualified name ("Contacts:NickName") is authoritative and resolves
/// exactly, independent of the ambient namespace hint; an unqualified name
/// resolves against the hinted code page when one is given, else by its
/// unique local name across all pages. Returns `None` when the name is not
/// a [MS-ASWBXML] token at all (the caller turns that into an error).
fn find_encode_tag(qualified_or_local: &str, override_cp: Option<u8>) -> Option<(u8, u8)> {
    // A qualified name ("Contacts:NickName") is authoritative: resolve it
    // exactly, independent of the ambient namespace hint. This lets legacy
    // alias entries (e.g. Contacts:NickName -> Contacts2 code page) encode
    // correctly even inside a Contacts-namespaced subtree.
    if qualified_or_local.contains(':')
        && let Some(&pair) = NAME_TO_TAG.get(qualified_or_local)
    {
        return Some((pair[0], pair[1]));
    }

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

/// WBXML codec for the [MS-ASWBXML] v20250520 profile: the only wire format
/// Exchange ActiveSync clients speak. `decode` turns a WBXML request body
/// into the in-memory XML this gateway's handlers parse; `encode` turns a
/// response template back into WBXML bytes. Both directions are fail-closed
/// per [MS-ASWBXML] §2.1.3 (see `decode`/`encode` for the exact contracts).
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

    /// Read a multi-byte unsigned integer ([WBXML1.2] §8.1.2.1): 7 bits of
    /// payload per byte, high bit set on every byte but the last.
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

    /// Read an inline string token's payload: raw bytes up to (not
    /// including) the terminating NUL, which is consumed as UTF-8.
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

    /// Decode a WBXML body into this gateway's in-memory XML form.
    ///
    /// Fail-closed per [MS-ASWBXML] §2.1.3: the algorithm uses no string
    /// tables, entities, processing instructions, or attribute encoding, so
    /// the corresponding [WBXML1.2] global tokens and tag tokens carrying
    /// the attribute bit are errors, as are unknown (code page, token)
    /// pairs, data outside the single root element, and truncated or
    /// header-only bodies. Two tokens are accepted leniently despite not
    /// being produced by Exchange: `ENTITY` (0x02), a legal WBXML character
    /// reference, and `STR_T` (0x83), which can only index the (always
    /// empty in this profile) string table. A body already starting with
    /// `<` is passed through unchanged (the callers' plain-XML escape
    /// hatch). The decoded document's root code page is its default
    /// namespace: root-page tags expand unqualified, SWITCH_PAGE-reached
    /// tags keep their `Prefix:` form.
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
        // The code page the root element was decoded on: the document's
        // default namespace (see the tag-emission note below).
        let mut root_code_page: Option<u8> = None;
        let mut seen_root = false;
        let mut xml_stack: Vec<String> = Vec::new();
        let mut output = String::from("<?xml version=\"1.0\" encoding=\"utf-8\"?>\n");

        while pos < bytes.len() {
            // A WBXML document's body is exactly one element; character data
            // outside it — after the root closes, or before the root opens —
            // is malformed, not content to silently parse.
            if seen_root && xml_stack.is_empty() {
                return Err(anyhow!("WBXML data after root element"));
            }
            let token = bytes[pos];
            pos += 1;
            if !seen_root && matches!(token, STR_I | STR_T | ENTITY | OPAQUE) {
                return Err(anyhow!("WBXML data before root element"));
            }

            match token {
                SWITCH_PAGE => {
                    if pos >= bytes.len() {
                        return Err(anyhow!("WBXML SWITCH_PAGE missing code page"));
                    }
                    current_code_page = bytes[pos];
                    pos += 1;
                }
                END => {
                    let Some(tag) = xml_stack.pop() else {
                        return Err(anyhow!("WBXML END with no open element"));
                    };
                    output.push_str(&format!("</{tag}>"));
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
                // [MS-ASWBXML] §2.1.3: the algorithm does not use string
                // tables, entities, processing instructions, or attribute
                // encoding. The corresponding [WBXML1.2] global tokens
                // (EXT_I_0/1/2, PI, LITERAL_C, EXT_T_0/1/2, LITERAL_A,
                // EXT_0/1/2, LITERAL_AC) and tag tokens carrying the
                // attribute bit are rejected instead of being guessed at;
                // ENTITY (0x02) and STR_T (0x83) are accepted leniently
                // (see the method doc) though Exchange never emits them.
                0x40..=0x44 => {
                    return Err(anyhow!(
                        "WBXML token 0x{:02x} (EXT_I/PI/LITERAL_C) is not used by [MS-ASWBXML] \u{a7}2.1.3",
                        token
                    ));
                }
                0x80..=0x82 => {
                    return Err(anyhow!(
                        "WBXML token 0x{:02x} (EXT_T) is not used by [MS-ASWBXML] \u{a7}2.1.3",
                        token
                    ));
                }
                0xC0..=0xC2 => {
                    return Err(anyhow!(
                        "WBXML token 0x{:02x} (EXT) is not used by [MS-ASWBXML] \u{a7}2.1.3",
                        token
                    ));
                }
                0x84 | 0xC4 => {
                    return Err(anyhow!(
                        "WBXML token 0x{:02x} (LITERAL_A/LITERAL_AC) is not used by [MS-ASWBXML] \u{a7}2.1.3",
                        token
                    ));
                }
                _ => {
                    if (token & 0x80) != 0 {
                        return Err(anyhow!(
                            "WBXML tag token 0x{:02x} carries attributes, but attribute encoding is not used by [MS-ASWBXML] \u{a7}2.1.3",
                            token
                        ));
                    }
                    let has_content = (token & 0x40) != 0;
                    let tag_id = token & 0x3F;
                    let Some(name) = TAG_TO_NAME.get(&[current_code_page, tag_id]) else {
                        return Err(anyhow!(
                            "WBXML decode: unknown tag code page {} token 0x{:02x}",
                            current_code_page,
                            tag_id
                        ));
                    };
                    root_code_page.get_or_insert(current_code_page);
                    seen_root = true;
                    // WBXML code pages ARE namespaces: the document's
                    // root page is its default namespace, so tags on
                    // that page expand UNQUALIFIED while tags reached
                    // through a SWITCH_PAGE keep their prefix
                    // (e.g. `<AirSyncBase:Body>` inside a Sync
                    // document rooted on the AirSync page). This
                    // matches the XML a real Exchange WBXML-to-XML
                    // expansion produces.
                    let display_name = if current_code_page == root_code_page.unwrap_or(u8::MAX) {
                        name.rsplit(':').next().unwrap_or(name)
                    } else {
                        name
                    };
                    output.push_str(&format!("<{display_name}>"));
                    if has_content {
                        xml_stack.push(display_name.to_string());
                    } else {
                        output.push_str(&format!("</{display_name}>"));
                    }
                }
            }
        }

        if !seen_root {
            return Err(anyhow!("WBXML document has no root element"));
        }
        if let Some(outermost) = xml_stack.first() {
            if xml_stack.len() > 1 {
                return Err(anyhow!(
                    "Truncated WBXML: {} unclosed elements, outermost <{outermost}>",
                    xml_stack.len()
                ));
            }
            return Err(anyhow!("Truncated WBXML: unclosed element <{outermost}>"));
        }

        Ok(output)
    }

    /// Encode the in-memory XML form into WBXML bytes.
    ///
    /// Fail-closed symmetrically with `decode`: processing instructions and
    /// DOCTYPE cannot be represented and are errors; an element's only
    /// representable attributes are namespace declarations (any other
    /// attribute, a malformed or duplicated attribute, or a declared
    /// namespace URI that maps to no [MS-ASWBXML] code page is an error,
    /// never silently dropped); character data outside the document
    /// element is an error. Character content is aggregated per element —
    /// split Text/CData/entity-reference segments encode as one STR_I, or
    /// one OPAQUE carrying the raw bytes for byte-array-typed elements
    /// ([MS-ASDTYPE] §2.7.1). An undeclared prefix that names a canonical
    /// [MS-ASWBXML] namespace still resolves to its own code page so
    /// decode output re-encodes without re-declaration; unqualified
    /// descendants fall back to the root's code page.
    pub fn encode(&self, xml: &str) -> Result<Vec<u8>> {
        let mut buf: Vec<u8> = vec![0x03, 0x01, 0x6A, 0x00];
        let mut current_code_page = 0u8;
        // The root element's code page once it resolves. A decoded EAS
        // document carries no xmlns declarations, yet by the decode
        // convention its root page IS the document's default namespace, so
        // unqualified descendant names resolve against it.
        let mut implicit_root_cp: Option<u8> = None;
        let mut ns_stack: Vec<Option<u8>> = Vec::new();
        let mut prefix_ns_stack: Vec<std::collections::HashMap<String, Option<u8>>> = Vec::new();
        // Parallel stack: whether each open element carries a byte-array value
        // that must be OPAQUE-encoded ([MS-ASDTYPE] §2.7.1).
        let mut byte_array_stack: Vec<bool> = Vec::new();
        // Character content of the current element, accumulated across
        // quick-xml's Text/CData/GeneralRef events and flushed as a single
        // WBXML token when the element boundary is reached: a byte-array
        // element's base64 can arrive split (CDATA + text, entity refs),
        // and per-event encoding would emit one OPAQUE or STR_I per
        // segment instead of one token per element.
        let mut pending_text = String::new();

        let mut reader = quick_xml::Reader::from_str(xml);
        reader.config_mut().trim_text(true);
        let mut event_buf = Vec::new();

        loop {
            match reader.read_event_into(&mut event_buf) {
                Ok(quick_xml::events::Event::Start(ref e)) => {
                    write_pending_content(&mut buf, &byte_array_stack, &mut pending_text)?;
                    let at_root = ns_stack.is_empty();
                    let (new_prefixes, ns_cp) = encode_namespace_attributes(e)?;
                    prefix_ns_stack.push(new_prefixes);
                    ns_stack.push(ns_cp);

                    let qname = e.name();
                    let full_name = qname.as_ref();
                    let (local_name, effective_cp) = if let Some(pos) = full_name.find(':') {
                        let prefix = &full_name[..pos];
                        let local = &full_name[pos + 1..];
                        let prefix_cp = match resolve_prefix_binding(prefix, &prefix_ns_stack)? {
                            Some(cp) => Some(cp),
                            // An undeclared prefix that names a [MS-ASWBXML]
                            // namespace (the canonical prefixes this codec's
                            // own decode output uses, e.g. "AirSyncBase")
                            // resolves to that code page, so a decoded
                            // document re-encodes without re-declaration.
                            None => namespace_uri_to_code_page(prefix),
                        };
                        (
                            local,
                            prefix_cp
                                .or(ns_cp)
                                .or_else(|| ns_stack.iter().rev().find_map(|&x| x)),
                        )
                    } else {
                        (
                            full_name,
                            ns_cp
                                .or_else(|| ns_stack.iter().rev().find_map(|&x| x))
                                .or(implicit_root_cp),
                        )
                    };

                    // Track byte-array-typed elements: their content MUST be
                    // transmitted as WBXML OPAQUE data with raw bytes
                    // ([MS-ASDTYPE] §2.7.1), not as an inline base64 string.
                    let resolved = find_encode_tag(local_name, effective_cp);
                    if at_root && let Some((cp, _)) = resolved {
                        implicit_root_cp = Some(cp);
                    }
                    byte_array_stack
                        .push(resolved.is_some_and(|(cp, token)| is_byte_array_element(cp, token)));

                    self.encode_open_tag(
                        &mut buf,
                        &mut current_code_page,
                        local_name,
                        effective_cp,
                        true,
                    )?;
                }
                Ok(quick_xml::events::Event::Empty(ref e)) => {
                    write_pending_content(&mut buf, &byte_array_stack, &mut pending_text)?;
                    let (new_prefixes, ns_cp) = encode_namespace_attributes(e)?;
                    prefix_ns_stack.push(new_prefixes);

                    let qname = e.name();
                    let full_name = qname.as_ref();
                    let (local_name, effective_cp) = if let Some(pos) = full_name.find(':') {
                        let prefix = &full_name[..pos];
                        let local = &full_name[pos + 1..];
                        let prefix_cp = match resolve_prefix_binding(prefix, &prefix_ns_stack)? {
                            Some(cp) => Some(cp),
                            // Undeclared canonical prefixes resolve to their
                            // own code page (see the Start arm note).
                            None => namespace_uri_to_code_page(prefix),
                        };
                        (
                            local,
                            prefix_cp
                                .or(ns_cp)
                                .or_else(|| ns_stack.iter().rev().find_map(|&x| x)),
                        )
                    } else {
                        (
                            full_name,
                            ns_cp
                                .or_else(|| ns_stack.iter().rev().find_map(|&x| x))
                                .or(implicit_root_cp),
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
                    if ns_stack.is_empty() && !e.is_empty() {
                        return Err(anyhow!(
                            "XML encode error: character data outside the document element"
                        ));
                    }
                    pending_text.push_str(e.as_ref());
                }
                Ok(quick_xml::events::Event::GeneralRef(ref r)) => {
                    let text = resolve_xml_reference_strict(r.as_ref()).ok_or_else(|| {
                        anyhow!(
                            "XML encode error: unsupported entity reference &{};",
                            r.as_ref()
                        )
                    })?;
                    if ns_stack.is_empty() {
                        return Err(anyhow!(
                            "XML encode error: character data outside the document element"
                        ));
                    }
                    pending_text.push_str(&text);
                }
                Ok(quick_xml::events::Event::CData(ref c)) => {
                    // CDATA carries the same character content as a text
                    // node, only unescaped in source form.
                    let content = String::from_utf8(c.as_ref().as_bytes().to_vec())
                        .map_err(|e| anyhow!("XML encode error: invalid CDATA UTF-8: {e}"))?;
                    if ns_stack.is_empty() && !content.is_empty() {
                        return Err(anyhow!(
                            "XML encode error: character data outside the document element"
                        ));
                    }
                    pending_text.push_str(&content);
                }
                // WBXML has no representation for processing instructions or
                // a DTD; silently dropping them would lose document meaning.
                Ok(quick_xml::events::Event::PI(ref pi)) => {
                    return Err(anyhow!(
                        "XML encode error: processing instruction <?{}?> cannot be represented in WBXML",
                        pi.as_ref()
                    ));
                }
                Ok(quick_xml::events::Event::DocType(ref d)) => {
                    return Err(anyhow!(
                        "XML encode error: DOCTYPE cannot be represented in WBXML: {}",
                        d.as_ref()
                    ));
                }
                Ok(quick_xml::events::Event::End(_)) => {
                    // The pending content belongs to the element being
                    // closed, so flush before popping its byte-array flag.
                    write_pending_content(&mut buf, &byte_array_stack, &mut pending_text)?;
                    ns_stack.pop();
                    prefix_ns_stack.pop();
                    byte_array_stack.pop();
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

    /// Emit one element's open tag: resolve the name to its (code page,
    /// token) pair — with a page switch first when the element lives on
    /// another page — and set the content bit ([WBXML1.2] §8.1) when the
    /// element has a matching End token to come. Unknown names are errors.
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

/// Namespace-declaration map: prefix -> its code page, `None` when the
/// declared URI is not a [MS-ASWBXML] namespace (an error when used).
type PrefixBindings = std::collections::HashMap<String, Option<u8>>;

/// Collect the namespace bindings an element's attributes declare.
///
/// [MS-ASWBXML] §2.1.3 defines no attribute encoding, so an element's only
/// representable attributes are its namespace declarations; every other
/// attribute, a malformed attribute, or a duplicated attribute name is a
/// hard error rather than silent data loss on the wire. Namespace URIs
/// that map to no [MS-ASWBXML] code page are also hard errors — an
/// explicitly declared-but-unsupported namespace must not be silently
/// re-encoded under a different page. Returns the `xmlns:*` prefix
/// bindings (innermost scope) and the default-namespace code page.
fn encode_namespace_attributes(
    e: &quick_xml::events::BytesStart<'_>,
) -> Result<(PrefixBindings, Option<u8>)> {
    let mut prefixes: PrefixBindings = std::collections::HashMap::new();
    let mut default_cp: Option<u8> = None;
    let mut seen_keys: Vec<String> = Vec::new();
    for attr in e.attributes() {
        let attr = attr.map_err(|err| anyhow!("XML encode error: malformed attribute: {err}"))?;
        let key = attr.key.as_ref().to_string();
        if seen_keys.contains(&key) {
            return Err(anyhow!("XML encode error: duplicate attribute '{key}'"));
        }
        seen_keys.push(key.clone());
        let value = attr
            .normalized_value(quick_xml::XmlVersion::Implicit1_0)
            .map_err(|err| anyhow!("XML encode error: invalid namespace value '{key}': {err}"))?;
        if key == "xmlns" {
            // The default namespace applies to this element immediately,
            // so an unrecognized URI is unrepresentable right here.
            default_cp = Some(namespace_uri_to_code_page(value.as_ref()).ok_or_else(|| {
                anyhow!(
                    "XML encode error: default namespace URI '{}' is not a [MS-ASWBXML] namespace",
                    value
                )
            })?);
        } else if key.starts_with("xmlns:") && key.len() > "xmlns:".len() {
            let prefix = key["xmlns:".len()..].to_string();
            prefixes.insert(prefix, namespace_uri_to_code_page(value.as_ref()));
        } else {
            return Err(anyhow!(
                "XML encode error: attribute '{key}' cannot be represented in WBXML: [MS-ASWBXML] \u{a7}2.1.3 defines no attribute encoding"
            ));
        }
    }
    Ok((prefixes, default_cp))
}

/// Resolve a prefix that is in scope for an element to its code page.
///
/// The innermost declaration wins (XML scoping). `Ok(Some(cp))` when the
/// binding names a known namespace; `Ok(None)` when the prefix is genuinely
/// undeclared (the canonical-prefix fallback in the caller applies); and
/// `Err` when the prefix is explicitly bound to a namespace URI this codec
/// does not support — an unrepresentable binding must never be silently
/// overridden by the canonical-prefix or ambient-page fallbacks.
fn resolve_prefix_binding(
    prefix: &str,
    prefix_ns_stack: &[std::collections::HashMap<String, Option<u8>>],
) -> Result<Option<u8>> {
    match prefix_ns_stack
        .iter()
        .rev()
        .find_map(|map| map.get(prefix).copied())
    {
        Some(Some(cp)) => Ok(Some(cp)),
        Some(None) => Err(anyhow!(
            "XML encode error: prefix '{prefix}' is declared with a namespace URI that is not a [MS-ASWBXML] namespace"
        )),
        None => Ok(None),
    }
}

/// Byte-array-typed elements whose content MUST be transmitted as WBXML
/// OPAQUE data carrying raw bytes ([MS-ASDTYPE] §2.7.1: "Elements with a
/// byte array structure MUST be encoded and transmitted as [WBXML1.2] opaque
/// data"), with the in-memory XML representation holding the base64 text.
/// The inventory is the complete set of elements the EAS specs type as a byte
/// array; `wbxml_conformance_byte_array_inventory_matches_specs` pins it
/// against the spec text:
///
/// - AirSyncBase:Content ([MS-ASAIRS] §2.2.2.15: "string data type byte
///   array"), code page 17 token 0x1F.
/// - GAL:Data ([MS-ASCMD] §2.2.3.39.1/§2.2.3.39.3/§2.2.3.39.4, the binary
///   contact photo data in Find/ResolveRecipients/Search responses), code
///   page 16 token 0x12.
/// - ComposeMail:Mime ([MS-ASCMD] §2.2.3.109: "transferred as an opaque BLOB
///   within the WBXML tags"), code page 21 token 0x10.
/// - ItemOperations:ConversationId ([MS-ASCON] §2.2.2.3.1), Search:ConversationId
///   (§2.2.2.3.2), Email2:ConversationId (§2.2.2.3.3; also [MS-ASEMAIL]
///   §2.2.2.21 "transferred as an opaque binary large object"), and
///   Email2:ConversationIndex (§2.2.2.4; [MS-ASEMAIL] §2.2.2.22), code pages
///   20/15/22 tokens 0x18/0x20/0x09/0x0A.
///
/// Elements that merely carry base64 of binary data but are typed "string"
/// (§2.7) stay inline strings: ItemOperations:Data ([MS-ASCMD] §2.2.3.39.2
/// attachment/document fetch content), Contacts:Picture ([MS-ASCNTC]
/// §2.2.2.58 contact photo), and the ResolveRecipients/ValidateCert
/// certificate elements. Email:GlobalObjId is included deliberately: it is
/// not typed byte array, but its value is a raw binary structure
/// ([MS-ASEMAIL] §2.2.2.37 ABNF) that clients transmit inside an opaque
/// BLOB, and the pair keeps encode/decode symmetric for it; note the spec
/// retires the element in favor of Calendar:UID at protocol version 16.0+.
fn is_byte_array_element(code_page: u8, token: u8) -> bool {
    // Byte-array elements ([MS-ASDTYPE] §2.7.1) encode as WBXML OPAQUE with
    // the raw bytes; the XML form carries the same bytes base64-encoded.
    matches!(
        (code_page, token),
        (2, 0x34)     // Email:GlobalObjId ([MS-ASEMAIL] §2.2.2.37 raw-binary ABNF)
            | (15, 0x20) // Search:ConversationId ([MS-ASCON] §2.2.2.3.2)
            | (16, 0x12) // GAL:Data ([MS-ASCMD] §2.2.3.39.1/3/4)
            | (17, 0x1F) // AirSyncBase:Content ([MS-ASAIRS] §2.2.2.15)
            | (20, 0x18) // ItemOperations:ConversationId ([MS-ASCON] §2.2.2.3.1)
            | (21, 0x10) // ComposeMail:Mime ([MS-ASCMD] §2.2.3.109)
            | (22, 0x09) // Email2:ConversationId ([MS-ASEMAIL] §2.2.2.21)
            | (22, 0x0A) // Email2:ConversationIndex ([MS-ASEMAIL] §2.2.2.22)
    )
}

/// Flush the character content accumulated for the current element as one
/// WBXML token. Content must be aggregated before encoding: a byte-array
/// element's base64 text can legally arrive split across Text, CData, and
/// entity-reference events ([MS-ASDTYPE] §2.7.1), and per-event encoding
/// would emit a separate OPAQUE — or STR_I — per segment, corrupting the
/// wire instead of transmitting one value.
fn write_pending_content(
    buf: &mut Vec<u8>,
    byte_array_stack: &[bool],
    pending: &mut String,
) -> Result<()> {
    if pending.is_empty() {
        return Ok(());
    }
    let text = std::mem::take(pending);
    write_element_content(buf, byte_array_stack, &text)
}

/// Write one element's character content. Byte-array elements carry base64
/// text in the XML representation, which is decoded and emitted as OPAQUE
/// data with raw bytes ([MS-ASDTYPE] §2.7.1); every other element uses an
/// inline string (STR_I).
fn write_element_content(buf: &mut Vec<u8>, byte_array_stack: &[bool], text: &str) -> Result<()> {
    let is_byte_array = byte_array_stack.last().copied().unwrap_or(false);
    if !is_byte_array {
        buf.push(STR_I);
        buf.extend_from_slice(text.as_bytes());
        buf.push(0x00);
        return Ok(());
    }
    // xsd:base64Binary tolerates whitespace anywhere in the value, so
    // pretty-printed or split segments must not break the decode.
    let compact: String = text.chars().filter(|c| !c.is_ascii_whitespace()).collect();
    let raw = base64::engine::general_purpose::STANDARD
        .decode(compact.as_str())
        .map_err(|e| anyhow!("WBXML encode: invalid base64 in byte-array element: {e}"))?;
    buf.push(OPAQUE);
    write_mb_uint(buf, raw.len() as u64);
    buf.extend_from_slice(&raw);
    Ok(())
}

/// Write a multi-byte unsigned integer ([WBXML1.2] §8.1.2.1): 7 bits per
/// byte, high bit set on all but the final byte, big-endian.
fn write_mb_uint(buf: &mut Vec<u8>, mut value: u64) {
    let mut octets = [0u8; 10];
    let mut count = 0;
    loop {
        octets[count] = (value & 0x7F) as u8;
        count += 1;
        value >>= 7;
        if value == 0 {
            break;
        }
    }
    for i in (0..count).rev() {
        let mut b = octets[i];
        if i != 0 {
            b |= 0x80;
        }
        buf.push(b);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// MS-ASWBXML ResolveRecipients code page (10) tags.
    const CP10: u8 = 10;
    const TP_RESPONSE: u8 = 0x06;
    const TP_CERTIFICATES: u8 = 0x0C;
    const TP_CERTIFICATE: u8 = 0x0D;

    #[test]
    fn resolve_recipients_certificates_block_roundtrips_wbxml() {
        let xml = concat!(
            r#"<?xml version="1.0" encoding="utf-8"?>"#,
            r#"<ResolveRecipients xmlns="ResolveRecipients:">"#,
            "<Status>1</Status>",
            "<Response><To>alice@example.com</To><Status>1</Status>",
            "<RecipientCount>1</RecipientCount>",
            "<Recipient><Type>1</Type><DisplayName>Alice</DisplayName>",
            "<EmailAddress>alice@example.com</EmailAddress>",
            "<Certificates><Status>1</Status><CertificateCount>1</CertificateCount>",
            "<RecipientCount>1</RecipientCount>",
            "<Certificate>QUJD</Certificate>",
            "</Certificates>",
            "</Recipient></Response></ResolveRecipients>"
        );
        let wb = Wbxml::new().encode(xml).expect("encode must succeed");
        // Header is 4 bytes; find the Certificates token (0x0C|0x40) on
        // code page 10.
        let mut pages = vec![];
        let mut cp = 0u8;
        let mut i = 4usize;
        while i < wb.len() {
            if wb[i] == 0x00 {
                // SWITCH_PAGE
                cp = wb[i + 1];
                i += 2;
                continue;
            }
            let token = wb[i] & !0x40u8;
            pages.push((cp, token));
            if wb[i] & 0x40 != 0 || wb[i] == 0x01 {
                // content/END handling: scan inline strings for simplicity
            }
            if wb[i] == 0x03 {
                // STR_I
                let end = wb[i + 1..].iter().position(|&b| b == 0).unwrap() + i + 1;
                i = end + 1;
                continue;
            }
            i += 1;
        }
        assert!(
            pages.contains(&(CP10, TP_CERTIFICATES)),
            "Certificates must encode on code page 10, tokens: {pages:?}"
        );
        assert!(
            pages.contains(&(CP10, TP_CERTIFICATE)),
            "Certificate must encode on code page 10"
        );
        assert!(pages.contains(&(CP10, TP_RESPONSE)));
        assert!(
            !pages.contains(&(11u8, TP_CERTIFICATES)),
            "must not switch to ValidateCert page"
        );

        // Full decode round-trip preserves the certificate body.
        let decoded = Wbxml::new().decode(&wb).expect("decode");
        assert!(decoded.contains("<Certificates>"), "decoded: {decoded}");
        assert!(decoded.contains("<Certificate>QUJD</Certificate>"));
        assert!(decoded.contains("<CertificateCount>1</CertificateCount>"));
    }

    #[test]
    fn resolve_recipients_certificates_status_only_and_count_only_roundtrip() {
        let xml = concat!(
            r#"<?xml version="1.0" encoding="utf-8"?>"#,
            r#"<ResolveRecipients xmlns="ResolveRecipients:"><Status>1</Status>"#,
            "<Response><To>a@b.c</To><Status>1</Status><RecipientCount>1</RecipientCount>",
            "<Recipient><Type>1</Type><DisplayName>A</DisplayName>",
            "<EmailAddress>a@b.c</EmailAddress>",
            "<Certificates><Status>7</Status></Certificates>",
            "</Recipient></Response></ResolveRecipients>"
        );
        let wb = Wbxml::new().encode(xml).expect("encode");
        let decoded = Wbxml::new().decode(&wb).expect("decode");
        assert!(decoded.contains("<Certificates><Status>7</Status></Certificates>"));
    }

    /// The document's root code page is its default namespace: root-page
    /// tags expand unqualified, SWITCH_PAGE-reached tags keep their prefix
    /// ([MS-ASWBXML] §2.1.2.1 — a Sync document rooted on the AirSync page
    /// carries `<AirSyncBase:BodyPreference>` inside `<Options>`).
    #[test]
    fn sync_root_page_expands_unqualified_and_switched_pages_prefixed() {
        let xml = concat!(
            r#"<?xml version="1.0" encoding="utf-8"?>"#,
            r#"<Sync xmlns="AirSync:" xmlns:AirSyncBase="AirSyncBase:">"#,
            r#"<Collections><Collection><SyncKey>1</SyncKey>"#,
            "<Options><AirSyncBase:BodyPreference><AirSyncBase:Type>2</AirSyncBase:Type>",
            "<AirSyncBase:TruncationSize>512</AirSyncBase:TruncationSize>",
            "</AirSyncBase:BodyPreference></Options>",
            "</Collection></Collections></Sync>"
        );
        let wb = Wbxml::new().encode(xml).expect("encode");
        let decoded = Wbxml::new().decode(&wb).expect("decode");
        assert!(
            decoded.contains("<Sync><Collections>"),
            "decoded: {decoded}"
        );
        assert!(decoded.contains("<SyncKey>1</SyncKey>"));
        assert!(
            decoded.contains("<AirSyncBase:BodyPreference>"),
            "decoded: {decoded}"
        );
        assert!(decoded.contains("<AirSyncBase:Type>2</AirSyncBase:Type>"));
        assert!(decoded.contains("<AirSyncBase:TruncationSize>512</AirSyncBase:TruncationSize>"));
        assert!(!decoded.contains("<AirSync:"));
        assert!(!decoded.contains("<Sync:"));
    }

    // ============ AUDIT.md §11: [MS-ASWBXML] conformance hardening ============
    //
    // The tests below hold the WBXML codec to the spec text shipped in
    // exchange_protocols/: the full code page/token tables are diffed against
    // [MS-ASWBXML] v20250520 §2.1.2.1, the per-token protocol-version matrix
    // pins which entries are 16.1-capable, the byte-array-typed elements
    // ([MS-ASDTYPE] §2.7.1, OPAQUE on the wire) are pinned to their spec
    // declarations, and §3's worked example must round-trip byte-exactly.

    const MS_ASWBXML_SPEC: &str = include_str!("../exchange_protocols/[MS-ASWBXML].txt");
    const MS_ASCMD_SPEC: &str = include_str!("../exchange_protocols/[MS-ASCMD].txt");
    const MS_ASAIRS_SPEC: &str = include_str!("../exchange_protocols/[MS-ASAIRS].txt");
    const MS_ASCON_SPEC: &str = include_str!("../exchange_protocols/[MS-ASCON].txt");
    const MS_ASEMAIL_SPEC: &str = include_str!("../exchange_protocols/[MS-ASEMAIL].txt");

    /// The [MS-ASWBXML] §2.1.2.1.x code pages that carry tag tables, as
    /// (code page, page name). Code page 3 (AirNotify) is obsolete and has no
    /// tags, so it is absent from both the spec tables and ours.
    const SPEC_PAGES: &[(u8, &str)] = &[
        (0, "AirSync"),
        (1, "Contacts"),
        (2, "Email"),
        (4, "Calendar"),
        (5, "Move"),
        (6, "GetItemEstimate"),
        (7, "FolderHierarchy"),
        (8, "MeetingResponse"),
        (9, "Tasks"),
        (10, "ResolveRecipients"),
        (11, "ValidateCert"),
        (12, "Contacts2"),
        (13, "Ping"),
        (14, "Provision"),
        (15, "Search"),
        (16, "GAL"),
        (17, "AirSyncBase"),
        (18, "Settings"),
        (19, "DocumentLibrary"),
        (20, "ItemOperations"),
        (21, "ComposeMail"),
        (22, "Email2"),
        (23, "Notes"),
        (24, "RightsManagement"),
        (25, "Find"),
    ];

    /// Slice a spec's body section, from its heading line (trimmed equality,
    /// which skips the dotted table-of-contents entries) up to the next
    /// numbered heading line.
    fn spec_section<'a>(spec: &'a str, heading: &str) -> &'a str {
        let mut start = None;
        let mut offset = 0usize;
        for line in spec.split('\n') {
            if line.trim() == heading {
                start = Some(offset);
                break;
            }
            offset += line.len() + 1;
        }
        let start = start.unwrap_or_else(|| panic!("spec heading not found: {heading}"));
        let rest = &spec[start..];
        let mut body_end = rest.len();
        let mut pos = 0usize;
        for (idx, line) in rest.split('\n').enumerate() {
            if idx > 0 && spec_heading_line(line) {
                body_end = pos;
                break;
            }
            pos += line.len() + 1;
        }
        &rest[..body_end]
    }

    /// A line that opens a new spec body section, e.g. "2.2.2.15 Content" or
    /// "2.1.3 Processing Rules" or "2.1.2.1.5 Code Page 4: Calendar": the
    /// first token is a dotted section number of at least three parts that
    /// ends in a digit.
    fn spec_heading_line(line: &str) -> bool {
        let Some(first) = line.split_whitespace().next() else {
            return false;
        };
        first.split('.').filter(|p| !p.is_empty()).count() >= 3
            && first.ends_with(|c: char| c.is_ascii_digit())
            && first.chars().all(|c| c.is_ascii_digit() || c == '.')
    }

    /// Table-of-contents/page-footer noise inside [MS-ASWBXML] page sections.
    fn spec_line_is_noise(line: &str) -> bool {
        let s = line.trim();
        if s.is_empty() {
            return true;
        }
        if s.split(" / ").count() == 2 && s.split(" / ").all(|p| p.trim().parse::<u32>().is_ok()) {
            return true;
        }
        s.starts_with("[MS-ASWBXML]")
            || s.starts_with("Exchange ActiveSync")
            || s.starts_with("Copyright")
            || s.starts_with("Release:")
            || s.starts_with("Note ")
            || s == "Tag name Token Protocol versions"
    }

    /// One [MS-ASWBXML] token-table row: (token, tag name, protocol versions).
    /// Version tokens keep their trailing commas so callers can tell whether
    /// the cell wraps onto the next line.
    fn parse_spec_token_row(line: &str) -> Option<(u8, String, Vec<String>)> {
        let tokens: Vec<&str> = line.split_whitespace().collect();
        let tok_idx = tokens.iter().position(|t| {
            t.len() == 4 && t.starts_with("0x") && t[2..].bytes().all(|b| b.is_ascii_hexdigit())
        })?;
        let name_end = tokens[..tok_idx]
            .iter()
            .position(|t| *t == "\u{2014}" || *t == "\u{2013}")
            .unwrap_or(tok_idx);
        let name = tokens[..name_end].join(" ");
        let versions = tokens[tok_idx + 1..]
            .iter()
            .map(|t| t.to_string())
            .collect();
        Some((
            u8::from_str_radix(&tokens[tok_idx][2..], 16).ok()?,
            name,
            versions,
        ))
    }

    fn parse_spec_page_rows(section: &str) -> Vec<(u8, String, Vec<String>)> {
        let lines: Vec<&str> = section.split('\n').collect();
        let mut rows = Vec::new();
        let mut i = 0;
        while i < lines.len() {
            let s = lines[i].trim();
            i += 1;
            if spec_line_is_noise(s) {
                continue;
            }
            let Some((token, name, mut cell)) = parse_spec_token_row(s) else {
                continue;
            };
            // Join wrapped version cells: a trailing comma continues onto the
            // next non-noise line.
            while cell.last().is_some_and(|v| v.ends_with(',')) {
                while i < lines.len() && spec_line_is_noise(lines[i].trim()) {
                    i += 1;
                }
                if i >= lines.len() {
                    break;
                }
                if parse_spec_token_row(lines[i].trim()).is_some() {
                    // The next row already started: leave it in the queue.
                    break;
                }
                cell.extend(lines[i].split_whitespace().map(str::to_string));
                i += 1;
            }
            rows.push((token, name, cell));
        }
        rows
    }

    fn spec_versions_support_16_1(versions: &[String]) -> bool {
        versions
            .iter()
            .any(|v| matches!(v.trim_end_matches(','), "16.1" | "All"))
    }

    #[test]
    fn wbxml_code_page_tables_match_ms_aswbxml_v20250520() {
        let mut spec_count = 0usize;
        let mut missing = Vec::new();
        let mut mismatched = Vec::new();
        for &(cp, page_name) in SPEC_PAGES {
            let heading = format!("2.1.2.1.{} Code Page {cp}: {page_name}", cp + 1);
            let section = spec_section(MS_ASWBXML_SPEC, &heading);
            for (token, name, _versions) in parse_spec_page_rows(section) {
                spec_count += 1;
                let expected = if cp == 0 {
                    name.clone()
                } else {
                    format!("{page_name}:{name}")
                };
                match TAG_TO_NAME.get(&[cp, token]) {
                    Some(actual) if *actual == expected => {}
                    Some(actual) => mismatched.push(format!(
                        "cp {cp} token 0x{token:02X}: spec '{expected}', table '{actual}'"
                    )),
                    None => missing.push(format!("cp {cp} token 0x{token:02X} {expected}")),
                }
            }
        }
        assert!(
            missing.is_empty(),
            "tags in [MS-ASWBXML] missing from TAG_TO_NAME: {missing:?}"
        );
        assert!(
            mismatched.is_empty(),
            "tag names diverging from [MS-ASWBXML]: {mismatched:?}"
        );
        assert_eq!(
            spec_count,
            TAG_TO_NAME.len(),
            "TAG_TO_NAME holds entries that [MS-ASWBXML] does not define"
        );
        // The encode-direction table must be the exact inverse of the decode
        // table so no tag resolves to a different token on the way out.
        assert_eq!(NAME_TO_TAG.len(), TAG_TO_NAME.len());
        for (&pair, &name) in TAG_TO_NAME.entries() {
            assert_eq!(
                NAME_TO_TAG.get(name),
                Some(&pair),
                "NAME_TO_TAG is not the inverse of TAG_TO_NAME for {name}"
            );
        }
    }

    #[test]
    fn wbxml_token_version_matrix_pins_non_16_1_inventory() {
        // Tokens the spec does not offer at protocol version 16.1 (the only
        // version this gateway serves). They stay in the decode tables as
        // documented legacy inventory but the pinned list forces a conscious
        // decision whenever the tables or the spec revision change.
        const EXPECTED_NON_16_1: &[(u8, u8)] = &[
            (0, 0x19), // Truncation
            (1, 0x09), // Contacts:Body
            (1, 0x0A), // Contacts:BodySize
            (1, 0x0B), // Contacts:BodyTruncated
            (2, 0x05), // Email:Attachment
            (2, 0x06), // Email:Attachments (AirSyncBase:Attachments from 12.0)
            (2, 0x07), // Email:AttName
            (2, 0x08), // Email:AttSize
            (2, 0x09), // Email:Att0id
            (2, 0x0A), // Email:AttMethod
            (2, 0x0C), // Email:Body (AirSyncBase:Body from 12.0)
            (2, 0x0D), // Email:BodySize
            (2, 0x0E), // Email:BodyTruncated
            (2, 0x10), // Email:DisplayName
            (2, 0x21), // Email:Location (AirSyncBase:Location from 16.0)
            (2, 0x34), // Email:GlobalObjId (Calendar:UID from 16.0)
            (2, 0x36), // Email:MIMEData
            (2, 0x37), // Email:MIMETruncated
            (2, 0x38), // Email:MIMESize
            (4, 0x0B), // Calendar:Body
            (4, 0x0C), // Calendar:BodyTruncated
            (4, 0x16), // Calendar:ExceptionStartTime
            (4, 0x17), // Calendar:Location
            (6, 0x09), // GetItemEstimate:Class
            (7, 0x05), // FolderHierarchy:Folders
            (7, 0x06), // FolderHierarchy:Folder
            (9, 0x05), // Tasks:Body
            (9, 0x06), // Tasks:BodySize
            (9, 0x07), // Tasks:BodyTruncated
        ];
        let mut found: Vec<(u8, u8)> = Vec::new();
        for &(cp, page_name) in SPEC_PAGES {
            let heading = format!("2.1.2.1.{} Code Page {cp}: {page_name}", cp + 1);
            for (token, _name, versions) in
                parse_spec_page_rows(spec_section(MS_ASWBXML_SPEC, &heading))
            {
                if !spec_versions_support_16_1(&versions) && TAG_TO_NAME.get(&[cp, token]).is_some()
                {
                    found.push((cp, token));
                }
            }
        }
        found.sort_unstable();
        assert_eq!(
            found, EXPECTED_NON_16_1,
            "the set of non-16.1 tokens in the tables diverges from the pinned inventory"
        );
        // Everything else in the shipped tables is 16.1-capable.
        let mut all_rows = Vec::new();
        for &(cp, page_name) in SPEC_PAGES {
            let heading = format!("2.1.2.1.{} Code Page {cp}: {page_name}", cp + 1);
            for (token, _name, versions) in
                parse_spec_page_rows(spec_section(MS_ASWBXML_SPEC, &heading))
            {
                all_rows.push((cp, token, spec_versions_support_16_1(&versions)));
            }
        }
        let capable = all_rows
            .iter()
            .filter(|(cp, tok, cap)| *cap && TAG_TO_NAME.get(&[*cp, *tok]).is_some())
            .count();
        assert_eq!(capable, TAG_TO_NAME.len() - EXPECTED_NON_16_1.len());
    }

    #[test]
    fn wbxml_conformance_byte_array_inventory_matches_specs() {
        // Every element the EAS specs type as a byte array ([MS-ASDTYPE]
        // §2.7.1 → WBXML OPAQUE on the wire), with the spec section that
        // declares it. Marker is a phrase that must appear inside that
        // section's body.
        let byte_arrays: &[(&str, &str, &str, u8, u8)] = &[
            (
                MS_ASAIRS_SPEC,
                "2.2.2.15 Content",
                "string data type byte array",
                17,
                0x1F,
            ),
            (
                MS_ASCMD_SPEC,
                "2.2.3.39.1 Data (Find)",
                "contains the binary data of the contact photo",
                16,
                0x12,
            ),
            (
                MS_ASCMD_SPEC,
                "2.2.3.39.3 Data (ResolveRecipients)",
                "contains the binary data of the contact photo",
                16,
                0x12,
            ),
            (
                MS_ASCMD_SPEC,
                "2.2.3.39.4 Data (Search)",
                "contains the binary data of the contact photo",
                16,
                0x12,
            ),
            (
                MS_ASCMD_SPEC,
                "2.2.3.109 Mime",
                "transferred as an opaque BLOB within the WBXML tags",
                21,
                0x10,
            ),
            (
                MS_ASCON_SPEC,
                "2.2.2.3.1 ConversationId (ItemOperations)",
                "byte array, as specified in [MS-ASDTYPE] section 2.7.1",
                20,
                0x18,
            ),
            (
                MS_ASCON_SPEC,
                "2.2.2.3.2 ConversationId (Search)",
                "byte array, as specified in [MS-ASDTYPE] section 2.7.1",
                15,
                0x20,
            ),
            (
                MS_ASCON_SPEC,
                "2.2.2.3.3 ConversationId (Sync)",
                "byte array, as specified in [MS-ASDTYPE] section 2.7.1",
                22,
                0x09,
            ),
            (
                MS_ASEMAIL_SPEC,
                "2.2.2.21 ConversationId",
                "byte array data type, as specified in [MS-ASDTYPE] section 2.7.1",
                22,
                0x09,
            ),
            (
                MS_ASCON_SPEC,
                "2.2.2.4 ConversationIndex",
                "byte array, as specified in [MS-ASDTYPE] section 2.7.1",
                22,
                0x0A,
            ),
            (
                MS_ASEMAIL_SPEC,
                "2.2.2.22 ConversationIndex",
                "byte array data type, as specified in [MS-ASDTYPE] section 2.7.1",
                22,
                0x0A,
            ),
        ];
        let mut spec_set: Vec<(u8, u8)> = Vec::new();
        for (spec, heading, marker, cp, token) in byte_arrays {
            let body = spec_section(spec, heading);
            assert!(
                body.contains(marker),
                "[MS-ASWBXML] conformance: {heading} no longer declares its byte array as '{marker}'"
            );
            assert!(
                is_byte_array_element(*cp, *token),
                "{heading} types ({cp}, 0x{token:02X}) as byte array but the codec does not OPAQUE-encode it"
            );
            if !spec_set.contains(&(*cp, *token)) {
                spec_set.push((*cp, *token));
            }
        }
        spec_set.sort_unstable();
        // Email:GlobalObjId is a deliberate extra (raw-binary ABNF carried in
        // an opaque BLOB; see is_byte_array_element).
        let mut expected = spec_set;
        if !expected.contains(&(2, 0x34)) {
            expected.push((2, 0x34));
        }
        expected.sort_unstable();
        let mut actual: Vec<(u8, u8)> = TAG_TO_NAME
            .entries()
            .filter(|(pair, _)| is_byte_array_element(pair[0], pair[1]))
            .map(|(pair, _)| (pair[0], pair[1]))
            .collect();
        actual.sort_unstable();
        assert_eq!(
            actual, expected,
            "byte-array inventory diverged from the spec-declared elements"
        );
    }

    /// [MS-ASWBXML] §3's worked example: the encoder must reproduce
    /// Microsoft's own 106 bytes byte-for-byte from the documented XML (which
    /// uses namespace URIs without trailing colons), and a decode → re-encode
    /// cycle must be stable.
    #[test]
    fn ms_aswbxml_section3_example_roundtrips_byte_exact() {
        const SPEC_WBXML: [u8; 106] = [
            0x03, 0x01, 0x6A, 0x00, 0x45, 0x5C, 0x4F, 0x50, 0x03, 0x43, 0x6F, 0x6E, 0x74, 0x61,
            0x63, 0x74, 0x73, 0x00, 0x01, 0x4B, 0x03, 0x32, 0x00, 0x01, 0x52, 0x03, 0x32, 0x00,
            0x01, 0x4E, 0x03, 0x31, 0x00, 0x01, 0x56, 0x47, 0x4D, 0x03, 0x32, 0x3A, 0x31, 0x00,
            0x01, 0x5D, 0x00, 0x11, 0x4A, 0x46, 0x03, 0x31, 0x00, 0x01, 0x4C, 0x03, 0x30, 0x00,
            0x01, 0x4D, 0x03, 0x31, 0x00, 0x01, 0x01, 0x00, 0x01, 0x5E, 0x03, 0x46, 0x75, 0x6E,
            0x6B, 0x2C, 0x20, 0x44, 0x6F, 0x6E, 0x00, 0x01, 0x5F, 0x03, 0x44, 0x6F, 0x6E, 0x00,
            0x01, 0x69, 0x03, 0x46, 0x75, 0x6E, 0x6B, 0x00, 0x01, 0x00, 0x11, 0x56, 0x03, 0x31,
            0x00, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01,
        ];
        const SPEC_XML: &str = concat!(
            r#"<?xml version="1.0" encoding="utf-8"?>"#,
            "\n",
            r#"<Sync xmlns="AirSync" xmlns:airsyncbase="AirSyncBase" xmlns:contacts="Contacts">"#,
            "\n",
            " <Collections>\n",
            "  <Collection>\n",
            "   <Class>Contacts</Class>\n",
            "   <SyncKey>2</SyncKey>\n",
            "   <CollectionId>2</CollectionId>\n",
            "   <Status>1</Status>\n",
            "   <Commands>\n",
            "    <Add>\n",
            "     <ServerId>2:1</ServerId>\n",
            "     <ApplicationData>\n",
            "      <airsyncbase:Body>\n",
            "       <airsyncbase:Type>1</airsyncbase:Type>\n",
            "       <airsyncbase:EstimatedDataSize>0</airsyncbase:EstimatedDataSize>\n",
            "       <airsyncbase:Truncated>1</airsyncbase:Truncated>\n",
            "      </airsyncbase:Body>\n",
            "      <contacts:FileAs>Funk, Don</contacts:FileAs>\n",
            "      <contacts:FirstName>Don</contacts:FirstName>\n",
            "      <contacts:LastName>Funk</contacts:LastName>\n",
            "      <airsyncbase:NativeBodyType>1</airsyncbase:NativeBodyType>\n",
            "     </ApplicationData>\n",
            "    </Add>\n",
            "   </Commands>\n",
            "  </Collection>\n",
            " </Collections>\n",
            "</Sync>\n",
        );

        let encoded = Wbxml::new()
            .encode(SPEC_XML)
            .expect("the spec's own XML must encode");
        assert_eq!(
            encoded, SPEC_WBXML,
            "encoder must reproduce [MS-ASWBXML] §3 byte-for-byte"
        );

        let decoded = Wbxml::new()
            .decode(&SPEC_WBXML)
            .expect("the spec's own WBXML must decode");
        assert!(
            decoded.contains("<Sync><Collections>"),
            "decoded: {decoded}"
        );
        assert!(decoded.contains("<ServerId>2:1</ServerId>"));
        assert!(decoded.contains("<AirSyncBase:Body>"));
        assert!(decoded.contains("<AirSyncBase:NativeBodyType>1</AirSyncBase:NativeBodyType>"));
        assert!(decoded.contains("<Contacts:FileAs>Funk, Don</Contacts:FileAs>"));
        assert!(decoded.contains("<Contacts:LastName>Funk</Contacts:LastName>"));

        let reencoded = Wbxml::new().encode(&decoded).expect("re-encode");
        assert_eq!(reencoded, SPEC_WBXML, "decode → encode must be stable");
    }

    #[test]
    fn decode_rejects_forbidden_wbxml_tokens() {
        // [MS-ASWBXML] §2.1.3: no string tables, entities, processing
        // instructions, or attribute encoding. The corresponding global
        // tokens must be rejected, never guessed at.
        for token in [
            0x40u8, 0x41, 0x42, // EXT_I_0/EXT_I_1/EXT_I_2
            0x43, // PI
            0x44, // LITERAL_C
            0x80, 0x81, 0x82, // EXT_T_0/EXT_T_1/EXT_T_2
            0x84, // LITERAL_A
            0xC0, 0xC1, 0xC2, // EXT_0/EXT_1/EXT_2
            0xC4, // LITERAL_AC
        ] {
            let mut doc = vec![0x03, 0x01, 0x6A, 0x00, 0x45];
            doc.push(token);
            let err = Wbxml::new()
                .decode(&doc)
                .expect_err("forbidden token must be rejected");
            assert!(
                err.to_string().contains("[MS-ASWBXML]"),
                "token 0x{token:02x} error should cite the spec: {err}"
            );
        }
    }

    #[test]
    fn decode_rejects_tag_tokens_with_attribute_bit() {
        // 0x85 = tag 0x05 with attribute bit; 0xC5 additionally has the
        // content bit. [MS-ASWBXML] defines no attribute code pages.
        for token in [0x85u8, 0xA5, 0xC5, 0xE5] {
            let doc = [0x03, 0x01, 0x6A, 0x00, token];
            let err = Wbxml::new()
                .decode(&doc)
                .expect_err("attribute-carrying tag must be rejected");
            assert!(
                err.to_string().contains("attributes"),
                "token 0x{token:02x}: {err}"
            );
        }
    }

    #[test]
    fn decode_rejects_unknown_tags_and_unknown_code_pages() {
        // Code page 0 defines no token 0x3F.
        let unknown_tag = [0x03, 0x01, 0x6A, 0x00, 0x45, 0x7F];
        let err = Wbxml::new()
            .decode(&unknown_tag)
            .expect_err("unknown tag must be rejected");
        assert!(err.to_string().contains("unknown tag"), "{err}");

        // Code page 99 does not exist in the profile.
        let unknown_page = [0x03, 0x01, 0x6A, 0x00, 0x00, 99, 0x45];
        let err = Wbxml::new()
            .decode(&unknown_page)
            .expect_err("unknown code page must be rejected");
        assert!(err.to_string().contains("unknown tag"), "{err}");
    }

    #[test]
    fn decode_rejects_structurally_invalid_documents() {
        // No root element at all.
        let no_root = [0x03, 0x01, 0x6A, 0x00];
        assert!(Wbxml::new().decode(&no_root).is_err());

        // END before any element opens.
        let end_only = [0x03, 0x01, 0x6A, 0x00, 0x01];
        let err = Wbxml::new()
            .decode(&end_only)
            .expect_err("END needs an open element");
        assert!(err.to_string().contains("no open element"), "{err}");

        // Truncated: root opened, never closed.
        let truncated = [0x03, 0x01, 0x6A, 0x00, 0x45, 0x03, b'a', 0x00];
        let err = Wbxml::new()
            .decode(&truncated)
            .expect_err("truncated document");
        assert!(err.to_string().contains("Truncated"), "{err}");

        // Data after the root closes.
        let two_roots = [0x03, 0x01, 0x6A, 0x00, 0x05, 0x05];
        let err = Wbxml::new()
            .decode(&two_roots)
            .expect_err("second root must be rejected");
        assert!(err.to_string().contains("after root"), "{err}");

        // A well-formed empty root still decodes.
        let empty_root = [0x03, 0x01, 0x6A, 0x00, 0x05];
        let decoded = Wbxml::new()
            .decode(&empty_root)
            .expect("empty root decodes");
        assert_eq!(
            decoded,
            "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<Sync></Sync>"
        );
    }

    #[test]
    fn byte_array_elements_encode_as_opaque_and_roundtrip() {
        // [MS-ASDTYPE] §2.7.1: byte-array elements travel as OPAQUE raw
        // bytes; base64 lives only in the in-memory XML form.
        let cases: &[(&str, &[u8])] = &[
            // GAL:Data ([MS-ASCMD] §2.2.3.39): SWITCH_PAGE to 16, token 0x52
            // (Data|content bit), then OPAQUE with the raw bytes of "QUJD"
            // (3 bytes: "ABC").
            (
                concat!(
                    r#"<Search xmlns="Search" xmlns:GAL="GAL">"#,
                    "<Response><Properties>",
                    "<GAL:Data>QUJD</GAL:Data>",
                    "</Properties></Response></Search>"
                ),
                &[0x00, 0x10, 0x52, 0xC3, 0x03, b'A', b'B', b'C'],
            ),
            // ComposeMail:Mime ([MS-ASCMD] §2.2.3.109), padded base64
            // "QUJDRA==" (4 bytes: "ABCD").
            (
                concat!(
                    r#"<SendMail xmlns="ComposeMail">"#,
                    "<Mime>QUJDRA==</Mime></SendMail>"
                ),
                &[0x45, 0x50, 0xC3, 0x04, b'A', b'B', b'C', b'D'],
            ),
            // AirSyncBase:Content ([MS-ASAIRS] §2.2.2.15)
            (
                concat!(
                    r#"<ItemOperations xmlns="ItemOperations" xmlns:AirSyncBase="AirSyncBase">"#,
                    "<Response><Fetch>",
                    "<AirSyncBase:Attachments><AirSyncBase:Add>",
                    "<AirSyncBase:Content>QUJD</AirSyncBase:Content>",
                    "</AirSyncBase:Add></AirSyncBase:Attachments>",
                    "</Fetch></Response></ItemOperations>"
                ),
                &[0x00, 0x11, 0x4E, 0x5C, 0x5F, 0xC3, 0x03, b'A', b'B', b'C'],
            ),
            // Email2:ConversationId/ConversationIndex ([MS-ASEMAIL] §2.2.2.21/22)
            (
                concat!(
                    r#"<Sync xmlns="AirSync" xmlns:Email2="Email2">"#,
                    "<Collections><Collection><ApplicationData>",
                    "<Email2:ConversationId>QUJDRA==</Email2:ConversationId>",
                    "<Email2:ConversationIndex>SGVsbG8=</Email2:ConversationIndex>",
                    "</ApplicationData></Collection></Collections></Sync>"
                ),
                &[
                    0x00, 0x16, 0x49, 0xC3, 0x04, b'A', b'B', b'C', b'D', 0x01, 0x4A, 0xC3, 0x05,
                    b'H', b'e', b'l', b'l', b'o',
                ],
            ),
            // ItemOperations:ConversationId ([MS-ASCON] §2.2.2.3.1): a child
            // of Move in ItemOperations commands ([MS-ASCMD] §2.2.3.117.1).
            (
                concat!(
                    r#"<ItemOperations xmlns="ItemOperations">"#,
                    "<Response><Move><ConversationId>QUJD</ConversationId>",
                    "</Move></Response></ItemOperations>"
                ),
                &[0x56, 0x58, 0xC3, 0x03, b'A', b'B', b'C'],
            ),
        ];
        for &(xml, opaque_span) in cases {
            let encoded = Wbxml::new().encode(xml).expect("encode");
            let pos = encoded
                .windows(opaque_span.len())
                .position(|w| w == opaque_span)
                .unwrap_or_else(|| {
                    panic!("no OPAQUE span {opaque_span:?} in {encoded:02x?} for {xml}")
                });
            assert_eq!(&encoded[pos..pos + opaque_span.len()], opaque_span);
            // Decode materializes the base64 text again.
            let decoded = Wbxml::new().decode(&encoded).expect("decode");
            assert!(decoded.contains("QUJD"), "decoded: {decoded}");
            let reencoded = Wbxml::new().encode(&decoded).expect("re-encode");
            assert_eq!(reencoded, encoded, "round-trip must be stable for {xml}");
        }
    }

    #[test]
    fn base64_bearing_string_elements_stay_inline_strings() {
        // Elements typed "string" ([MS-ASDTYPE] §2.7 → inline strings) even
        // though their content is base64 of binary data.
        let cases: &[&str] = &[
            // ItemOperations:Data ([MS-ASCMD] §2.2.3.39.2: "content of the
            // Data element is a base64 encoding of the binary document,
            // attachment, or body data")
            concat!(
                r#"<ItemOperations xmlns="ItemOperations">"#,
                "<Response><Fetch><Properties>",
                "<Data>QUJD</Data>",
                "</Properties></Fetch></Response></ItemOperations>"
            ),
            // Contacts:Picture ([MS-ASCNTC] §2.2.2.58: "string data type")
            concat!(
                r#"<Sync xmlns="AirSync" xmlns:Contacts="Contacts">"#,
                "<Collections><Collection><ApplicationData>",
                "<Contacts:Picture>QUJD</Contacts:Picture>",
                "</ApplicationData></Collection></Collections></Sync>"
            ),
        ];
        for &xml in cases {
            let encoded = Wbxml::new().encode(xml).expect("encode");
            assert!(
                !encoded.contains(&OPAQUE),
                "string-typed element must not OPAQUE-encode: {xml} -> {encoded:02x?}"
            );
            assert!(
                encoded
                    .windows(5)
                    .any(|w| w == [0x03, b'Q', b'U', b'J', b'D']),
                "inline string QUJD expected in {encoded:02x?}"
            );
            let decoded = Wbxml::new().decode(&encoded).expect("decode");
            assert!(decoded.contains("QUJD"), "decoded: {decoded}");
        }
    }

    #[test]
    fn cdata_content_is_encoded_and_pi_doctype_are_rejected() {
        let with_cdata = concat!(
            r#"<Sync xmlns="AirSync">"#,
            "<Collections><Collection><SyncKey><![CDATA[7]]></SyncKey>",
            "</Collection></Collections></Sync>"
        );
        let encoded = Wbxml::new()
            .encode(with_cdata)
            .expect("CDATA content encodes");
        assert!(
            encoded.windows(3).any(|w| w == [0x03, b'7', 0x00]),
            "CDATA text must reach the wire: {encoded:02x?}"
        );
        assert!(
            Wbxml::new()
                .decode(&encoded)
                .expect("decode")
                .contains("<SyncKey>7</SyncKey>")
        );

        let with_pi = concat!(
            r#"<Sync xmlns="AirSync">"#,
            "<?debug data?>",
            "<Collections></Collections></Sync>"
        );
        assert!(Wbxml::new().encode(with_pi).is_err());

        let with_doctype = concat!(
            r#"<!DOCTYPE Sync>"#,
            r#"<Sync xmlns="AirSync">"#,
            "<Collections></Collections></Sync>"
        );
        assert!(Wbxml::new().encode(with_doctype).is_err());
    }

    /// [WBXML1.2] §5.3: the document body is exactly one element, so
    /// character-data tokens (STR_I, STR_T, ENTITY, OPAQUE) before the
    /// root opens are as malformed as data after it closes — they must be
    /// rejected, not emitted as text outside the document element.
    #[test]
    fn decode_rejects_data_before_root_element() {
        // STR_I before the root.
        let str_i_first = [
            0x03, 0x01, 0x6A, 0x00, 0x03, b'j', b'u', b'n', b'k', 0x00, 0x05,
        ];
        let err = Wbxml::new()
            .decode(&str_i_first)
            .expect_err("data before the root must be rejected");
        assert!(err.to_string().contains("before root"), "{err}");

        // OPAQUE before the root.
        let opaque_first = [0x03, 0x01, 0x6A, 0x00, 0xC3, 0x01, 0xFF, 0x05];
        let err = Wbxml::new()
            .decode(&opaque_first)
            .expect_err("OPAQUE before the root must be rejected");
        assert!(err.to_string().contains("before root"), "{err}");

        // ENTITY before the root.
        let entity_first = [0x03, 0x01, 0x6A, 0x00, 0x02, 0x26, 0x05];
        let err = Wbxml::new()
            .decode(&entity_first)
            .expect_err("ENTITY before the root must be rejected");
        assert!(err.to_string().contains("before root"), "{err}");

        // A SWITCH_PAGE before the root stays legal: a document may root
        // on any code page (the ResolveRecipients fixtures do exactly
        // this with page 10).
        let switch_then_root = [0x03, 0x01, 0x6A, 0x00, 0x00, 0x0A, 0x46, 0x01];
        let decoded = Wbxml::new()
            .decode(&switch_then_root)
            .expect("leading SWITCH_PAGE must stay legal");
        assert!(decoded.contains("<Response"), "{decoded}");
    }

    /// The truncation error must name the OUTERMOST unclosed element —
    /// `xml_stack.first()`, not `.last()` (the innermost) — or a truncated
    /// `Sync > Collections > Collection` body would misleadingly report
    /// "outermost <Collection>".
    #[test]
    fn decode_truncation_error_names_outermost_element() {
        // Sync (0x05|0x40=0x45) > Collections (0x06|0x40=0x46) > Collection
        // (0x05|0x40=0x45) on code page 0, never closed.
        let truncated = [0x03, 0x01, 0x6A, 0x00, 0x45, 0x46, 0x45];
        let err = Wbxml::new()
            .decode(&truncated)
            .expect_err("truncated document");
        let msg = err.to_string();
        assert!(msg.contains("3 unclosed elements"), "{msg}");
        assert!(msg.contains("outermost <Sync>"), "{msg}");
        assert!(!msg.contains("<Collection>"), "{msg}");

        // Single unclosed element: the root itself.
        let one = [0x03, 0x01, 0x6A, 0x00, 0x45];
        let err = Wbxml::new().decode(&one).expect_err("truncated root");
        assert!(err.to_string().contains("unclosed element <Sync>"), "{err}");
    }

    /// A prefix explicitly declared with an unsupported namespace URI is a
    /// caller error: it must be rejected, never silently re-encoded under
    /// the canonical-prefix or ambient-page fallback (`xmlns:AirSyncBase=
    /// "Unknown"` with `<AirSyncBase:Body>` used to encode as AirSyncBase).
    #[test]
    fn encode_rejects_declared_unknown_namespace_bindings() {
        let unknown_prefix_binding = concat!(
            r#"<Sync xmlns="AirSync:" xmlns:AirSyncBase="Unknown">"#,
            "<Collections><AirSyncBase:Body>3</AirSyncBase:Body></Collections></Sync>"
        );
        let err = Wbxml::new()
            .encode(unknown_prefix_binding)
            .expect_err("a declared-but-unknown prefix URI must be rejected");
        assert!(err.to_string().contains("prefix 'AirSyncBase'"), "{err}");

        // The default namespace has the same rule: an explicitly
        // declared, unrecognized URI is unrepresentable.
        let unknown_default = concat!(
            r#"<Sync xmlns="Unknown">"#,
            "<Collections></Collections></Sync>"
        );
        let err = Wbxml::new()
            .encode(unknown_default)
            .expect_err("a declared-but-unknown default namespace must be rejected");
        assert!(
            err.to_string().contains("default namespace URI 'Unknown'"),
            "{err}"
        );

        // An unused declaration to an unknown URI is harmless XML scoping
        // noise and must stay encodable.
        let unused_unknown = concat!(
            r#"<Sync xmlns="AirSync:" xmlns:Unused="Unknown">"#,
            "<Collections></Collections></Sync>"
        );
        assert!(
            Wbxml::new().encode(unused_unknown).is_ok(),
            "an unused unknown-URI declaration must not error"
        );

        // The undeclared canonical-prefix fallback is unaffected.
        let canonical_fallback = concat!(
            r#"<Sync xmlns="AirSync:">"#,
            "<Collections><AirSyncBase:Body>3</AirSyncBase:Body></Collections></Sync>"
        );
        assert!(Wbxml::new().encode(canonical_fallback).is_ok());
    }

    /// [MS-ASWBXML] §2.1.3 defines no attribute encoding, so any attribute
    /// other than a namespace declaration is a hard error on encode, not
    /// silent data loss (`<Sync version="1">` used to drop the attribute).
    #[test]
    fn encode_rejects_non_namespace_attributes() {
        let versioned = concat!(
            r#"<Sync xmlns="AirSync:" version="1">"#,
            "<Collections></Collections></Sync>"
        );
        let err = Wbxml::new()
            .encode(versioned)
            .expect_err("non-xmlns attributes cannot be represented");
        assert!(err.to_string().contains("version"), "{err}");

        let on_empty = r#"<Sync xmlns="AirSync:" version="1"/>"#;
        let err = Wbxml::new()
            .encode(on_empty)
            .expect_err("attributes on empty elements are rejected too");
        assert!(err.to_string().contains("version"), "{err}");

        // XML well-formedness: duplicate attribute names in one tag.
        let duplicated = concat!(
            r#"<Sync xmlns="AirSync:" xmlns:AirSyncBase="AirSyncBase:" "#,
            r#"xmlns:AirSyncBase="AirSyncBase:">"#,
            "<Collections></Collections></Sync>"
        );
        let err = Wbxml::new()
            .encode(duplicated)
            .expect_err("duplicate attribute names must be rejected");
        assert!(err.to_string().contains("duplicate"), "{err}");

        // Malformed attribute syntax is an error, never silently skipped.
        let malformed = concat!(
            r#"<Sync xmlns="AirSync:" broken="unclosed>"#,
            "<Collections></Collections></Sync>"
        );
        assert!(
            Wbxml::new().encode(malformed).is_err(),
            "malformed attribute syntax must be rejected"
        );
    }

    /// Character content must be aggregated per element before encoding:
    /// a byte-array element's base64 can arrive split across Text, CData,
    /// and entity-reference events ([MS-ASDTYPE] §2.7.1), and per-event
    /// encoding would emit one OPAQUE (or STR_I) per segment — corrupting
    /// the wire — instead of one token carrying the whole value.
    #[test]
    fn split_element_content_encodes_as_single_wbxml_token() {
        // AirSyncBase:Content is byte-array typed: base64 "QUJD" (=> "ABC")
        // split across a CDATA section and a text node must become exactly
        // one OPAQUE with the three raw bytes, not two partial blobs.
        let split_byte_array = concat!(
            r#"<ItemOperations xmlns="ItemOperations:" xmlns:AirSyncBase="AirSyncBase:">"#,
            "<Response><Fetch><AirSyncBase:Body>",
            "<AirSyncBase:Type>1</AirSyncBase:Type>",
            "<AirSyncBase:Content><![CDATA[QUJ]]>D</AirSyncBase:Content>",
            "</AirSyncBase:Body></Fetch></Response></ItemOperations>"
        );
        let encoded = Wbxml::new()
            .encode(split_byte_array)
            .expect("split byte-array content must encode");
        // One OPAQUE, length 3, bytes 'A','B','C' — and no second OPAQUE.
        // Tokens per the [MS-ASWBXML] tables: page 20 ItemOperations 0x05,
        // Response 0x0E, Fetch 0x06; page 17 AirSyncBase Body 0x0A,
        // Type 0x06, Content 0x1F (the byte-array entry).
        assert_eq!(
            encoded,
            vec![
                0x03,
                0x01,
                0x6A,
                0x00, // header
                0x00,
                0x14,
                0x45,        // SWITCH_PAGE 20, ItemOperations|content
                0x0E | 0x40, // Response|content
                0x06 | 0x40, // Fetch|content
                0x00,
                0x11,
                0x0A | 0x40, // SWITCH_PAGE 17, AirSyncBase Body|content
                0x06 | 0x40, // AirSyncBase Type|content
                0x03,
                b'1',
                0x00,
                0x01,        // STR_I "1", END Type
                0x1F | 0x40, // AirSyncBase Content|content
                0xC3,
                0x03,
                b'A',
                b'B',
                b'C', // OPAQUE len 3, raw bytes
                0x01,
                0x01,
                0x01,
                0x01,
                0x01, // END x5: Content, Body, Fetch, Response, ItemOperations
            ],
            "split content must be one OPAQUE: {encoded:02x?}"
        );

        // Round-trip stability of the aggregated span.
        let decoded = Wbxml::new()
            .decode(&encoded)
            .expect("decode the encoded form");
        assert!(decoded.contains("QUJD"), "{decoded}");
        assert_eq!(
            Wbxml::new().encode(&decoded).expect("re-encode"),
            encoded,
            "decode output must re-encode byte-identically"
        );

        // xsd:base64Binary tolerates whitespace, so a pretty-printed or
        // line-wrapped value must not break the raw-byte decode.
        let pretty_printed = concat!(
            r#"<ItemOperations xmlns="ItemOperations:" xmlns:AirSyncBase="AirSyncBase:">"#,
            "<Response><Fetch><AirSyncBase:Body>",
            "<AirSyncBase:Type>1</AirSyncBase:Type>",
            "<AirSyncBase:Content> QUJD\nRA== </AirSyncBase:Content>",
            "</AirSyncBase:Body></Fetch></Response></ItemOperations>"
        );
        let encoded = Wbxml::new()
            .encode(pretty_printed)
            .expect("whitespace inside base64 must be tolerated");
        assert!(
            encoded.ends_with(&[
                0xC3, 0x04, b'A', b'B', b'C', b'D', 0x01, 0x01, 0x01, 0x01, 0x01
            ]),
            "expected one OPAQUE with raw bytes ABCD: {encoded:02x?}"
        );

        // A string element split by an entity reference encodes as ONE
        // STR_I carrying the whole value: quick-xml emits Text("A "),
        // GeneralRef("amp"), Text(" B") for the source, and the
        // aggregation joins them into a single token.
        let split_string = concat!(
            r#"<Sync xmlns="AirSync:">"#,
            "<Collections><Collection><SyncKey>A &amp; B</SyncKey></Collection></Collections></Sync>"
        );
        let encoded = Wbxml::new()
            .encode(split_string)
            .expect("entity-ref-split string content must encode");
        // The reader trims each text segment (trim_text(true) is what lets
        // pretty-printed templates encode without spurious STR_I tokens),
        // so the aggregated value is "A&B" — in exactly ONE STR_I, which is
        // the invariant under test.
        assert!(
            encoded
                .windows(5)
                .any(|w| w == [0x03, b'A', b'&', b'B', 0x00]),
            "one STR_I must carry the aggregated text: {encoded:02x?}"
        );
        assert!(
            encoded[4..].iter().filter(|&&b| b == 0x03).count() == 1,
            "must be a single STR_I token for the whole value: {encoded:02x?}"
        );
    }

    /// The encode-side mirror of the decode rule: character data outside
    /// the document element is malformed, not a sibling token to emit.
    #[test]
    fn encode_rejects_character_data_outside_document_element() {
        let before_root = "junk<Sync xmlns=\"AirSync:\"></Sync>";
        let err = Wbxml::new()
            .encode(before_root)
            .expect_err("text before the root must be rejected");
        assert!(
            err.to_string().contains("outside the document element"),
            "{err}"
        );

        let after_root = "<Sync xmlns=\"AirSync:\"></Sync>trailing";
        let err = Wbxml::new()
            .encode(after_root)
            .expect_err("text after the root must be rejected");
        assert!(
            err.to_string().contains("outside the document element"),
            "{err}"
        );

        // XML comments and declarations are metadata events, not text;
        // dropping them stays the correct (and previous) behavior.
        let comment_after_root = "<Sync xmlns=\"AirSync:\"><Status>1</Status></Sync><!-- note -->";
        assert!(
            Wbxml::new().encode(comment_after_root).is_ok(),
            "comments outside the root are metadata and must stay encodable"
        );
    }
}
