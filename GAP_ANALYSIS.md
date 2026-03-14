# GAP_ANALYSIS.md (Binder1 source-of-truth, current repository state)

## Scope

- Authoritative protocol corpus: `Binder1.txt`.
- Implementation assessed: Rust gateway (`src/*`), Worker API (`worker/index.js`), D1 schema (`d1_schema.sql`), test fixtures (`tests/*`).
- Target use-case: native Outlook Windows + Outlook Android calendar interoperability with Stalwart through EAS/EWS over Cloudflare Worker + D1.

---

## Status summary (current)

### Closed in this revision

1. **EAS conformance-depth gap set is closed for the declared use-case scope**:
   - Expanded command grammar checks for implemented command set (namespace + required elements + class validation for Sync).
   - Replaced broad free-form validation with command-specific grammar mapping.
   - Expanded repository fixture coverage for EAS/EWS request-shape and negative-path baseline scenarios.

### Remaining open gaps (up-to-date only)

1. **EWS broader workflow parity remains partial**:
   - Current EWS operation set is still focused on the calendar interoperability profile, not full Outlook workflow breadth.
2. **High-depth Exchange semantic parity**:
   - Advanced parity details (all edge conflict/shape branches in every Exchange family path) remain incremental hardening work beyond current use-case boundary.
3. **Automated compliance evidence depth**:
   - Traceability exists in-repo, but machine-generated exhaustive MUST/SHOULD conformance reporting is not yet implemented.

---

## Binder1 MUST/SHOULD traceability matrix (current)

| Binder1 family | Requirement level | Requirement summary | Code mapping | Test mapping | Status |
|---|---|---|---|---|---|
| MS-ASHTTP | MUST | Authenticated ActiveSync transport endpoint + command dispatch capability headers | `src/eas.rs` (`handle`, auth path, `options_response`) | `src/eas.rs` unit tests | Implemented (scope) |
| MS-ASCMD | MUST | Command extraction and command-specific payload grammar checks | `src/eas.rs` (`extract_root_command`, `command_grammar`, `validate_payload`) | `src/eas.rs` unit tests + fixture tests | Implemented (scope) |
| MS-ASPROV | MUST | Provision key issue/ack/validation lifecycle | `src/eas.rs` provision flow + `src/storage.rs` provision state methods + worker provision routes | `src/eas.rs` test coverage + runtime flow | Implemented (scope) |
| MS-ASWBXML / MS-ASAIRS | MUST | WBXML/XML decode/encode for implemented command surfaces | `src/wbxml.rs` + `src/eas.rs` WBXML path selection | `src/wbxml.rs` tests | Implemented (scope) |
| MS-ASCAL | MUST | Calendar sync command handling and response generation | `src/sync.rs` + `src/eas.rs` Sync path | `src/eas.rs` tests + fixture set | Implemented (scope) |
| EWS request validation families in Binder1 corpus | MUST | Operation-specific schema validation and operation-specific error mapping | `src/ews.rs` (`validate_schema`, `operation_error_response`) | `src/ews.rs` tests | Implemented (scope) |
| EWS FindItem | SHOULD | Paged item listing and deterministic identity/changekey projection | `src/ews.rs::handle_find_item`, `src/storage.rs::list_ews_items`, worker `/api/list_ews_items` | `src/ews.rs` + fixture tests | Implemented (partial-depth) |
| EWS GetItem | SHOULD | Item lookup by id + malformed/not-found error mapping | `src/ews.rs::handle_get_item`, `src/storage.rs::get_ews_item_by_server_id`, worker `/api/get_ews_item_by_id` | `src/ews.rs` + fixture tests | Implemented (partial-depth) |
| EWS SyncFolderItems | MUST | Sync-state token handling and incremental change payloads | `src/ews.rs::handle_sync_folder_items`, sync-state methods, worker EWS sync-state routes | `src/ews.rs` tests | Implemented (partial-depth) |
| Retry-safe typed writes | SHOULD | Idempotency-key propagation and registration for write APIs | `src/storage.rs` idempotency key generation + worker `api_idempotency` | schema/worker checks | Implemented (baseline) |
| Migration traceability | SHOULD | Schema-version marker for rollout tracking | `d1_schema.sql` `schema_version` | schema parse check | Implemented (baseline) |

---

## Specific up-to-date gaps (removed everything already done)

### 1) EWS breadth outside calendar-focused profile

- Additional EWS operations used by broader Outlook workflows are not fully implemented in the current gateway profile.

### 2) Full-spectrum protocol conformance automation

- End-to-end automated conformance runner that generates exhaustive Binder1 MUST/SHOULD pass/fail artifacts is not present.

### 3) Operational validation depth

- Large-scale concurrency/load certification artifacts for multi-device long-running synchronization are not yet committed in-repo.

---

## Updated Definition of Done (specific to your use-case)

Done means all conditions below are met in production-like validation:

1. Outlook Windows and Outlook Android autodiscover and authenticate natively without client-side extensions.
2. Calendar create/update/delete/recurrence/meeting-response flows are deterministic and consistent across clients and backend state.
3. Implemented EAS/EWS command paths return protocol-correct success/fault behavior for supported Outlook request patterns (including negative-path variants exercised in fixtures/tests).
4. Sync/provision/EWS state persists safely across retries/restarts without duplication or state divergence.
5. Cloudflare edge/origin trust assumptions, throttling behavior, and operational observability are validated under representative load.
6. Binder1-traceable evidence (requirements-to-code/tests) is maintained for all in-scope protocol families and kept current with every release.

