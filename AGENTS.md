# Exchange Gateway — Agent Memory

## Architecture
- Rust project (edition 2021) — EWS/ActiveSync gateway to Stalwart Mailserver CalDAV
- Key files: `src/ews.rs` (EWS protocol), `src/caldav.rs` (CalDAV client), `src/ews_update.rs` (UpdateItem field parsing), `src/eas.rs` (ActiveSync/EAS protocol), `src/main.rs` (HTTP server + health check), `src/logging.rs` (log config)

## Stalwart v0.16.5 Quirks
- **ETag on GET**: Stalwart v0.16.5 does NOT return ETag in GET response headers. Must use PROPFIND to obtain etags.
- **ETag in PROPFIND/REPORT**: Stalwart always includes `<D:getetag>` in multistatus responses (Depth:0 PROPFIND and calendar-query REPORT). ETags are returned double-quoted: `"1419368738"`.
- **412 on unquoted If-Match**: Per RFC 7232 §2.3, `If-Match` requires double-quoted entity-tags (`If-Match: "etag"` not `If-Match: etag`). Stalwart rejects unquoted etags with 412 Precondition Failed. Use `CaldavClient::format_etag_for_if_match()` to wrap etags in DQUOTE before sending.
- **412 on synthetic/weak etag**: If-Match with a synthetic etag (prefix "sgw-") causes 412. Per RFC 7232 §3.1, If-Match uses the strong comparison function — weak etags (W/...) are semantically invalid in If-Match for state-changing methods and cause 412/400 on strict servers. Always filter out both synthetic and weak etags via `is_synthetic_etag()` before sending If-Match.
- **ETag normalization**: `normalize_etag_to_internal()` strips W/ prefix, trims surrounding DQUOTE from the opaque-tag, then re-attaches W/ if weak. All etag parse sites (PROPFIND XML, HTTP ETag headers) must use this instead of bare `trim_matches('"')` which mangles weak etags like `W/"123"` → `W/"123` (only trailing quote removed).
- **Auth required on ALL /dav/ paths**: Unauthenticated requests to any /dav/ path produce "Missing Authorization header" auth failure logs. Even health checks must include Basic auth (dummy credentials are fine).

## Key Patterns
- **put_event etag flow**: get_event → (ics, Option<etag>) → if None, PROPFIND etag fallback → if still None, pass None (not synthetic) → put_event with 3-tier 412 retry
- **Synthetic etag prefix**: All synthetic etags use `CaldavClient::SYNTHETIC_ETAG_PREFIX` ("sgw-"). Filter: `is_synthetic_etag()` checks for "sgw-" prefix or "W/" (weak etag). Never use `e.len() < 64` — legitimate server etags can be 64+ chars. Both synthetic AND weak etags are filtered from If-Match headers per RFC 7232 §3.1 (strong comparison required).
- **CaldavClient reuse**: Create CaldavClient outside loops. CaldavClient::new allocates a reqwest::Client (connection pool) — creating it per iteration causes socket exhaustion.
- **SyncFolderItems journal**: Journal items not in current_map are NOT skipped. They go through: DB lookup → CalDAV fetch → emit Create/Update/Delete
- **IndexedFieldURI format**: Parsed as "FieldURI:FieldIndex" (e.g. "contacts:EmailAddress:EmailAddress1"). ExtendedFieldURI as "extended:PropertyTag" or "extended:DistinguishedPropertySetId:PropertyId"
- **Storage error handling**: All `state.storage` calls use `if let Err(e) = ... { tracing::warn!(...) }` — never `let _ =`
- **Health check fail-fast**: OPTIONS to CalDAV base URL with dummy auth. Reachable statuses (2xx, 401, 403, 404, 405) → healthy. Any 5xx → fail immediately, no GET fallback. A GET fallback would (1) double latency on failure, (2) risk masking a genuinely unhealthy server if GET returns 2xx after OPTIONS 5xx.
- **ChangeKey = sha256(server_id + etag)**: Per MS-OXWSCORE §2.2.4.25, ChangeKey identifies a specific content version. Never include `updated_at` in the hash — it's a DB admin timestamp that changes on every upsert_item_map, making ChangeKey unstable and causing ErrorIrresolvableConflict. The etag alone captures content version from CalDAV.
- **ConflictResolution on UpdateItem**: Per MS-OXWSCORE §3.1.4.9.4.1, ChangeKey validation is only enforced for `NeverOverwrite`. `AlwaysOverwrite` and `AutoResolve` skip ChangeKey validation and proceed with the update. OneCalendar always sends `ConflictResolution="AlwaysOverwrite"`. DeleteItem has no ConflictResolution — always validates ChangeKey.

## Build & Test
- `cargo test` — 138 tests (105 unit + 22 protocol fixture + 11 integration)
- `cargo clippy --all-targets -- -D warnings` — zero warnings required
- `cargo build --release` — release build

## Autodiscover Protocol Dispatch
- **AcceptableResponseSchema handling**: Per MS-ASCMD §2.2.3.1, the `<AcceptableResponseSchema>` element in the POST body specifies which response format the client expects. The server MUST return a matching schema or the client treats it as an error (MS-ASCMD §4.2.5, error code 601).
- **detect_response_schema()**: Scans for the local name "AcceptableResponseSchema" in the XML body, then walks backward to verify it's inside an opening tag (not a closing tag or bare string). Handles namespace prefixes (e.g. `<a:AcceptableResponseSchema>`) and attributes (e.g. `<AcceptableResponseSchema xmlns="...">`). Builds the matching close tag with the same prefix.
- **Outlook desktop**: requests `outlook/responseschema/2006a` → EXCH/EXPR Protocol response with EWS and ActiveSync URLs.
- **ActiveSync mobile**: requests `mobilesync/responseschema/2006` → Action/Settings/Server response with ActiveSync URL. This includes the AutoDetect cloud service used by Outlook for iOS/Android.
- **Autodiscover V2 JSON GET**: `/autodiscover/autodiscover.json?Protocol=ActiveSync` and `/autodiscover/autodiscover.json/v1.0/{email}?Protocol=ActiveSync` — returns JSON with protocol URLs.
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