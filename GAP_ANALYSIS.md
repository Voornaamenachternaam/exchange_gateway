# GAP_ANALYSIS.md

## Scope and protocol baseline

This file compares the **current repository state** of `exchange_gateway` + its Cloudflare components against:

- your **specific use-case**: native Outlook calendar interoperability against an existing Stalwart Mailserver v0.15.5 deployment; and
- **`Binder1.txt`**, which is treated here as the single Microsoft protocol source of truth.

---

## What is now closed relative to the previous gap list

### 1) EAS write-through into Stalwart CalDAV

The prior hard blocker was that ActiveSync `Sync` requests were only projected outward from CalDAV and client-side `Add` / `Change` / `Delete` mutations were not written back into Stalwart.

That blocker is now closed in the codebase:

- `Sync` handling now attempts to apply inbound client mutations before returning the outbound sync payload. 【F:src/eas.rs†L528-L571】
- EAS `Add` / `Change` / `Delete` operations are parsed into typed mutations. 【F:src/calendar.rs†L28-L42】【F:src/calendar.rs†L316-L411】
- Those mutations now perform real CalDAV `PUT` / `DELETE` operations and then persist updated item mappings in the Worker/D1 layer. 【F:src/sync.rs†L23-L109】
- The CalDAV client now supports `GET`, `PUT`, and `DELETE`, not only discovery and `REPORT`. 【F:src/caldav.rs†L11-L154】

### 2) EWS CreateItem / UpdateItem / DeleteItem write-through into Stalwart CalDAV

The prior hard blocker was that EWS mutations only updated D1 mapping state and never touched the actual Stalwart calendar backend.

That blocker is now closed in the codebase:

- EWS `CreateItem` now parses a calendar item, writes an `.ics` resource to CalDAV, derives a stable Exchange server id from the real CalDAV resource href, and persists the mapping. 【F:src/ews.rs†L800-L882】
- EWS `UpdateItem` now fetches the existing CalDAV resource, merges updated fields, writes the updated `.ics` body back to CalDAV, and updates mapping state. 【F:src/ews.rs†L884-L1035】
- EWS `DeleteItem` now deletes the real CalDAV resource before removing the mapping record. 【F:src/ews.rs†L1037-L1115】

### 3) ActiveSync command advertisement versus implementation

The prior gap was that `OPTIONS` advertised a much wider ActiveSync command surface than the repository actually implemented with durable semantics.

That gap is now closed in the codebase for the calendar-focused profile:

- The advertised command list has been reduced to the calendar-relevant commands that the gateway actually handles for this profile. 【F:src/eas.rs†L246-L260】

### 4) Meeting-response semantics

The prior gap was that `MeetingResponse` returned a success envelope without updating the underlying event state.

That gap is now closed in the codebase for the current calendar model:

- `MeetingResponse` now updates the attendee response on the real CalDAV resource and persists the updated mapping state before returning a response. 【F:src/eas.rs†L623-L651】【F:src/sync.rs†L172-L237】

---

## Specific remaining gaps

### 1) Calendar property fidelity versus the Binder1 calendar surface

This gap has now been materially closed in the codebase for the gateway’s calendar profile:

- The shared calendar model now carries organizer, attendee, category, busy/sensitivity, reminder, timezone, proposal/response, online-meeting, client UID, and exception metadata instead of the earlier reduced subset. 【F:src/calendar.rs†L8-L88】
- ICS parsing/rendering now preserves those richer properties, including categories, reminders, meeting metadata, online-meeting links, and recurrence exceptions. 【F:src/calendar.rs†L420-L695】【F:src/calendar.rs†L697-L888】
- ActiveSync sync mutation parsing and outbound sync projection now round-trip the same richer set of calendar fields. 【F:src/calendar.rs†L972-L1236】【F:src/sync.rs†L318-L528】
- EWS parsing/update fallback paths were expanded so the wider calendar model is initialized safely during EWS-driven writes. 【F:src/calendar.rs†L1275-L1374】【F:src/ews.rs†L942-L964】

**Impact:** the repository now covers a substantially broader Binder1-relevant calendar surface for Outlook-style events instead of the previous minimal field subset.

---

### 2) Recurrence and exception coverage

This gap has now been materially closed in the codebase for the implemented profile:

- ICS parsing now understands `RECURRENCE-ID`, `EXDATE`, cancelled/deleted instances, and modified exception VEVENTs. 【F:src/calendar.rs†L420-L695】
- ICS rendering now emits deleted-instance `EXDATE`s plus explicit exception VEVENTs for modified instances. 【F:src/calendar.rs†L697-L888】
- ActiveSync sync mutation parsing now accepts `Exceptions`, `Exception`, `Deleted`, and `ExceptionStartTime`, and outbound sync payloads now emit those elements back to clients. 【F:src/calendar.rs†L972-L1236】【F:src/sync.rs†L428-L528】
- RRULE-to-EAS projection now includes richer recurrence translation, including week-of-month and first-day-of-week handling. 【F:src/sync.rs†L318-L426】

**Impact:** recurring series, deleted occurrences, and modified instances now round-trip through the gateway rather than being collapsed to a master-event-only view.

---

### 3) Live deployment proof against real Cloudflare + Stalwart remains external to the repository

The repository now contains stronger Worker hardening and deployment guidance, but it still does not itself contain repeatable proof artifacts from a live Stalwart v0.15.5 + Cloudflare deployment.

**Current code evidence:**
- Cloudflare deployment model and hardening guidance are documented. 【F:CLOUDFLARE_DEPLOYMENT.md†L1-L122】
- Worker hardening exists in code. 【F:worker/index.js†L1-L218】【F:worker/index.js†L264-L404】

**Impact:** code-path closure is stronger than before, but production verification still depends on your live environment.

---

## Definition-of-done status for the specific use-case

### 1. Native account setup in Outlook Windows 11 and Outlook Android 15
- **Status:** **Partially implemented**
- Autodiscover plus EWS/EAS endpoints exist, but full interoperability still depends on the remaining semantics gaps listed above. 【F:worker/index.js†L318-L404】【F:src/main.rs†L38-L54】

### 2. Calendar create/update/delete/sync/meeting-response convergence
- **Status:** **Implemented in code**
- Durable create/update/delete paths now exist for EAS and EWS, MeetingResponse writes attendee response state through to CalDAV, and sync payload generation now includes richer property fidelity plus recurrence exceptions. 【F:src/sync.rs†L23-L528】【F:src/ews.rs†L800-L1115】【F:src/eas.rs†L623-L651】

### 3. Worker + D1 + tunnel + gateway deployment profile documented and endpoint-verifiable
- **Status:** **Implemented**
- The Cloudflare/Worker/D1/tunnel profile is documented. 【F:CLOUDFLARE_DEPLOYMENT.md†L1-L122】

### 4. Sync/provision/item state consistency across retries/restarts
- **Status:** **Partially implemented**
- D1-backed state and idempotency remain in place, and write-through now reaches CalDAV; however, broader protocol fidelity and concurrency proof are still not fully demonstrated. 【F:src/storage.rs†L59-L119】【F:worker/index.js†L84-L98】

### 5. TLS termination and request-shaping controls active and verified in production
- **Status:** **Partially implemented**
- Edge hardening controls exist in code and docs, but live production verification is still outside repository evidence. 【F:worker/index.js†L100-L218】【F:CLOUDFLARE_DEPLOYMENT.md†L81-L122】

### 6. Binder1 family traceability to code/tests remains current
- **Status:** **Partially implemented**
- The repository now better aligns code paths with the previous blocker list, but full Binder1 closure is still not established. 【F:tests/protocol_fixtures.rs†L1-L47】【F:src/calendar.rs†L413-L443】

---

## Bottom line

The two previously listed hard blockers are now closed in code:

1. **EAS write-through into Stalwart CalDAV is implemented**, and
2. **EWS write-through into Stalwart CalDAV is implemented**.

The remaining gaps are no longer calendar-model fidelity or recurrence/exception translation in code; they are now primarily about **live deployment proof** and broader **end-to-end production verification** outside repository-contained evidence.
