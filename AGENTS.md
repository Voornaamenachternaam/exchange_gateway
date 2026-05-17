# Exchange Gateway — Agent Memory

## Architecture
- Rust project (edition 2021) — EWS/ActiveSync gateway to Stalwart Mailserver CalDAV
- Key files: `src/ews.rs` (EWS protocol), `src/caldav.rs` (CalDAV client), `src/ews_update.rs` (UpdateItem field parsing), `src/main.rs` (HTTP server + health check), `src/logging.rs` (log config)

## Stalwart v0.16.5 Quirks
- **ETag on GET**: Stalwart v0.16.5 does NOT return ETag in GET response headers. Must use PROPFIND to obtain etags.
- **ETag in PROPFIND/REPORT**: Stalwart always includes `<D:getetag>` in multistatus responses (Depth:0 PROPFIND and calendar-query REPORT).
- **412 on synthetic etag**: If-Match with a synthetic etag causes 412 Precondition Failed. Always use PROPFIND-obtained etags for If-Match headers.
- **Auth required on ALL /dav/ paths**: Unauthenticated requests to any /dav/ path produce "Missing Authorization header" auth failure logs. Even health checks must include Basic auth (dummy credentials are fine).

## Key Patterns
- **put_event etag flow**: get_event → (ics, Option<etag>) → if None, PROPFIND etag fallback → if still None, pass None (not synthetic) → put_event with 3-tier 412 retry
- **Synthetic etag prefix**: All synthetic etags use `CaldavClient::SYNTHETIC_ETAG_PREFIX` ("sgw-"). Filter: `is_synthetic_etag()` checks for "sgw-" prefix or "W/" (weak etag). Never use `e.len() < 64` — legitimate server etags can be 64+ chars.
- **CaldavClient reuse**: Create CaldavClient outside loops. CaldavClient::new allocates a reqwest::Client (connection pool) — creating it per iteration causes socket exhaustion.
- **SyncFolderItems journal**: Journal items not in current_map are NOT skipped. They go through: DB lookup → CalDAV fetch → emit Create/Update/Delete
- **IndexedFieldURI format**: Parsed as "FieldURI:FieldIndex" (e.g. "contacts:EmailAddress:EmailAddress1"). ExtendedFieldURI as "extended:PropertyTag" or "extended:DistinguishedPropertySetId:PropertyId"
- **Storage error handling**: All `state.storage` calls use `if let Err(e) = ... { tracing::warn!(...) }` — never `let _ =`
- **Health check fail-fast**: OPTIONS to CalDAV base URL with dummy auth. Reachable statuses (2xx, 401, 403, 404, 405) → healthy. Any 5xx → fail immediately, no GET fallback. A GET fallback would (1) double latency on failure, (2) risk masking a genuinely unhealthy server if GET returns 2xx after OPTIONS 5xx.

## Build & Test
- `cargo test` — 83 tests (50 unit + 22 protocol fixture + 11 integration)
- `cargo clippy --all-targets -- -D warnings` — zero warnings required
- `cargo build --release` — release build