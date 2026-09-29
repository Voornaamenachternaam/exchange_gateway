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
        // The code page the root element was decoded on: the document's
        // default namespace (see the tag-emission note below).
        let mut root_code_page: Option<u8> = None;
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
                            root_code_page.get_or_insert(current_code_page);
                            // WBXML code pages ARE namespaces: the document's
                            // root page is its default namespace, so tags on
                            // that page expand UNQUALIFIED while tags reached
                            // through a SWITCH_PAGE keep their prefix
                            // (e.g. `<AirSyncBase:Body>` inside a Sync
                            // document rooted on the AirSync page). This
                            // matches the XML a real Exchange WBXML-to-XML
                            // expansion produces.
                            let display_name =
                                if current_code_page == root_code_page.unwrap_or(u8::MAX) {
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
        // Parallel stack: whether each open element carries a byte-array value
        // that must be OPAQUE-encoded ([MS-ASDTYPE] §2.7.1).
        let mut byte_array_stack: Vec<bool> = Vec::new();

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
                        if key_bytes.starts_with("xmlns:") && key_bytes.len() > 6 {
                            let prefix = key_bytes[6..].to_string();
                            if let Ok(val) =
                                attr.normalized_value(quick_xml::XmlVersion::Implicit1_0)
                            {
                                let cp = namespace_to_code_page(val.as_ref());
                                new_prefixes.insert(prefix, cp);
                            }
                        }
                    }
                    prefix_ns_stack.push(new_prefixes);

                    let ns_cp = extract_xmlns_cp(e);
                    ns_stack.push(ns_cp);

                    let qname = e.name();
                    let full_name = qname.as_ref();
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
                            full_name,
                            ns_cp.or_else(|| ns_stack.iter().rev().find_map(|&x| x)),
                        )
                    };

                    // Track byte-array-typed elements: their content MUST be
                    // transmitted as WBXML OPAQUE data with raw bytes
                    // ([MS-ASDTYPE] §2.7.1), not as an inline base64 string.
                    let resolved = find_encode_tag(local_name, effective_cp);
                    byte_array_stack.push(
                        resolved.is_some_and(|(cp, token)| is_byte_array_element(cp, token)),
                    );

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
                        if key_bytes.starts_with("xmlns:") && key_bytes.len() > 6 {
                            let prefix = key_bytes[6..].to_string();
                            if let Ok(val) =
                                attr.normalized_value(quick_xml::XmlVersion::Implicit1_0)
                            {
                                let cp = namespace_to_code_page(val.as_ref());
                                new_prefixes.insert(prefix, cp);
                            }
                        }
                    }
                    prefix_ns_stack.push(new_prefixes);

                    let ns_cp = extract_xmlns_cp(e);

                    let qname = e.name();
                    let full_name = qname.as_ref();
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
                            full_name,
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
                    let txt = e.to_string();
                    if !txt.is_empty() {
                        write_element_content(&mut buf, &byte_array_stack, &txt)?;
                    }
                }
                Ok(quick_xml::events::Event::GeneralRef(ref r)) => {
                    let text = resolve_xml_reference_strict(r.as_ref()).ok_or_else(|| {
                        anyhow!(
                            "XML encode error: unsupported entity reference &{};",
                            r.as_ref()
                        )
                    })?;
                    if !text.is_empty() {
                        write_element_content(&mut buf, &byte_array_stack, &text)?;
                    }
                }
                Ok(quick_xml::events::Event::End(_)) => {
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

fn extract_xmlns_cp<'a>(e: &quick_xml::events::BytesStart<'a>) -> Option<u8> {
    for attr in e.attributes().flatten() {
        if attr.key.as_ref() == "xmlns"
            && let Ok(val) = attr.normalized_value(quick_xml::XmlVersion::Implicit1_0)
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

/// Byte-array-typed elements whose content MUST be transmitted as WBXML
/// OPAQUE data carrying raw bytes ([MS-ASDTYPE] §2.7.1), with the in-memory
/// XML representation holding the base64 text:
///
/// - Email2:ConversationId/ConversationIndex ([MS-ASEMAIL] §2.2.2.21,
///   §2.2.2.22: "transferred as an opaque binary large object (BLOB)"),
///   code page 22 tokens 0x09/0x0A.
/// - AirSyncBase:Content ([MS-ASAIRS] §2.2.2.15: "string data type byte
///   array, as specified in [MS-ASDTYPE] section 2.7.1"), code page 17
///   token 0x1F.
///
/// Note that itemoperations:ConversationId ([MS-ASCMD] §2.2.3.35.1) is a
/// plain string despite sharing the local name with the Email2 element, so
/// the match is on the resolved (code page, token) pair, never on the bare
/// local name.
fn is_byte_array_element(code_page: u8, token: u8) -> bool {
    // Byte-array elements ([MS-ASDTYPE] §2.7.1) encode as WBXML OPAQUE with
    // the raw bytes; the XML form carries the same bytes base64-encoded.
    matches!(
        (code_page, token),
        (2, 0x34)    // Email:GlobalObjId ([MS-ASEMAIL] §2.2.2.37 ABNF)
            | (15, 0x20) // Search:ConversationId ([MS-ASCON] §2.2.2.3.2)
            | (17, 0x1F) // AirSyncBase:Content ([MS-ASAIRS] §2.2.2.18)
            | (20, 0x18) // ItemOperations:ConversationId ([MS-ASCON] §2.2.2.3.1)
            | (22, 0x09) // Email2:ConversationId ([MS-ASEMAIL] §2.2.2.21)
            | (22, 0x0A) // Email2:ConversationIndex ([MS-ASEMAIL] §2.2.2.22)
    )
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
    let raw = base64::engine::general_purpose::STANDARD
        .decode(text.trim())
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
    use super::Wbxml;

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
        assert!(decoded.contains("<Sync><Collections>"), "decoded: {decoded}");
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
}
