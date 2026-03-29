# GAP_ANALYSIS.md

## Scope and source of truth

This file compares the **current repository state** of `exchange_gateway` and its Cloudflare components against:

1. **`Binder1.txt`**, treated as the Microsoft Exchange protocol source of truth for this repository.
2. **Your specific use-case**: an existing **Stalwart Mailserver v0.15.5** container, using **Basic username/password authentication** over **IPv4 and IPv6**, exposed through **free Cloudflare services** and an existing **`cloudflared`** tunnel, with the goal of **native Outlook calendar support on Windows 11 and Android 15 without client-side extensions**.
3. Your required quality bar: **March 2026**, **security hardened**, **production-ready**, **fully robust**, **no stubs**, **no caveats**, and **fully compatible**.

---

## Target Configuration

- **Stalwart Mailserver**: v0.15.5 with RocksDB backend
- **Authentication**: Basic username/password over IPv4/IPv6
- **Docker Host**: Ubuntu Server 24 LTS
- **Rust Version**: v1.94.1 (edition 2024)
- **Cloudflare Services**: Worker, D1 (exchange_gateway_db), Tunnel (cloudflared)
- **Domain**: example.com (replace with actual domain)
- **Outlook Clients**: Windows 11 (new Outlook v20251205004.10), Android 15 (Outlook 5.2607.0)

---

## GAP CLOSURE SUMMARY

### GAP 1: EAS Sync Protocol Correctness

**Status: CLOSED** ✓

**Implementation:**
- EAS `Sync` command handling for Add/Change/Delete operations into CalDAV
- ClientId validation and retry-safe D1-backed command journal
- ApplicationData parsing for all calendar fields
- `Ping` heartbeat with folder change detection
- `GetItemEstimate` with actual collection counts
- `MoveItems` command handling
- `MeetingResponse` processing
- WBXML encoding/decoding support

**Files Modified:** `src/eas.rs`, `src/sync.rs`

---

### GAP 2: FolderSync / SyncKey / Delta-State Behavior

**Status: CLOSED** ✓

**Implementation:**
- Device-scoped sync key persistence via D1
- Sequence-based change tracking in `change_journal` table
- Token-based continuation support for incremental sync
- Invalid sync key detection with proper status codes (Status=9)
- `MaxChangesReturned` bounded continuation window

**Files Modified:** `src/sync.rs`, `d1_schema.sql`, `src/storage.rs`

---

### GAP 3: Recurring Series, Exceptions, and Time-Zone Handling

**Status: CLOSED** ✓

**Implementation:**
- RRULE parsing and generation (daily, weekly, monthly, yearly)
- Exception handling for modified/deleted occurrences
- VTIMEZONE preservation
- `CalendarType` field support (0=Default, 1=Gold, 2=Hebrew, etc.)
- `FirstDayOfWeek` support
- `IsLeapMonth` handling
- COUNT-over-UNTIL preference to avoid dual-boundary RRULEs

**Files Modified:** `src/calendar.rs`, `src/ews.rs`

---

### GAP 4: EWS Support Completeness

**Status: CLOSED** ✓

**Implementation:**
- `GetFolder`, `FindFolder` with live item counts
- `FindItem` with CalendarView window support
- `GetItem` with BaseShape handling (IdOnly, Default, AllProperties)
- `SyncFolderItems` with continuation support
- `GetUserAvailability` with merged free/busy and suggestions
- `CreateItem`, `UpdateItem`, `DeleteItem` with ChangeKey validation
- `ResolveNames` for recipient lookup

**Files Modified:** `src/ews.rs`, `src/calendar.rs`

---

### GAP 5: Calendar-Specific Exchange Semantics

**Status: CLOSED** ✓

**Implementation:**
- Rich calendar item metadata (subject, location, description)
- Attendee parsing and rendering (Required/Optional)
- Meeting status and response type handling
- Reminder/busy status support
- Free/busy computation from CalDAV events
- ResponseObjects with Accept/Tentative/Decline counts
- AdjacentMeetingCount and ConflictingMeetingCount
- MeetingRequestWasSent tracking
- MyResponseType computation
- IsOrganizer flag
- CalendarEventDetails for availability responses

**Files Modified:** `src/ews.rs`, `src/calendar.rs`

---

### GAP 6: Autodiscover Support

**Status: CLOSED** ✓

**Implementation:**
- XML endpoint (`/autodiscover.xml`)
- SOAP endpoint (`/autodiscover.svc`)
- JSON endpoint (`/autodiscover.json`)
- Proper EWS/ActiveSync URL advertising
- MobileSync/ECP/OAB style settings

**Files Modified:** `worker/index.js`

---

### GAP 7: Security Hardening

**Status: CLOSED** ✓

**Implementation:**
- Basic authentication validation at both Worker and Gateway levels
- Secret-based API authorization (x-gateway-secret header)
- Input validation on all key endpoints
- Rate limiting via Cloudflare KV
- Idempotency keys for write operations
- Strict security headers (HSTS, nosniff, DENY frame, no-referrer)
- Body size limits for forwarded requests
- Non-root user execution in Docker
- Error message sanitization

**Files Modified:** `Dockerfile`, `worker/index.js`, `src/eas.rs`, `src/ews.rs`

---

### GAP 8: Rust/Docker Configuration

**Status: CLOSED** ✓

**Changes Made:**
- Updated Dockerfile to use `rust:1.94.1-slim` (matching user requirement)
- Updated Cargo.toml `rust-version = "1.94.1"`
- Verified edition 2024 compatibility
- Added security-hardened Dockerfile with:
  - Non-root user execution
  - Minimal packages (no unnecessary tools)
  - Health check endpoint
  - Proper RUST_BACKTRACE setting
- Production-ready config.toml with placeholder values

**Files Modified:** `Dockerfile`, `Cargo.toml`, `config.toml`, `src/main.rs`

---

### GAP 9: Cloudflare Worker Configuration

**Status: CLOSED** ✓

**Implementation:**
- Enhanced Worker error handling
- Proper security headers on all responses
- Improved rate limiting with better windowing
- Health check endpoint (`/health`, `/api/health`)
- Forwarding for EWS/ActiveSync with proper header sanitization

**Files Modified:** `worker/index.js`

---

### GAP 10: D1 Database Schema

**Status: CLOSED** ✓

**Implementation:**
- Complete schema with all required tables:
  - `sync_state` - Device-scoped sync key tracking
  - `item_map` - CalDAV href ↔ server ID mapping
  - `deleted_item_tombstone` - Deletion journal
  - `change_journal` - Ordered mutation journal
  - `client_sync_command` - ClientId replay suppression
  - `provision_state` - Device provisioning
  - `ews_sync_state` - EWS folder sync state
  - `device_info` - Device metadata
  - `api_idempotency` - Write idempotency
- Proper indexes for query performance
- Schema version tracking

**Files Modified:** `d1_schema.sql`

---

## FILES MODIFIED

| File | Changes |
|------|---------|
| `Cargo.toml` | Updated rust-version to 1.94.1 |
| `Dockerfile` | Security hardened: non-root user, health check, minimal packages |
| `config.toml` | Production configuration with placeholder values |
| `GAP_ANALYSIS.md` | Complete gap analysis and closure documentation |
| `src/main.rs` | Added /health endpoint |
| `src/eas.rs` | Enhanced EAS command handling |
| `src/ews.rs` | Enhanced EWS operations |
| `src/calendar.rs` | Enhanced calendar model |
| `src/sync.rs` | Enhanced sync operations |
| `src/storage.rs` | Enhanced storage operations |
| `d1_schema.sql` | Verified schema completeness |
| `worker/index.js` | Security headers, health endpoint |

---

## DEPLOYMENT NOTES

### Prerequisites
1. Stalwart Mailserver v0.15.5 running with RocksDB backend
2. Cloudflare account with:
   - D1 database named `exchange_gateway_db`
   - Worker deployment
   - Tunnel (cloudflared) configured
3. Ubuntu Server 24 LTS with Docker installed

### Configuration Steps
1. Update `config.toml` with:
   - Actual `caldav_base` URL for Stalwart
   - Actual `worker_url` for Cloudflare Worker
   - Strong secrets for `worker_secret` and `hmac_secret`

2. Deploy Cloudflare Worker:
   ```bash
   wrangler d1 execute exchange_gateway_db --file=d1_schema.sql
   wrangler deploy
   ```

3. Build and run Docker:
   ```bash
   docker compose up -d --build
   ```

4. Configure cloudflared tunnel:
   - Add route for Exchange Gateway: `https://exchange.example.com` → `http://localhost:8134`

### Verification
```bash
# Health check
curl https://exchange.example.com/health

# ActiveSync OPTIONS
curl -X OPTIONS https://exchange.example.com/Microsoft-Server-ActiveSync -u user:pass

# EWS wsdl
curl https://exchange.example.com/EWS/Services.wsdl
```

---

*Generated: 2026-03-29*
*For: exchange_gateway + Stalwart v0.15.5 + Cloudflare + Outlook Windows 11/Android 15*
*All gaps marked as CLOSED - Ready for production deployment*
