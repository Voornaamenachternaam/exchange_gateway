# GAP_ANALYSIS.md (Binder1-grounded, current implementation state)

## Scope
- Single protocol source of truth: `Binder1.txt`.
- Evaluated implementation: Rust gateway (`src/*`), Worker (`worker/index.js`), D1 schema (`d1_schema.sql`), and tests in `src/*` + `tests/*`.
- Stated use-case: native Outlook (Windows + Android) calendar interoperability with Stalwart via Cloudflare tunnel/worker/D1.

---

## What is implemented
- EAS transport/auth/options, command detection, and implemented-command validation/dispatch exist.
- EWS includes read/sync plus `CreateItem`/`UpdateItem`/`DeleteItem`/`ResolveNames` handlers with operation-scoped schema checks.
- Typed Worker/D1 storage endpoints are wired to Rust storage client methods.
- Repository includes unit/fixture tests for grammar/action/schema shapes and selected negative paths.

---

## Binder1 traceability matrix (MUST/SHOULD -> code + tests)

| Binder1 family | Level | Requirement focus | Code mapping | Test evidence | Status |
|---|---|---|---|---|---|
| MS-ASHTTP | MUST | EAS transport/auth/options behavior | `src/eas.rs` | `src/eas.rs` tests | Implemented (profile) |
| MS-ASCMD | MUST | Command parse/dispatch and payload validation for implemented commands | `src/eas.rs` | `src/eas.rs` tests | Implemented (profile) |
| MS-ASPROV | MUST | Provision key/state persistence | `src/eas.rs`, `src/storage.rs`, `worker/index.js` | EAS tests + worker/schema checks | Implemented (profile) |
| MS-ASWBXML / MS-ASAIRS | MUST | WBXML/XML conversion for implemented command set | `src/wbxml.rs`, `src/eas.rs` | `src/wbxml.rs` tests | Implemented (profile) |
| MS-ASCAL | MUST | Calendar sync flow support | `src/sync.rs`, `src/eas.rs` | fixture + unit tests | Implemented (profile) |
| MS-OXWSCORE / MS-OXWSFOLD / MS-OXWSSYNC | MUST/SHOULD | EWS folder/item/sync operations for calendar-centric workflow | `src/ews.rs`, `src/storage.rs`, `worker/index.js` | `src/ews.rs` tests + fixtures | Implemented (profile) |
| MS-OXWSRSLNM | SHOULD | ResolveNames path | `src/ews.rs` | `src/ews.rs` tests | Implemented (baseline) |

---

## Up-to-date remaining gaps
1. **Full Exchange parity beyond current profile remains open**  
   Binder1 contains broader Exchange protocol families and deeper branch permutations than currently implemented. Advanced property-shape permutations, strict conflict/version semantics, and broader Outlook workflows outside this calendar-focused profile are not exhaustively implemented.

2. **Conformance evidence depth remains open**  
   Tests cover command/operation shape matrices and selected negative paths, but there is no exhaustive, automated Outlook interoperability matrix producing complete Binder1 MUST/SHOULD evidence across all negative permutations.

3. **Operational production evidence remains open**  
   Long-running multi-device soak, failure-injection, and rollout/migration evidence are not present as repeatable CI artifacts in this repository.

---

## Updated Definition of Done (specific to this use-case)
A release is done when all are true:
1. Outlook Windows and Outlook Android autodiscover/authenticate and maintain stable native calendar sync.
2. Calendar CRUD/recurrence/meeting-response semantics converge deterministically across clients/backend.
3. Binder1-relevant implemented EAS/EWS operations pass automated positive and negative conformance suites.
4. Sync/provision/item state remains correct across retries/restarts and concurrent device activity.
5. Gateway + Worker + D1 deployment and edge/origin security constraints are validated under production-like load.
6. Requirement-to-code-to-test traceability remains current and release-gated.

## Of the six Definition-of-Done items, not yet 100% complete today
- **Not yet 100% complete:** 1, 2, 3, 4, and 5.
- **Closest to complete:** 6 (traceability is present, but still depends on unresolved gaps above for full release-gate confidence).
