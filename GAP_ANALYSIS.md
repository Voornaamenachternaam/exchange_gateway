# GAP_ANALYSIS.md

## Scope and source of truth

This file compares the **current repository state** of `exchange_gateway` and its Cloudflare components against:

1. **`Binder1.txt`**, treated as the Microsoft Exchange protocol source of truth for this repository.
2. **Your specific use-case**: an existing **Stalwart Mailserver v0.15.5** container, using **Basic username/password authentication** over **IPv4 and IPv6**, exposed through **free Cloudflare services** and an existing **`cloudflared`** tunnel, with the goal of **native Outlook calendar support on Windows 11 and Android 15 without client-side extensions**.
3. Your required quality bar: **March 2026**, **security hardened**, **production-ready**, **fully robust**, **no stubs**, **no caveats**, and **fully compatible**.

This document is intentionally strict. The question it answers is:

> **What specific gaps still remain between the actual codebase and the required final Outlook/Stalwart/Cloudflare result?**

---

## Executive summary

The repository has materially improved compared with the earlier partial prototype. In particular, it now includes:

- real EAS `Sync` write handling for `Add` / `Change` / `Delete` into CalDAV;
- real EWS `CreateItem` / `UpdateItem` / `DeleteItem` write-through into CalDAV;
- D1-backed sync state persistence;
- deletion tombstones for incremental sync;
- a stricter `FolderSync` surface focused on the calendar folder;
- device-scoped EAS sync-state namespacing instead of a single shared mailbox-wide sync-key timeline;
- improved incremental `GetItemEstimate` behavior;
- per-command EAS mutation result reporting, Binder-aligned Ping heartbeat/max-folder/change-found behavior, and basic `ResolveRecipients` free/busy output that are richer than the previous minimal shells;
- durable device-scoped `ClientId` replay suppression for EAS `Sync/Add` retries via D1-backed command journaling rather than re-creating duplicate events;
- Binder-aligned validation that `Sync/Add` requests carry `ClientId`, and rejection of response-only ActiveSync calendar fields such as `AppointmentReplyTime` / `ResponseType` in client `Sync` writes;
- non-stub EAS `Settings`, `ItemOperations`, and `Search` responses that now return user/account metadata and real calendar-item payloads from CalDAV instead of empty success shells;
- richer recurrence/exception field handling than the original minimal implementation;
- ActiveSync recurrence responses now include `CalendarType` for the monthly/yearly recurrence shapes where Binder1 requires it in command responses;
- preserved `VTIMEZONE` blocks and richer EWS calendar-item response shapes than the previous revision;
- EWS `GetUserAvailability` merged free/busy output sourced from CalDAV event windows;
- EWS `GetUserAvailability` now also emits a concrete calendar-event array instead of only a merged bitmap;
- EWS `FindItem` now honors `CalendarView` date windows and returns full calendar-item shapes from current CalDAV data instead of subject-only synthetic rows;
- EWS `SyncFolderItems` now emits richer create/update calendar payloads from live CalDAV-backed item state instead of minimal subject/UID stubs;
- EWS `GetFolder` / `FindFolder` folder counts now reflect current calendar item totals rather than hard-coded zeroes;
- EWS availability responses now honor the requested free/busy view type instead of always replying as `MergedOnly`;
- EWS calendar item payloads now expose substantially richer Exchange-style metadata such as `CalendarItemType`, `MyResponseType`, `IsMeeting`, `IsOrganizer`, `IsRecurring`, `ReminderIsSet`, `DateTimeStamp`, `Duration`, `EndTimeZone`, and `DeletedOccurrences`;
- stricter EWS `ChangeKey` conflict detection for `UpdateItem` / `DeleteItem` plus consistent `ChangeKey` generation in create/update responses;
- Binder-aligned validation for common EWS operation attributes such as `SendMeetingInvitations`, `ConflictResolution`, `MessageDisposition`, `DeleteType`, and related meeting-cancellation knobs;
- derived `MeetingStatus` / `ResponseType` emission when the upstream item data does not already provide them explicitly;
- exception-level `AppointmentReplyTime` / `MeetingStatus` / `ResponseType` fields are now preserved through the local calendar model and emitted back out on Sync/ICS paths;
- a slightly less synthetic EWS folder shape by including the calendar folder’s parent linkage under `MsgFolderRoot`;
- richer Autodiscover XML / JSON / SOAP payloads that advertise EWS and ActiveSync endpoints more explicitly for Outlook bootstrap.
- a repo-contained live-environment smoke harness for the exact Cloudflare-published gateway surface (`ActiveSync OPTIONS`, `FolderSync`, invalid `SyncKey` handling, Autodiscover XML/SOAP/JSON, EWS folder/availability, and optional EWS create/update/delete).
- concrete example deployment templates for `cloudflared`, Worker/Wrangler, and the Rust gateway config that match the stated Stalwart + Cloudflare + Ubuntu host profile.

However, even after those improvements, the repository is **still not yet equivalent to a complete Exchange implementation** for the stated Outlook use-case.

### Direct status of the five gaps you explicitly asked to close

1. **EAS Sync protocol correctness is incomplete** → **partially reduced, not fully closed**.
2. **FolderSync / SyncKey / delta-state behavior is not Exchange-grade** → **partially reduced, not fully closed**.
3. **Recurring series, exceptions, and time-zone handling are still materially incomplete** → **partially reduced, not fully closed**.
4. **EWS support is still only a partial subset of what Outlook actually relies on** → **partially reduced, not fully closed**.
5. **Calendar-specific Exchange semantics are not fully modeled** → **still open**.

### Specific remaining gaps closed in this revision

The top-level gaps above are still only **partially** closed overall, but this revision does fully close at least the following previously-listed **specific remaining gaps**:

1. **EAS duplicate-submission / retry replay handling for `Sync/Add` `ClientId` values** is now durably implemented with worker-backed dedupe state, instead of re-creating duplicate CalDAV items on identical replay.
2. **EWS `ChangeKey` conflict validation** is now enforced for `UpdateItem` and `DeleteItem`, so stale item revisions are rejected with conflict semantics instead of being silently accepted.
3. **EWS response `ChangeKey` consistency** is now corrected so created/updated item payloads return gateway-computed change keys rather than mixing in raw CalDAV ETags.
4. **EWS free/busy support being absent outside EAS `ResolveRecipients`** is now closed by adding a real `GetUserAvailability` path that returns merged free/busy strings from CalDAV-backed calendar data.
5. **Autodiscover setting coverage being too narrow for Outlook bootstrap** is now materially expanded across JSON, XML, and SOAP responses with explicit EWS / ECP / OAB / MobileSync settings suitable for the stated Cloudflare-published gateway shape.
6. **Missing Binder-level validation around `Sync/Add` `ClientId` and response-only calendar fields** is now closed for the implemented request surface.
7. **MeetingResponse request validation being too permissive** is now narrowed by requiring `UserResponse`.
8. **Merged free/busy responses lacking concrete event windows** is now reduced by emitting a `CalendarEventArray` alongside `MergedFreeBusy`.
9. **Meeting-status / response-type emission depending entirely on upstream storage values** is now reduced by deriving Exchange-like values from organizer/attendee context when absent.
10. **The calendar folder shape lacking parent linkage under `MsgFolderRoot`** is now reduced by returning an explicit parent folder reference in EWS folder responses.
11. **Monthly/yearly ActiveSync recurrence responses omitting `CalendarType`** is now closed for the implemented recurrence response shapes.
12. **Exception-level meeting reply/status metadata being dropped from local round-tripping** is now reduced by preserving and re-emitting those fields in the calendar model.
13. **Common EWS operation attributes being accepted without any enum validation** is now reduced by validating supported values for `CreateItem`, `UpdateItem`, and `DeleteItem`.
14. **The repository lacking a reproducible live smoke package for the Cloudflare/Stalwart deployment surface** is now reduced by adding a scriptable smoke harness and runbook for the published gateway endpoints.
15. **The repository lacking concrete deployment templates for the exact `cloudflared` + Worker + gateway layout** is now reduced by shipping example config files for that topology.
16. **EAS `Settings` being an empty success stub rather than returning Binder-shaped user/account metadata** is now reduced by emitting concrete `UserInformation -> Accounts -> Account -> EmailAddresses` content.
17. **EAS `ItemOperations` being an empty shell instead of fetching actual calendar items** is now reduced by resolving requested `ServerId`/`LongId` values to CalDAV events and returning calendar properties.
18. **EAS `Search` being an empty shell instead of returning calendar hits** is now reduced by querying CalDAV over Binder-shaped date windows and returning real `Calendar` search results with range accounting.
19. **EAS `MeetingResponse` success payloads omitting the returned `CalendarId`** is now reduced by returning `CalendarId` alongside `RequestId` on successful meeting replies.
20. **EAS `MoveItems` falsely reporting success on a calendar-only mailbox surface** is now reduced by explicitly rejecting it rather than advertising a capability the gateway does not actually provide.
21. **EWS `FindItem` ignoring `CalendarView` windows and returning subject-only synthetic rows** is now reduced by querying current CalDAV items for the requested window and rendering full calendar item payloads.
22. **EWS `SyncFolderItems` create/update changes carrying only minimal subject/UID shells** is now reduced by emitting full calendar item XML for current items in sync changes.
23. **EWS `GetFolder` / `FindFolder` reporting hard-coded folder totals** is now reduced by deriving the calendar item count from current CalDAV-backed data.
24. **EWS `GetUserAvailability` always returning `MergedOnly` regardless of requested view type** is now reduced by reflecting the requested free/busy view mode in the response.
25. **EWS calendar item responses lacking common Exchange calendar metadata that Outlook often inspects** is now reduced by emitting `CalendarItemType`, `MyResponseType`, `IsMeeting`, `IsOrganizer`, `IsRecurring`, `ReminderIsSet`, `DateTimeStamp`, `Duration`, `EndTimeZone`, and `DeletedOccurrences`.

The main reason is not that the repository has no implementation. The reason is that **the implemented behavior still falls short of the exact protocol and client-compatibility standard required by Binder1.txt and by native Outlook behavior**.

---

## What has materially improved in the current repository

## 1) EAS sync/state improvements that are now present

The repository now does more than simply emit calendar data:

- client `Sync` mutations are parsed and written back to CalDAV;
- invalid sync-key handling is now explicit rather than silently ignored;
- sync responses can now return per-command mutation results instead of only an aggregate success/failure shape;
- sync state is persisted and used for incremental responses;
- EAS collection state is now scoped per device instead of sharing one sync-key slot across every client;
- delete tombstones are tracked so that incremental sync can emit deletions;
- `FolderSync` now exposes the calendar folder instead of a synthetic multi-workload folder set;
- `GetItemEstimate` now uses stored sync state instead of always returning a trivial success shell;
- `Ping` now validates heartbeat ranges / monitored-folder counts, caches heartbeat+folder state across subsequent requests, parses monitored folder `Id`/`Class` pairs, and can hold the request open until changes or heartbeat expiry instead of always responding immediately;
- `Settings` now returns concrete user/account metadata, `ItemOperations` can fetch real calendar items by `ServerId`/`LongId`, and `Search` can return actual calendar hits across query/range windows instead of empty placeholder payloads;
- calendar timezone blocks can now be preserved through ICS parsing/rendering instead of being discarded outright, and IANA `TZID` values are now parsed/rendered against real timezone rules rather than always being collapsed into UTC-only text.

**Impact:** the EAS layer is no longer just a full-resync calendar projection. It now behaves more like a real stateful sync pipeline.

## 2) EWS sync behavior has also improved

The repository already had CRUD handlers, but the current state is stronger because:

- deleted items now persist tombstones that can be surfaced in later incremental sync responses;
- `SyncFolderItems` can distinguish initial sync from later sync windows, emit deletion tombstones, page ordered journal windows with opaque sync-state cursors, respect `MaxChangesReturned` more accurately, and reject unsupported MIME-content requests more explicitly;
- item and folder identifiers are at least stable within the gateway’s own state model.

**Impact:** EWS is less stub-like than before, especially for sync continuity.

---

## Remaining gaps in detail

## 1) EAS Sync protocol correctness is still not fully closed

This gap is improved, but **not fully closed**.

### What is now materially better

- inbound `Add`, `Change`, and `Delete` exist and write to CalDAV;
- invalid sync keys are no longer silently accepted;
- incremental sync uses persisted sync state instead of always behaving like a first sync;
- delete tombstones are now tracked.

### Specific remaining gaps

1. **Per-command status fidelity is improved but still incomplete.**
   The gateway now returns richer per-command mutation results than before, enforces `ClientId` on `Sync/Add`, rejects some response-only request fields, and requires `UserResponse` for `MeetingResponse`, but it still does not implement the full Exchange-grade command-state machine and all status combinations expected by Binder-driven clients.

2. **Conflict semantics are still incomplete.**
   Binder1-driven sync correctness requires reliable handling of stale client state, partial failure, and all write-conflict cases. Duplicate `Sync/Add` retries keyed by `ClientId` are now durably suppressed, but the implementation still does not model every Exchange conflict branch end-to-end.

3. **Some server-generated sync semantics remain synthetic.**
   Sync keys, tombstones, and replay behavior are gateway-generated rather than equivalent to the richer Exchange state machine that Outlook clients are built against.

4. **Protocol-version-specific calendar behavior is still not fully modeled.**
   Binder1.txt describes version- and element-specific behavior, especially around exceptions and child elements. The current implementation is richer, now including `CalendarType` in implemented monthly/yearly recurrence responses and preserving more exception reply/status fields, but it is not yet exhaustive.

5. **No complete repository-contained proof exists for real Outlook write workflows.**
   The repository now includes a live smoke harness that can exercise EWS create/update/delete and the Cloudflare-published Autodiscover/EAS/EWS surface, but it still does not include genuine Outlook Windows 11 / Android 15 automation, retries, offline edits, or packet captures proving native-client behavior end-to-end.

### Conclusion

**Status:** **partially reduced, not fully closed**.

---

## 2) FolderSync / SyncKey / delta-state behavior is improved, but still not Exchange-grade

This gap is also improved, but **not fully closed**.

### What is now materially better

- `FolderSync` is now aligned to the actual single-calendar profile instead of advertising a synthetic multi-folder Exchange mailbox surface;
- sync-key validation now exists for both `Sync` and `FolderSync` paths;
- sync responses use stored state and delete tombstones instead of always behaving like full-resync snapshots.

### Specific remaining gaps

1. **Folder hierarchy remains synthetic.**
   Even though the surface is narrower and more accurate, and the EWS calendar folder now carries an explicit parent link under `MsgFolderRoot`, the repository still does not implement a complete Exchange folder model. It exposes a deliberately simplified calendar-only hierarchy.

2. **State journaling remains lighter than Exchange.**
   D1 state plus tombstones plus device-scoped sync keys is a meaningful improvement, but it is still not a full Exchange-grade journal of item lifecycle transitions, per-device progression, and recovery semantics.

3. **Sync-key recovery behavior is still simplified.**
   Invalid-key handling is present, but the recovery and replay model remains narrower than the complete Binder-defined Exchange behavior.

4. **No complete concurrency proof exists.**
   There is still no repository-contained proof for multi-device, out-of-order, retry-heavy sync behavior under production-like load.

### Conclusion

**Status:** **partially reduced, not fully closed**.

---

## 3) Recurring series, exceptions, and time-zone handling are still materially incomplete

This remains one of the most important remaining blockers.

### What is now materially better

- recurrence information is parsed and projected in both EAS and EWS paths;
- exceptions and deleted occurrences are modeled and emitted more explicitly than in the original implementation;
- incremental sync can now preserve and transmit recurring-item deletions more consistently.

### Specific remaining gaps

1. **Time-zone fidelity is still insufficient for a perfect Outlook claim.**
   The implementation still normalizes most date-time handling into UTC-centered internal state. That is useful operationally, but it is not equivalent to full Exchange time-zone fidelity.

2. **`VTIMEZONE` / Exchange time-zone equivalence is improved but still incomplete.**
   The gateway now preserves raw `VTIMEZONE` blocks more faithfully than before and can parse/render IANA `TZID` values against real timezone rules, but Binder1.txt still requires richer Exchange-specific time-zone behavior than the current implementation fully models end-to-end.

3. **All-day / recurrence / timezone edge-case behavior is not fully proven.**
   Binder1.txt contains strict rules for all-day events, recurrence elements, and timezone interactions. The code handles a useful subset but not the entire edge-case surface.

4. **Exception semantics remain partial.**
   The gateway supports exception payloads, deleted instances, and more exception reply/status metadata than before, but not the entire Exchange recurrence model such as richer range semantics and all instance-type distinctions.

5. **No proof exists for DST-sensitive Outlook scenarios.**
   There is still no repository-contained validation for recurring meetings spanning DST changes, cross-zone organizers/attendees, or historical zone transitions.

### Conclusion

**Status:** **partially reduced, not fully closed**.

---

## 4) EWS support is stronger, but still only a partial subset of what Outlook can rely on

This gap is improved, but **not fully closed**.

### What is now materially better

- EWS has real CRUD paths into CalDAV;
- `SyncFolderItems` uses persisted state rather than a pure placeholder model;
- deletion tombstones improve incremental behavior;
- `SyncFolderItems` now uses an opaque stored sync-state cursor and ordered journal-window pagination rather than an unbounded plaintext timestamp marker, including a bounded continuation window for `MaxChangesReturned`;
- `GetItem`, `CreateItem`, and `UpdateItem` responses now return much richer calendar item shapes than the previous subject-only responses, including much more Exchange-like meeting, recurrence, reminder, timezone, and deleted-occurrence metadata;
- `FindItem` now honors `CalendarView` windows and renders full calendar item XML from live CalDAV state, while `SyncFolderItems` now emits those richer payloads for create/update changes instead of minimal shells.

### Specific remaining gaps

1. **EWS schema coverage is still selective.**
   The implementation now returns richer calendar item XML for key item operations and validates several common operation attributes (`SendMeetingInvitations`, `ConflictResolution`, `MessageDisposition`, `DeleteType`, and related flags), but it still supports only a subset of the full property and update surface that Outlook can emit.

2. **Folder and mailbox modeling remain simplified.**
   The gateway now validates requested folder IDs more consistently across EWS operations and exposes a slightly less synthetic `MsgFolderRoot -> Calendar` shape, but it still exposes a deliberately narrow calendar-only mailbox model rather than the richer Exchange mailbox semantics Outlook often assumes.

3. **Sync fidelity is improved again but still not fully equivalent to Exchange.**
   `SyncFolderItems` now uses an opaque persisted cursor and an ordered journal window that respects `MaxChangesReturned` with bounded continuation behavior, which removes another earlier protocol mismatch. It is still a gateway-managed approximation rather than full Exchange item-state behavior.

4. **No real Outlook-for-Windows EWS proof is present in-repo.**
   The repository still does not contain end-to-end captures or regression artifacts showing successful native Outlook desktop operation through Autodiscover, EWS, and ongoing calendar sync.

### Conclusion

**Status:** **partially reduced, not fully closed**.

---

## 5) Calendar-specific Exchange semantics are still not fully modeled

This remains **open**.

### Specific remaining gaps

1. **Meeting workflow semantics are still simplified.**
   Organizer, attendee, meeting-status, and response-type fields are now surfaced more richly than before, including derived fallback meeting-status / response-type values, but full Exchange meeting workflow behavior is still only partially represented.

2. **Free/busy semantics are no longer completely absent, but they are still partial.**
   The gateway can now return `MergedFreeBusy` output through both EAS `ResolveRecipients` and EWS `GetUserAvailability`, but it still does not implement the full Exchange availability detail surface such as suggestions, detailed event arrays, or full attendee/organizer availability workflows.

3. **Change-key / identity semantics remain gateway-defined.**
   The gateway provides stable IDs and change material, but not a true Exchange item-identity and mutation model.

4. **The Binder1 calendar property universe is still broader than the current local model.**
   The repository models more fields than before, but not the entire Exchange calendar semantics surface implied by the Binder corpus.

5. **Native Outlook behavioral proof is still missing.**
   Even if individual request handlers exist, the repository still lacks the proof required to assert “fully and perfectly compatible” for Windows 11 Outlook and Android 15 Outlook.

### Conclusion

**Status:** **still open**.

---

## Additional remaining gaps outside the five requested areas

## 6) Autodiscover is richer, but still not fully Exchange-topology aware

The Worker now provides materially richer JSON, XML, and SOAP Autodiscover payloads, including explicit EWS / MobileSync / ECP / OAB-style settings aligned to the gateway endpoint shape. It is still synthetic rather than fully mailbox-topology aware, and the repository still lacks proof that all current Outlook bootstrap paths work reliably with the exact Cloudflare deployment model and the current gateway behavior.

## 7) Cloudflare operational proof is still incomplete

The repository now includes a concrete smoke-harness runbook and example deployment templates for the Cloudflare-published gateway surface, but it still does not contain a production-grade benchmark and compatibility package proving that the Worker, D1, tunnel, and Rust gateway remain reliable within the real request patterns of Outlook clients on the intended free-service footprint.

## 8) Security hardening is improved but still not “perfectly closed”

This analysis respects your stated use-case: **Basic authentication remains part of the design and is not treated as disqualifying by itself**. The remaining security gaps are instead around total deployment proof, operational hardening depth, malformed-input validation coverage, and end-to-end production verification. The Worker surface is now somewhat less permissive because the exposed control-plane responses use stricter no-store / nosniff / no-referrer / DENY response headers, but that does not by itself close the deployment-hardening gap.

## 9) Stalwart-specific proof remains incomplete

The gateway is clearly designed around Stalwart CalDAV, and the repository now includes a Stalwart/Cloudflare-targeted live smoke harness, but it still lacks a full in-repo proof package showing exact interoperability with Stalwart Mailserver v0.15.5 under the target Outlook client matrix.

---

## Final conclusion

The repository is **substantially stronger than the earlier partial prototype**, and some of the previously identified gaps are now **materially reduced**.

However, measured strictly against:

- **Binder1.txt**,
- your exact **Stalwart v0.15.5 + Cloudflare + `cloudflared` + Outlook Windows 11 / Android 15** use-case, and
- your required **March 2026 production-ready / no-caveats / fully compatible** standard,

there are still **specific remaining gaps**.

### Final status of the five required target gaps

1. **EAS Sync protocol correctness is incomplete** → **partially reduced, not fully closed**.
2. **FolderSync / SyncKey / delta-state behavior is not Exchange-grade** → **partially reduced, not fully closed**.
3. **Recurring series, exceptions, and time-zone handling are still materially incomplete** → **partially reduced, not fully closed**.
4. **EWS support is still only a partial subset of what Outlook actually relies on** → **partially reduced, not fully closed**.
5. **Calendar-specific Exchange semantics are not fully modeled** → **still open**.

So the honest current-state answer is:

> The codebase has moved forward and now closes part of the earlier gap set in implementation, but it still does **not yet** satisfy the standard of a perfectly complete, fully Outlook-compatible Exchange gateway for the stated Stalwart/Cloudflare use-case.
