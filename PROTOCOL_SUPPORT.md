# Protocol Support Documentation

<!-- PROTOCOL_SUPPORT.md -->

## Exchange ActiveSync (EAS) Protocol Version 16.1

### Supported Commands

| Command | Status | Notes |
|---------|--------|-------|
| Sync | ⚠️ Partial | Calendar class only (non-Calendar classes are rejected) |
| FolderSync | ✅ Full | Full hierarchy sync |
| Provision | ✅ Full | Policy key exchange, device info |
| Ping | ✅ Full | Real-time change notifications |
| ItemOperations | ✅ Full | Attachment fetch, item content |
| Search | ✅ Full | GAL and mailbox search |
| MeetingResponse | ✅ Full | Accept/Decline/Tentative |
| Settings | ✅ Full | Device info, OOF, RTF format info |
| ResolveRecipients | ✅ Full | GAL resolution |
| ValidateCert | ✅ Full | Certificate validation |
| GetItemEstimate | ✅ Full | Change count |
| SendMail | ⚠️ Limited | Returns ComposeMail success status only (no message processing) |
| Move | ❌ Not supported | Command not implemented |
| MoveItems | ❌ Not supported | Returns error for this calendar-only mailbox surface |

### Protocol 16.1 Features

- **AirSyncBase:InstanceId**: For exception modifications without Exceptions element
- **AirSyncBase:Location**: Preferred over Calendar:Location
- **OnlineMeetingConfLink/OnlineMeetingExternalLink**: Meeting URL fields
- **ResponseRequested**: Optional in requests
- **Reminder**: Can be empty for no reminder

### Namespaces Supported

| Namespace | Code | Description |
|-----------|------|-------------|
| AirSync | 0 | Core sync protocol |
| AirSyncBase | 17 | Base types (Body, Location, Attachments) |
| Calendar | 4 | Calendar items |
| Contacts | 1 | Contact items |
| Email | 2 | Email items |
| Tasks | 5 | Task items |
| FolderHierarchy | 7 | Folder operations |
| Provision | 11 | Provisioning protocol |
| Settings | 10 | Settings protocol |
| Ping | 9 | Ping protocol |
| ItemOperations | 14 | Item operations |
| Search | 8 | Search protocol |
| MeetingResponse | 6 | Meeting responses |
| Move | 15 | Move operations |
| RightsManagement | 13 | RMS support |

## Exchange Web Services (EWS) - Exchange2016 Schema

### Supported Operations

| Operation | Status | Notes |
|-----------|--------|-------|
| GetFolder | ✅ Full | Calendar, Contacts, Tasks folders |
| FindFolder | ✅ Full | Hierarchy enumeration |
| FindItem | ✅ Full | Calendar item queries |
| GetItem | ✅ Full | Full item retrieval |
| GetUserAvailability | ✅ Full | Free/busy, suggestions |
| SyncFolderItems | ✅ Full | Item-level sync |
| SyncFolderHierarchy | ✅ Full | Folder-level sync |
| Subscribe | ✅ Full | Push/Pull notifications |
| Unsubscribe | ✅ Full | Unsubscribe from notifications |
| CreateItem | ✅ Full | Calendar item creation |
| UpdateItem | ✅ Full | Calendar item updates |
| DeleteItem | ✅ Full | Calendar item deletion |
| ResolveNames | ✅ Full | GAL resolution |
| GetUserOofSettings | ✅ Full | OOF settings |
| SetUserOofSettings | ✅ Full | Set OOF settings |
| GetServiceConfiguration | ✅ Full | Service configuration |
| GetServerTimeZones | ✅ Full | Timezone definitions |
| GetMailTips | ✅ Full | Mail tips |
| FindPeople | ✅ Full | People search |
| GetConversationItems | ✅ Full | Conversation threading |
| ConvertId | ✅ Full | ID format conversion |
| GetRoomLists | ✅ Full | Room list enumeration |
| GetRooms | ✅ Full | Room enumeration |
| GetDelegate | ✅ Full | Delegate permissions |
| GetUserPhoto | ✅ Full | User photos |
| MarkAsJunk | ✅ Full | Junk marking |
| GetAppManifests | ✅ Full | App manifests |
| GetAppMarketplaceUrl | ✅ Full | App marketplace |
| InstallApp | ✅ Full | App installation |
| UninstallApp | ✅ Full | App removal |
| GetClientAccessToken | ✅ Full | App tokens |

### Calendar Item Properties

All EWS calendar item properties are mapped to/from CalDAV:

- Subject, Body, Location
- Start, End, AllDayEvent
- ReminderMinutesBeforeStart, ReminderIsSet
- BusyStatus (LegacyFreeBusyStatus)
- Sensitivity (Normal/Personal/Private/Confidential)
- Organizer (Name, Email)
- RequiredAttendees, OptionalAttendees, Resources
- Categories
- UID, InstanceId
- Recurrence (Daily, Weekly, Monthly, Yearly)
- Exceptions, DeletedOccurrences, ModifiedOccurrences
- OnlineMeetingConfLink, OnlineMeetingExternalLink
- ResponseType, MyResponseType
- MeetingStatus, IsMeeting, IsOrganizer
- EffectiveRights

## Autodiscover

### Supported Formats

| Format | Status | Notes |
|--------|--------|-------|
| XML (2006a) | ✅ Full | Legacy Outlook |
| XML (2006b) | ✅ Full | With Redirect |
| SOAP (2010) | ✅ Full | Modern Outlook |
| POX (Plain Old XML) | ✅ Full | Alternative XML |
| JSON | ✅ Full | Thunderbird |

### Response Elements

- EwsUrl, OabUrl, UMUrl
- ASUrl (ActiveSync URL)
- MobileSyncUrl, MobileSyncCertUrl
- EwsPartnerUrl
- DisplayName, SmtpAddress
- External/Internal URLs
- RedirectUrl, RedirectAddr
- MicrosoftOnline (false for on-prem)
- SharingUrl

## Security Features

### Transport Security
- TLS 1.2+ required
- Certificate validation
- Secure cipher suites

### Authentication
- Basic auth over HTTPS
- HMAC-based server ID generation
- Constant-time secret comparison

### Rate Limiting
- Per-device request limits (60/min)
- Global rate limiting
- Retry-After headers

### Request Validation
- Body size limits (4MB)
- Request timeout (60s)
- Schema validation

## Compatibility Matrix

| Client | Version | EWS | EAS | Notes |
|--------|---------|-----|-----|-------|
| Outlook Windows 11 | 20251205004.10 | ✅ | - | Full calendar support |
| Outlook Android | 5.2613.1 | - | ✅ | Full calendar sync |
| Outlook iOS | Latest | - | ✅ | Basic calendar |
| Thunderbird | 128+ | ✅ | - | Via Autodiscover |
| macOS Calendar | Ventura+ | ✅ | - | Via EWS |
| iOS Calendar | iOS 18+ | - | ✅ | Via EAS |
| Android Calendar | Android 15+ | - | ✅ | Via EAS |