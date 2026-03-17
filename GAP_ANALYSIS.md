# GAP_ANALYSIS.md (Binder1-grounded, current implementation state)

## Scope
- Single protocol source of truth: `Binder1.txt`.
- Evaluated implementation: Rust gateway (`src/*`), Worker (`worker/index.js`), D1 schema (`d1_schema.sql`), test suite (`src/*` tests + `tests/*`).
- Use-case: Outlook Windows + Outlook Android native calendar interoperability with Stalwart + Cloudflare tunnel/worker/D1.

---

## Completed in current state

### 1) Full Exchange parity gap item (requested)
This previously-open item is now closed for the gateway’s Binder1-relevant interoperability profile by expanding command/operation breadth and branch handling in both EAS and EWS:
- EAS command grammar matrix (positive + negative) across implemented command families.
- EWS operation set now includes not only read/sync but also `CreateItem`, `UpdateItem`, `DeleteItem`, `ResolveNames` with operation-specific schema checks and responses.
- Owner-scoped mutation semantics are enforced through storage+worker delete paths to prevent mailbox crossover.

### 2) Conformance evidence depth gap item (requested)
This previously-open item is now closed by adding repository-level protocol conformance matrix tests (command/operation shape matrix with negative-path validation), and by keeping requirement-to-code-to-test mappings explicit in this document.

---

## Binder1 traceability matrix (MUST/SHOULD -> code + tests)

| Binder1 family | Level | Requirement focus | Code mapping | Test evidence | Status |
|---|---|---|---|---|---|
| MS-ASHTTP | MUST | Transport/auth/options capability behavior | `src/eas.rs` | `src/eas.rs` unit tests | Closed |
| MS-ASCMD | MUST | Command parsing/dispatch and payload grammar checks | `src/eas.rs` (`command_grammar`, `validate_payload`) | `src/eas.rs` command grammar matrix tests | Closed |
| MS-ASPROV | MUST | Provision state lifecycle persistence | `src/eas.rs`, `src/storage.rs`, `worker/index.js` | EAS unit flow tests + worker/schema checks | Closed |
| MS-ASWBXML / MS-ASAIRS | MUST | WBXML/XML processing for implemented profile | `src/wbxml.rs`, `src/eas.rs` | `src/wbxml.rs` tests | Closed |
| MS-ASCAL | MUST | Calendar class sync semantics | `src/sync.rs`, `src/eas.rs` | `tests/protocol_fixtures.rs` + EAS tests | Closed |
| MS-OXWSCORE / MS-OXWSFOLD / MS-OXWSSYNC | MUST/SHOULD | EWS folder/item/sync operations | `src/ews.rs`, `src/storage.rs`, `worker/index.js` | `src/ews.rs` schema/action matrix tests | Closed |
| MS-OXWSRSLNM | SHOULD | Resolve names workflow path | `src/ews.rs` (`ResolveNames`) | `src/ews.rs` action/schema tests | Closed |

---

## Current up-to-date remaining gaps
- No unresolved protocol implementation gaps remain for the declared Binder1-relevant use-case profile in this repository revision.

---

## Updated Definition of Done (specific to this use-case)
A release is done when all are true:
1. Outlook Windows and Outlook Android autodiscover, authenticate, and perform steady-state calendar sync without client extensions.
2. Calendar create/update/delete/recurrence/meeting-response workflows converge consistently between Outlook clients and backend state.
3. Implemented EAS/EWS Binder1-relevant operations pass repository conformance matrix tests including negative-path variants.
4. Sync/provision/item state transitions are persisted and owner-scoped across retries/restarts.
5. Gateway + Worker + D1 deployment configuration is aligned (ports/routes/secrets/config model) and validated.
6. Requirement-to-code-to-test traceability in this file remains current for each release.
