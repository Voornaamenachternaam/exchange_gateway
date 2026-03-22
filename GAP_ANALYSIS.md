# GAP_ANALYSIS.md

## Purpose and source-of-truth

This document replaces the previous gap analysis and is intentionally stricter.

It compares the **actual current repository state** of `exchange_gateway` and its Cloudflare components against all three of the following at the same time:

1. **Your specific use-case**: native Outlook calendar use on **Windows 11 Outlook** and **Android 15 Outlook**, with **no client-side extensions**, against an existing **Stalwart Mailserver v0.15.5** deployment.
2. **`Binder1.txt`** as the Microsoft Exchange protocol source of truth.
3. The target quality bar you explicitly set: **production-ready, security-hardened, fully robust, fully compatible, no stubs, no caveats, no compromise, current as of March 2026**.

This file therefore does **not** ask whether the repository is "better than before". It asks a narrower and much harder question:

> **What specific gaps still remain before this repository can honestly be described as a complete Exchange-compatible gateway for the stated Outlook/Stalwart/Cloudflare use-case?**

The answer is: **many critical gaps still remain**. Some earlier blockers are partially addressed in code, but the implementation is still far from the standard required by Binder1.txt and your stated definition of done.

---

## Executive summary

The current repository is best described as:

- a **calendar-focused prototype / partial gateway**;
- with **basic** EAS and EWS request handling;
- with **CalDAV read/write translation for a subset of event fields**;
- with **Cloudflare Worker + D1 helper services** for state and forwarding;
- but **not yet a complete, protocol-correct, production-ready Exchange facade** for Outlook.

### Bottom line

For your use-case, the repository is **not yet sufficient** to claim:

- full native Outlook Windows 11 compatibility;
- full native Outlook Android 15 compatibility;
- full Binder1.txt conformance for the relevant Exchange protocols;
- complete production hardening;
- complete recurrence/time-zone correctness;
- complete EAS sync semantics;
- complete EWS calendar semantics.

The most important unresolved gaps are in these areas:

1. **EAS Sync protocol correctness is incomplete.**
2. **FolderSync / SyncKey / delta-state behavior is not Exchange-grade.**
3. **Recurring series, exceptions, and time-zone handling are still materially incomplete.**
4. **EWS support is still only a partial subset of what Outlook actually relies on.**
5. **Calendar-specific Exchange semantics are not fully modeled.**
6. **Autodiscover and Outlook endpoint behavior are still simplified.**
7. **Cloudflare free-tier operational limits and state design are not fully resolved for production.**
8. **Security hardening is improved but still not complete enough to support the "perfect / no caveats" requirement.**
9. **There is no repository-contained proof of end-to-end compatibility with real Outlook clients and real Stalwart v0.15.5.**

---

## What is actually closed versus what is only partially closed

### Gaps that are partially closed in code

The repository **does now contain real code paths** for several items that used to be obviously missing:

- EAS `Sync` request parsing includes `Add`, `Change`, and `Delete` mutation parsing and attempts to write those changes back to CalDAV.
- EWS includes implementations for `FindItem`, `GetItem`, `SyncFolderItems`, `CreateItem`, `UpdateItem`, and `DeleteItem`.
- The shared calendar model includes more fields than a minimal subject/start/end projection.
- ICS parsing/rendering includes some recurrence, exception, attendee, reminder, and organizer handling.
- Worker/D1 state exists for sync keys, item mappings, and provision state.

These are important improvements.

### However: none of the requested “perfectly closed” protocol gaps are actually closed to the standard you asked for

The fact that a handler exists does **not** mean the Binder-defined protocol surface is complete, Outlook-compatible, or production-ready.

For the six gaps you explicitly required to be closed:

- **Write Sync (Add/Change/Delete)** → **partially closed only**.
- **Incomplete EAS Commands** → **not closed**.
- **Recurrence and Time Zones** → **not closed**.
- **FolderSync and Pings** → **not closed**.
- **Minimal EWS Support** → **partially closed only**.
- **Calendar-Specific EWS Features** → **not closed**.

The remainder of this file explains the precise reasons.

---

## Remaining gaps in detail

## 1) Exchange ActiveSync (EAS) gaps

### 1.1 Write Sync (Add / Change / Delete) is implemented only at a partial translation level

The current implementation can parse some inbound `Sync` mutations and convert them to CalDAV `PUT` / `DELETE`, but that is **not the same** as complete Exchange ActiveSync write-sync support.

#### Remaining gaps

1. **Conflict semantics are incomplete.**
   - Exchange ActiveSync expects well-defined sync conflict behavior, status handling, and client/server reconciliation semantics.
   - The current implementation writes directly to CalDAV and persists mappings, but it does not implement the full range of protocol-level conflict statuses, per-command statuses, or robust rollback/compensation behavior expected by clients.

2. **Change application is field-subset based, not protocol-complete.**
   - `Change` updates only the fields the local model knows how to parse and write.
   - Binder1.txt covers a much larger calendar property surface than the repository currently models.

3. **Collection semantics are oversimplified.**
   - Mutations are effectively written to the first discovered calendar collection.
   - For the specific use-case, this may be workable only if there is exactly one relevant calendar collection and the Outlook behavior never requires richer folder semantics.
   - That is not a safe production assumption for Exchange protocol emulation.

4. **Server status fidelity is incomplete.**
   - A complete EAS implementation must return command/collection/status values that accurately reflect each mutation result.
   - The current design is closer to “best-effort apply then respond” than full EAS state-machine fidelity.

5. **No repository-contained proof of Outlook write interoperability.**
   - There is no evidence in the repository of real Outlook Windows 11 or Outlook Android 15 test matrices showing successful create/update/delete, retries, conflicts, offline edits, or meeting update propagation.

#### Practical conclusion

This gap is **improved**, but **not fully closed** to the standard of “100% perfectly, completely, no caveats”.

---

### 1.2 Incomplete EAS command coverage remains a major gap

The repository only meaningfully supports a small subset of EAS commands.

#### What exists now

The command grammar and routing mention several commands, but the actually usable calendar profile is still narrow.

#### Remaining gaps

1. **`Settings` is not implemented as a complete Exchange settings surface.**
   - Grammar recognition exists, but the repository does not provide complete, Outlook-grade Settings semantics.

2. **`ItemOperations`, `Search`, `ResolveRecipients`, `MoveItems`, `ValidateCert`, `SendMail`, `SmartReply`, `SmartForward` are not implemented to production Exchange behavior.**
   - Recognition or validation of a command name is not equivalent to implementation.
   - For your use-case, not all of these must necessarily be supported at full depth, but Outlook compatibility often depends on more than just `Sync` and `FolderSync`.

3. **`GetItemEstimate` is superficial.**
   - It returns a success envelope, but there is no evidence of accurate estimate calculation tied to actual pending changes.

4. **Provisioning is minimal.**
   - The current implementation stores a policy key/state, but does not implement a realistic Exchange-grade device policy system.
   - If clients request security policy details, remote wipe semantics, or richer policy negotiation, the gateway is not equivalent to Exchange.

5. **No capability/version negotiation proof per real clients.**
   - Advertising support and satisfying actual Outlook/Android client behavior are different things.

#### Practical conclusion

This gap is **not closed**.

---

### 1.3 FolderSync is still not correct for the stated single-calendar use-case

Your use-case is specifically about a Stalwart calendar being exposed natively to Outlook. The current `FolderSync` behavior remains problematic.

#### Remaining gaps

1. **Folder list is hard-coded rather than derived from actual backend capabilities.**
   - The implementation returns a fixed folder payload including Calendar, Contacts, Tasks, Notes, and Documents.
   - That is not an accurate representation of the real backend if only calendar is actually supported.

2. **Folder identity/state semantics are oversimplified.**
   - Exchange clients expect syncable, evolving folder state.
   - Static synthetic folder identities are not sufficient for a protocol-perfect implementation.

3. **Misleading capability surface.**
   - Advertising folders such as Contacts and Tasks without implementing those workloads creates protocol inconsistency.

4. **No robust folder hierarchy model.**
   - There is no true mapping between Exchange folder identifiers and Stalwart/CalDAV resources beyond a simplified approach.

#### Practical conclusion

This gap is **not closed**.

---

### 1.4 SyncKey / delta sync behavior is still not Exchange-grade

A production-ready Exchange-style sync implementation requires strict, durable, replay-safe sync state handling.

#### Remaining gaps

1. **Sync state is too simple for full EAS correctness.**
   - D1 stores sync keys, but the repository does not implement a full, protocol-robust sync state machine with complete invalid-key handling, replay semantics, and per-collection change tracking fidelity.

2. **Change tracking is timestamp-centric and mapping-centric, not protocol-complete.**
   - The Worker tracks updates using timestamps and item mappings.
   - That is not equivalent to full Exchange delta semantics, especially under concurrent edits, retries, and multi-device sync races.

3. **No full tombstone/change journal model.**
   - Robust incremental sync requires durable change journaling, not just current-row state and update timestamps.

4. **No demonstrated sync reset and resync correctness.**
   - Outlook clients can and do force recovery flows.
   - There is no repository-contained proof that invalid sync keys, state corruption, partial replay, or out-of-order retries are handled exactly as clients expect.

#### Practical conclusion

This gap is **not closed**.

---

### 1.5 Ping support is minimal and likely insufficient for long-lived client expectations

The repository returns a simple successful Ping-style response.

#### Remaining gaps

1. **No evidence of full folder-monitoring semantics.**
   - Proper EAS Ping behavior is tied to monitored folders, heartbeat intervals, and change detection semantics.

2. **No proof of client-observed long-poll compatibility through Cloudflare.**
   - Outlook/mobile behavior behind Cloudflare edge/tunnel/worker timeouts needs explicit validation.

3. **No robust heartbeat policy logic.**
   - Real EAS clients adapt heartbeat values; the server side must behave predictably.

#### Practical conclusion

This gap is **not closed**.

---

### 1.6 Recurrence and time-zone support remains materially incomplete

This is one of the most important remaining issues for Outlook interoperability.

#### What the code does now

The repository does parse and render:

- `RRULE`
- `EXDATE`
- `RECURRENCE-ID`
- some attendee/exception data
- a custom `X-EAS-TIMEZONE` field

That is useful, but it is not enough.

#### Remaining gaps

1. **Time-zone handling is mostly normalized to UTC rather than fully modeled.**
   - The internal calendar item stores `start` and `end` as UTC datetimes.
   - `parse_datetime` converts non-UTC forms into UTC without preserving full original zone rules.
   - Outlook calendar correctness often depends on preserving time-zone identity and recurrence interpretation in local zone context.

2. **No full `VTIMEZONE` round-trip fidelity.**
   - Binder1.txt includes rich Exchange time-zone semantics and mappings.
   - The current implementation does not preserve a full `VTIMEZONE` block or perform complete bidirectional conversion between Exchange zone concepts and iCalendar zone definitions.

3. **The custom `X-EAS-TIMEZONE` approach is not enough.**
   - A private field can help carry some context, but it is not equivalent to fully correct Exchange/EAS/EWS time-zone semantics.

4. **Recurrence translation is partial.**
   - The repository supports a subset of RRULE/EAS recurrence mappings, but not the complete recurrence model and edge cases required for Outlook-grade behavior.

5. **Exception handling is partial.**
   - Modified exceptions and deleted instances are represented, but not all Exchange recurrence exception semantics are modeled.
   - Range operations such as “this and future” semantics are not fully represented.

6. **No complete orphan-instance / instance-type model.**
   - Binder1.txt describes recurring masters, instances, exceptions, and related semantics.
   - The repository’s shared calendar model is much simpler than the Exchange recurrence object model.

7. **DST-sensitive behavior is not proven.**
   - There is no repository-contained proof for recurring events spanning daylight-saving transitions, historical zone changes, or cross-zone organizer/attendee scenarios.

#### Practical conclusion

This gap is **not closed** and remains a high-risk blocker for “perfect Outlook compatibility”.

---

## 2) Exchange Web Services (EWS) gaps

### 2.1 EWS is no longer “minimal”, but it is still far from complete Outlook-grade EWS calendar support

The repository now has more than just a `GetFolder` stub, but it is still a partial implementation.

#### What exists now

Handlers exist for:

- `GetFolder`
- `FindFolder`
- `FindItem`
- `GetItem`
- `SyncFolderItems`
- `CreateItem`
- `UpdateItem`
- `DeleteItem`
- `ResolveNames`

#### Remaining gaps

1. **Schema coverage is still selective and simplified.**
   - The parser/renderer recognizes only a subset of EWS calendar fields and update forms.
   - Exchange clients can emit richer SOAP shapes than the repository currently handles.

2. **Folder model is still synthetic.**
   - `GetFolder` / `FindFolder` behavior is not backed by a complete Exchange folder hierarchy or mailbox model.

3. **`SyncFolderItems` is partial.**
   - True Exchange sync state semantics are more detailed than a simple stored sync token with item enumeration.

4. **Response fidelity is incomplete.**
   - EWS clients rely on specific response classes, item shapes, identifiers, and property sets.
   - The implementation focuses on a useful subset, not full fidelity.

5. **No proof of Outlook-for-Windows real-world EWS interoperability.**
   - Outlook desktop behavior is extremely sensitive to EWS details.
   - There is no repository-contained client verification proving this works as a native Outlook account end-to-end.

#### Practical conclusion

This gap is **partially closed only**.

---

### 2.2 Calendar-specific EWS semantics remain incomplete

This is distinct from merely having CRUD handlers.

#### Remaining gaps

1. **Meeting workflow semantics are incomplete.**
   - Exchange calendar behavior includes meeting requests, responses, organizer/attendee semantics, response objects, and related status transitions.
   - The repository updates attendee response state in a simplified manner, but that is not a full Exchange meeting workflow implementation.

2. **Free/busy semantics are absent.**
   - Outlook commonly depends on Exchange availability semantics.
   - There is no complete free/busy implementation here.

3. **Reminder semantics are partial.**
   - A simple reminder offset does not equal complete Exchange reminder behavior.

4. **Recurrence exception semantics are incomplete in EWS shape terms.**
   - Exchange represents recurring masters, occurrences, exceptions, and associated identifiers with richer semantics than the repository currently models.

5. **Server-generated metadata and item identity semantics are simplified.**
   - Exchange item IDs, change keys, and related update semantics are richer than the current server-id + mapping approach.

6. **No support for the broader calendar property universe expected by Outlook.**
   - The current local model still excludes many Exchange calendar properties and behaviors defined across the Binder corpus.

#### Practical conclusion

This gap is **not closed**.

---

## 3) Autodiscover and Outlook account bootstrap gaps

Autodiscover exists in the Worker, but it remains a simplified implementation.

### Remaining gaps

1. **Responses are synthetic rather than mailbox-topology aware.**
   - The Worker emits static-looking XML/JSON/SOAP payloads.
   - Outlook behavior can depend on more than just the presence of an endpoint URL.

2. **No proof of complete Outlook autodiscover negotiation.**
   - There is no repository-contained capture showing full account bootstrap succeeds on current Outlook Windows 11 and Android 15 without client workarounds.

3. **Protocol advertisements may exceed real backend capability.**
   - If autodiscover advertises EWS/EAS routes as Exchange-like but the backend semantics remain partial, Outlook can configure successfully and still fail functionally later.

4. **Realm / challenge / auth discovery behavior is not exhaustively demonstrated.**
   - Binder1.txt includes Basic-auth related realm challenge behavior.
   - The repository does not provide full proof that these flows match Outlook expectations in all bootstrap scenarios.

### Practical conclusion

This gap is **not closed**.

---

## 4) Cloudflare architecture and production operations gaps

Your use-case specifically requires free Cloudflare services plus a Rust container beside Stalwart and a `cloudflared` tunnel. The repository still leaves important production gaps.

### 4.1 Worker runtime and request-model risk remains

#### Remaining gaps

1. **No demonstrated margin against Worker runtime/resource constraints.**
   - The code path includes SOAP/WBXML/XML processing, request validation, forwarding, and D1 lookups.
   - There is no repository-contained benchmark suite proving the target workloads remain safe and reliable on the intended Cloudflare plan profile.

2. **Long-lived or large sync response behavior is not validated.**
   - Calendar sync responses can grow nontrivially with recurring events and exceptions.

3. **No replayable load test evidence.**
   - A production-ready statement requires measured behavior under concurrent Outlook clients, retries, and bursts.

### 4.2 D1-backed state remains too lightweight for Exchange-grade sync durability

#### Remaining gaps

1. **D1 schema is mapping/state oriented, not a full event journal.**
   - This is useful, but still too thin for full Exchange sync semantics under all failure and concurrency conditions.

2. **No complete migration / recovery / backup / corruption handling story in repository code.**
   - Production readiness requires more than a schema file.

3. **No demonstrated resilience under eventual consistency / transient failure patterns.**
   - Exchange-style sync state machines are sensitive to precisely this class of issue.

### 4.3 Tunnel/origin deployment proof remains external

#### Remaining gaps

1. **No repository-contained end-to-end deployment validation with actual `cloudflared` + Stalwart v0.15.5.**
2. **No formal verification of IPv4 + IPv6 behavior for all relevant paths.**
3. **No repository-contained validation that origin exposure is sufficiently locked down in the exact target deployment.**

### Practical conclusion

Cloudflare support is **usable as infrastructure scaffolding**, but **not yet proven production-complete** for the target claim.

---

## 5) Security gaps relative to your stated “security hardened” requirement

You explicitly require a hardened production system. Relative to that requirement, important gaps remain even though Basic authentication is acceptable in your specific use-case.

### Important note about Basic auth in this use-case

Per your instruction, this analysis does **not** treat Exchange Online deprecation as a source of truth for your design because your target is **not Exchange Online**. Also, you explicitly state that **Stalwart v0.15.5 uses Basic username/password authentication and that this will not change**.

Therefore, the correct standard here is not “remove Basic because Exchange Online did.”

The correct standard is:

> **If Basic authentication remains part of the use-case, the gateway must harden everything around it and implement the protocol correctly with Basic over properly protected transport.**

### Remaining security gaps

1. **Origin transport hardening is not fully enforced by repository code alone.**
   - The Worker accepts `http` or `https` origin URLs.
   - For the stated “perfectly security hardened” target, this should be locked down more strictly or at least proven safe in the exact deployment.

2. **Request signing / trust boundary design is still relatively simple.**
   - The Worker-to-gateway trust model uses a shared secret and forwarded requests.
   - This can be acceptable, but the repository does not yet provide a full hardening story covering rotation, replay boundaries, and deployment misconfiguration prevention.

3. **No full audit/security test suite is included.**
   - There is no repository-contained penetration-style validation for malformed SOAP/WBXML/XML, oversized payloads, authentication edge cases, or state-tampering attempts.

4. **No complete secrets lifecycle management inside the repository.**
   - The docs mention secret handling patterns, but the repository itself does not enforce end-to-end operational security posture.

5. **No complete abuse-handling posture proven for exposed Exchange-like endpoints.**
   - Rate limiting exists in the Worker, which is good.
   - That still does not prove robust protection against all practical abuse patterns for EWS/EAS endpoints.

### Practical conclusion

Security is **improved**, but **not yet at the “fully security hardened, no caveats” level**.

---

## 6) Stalwart integration gaps

Your use-case is specifically tied to **Stalwart Mailserver v0.15.5**.

### Remaining gaps

1. **The repository assumes a simplified CalDAV collection model.**
   - It discovers calendars by probing a predictable path and then uses the first calendar collection.
   - That is not a complete Stalwart integration layer.

2. **No repository-contained proof for all relevant Stalwart calendar behaviors.**
   - Recurrence, exceptions, attendee state, ETag behavior, PUT preconditions, and collection layouts need explicit verification against Stalwart v0.15.5.

3. **Config-level Stalwart requirements are not fully codified.**
   - The request mentions that Stalwart `config.toml` may need modifications.
   - The repository does not yet provide a definitive, proven, end-to-end Stalwart configuration profile guaranteeing the required Exchange-like behavior.

### Practical conclusion

This gap is **not closed**.

---

## 7) Testing and verification gaps

This is one of the largest remaining blockers to your required standard.

### Remaining gaps

1. **No complete protocol conformance test suite tied to Binder1.txt.**
   - There are unit-level tests and parser checks, but not a comprehensive Binder-derived compatibility suite.

2. **No full client interoperability matrix in-repo.**
   - Missing: Outlook Windows 11 native account setup, Outlook Android 15 native account setup, create/update/delete, recurring meetings, meeting responses, reminders, offline changes, conflict resolution, and resync behavior.

3. **No production-style fault-injection tests.**
   - Missing: Worker failure, D1 failure, network retries, stale ETags, duplicate submissions, out-of-order syncs, tunnel interruptions, and large recurrence sets.

4. **No performance qualification artifacts.**
   - Missing: latency, throughput, memory, Worker CPU/runtime, and D1 usage evidence.

### Practical conclusion

Without this verification, the repository cannot honestly claim “fully and perfectly compatible” for the use-case.

---

## 8) Specific status of the gaps you explicitly required to be closed

Below is the direct answer to your instruction to “perfectly close” the following gaps.

### 8.1 Write Sync (Add/Change/Delete)
- **Status:** **Partially closed only**.
- **Why not fully closed:** translation exists, but full EAS sync semantics, conflict handling, status fidelity, and proven Outlook interoperability are still missing.

### 8.2 Incomplete EAS Commands
- **Status:** **Not closed**.
- **Why not fully closed:** command surface remains partial, and some commands are recognized more than they are fully implemented.

### 8.3 Recurrence and Time Zones
- **Status:** **Not closed**.
- **Why not fully closed:** recurrence support is partial, exception semantics are incomplete, and time-zone fidelity is insufficient for a “perfect Outlook” claim.

### 8.4 FolderSync and Pings
- **Status:** **Not closed**.
- **Why not fully closed:** FolderSync is synthetic and Ping behavior is minimal.

### 8.5 Minimal EWS Support
- **Status:** **Partially closed only**.
- **Why not fully closed:** CRUD/read sync handlers exist, but the EWS surface remains simplified and not fully Outlook-grade.

### 8.6 Calendar-Specific EWS Features
- **Status:** **Not closed**.
- **Why not fully closed:** full meeting workflow, free/busy, recurrence exception semantics, and broader calendar property fidelity remain incomplete.

---

## 9) What would still need to be true before this repository could satisfy the requested standard

The following would still need to be delivered and verified before the repository could reasonably claim to meet your requirement.

### Protocol completeness

1. A much more complete EAS state machine:
   - robust `Sync` state handling;
   - accurate per-command/per-collection statuses;
   - complete delta/tombstone logic;
   - correct invalid-sync-key recovery;
   - Outlook-proven `FolderSync`, `Ping`, `GetItemEstimate`, and provisioning behavior.

2. A much more complete EWS calendar implementation:
   - richer property set coverage;
   - better item identity/change-key semantics;
   - complete recurring-series/exception handling in EWS shapes;
   - meeting-response / meeting-object fidelity;
   - Outlook-proven behavior for desktop account setup and ongoing sync.

3. Stronger recurrence/time-zone correctness:
   - preserved time-zone identity;
   - robust `VTIMEZONE` / Exchange zone mapping;
   - DST-safe recurrence behavior;
   - exception and range semantics such as “this and future” where relevant.

### Infrastructure and operations

4. Exchange-grade sync durability on Cloudflare state services.
5. A fully proven `cloudflared` + Worker + gateway + Stalwart deployment guide.
6. Benchmarks and operational SLO evidence for realistic Outlook workloads.

### Security and hardening

7. Stronger transport and deployment lock-down.
8. Expanded abuse, malformed-input, and state-tampering testing.
9. Complete operational secret rotation and recovery guidance.

### Proof

10. A repository-contained validation package showing successful real-client operation with:
    - Outlook on Windows 11;
    - Outlook on Android 15;
    - Stalwart v0.15.5;
    - IPv4 and IPv6;
    - Cloudflare free-service deployment constraints.

---

## Final conclusion

The current repository is **not yet** a complete answer to your stated goal.

It does contain meaningful progress beyond a trivial prototype:

- EAS writes exist in some form.
- EWS CRUD/read/sync handlers exist in some form.
- Calendar modeling is richer than before.
- Worker/D1 support is present.

But measured against **Binder1.txt**, **your exact Outlook/Stalwart/Cloudflare use-case**, and your required March 2026 standard of **perfect, complete, production-ready compatibility with no caveats**, the repository still has **substantial remaining gaps**.

### Most important conclusion

The previously cited gaps are **not actually all closed**.

In particular, the following are **still open** at a material level:

- **Write Sync (Add/Change/Delete)** — partially closed only.
- **Incomplete EAS Commands** — open.
- **Recurrence and Time Zones** — open.
- **FolderSync and Pings** — open.
- **Minimal EWS Support** — partially closed only.
- **Calendar-Specific EWS Features** — open.

Therefore, the repository should **not** currently be represented as delivering a fully complete Exchange-compatible native Outlook calendar solution for the specified Stalwart v0.15.5 + Cloudflare use-case.
