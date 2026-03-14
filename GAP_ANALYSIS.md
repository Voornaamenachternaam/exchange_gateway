# exchange_gateway Gap Analysis (Binder1-aligned, current repository state)

## Scope and method

- Source of protocol truth: `Binder1.txt` (full corpus in this repo).
- Scope of this analysis: the current implementation in this repository (`src/*`, `worker/index.js`, `d1_schema.sql`, runtime config files), evaluated against the stated use-case:
  - Stalwart v0.15.5 calendar backend
  - Cloudflare Tunnel + Worker + D1
  - Outlook Windows 11 + Outlook Android ActiveSync/EWS interoperability without client extensions.
- This document is intentionally **current-state only**: completed items are recorded as done; open items are only those still not implemented or only partially implemented.

---

## What is already implemented (current, verified in code)

### Deployment/runtime alignment

- Gateway bind/runtime path alignment is implemented:
  - `config.toml` binds `0.0.0.0:8134`.
  - container runtime is file-config driven (`/etc/exchange-gateway/config.toml`) instead of legacy env-only architecture.
- Worker typed API + Rust storage client contract exists and is wired.

### ActiveSync transport and command surface

- EAS endpoint supports:
  - `OPTIONS` capability discovery headers.
  - Command detection from XML root with `Cmd` query fallback.
  - WBXML/XML decode/encode path via `Wbxml`.
  - Request tracing header (`X-Request-Id`).
  - Basic per-device throttling/backoff behavior.
  - Provision policy persistence/validation flow (owner/device keyed).
  - Sync path consuming request `CollectionId` (no fixed-only collection id).

### EWS (now beyond static stubs)

- EWS now has operation-specific dispatch and baseline schema checks for:
  - `GetFolder`
  - `FindItem`
  - `SyncFolderItems`
- `FindItem` now returns dynamic, paged data sourced from persisted item mappings.
- `SyncFolderItems` now persists/reads sync state and emits change sets from stored deltas.
- Responses use deterministic IDs/change keys derived from persisted data (not static placeholder literals).

### Data/consistency mechanisms

- Typed storage endpoints are present for sync/provision/EWS state flows.
- D1 schema supports:
  - `sync_state`
  - `item_map`
  - `provision_state`
  - `ews_sync_state`
  - `schema_version`
  - `api_idempotency`
- Write-path idempotency key propagation from Rust client to worker is implemented.

---

## Current open gaps (up-to-date)

## A) ActiveSync conformance depth (MS-ASHTTP / MS-ASCMD / MS-ASPROV / MS-ASWBXML families)

1. **Command semantics are still interoperability-profile level, not full Exchange-grade conformance**:
   - Many commands return baseline success/status bodies without full per-command behavioral model (state transitions, conflict rules, protocol-specific error maps).
2. **Schema validation is partial**:
   - Current validation checks required high-level elements/namespaces for selected commands, but does not enforce full payload grammar and versioned constraints for all supported command variants.
3. **Provisioning policy model is persisted but simplified**:
   - Lacks richer policy payload negotiation and device-policy lifecycle semantics expected by full Exchange policy engines.
4. **WBXML breadth**:
   - WBXML codec is functional for implemented command paths, but full token/code-page parity for all protocol-family payload permutations remains incomplete.

## B) EWS protocol depth (MS-OXWS* families relevant to use-case)

1. **Operation coverage remains limited**:
   - `GetFolder`, `FindItem`, and `SyncFolderItems` are implemented.
   - Full Outlook-grade parity still requires deeper support for additional EWS operations and edge-case SOAP faults.
2. **Delta semantics are basic**:
   - `SyncFolderItems` currently models create/change style deltas from item-map history, but advanced delete/tombstone/conflict/version semantics are not fully modeled.
3. **Paging and shape fidelity**:
   - Basic paging is implemented; full Exchange shape fidelity (all requested properties, strict response-class nuances, advanced traversal/base shape behaviors) is still partial.

## C) Data consistency / reliability hardening

1. **Idempotency persistence exists, but lifecycle management is pending**:
   - No cleanup/TTL policy yet for `api_idempotency` growth management.
2. **Concurrency/transaction semantics**:
   - Core CRUD paths are in place; higher-load contention behavior and strict transaction isolation patterns are not yet fully hardened/tested.
3. **Migration management**:
   - `schema_version` table exists, but no formal migration runner/process is present in-repo.

## D) End-to-end use-case verification gap

1. **Protocol conformance traceability matrix is missing**:
   - No repository-level MUST/SHOULD matrix mapping Binder1 protocol requirements to tests and code points.
2. **Automated interoperability test suite is missing**:
   - No exhaustive automated Outlook Windows/Android scenario suite in-repo for regression-proof “native perfect” validation.

---

## Protocol status table (use-case relevant families only)

| Protocol family (Binder1) | Current status | Gap status |
|---|---|---|
| MS-ASAIRS / MS-ASWBXML | Implemented for active paths | Partial breadth remaining |
| MS-ASCMD / MS-ASHTTP | Implemented for core command transport/dispatch | Full semantic parity still open |
| MS-ASCAL | Implemented (calendar-focused sync path) | Edge-case parity still open |
| MS-ASPROV | Implemented with persisted policy key flow | Advanced policy semantics still open |
| MS-ASDTYPE | Implemented for used payload subsets | Broader type parity still open |
| MS-OXDISCO / MS-OXWCONFIG | Implemented in worker autodiscover surfaces | Variant completeness still open |
| EWS operation families used by Outlook calendar sync | `GetFolder`/`FindItem`/`SyncFolderItems` implemented | Broader op/fault/detail parity still open |

---

## Prioritized remaining work

1. Add full protocol traceability matrix (Binder1 MUST/SHOULD -> code/test mapping).
2. Expand EWS operation fidelity and SOAP fault mapping for Outlook edge-cases.
3. Expand EAS command-specific semantics/status handling beyond baseline success paths.
4. Harden WBXML code-page/token coverage for broader payload permutations.
5. Add migration runner + idempotency cleanup policy.
6. Add concurrency/load and long-running sync reliability test harness.
7. Add end-to-end Outlook Windows/Android automated interoperability suite.

---

## Updated definition of done (specific to your use-case)

The solution is done when all of the following are true in staging and production-like validation:

1. **Native client behavior**
   - Outlook Windows and Outlook Android can autodiscover, authenticate, and continuously sync calendar data without client extensions or manual protocol overrides.

2. **Calendar interoperability correctness**
   - Calendar create/update/delete/recurrence/meeting-response flows round-trip correctly and deterministically across clients and backend.
   - Multi-device sync is stable, with no duplicate or missing events under normal retry/network churn.

3. **Protocol correctness envelope**
   - Implemented EAS/EWS operations return protocol-correct status/fault semantics for both happy-path and negative-path scenarios.
   - WBXML/XML handling is robust for all payloads exercised by supported Outlook client versions.

4. **State and persistence robustness**
   - Sync/provision/EWS state persistence survives restarts and retries without corruption.
   - Idempotency and migration controls are operationally defined, deployed, and monitored.

5. **Operational hardening**
   - Observability (request correlation, structured logs, actionable errors), rate controls, and retry behavior are validated under load.
   - Cloudflare edge/origin trust assumptions are explicitly enforced and verified in deployment.

6. **Evidence-based validation**
   - Automated tests and traceability artifacts demonstrate conformance for relevant Binder1 requirements in the supported scope.

