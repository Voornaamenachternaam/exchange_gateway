# Exchange Gateway - Gap Analysis for Production Use-Case

## Use-Case Summary
- **Objective**: Full native calendar sync between Stalwart Mailserver v0.15.5 (RocksDB backend, ACME TLS) and Outlook clients (Windows 11 new Outlook v20251205004.10, Android 15 Outlook v5.2607.0) without client-side extensions
- **Architecture**: 
  - Exchange Gateway Rust Docker container (v1.94.1) running alongside Stalwart Mailserver
  - Cloudflare Worker (worker/index.js) for D1 database access
  - Cloudflare D1 SQL database for persistence
  - Cloudflare Tunnel (cloudflared) for TLS termination to exchange_gateway container
  - Basic authentication with username/password

## Gaps Identified - With Implementation Status

### G1: Autodiscover JSON Endpoint Missing in Rust Gateway
**Status**: ✅ CLOSED
**Severity**: CRITICAL
**Description**: Outlook clients (especially Android) often use Autodiscover JSON endpoint for auto-configuration. The Rust gateway only handles XML and SOAP endpoints. The Cloudflare Worker has a stub but the Rust gateway needs to handle this route natively for proper Outlook integration.

**Implementation**: Implemented `handle_autodiscover_json` function in `src/autodiscover.rs` that returns complete JSON configuration for:
- Outlook for Windows (new Outlook)
- Outlook for iOS/Android
- Office 365 hybrid configurations
- Full protocol URLs for EWS, ActiveSync, OWA, MAPI

### G2: Outlook for Windows Compatibility - MAPI/HTTP Not Supported
**Status**: ⏳ ACKNOWLEDGED LIMITATION
**Severity**: HIGH
**Description**: The "new" Outlook for Windows uses MAPI over HTTP protocol which is not implemented. While EAS provides basic sync, the full Outlook experience requires MAPI endpoint compatibility.

**Note**: This is a protocol gap that cannot be fully closed without significant implementation. EAS (ActiveSync) provides sufficient calendar functionality for basic use with new Outlook.

### G3: Free/Busy Lookup Not Implemented
**Status**: ✅ ALREADY IMPLEMENTED
**Severity**: HIGH
**Description**: When scheduling meetings, Outlook requests free/busy information via EWS GetUserAvailability. The implementation exists and queries Stalwart CalDAV for attendee availability.

**Implementation**: The `merged_freebusy_for_mailbox` function in `src/ews.rs` properly queries CalDAV and returns merged free/busy status with proper granularity.

### G4: Meeting Response Handling - Attendee Status Updates
**Status**: ⚠️ PARTIALLY IMPLEMENTED
**Severity**: HIGH
**Description**: When a user accepts/declines/tentatively accepts a meeting from Outlook, the response needs to be sent back to the organizer's calendar via CalDAV. The EAS MeetingResponse command is implemented but CalDAV inverse sync is incomplete.

**Note**: Partial implementation exists. Full implementation would require CalDAV scheduling (iCalendar REQUEST, REPLY, CANCEL).

### G5: Cloudflare Worker CORS Configuration
**Status**: ✅ CLOSED
**Severity**: MEDIUM
**Description**: The Cloudflare Worker may need CORS headers for browser-based testing or direct API access.

**Implementation**: Added CORS helper functions and applied to API responses in `worker/index.js`:
- Added `CORS_HEADERS` constant
- Added `withCors()` wrapper function
- Added `corsPreflightResponse()` for OPTIONS
- Added OPTIONS handling in main fetch

### G6: Calendar Recurrence Edge Cases
**Status**: ⏳ PARTIALLY IMPLEMENTED
**Severity**: MEDIUM
**Description**: Some complex recurrence patterns (e.g., BYMONTHDAY with BYMONTH, custom end dates) may not be fully handled.

**Note**: Current implementation handles most common patterns via `rrule` crate.

### G7: Timezone Handling for Non-IANA Timezones
**Status**: ✅ CLOSED
**Severity**: MEDIUM
**Description**: Windows uses Windows Time Zone IDs while we use IANA timezone IDs. Conversion needs to handle all Windows timezone IDs.

**Implementation**: Created `src/timezone.rs` with:
- `WINDOWS_TO_IANA` mapping table for 100+ Windows timezone IDs
- `IANA_TO_WINDOWS` reverse mapping
- `windows_to_iana()` and `iana_to_windows()` conversion functions
- `normalize_timezone()` for parsing various formats
- Comprehensive test coverage

### G8: Security - Rate Limiting in Rust Gateway
**Status**: ⏳ PARTIALLY IMPLEMENTED
**Severity**: MEDIUM
**Description**: Rate limiting is implemented in Cloudflare Worker but not in the Rust gateway itself.

**Note**: Cloudflare provides rate limiting at the edge. Gateway rate limiting would be for direct access scenarios.

### G9: OAuth2/Bearer Token Authentication Not Supported
**Status**: ⏳ NOT IMPLEMENTED
**Severity**: MEDIUM
**Description**: The gateway only supports Basic authentication. Modern Outlook may require OAuth2.

**Note**: Basic auth works for current use-case. OAuth2 would be future enhancement.

### G10: Calendar Folder Discovery
**Status**: ⚠️ PARTIALLY IMPLEMENTED
**Severity**: MEDIUM
**Description**: Users may have multiple calendars. The current implementation defaults to the first calendar found.

**Note**: Partial implementation exists. Full discovery would require EWS FindFolder/SyncFolderHierarchy enhancements.

### G11: ItemID Stability Across Devices
**Status**: ⚠️ PARTIALLY IMPLEMENTED
**Severity**: MEDIUM
**Description**: ServerIDs generated via HMAC should be stable, but cross-device sync may have issues if HMAC secret differs.

**Note**: Implementation uses HMAC-SHA256 for stable server IDs across devices.

### G12: Attachment Handling Not Implemented
**Status**: ⏳ NOT IMPLEMENTED
**Severity**: LOW
**Description**: Calendar items with attachments (meeting room resources, files) are not fully supported.

**Note**: Would require EWS GetItem/UpdateItem with attachments support via CalDAV.

### G13: Notes/Categories Color Sync
**Status**: ⚠️ PARTIALLY IMPLEMENTED
**Severity**: LOW
**Description**: Outlook categories with colors need proper iCalendar CATEGORIES and X-COLOR extensions.

**Note**: Basic categories support exists in calendar.rs. Color mapping would be enhancement.

### G14: Docker Container Health Check Enhancement
**Status**: ✅ CLOSED
**Severity**: LOW
**Description**: Basic HTTP health check exists but could verify database connectivity.

**Implementation**: Enhanced health check in `src/main.rs` to verify Cloudflare Worker connectivity:
- Checks storage.get_latest_change_seq() 
- Returns 503 if worker is unavailable
- Provides proper status codes for load balancers

### G15: Configuration Validation at Startup
**Status**: ✅ CLOSED
**Severity**: LOW
**Description**: Configuration is loaded but not fully validated.

**Implementation**: Added comprehensive config validation in `src/config.rs`:
- Validates required fields (bind, caldav_base, worker_url, worker_secret, hmac_secret)
- Validates URL formats for caldav_base and worker_url
- Warns about weak secrets (<16 chars for worker_secret, <32 for hmac_secret)
- Validates gateway_host format (should be hostname only)

### G16: Multi-Folder Calendar Sync
**Status**: ⏳ NOT IMPLEMENTED
**Severity**: MEDIUM
**Description**: Currently only syncs default calendar. Users may have additional calendars.

**Note**: Would require implementing multi-calendar discovery and sync.

### G17: Out-of-Office (OOF) Status Sync
**Status**: ⏳ NOT IMPLEMENTED
**Severity**: LOW
**Description**: Outlook out-of-office settings need to sync with CalDAV.

**Note**: Would require implementation via CalDAV or EWS SetUserOofSettings.

### G18: Contacts and Tasks Sync
**Status**: ⚠️ PARTIALLY IMPLEMENTED
**Severity**: LOW
**Description**: Only calendar is fully implemented. Contacts and Tasks use placeholder data.

**Note**: Placeholder support exists. Full CalDAV addressbook and task-list support would be required.

### G19: Push Notifications
**Status**: ⏳ NOT IMPLEMENTED
**Severity**: MEDIUM
**Description**: Outlook uses Push for real-time updates but EAS Ping is basic.

**Note**: Basic EAS Ping exists. Long-lived connections with proper heartbeat enhancement would be needed.

### G20: Error Handling and Logging Enhancement
**Status**: ⚠️ PARTIALLY IMPLEMENTED
**Severity**: LOW
**Description**: Basic logging exists but needs structured logging for production debugging.

**Note**: Basic tracing exists. Structured logging with correlation IDs would be enhancement.

---

## Priority Classification - Status Summary

### Critical (Must Fix)
- ✅ G1: Autodiscover JSON - **CLOSED**
- ✅ G3: Free/Busy Lookup - **ALREADY IMPLEMENTED**
- ⚠️ G4: Meeting Response Handling - **PARTIALLY IMPLEMENTED**

### High Priority
- ⏳ G7: Timezone Mapping - **CLOSED**
- ⚠️ G10: Calendar Folder Discovery - **PARTIALLY IMPLEMENTED**
- ⏳ G16: Multi-Folder Calendar Sync - **NOT IMPLEMENTED**

### Medium Priority
- ✅ G5: CORS - **CLOSED**
- ⚠️ G8: Rate Limiting - **PARTIALLY IMPLEMENTED** (Cloudflare provides)
- ⚠️ G11: ItemID Stability - **PARTIALLY IMPLEMENTED**
- ⚠️ G13: Categories Color - **PARTIALLY IMPLEMENTED**
- ✅ G14: Health Check - **CLOSED**
- ✅ G15: Config Validation - **CLOSED**
- ⏳ G19: Push Notifications - **NOT IMPLEMENTED**

### Lower Priority
- ⏳ G2: MAPI/HTTP - **ACKNOWLEDGED LIMITATION**
- ⚠️ G6: Recurrence Edge Cases - **PARTIALLY IMPLEMENTED**
- ⏳ G9: OAuth2 - **NOT IMPLEMENTED**
- ⏳ G12: Attachments - **NOT IMPLEMENTED**
- ⏳ G17: OOF - **NOT IMPLEMENTED**
- ⚠️ G18: Contacts/Tasks - **PARTIALLY IMPLEMENTED**
- ⚠️ G20: Logging - **PARTIALLY IMPLEMENTED**