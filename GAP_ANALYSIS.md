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
- richer recurrence/exception field handling than the original minimal implementation;
- preserved `VTIMEZONE` blocks and richer EWS calendar-item response shapes than the previous revision.

However, even after those improvements, the repository is **still not yet equivalent to a complete Exchange implementation** for the stated Outlook use-case.

### Direct status of the five gaps you explicitly asked to close

1. **EAS Sync protocol correctness is incomplete** → **partially reduced, not fully closed**.
2. **FolderSync / SyncKey / delta-state behavior is not Exchange-grade** → **partially reduced, not fully closed**.
3. **Recurring series, exceptions, and time-zone handling are still materially incomplete** → **partially reduced, not fully closed**.
4. **EWS support is still only a partial subset of what Outlook actually relies on** → **partially reduced, not fully closed**.
5. **Calendar-specific Exchange semantics are not fully modeled** → **still open**.

The main reason is not that the repository has no implementation. The reason is that **the implemented behavior still falls short of the exact protocol and client-compatibility standard required by Binder1.txt and by native Outlook behavior**.

---

## What has materially improved in the current repository

## 1) EAS sync/state improvements that are now present

The repository now does more than simply emit calendar data:

- client `Sync` mutations are parsed and written back to CalDAV;
- invalid sync-key handling is now explicit rather than silently ignored;
- sync state is persisted and used for incremental responses;
- EAS collection state is now scoped per device instead of sharing one sync-key slot across every client;
- delete tombstones are tracked so that incremental sync can emit deletions;
- `FolderSync` now exposes the calendar folder instead of a synthetic multi-workload folder set;
- `GetItemEstimate` now uses stored sync state instead of always returning a trivial success shell;
- calendar timezone blocks can now be preserved through ICS parsing/rendering instead of being discarded outright.

**Impact:** the EAS layer is no longer just a full-resync calendar projection. It now behaves more like a real stateful sync pipeline.

## 2) EWS sync behavior has also improved

The repository already had CRUD handlers, but the current state is stronger because:

- deleted items now persist tombstones that can be surfaced in later incremental sync responses;
- `SyncFolderItems` can distinguish initial sync from later sync windows and can emit deletion tombstones;
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

1. **Per-command status fidelity is still incomplete.**
   Exchange ActiveSync expects detailed command/collection/status semantics. The gateway still behaves more like a consolidated best-effort sync operation than a fully Exchange-grade command-state machine.

2. **Conflict semantics are still incomplete.**
   Binder1-driven sync correctness requires reliable handling of stale client state, duplicate submissions, retries, partial failure, and write conflicts. The current implementation improves validation, but it still does not implement a complete conflict-resolution model.

3. **Some server-generated sync semantics remain synthetic.**
   Sync keys, tombstones, and replay behavior are gateway-generated rather than equivalent to the richer Exchange state machine that Outlook clients are built against.

4. **Protocol-version-specific calendar behavior is still not fully modeled.**
   Binder1.txt describes version- and element-specific behavior, especially around exceptions and child elements. The current implementation is richer, but it is not yet exhaustive.

5. **No repository-contained proof exists for real Outlook write workflows.**
   There is still no in-repo interoperability evidence covering create/update/delete under real Outlook Windows 11 and Outlook Android 15 behavior, including retries and offline edits.

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
   Even though the surface is narrower and more accurate, the repository still does not implement a complete Exchange folder model. It exposes a deliberately simplified calendar-only hierarchy.

2. **State journaling remains lighter than Exchange.**
   D1 state plus tombstones plus device-scoped sync keys is a meaningful improvement, but it is still not a full Exchange-grade journal of item lifecycle transitions, per-device progression, and recovery semantics.

3. **Sync-key recovery behavior is still simplified.**
   Invalid-key handling is present, but the recovery and replay model remains narrower than the complete Binder-defined Exchange behavior.

4. **`Ping` remains minimal.**
   The repository still does not provide a full long-lived monitored-folder heartbeat implementation equivalent to Exchange server behavior across all real-client timing patterns.

5. **No complete concurrency proof exists.**
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
   The gateway now preserves raw `VTIMEZONE` blocks more faithfully than before, but Binder1.txt still requires richer Exchange-specific time-zone behavior than the current implementation fully models end-to-end.

3. **All-day / recurrence / timezone edge-case behavior is not fully proven.**
   Binder1.txt contains strict rules for all-day events, recurrence elements, and timezone interactions. The code handles a useful subset but not the entire edge-case surface.

4. **Exception semantics remain partial.**
   The gateway supports exception payloads and deleted instances, but not the entire Exchange recurrence model such as richer exception metadata, range semantics, and all instance-type distinctions.

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
- `GetItem`, `CreateItem`, and `UpdateItem` responses now return much richer calendar item shapes than the previous subject-only responses.

### Specific remaining gaps

1. **EWS schema coverage is still selective.**
   The implementation now returns richer calendar item XML for key item operations, but it still supports only a subset of the full property and update surface that Outlook can emit.

2. **Folder and mailbox modeling remain simplified.**
   The gateway exposes a deliberately narrow calendar-only mailbox model rather than the richer Exchange mailbox semantics Outlook often assumes.

3. **Sync fidelity is still not fully equivalent to Exchange.**
   `SyncFolderItems` is meaningfully better, but it is still a gateway-managed approximation rather than full Exchange item-state behavior.

4. **No real Outlook-for-Windows EWS proof is present in-repo.**
   The repository still does not contain end-to-end captures or regression artifacts showing successful native Outlook desktop operation through Autodiscover, EWS, and ongoing calendar sync.

### Conclusion

**Status:** **partially reduced, not fully closed**.

---

## 5) Calendar-specific Exchange semantics are still not fully modeled

This remains **open**.

### Specific remaining gaps

1. **Meeting workflow semantics are still simplified.**
   Organizer, attendee, meeting-status, response-type, and related fields are now surfaced more richly than before, but full Exchange meeting workflow behavior is still only partially represented.

2. **Free/busy semantics are still absent.**
   Outlook-native Exchange behavior often depends on availability semantics that are still not implemented here.

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

## 6) Autodiscover remains simplified

The Worker provides Autodiscover payloads, but the behavior is still synthetic rather than fully mailbox-topology aware. The repository still lacks proof that all current Outlook bootstrap paths work reliably with the exact Cloudflare deployment model and the current gateway behavior.

## 7) Cloudflare operational proof is still incomplete

The repository still does not contain a production-grade benchmark and compatibility package proving that the Worker, D1, tunnel, and Rust gateway remain reliable within the real request patterns of Outlook clients on the intended free-service footprint.

## 8) Security hardening is improved but still not “perfectly closed”

This analysis respects your stated use-case: **Basic authentication remains part of the design and is not treated as disqualifying by itself**. The remaining security gaps are instead around total deployment proof, operational hardening depth, malformed-input validation coverage, and end-to-end production verification.

## 9) Stalwart-specific proof remains incomplete

The gateway is clearly designed around Stalwart CalDAV, but the repository still lacks a full in-repo proof package showing exact interoperability with Stalwart Mailserver v0.15.5 under the target Outlook client matrix.

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
