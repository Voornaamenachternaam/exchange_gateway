# AUDIT5 — Exhaustive codebase + deployment audit for the target use-case

Scope: the containerized `exchange_gateway` must let the **calendar and email**
of an existing **Stalwart Mailserver v1.1.0** account be used **fully,
natively** by **New Outlook for Windows (October 7, 2026 build)** and
**Outlook Android (5.2641.1)** via the native "Exchange" account flow, with
the clients talking only to the gateway and Stalwart acting only as backend.
JMAP (as implemented by Stalwart v1.1.0) must be used instead of any other
standard/protocol wherever feasible.

The gateway runs alongside Stalwart in a container on an Ubuntu Server host
that also runs `cloudflared` (TLS terminated at Cloudflare edge). The gateway
**serves plain HTTP** on port 8134; cloudflared forwards `https://calendar.<domain>`
and `https://autodiscover.<domain>` to `http://localhost:8134`.

No `.env` file may exist; every value lives inline in `docker-compose.yml`.
`example.com` hostnames are placeholders and must never be deployed as-is.

This audit disregards all prior audit files (AUDIT.md, AUDIT2.MD, AUDIT3.MD,
AUDIT4.MD) and re-derives every gap from the current source, compose,
Dockerfile, and docs.

Fix priority ladder (applied per item): 1) existing dependencies → 2) new
high-quality dependencies → 3) Rust 1.99.1 std → 4) custom code.

Issues are numbered **P0** (blocks the use-case) → **P1** (degrades the
use-case) → **P2** (robustness / friction), listed top to bottom in priority
order.

---

## P0 — Blocks the use-case (must be resolved before deployment)

### 1. Stalwart version mismatch: code/docs validated against v1.0.x, operator runs v1.1.0

The source, AGENTS.md, DEPLOY_RUNBOOK.md, and CLOUDFLARED_SETUP.md all
reference Stalwart Mailserver **v1.0.x / v1.0.3** (JMAP Calendars at
`urn:ietf:params:jmap:calendars`, CardDAV at `/dav/card/{user}/`, no JMAP
Task/Note types). The operator runs **v1.1.0-container**. Stalwart v1.1.0 may
have changed:
- JMAP Calendar capabilities / property names (e.g. `showWithoutTime` vs
  `isAllDay`, `duration` vs `end`, `locations` vs `location`,
  `recurrenceRule` vs `recurrenceRules` — already corrected in `CALENDAR_EVENT_PROPERTIES`
  but must be re-verified against v1.1.0).
- The `iCalendar` object shape returned by `CalendarEvent/get` (currently a
  JSCalendar object, reconstructed via `calcard`).
- JMAP Contacts (RFC 9610) availability and behaviour.
- CalDAV/CardDAV endpoint paths.

**Fix:** pin the gateway's validated Stalwart version contract (JMAP
calendars/contacts/availability, CalDAV VTODO/VJOURNAL, CardDAV) against
Stalwart v1.1.0 by live-probing a v1.1.0 instance, then update AGENTS.md,
DEPLOY_RUNBOOK.md, CLOUDFLARED_SETUP.md, and the compose comments to state
v1.1.0 as the validated backend. Do NOT change the gateway image tag — the
operator pulls it from their registry.

### 2. EAS folder-management commands return failure statuses — DELIVERED

**Original gap.** `FolderCreate`, `FolderDelete`, `FolderUpdate` returned
Status 6 ("An error occurred on the server") — they were stubs with no
persisted folder store and no JMAP `Mailbox/set` or CalDAV collection mutation
path. `GetAttachment` returned Status 2 / HTTP 500 (no attachment fetch).
`GetHierarchy` returned a hard-coded fixed system-folder list only (no
user-created folders).

**Delivered.** A new module `src/eas_folders.rs` implements the three
folder-mutation commands against Stalwart as the system of record, plus a
persisted `folder_map` store (schema **v11**) so user-created folders survive
gateway restarts and surface in `FolderSync`/`GetHierarchy`.

- **Backend selection** ([MS-ASCMD] §2.2.3.186.2 user-created Types):
  Type 12/1 (mail) → JMAP `Mailbox/set`; Type 13 (Calendar) → CalDAV
  `MKCALENDAR` with `VEVENT`. Types 14–17 are **not creatable** and
  `FolderCreate` rejects them up front with Status 10: Type 14 (Contacts) lives
  on CardDAV (collection creation is unsupported), and Types 15/16/17
  (Tasks/Journal/Notes) map to the gateway's single *gateway-local*
  Tasks/Notes collections, which EAS Sync reaches by content class rather than
  by the folder's mapping — a custom folder of those types would sync the wrong
  collection and write the client's mutations to the default one.
- **`FolderCreate`** (§2.2.1.3): validates the request (missing DisplayName/
  ParentId → 10; a reserved *default* Type 2–11/19 → 10; a DisplayName that
  collides with a system folder or an existing sibling → 2; DisplayName > 64
  chars → 10). On success the JMAP mailbox id (or the CalDAV collection href)
  becomes the EAS ServerId, is persisted in `folder_map`, and the response is
  `<Status>1</Status><SyncKey>…</SyncKey><ServerId>…</ServerId>` (§6.10).
- **`FolderDelete`** (§2.2.1.4): a system-folder ServerId → 3 ("The specified
  folder is a special system folder"); an unknown ServerId → 4; otherwise the
  backing mailbox (`Mailbox/set destroy`) or CalDAV collection (`DELETE`) is
  removed first, then the `folder_map` row (and every descendant row) is
  dropped. Response is `<Status>` + `<SyncKey>`.
- **MAPI mirror consistency.** A mailbox an EAS client created (so it has a
  `folder_map` row keyed on its JMAP mailbox id) that is later destroyed through
  MAPI — `RopDeleteFolder` or `RopHardDeleteMessagesAndSubfolders` — has its
  `folder_map` row pruned and a `folder-delete` journaled for every mailbox the
  ROP actually destroyed, so EAS `FolderSync`/`GetHierarchy` stop advertising it
  and already-primed devices receive its Delete. `delete_folders_journaled`
  journals only ids that had a mapping row, so a system folder (whose EAS
  ServerId is never a JMAP mailbox id) never fabricates a Delete.
- **EWS mirror consistency.** The same rule holds for the EWS front door: an
  EWS `DeleteFolder` with `DeleteType="HardDelete"` destroys the target JMAP
  mailbox and its whole descendant subtree bottom-up, then prunes the
  `folder_map` row of every mailbox it actually destroyed and journals a
  `folder-delete` for each in one transaction, so a folder an EAS client
  created and an EWS client deleted is no longer advertised by EAS
  `FolderSync`/`GetHierarchy` and already-primed EAS devices receive its Delete.
  A failed mirror write is NOT acknowledged as success (the mailbox is already
  gone, so a stale `folder_map` row would keep EAS advertising a deleted
  folder); it surfaces as a retryable `ErrorInternalServerError`, and the
  destroy is idempotent so a retry re-runs the prune. The mirror runs on EVERY
  exit path, including a walk that aborts part-way (a mid-subtree backend
  refusal or transport error): the subtree is destroyed deepest-first, so an
  abort leaves the already-destroyed descendants gone from JMAP, and pruning
  only on the full-success path would strand their `folder_map` rows (EAS keeps
  advertising them) while a retry — re-querying the now-childless hierarchy —
  could never recover the descendant ids. The destroyed-so-far set is pruned
  before the walk's own failure is surfaced.
- **Deferred folder-map prune work list (durable recovery).** A folder destroy
  whose backend object is already gone but whose `folder_map` prune fails (a DB
  error, or a transport failure after the destroy committed) is not recoverable
  by a retry: the destroyed mailbox is no longer returned by JMAP, and for a deep
  subtree only the ids destroyed so far are known. Every destroy path — EAS
  `FolderDelete`, EWS `DeleteFolder` (including a walk that aborts part-way),
  MAPI `RopDeleteFolder` and `RopHardDeleteMessagesAndSubfolders` — therefore
  records the destroyed ids in `pending_folder_prune` (schema **v13**) BEFORE
  pruning, then prunes in the same request; a successful prune clears the pending
  rows (the prune itself is a no-op for an unmapped id, so a system folder never
  fabricates a Delete). Any row still present is replayed by the next
  folder-sync read — EAS `FolderSync` (before the journal head is captured, so
  the `folder-delete` it journals lands inside that same window and is delivered
  immediately) and `GetHierarchy`, plus EWS `SyncFolderHierarchy` — which re-runs
  the prune and journals the `folder-delete`, so the client receives the Delete
  instead of EAS advertising a folder that no longer exists. A replay failure is
  non-fatal: the pending row survives for the next read.
- **`FolderUpdate`** (§2.2.1.6): rename and/or move. A system folder → 2; an
  unknown folder → 4; otherwise JMAP `Mailbox/set update` (patch `name` and/or
  `parentId`) or a CalDAV `PROPPATCH` `<D:displayname>`; the CalDAV collection
  href (ServerId) is kept stable across a rename.
- **`GetHierarchy`** (§2.2.1.8) now lists user-created folders after the fixed
  system set, in the §6.19 `<Folders>/<Folder>` shape.
- **`FolderSync`** (§2.2.1.5): a prime (`SyncKey 0`) lists the system set plus
  every persisted user-created folder; an incremental call reports the
  folder-hierarchy changes journaled since the client's watermark
  (`change_journal` rows with `resource_href = 'folder'`), so a
  FolderCreate/Update/Delete mutation reaches an already-primed client as an
  `<Add>`/`<Update>`/`<Delete>` rather than a stale `<Count>0</Count>`. A
  successful folder mutation rotates the mutating device's SyncKey while
  **preserving its own acknowledged watermark** (the stored `seq:` token is
  carried onto the new key): the watermark is never advanced to the global
  journal head, which would permanently skip a change another device journaled
  below that head before this device had seen it.
- **`GetAttachment`** (§2.2.1.7): the file reference (from the
  `AttachmentName` command parameter or an `AttachmentName`/`FileReference`
  body element) is streamed through `JmapClient::download_blob_range` and
  returned as the response BODY with the blob's `Content-Type`; a missing name
  → Status 2, a missing/empty attachment → the spec's HTTP 500.
- **`Find`** (§2.2.1.2, removed in 14.0): kept as the legacy compatibility
  shape — GAL criteria route to the GAL search path and mailbox criteria to
  the mailbox search path, rendered in the `Find:` envelope. 14.0+ clients use
  `Search` instead.
- **Email delta sync scoping (correctness).** The EAS email delta
  (`handle_email_delta_sync`) and the EWS `SyncFolderItems` email delta now
  drive the **mailbox-scoped** `Email/queryChanges` feed (RFC 8620 §5.6,
  `filter.inMailbox`) instead of the account-wide `Email/changes`. The latter
  reports changes across *every* mailbox, so a single EAS/EWS collection would
  receive `<Add>`/`<Delete>` for messages belonging to other folders —
  corruption on the wire and spurious Ping wakes. `accumulate_email_changes`
  resolves the collection's concrete JMAP mailbox id (`resolve_email_mailbox_id`:
  a user folder's EAS ServerId *is* the mailbox id; a system role resolves via
  its JMAP role) and pages `queryChanges` until the feed drains. An id that
  Stalwart reports in **both** `added` and `removed` (an in-place reorder — a
  flag edit re-sorting a keyword-sorted query) is classified as a single `<Change>`,
  not an Add+Delete (`classify_query_change_ops`). The initial sync now stores
  the **first window's query state** (`Email/query` `queryState`) as the delta
  base and keeps it across continuation pages rather than replacing it with
  each page's state: a base taken from the first window makes the delta
  re-report anything that arrived mid-prime (a duplicate Add the client
  resolves by ServerId), whereas a base taken from a later window would
  silently skip a message that arrived during paging and was never delivered. A state the backend can no longer compute — Stalwart returns
  `invalidArguments` ("invalid JMAP State"), not `cannotCalculateChanges` — is
  mapped to the typed `CannotCalculateChanges`, so the delta forces a clean
  re-prime (EAS SyncKey 0 / Status 3; EWS `ErrorInvalidSyncStateData`) instead
  of freezing the feed. The Ping email-folder probe uses the same
  mailbox-scoped feed. The push monitor keeps the account-wide `Email/changes`
  deliberately — it must observe every mailbox to wake the right folder.
  A continuation page pins its window on the **last delivered message id**
  (RFC 8620 §5.5 `anchor` + `anchorOffset: 1`, "start immediately after the
  anchor") instead of a numeric `position`: a message deleted before the next
  request shrinks the live `receivedAt`-ordered result set, so a stored numeric
  offset would skip an unchanged message — and, because that message was
  already covered by the first window's query state, the delta would never
  re-report it (permanently absent from the client). A vanished anchor surfaces
  as `anchorNotFound` and falls back to the numeric position **rebased by one**:
  the stored offset is the index PAST the previous window, so the anchor — the
  last message that window delivered — occupied the slot `offset - 1`. When that
  message is deleted everything after it shifts up by one, so the first
  UNDELIVERED message now sits at `offset - 1`; resuming at the unchanged offset
  would skip it — and, being already inside the first window's query state, no
  later delta would re-report it. Resuming at `offset - 1` re-delivers that slot
  (the now-deleted anchor's position, i.e. the first undelivered message) and the
  client resolves the duplicate Add by ServerId. The clipped-prime
  resume decision uses the response's **authoritative `position`** (RFC 8620
  §5.5 `EmailListResult::query_position`), not a locally accumulated offset, for
  the same reason. A `get_email_sync_cursor` READ failure on the initial-prime
  continuation path fails closed (retryable) rather than being flattened to "no
  cursor" (which would restart the walk from position 0 and re-derive every
  page); a failed cursor WRITE after a clipped window likewise fails closed, so
  the next Sync cannot route a not-yet-delivered tail to the delta from the
  first page's state.

- **Atomic delta-queue drain.** When a persisted email-delta overflow queue is
  fully delivered within the EAS window, dropping the queue AND advancing the
  stored (SyncKey, token) to the queue's base state happen in ONE SQLite
  transaction (`Storage::drain_email_sync_queue`). A cleared queue with an
  unadvanced key would make the next delta recompute the whole change set and
  replay already-delivered commands; an advanced key with a surviving queue
  would re-deliver the same ops — either ordering surfaces duplicate commands.
  The drain also CLAIMS the queue via the `DELETE`'s `rows_affected`: two
  overlapping Sync requests can both read and render the same queued
  operations, and without a claim the second `DELETE` would remove zero rows yet
  still advance the token and acknowledge a window it did not own. Exactly one
  caller observes `rows_affected == 1` (returns `true`); every later caller gets
  `Ok(false)` with the stored token untouched. A failed drain (or a claim loss)
  is not acknowledged: the caller answers a retryable Status 6 and the
  transaction rolls back, so the next Sync retries the same drain — or, after a
  claim loss, finds the queue gone and diffs from the winner's advanced token
  instead of replaying.

- **Class-omitted folder routing.** When the client omits the
  protocol-optional `<Class>` ([MS-ASCMD] §2.2.3.30), the persisted `folder_map`
  row's `backend_kind` disambiguates a user-created folder: `"jmap"` routes to
  the Email path (a user mail folder), `"caldav"` routes to the Calendar path (a
  user-created calendar-family folder, Type 13). Without the CalDAV inference a
  custom calendar folder would fall through to the unsupported-collection
  Status 4 and could never sync. A SYSTEM collection (a system email id, `1`,
  `8`, or the Tasks/Notes ids) is routed by its id alone and never consults
  `folder_map`. For a user-created folder the mapping is read ONCE and reused;
  a transient READ failure is NOT flattened to "no mapping" (which would answer
  Status 4 and never sync the folder) — the collection fails closed with a
  retryable Status 6 so the next Sync re-reads the mapping, matching the
  class-specific handlers' own fail-closed resolution.

- **Email property (flag) updates.** `Email/queryChanges` reports only
  membership of the query result set (RFC 8620 §5.6), so a message whose flags
  change without leaving the mailbox (mark read/flagged, ...) never appears in
  its `added`/`removed` lists and would otherwise be dropped by both the EAS
  and EWS email deltas. Both deltas now keep a per-collection **keyword
  baseline** (`email_keyword_baseline`, one row per JMAP Email id holding its
  sorted keyword list) captured at the start of every initial sync — the
  snapshot is taken before the first window is fetched, so a flag edit during
  pagination is not folded into the baseline as already-synchronized and is
  reported as a `<Change>` once the rotating scan window reaches that message
  (the scan is bounded, so a large mailbox may take several sweeps to reach it);
  the capture is best-effort (a backend hiccup leaves the previous baseline, and
  the first delta then captures it without reporting the intervening edits). The
  delta then diffs the live mailbox against this baseline, emitting a `<Change>`
  for each id whose keyword set changed and was not already queued. A delivered
  `<Add>`/`<Create>` seeds its own baseline row at delivery time, so a flag edit
  before the rotating scan reaches the message is still detected (a missing row
  on the scan's first observation is recorded without a `<Change>` — the Add
  already carries the current flags). The EWS email delta runs the SAME bounded
  scan and persists the SAME baseline/position (keyed on the EWS folder id), so
  a flag edit made by any client on the mailbox (an EAS device, another EWS
  client, or the web UI) surfaces to an EWS client as a `<t:Update>`; without it
  the membership-only feed would leave EWS flag state stale forever. The diff is
  **bounded per delta**: the scan walks at most a fixed number of pages
  (`KEYWORD_SCAN_MAX_PAGES`) from a per-collection rotating position persisted
  in `email_keyword_scan` (wrapping to the top at the end), so a delta never
  pays O(mailbox) in JMAP calls — every message present for a whole sweep is
  re-examined once per sweep, with the sweep's cost amortized across deltas. The
  observed baseline rows, the rows for messages the feed reported as having left
  the mailbox (they can never be observed by a later scan page, so they are
  pruned rather than leaking), and the next position are persisted only after the
  whole change set is delivered, so a crash re-scans the same window rather than
  swallowing an update; the rotating position advances only when every baseline
  row was written, so a failed write holds the cursor and the next sweep
  re-examines the same window instead of skipping the change; an overflowed delta
  (the change set exceeds the EAS window) persists them ONLY once the remainder
  is durably queued AND the best-effort sync-token write has committed — if
  persisting the queue fails the delta FAILS CLOSED: it
  returns a retryable error (the caller answers Sync Status 6 with the incoming
  key and no commands) and leaves the stored token and the baseline untouched,
  rather than acknowledging the clipped window while the stored token still sits
  at the pre-delta state (which would make the next delta re-diff and replay the
  already-delivered commands as duplicate Adds/Deletes). If the queue persist
  succeeds but the token write fails, the window is still acknowledged (Status 1
  + `<MoreAvailable/>`; the queue carries progress) but the SCAN-DERIVED baseline
  and rotating position are HELD — the stored token stays at the pre-delta
  state, so advancing the scan baseline would let a retry's `Email/queryChanges`
  re-derive the membership set without re-reporting a flag-only `<Change>` (the
  feed reports membership only), losing the edit. A delivered `<Add>`/`<Create>`
  seeds its own baseline row eagerly in `render_pending_email_op` (BEFORE the queue
  and token writes), so a failed token write can leave that per-item seed row
  behind; only the scan's snapshot rows and rotating position are held. The drain
  and every
  later delta do not re-queue the same flag change. A backend hiccup
  during the scan degrades to serving the query-feed changes without persisting
  that page's rows.
  `Email/queryChanges` has no `hasMoreChanges` field (RFC 8620 §5.6), so
  `has_more_changes` is a heuristic — set when a page returns exactly at the
  `maxChanges` cap — and the drain loop plus the EWS
  `IncludesLastItemInRange` reflect a *possibly* clipped page (one extra page
  is fetched to confirm the feed drained) rather than a fixed `false`.
  The EWS baseline is keyed on the EWS folder id, NOT on the opaque `SyncState`:
  EWS SyncFolderItems carries no device/stream identifier on the wire (unlike EAS,
  whose `Sync` is scoped per device), so a second concurrent client on the same
  mailbox is not distinguishable. This matches the gateway's pre-existing EWS
  SyncState model -- one persisted cursor per folder -- and is correct for the
  supported single-EWS-client-per-mailbox deployment; the flag baseline is no more
  shared than the cursor it advances. A full initial prime also captures the whole
  mailbox's keyword baseline inside the request (unbounded page count), reducing
  but not eliminating the chance that a flag edit made during capture is folded
  into the baseline as already-synchronized before the first window observes it;
  this one-time cost is the deliberate price of flag-edit fidelity, and a
  mid-prime continuation keeps the baseline the first page captured.

**Status codes.** Per [MS-ASCMD] §2.2.3.177.8 / §6.10–§6.19: 1 success,
2 invalid/duplicate or special system folder, 3 special system folder
(delete), 4 not found, 5 invalid parent, 6 server error, 10 malformed request.

**Tests.** The `src/eas_folders.rs` test module (classification helpers, response
rendering + a WBXML round-trip of the FolderHierarchy-rooted responses, and
end-to-end create/delete/update against a combined JMAP+CalDAV stub asserting
the exact `Mailbox/set`/`MKCALENDAR`/`DELETE`/`PROPPATCH` wire calls and the
persisted `folder_map` rows, plus the hierarchy-change journal watermark;
`create_rejects_custom_task_note_folder_types` pins the Type 14–17 rejection).
The `eas::tests::test_folder_sync_create_then_rename_reaches_new_device_as_add`
test drives a real `handle_folder_sync` prime + delta to pin the
create-then-rename compaction (a folder added and then renamed before a second
device's first incremental FolderSync reaches it as an `<Add>` with the latest
DisplayName, never an `<Update>` for a folder it never received), and
`eas::tests::test_folder_mutation_rotation_preserves_unseen_earlier_change`
pins the rotation watermark preservation (a change journaled by another device
before a mutation is still delivered to the mutating device after its key
rotates). The email-delta scoping adds
`eas::tests::test_classify_query_change_ops_maps_add_remove_and_in_place`
(pure classification) and
`eas::tests::test_email_delta_sync_uses_mailbox_scoped_query_changes` (an
end-to-end delta against a JMAP stub asserting the wire call carries
`filter.inMailbox`, never `Email/changes`, and renders Add/Delete), plus three
`jmap::tests::query_email_changes_*` tests (mailbox scoping + classification,
`cannotCalculateChanges` and Stalwart `invalidArguments` both downcasting to
the typed `CannotCalculateChanges`).
`eas::tests::test_email_delta_overflow_queue_persist_failure_fails_closed` pins
the fail-closed overflow acknowledgement (a poisoned queue write makes the
delta return an error with the stored token and queue untouched, then drain
cleanly once storage recovers).
`eas::tests::test_email_delta_queue_drain_is_atomic_and_fails_closed` pins the
atomic drain (a poisoned `sync_state` write during the drain makes the delta
return an error with the queue and stored token both untouched, so the next
Sync re-drains rather than replaying or losing the window);
`eas::tests::test_email_delta_queue_drain_claims_and_loser_does_not_advance`
pins the queue claim (the first `drain_email_sync_queue` returns `true` and
advances the token; a second returns `false` and leaves the winner's key/token
untouched); and
`eas::tests::test_email_initial_sync_fails_closed_when_cursor_reread_fails`
pins the fail-closed initial-prime cursor re-read.
`eas::tests::test_email_initial_sync_anchor_not_found_falls_back_to_position`
pins the rebased `anchorNotFound` fallback (the fallback `Email/query` carries
`position` one less than the stored post-window offset and no anchor).
`sync::tests::calendar_add_fails_closed_when_folder_lookup_fails` pins that a
transient `folder_map` read failure during a calendar Add mutation fails the
mutation closed (no CalDAV PUT reaches the backend), and
`eas::tests::test_sync_collection_class_omitted_caldav_folder_routes_to_calendar`
pins the Class-omitted CalDAV-folder routing to the Calendar path (never the
unsupported-collection Status 4).
`eas::tests::test_sync_collection_class_omitted_folder_read_failure_fails_closed`
pins the Class-omitted routing fail-closed on a `folder_map` read failure
(Status 6, never Status 4), and
`eas::tests::test_sync_collection_class_omitted_system_folder_ignores_folder_map`
pins that a system collection routes by id alone (unaffected by a `folder_map`
read failure).
`eas::tests::test_get_hierarchy_fails_closed_when_folder_list_read_fails`
pins the GetHierarchy HTTP 500 on a folder-list read failure. `mapi::handler` gains
`rop_delete_folder_prunes_eas_folder_map_and_journals` and
`rop_delete_folder_without_mapping_row_journals_nothing`, pinning the
EAS-mirror consistency of a MAPI mailbox destroy, plus
`rop_delete_folder_mirror_write_failure_is_disk_error` (the post-commit
hardening below); `ews_folder_ops::tests` gains
`hard_delete_prunes_eas_folder_map_and_journals`,
`hard_delete_without_mapping_row_journals_nothing` and
`hard_delete_mirror_write_failure_is_internal_error`, pinning the same
EAS-mirror consistency for the EWS `DeleteFolder` hard-delete path, plus
`hard_delete_partial_failure_prunes_destroyed_descendants`, pinning that a
walk aborted mid-subtree still prunes the descendants it already destroyed.
The hierarchy-rotation tests
`rotate_hierarchy_key_for_never_primed_device_returns_prime_key`,
`rotate_hierarchy_key_preserves_primed_watermark` and
`rotate_hierarchy_key_without_token_forces_prime` pin the fail-closed rotation;
`create_calendar_folder_405_on_preexisting_collection_is_conflict` and
`create_calendar_folder_405_with_existing_mapping_is_conflict` pin the
no-adoption-on-405 rule, `create_calendar_folder_rejects_sanitized_system_collection_name`
pins the reserved-collection guard, and
`create_mail_folder_rolls_back_mailbox_when_mapping_write_fails` /
`create_caldav_folder_rolls_back_collection_when_mapping_write_fails` pin the
create compensation; and the sync suite gains
`scoped_calendar_delta_does_not_tombstone_foreign_destroyed_event`,
`scoped_calendar_prime_does_not_tombstone_foreign_event` and
`unscoped_calendar_delta_tombstones_destroyed_event` pinning the scoped-folder
tombstone guard (and its deliberate absence for the default folder).
The post-commit hardening (j)–(l) adds
`eas_folders::tests::create_rejects_unrecognised_folder_types` +
`creatable_folder_types_match_spec` (out-of-range Type rejected with Status 10,
no backend call, no persisted row) and
`eas::tests::test_folder_create_rejects_missing_or_malformed_type` (missing/
non-numeric `<Type>` → Status 10, no row), plus three fail-closed delta tests:
`eas::tests::test_email_sync_mailbox_resolution_failure_preserves_token`
(a `Mailbox/query` 500 makes the Sync return an error with the stored query
state untouched),
`eas::tests::test_email_delta_fetch_error_fails_closed_not_vanished` (an
`Email/get` 500 for a queued Add fails the delta with token + queue untouched)
and `eas::tests::test_email_delta_add_seed_failure_fails_closed` (a poisoned
baseline seed fails the delta, then the same delta delivers the Add and seeds
it once storage recovers).

The round-5 PR-review hardening (m)–(n) adds
`eas::tests::test_email_delta_token_write_failure_does_not_advance_baseline`
(a poisoned `sync_state` write fails the delta with the stored token AND the
flag baseline both untouched, then the same delta commits the token and
persists the baseline once storage recovers) and
`ews_folder_ops::tests::update_folder_rename_mirrors_eas_folder_map_and_journals`
(an EWS rename mirrors the new DisplayName into `folder_map` and journals a
`folder-update`) plus
`ews_folder_ops::tests::update_folder_rename_without_mapping_row_journals_nothing`
(a rename of a mailbox with no `folder_map` row succeeds but journals nothing).

The round-6 PR-review hardening (o)-(s) adds
`eas::tests::test_email_keyword_scan_defers_excess_changes_and_holds_position`
(a caller-supplied change budget caps the flag scan's `<Change>` emission, holds
the rotating position at the page start and marks the scan deferred while only
the emitted id's baseline advances);
`ews::tests::sync_email_folder_items_defers_flag_changes_beyond_max_changes`
(an EWS delta with a quiet membership feed and three changed flags delivers one
`<t:Update>` under `MaxChangesReturned=1`, reports
`IncludesLastItemInRange=false`, advances the SyncState but leaves the deferred
ids' baselines stale);
`ews::tests::sync_email_folder_items_fails_closed_when_state_write_fails` (a
failed `set_ews_sync_state` fails the delta with `ErrorServerBusy` and leaves the
flag baseline untouched);
`ews_folder_ops::tests::move_folder_mirrors_parent_into_eas_folder_map` (an EWS
`MoveFolder` writes the new EAS ParentId into `folder_map` and journals a
`folder-update` without touching the DisplayName); and
`storage::tests::update_folder_journaled_is_targeted_and_never_resurrects` (the
targeted helper updates only the named columns, journals exactly one
`folder-update`, and is a no-op -- no write, no journal row -- for a vanished
mapping).

**Operator change list.** None required for Cloudflare or Stalwart: the new
commands use the already-configured JMAP and CalDAV endpoints. No new
environment variables.

**PR-review hardening.** (a) Folder mutations journal ATOMICALLY with the
mapping: `Storage::upsert_folder_journaled` (folder-add/folder-update) and
`delete_folders_journaled` write the `folder_map` row(s) AND the
`change_journal` row(s) in one transaction, so a FolderCreate/Update/Delete is
acknowledged only once an already-primed device can learn about it (the previous
separate `record_hierarchy_change` calls could ack a mutation whose journal row
was never written). (b) MAPI `RopHardDeleteMessagesAndSubfolders`
([MS-OXCFOLD] §2.2.1.10) removes the folder's messages AND its whole descendant
subtree (the folder itself survives), walking it deepest-first with
`onDestroyRemoveEmails` set on every child destroy. (c) The rotating keyword
scan HOLDS its position (does not advance past the window) when a baseline row
read fails — `Storage::get_email_keyword` propagates a decode error as `Err`
instead of treating it as a missing row, so a flag change is never silently
swallowed — and the per-delta scan's upserts SKIP ids delivered as an `<Add>`
this delta, so the Add's fresh seed wins over the older scan snapshot (no
duplicate flag `<Change>` on the next delta).

**PR-review hardening (post-commit).** (d) A `FolderCreate` whose
`MKCALENDAR` answers 405 is reported as Status 2 and NEVER adopted: the gateway
cannot tell an orphan left by a failed mapping write from a collection the user
created outside EAS, so adopting would silently link a new EAS folder to a
collection whose events it does not own. Orphans are instead prevented up front
by the create-compensation below. A display name that sanitizes to a system
collection (`Tasks.`/`.Tasks` → `Tasks`, `Notes.` → `Notes`) is rejected before
`MKCALENDAR`, so a later `FolderDelete` can never destroy the shared
Tasks/Notes store (`is_reserved_collection_name`). (e) A MAPI mirror-write
failure after a JMAP mailbox destroy is no longer acknowledged as success:
`RopDeleteFolder` and `RopHardDeleteMessagesAndSubfolders` surface a retryable
`DiskError` when `delete_folders_journaled` fails, because the mailbox is
already gone and a stale `folder_map` row would keep EAS advertising a deleted
folder and leave primed devices without its Delete (the destroy is idempotent,
so a retry finds the mailbox already gone and re-runs the prune). (f) Hierarchy
key rotation is fail-closed: a `get_sync_key` READ error is logged and returns
the current key (never `seq:0`, which would clobber the device's real
watermark and skip every earlier change), and a device with NO acknowledged
watermark — no stored state, or a stored key with a NULL token — receives the
prime key `"0"` so its next `FolderSync` takes the full-hierarchy branch
instead of rendering only journal rows and omitting every default folder. (g)
The calendar `item_map` is owner-global (no collection column), so a SCOPED
(custom-calendar) folder must not infer deletions from the account-wide
`CalendarEvent/changes` feed nor from its own single-calendar snapshot: an id
destroyed in another calendar would otherwise emit a `<Delete>` in this folder
for an event the client never held AND remove the shared row before the event's
own collection could deliver it. The delta path only records destroyed ids for
the unscoped (default) folder, and the full-snapshot tombstone sweep is skipped
entirely for scoped folders. (h) A `FolderCreate` whose backend object was
created but whose mapping write then fails is COMPENSATED, not left as an
orphan: the just-created JMAP mailbox (`Mailbox/set destroy`, contents included)
or CalDAV collection (`DELETE`) is rolled back, so a retry starts clean instead
of being rejected as a duplicate sibling name (JMAP) or 405ing on an unmappable
orphan (CalDAV, which no longer adopts). A failed compensation is logged and the
client still receives the retryable Status 6. (i) `GetHierarchy` fails CLOSED
(HTTP 500, [MS-ASCMD] §2.2.1.8) when the user-created folder list cannot be
read, instead of returning a hierarchy that silently omits those folders.
(j) A `FolderCreate` whose `<Type>` is missing, non-numeric or outside the
spec's creatable set (1/12/13) is rejected as malformed (Status 10) WITHOUT
touching the backend. `<Type>` is a REQUIRED `unsignedByte` child
([MS-ASCMD] §2.2.3.186.2); the old handler defaulted an absent value to 12 and
`backend_for_type` sent every other numeric value to JMAP, so a malformed or
out-of-range Type silently created a mail mailbox the client never asked for
(`is_creatable_folder_type` now gates the create). (k) The email delta's
mailbox-role resolution is fail-closed: a transient `Mailbox/query` failure
propagates as Status 6 with the stored query state untouched, instead of
flattening to "no mailbox" — which would clear the token and answer an empty
success window, losing every change made during the outage until a full
re-prime. A role that genuinely has no mailbox is `Ok(empty)` and still scopes
to nothing. (l) `render_pending_email_op` is fail-closed: a transient
`Email/get` error for a queued created/changed op, or a failed keyword-baseline
seed for a delivered `<Add>`, now returns an error so the caller does NOT
acknowledge the window or consume the op (no token advance, no overflow queue,
no baseline write) — a later Sync re-derives and re-delivers the same op. The
old code turned a fetch error into an empty render (treated as a vanished item)
and only logged a failed seed, silently dropping an email change the client
never received and leaving its flags stale. (m) The delta's **sync-token write
is fail-closed** in the branch that completes a window with no overflow queue:
a failed `set_sync_key` now returns a retryable error instead of being logged
and ignored, and the flag-scan baseline is persisted ONLY after the token write
commits. Persisting the baseline while the stored token still sat at the
pre-delta state would let a later `Email/queryChanges` re-derive the membership
set but NOT re-report a flag-only `<Change>` (the feed reports membership only),
so a flag edit the client never received would be lost forever; the branch holds
no queue, so a retry re-derives the whole change set from the unchanged token.
The clipped-window branch keeps its token write best-effort (it stores the SAME
`old_state`, so a failed write changes nothing; the durable overflow queue
carries progress) but now HOLDS the flag-scan baseline unless that token write
committed: the stored token stays at the pre-delta state, so advancing the
baseline would let a retry's `Email/queryChanges` re-derive the membership set
but NOT re-report a flag-only `<Change>` (the feed reports membership only),
losing the edit; the window is still acknowledged (Status 1 +
`<MoreAvailable/>`) because the queue carries progress and failing the delta
there would let a retry drain only the queued remainder while dropping this
window's delivered ops. The baseline persist itself is deliberately NOT
fail-closed: the
partially-written rows would otherwise blind the retry's scan to the very change
it must re-report, and the held scan position already guarantees a re-scan.
(n) An EWS `UpdateFolder` rename of an EAS-created mail folder (a `folder_map`
row whose ServerId is the JMAP mailbox id) is mirrored into `folder_map` and
journaled as `folder-update` in one transaction, so EAS `GetHierarchy` stops
advertising the old DisplayName and already-primed devices receive the change; a
mailbox with no `folder_map` row (one EAS never advertised) is a no-op — the
mirror never fabricates an EAS-visible folder. The rename is held to the SAME
name rules EAS `FolderUpdate` enforces ([MS-ASCMD] §2.2.1.6) BEFORE the backend
is touched — a reserved system-folder name, a name over the 64-character bound,
or a case-insensitive sibling collision is rejected — so the EAS-visible
hierarchy can never advertise a name EAS itself would refuse. `update_mailbox`'s
boolean is honoured: the mirror runs only on `Ok(true)`; `Ok(false)` (the id in
neither `updated` nor `notUpdated`) is a retryable `ErrorInternalServerError`
rather than mirroring a name the backend never applied. A failed mirror write is
NOT acknowledged as success (retryable `ErrorInternalServerError`), because the
JMAP mailbox is already renamed and a stale DisplayName would keep EAS
advertising the old name; the rename is idempotent, so a retry re-applies it and
re-runs the mirror.

**Upgrade note (expected one-time re-prime).** Before this change the stored
email delta token was the `Email/get` data-type state; the new delta passes it
to `Email/queryChanges` as `sinceQueryState`, which Stalwart rejects with
`invalidArguments` (mapped to `CannotCalculateChanges`). Every already-synced
email collection therefore answers EAS `Sync` with Status 3 (SyncKey 0) and
EWS `SyncFolderItems` with `ErrorInvalidSyncStateData` exactly once, then
re-downloads its mailbox from a clean prime. This is self-healing and needs no
operator action, but the first Sync after the upgrade is heavier than steady
state.


### 3. Autodiscover hostname pairing is only a startup warning, not enforced

`validate_host_pairing` (src/config.rs:1262) emits `tracing::warn!` for
placeholder/mismatched `GATEWAY_HOST` / `GATEWAY_MAIL_DOMAIN` /
`GATEWAY_MAIL_HOST`, but startup continues. If the operator forgets to
replace `example.com` placeholders, Autodiscover advertises unresolvable URLs
(`https://calendar.example.com/…`, `https://mail.example.com/…`) and the
Android "Exchange" wizard fails with an opaque error.

**Fix (ladder 1):** make `validate_host_pairing` fatal at startup when any of
the three hostnames is a placeholder or fails the pairing invariant
(`GATEWAY_HOST == calendar.<GATEWAY_MAIL_DOMAIN>`, autodiscover derivable,
`GATEWAY_MAIL_HOST == mail.<GATEWAY_MAIL_DOMAIN>` or a non-internal MX host),
with a clear actionable error naming the offending variable. Allow `localhost`
only for local dev (detect via bind/host).

### 4. `GATEWAY_MAIL_HOST` leaks the internal bridge name into Autodiscover IMAP/SMTP blocks

The compose ships `GATEWAY_MAIL_HOST=mail.example.com` (correct: advertised
to clients) but `GATEWAY_SMTP_HOST=stalwart` / `GATEWAY_IMAP_HOST=stalwart`
(internal bridge name, correct for backend plumbing). If an operator copies
the template and sets `GATEWAY_SMTP_HOST=mail.example.com` (the public MX),
SMTP TLS verification fails because the certificate is for the public name
but the connection is to the internal container. Conversely, if
`GATEWAY_MAIL_HOST` is left as `stalwart`, IMAP/SMTP-capable clients are
pointed at an unreachable host.

**Fix (ladder 1):** add a startup validation that `GATEWAY_SMTP_HOST` and
`GATEWAY_IMAP_HOST` are internal/bridge/localhost/RFC1918 addresses (never a
publicly resolvable MX host), while `GATEWAY_MAIL_HOST` must be a publicly
resolvable name matching the MX record. Reject mismatches at startup with a
clear error.

### 5. Gateway serves plain HTTP only — no TLS listener; `x-forwarded-proto` enforcement inconsistent

`serve_plain` (src/main.rs) binds a single `TcpListener` and serves HTTP.
Cloudflare terminates TLS at the edge and forwards plain HTTP. The gateway
must not serve HTTPS directly (mutually exclusive with cloudflared on the
same port). However `x-forwarded-proto` enforcement is inconsistent across
surfaces — some paths trust the header, others do not, which can cause
mixed-content or redirect loops behind Cloudflare.

**Fix (ladder 1):** centralize `x-forwarded-proto` / `CF-RAY` handling in one
middleware layer (axum `FromRequest` or tower middleware) so every surface
(EAS, EWS, Autodiscover, MAPI, OAB, ECP, health) consistently treats
Cloudflare-proxied requests as trusted HTTPS origins and answers with
`https://`-based URLs in Autodiscover/EWS responses. Keep the single HTTP
listener (do not add TLS to the gateway).

### 6. Outlook Android provisions via Microsoft's cloud sync connector (credentials transit Microsoft)

Outlook Android does **not** talk directly to the gateway for third-party
(non-Microsoft-365) accounts; it sends credentials to Microsoft, and a
Microsoft datacenter service becomes the EAS client connecting to the
gateway. Mail, calendar, and contact data transit Microsoft's servers. This
is a client property, not a gateway defect, but it must be documented as a
deployment prerequisite: the operator must consent to Microsoft cloud sync
or use a direct-connection account flow if available.

**Fix (ladder 1 — docs only):** make this explicit in DEPLOY_RUNBOOK.md and
CLOUDFLARED_SETUP.md (the smoke script already covers part of it). No code
change.

### 7. Outlook Android option matrix pinned to build 5.2634.2; task pins 5.2641.1

`src/eas_sync_options.rs` pins `parse_outlook_android_option_matrix` and
`parse_new_outlook_windows_option_matrix` to the older builds (5.2634.2 /
20251205004.10 per AGENTS.md). The task explicitly requires Outlook Android
5.2641.1 and New Outlook for Windows October 7, 2026. If 5.2641.1 sends a
different `<Options>` matrix (e.g. different `BodyType`, `Preview`,
`MIMETruncation`, `MIMESupport` values), the gateway will negotiate wrong
body/truncation settings and calendar/email rendering breaks.

**Fix (ladder 1):** re-capture the exact `<Options>` matrix sent by Outlook
Android 5.2641.1 and New Outlook for Windows (October 7, 2026) against a
running gateway, update `parse_outlook_android_option_matrix` /
`parse_new_outlook_windows_option_matrix` and their pinned tests, and update
AGENTS.md version references.

### 8. New Outlook for Windows uses MAPI/HTTP, not EAS — MAPI path unproven

New Outlook for Windows (October 7, 2026) connects over **MAPI/HTTP**
(MS-OXCMAPIHTTP) for third-party accounts, not EAS. The gateway implements
the MAPI/HTTP surface (`/mapi/emsmdb`, `/mapi/nspi`, logon, ROPs, session,
push notifications), but there is **no MAPI smoke/end-to-end coverage** in
the test suite or deploy runbook. The smoke script only probes EAS/EWS/
Autodiscover.

**Fix (ladder 1 — docs + test):** add MAPI/HTTP login + a minimal ROP round
trip (e.g. `RopLogon` + `RopGetPropsTable`/`RopGetContentsTable` on the
calendar Inbox) to the smoke harness or a new `tests/mapi_smoke.sh`, and
document the New Outlook for Windows MAPI provisioning flow in
DEPLOY_RUNBOOK.md §5.

### 9. `GATEWAY_ADMIN_USERNAME` / `GATEWAY_ADMIN_PASSWORD` ship empty

When empty, the deploy-time JMAP calendar capability check (`verify_calendar_capability_at_startup`)
is skipped, the full GAL (NSPI + OAB) falls back to a self-only stub, and
`jmap_calendar_write_enabled` defaults to `true` with the lossy-write guard
relying on per-write read-back (which works without admin creds, but the
deploy-time probe that catches a broken backend up front never runs).

**Fix (ladder 1):** document in DEPLOY_RUNBOOK.md that the operator should set
a Stalwart admin account (able to create a throwaway calendar) for the
deploy-time probe; the compose already leaves them as empty placeholders
(which is correct — do not commit real credentials). Strengthen the startup
warning to state exactly what is lost (full GAL + deploy-time JMAP probe).

### 10. SMTP TLS verification fails: `GATEWAY_SMTP_HOST=stalwart` with implicit TLS on 465

The compose sets `GATEWAY_SMTP_HOST=stalwart` (internal bridge service name)
and `GATEWAY_SMTP_PORT=465` (implicit TLS/SMTPS). `lettre` connects to
`stalwart:465` with TLS, but Stalwart's certificate (if any) is issued for
the public MX host (`mail.example.com`), not `stalwart`, so hostname
verification fails and SMTP submission (SendMail) breaks — email cannot be
sent from Outlook.

**Fix (ladder 1 — compose value):** keep `GATEWAY_SMTP_HOST=stalwart` for the
bridge connection but disable TLS hostname verification for the internal
bridge backend (or add a `GATEWAY_SMTP_TLS_VERIFY=0` opt-in for private
networks), while advertising `GATEWAY_MAIL_HOST` (public MX) in Autodiscover
IMAP/SMTP blocks so clients connect directly to the public MX with proper
TLS. The gateway's own SMTP relay to Stalwart is internal and can use
insecure/no-verification TLS over the bridge network.

---

## P1 — Degrades the use-case (should be resolved for a frictionless deployment)

### 11. Backend ports 8080 vs 8133 undocumented

Stalwart exposes both 8080 (HTTP: CalDAV `/dav/cal`, CardDAV `/dav/card`,
JMAP `/jmap`) and 8133. The compose points all gateway backend vars at
`stalwart:8080`. It is not documented which port serves what, so an operator
who needs 8133 (e.g. for a different Stalwart service) cannot re-point
correctly.

**Fix (ladder 1 — docs):** document in CLOUDFLARED_SETUP.md / docker-compose
comments that 8080 = CalDAV/CardDAV/JMAP HTTP, and that the gateway uses
8080 for all backend plumbing.

### 12. `cloudflared/config.yml` references a forbidden `.env` file

The config file template contains shell variable references (`$GATEWAY_MAIL_DOMAIN`,
`$GATEWAY_HOST`) and comments about exporting them, implying an env file.
The deployment forbids `.env`; values must be rendered inline.

**Fix (ladder 1 — docs):** update `cloudflared/config.yml` and CLOUDFLARED_SETUP.md
Step 3 to instruct the operator to substitute `<GATEWAY_MAIL_DOMAIN>` and
`<GATEWAY_HOST>` inline (via `sed` one-liner or manual edit), with no `.env`
file.

### 13. Healthcheck `/health/ready` requires Stalwart backend; no auto-restart on unhealthy

`/health/ready` returns 503 when the JMAP/CalDAV auth backend is unreachable.
Docker `restart: unless-stopped` does **not** restart an unhealthy container
— only marks it unhealthy. The optional `autoheal` (willfarrell/autoheal)
sidecar is commented out. If Stalwart restarts, the gateway stays unhealthy
until manually restarted.

**Fix (ladder 1 — docs):** document that automatic restart requires a
health-aware controller (uncomment the `autoheal` block or use an
orchestrator health check). Add the `autoheal` label to the gateway service
in the compose template as an optional, commented block (already present).

### 14. EWS `GetStreamingEvents` holds a connection up to 30 minutes, exceeding Cloudflare's 125s Proxy Read Timeout

Cloudflare cuts unanswered requests at 125s (HTTP 524). A 30-minute streaming
hold is always cut, breaking push-style calendar/mail notifications through
the tunnel.

**Fix (ladder 1):** clamp the streaming hold to ~80s (matching the EAS Ping
`GATEWAY_MAX_PING_HEARTBEAT` clamp) when `CF-RAY` is detected, so the client
re-issues the request. Document the behaviour.

### 15. Rate limiter keys on `CF-Connecting-IP`; Microsoft's cloud egress shares IPs across many users

Outlook Android via Microsoft cloud sync and New Outlook for Windows via
Microsoft datacenter egress all arrive from shared Microsoft ASNs/IPs. A
per-IP limit (default 1200/min) can be exhausted by one connector serving
many mailboxes, causing 429s and sync stalls.

**Fix (ladder 1 — docs):** document that operators behind Microsoft cloud
egress should raise `GATEWAY_RATE_LIMIT_REQUESTS_PER_MINUTE` and consider
keying on an additional header (e.g. `X-Forwarded-For` first hop) when many
users share one egress IP. No code change unless a new header key is added.

### 16. `GATEWAY_ALLOW_INSECURE_HTTP=1` is a global switch disabling HTTPS checks for all backends

It disables the HTTPS requirement for JMAP/CalDAV/CardDAV globally. If the
operator points any backend base URL at a public host (typo), Basic
credentials are sent in cleartext. There is a startup warning, but the flag
is all-or-nothing.

**Fix (ladder 1):** keep the flag for private-network (bridge) backends but
add per-backend allowlists (only `stalwart`, `localhost`, RFC1918) and reject
any base URL outside that set at startup when the flag is set. Already
partially implemented (startup warning exists); make it a hard error for
non-private hosts.

### 17. IMAP advertised in Autodiscover but no gateway IMAP client — dead config

Autodiscover renders IMAP/SMTP `<Protocol>` blocks pointing at
`GATEWAY_MAIL_HOST` (public MX), which is correct for clients that speak
IMAP/SMTP directly to Stalwart. But the gateway itself has no IMAP client,
so `GATEWAY_IMAP_HOST`/`GATEWAY_IMAP_PORT` are dead config inside the
gateway (they are only advertised, never used). This is fine for the use-case
(Outlook uses EAS/EWS/MAPI, not IMAP), but it is misleading documentation.

**Fix (ladder 1 — docs):** clarify in docker-compose comments and
DEPLOY_RUNBOOK.md that `GATEWAY_IMAP_HOST`/`PORT` are advertised to clients
only (clients talk IMAP directly to Stalwart's public MX), not used by the
gateway.

### 18. JMAP session URL internalisation scattered across three sites

`apiUrl` is overridden to the internal JMAP base, but `downloadUrl`,
`uploadUrl`, and `eventSourceUrl` templates are only partially internalised.
If an operator changes `GATEWAY_JMAP_BASE`, blob downloads/uploads and push
event sources may point at the wrong host.

**Fix (ladder 1 — docs):** audit all three URL templates in `src/jmap.rs`
and ensure every JMAP session URL is derived from `GATEWAY_JMAP_BASE`
(or the internal bridge equivalent), with a startup check that they are
consistent.

### 19. Tasks and Notes are CalDAV-backed (VTODO/VJOURNAL), not JMAP

Stalwart v1.1.0 has no JMAP Task/Note type (confirmed for v1.0.x; must be
re-verified for v1.1.0). Tasks/Notes sync via CalDAV (`/Tasks/`, `/Notes/`
collections). This satisfies the use-case (Outlook can sync Tasks/Notes
via CalDAV-backed EAS), but it is not "JMAP-first" and cannot be made so
until Stalwart exposes JMAP Task/Note types.

**Fix (ladder 1 — docs):** document in DEPLOY_RUNBOOK.md and CLOUDFLARED_SETUP.md
that Tasks/Notes use CalDAV by design on Stalwart v1.1.0. Re-verify
`GATEWAY_PREFER_JMAP_TASKS` is intentionally omitted (it is).

### 20. EWS `GetServiceConfiguration` advertises placeholder `GATEWAY_HOST` URLs

If `GATEWAY_HOST` is a placeholder (`calendar.example.com`), the EWS
configuration response advertises `https://calendar.example.com/EWS/Exchange.asmx`
etc. — unresolvable until the operator replaces placeholders.

**Fix (ladder 1):** covered by issue #3 (pairing check fatal at startup). If
the placeholder check passes, `GATEWAY_HOST` is real and the advertised URLs
are correct.

### 21. Version stamp advertises Exchange SE 15.2 vs 2025/2026 clients

`GATEWAY_SERVER_VERSION=15.2.2562.45` / `GATEWAY_SERVER_EXCHANGE_VERSION=Exchange2016`
advertise Exchange Server SE / 2016, while the clients are 2025/2026 builds.
This is cosmetic (clients don't refuse older versions), but may cause
behaviour differences in option negotiation.

**Fix (ladder 1 — docs):** verify against real clients; bump the advertised
version to a modern Exchange build if real clients require it. Document as a
tunable.

### 22. Dual-stack bind `[::]:8134` vs compose publish `127.0.0.1:8134:8134` (IPv4 only)

The gateway binds `[::]:8134` (dual-stack) but the compose publishes only
IPv4 loopback. cloudflared on the host reaches `http://localhost:8134`
(IPv4), which works. If the operator's host has IPv6-only cloudflared or the
container lacks IPv6, there is a mismatch.

**Fix (ladder 1 — compose):** align the bind to the publish address
(`127.0.0.1:8134` for loopback-only, which is the secure posture) or keep
dual-stack and publish both. Document the choice. The current loopback-only
publish is correct for the cloudflared-on-host deployment.

---

## P2 — Robustness and friction (polish, not blocking)

### 23. `GATEWAY_CONFIG` (TOML file) supported but must stay unset

The code reads a TOML config file at `GATEWAY_CONFIG`, but the deployment
contract is compose-only (env vars). The compose does not set it (correct).
Risk: an operator mounts a stray config file and env vars are silently
ignored/overridden.

**Fix (ladder 1 — docs):** document that `GATEWAY_CONFIG` must remain unset;
the compose template is authoritative.

### 24. `GATEWAY_HMAC_SECRET` placeholder fails startup validation

The compose ships `REPLACE_WITH_A_RANDOM_32_CHAR_MINIMUM_SECRET`, which fails
validation (`starts_with("REPLACE_")`). The container will not start until
replaced with `openssl rand -hex 32` output.

**Fix (ladder 1 — docs):** make this a highlighted step in DEPLOY_RUNBOOK.md §0/§3
(already present). Keep the placeholder in the template (it must not be
deployed as-is).

### 25. `GATEWAY_ADMIN_PASSWORD` in cleartext in compose; `_FILE` form undocumented

The password sits in cleartext. Each secret var has a `<VAR>_FILE` companion
(Docker secrets / mounted files) that takes precedence, but this is not
documented in the compose or runbook.

**Fix (ladder 1 — docs):** document the `_FILE` form and recommend
`chmod 600 docker-compose.yml`.

### 26. Dockerfile / compose healthcheck duplicated

Both define `/health/ready` checks. Minor duplication.

**Fix (ladder 1 — docs):** add a comment noting the duplication is intentional
(Dockerfile image check vs compose runtime check) or consolidate.

### 27. `GATEWAY_MAX_ATTACHMENT_BYTES=26214400` (25 MiB) may be too small

Outlook's send cap is 25 MB; the code cap is 50 MiB. 25 MiB attachments yield
~35 MiB of base64 MIMEData inside SendMail. Raising to 50 MiB accommodates
larger sends.

**Fix (ladder 1 — compose):** document the trade-off; operator can raise to
50 MiB if needed.

### 28. `GATEWAY_RATE_LIMIT_REQUESTS_PER_MINUTE=600` default vs compose 1200

Code default 600, compose ships 1200. Initial sync bursts may still exceed.

**Fix (ladder 1 — docs):** document raising for multiple simultaneous devices.

### 29. `AutoDetect` `WWW-Authenticate: Bearer` advertises Microsoft's `authorization_uri`

Misleading for a self-hosted gateway (AutoDetect expects Microsoft cloud).
The gateway must include `authorization_uri` for AutoDetect compatibility,
but it points at Microsoft's cloud, not the gateway.

**Fix (ladder 1 — docs):** document that this is required for AutoDetect and
is a client-side behaviour, not a gateway misconfiguration.

### 30. MAPI `GATEWAY_MAPI_ENABLED=1` + `GATEWAY_MAPI_HMA_ENABLED=0` advertises `mapiHttp` with Basic auth

Intended posture (MAPI/HTTP accepts Basic without HMA), but a security
trade-off: Basic auth over plain HTTP (tunnel-terminated TLS) is exposed to
clients without OAuth/HMA.

**Fix (ladder 1 — docs):** document the trade-off; enable HMA only if the
operator has an OIDC issuer.

### 31. No `README.md` in repo; entry point is `DEPLOY_RUNBOOK.md`

A new operator lands on `DEPLOY_RUNBOOK.md`, which is not linked from the
compose file.

**Fix (ladder 1 — docs):** add a `README.md` that points to
`DEPLOY_RUNBOOK.md`, `CLOUDFLARED_SETUP.md`, and `docker-compose.yml`.

### 32. `AGENTS.md` version pins stale

Says Stalwart v1.0.1 / Outlook Android 5.2634.2 in places; task pins v1.1.0 /
5.2641.1 / New Outlook October 7, 2026.

**Fix (ladder 1 — docs):** update AGENTS.md version references.

### 33. Smoke script uses env vars (`GATEWAY_BASE_URL`, `GATEWAY_USER`, `GATEWAY_PASS`)

Compliant (no `.env` file), but confusing since the deployment forbids env
files. The script reads from the shell, not a file.

**Fix (ladder 1 — docs):** add a header comment clarifying these are shell
vars for the smoke test only, not deployment env vars.

---

## Operator change list (must-do before deployment)

### Cloudflare Zero-Trust (webui only — as already configured)

The two existing routes are sufficient **iff** the client uses the single
canonical host `calendar.<domain>` (`GATEWAY_HOST`):
- `https://calendar.<domain>/*` → port 8134 on the Ubuntu host (EAS/EWS/MAPI/Autodiscover/OAB/ECP).
- `https://autodiscover.<domain>/*` → port 8134 on the Ubuntu host (Android wizard primary probe).

No route for the bare root domain (already has A/AAAA records). Both records
must be CNAME → `<tunnel-id>.cfargotunnel.com`, Proxied (orange cloud).

**Changes to report to the operator:** none beyond the two existing routes,
provided `GATEWAY_HOST == calendar.<GATEWAY_MAIL_DOMAIN>` and the operator
replaces `example.com` placeholders with their real domain.

### Stalwart Mailserver v1.1.0-container

No changes strictly required if JMAP Calendars (`urn:ietf:params:jmap:calendars`),
JMAP Contacts (`urn:ietf:params:jmap:contacts`), CardDAV at `/dav/card/{user}/`,
and CalDAV at `/dav/cal/{user}/` are all functional (they are in v1.0.x;
re-verify for v1.1.0).

**Changes to report to the operator:**
1. Pin the Stalwart image to v1.1.0 digest (do NOT use `:latest`).
2. Confirm the address book exists at `/dav/card/{user}/default/` (CardDAV).
3. If the operator wants the deploy-time JMAP calendar capability check to run,
   set `GATEWAY_ADMIN_USERNAME`/`GATEWAY_ADMIN_PASSWORD` to a Stalwart account
   that can create a throwaway calendar (the probe creates and destroys an
   isolated `exchange-gateway-deploy-probe-<uuid>` calendar).

### Gateway compose (`docker-compose.yml`)

The operator must:
1. Replace every `example.com` placeholder with their real domain:
   - `GATEWAY_HOST=calendar.<real domain>` (must equal `calendar.` + mail domain)
   - `GATEWAY_MAIL_DOMAIN=<real domain>`
   - `GATEWAY_MAIL_HOST=<real MX hostname>` (matching the MX record)
   - `GATEWAY_SMTP_HOST=stalwart` (keep — internal bridge name)
   - `GATEWAY_JMAP_BASE=http://stalwart:8080/jmap` (keep)
   - `GATEWAY_CALDAV_BASE=http://stalwart:8080/dav` (keep)
   - `GATEWAY_CARDDAV_BASE=http://stalwart:8080/dav/card` (keep)
2. Replace `GATEWAY_HMAC_SECRET` with `openssl rand -hex 32` output (must not
   start with `REPLACE_`).
3. Create `/armblock/exchange-gateway-data` and `chown 10001:10001` it.
4. Optionally set `GATEWAY_ADMIN_USERNAME`/`GATEWAY_ADMIN_PASSWORD` for the
   full GAL + deploy-time probe.
5. Do **not** create or use a `.env` file.

### DNS records (Cloudflare)

| Type | Name | Target | Proxy |
|------|------|--------|-------|
| CNAME | `calendar` | `<tunnel-id>.cfargotunnel.com` | Proxied |
| CNAME | `autodiscover` | `<tunnel-id>.cfargotunnel.com` | Proxied |

No `@` (root) CNAME. Both must be Proxied from the start.

---

## Summary: prioritized issue list

| # | Sev | Area | Defect | Fix ladder |
|---|-----|------|--------|------------|
| 1 | P0 | Backend version | Code/docs validated vs Stalwart v1.0.x; operator runs v1.1.0 | docs + live re-verify (1→3) |
| 2 | P0 | EAS | ~~FolderCreate/Delete/Update/GetAttachment/Find unimplemented~~ **DELIVERED** (`eas_folders.rs` + `folder_map` v11) | handlers + JMAP Mailbox/set + CalDAV collections (1→4) |
| 3 | P0 | Autodiscover | Hostname pairing is warning-only; placeholders not enforced | make fatal at startup (1) |
| 4 | P0 | SMTP/Autodiscover | `GATEWAY_MAIL_HOST` vs `GATEWAY_SMTP_HOST` TLS/hostname mismatch risk | startup validation of internal vs public hosts (1) |
| 5 | P0 | HTTP/TLS | Plain HTTP only; `x-forwarded-proto` enforcement inconsistent | centralized proxy-header middleware (1) |
| 6 | P0 | Outlook Android | Provisions via Microsoft cloud; credentials transit Microsoft | docs (1) |
| 7 | P0 | Outlook Android | Option matrix pinned to 5.2634.2; task pins 5.2641.1 | re-capture matrix + tests (1) |
| 8 | P0 | New Outlook Win | MAPI/HTTP is the real path; no MAPI smoke coverage | smoke test + docs (1) |
| 9 | P0 | GAL | Admin creds empty → self-only GAL, no deploy-time JMAP probe | docs + stronger startup warning (1) |
| 10 | P0 | SMTP | `stalwart:465` implicit TLS fails cert verification vs public MX | disable TLS verify for internal bridge backend (1) |
| 11 | P1 | Backend ports | 8080 vs 8133 undocumented | docs (1) |
| 12 | P1 | Docs | `cloudflared/config.yml` references forbidden `.env` | docs (1) |
| 13 | P1 | Healthcheck | `/health/ready` unhealthy during Stalwart restart; no auto-restart | docs + optional autoheal (1) |
| 14 | P1 | EWS | `GetStreamingEvents` 30-min hold exceeds CF 125s cap | clamp to ~80s behind CF-RAY (1) |
| 15 | P1 | Rate limit | Per-IP limit throttles shared Microsoft cloud egress | docs (1) |
| 16 | P1 | Security | `GATEWAY_ALLOW_INSECURE_HTTP=1` global; cleartext creds risk | per-backend allowlist + hard error for public hosts (1) |
| 17 | P1 | IMAP | Advertised but no gateway IMAP client; dead config | docs (1) |
| 18 | P1 | JMAP | `downloadUrl`/`uploadUrl`/`eventSourceUrl` partial internalisation | audit + make consistent (1) |
| 19 | P1 | Tasks/Notes | CalDAV-backed, not JMAP (Stalwart limitation) | docs (1) |
| 20 | P1 | EWS | `GetServiceConfiguration` advertises placeholder URLs | covered by #3 (1) |
| 21 | P1 | Version stamp | Exchange SE 15.2 vs 2025/2026 clients | verify + tune (1) |
| 22 | P1 | Bind | Dual-stack bind vs IPv4 loopback publish | align + docs (1) |
| 23–33 | P2 | Robustness/friction | Config file, HMAC placeholder, cleartext admin password, duplicated healthcheck, attachment cap, rate limit default, Bearer AutoDetect URI, MAPI Basic auth, no README, stale AGENTS.md, smoke env vars | docs/compose (1) |

**Bottom line:** with the current code, the calendar and email read/write paths are
substantially sound (AUDIT3 P0 calendar/contacts are delivered) and the EAS
folder-management, GetAttachment and Find commands are delivered (P0 #2), but the
use-case is **not** deployment-ready: the hostname pairing is not enforced
(P0 #3), SMTP TLS will fail
against the internal bridge name (P0 #10), and the Stalwart backend version
contract is unvalidated for v1.1.0 (P0 #1). Resolving the remaining P0 items
(1, 3–10), plus the
operator change list above, is required before the container can be deployed via
Portainer for the stated use-case.
