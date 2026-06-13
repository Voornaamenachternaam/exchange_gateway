# Exchange Gateway — Agent Memory

## Architecture
- Rust project (edition 2024) — EWS/ActiveSync gateway to Stalwart Mailserver JMAP + CalDAV
- Key files: `src/ews.rs` (EWS protocol), `src/caldav.rs` (CalDAV client, fallback), `src/jmap.rs` (JMAP client — email + calendar), `src/smtp.rs` (SMTP client), `src/email.rs` (email domain logic), `src/ews_update.rs` (UpdateItem field parsing), `src/eas.rs` (ActiveSync/EAS protocol), `src/main.rs` (HTTP server + health check), `src/logging.rs` (log config)
- **JMAP Calendar (urn:ietf:params:jmap:calendars)**: Stalwart v0.16.6+ supports `draft-ietf-jmap-calendars-26` (RFC 8984 precursor). The gateway uses JMAP Calendar as the primary calendar backend when `urn:ietf:params:jmap:calendars` appears in the JMAP session capabilities. CalDAV is the fallback.
- **JMAP Calendar operation mapping**: `find_user_calendars` → `Calendar/get`, `query_events` → `CalendarEvent/query` + `CalendarEvent/get`, `get_event` → `CalendarEvent/get` (with `iCalendar` property), `put_event` → `CalendarEvent/set` (with `iCalendar` property), `delete_event` → `CalendarEvent/set` (destroy), `get_freebusy` → `Principal/getAvailability`, `verify_credentials` → JMAP session fetch
- **JMAP Calendar eliminates ETag complexity**: JMAP uses state-based change tracking instead of HTTP ETags. No more If-Match/412/SYNTHETIC_ETAG_PREFIX when using JMAP Calendar.
- **JMAP auto-derive**: When `GATEWAY_JMAP_BASE` is not explicitly set, it is auto-derived from `GATEWAY_CALDAV_BASE` by replacing `/dav` with `/jmap`.

## Dependency Decisions (May 2026)
- **jmap-client v0.4.1 (Stalwart Labs)**: REJECTED — No Calendar support, reqwest 0.13 conflicts with our 0.12, single-user Client model incompatible with multi-tenant gateway
- **jmap_proto v0.16.7 (Stalwart internal)**: REJECTED — NOT on crates.io (workspace-internal), AGPL-3.0-only license (viral), 4 path dependencies (utils/types/trc/registry — also not on crates.io), server-side architecture (we need client-side), reqwest 0.13 conflict. Its sub-crate `calcard` (0.3.4, Apache-2.0/MIT) and `jmap-tools` (0.1.5, Apache-2.0/MIT) ARE on crates.io but don't add enough value vs our existing `icalendar` crate
- **Custom JmapClient retained**: Our `src/jmap.rs` (1802 LOC) is the correct approach — full control, zero version conflicts, minimal binary size, multi-tenant compatible

## Stalwart v0.16.6 JMAP Calendar Support
- **Capability URN**: `urn:ietf:params:jmap:calendars` — check via `JmapClient::supports_calendar()`
- **Calendar/get**: Returns list of Calendar objects (id, name, color, timeZone, etc.)
- **CalendarEvent/get with iCalendar property**: Stalwart returns the raw ICS data in the `iCalendar` property when requested. This is critical for EWS/EAS rendering which needs iCalendar format.
- **CalendarEvent/set with iCalendar property**: Stalwart accepts raw ICS data in the `iCalendar` property for create/update operations. This eliminates the need for CalendarEvent/parse + blob upload.
- **CalendarEvent/query**: Filter by time range (`after`/`before`), calendar (`inCalendarIds`). Supports sorting and pagination.
- **CalendarEvent/set (destroy)**: Destroy events by ID. No ETag/If-Match needed — JMAP uses state-based concurrency.
- **Principal/getAvailability**: Replaces CalDAV free-busy-query. Capability URN: `urn:ietf:params:jmap:principals:availability`

## Stalwart v0.16.x Quirks
- **JMAP Calendar datetime format**: Stalwart's JMAP CalendarEvent/query filter uses `DateTime::parse_rfc3339()` internally, which requires RFC 3339 **extended** format (`2026-05-28T03:52:04Z`), NOT the basic ISO 8601 format (`20260528T035204Z`) used by iCalendar/CalDAV. Sending basic format causes the entire JMAP request to be rejected as 400 `notRequest`. All JMAP Calendar `after`/`before` filter values must use `%Y-%m-%dT%H:%M:%SZ`. CalDAV paths continue using `%Y%m%dT%H%M%SZ`.
- **ETag on GET**: Stalwart v0.16.5 does NOT return ETag in GET response headers. Must use PROPFIND to obtain etags.
- **ETag in PROPFIND/REPORT**: Stalwart always includes `<D:getetag>` in multistatus responses (Depth:0 PROPFIND and calendar-query REPORT). ETags are returned double-quoted: `"1419368738"`.
- **412 on unquoted If-Match**: Per RFC 7232 §2.3, `If-Match` requires double-quoted entity-tags (`If-Match: "etag"` not `If-Match: etag`). Stalwart rejects unquoted etags with 412 Precondition Failed. Use `CaldavClient::format_etag_for_if_match()` to wrap etags in DQUOTE before sending.
- **412 on synthetic/weak etag**: If-Match with a synthetic etag (prefix "sgw-") causes 412. Per RFC 7232 §3.1, If-Match uses the strong comparison function — weak etags (W/...) are semantically invalid in If-Match for state-changing methods and cause 412/400 on strict servers. Always filter out both synthetic and weak etags via `is_synthetic_etag()` before sending If-Match.
- **ETag normalization**: `normalize_etag_to_internal()` strips W/ prefix, trims surrounding DQUOTE from the opaque-tag, then re-attaches W/ if weak. All etag parse sites (PROPFIND XML, HTTP ETag headers) must use this instead of bare `trim_matches('"')` which mangles weak etags like `W/"123"` → `W/"123` (only trailing quote removed).
- **Auth required on ALL /dav/ paths**: Unauthenticated requests to any /dav/ path produce "Missing Authorization header" auth failure logs. Even health checks must include Basic auth (dummy credentials are fine).
- **CalDAV REPORT calendar-data parsing**: Stalwart may return `<C:calendar-data>` content using CDATA sections (`<![CDATA[...]]>`) or as multi-line XML text. The XML parser must: (1) set `trim_text(false)` to preserve ICS whitespace, (2) accumulate both `Event::Text` and `Event::CData` events inside a `in_caldata` flag, (3) flush the accumulated buffer on `Event::End(calendar-data)`. Single `Event::Text` reads miss CDATA content and multi-line ICS data, causing all events to fail `parse_ics_event()`. This pattern applies to ALL calendar-data parse sites: `sync.rs` EAS Sync, `ews.rs` GetUserAvailability, `ews.rs` load_current_calendar_items, `eas.rs` merged_freebusy_for_mailbox, `eas.rs` load_calendar_events.

## SQLite Configuration
- **WAL mode**: `PRAGMA journal_mode = WAL` in `sqlite_schema.sql` — persistent across connections, set on first init. Enables concurrent reads while a write is in progress (critical for Sync operations that read during multi-step CalDAV fetches).
- **busy_timeout**: `PRAGMA busy_timeout = 5000` — waits up to 5 seconds instead of failing immediately with SQLITE_BUSY. Prevents transient lock contention errors during concurrent sync.
- **foreign_keys**: `PRAGMA foreign_keys = ON` in both `sqlite_schema.sql` and the Rust connection setup. Belt-and-suspenders: the schema SQL ensures ad-hoc sqlite3 CLI sessions also respect FK constraints.

## Key Patterns
- **Auth verification priority**: JMAP first, CalDAV fallback. `AuthVerifier::verify()` tries JMAP session fetch first (when `GATEWAY_JMAP_BASE` is set), then falls back to CalDAV PROPFIND. This unifies auth for email + calendar via a single HTTP endpoint.
- **Health check priority**: JMAP first, CalDAV fallback. `health_check()` tries JMAP `/session` endpoint first (when `GATEWAY_JMAP_BASE` is set), then falls back to CalDAV OPTIONS. If JMAP fails but CalDAV succeeds, returns "degraded" mode.
- **JMAP Calendar operation map**: `Calendar/get` → find calendars, `CalendarEvent/query + /get` → query events, `CalendarEvent/get (iCalendar)` → get event ICS, `CalendarEvent/set (iCalendar)` → create/update event, `CalendarEvent/set (destroy)` → delete event, `Principal/getAvailability` → free-busy
- **JMAP Calendar freebusy dispatch**: EWS `merged_freebusy_for_mailbox()` and EAS `merged_freebusy_for_mailbox()` try JMAP Calendar first via `fetch_freebusy_jmap()` / `fetch_freebusy_jmap_eas()`. If JMAP Calendar is unavailable or the query fails, falls back to CalDAV. This pattern (JMAP-primary, CalDAV-fallback) should be applied to all remaining calendar handlers in a future refactoring.
- **JMAP session caching**: `JmapClient::get_session()` caches sessions per-username in a `DashMap` with a 5-minute TTL (`SESSION_CACHE_TTL`). Every JMAP method previously made an HTTP GET to `/session` before each API call. The cache eliminates redundant round-trips within the TTL window, critical for sync operations that call multiple JMAP methods sequentially.
- **JMAP session apiUrl override**: After fetching the session, `get_session()` overrides `session.api_url` with `self.base_url`. Stalwart returns the external URL (e.g. `https://stalwart.example.com/jmap/`) as `apiUrl`, but the gateway must use the internal Docker network URL (e.g. `http://stalwart:8080/jmap`) for API calls. Without this override, JMAP POST requests route through the reverse proxy and fail with 400 `notRequest`.
- **JMAP methodCalls must be arrays, not objects (RFC 8621 §3.2)**: Each method invocation in `methodCalls` must be serialized as a 3-element array `["methodName", {arguments}, "id"]`, NOT as an object `{"name":"...", "arguments":{...}, "id":"..."}`. Stalwart rejects object-form with 400 `notRequest: "invalid type: map, expected an array with 3 elements"`. The `JmapMethodCall` struct implements custom `Serialize` that outputs the tuple format `(&self.name, &self.arguments, &self.id).serialize(serializer)`. This affects ALL JMAP API calls (email, calendar, contacts).
- **put_event etag flow (CalDAV fallback)**: get_event → (ics, Option<etag>) → if None, PROPFIND etag fallback → if still None, pass None (not synthetic) → put_event with 3-tier 412 retry
- **Synthetic etag prefix**: All synthetic etags use `CaldavClient::SYNTHETIC_ETAG_PREFIX` ("sgw-"). Filter: `is_synthetic_etag()` checks for "sgw-" prefix or "W/" (weak etag). Never use `e.len() < 64` — legitimate server etags can be 64+ chars. Both synthetic AND weak etags are filtered from If-Match headers per RFC 7232 §3.1 (strong comparison required).
- **CaldavClient reuse**: Create CaldavClient outside loops. CaldavClient::new allocates a reqwest::Client (connection pool) — creating it per iteration causes socket exhaustion.
- **SyncFolderItems journal**: Journal items not in current_map are NOT skipped. They go through: DB lookup → CalDAV fetch → emit Create/Update/Delete
- **IndexedFieldURI format**: Parsed as "FieldURI:FieldIndex" (e.g. "contacts:EmailAddress:EmailAddress1"). ExtendedFieldURI as "extended:PropertyTag" or "extended:DistinguishedPropertySetId:PropertyId"
- **Storage error handling**: All `state.storage` calls use `if let Err(e) = ... { tracing::warn!(...) }` — never `let _ =`
- **Health check fail-fast**: OPTIONS to CalDAV base URL with dummy auth. Reachable statuses (2xx, 401, 403, 404, 405) → healthy. Any 5xx → fail immediately, no GET fallback. A GET fallback would (1) double latency on failure, (2) risk masking a genuinely unhealthy server if GET returns 2xx after OPTIONS 5xx.
- **ChangeKey = sha256(server_id + etag)**: Per MS-OXWSCORE §2.2.4.25, ChangeKey identifies a specific content version. Never include `updated_at` in the hash — it's a DB admin timestamp that changes on every upsert_item_map, making ChangeKey unstable and causing ErrorIrresolvableConflict. The etag alone captures content version from CalDAV. **For email items**, ChangeKey equals the `em-` prefixed server_id (no separate hash), since OneCalendar uses `ConflictResolution="AlwaysOverwrite"` which skips ChangeKey validation.
- **ConflictResolution on UpdateItem**: Per MS-OXWSCORE §3.1.4.9.4.1, ChangeKey validation is only enforced for `NeverOverwrite`. `AlwaysOverwrite` and `AutoResolve` skip ChangeKey validation and proceed with the update. OneCalendar always sends `ConflictResolution="AlwaysOverwrite"`. DeleteItem has no ConflictResolution — always validates ChangeKey.

## Build & Test
- `cargo test` — 205+ tests (171+ unit + 22 protocol fixture + 11 integration + 1 doc)
- `cargo clippy --all-targets -- -D warnings` — zero warnings required
- `cargo build --release` — release build
- **Never set RUST_LOG in Dockerfile**: `build_env_filter()` gives GATEWAY_LOG_LEVEL priority over RUST_LOG. The Dockerfile must NOT set RUST_LOG; logging.rs defaults to "info" when neither env var is set.
- **build_env_filter() preserves complex directives**: `GATEWAY_LOG_LEVEL=trace,axum=info` passes the full directive string to `EnvFilter::try_new()`, preserving per-module overrides. The `parse_global_level()` helper extracts the first comma-separated segment (the ambient level) for auto-enable features like module targets — it does NOT replace the EnvFilter.
- **Error handling in build_env_filter()**: Uses `match` on `EnvFilter::try_new()` result — errors are logged via `eprintln!` before returning `Err(String)`, never `.unwrap_or_default()`. Per repository pattern: all errors must be logged before being handled.

- **No hardcoded example.com in production code**: `active_user_emails(username, mail_domain)` takes the mail domain as a parameter instead of hardcoding "example.com". The domain comes from `state.cfg.mail_domain` (set via `GATEWAY_MAIL_DOMAIN` env var). Test code may still use "example.com" per RFC 2606.
- **Username domain canonicalization**: `canonicalize_username(username, mail_domain)` in `util.rs` replaces the domain in the username with `GATEWAY_MAIL_DOMAIN`, extracting only the local part. Applied at EAS and EWS entry points (after `parse_basic_auth`, before `verify`). Ensures: (1) consistent DB owner keys regardless of what domain the client supplies, (2) CalDAV URL `/cal/{canonical}/` matches Stalwart's home set, (3) `active_user_emails()` reports the correct primary SMTP. Example: `contact@exchange.com` → `contact@example.com`. The gateway logs when canonicalization changes the domain.
- **active_user_emails always uses mail_domain**: Extracts only the local part from `username` and constructs the primary SMTP with `GATEWAY_MAIL_DOMAIN`. Defense-in-depth: even if a non-canonical username leaks through, the Settings response always reports the correct domain.

## Autodiscover Protocol Dispatch
- **AcceptableResponseSchema handling**: Per MS-ASCMD §2.2.3.1, the `<AcceptableResponseSchema>` element in the POST body specifies which response format the client expects. The server MUST return a matching schema or the client treats it as an error (MS-ASCMD §4.2.5, error code 601).
- **detect_response_schema()**: Scans for the local name "AcceptableResponseSchema" in the XML body, then walks backward to verify it's inside an opening tag (not a closing tag or bare string). Handles namespace prefixes (e.g. `<a:AcceptableResponseSchema>`) and attributes (e.g. `<AcceptableResponseSchema xmlns="...">`). Builds the matching close tag with the same prefix.
- **Outlook desktop**: requests `outlook/responseschema/2006a` → EXCH/EXPR Protocol response with EWS and ActiveSync URLs.
- **ActiveSync mobile**: requests `mobilesync/responseschema/2006` → Action/Settings/Server response with ActiveSync URL. This includes the AutoDetect cloud service used by Outlook for iOS/Android.
- **Autodiscover V2 JSON GET**: `/autodiscover/autodiscover.json?Protocol=ActiveSync` and `/autodiscover/autodiscover.json/v1.0/{email}?Protocol=ActiveSync` — returns minimal JSON with only `Protocol` and `Url`. Per MS-OXDSCLI, each V2 response contains exactly one URL for the requested protocol — no extra fields. Valid protocol names: `ActiveSync`, `Ews`, `AutodiscoverV1`, `Rest`. "Exchange" is NOT a valid V2 protocol name (it's V1 XML only). Default (no Protocol) returns ActiveSync. The old gateway code erroneously included V1-XML-era fields (`ActiveSyncUrl`, `MobileSyncUrl`, `EwsUrl`, `ExternalEwsUrl`, `InternalEwsUrl`, `ExternalEwsVersion`, `EwsSupportedSchemas`) in V2 JSON responses; these were removed because they violate the V2 design intent and can confuse strict clients like AutoDetect.
- **Culture from Accept-Language**: The `<Culture>` element in mobilesync responses is derived from the client's Accept-Language header (RFC 7231 §5.3.5) via `parse_culture_from_accept_language()`. Format: `{language}:{country}`. Falls back to "en:us" per MS-ASCMD §4.2.4 example.
- **MobileSync Protocol block removed from Outlook response**: The `<Protocol><Type>MobileSync</Type>...</Protocol>` block is non-standard per MS-OXDSCLI §2.2.4 and was causing schema mismatch errors. MobileSync clients must use the dedicated mobilesync XML response.
- **ServerExclusiveConnect=on for EXPR**: Per MS-OXDSCLI §3.1.5.4, setting this to "on" causes Outlook to prefer the EXPR (external) configuration.
- **Case-insensitive autodiscover paths**: Both `/autodiscover/...` and `/Autodiscover/...` are registered per MS-OXDISCO §2.2.3.
- **GET support on autodiscover.xml**: Per MS-OXDISCO §3.1.5.4, the client may send a GET request to verify the autodiscover endpoint exists before sending the POST. The GET handler extracts email from query parameters.
- **Single-segment {email} in JSON V2 path**: `/autodiscover/autodiscover.json/v1.0/{email}` uses `{email}` (not `{*email}`) because email addresses never contain '/', and a wildcard would capture trailing path garbage.

## ActiveSync AutoDetect Compatibility
- **AutoDetect cloud service**: Outlook for iOS/Android uses Microsoft's AutoDetect cloud service (`prod-autodetect.outlookmobile.com`) to discover the ActiveSync endpoint. The flow is: (1) AutoDetect queries Autodiscover V2 JSON for the ActiveSync URL, (2) AutoDetect probes the ActiveSync endpoint with an empty Bearer challenge (`Authorization: Bearer`), (3) The 401 response MUST include `WWW-Authenticate: Bearer` with `authorization_uri` for AutoDetect to recognise the server as a valid ActiveSync endpoint.
- **Bearer header format (per MS-XOAUTH §4.1)**: The EAS 401 response MUST include `WWW-Authenticate: Bearer` with three parameters: `client_id`, `trusted_issuers`, and `authorization_uri`. Without `authorization_uri`, AutoDetect reports "missing authorization URL" and falls back to IMAP, making the calendar unusable. The complete header is:
  ```
  WWW-Authenticate: Bearer client_id="00000002-0000-0ff1-ce00-000000000000", trusted_issuers="00000001-0001-0000-c000-000000000000@*", authorization_uri="https://login.microsoftonline.com/common/oauth2/authorize"
  WWW-Authenticate: Basic realm="Microsoft-Server-ActiveSync"
  ```
- **BEARER_WWW_AUTHENTICATE**: Compile-time constant using `HeaderValue::from_static(concat!(...))` — zero per-request allocation. The three embedded values are exposed as `#[cfg(test)]` constants for test assertions:
  - `EXCHANGE_ACTIVESYNC_CLIENT_ID`: `00000002-0000-0ff1-ce00-000000000000` — the well-known Exchange ActiveSync application ID in Microsoft Entra ID.
  - `TRUSTED_ISSUERS`: `00000001-0001-0000-c000-000000000000@*` — the well-known Microsoft STS issuer GUID with wildcard tenant.
  - `AUTHORIZATION_URI`: `https://login.microsoftonline.com/common/oauth2/authorize` — the common Microsoft Entra ID OAuth 2.0 authorization endpoint.
- **Gateway auth model**: The gateway only supports Basic authentication. The Bearer header is included solely for AutoDetect discovery compatibility. When a client attempts Bearer auth, `parse_basic_auth()` rejects it and the client falls back to Basic.
- **V1 XML autodiscover vs V2 JSON**: The V1 XML autodiscover fix (dispatching by AcceptableResponseSchema) helps direct ActiveSync clients but does NOT help Outlook mobile's AutoDetect flow, which uses the V2 JSON endpoint exclusively.

## Outlook Mobile Architecture Limitation (CRITICAL)
- **Outlook for iOS/Android app is cloud-backed**: ALL data flows through Microsoft's cloud middle tier (Exchange Online). The Outlook app does NOT connect directly to on-premises ActiveSync endpoints, even when using "Basic authentication" mode. Per Microsoft docs: "Outlook for iOS and Android is a cloud-backed application. This characteristic indicates that your experience consists of a locally installed app powered by a secure and scalable service running in the Microsoft Cloud."
- **Basic auth mode still requires cloud**: Even in "Basic authentication" mode (non-hybrid), "The EAS connection between Exchange Online and the on-premises environment enables synchronization of the users' on-premises data." Exchange Online creates a "shard mailbox" to cache on-prem data.
- **No M365 tenant = no ActiveSync via Outlook app**: Without an M365/O365 tenant, AutoDetect falls back to IMAP. Selecting "Exchange" account type manually in the Outlook app also fails because the app still routes through the cloud.
- **Android native Exchange account works directly**: The built-in Android Exchange account (Settings → Accounts → Exchange) is a direct EAS client that connects to the server WITHOUT the cloud middle tier. This is how grommunio achieves Android EAS calendar sync — they use the native Android Exchange account, NOT the Outlook app. The exchange_gateway's EAS implementation is compatible with this path.
- **grommunio comparison**: grommunio's documentation shows "Settings → Accounts → Exchange" (native Android), not the Outlook app. grommunio-sync README explicitly says "While Microsoft Outlook supports EAS, it is not recommended to use grommunio Sync due to a very small subset of features only supported" — confirming the Outlook app doesn't work well with non-Exchange EAS servers. grommunio recommends MAPI/HTTP for Outlook desktop.
- **This is NOT a gateway code bug for native EAS clients**: The EAS protocol implementation is correct (OPTIONS, Bearer challenge, Provision, FolderSync, Sync, Settings all work). The limitation applies only to the Outlook app, not to direct EAS clients.
- **Why Outlook Windows works**: New Outlook for Windows (and Classic Outlook) connect DIRECTLY to on-premises servers via EWS/ActiveSync without requiring the cloud middle tier.
- **Android calendar via gateway**: Use Android's native Exchange account (Settings → Accounts → Exchange) with the gateway's ActiveSync URL. Calendar syncs to native Android calendar. For email, use any IMAP client including Outlook for email-only.

## EAS Multi-Collection Sync (June 2026)
- **Problem**: Android clients (including Gmail's Exchange account) send multi-collection Sync requests containing both Calendar and Email `<Collection>` elements in a single Sync command. Previously, the gateway only processed the first Collection, causing email sync to be silently ignored.
- **Solution**: `parse_sync_collections()` parses all `<Collection>` elements from the XML body. `handle_sync_collections()` processes each collection independently (Calendar via CalDAV/JMAP Calendar, Email via JMAP Email) and combines the responses into a single multi-collection `<Sync><Collections>...</Collections></Sync>` envelope.
- **`handle_email_sync` return type**: Changed from `Result<Response>` to `Result<String>`. Returns just the inner `<Collection>...</Collection>` XML fragment, allowing composition into multi-collection responses. The caller wraps it in the full Sync envelope.
- **Fallback**: If `parse_sync_collections()` returns empty (no nested `<Collection>` elements found), the gateway constructs a single-element list from the `EasRequest` fields — backward compatible with older single-collection clients.
- **Per MS-ASCMD §2.2.3.31.2**: The Sync command supports 1..N Collection elements. Each collection is processed independently with its own SyncKey, CollectionId, Class, WindowSize, FilterType, and GetChanges.

## Email Architecture (JMAP + SMTP)

### Overview
- The gateway now supports sending and receiving email alongside calendar functionality
- Email reading/sync uses **JMAP** (RFC 8621) via Stalwart's JMAP API
- Email sending prefers **JMAP EmailSubmission** (RFC 8621 §2.7) via Stalwart, with **SMTP** as fallback
- Calendar free-busy uses **JMAP Calendar** (draft-ietf-jmap-calendars-26) as primary path, with **CalDAV** as fallback
- Calendar CRUD (GetItem, SyncFolderItems, CreateItem, UpdateItem, DeleteItem) still uses **CalDAV** directly — JMAP Calendar API parity exists but handlers not yet wired
- When JMAP submission is available, Stalwart ports 465 (SMTPS) and 993 (IMAPS) are optional

### Key Files
- `src/jmap.rs` — JMAP client (session discovery, Email/query, Email/get, Email/set, Email/changes, Mailbox/query, EmailSubmission/set, Calendar/get, CalendarEvent/query+get+set+destroy, session caching)
- `src/smtp.rs` — SMTP client (lettre with tokio1-rustls-tls, implicit TLS on port 465, STARTTLS on port 587)
- `src/email.rs` — Email domain logic (EWS Message parsing, JMAP→EWS/EAS rendering, JMAP/SMTP sending, EAS SendMail parsing)

### Configuration
- `GATEWAY_JMAP_BASE` — JMAP API base URL (e.g., `https://stalwart.example.com/jmap`)
- `GATEWAY_SMTP_HOST` — SMTP server hostname (optional if JMAP submission is available; e.g., `stalwart`)
- `GATEWAY_SMTP_PORT` — SMTP port (default: 465 for SMTPS, 587 for STARTTLS)
- `GATEWAY_EMAIL_ENABLED` — Enable/disable email features (default: true)
- `GATEWAY_MAIL_HOST` — Mail server hostname for autodiscover IMAP/SMTP settings (only used when SMTP is configured)

### EWS Email Operations
- **FindItem** (email folders) → JMAP Email/query with mailbox role filter
- **GetItem** (Message class) → JMAP Email/get with properties
- **SyncFolderItems** (email folders) → JMAP Email/changes + Email/get
- **CreateItem** (MessageDisposition=SendOnly/SendAndSaveCopy) → JMAP EmailSubmission (or SMTP) + optional JMAP save
- **SendItem** → JMAP EmailSubmission (or SMTP)
- **UpdateItem** (IsRead, Importance) → JMAP Email/set (keywords: $seen, $important)
- **DeleteItem** (email items) → JMAP Email/set (destroy)
- **MoveItem** → JMAP Email/set (mailboxIds update) — currently returns success with pending JMAP mailbox mapping

### EAS Email Operations
- **Sync** (Email class) → JMAP Email/query for initial sync, Email/changes for subsequent
- **SendMail** → Parse MIME/XML + JMAP EmailSubmission (or SMTP fallback)
- **SmartReply/SmartForward** → JMAP EmailSubmission (or SMTP) with original message reference

### JMAP Client Details
- **Session discovery**: `/.well-known/jmap` → `fetchSession` URL → GET session object
- **Account ID**: Retrieved from session's `urn:ietf:params:jmap:mail` account
- **Email/query + Email/get**: Batched in a single JMAP request using RFC 8621 §3.6 back-references. Filters by `inMailboxRole` (inbox, sent, drafts, junk, trash). Also returns the Email data type `state` token for subsequent `Email/changes` calls, eliminating the need for a separate `get_email_state()` round-trip
- **Email/get**: Properties include `id`, `blobId`, `threadId`, `mailboxIds`, `keywords`, `from`, `to`, `cc`, `bcc`, `subject`, `receivedAt`, `hasAttachment`, `bodyValues`
- **Email/changes**: State-based change tracking for SyncFolderItems
- **Email/set**: Update keywords ($seen, $important), destroy emails
- **EmailSubmission/set** (RFC 8621 §2.7): Create email via Email/set, then submit via EmailSubmission/set with `emailId: "#e0"` back-reference. Uses capability `urn:ietf:params:jmap:submission`. Stalwart v0.16.5 fully supports this.
- **Mailbox/query**: List mailboxes by role, find "sent" mailbox for EmailSubmission

### SMTP Client Details (fallback when JMAP submission unavailable)
- Uses `lettre` 0.11.22 with `tokio1-rustls-tls`
- Port 465: Implicit TLS (`AsyncSmtpTransport::relay()`) — **default and preferred** for Stalwart
- Port 587: STARTTLS (`AsyncSmtpTransport::starttls_relay()`) — available if needed
- Credentials: Same username/password as CalDAV/JMAP auth
- MIME construction: MultiPart (text + HTML) or single-part (text only)
- **Message-ID extraction**: Before `transport.send()` takes ownership of the `Message`, the lettre-generated `Message-ID` header is extracted via `message.headers().get::<MessageId>()`. This returns the actual RFC 5322 Message-ID (e.g. `<1717012345.abc@host>`), enabling correlation with the copy in the Sent Items folder. Falls back to a synthetic timestamp-UUID ID only if lettre didn't generate one.
- **Optional**: When JMAP EmailSubmission is available, SMTP is not needed between gateway and Stalwart

### Email Server ID Generation
- `email_server_id_from_jmap_id(jmap_id)` — prefix-based reversible ID: `"em-{jmap_id}"` (for JMAP-sourced IDs)
- `email_server_id_from_send_result(id)` — normalizes both JMAP IDs and RFC 5322 Message-IDs into server IDs. Strips angle brackets from RFC 5322 Message-IDs before prefixing with `"em-"`. Used by CreateItem handler which receives IDs from both JMAP EmailSubmission and SMTP fallback paths.
- `jmap_id_from_email_server_id(server_id)` — strips `"em-"` prefix to recover JMAP ID
- `is_email_server_id(id)` — checks for `"em-"` prefix to distinguish from calendar HMAC IDs
- Email IDs use prefix encoding (not HMAC) because EWS GetItem/UpdateItem/DeleteItem receive the server ID from the client and must reverse it to query JMAP
- Calendar IDs continue to use HMAC-SHA256 (they're resolved via DB lookup, not reversal)

### JMAP→EWS Message Rendering
- `render_jmap_email_as_ews_message()` — Full EWS `<t:Message>` XML
- Maps JMAP keywords to EWS: `$seen` → IsRead=true, `$important` → Importance=High
- Includes: Subject, From, ToRecipients, CcRecipients, BccRecipients, DateTimeReceived, Body, etc.

### JMAP→EAS Application Data Rendering
- `render_jmap_email_as_eas_application_data()` — EAS Sync command format
- Maps to AirSync namespace with Email: and AirSyncBase: namespaces
- Includes: Subject, From, To, DateReceived, Importance, Read, HasAttachment, Body

### Port Elimination Architecture
- **Port 993 (IMAPS)**: NOT used by the gateway. JMAP (HTTP) handles all email reading. Port 993 only needed for standalone IMAP clients connecting directly to Stalwart.
- **Port 465 (SMTPS)**: Optional when JMAP EmailSubmission is available. The gateway's `send_email()` function prefers JMAP `EmailSubmission/set`, falling back to SMTP only if JMAP submission fails or is not configured.
- **Port 25 (SMTP MX)**: Always required for receiving inbound email from external MTAs. This is MTA-to-MTA traffic, unrelated to the gateway.
- **Port 8080 (HTTP)**: Used for ALL gateway-to-Stalwart communication (JMAP + CalDAV). When JMAP EmailSubmission is used, this single port handles calendar AND email read/write/send.
- **Autodiscover IMAP/SMTP blocks**: Only included in Outlook XML autodiscover response when `smtp_client.is_some()` (i.e., SMTP is explicitly configured). When JMAP-only, these blocks are omitted since Outlook uses EWS/ActiveSync directly.

### Email Submission Flow (JMAP)
1. `email::send_email()` checks for `jmap_client`
2. If JMAP available: calls `send_email_jmap()` → `JmapClient::submit_email()`
3. `submit_email()` sends batched JMAP request:
   - `Email/set` with `create: { e0: { mailboxIds, from, to, cc, bcc, subject, bodyValues } }`
   - `EmailSubmission/set` with `create: { s0: { emailId: "#e0", envelope: { mailFrom, rcptTo } } }`
4. Both methods use `urn:ietf:params:jmap:submission` capability
5. If JMAP fails, falls back to `send_email_smtp()` via lettre

### EAS Email Sync — CollectionId Mapping
- **CollectionId → JMAP mailbox role**: `eas_collection_id_to_mailbox_role()` maps EAS folder IDs to JMAP roles: "2" → inbox, "3" → drafts, "4" → sent, "5" → trash, "6" → None (outbox, no JMAP equivalent), "7" → junk. Unknown IDs return `None` (empty result), NOT "inbox" (privacy fix — prevents returning Inbox emails for unrecognised CollectionIds).
- **EAS folder Type values per MS-ASCMD §2.2.3.186.3**: Inbox=2, Drafts=3, DeletedItems=4, SentItems=5, Outbox=6, JunkEmail=12 (User-created Mail folder). **NOT** 7 (which is Tasks). Previously, SENT_ITEMS and DELETED_ITEMS were swapped (4/5 reversed), and JUNK_EMAIL was 7 (Tasks) instead of 12. This caused Outlook/ActiveSync clients to misinterpret folder types.
- **Outbox returns empty**: CollectionId "6" (Outbox) has no JMAP mailbox equivalent — outbound email is handled via `EmailSubmission/set`, not a mailbox. Both `eas_collection_id_to_mailbox_role()` returning `None` and `fetch_emails_jmap("outbox")` return empty results, preventing a privacy bug where "outbox" fell through to the catch-all filter (returning ALL emails in the account).
- **EAS Sync uses actual CollectionId**: The Sync response includes the real `collection_id` from the request, not hardcoded "2". Previously, syncing any non-inbox folder (Sent Items, Drafts, Junk) would incorrectly return Inbox emails under CollectionId "2".
- **Scoped vs raw CollectionId in handle_email_sync**: `handle_email_sync()` receives both `collection_id` (raw, e.g. "2") and `state_collection_id` (scoped, e.g. "2::deviceid"). The raw `collection_id` must be used for `eas_collection_id_to_mailbox_role()` lookups and XML `<CollectionId>` elements. The scoped `state_collection_id` is only for DB operations (`set_sync_key`, `get_sync_key`). Previously, the scoped form was used everywhere, causing `eas_collection_id_to_mailbox_role("2::deviceid")` to return `None` (no match) -- all email folder syncs returned empty.
- **JMAP API URL override**: `JmapClient::get_session()` overrides `session.api_url` with the configured `self.base_url` (internal Docker URL). Stalwart's session returns the external `apiUrl` (e.g. `https://stalwart.example.com/jmap/`) which routes through the reverse proxy, causing 400 `notRequest` errors. The override ensures all API calls stay within the Docker network (e.g. `http://stalwart:8080/jmap`).
- **EAS SendMail error status**: Per MS-ASCMD §2.2.1.17, SendMail returns Status 1 (success) or Status 4 (mailbox server error — transient). Previously, send failures returned Status 1, causing silent email loss where the client thought the email was sent.

### EWS Email — Pagination and Entity Unescaping
- **FindItem total_items**: Uses `result.total` from JMAP `calculateTotal:true` (total matching items across all pages), not `emails.len()` (items in the current page). Previously, `total_items = emails.len()` made `includes_last` always true, preventing the client from paginating beyond the first page.
- **XML entity unescaping in parse_ews_message**: All text extracted from EWS XML (Subject, Body, EmailAddress) is passed through `unescape_xml_text()` which uses `quick_xml::escape::unescape()` to resolve `&amp;`, `&lt;`, `&gt;`, `&quot;`, `&apos;`, and numeric character references. On unescape failure (malformed entity), returns the original text unchanged rather than dropping the email. Previously, raw XML entities were passed through to the email content, resulting in subjects like `Q&amp;A` instead of `Q&A`.

## JMAP Calendar — Verification against CalDAV (May 2026)

### Capability Verification (Stalwart v0.16.6)
- **JMAP Calendar capability**: `urn:ietf:params:jmap:calendars` in session → `JmapClient::supports_calendar()` checks for this
- **Calendar/get**: Returns calendar list (id, name, color, timeZone) — replaces CalDAV PROPFIND calendar discovery
- **CalendarEvent/query + CalendarEvent/get**: Time-range filtered query + full event data — replaces CalDAV REPORT calendar-query
- **CalendarEvent/get with iCalendar property**: Returns raw ICS data — critical for EWS/EAS which need iCalendar format
- **CalendarEvent/set with iCalendar property**: Creates/updates events from raw ICS — replaces CalDAV PUT with If-Match
- **CalendarEvent/set (destroy)**: Deletes events by ID — replaces CalDAV DELETE with If-Match
- **JMAP Calendar API parity**: ALL CalDAV operations the gateway uses have JMAP equivalents. JMAP CAN replace CalDAV.

### Current JMAP Calendar Integration Status
- ✅ **Free-busy (EWS GetUserAvailability)**: JMAP-primary via `fetch_freebusy_jmap()`, CalDAV fallback
- ✅ **Free-busy (EAS ResolveRecipients)**: JMAP-primary via `fetch_freebusy_jmap_eas()`, CalDAV fallback
- ✅ **Auth verification**: JMAP session fetch first, CalDAV fallback
- ✅ **Health check**: JMAP /session first, CalDAV OPTIONS fallback
- ✅ **Session caching**: DashMap-backed, 5-minute TTL, eliminates redundant HTTP GETs
- ⏳ **EWS GetItem (calendar)**: Still uses CaldavClient — JMAP CalendarEvent/get ready but not wired
- ⏳ **EWS SyncFolderItems (calendar)**: Still uses CaldavClient — JMAP CalendarEvent/changes ready but not wired
- ⏳ **EWS CreateItem (calendar)**: Still uses CaldavClient — JMAP CalendarEvent/set ready but not wired
- ⏳ **EWS UpdateItem (calendar)**: Still uses CaldavClient — JMAP CalendarEvent/set ready but not wired
- ⏳ **EWS DeleteItem (calendar)**: Still uses CaldavClient — JMAP CalendarEvent/set (destroy) ready but not wired
- ⏳ **EAS Sync (Calendar class)**: Still uses CaldavClient — needs JMAP CalendarEvent/query + /get

### JMAP Calendar Benefits over CalDAV
- **No ETag complexity**: JMAP uses state-based change tracking. No If-Match, no 412 Precondition Failed, no synthetic etags, no ETag normalization.
- **Single HTTP endpoint**: All operations via one POST to the JMAP API URL. No PROPFIND/REPORT/GET/PUT/DELETE method diversity.
- **Structured JSON responses**: No XML parsing, no CDATA accumulation, no trim_text(false) workarounds.
- **Batched requests**: Multiple operations in a single HTTP call using RFC 8621 §3.6 back-references.
- **Session caching**: Reduces HTTP round-trips from 2 (GET session + POST API) to 1 (POST API) per method call within TTL.

### Outlook Client Compatibility (May 2026)
- **New Outlook for Windows (20251205004.10)**: Connects directly to on-prem EWS. Gateway provides EWS ↔ JMAP/CalDAV translation. Works with JMAP Calendar for free-busy. Calendar CRUD still via CalDAV.
- **Outlook Android (5.2618.2)**: Cloud-backed via AutoDetect. Without M365 tenant, falls back to IMAP for email. Native Android Exchange account works for EAS calendar sync directly.
- **Android native Exchange account**: Direct EAS client, no cloud middle tier. Gateway provides EAS ↔ JMAP/CalDAV translation. Works with JMAP Calendar for free-busy.
