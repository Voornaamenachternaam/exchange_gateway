# GAP_ANALYSIS.md

## Scope and source of truth

This file compares:

1. the **current repository state** of `exchange_gateway` plus its Cloudflare components; and
2. your **specific use-case**: native Outlook calendar interoperability against an existing Stalwart Mailserver v0.15.5 deployment,

using **`Binder1.txt` as the single Microsoft protocol source of truth**.

The assessment below is deliberately gap-focused: it lists what still remains between the current implementation and the target behavior required by your use-case.

---

## Evaluated implementation

- Rust HTTP entrypoints: `src/main.rs`
- ActiveSync transport/dispatch: `src/eas.rs`
- Calendar sync shaping and CalDAV read path: `src/sync.rs`, `src/caldav.rs`
- EWS transport/dispatch: `src/ews.rs`
- Worker-backed persistence bridge: `src/storage.rs`, `worker/index.js`, `d1_schema.sql`
- Repository tests: `tests/protocol_fixtures.rs` and unit tests in `src/*`

---

## Current implemented surface

The repository currently does provide these important building blocks:

- Basic-authenticated EAS and EWS HTTP endpoints.
- Outlook autodiscover responses through the Cloudflare Worker.
- D1-backed persistence for sync keys, provision keys, EWS sync state, and server-id/resource mappings.
- Outbound calendar projection from CalDAV data into EAS Sync responses.
- Basic EWS folder/item discovery shapes and response envelopes.
- Cloudflare edge hardening controls for rate limiting, payload caps, method allow-listing, and authenticated typed API access.

Those are useful foundations, but they do **not** yet close all gaps to the requested end state.

---

## Specific remaining gaps

### 1) **Hard blocker: EAS calendar write requests are not persisted into Stalwart**

`Sync` in `src/eas.rs` always routes to `sync::perform_sync(...)`, and `sync::perform_sync(...)` reads CalDAV data and emits outbound Sync XML; it does not parse client-side `Add`, `Change`, or `Delete` commands and does not write those mutations back to CalDAV. This means the current implementation is not yet sufficient for native Outlook-on-Android calendar authoring through ActiveSync.  

**Why this is a Binder1 gap:** `Binder1.txt` includes the ActiveSync command and calendar-class protocol families (`[MS-ASCMD]`, `[MS-ASCAL]`, `[MS-ASAIRS]`, `[MS-ASHTTP]`) and those families cover client-to-server mutation semantics, not just server-to-client projection.  

**Current code evidence:**
- `Sync` dispatch calls `sync::perform_sync(...)` without a mutation-application path. 【F:src/eas.rs†L523-L549】
- `perform_sync(...)` reads CalDAV calendars/events and builds outbound `<Add>`, `<Change>`, and `<Delete>` blocks for the client, but does not apply inbound client mutations to CalDAV. 【F:src/sync.rs†L222-L270】【F:src/sync.rs†L277-L496】
- `CaldavClient` only implements discovery and `REPORT` query operations, not `PUT`, `DELETE`, or other write methods. 【F:src/caldav.rs†L6-L68】

**Impact on your use-case:** Outlook Android calendar create/update/delete cannot be considered fully implemented.

---

### 2) **Hard blocker: EWS CreateItem / UpdateItem / DeleteItem do not write to Stalwart CalDAV**

The current EWS mutation handlers update only the Worker/D1 mapping layer. They create synthetic item identifiers and update mapping records, but they do **not** persist `.ics` resources into Stalwart through CalDAV. Therefore Outlook-for-Windows EWS mutations are not yet end-to-end durable in the actual calendar backend.  

**Why this is a Binder1/use-case gap:** your requested use-case requires the Stalwart calendar itself to be used natively by Outlook. Mapping-only behavior is not sufficient.  

**Current code evidence:**
- `handle_create_item(...)` generates a server id and stores an item mapping, but no CalDAV write is performed. 【F:src/ews.rs†L816-L868】
- `handle_update_item(...)` mutates only mapping metadata and synthetic ETag state, but no CalDAV update is performed. 【F:src/ews.rs†L873-L958】
- `handle_delete_item(...)` deletes only the D1 mapping entry, but no CalDAV delete is performed. 【F:src/ews.rs†L960-L999】
- The only CalDAV client methods present are calendar discovery and event query. 【F:src/caldav.rs†L11-L68】

**Impact on your use-case:** Outlook Windows calendar create/update/delete cannot be considered fully implemented against the real Stalwart calendar store.

---

### 3) **ActiveSync command advertisement exceeds real implementation fidelity**

The OPTIONS response advertises a broad command set including `SendMail`, `SmartForward`, `SmartReply`, `GetAttachment`, `FolderCreate`, `FolderDelete`, `FolderUpdate`, `MoveItems`, `GetItemEstimate`, `MeetingResponse`, `Search`, `Settings`, `Ping`, `ItemOperations`, `Provision`, `ResolveRecipients`, and `ValidateCert`. However, several of these commands currently return generic success-shaped placeholder responses rather than full command semantics.  

**Why this is a Binder1 gap:** `Binder1.txt` includes `[MS-ASCMD]` command semantics and related namespaces. Advertising commands with placeholder semantics leaves protocol conformance incomplete.  

**Current code evidence:**
- Advertised command list in `OPTIONS`. 【F:src/eas.rs†L246-L260】
- Generic success responses for `Ping`, `Settings`, `SendMail`, `SmartReply`, `SmartForward`, `ItemOperations`, `Search`, `MeetingResponse`, `ResolveRecipients`, `ValidateCert`, `GetItemEstimate`, and `MoveItems`. 【F:src/eas.rs†L551-L645】

**Impact on your use-case:** Outlook clients may exercise a command branch that the server advertises as supported but does not implement with full semantics.

---

### 4) **Meeting-response semantics are not implemented end-to-end**

`MeetingResponse` currently returns a success envelope but does not translate accept/tentative/decline into Stalwart calendar state changes, attendee response state, or CalDAV/iCalendar attendee semantics.  

**Why this is a Binder1 gap:** meeting-response behavior is explicitly part of the ActiveSync command family covered in `Binder1.txt`, and your use-case is calendar-focused rather than read-only.  

**Current code evidence:**
- `MeetingResponse` validation exists at shape level. 【F:src/eas.rs†L77-L80】
- Runtime handling is a generic success wrapper only. 【F:src/eas.rs†L600-L607】

**Impact on your use-case:** meeting invitations and responses are not yet trustworthy for production use.

---

### 5) **Calendar property coverage remains partial versus Binder1 calendar surface**

The current outbound EAS calendar item shaping covers a limited subset such as subject, start, end, dtstamp, busy status, sensitivity, location, body, UID, all-day flag, and a reduced recurrence mapping. Binder1’s ActiveSync calendar specifications include a materially broader calendar property surface, including organizer/attendee and other meeting-related fields, instance/exception handling, body/truncation/ghosting interactions, location variants, proposal fields, online meeting links, and version-dependent behaviors.  

**Current code evidence for the currently emitted subset:**
- Subject, times, busy/sensitivity, location, body, UID, all-day event, recurrence. 【F:src/sync.rs†L418-L465】
- Reduced recurrence mapping only. 【F:src/sync.rs†L77-L216】

**Impact on your use-case:** some Outlook calendar items can round-trip incompletely or lose fidelity even when discovery/sync succeeds.

---

### 6) **Exception / recurrence mutation semantics are incomplete**

Binder1 covers recurring-series exception behavior, including version-specific exception handling and `InstanceId` usage. The current implementation projects limited recurrence information but does not implement full inbound recurrence exception mutation handling through EAS or EWS.  

**Current code evidence:**
- Recurrence serialization is limited to a subset of RRULE shapes. 【F:src/sync.rs†L77-L216】
- No CalDAV write-path exists for inbound recurring exception edits. 【F:src/caldav.rs†L11-L68】【F:src/eas.rs†L523-L549】【F:src/ews.rs†L816-L999】

**Impact on your use-case:** edited instances of recurring meetings remain a correctness risk.

---

### 7) **Cloudflare Worker persists state, but the generic SQL API remains an administrative risk surface**

The Worker now disables the generic SQL API by default and restricts it to `SELECT` when enabled, which is an improvement. However, for the strictest production profile aligned to your use-case, the generic SQL API is still a residual surface that exists in code and can be enabled by configuration.  

**Current code evidence:**
- SQL API gate and `SELECT`-only restriction. 【F:worker/index.js†L264-L316】

**Impact on your use-case:** this is not the top blocker, but it remains a security review item for a hardened production posture.

---

### 8) **Cloudflare/Stalwart deployment proof is not present as reproducible repository evidence**

The repository documents a Cloudflare free-plan deployment shape and Stalwart v0.15.5 ingress model, but it does not contain reproducible proof artifacts showing a full end-to-end validation run against a real Stalwart v0.15.5 instance and real Cloudflare services.  

**Current code evidence:**
- Documented deployment model exists. 【F:CLOUDFLARE_DEPLOYMENT.md†L1-L122】

**Impact on your use-case:** production-readiness cannot honestly be marked complete from repository evidence alone.

---

## Definition-of-done status for the specific use-case

### 1. Native account setup in Outlook Windows 11 and Outlook Android 15
- **Status:** **Partially implemented**
- Autodiscover, EWS endpoint, and EAS endpoint exist, but full client interoperability depends on the missing durable write paths and calendar fidelity gaps. 【F:worker/index.js†L318-L404】【F:src/main.rs†L37-L53】

### 2. Calendar create/update/delete/sync/meeting-response convergence
- **Status:** **Not implemented completely**
- Outbound sync exists, but durable EAS/EWS writes and meeting-response semantics are still missing. 【F:src/eas.rs†L523-L645】【F:src/ews.rs†L816-L999】

### 3. Worker + D1 + tunnel + gateway deployment profile documented and endpoint-verifiable
- **Status:** **Implemented**
- The deployment profile is documented and the Worker/D1/tunnel shape is explicitly described. 【F:CLOUDFLARE_DEPLOYMENT.md†L1-L122】

### 4. Sync/provision/item state consistency across retries/restarts
- **Status:** **Partially implemented**
- D1-backed state and idempotency exist, but because write-through into CalDAV is missing, consistency is only partial relative to the real Stalwart calendar backend. 【F:src/storage.rs†L59-L119】【F:worker/index.js†L84-L98】

### 5. TLS termination and request-shaping controls active and verified in production
- **Status:** **Partially implemented**
- Edge hardening controls exist in code and docs, but deployed verification evidence is not in-repo. 【F:worker/index.js†L100-L218】【F:CLOUDFLARE_DEPLOYMENT.md†L81-L122】

### 6. Binder1 family traceability to code/tests remains current
- **Status:** **Partially implemented**
- There is code and test coverage for several implemented branches, but Binder1-derived remaining gaps are still material and not fully closed. 【F:tests/protocol_fixtures.rs†L1-L47】【F:src/eas.rs†L656-L792】【F:src/ews.rs†L1038-L1124】

---

## Bottom line

The **current** `exchange_gateway` repository is **not yet fully compatible** with your requested end state. The two most important blockers are:

1. **no durable EAS write-through into Stalwart CalDAV**, and
2. **no durable EWS write-through into Stalwart CalDAV**.

Until those are implemented, the solution cannot honestly be considered fully production-ready for your Outlook + Stalwart calendar use-case, regardless of the Worker and transport hardening already present.
