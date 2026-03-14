# GAP_ANALYSIS.md (Binder1-driven, current-state only)

## Scope

- Single protocol source of truth: `Binder1.txt`.
- Target use-case: Outlook Windows + Outlook Android calendar interoperability against Stalwart via this gateway (EAS/EWS), through Cloudflare Worker + D1.
- This document lists **only currently open gaps** and an up-to-date traceability matrix.

---

## Open gaps (only)

### A) EAS conformance depth

1. **Command grammar coverage is still not complete for all real-world Outlook variants**
   - Current validation enforces required namespace/element checks for key commands, but does not implement full XML grammar/state-machine validation for every command payload permutation in Binder1 families.
2. **Advanced semantic parity remains partial**
   - Several commands still return baseline interoperable success/status structures rather than exhaustive Exchange-grade state/conflict semantics for every branch.
3. **End-to-end protocol fixture depth remains limited**
   - Repository has fixture-oriented tests, but not a full automated Outlook interoperability matrix proving all negative-path MUST/SHOULD scenarios.

### B) EWS fidelity and edge-case parity

1. **EWS shape/traversal/property fidelity is partial**
   - `FindItem`/`GetItem`/`SyncFolderItems` are implemented with operation-specific validation and faults, but advanced property-shape handling and full traversal nuances remain incomplete.
2. **SyncFolderItems parity is still partial for full Exchange behavior**
   - Delete/tombstone and conflict/version semantics improved but still do not model the entire Exchange server behavior surface.
3. **Broader Outlook workflow EWS operations are still missing**
   - Current implemented set is calendar-focused; additional operations used by wider Outlook workflows remain unimplemented.

### C) Data/reliability hardening

1. `api_idempotency` retention/cleanup lifecycle is not implemented.
2. Migration orchestration runner is not implemented (schema version tracking exists, but migration execution is external).
3. No full concurrency/load race harness validating state integrity under sustained parallel sync workloads.

### D) Conformance evidence gap

1. Traceability matrix exists, but machine-verifiable MUST/SHOULD evidence generation is not implemented.
2. Several matrix rows do not yet have full end-to-end integration artifacts.

---

## Binder1 MUST/SHOULD traceability matrix (current)

| Binder1 family | Level | Requirement summary | Code mapping | Test mapping | Current state |
|---|---|---|---|---|---|
| MS-ASHTTP | MUST | Authenticated EAS endpoint transport and command dispatch capability headers | `src/eas.rs` (`handle`, `options_response`, auth + routing) | `src/eas.rs` unit tests | Implemented (scope) |
| MS-ASCMD | MUST | Command extraction + per-command routing and status envelope behavior | `src/eas.rs` command dispatch + command validators | `src/eas.rs` unit tests | Implemented (scope) |
| MS-ASPROV | MUST | Provisioning key lifecycle (issue/ack/validate) | `src/eas.rs` provision flow + `src/storage.rs` + worker provision endpoints | unit-level + runtime path | Implemented (scope) |
| MS-ASWBXML / MS-ASAIRS | MUST | WBXML encode/decode for implemented command paths | `src/wbxml.rs`, `src/eas.rs` WBXML path | `src/wbxml.rs` tests | Implemented (scope) |
| MS-ASCAL | MUST | Calendar sync response generation for ActiveSync collections | `src/sync.rs` + `src/eas.rs` Sync path | `src/eas.rs` parsing tests | Implemented (scope) |
| EWS core message/type schemas (Binder1 EWS families) | MUST | Operation-specific request validation and operation-specific fault mapping | `src/ews.rs` (`validate_schema`, `operation_error_response`) | `src/ews.rs` tests | Implemented (scope) |
| EWS FindItem | SHOULD | Paged item listing with deterministic IDs/change keys | `src/ews.rs::handle_find_item`, `src/storage.rs::list_ews_items`, worker `/api/list_ews_items` | unit-level + fixture test | Implemented (partial-depth) |
| EWS GetItem | SHOULD | Item fetch by id with not-found and malformed-id fault mapping | `src/ews.rs::handle_get_item`, `src/storage.rs::get_ews_item_by_server_id`, worker `/api/get_ews_item_by_id` | `src/ews.rs` tests | Implemented (partial-depth) |
| EWS SyncFolderItems | MUST | Incremental state token handling and change return | `src/ews.rs::handle_sync_folder_items`, storage sync-state methods, worker `/api/get_ews_sync_state` + `/api/set_ews_sync_state` | `src/ews.rs` tests | Implemented (partial-depth) |
| Retry-safe typed writes | SHOULD | Idempotency signal propagation for typed write APIs | `src/storage.rs` Idempotency-Key generation + worker `api_idempotency` registration | worker/schema checks | Implemented (baseline) |
| Migration traceability | SHOULD | Explicit schema version marker | `d1_schema.sql` `schema_version` | schema check | Implemented (baseline) |

---

## New up-to-date Definition of Done (specific use-case)

Done means all of the following are true simultaneously in production-like validation:

1. Outlook Windows and Outlook Android autodiscover + authenticate natively without client extensions.
2. Calendar create/update/delete/recurrence/meeting-response flows round-trip deterministically between clients and backend.
3. Implemented EAS/EWS operations return protocol-correct success/fault behavior for happy and negative paths observed in supported Outlook versions.
4. Sync/provision/EWS state survives retries/restarts without corruption, duplication, or divergence.
5. Cloudflare edge/origin trust path, throttling, and observability are validated under realistic load.
6. Traceability evidence demonstrates Binder1 MUST/SHOULD coverage for all in-scope protocol families and is backed by automated integration artifacts.

