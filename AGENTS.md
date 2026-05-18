# Exchange Gateway — Agent Memory

## Architecture
- Rust project (edition 2021) — EWS/ActiveSync gateway to Stalwart Mailserver CalDAV
- Key files: `src/ews.rs` (EWS protocol), `src/caldav.rs` (CalDAV client), `src/ews_update.rs` (UpdateItem field parsing), `src/main.rs` (HTTP server + health check), `src/logging.rs` (log config)

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
- `cargo test` — 111 tests (78 unit + 22 protocol fixture + 11 integration)
- `cargo clippy --all-targets -- -D warnings` — zero warnings required
- `cargo build --release` — release build