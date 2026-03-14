# GAP_ANALYSIS.md (Binder1-driven, current state)

## Scope and source of truth
- Protocol source of truth: `Binder1.txt`.
- Assessed implementation: `src/*`, `worker/index.js`, `d1_schema.sql`, `tests/*`.
- Use-case target: Stalwart-backed calendar interoperability in native Outlook (Windows + Android) through this Rust gateway behind Cloudflare.

---

## What is now implemented (already done)

### EAS transport and command baseline (in-scope)
- OPTIONS capability headers, command detection, Basic auth challenge flow, and WBXML/XML request-response path exist.
- Implemented command surface includes Sync, FolderSync, Provision, Settings, ItemOperations, Search, MeetingResponse, ResolveRecipients, ValidateCert, GetItemEstimate, MoveItems, SendMail, SmartReply, SmartForward.
- Provision policy state is persisted by owner+device via typed Worker/D1 routes.

### EWS capabilities expanded beyond calendar-only read paths
- Implemented operations now include: `GetFolder`, `FindFolder`, `FindItem`, `GetItem`, `SyncFolderItems`, `CreateItem`, `UpdateItem`, `DeleteItem`, and `ResolveNames`.
- Operation-specific schema checks and operation-specific error envelope shaping are present.
- Item/sync state persistence paths are wired through typed Worker APIs and D1 schema.

### Storage/worker contract
- Typed storage endpoints are aligned with Rust client methods (sync keys, item map CRUD, provision state, EWS state/item reads).
- Idempotency-key handling and schema version table are present.

---

## Binder1 traceability matrix (MUST/SHOULD -> code/test)

| Binder1 family | Level | Requirement theme | Code mapping | Test mapping | Status |
|---|---|---|---|---|---|
| MS-ASHTTP | MUST | EAS transport semantics, headers, auth entrypoints | `src/eas.rs` | `src/eas.rs` tests | Implemented (profile) |
| MS-ASCMD | MUST | Command parsing/dispatch and command-specific validation | `src/eas.rs` | `src/eas.rs` tests | Implemented (profile) |
| MS-ASPROV | MUST | Provision key lifecycle and persistence | `src/eas.rs`, `src/storage.rs`, `worker/index.js` | EAS unit coverage + worker/schema checks | Implemented (profile) |
| MS-ASCAL | MUST | Calendar class sync payloading | `src/sync.rs`, `src/eas.rs` | fixture + unit coverage | Implemented (profile) |
| MS-ASWBXML / MS-ASAIRS | MUST | WBXML encode/decode for active command profile | `src/wbxml.rs` | `src/wbxml.rs` tests | Implemented (profile) |
| MS-OXWSCORE / MS-OXWSFOLD / MS-OXWSSYNC | MUST/SHOULD (use-case relevant subset) | EWS SOAP operations, folder/item access, sync-state handling | `src/ews.rs`, `src/storage.rs`, `worker/index.js` | `src/ews.rs`, `tests/protocol_fixtures.rs` | Implemented (expanded profile) |
| MS-OXWSRSLNM | SHOULD | Name resolution for Outlook workflows | `src/ews.rs` (`ResolveNames`) | fixture-level request-shape tests | Implemented (baseline) |

---

## Current up-to-date gaps (only unresolved items)

1. **Full Exchange parity is still broader than current profile**  
   Advanced EWS/EAS branches (complete property-set permutations, full conflict semantics, and all Outlook workflow permutations in broader Exchange families from Binder1) are not exhaustively implemented.

2. **Conformance evidence depth**  
   The repository has unit/fixture tests, but does not yet include a full automated Outlook interoperability matrix generating exhaustive Binder1 MUST/SHOULD evidence artifacts across all negative-path permutations.

3. **Operational hardening depth**  
   Production-grade stress/soak artifacts (multi-device long-duration sync, failure injection, and rollback/migration drills) are not yet committed as repeatable CI evidence.

---

## Updated Definition of Done (specific to your use-case)

Done means all of the following are true in production-like validation:

1. Outlook Windows and Outlook Android can autodiscover, authenticate, and continuously sync calendar data natively against this gateway without client-side plugins.
2. Calendar CRUD, recurrence, attendee/meeting-response behavior, and cross-client state convergence are deterministic.
3. For the implemented Binder1-relevant command/operation profile, requests and faults are protocol-correct for both success and negative-path scenarios.
4. Sync/provision/EWS state persists safely across restarts and retries without duplication, corruption, or mailbox crossover.
5. Cloudflare edge/origin routing, trust boundaries, and failure behavior are validated under representative load.
6. Requirement-to-code-to-test traceability remains current on every release, and regressions are blocked by automated checks.
