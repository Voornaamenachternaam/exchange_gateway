# Exchange Gateway - Project Knowledge

## Project Overview
Rust-based EWS/ActiveSync gateway that translates Exchange protocols (EWS SOAP, ActiveSync WBXML) to CalDAV for Stalwart Mailserver. Enables Outlook (Windows/Android) and OneCalendar to use Stalwart calendars natively.

## Build & Test
- **Build (dev):** `cargo check` or `cargo build`
- **Build (release):** `cargo build --release --bin exchange_gateway`
- **Test:** `cargo test` (50 unit tests + 22 protocol fixtures + 11 snapshot tests)
- **Docker:** `docker build -t exchange-gateway:latest .`
- **Rust version:** 1.95.0 (pinned in Dockerfile)

## Architecture
- `src/ews.rs` — EWS SOAP handler (SyncFolderItems, GetItem, UpdateItem, CreateItem, DeleteItem, etc.)
- `src/eas.rs` — ActiveSync WBXML handler
- `src/caldav.rs` — CalDAV client (PUT/GET/DELETE events with etag-based optimistic concurrency, connection retry, URL encoding)
- `src/sync.rs` — EAS Sync protocol implementation
- `src/ews_update.rs` — EWS UpdateItem field change parsing
- `src/calendar.rs` — Calendar item model, ICS parsing/rendering, EWS XML parsing
- `src/storage.rs` — SQLite persistence (item_map, change_journal, tombstones)
- `src/auth.rs` — Auth verification with fail-open on unreachable Stalwart backend
- `src/logging.rs` — Configurable logging (pretty/compact/json, timestamps, threads, targets)
- `src/config.rs` — Environment variable configuration
- `src/autodiscover.rs` — Autodiscover XML/SOAP/JSON endpoints
- `src/ews_folders.rs` — EWS folder hierarchy
- `src/attachment.rs` — EWS attachment support
- `src/room.rs` — Room/resource booking
- `src/permission.rs` — Calendar permissions and delegate management

## Key Patterns
- **Server IDs**: HMAC-SHA256 of CalDAV resource href (URL-safe base64, no padding)
- **ChangeKeys**: SHA-256 of (server_id + etag + updated_at), first 12 bytes hex-encoded
- **Etag flow**: CalDAV etags stored in `item_map`, used for If-Match on updates; 412 retry logic strips If-Match on second attempt; PUT without ETag fetches real etag via GET
- **ConflictResolution**: `AlwaysOverwrite` skips If-Match per MS-OXWSCORE; extracted from XML attribute (primary) or child element (fallback) to handle OneCalendar
- **Sync state**: Base64-encoded (since_seq, upper_bound_seq) cursor for SyncFolderItems
- **Change journal**: SQLite table tracking all upsert/delete operations for incremental sync
- **Missing journal items**: Items in journal but absent from CalDAV are treated as deletes (tombstone + cleanup)
- **Auth fail-open**: When Stalwart is unreachable, previously-authenticated users are allowed through; cache is NOT poisoned with `false` on connection errors
- **Connection retry**: CalDAV operations (get_event, query_events, find_user_calendars) retry once after 500ms backoff on connection errors
- **URL encoding**: Usernames in CalDAV URLs are percent-encoded (e.g., `@` → `%40`)
- **Connection pooling**: reqwest Client configured with pool_max_idle_per_host=4, pool_idle_timeout=90s, tcp_keepalive=30s

## Environment Variables
- `GATEWAY_LOG_LEVEL` — Log level (supports leading-dash stripping for CLI-style values)
- `GATEWAY_LOG_FORMAT` — pretty/compact/json (supports leading-dash stripping)
- `GATEWAY_LOG_NO_TIMESTAMPS` — Set "1" or "true" to disable timestamps
- `GATEWAY_LOG_THREADS` — Set "1" or "true" to enable thread info (strips leading dashes)
- `GATEWAY_LOG_TARGET` — Set "1" or "true" to enable module targets (strips leading dashes)
- `GATEWAY_CALDAV_BASE` — CalDAV base URL (must end with /dav)
- `GATEWAY_HMAC_SECRET` — HMAC secret for server ID generation (min 32 chars)
- `GATEWAY_HOST` — External hostname for autodiscover responses