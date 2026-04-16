# Missing Exchange Calendar Features in exchange_gateway

## Executive Summary

This document provides a comprehensive analysis of Exchange calendar features that are **currently missing** or **incomplete** in the exchange_gateway implementation. The analysis is based on Microsoft's official EWS documentation and comparison with Exchange Server capabilities.

---

## 1. Calendar Permissions & Sharing

### ❌ Missing: Calendar Permissions Management

**Exchange Feature**: Fine-grained calendar permissions and sharing
- `PermissionLevel` (Owner, Editor, Author, Reviewer, Contributor, FreeBusyTimeOnly, etc.)
- `CanCreateItems`, `CanReadItems`, `CanEditItems`, `CanDeleteItems`
- `CalendarPermissionLevel` specific to calendars
- `FolderPermission` for delegation

**Current State**: 
- ❌ No calendar permission CRUD operations
- ❌ No sharing invitation mechanism
- ❌ No permission level management
- ⚠️ Stub `GetDelegate` operation (returns hardcoded response)

**Impact**: 
- Users cannot share calendars with colleagues
- No delegate access for assistants
- Cannot set read-only or edit permissions
- Privacy and security limitations

**EWS Operations Missing**:
- `AddDelegate`
- `UpdateDelegate`
- `RemoveDelegate`
- `GetFolderPermissions` (custom)
- `SetFolderPermissions` (custom)

---

## 2. Meeting Workflow & Responses

### ❌ Missing: Full Meeting Request Workflow

**Exchange Feature**: Complete meeting request/response workflow
- `AcceptItem` - Accept meeting invitations
- `DeclineItem` - Decline meeting invitations
- `TentativelyAcceptItem` - Tentatively accept
- `CancelCalendarItem` - Cancel meetings as organizer
- Meeting request tracking and status

**Current State**:
- ⚠️ `AcceptItem`, `DeclineItem`, `TentativelyAcceptItem` mentioned in response objects
- ❌ No actual implementation of response processing
- ❌ No meeting request message handling
- ❌ No `CancelCalendarItem` operation
- ❌ No `SuppressReadReceipt` operation

**Impact**:
- Recipients cannot respond to meeting invitations
- Organizers cannot cancel meetings properly
- Meeting status not properly tracked
- Meeting request messages not created/sent

**EWS Operations Missing**:
- Full `CreateItem` with meeting response items
- Meeting cancellation workflow
- Meeting request message generation

---

## 3. Calendar Attachments

### ❌ Missing: Calendar Item Attachments

**Exchange Feature**: Attach files and items to calendar items
- `FileAttachment` - Attach files to appointments
- `ItemAttachment` - Attach other items (contacts, messages)
- `CreateAttachment` - Add attachments
- `GetAttachment` - Retrieve attachments
- `DeleteAttachment` - Remove attachments
- Inline attachments (embedded images in descriptions)

**Current State**:
- ⚠️ `CreateAttachment`, `GetAttachment`, `DeleteAttachment` are **stub operations**
- ❌ No attachment storage in CalDAV backend
- ❌ No attachment handling in ICS parsing/rendering
- ⚠️ EAS reports `<AttachmentsEnabled>1</AttachmentsEnabled>` but not implemented

**Impact**:
- Cannot attach meeting agendas, presentations, documents
- No support for inline images in descriptions
- Meeting materials not accessible to attendees
- Loss of critical meeting context

**Implementation Gap**:
- CalDAV doesn't natively support attachments
- Would require separate storage solution (S3, database, etc.)
- ICS format has ATTACH property (RFC 5545 Section 3.8.1.1) but limited

---

## 4. Recurring Meeting Enhancements

### ⚠️ Partial: Advanced Recurrence Features

**Exchange Feature**: Complete recurrence support
- Recurrence patterns: Daily, Weekly, Monthly, Yearly
- Recurrence ranges: NoEnd, EndDate, NumberedOccurrences
- Exception handling: Modified occurrences, Deleted occurrences
- `RecurringMasterItemId` reference
- FirstOccurrence, LastOccurrence tracking
- ModifiedOccurrences, DeletedOccurrences collections

**Current State**:
- ✅ Basic recurrence patterns (RRULE parsing)
- ✅ EXDATE for deleted occurrences
- ✅ Modified exception handling (CalendarException struct)
- ❌ No `NumberedRecurrence` endpoint support
- ❌ No `EndDateRecurrence` endpoint support
- ❌ No `FirstOccurrence`/`LastOccurrence` tracking
- ❌ Limited recurrence modification operations

**Impact**:
- Cannot limit series by occurrence count
- Limited recurrence range modifications
- Occurrence tracking incomplete
- Some Outlook features unavailable

---

## 5. Time Zone Management

### ⚠️ Partial: Advanced Timezone Features

**Exchange Feature**: Complete timezone management
- `TimeZoneDefinition` with full transition rules
- Standard/Daylight time transitions
- `Bias`, `StandardBias`, `DaylightBias`
- `TransitionsGroup` for complex rules
- Historical timezone data
- Timezone conversion operations

**Current State**:
- ✅ Basic timezone support (VTIMEZONE)
- ✅ Timezone conversion (chrono-tz)
- ❌ No `GetServerTimeZones` implementation (stub)
- ❌ No timezone transition rule storage
- ❌ Limited timezone definition in responses
- ❌ No historical timezone data

**Impact**:
- Recurring meetings across DST changes may have issues
- Limited timezone metadata for clients
- Some Outlook timezone features unavailable

---

## 6. Calendar Views & Queries

### ⚠️ Partial: Advanced Calendar Queries

**Exchange Feature**: Rich calendar querying
- `CalendarView` with date range
- `RecurringMasterItemId` expansion
- `Occurrences` collection
- Filtered queries by properties
- Grouped results

**Current State**:
- ✅ Basic `FindItem` with date range
- ✅ Calendar view support
- ⚠️ Limited recurrence expansion in responses
- ❌ No grouped calendar queries
- ❌ No advanced filtering

**Impact**:
- Limited calendar browsing in clients
- Some query optimizations unavailable
- Complex calendar searches not supported

---

## 7. Free/Busy Enhancements

### ⚠️ Partial: Advanced Availability Features

**Exchange Feature**: Complete availability service
- `GetUserAvailability` with detailed free/busy
- `FreeBusyViewType` (None, FreeBusy, FreeBusyMerged, Detailed, DetailedMerged)
- `MergedFreeBusy` with configurable intervals
- `CalendarEventDetails` with private events (optional)
- Working hours and timezone
- Suggestions for meeting times

**Current State**:
- ✅ Basic `GetUserAvailability` implemented
- ✅ `MergedFreeBusy` support
- ✅ `FreeBusyViewType` support
- ✅ Basic meeting time suggestions
- ❌ No `CalendarEventDetails` with private events
- ❌ No working hours configuration
- ❌ Limited suggestion quality (hardcoded "Excellent")
- ❌ No detailed availability with subject/location

**Impact**:
- Limited availability detail for privacy scenarios
- No custom working hours
- Suggestion quality not personalized

---

## 8. Room & Resource Management

### ❌ Missing: Room and Resource Booking

**Exchange Feature**: Room and resource scheduling
- `GetRoomLists` - Get room list distribution groups
- `GetRooms` - Get rooms from a room list
- Resource mailboxes (rooms, equipment)
- Automatic room booking and conflict detection
- Resource approval workflow

**Current State**:
- ❌ `GetRoomLists` is a **stub operation**
- ❌ `GetRooms` is a **stub operation**
- ❌ No resource mailbox support
- ❌ No room booking logic
- ❌ No conflict detection for resources

**Impact**:
- Cannot book meeting rooms
- No equipment reservation
- Manual room coordination required
- No automatic conflict resolution

---

## 9. Meeting Enhancements

### ⚠️ Partial: Advanced Meeting Features

**Exchange Feature**: Complete meeting capabilities
- Online meeting integration (Teams, Skype)
- Meeting workspace links
- Agenda and notes tracking
- `OnlineMeetingSettings` (Lobby, AccessLevel, Participants)
- `JoinOnlineMeetingUrl`

**Current State**:
- ✅ `X-MS-OLK-CONFLINK` (online meeting links) - stored as property
- ✅ `X-MS-OLK-EXTERNALLINK` (external links)
- ❌ No `OnlineMeetingSettings` structure
- ❌ No automatic meeting link generation
- ❌ No meeting workspace integration
- ❌ No agenda/notes separation

**Impact**:
- Manual meeting link management
- No Teams/Skype integration
- Limited meeting metadata

---

## 10. Notification & Subscriptions

### ⚠️ Partial: Calendar Notifications

**Exchange Feature**: Real-time calendar notifications
- `Subscribe` with push/pull/streaming notifications
- Calendar folder change notifications
- Meeting request/response notifications
- `SyncFolderHierarchy` for folder changes
- `SyncFolderItems` for item changes

**Current State**:
- ⚠️ `Subscribe` and `Unsubscribe` defined but limited
- ✅ `SyncFolderItems` implemented (basic)
- ⚠️ `SyncFolderHierarchy` is a **stub operation**
- ❌ No push notifications
- ❌ No streaming notifications
- ❌ Limited event type filtering

**Impact**:
- Clients must poll for changes
- No real-time calendar updates
- Higher latency for notifications
- Increased server load from polling

---

## 11. In-Place Archive & Retention

### ❌ Missing: Archive and Retention Policies

**Exchange Feature**: Calendar archiving
- Archive calendar folder
- Retention policies for calendar items
- In-Place Hold for calendar items
- Archive item movement

**Current State**:
- ❌ No archive calendar support
- ❌ No retention policy tags
- ❌ No `ArchiveItem` operation
- ❌ No compliance features

**Impact**:
- No calendar item lifecycle management
- No compliance with retention requirements
- Manual calendar cleanup required

---

## 12. Bulk Operations

### ⚠️ Partial: Batch Calendar Operations

**Exchange Feature**: Efficient bulk operations
- Process multiple calendar items in one request
- Batch create/update/delete
- Error handling per-item
- Transaction-like semantics

**Current State**:
- ⚠️ Basic batch support in `CreateItem`/`UpdateItem`/`DeleteItem`
- ❌ No `ProcessCalendarItemsInBatches` implementation
- ❌ Limited error handling for batch items
- ❌ No partial success reporting

**Impact**:
- Less efficient for bulk operations
- Limited error recovery
- Client must handle failures manually

---

## 13. Enhanced Recurrence Operations

### ❌ Missing: Advanced Recurrence Modification

**Exchange Feature**: Modify entire series or single occurrence
- Update recurring master (affects all occurrences)
- Update single occurrence (creates exception)
- Restore occurrence to series (remove exception)
- Series expansion control

**Current State**:
- ⚠️ Basic recurrence modification
- ⚠️ Exception creation on single occurrence update
- ❌ No "restore to series" operation
- ❌ Limited series-level operations
- ❌ No occurrence reordering

**Impact**:
- Cannot revert modified occurrences
- Limited series manipulation
- Some Outlook features unavailable

---

## 14. Calendar Item Classes

### ⚠️ Partial: Specialized Calendar Item Types

**Exchange Feature**: Multiple calendar item types
- Single appointments
- Recurring appointments
- Meetings (with attendees)
- Meeting requests (invitations)
- Meeting responses (accept/decline/tentative)
- Meeting cancellations
- Calendar exception items

**Current State**:
- ✅ Single appointments
- ✅ Recurring appointments
- ✅ Basic meetings (stored as CalendarItem)
- ❌ No distinct `MeetingRequestMessageType`
- ❌ No distinct `MeetingCancellationMessageType`
- ❌ Limited meeting message handling

**Impact**:
- Meeting requests not fully differentiated
- Limited workflow tracking
- Some client features unavailable

---

## 15. Location & Address Resolution

### ⚠️ Partial: Location Enhancement

**Exchange Feature**: Rich location support
- `Location` with display name
- `LocationUri` for reference
- `LocationEmailAddress` for room mailboxes
- `PostalAddress` with street/city/state/country
- Multiple locations

**Current State**:
- ✅ Basic `Location` string field
- ❌ No `LocationUri`
- ❌ No `PostalAddress` structure
- ❌ No multiple locations
- ❌ No location resolution (ResolveNames for rooms)

**Impact**:
- Limited location metadata
- No map integration
- Manual address handling

---

## 16. Enhanced Attendee Management

### ⚠️ Partial: Advanced Attendee Features

**Exchange Feature**: Complete attendee handling
- Required, Optional, Resources
- `ResponseType` tracking (Organizer, Tentative, Accept, Decline, NoResponseReceived)
- `LastResponseTime`
- Attendee permissions
- `SendMeetingInvitationsOrCancellations` control

**Current State**:
- ✅ Basic attendee support (Attendee struct)
- ✅ `attendee_type` (Required, Optional, Resource)
- ✅ `attendee_status` and `partstat`
- ✅ `response_type` tracking
- ❌ No `LastResponseTime` tracking
- ❌ No attendee-specific permissions
- ❌ Limited response time history

**Impact**:
- Limited response tracking
- No attendee-level permissions
- Reduced meeting insights

---

## Summary Matrix

| Feature Category | Status | Priority | Effort | CalDAV Support |
|------------------|--------|----------|--------|----------------|
| Calendar Permissions | ❌ Missing | **High** | High | ❌ No native support |
| Meeting Workflow | ❌ Missing | **Critical** | High | ⚠️ Partial |
| Attachments | ❌ Missing | High | High | ❌ Limited (ATTACH) |
| Advanced Recurrence | ⚠️ Partial | Medium | Medium | ✅ Yes |
| Timezone Management | ⚠️ Partial | Medium | Medium | ✅ Yes |
| Calendar Views | ⚠️ Partial | Medium | Low | ✅ Yes |
| Free/Busy Enhancements | ⚠️ Partial | Medium | Medium | ✅ Yes |
| Room/Resource Booking | ❌ Missing | High | High | ❌ No |
| Online Meetings | ⚠️ Partial | Medium | Low | ✅ Custom properties |
| Notifications | ⚠️ Partial | Medium | High | ⚠️ SyncCollections |
| Archive/Retention | ❌ Missing | Low | High | ❌ No |
| Bulk Operations | ⚠️ Partial | Medium | Medium | ✅ Yes |
| Enhanced Recurrence | ❌ Missing | Medium | Medium | ✅ Yes |
| Calendar Item Classes | ⚠️ Partial | Medium | High | ⚠️ Partial |
| Location Enhancement | ⚠️ Partial | Low | Low | ⚠️ LOCATION prop |
| Attendee Management | ⚠️ Partial | Medium | Medium | ✅ Yes |

---

## Critical Missing Features (Priority Ranking)

### 1. **CRITICAL: Meeting Workflow & Responses**
**Why**: Core calendar functionality - users cannot respond to meeting invitations
**Impact**: Blocks real-world usage for teams
**Effort**: High (requires message generation, tracking, state machine)
**CalDAV**: Scheduling extensions (RFC 6638) provide some support

### 2. **HIGH: Calendar Permissions & Sharing**
**Why**: Essential for team collaboration and delegation
**Impact**: No calendar sharing, assistants cannot manage calendars
**Effort**: High (requires permission model, storage, enforcement)
**CalDAV**: No native support - requires custom implementation

### 3. **HIGH: Room & Resource Booking**
**Why**: Essential for physical meeting coordination
**Impact**: Manual room booking, no conflict detection
**Effort**: High (requires resource mailbox concept, availability checking)
**CalDAV**: No native support - requires custom implementation

### 4. **HIGH: Attachments**
**Why**: Meeting agendas, presentations, supporting materials
**Impact**: Critical context lost, manual workarounds needed
**Effort**: High (requires storage backend, attachment handling)
**CalDAV**: Limited (ATTACH property but no content)

---

## Recommended Implementation Order

### Phase 1: Critical Features (3-6 months)
1. **Meeting Response Workflow** (AcceptItem, DeclineItem, TentativelyAcceptItem)
2. **Meeting Cancellation** (CancelCalendarItem)
3. **Meeting Request Generation** (CreateItem with meeting invitations)

### Phase 2: High Priority (6-12 months)
4. **Calendar Permissions Model** (permission storage and enforcement)
5. **Delegate Management** (AddDelegate, UpdateDelegate, RemoveDelegate)
6. **Room List/Resource Stubs → Real Implementation**
7. **Attachment Support** (with S3/database storage backend)

### Phase 3: Medium Priority (12-18 months)
8. **Enhanced Recurrence Operations** (restore to series, series-level updates)
9. **Advanced Free/Busy** (detailed availability, working hours)
10. **Real-time Notifications** (push/streaming subscriptions)
11. **Bulk Operations Enhancement** (better error handling)

### Phase 4: Lower Priority (18+ months)
12. **Location Enhancement** (PostalAddress, multiple locations)
13. **Archive/Retention Policies**
14. **Advanced Timezone Definitions**
15. **Calendar Item Class Differentiation**

---

## CalDAV Limitations

Several missing features stem from CalDAV protocol limitations:

| Feature | CalDAV Support | Workaround |
|---------|----------------|------------|
| Permissions | ❌ No | Custom extension or separate storage |
| Attachments | ⚠️ Limited (ATTACH URL only) | External storage (S3/database) |
| Room Booking | ❌ No | Custom calendar per resource |
| Delegation | ❌ No | Custom permission model |
| Notifications | ⚠️ SyncCollections | Polling or WebSocket extension |
| Archive | ❌ No | Separate archive calendar |
| Bulk Operations | ✅ Yes (multiget/multiput) | Use CALDAV:bulk extension |

**Key Insight**: Many Exchange features require **custom extensions** beyond CalDAV, which is why Grommunio uses its own storage backend (exmdb) instead of CalDAV.

---

## Conclusion

The exchange_gateway implementation covers **basic calendar operations** well (CRUD, recurrence, timezones, attendees, free/busy). However, it lacks **critical collaboration features** that make Exchange calendars useful in team environments:

**Strengths**:
- ✅ Solid calendar item CRUD
- ✅ Recurrence support (RRULE, EXDATE)
- ✅ Timezone handling
- ✅ Basic free/busy
- ✅ Microsoft property extensions

**Critical Gaps**:
- ❌ Meeting response workflow
- ❌ Calendar permissions/sharing
- ❌ Room/resource booking
- ❌ Attachments

**Recommendation**: Focus Phase 1 implementation on **meeting workflow** to enable real-world team calendar usage. This requires implementing the full meeting request/response state machine and message generation.

---

**Document Version**: 1.0  
**Analysis Date**: 2026-04-16  
**Based on**: Exchange Server 2019/Exchange Online EWS capabilities  
**PR #1461 Status**: Includes improvements but doesn't address these gaps