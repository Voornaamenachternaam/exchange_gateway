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

---

## Specific remaining gaps

### 1) ActiveSync command advertisement still exceeds implemented semantics

`OPTIONS` still advertises a broader command surface than the repository currently implements with full durable semantics. Several branches still return generic success-shaped responses rather than protocol-complete behavior.  

**Current code evidence:**
- Advertised command list in `OPTIONS`. 【F:src/eas.rs†L246-L260】
- Generic success wrappers remain for `Ping`, `Settings`, `SendMail`, `SmartReply`, `SmartForward`, `ItemOperations`, `Search`, `MeetingResponse`, `ResolveRecipients`, `ValidateCert`, `GetItemEstimate`, and `MoveItems`. 【F:src/eas.rs†L574-L668】

**Why it remains a Binder1 gap:** `Binder1.txt` includes the corresponding ActiveSync command families and namespaces; placeholder success responses are not the same thing as protocol-complete implementations.

---

### 2) Meeting-response semantics remain incomplete

Although calendar create/update/delete now write through to CalDAV, `MeetingResponse` still does not translate accept / tentative / decline into attendee response semantics against the underlying calendar data.

**Current code evidence:**
- Shape-level validation exists. 【F:src/eas.rs†L77-L80】
- Runtime handling is still a generic success response. 【F:src/eas.rs†L623-L630】

**Why it remains a Binder1/use-case gap:** meeting workflow fidelity is part of your calendar-focused Outlook use-case and part of the ActiveSync family represented in `Binder1.txt`.

---

### 3) Calendar property fidelity remains partial versus Binder1 calendar surface

The repository now writes calendar items through both EAS and EWS into CalDAV, but the transformed calendar model still covers only a reduced subset of the full Binder1 calendar field surface.

**Currently modeled fields:**
- UID, subject, description/body, location, start, end, all-day, and a reduced RRULE mapping. 【F:src/calendar.rs†L8-L26】【F:src/calendar.rs†L176-L312】

**Examples of remaining fidelity risk areas from the Binder1 calendar families:**
- organizer / attendee fields,
- richer meeting state and response metadata,
- online meeting link fields,
- body truncation / ghosting interactions,
- proposal fields,
- richer recurrence / exception permutations.

**Impact:** some Outlook-originated calendar items can now be durably written, but can still lose protocol-level fidelity relative to the broader Binder1 surface.

---

### 4) Recurrence and exception coverage remains reduced

The repository now round-trips a subset of recurrence rules, but still does not implement the full set of recurrence and exception permutations represented by Binder1’s calendar-related documents.

**Current code evidence:**
- Reduced EAS recurrence decoding into RRULE. 【F:src/calendar.rs†L69-L165】
- Reduced RRULE-to-EAS recurrence projection. 【F:src/sync.rs†L163-L302】

**Impact:** recurring series and modified instances are improved relative to the prior state, but still not protocol-complete.

---

### 5) Live deployment proof against real Cloudflare + Stalwart remains external to the repository

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
- **Status:** **Partially implemented**
- Durable create/update/delete paths now exist for EAS and EWS, but meeting-response semantics and broader calendar fidelity remain incomplete. 【F:src/sync.rs†L23-L109】【F:src/ews.rs†L800-L1115】【F:src/eas.rs†L623-L630】

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

The remaining gaps are no longer the absence of durable write paths; they are now primarily about **protocol fidelity depth**, **meeting-response semantics**, **recurrence/exception completeness**, and **live deployment proof**.
