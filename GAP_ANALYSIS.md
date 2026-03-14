# exchange_gateway gap analysis and Binder1 traceability matrix (current state)

## Scope

- Protocol source of truth: `Binder1.txt`.
- Implementation assessed: Rust gateway (`src/*`), Worker API (`worker/index.js`), persistence schema (`d1_schema.sql`), runtime config model.
- Use-case target: Stalwart calendar interoperability for Outlook Windows + Outlook Android via EAS/EWS over Cloudflare.

---

## What is already implemented (current repository state)

1. **Deployment contract alignment**
   - File-based runtime config (`/etc/exchange-gateway/config.toml`) and bind `0.0.0.0:8134` architecture are in place.
2. **EAS baseline transport and sync**
   - EAS command dispatch, OPTIONS capabilities, per-device throttling, WBXML/XML handling, provision key persistence flow, and sync collection-id usage from request payload are implemented.
3. **EWS operation surface expanded**
   - `GetFolder`, `FindFolder`, `FindItem`, `GetItem`, and `SyncFolderItems` are implemented with operation-specific validation and operation-specific error response mapping.
4. **Typed state persistence**
   - Worker + D1 support sync/provision/EWS state and idempotency-aware write APIs; schema version tracking is present.

---

## Binder1 traceability matrix (MUST/SHOULD -> code + tests)

> Matrix is scoped to the protocol families that are relevant to the stated use-case.

| Binder1 protocol family | Requirement level | Requirement summary (implementation-facing) | Current implementation mapping | Test mapping | Status |
|---|---|---|---|---|---|
| MS-ASHTTP | MUST | Accept authenticated ActiveSync requests on `/Microsoft-Server-ActiveSync`; support protocol discovery (`OPTIONS`) and command dispatch semantics | `src/eas.rs` request auth + method handling + command detection + OPTIONS capability headers | `src/eas.rs` unit tests for command extraction/validation | Implemented (scope) |
| MS-ASCMD | MUST | Parse command envelope/query and route command-specific processing with valid status envelopes | `src/eas.rs` command dispatch table and command-specific responses | `src/eas.rs` command/query extraction tests | Implemented (scope) |
| MS-ASPROV | MUST | Provision policy key handshake semantics across phases | `src/eas.rs` provision branch + `src/storage.rs` policy get/set + worker provision endpoints | runtime-covered; no dedicated integration fixture yet | Implemented (scope) |
| MS-ASWBXML / MS-ASAIRS | MUST | Decode/encode WBXML payloads used by implemented command surfaces | `src/wbxml.rs` codec + `src/eas.rs` wbxml path selection | `src/wbxml.rs` roundtrip tests | Implemented (scope) |
| MS-ASCAL | MUST | Calendar synchronization surface for ActiveSync collection | `src/sync.rs` CalDAV-backed sync projection + EAS Sync response path | `src/eas.rs` sync parsing tests; no full E2E fixture in-repo | Implemented (scope) |
| EWS core message/type schemas (Binder1 EWS families) | MUST | Validate operation request shape before execution; return operation-specific error semantics | `src/ews.rs` `validate_schema` and `operation_error_response` | `src/ews.rs` schema/sync-state parser tests | Implemented (scope) |
| EWS FindItem behavior | SHOULD | Support paging and deterministic item identity/changekey in response payload | `src/ews.rs` `handle_find_item`; `src/storage.rs` EWS item listing API; worker `/api/list_ews_items` | currently unit-level only | Implemented (partial-depth) |
| EWS GetItem behavior | SHOULD | Return item payload by requested id and map not-found to operation-specific error | `src/ews.rs` `handle_get_item`; `src/storage.rs` `get_ews_item_by_server_id`; worker `/api/get_ews_item_by_id` | currently unit-level only | Implemented (partial-depth) |
| EWS SyncFolderItems behavior | MUST | Persist sync state and return incremental changes based on state token | `src/ews.rs` `handle_sync_folder_items`; `src/storage.rs` sync-state methods; worker EWS sync-state endpoints | currently unit-level only | Implemented (partial-depth) |
| Data consistency for retry paths | SHOULD | Retry-safe typed writes with idempotency signal propagation | `src/storage.rs` deterministic `Idempotency-Key`; worker `api_idempotency` registration | schema + route smoke checks | Implemented (baseline) |
| Migration/version traceability | SHOULD | explicit schema version marker for rollout sequencing | `d1_schema.sql` `schema_version` table + baseline row | SQL schema parse check | Implemented (baseline) |

---

## Up-to-date open gaps (only remaining work)

## A) EAS conformance depth

1. Command grammar validation is still subset-based and not full payload-grammar complete for all command variants used by modern Outlook clients.
2. Some command responses remain interoperability-profile responses rather than full Exchange semantic parity for advanced state/conflict branches.
3. No repository-level end-to-end Outlook protocol fixture suite proving full MUST/SHOULD coverage across negative-path permutations.

## B) EWS fidelity and edge-case parity

1. `FindItem`/`GetItem`/`SyncFolderItems` are implemented, but advanced shape/traversal/property sets and strict parity for all edge fault conditions remain partial.
2. Delete/tombstone and conflict/version semantics in `SyncFolderItems` are baseline-level and not yet full parity with all Exchange server behaviors.
3. Additional EWS operations used in broader Outlook workflows (beyond current calendar-focused scope) are still not implemented.

## C) Data and reliability hardening

1. `api_idempotency` cleanup/retention lifecycle is not implemented.
2. No dedicated migration runner is included; schema version table exists but deployment migration orchestration is external.
3. Concurrency/load test coverage for worker + gateway state update races is not present in-repo.

## D) Binder1 compliance evidence gap

1. Traceability is now documented in this file, but automated evidence generation (machine-checked conformance report) is not yet implemented.
2. MUST/SHOULD matrix rows do not yet have full integration-test artifacts for every row.

---

## Updated definition of done for your specific use-case

The solution is done when all conditions below are simultaneously true:

1. **Outlook-native onboarding**
   - Outlook Windows and Outlook Android autodiscover and authenticate without any client-side extensions or manual protocol workarounds.

2. **Calendar interoperability correctness**
   - Calendar CRUD, recurrence handling, meeting response updates, and deletions are deterministic and consistent across Outlook clients and Stalwart-backed state.

3. **Protocol correctness**
   - Implemented EAS/EWS operations return protocol-correct success and fault semantics for both happy-path and negative-path requests that are exercised by supported Outlook versions.

4. **State durability and retry safety**
   - Sync/provision/EWS state survives restarts and retries without corruption, duplicate side effects, or divergent sync windows.

5. **Operational hardening**
   - Observability, throttling, retry/backoff behavior, and Cloudflare trust-path assumptions are validated in staging under load representative of production usage.

6. **Evidence-backed conformance**
   - Automated test artifacts and traceability evidence show that all Binder1 MUST/SHOULD requirements in the scoped protocol families are satisfied for the declared use-case boundary.

