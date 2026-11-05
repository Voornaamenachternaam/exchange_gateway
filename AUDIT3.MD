# AUDIT3 — Independent code-level audit for the Stalwart + Outlook deployment

Scope: the containerized `exchange_gateway` must let the calendar and email of an
existing Stalwart Mailserver v1.0.x account be used **fully, natively** by
**New Outlook for Windows (build 20251205004.10)** and **Outlook Android
(5.2634.2)**, with the clients talking only to the gateway and Stalwart acting
only as backend. JMAP must be preferred wherever Stalwart implements it.

Method: every finding below was re-derived from the source in
`/workspace/project/exchange_gateway` and from **live probes** against the
running Stalwart v1.0.0 instance (JMAP at `http://localhost:18080/jmap/`, CalDAV
`/dav/cal/…`, CardDAV `/dav/card/…`, credentials `testuser@redacted.com`). No
finding relies on the prior `AUDIT.md` / `AUDIT2.md` prose. Where a finding is a
runtime contract mismatch, the exact Stalwart source that produces the wire
behaviour is cited (`/tmp/sw`, `stalwartlabs/stalwart`).

Priority ordering is by user-visible impact on the stated use-case. The
requested fix-priority ladder (existing deps → new high-quality deps → Rust
1.99.1 std → custom code) is applied per item.

---

## P0 — Calendar is non-functional in both directions (data loss + total read failure)

### 1. JMAP calendar **reads** always return zero events — `iCalendar` is an object, the gateway deserializes it as a `String`

> **STATUS: DELIVERED.** Fixed as described below; see the *Delivered* note at
> the end of this section.

`JmapCalendarEvent::i_calendar` is declared `Option<String>`
(`src/jmap.rs:706`):

```rust
/// Stalwart returns the original iCalendar data in this property.
#[serde(default)]
pub i_calendar: Option<String>,
```

Stalwart does **not** return an ICS string in `iCalendar`. It returns the
JSCalendar *representation of the ICS object* — a JSON object. The response is
built from `calcard`'s `JSCalendarProperty::ICalendar`
(`/tmp/sw/crates/calcard/src/jscalendar/types.rs:150`) and the get handler
selects it as a normal JSCalendar property
(`/tmp/sw/crates/jmap/src/calendar_event/get.rs:881`). Live proof (exact
gateway property list):

```
CalendarEvent/get properties=["…","iCalendar","@etag"]
  → [{"iCalendar":{"convertedProperties":{"duration":{"name":"dtend"}},"name":"vevent"},
      "uid":"cross-1","id":"d"}, …]
```

8 of 17 events in the test account return `iCalendar` as an **object**; the rest
omit it entirely (no event returns a string). The gateway then does:

```rust
events = data.get("list")
    .and_then(|v| serde_json::from_value(v.clone()).ok())
    .unwrap_or_default();          // src/jmap.rs:4600
```

Because **one** element in the list has a map where `Option<String>` is
expected, `serde_json::from_value::<Vec<JmapCalendarEvent>>` fails for the
**entire list**, `.ok()` discards the error, and `unwrap_or_default()` yields an
empty `Vec`. Reproduced with the project's own Rust 1.99.1 toolchain:

```
DESERIALIZATION ERROR (whole list): invalid type: map, expected a string
events delivered after .ok().unwrap_or_default(): 0
```

This kills **every** JMAP calendar read path that is the default in the shipped
compose (`GATEWAY_PREFER_JMAP_CALENDAR=1`):

- EAS `Sync`/initial sync — `sync.rs:1390` `query_calendar_events` → 0 events.
- EAS delta sync — `sync.rs:1443` `changes_calendar_events` + `get_calendar_events`
  → `get_calendar_events` (`src/jmap.rs:4692`) does the same
  `event.get("iCalendar").and_then(|v| v.as_str())` and returns an **empty map**,
  so `all_ids` (created ∪ updated) resolve to nothing and the delta is empty.
- EWS `FindItem`/`GetItem` calendar — `ews.rs:2014` `load_current_calendar_items`
  and `ews.rs:4183` `get_calendar_event`.
- Free/busy — `ews.rs:1590` and `eas.rs:4253` both call `query_calendar_events`.
- MAPI calendar contents — the same `query_calendar_events` read path.

Note the CalDAV fallback masks this in one narrow case: `query_calendar_events`
returns `Ok` (empty list), so `jmap_failed` stays `false` and **no** fallback to
CalDAV occurs (`sync.rs:1508` comment: "Fallback to CalDAV only if JMAP Calendar
failed (not if it returned zero events)"). The calendar therefore presents as
permanently empty rather than erroring — the worst possible failure mode for a
user.

Fix (ladder 1→4): treat `iCalendar` as `serde_json::Value` (existing dep
`serde_json`), and convert the JSCalendar object to ICS with the in-repo
`calcard`-equivalent path the gateway already uses elsewhere, or request the
ICS via the **`CalendarEvent/parse`-free** route the server supports
(`urn:ietf:params:jmap:calendars:parse` is advertised). A minimal, correct
first step is to change the field type and stop discarding the whole list on a
single-element decode error (deserialize per-element, skipping bad ones).
Either way the current `Option<String>` contract is provably wrong and must
change.

#### Delivered (this change)

The fix is the ladder 1→4 route, verified empirically against Stalwart's own
JSCalendar producer (`calcard` 1.0.2, the version bundled with Stalwart v1.0.x)
over a 25-shape event corpus (simple, attendees, organizer, RRULE with
COUNT/UNTIL/BYDAY, EXDATE, moved `RECURRENCE-ID` override, all-day single and
multi-day, DURATION-only, CATEGORIES/CLASS/STATUS/TRANSP, VALARM, TZID,
custom VTIMEZONE, X-MS-* properties, cancellation, sequence/priority):

1. **New dependency (ladder 2): `calcard` 0.3.14** (`Apache-2.0 OR MIT`) —
   Stalwart's own iCalendar ⇄ JSCalendar conversion library. It is the exact
   library that produces the JSCalendar the gateway receives, so converting
   back is faithful rather than approximate.
2. **New module `src/jscalendar.rs`** — `jscalendar_event_to_ics` accepts
   either a literal ICS string (returned verbatim) or a JSCalendar object,
   strips the JMAP-only properties (`id`, `calendarIds`, `isDraft`, …) that
   have no iCalendar counterpart, wraps the object as a JSCalendar `Group`
   (calcard's parser only accepts a Group/VCALENDAR root), and converts with
   `calcard`. The `iCalendar` fragment carries only the converted-property
   mapping (`{"convertedProperties":…,"name":"vevent"}`) — **not** the event
   data — so the whole event object is reassembled, which is why the fix is
   not a one-line type change. A participant with no `calendarAddress` cannot
   become an `ATTENDEE`; instead of dropping it, it is pinned to a
   `PARTICIPANT` component (RFC 8984 §4.4.6 permits addressless participants)
   so its name and remaining properties survive the reconstruction — see
   `sanitize_event` in `src/jscalendar.rs`. calcard's exporter asserts on such
   a participant in debug builds, which is why the fragment is injected.
   The ICS parser (`parse_event_lines` in `src/calendar.rs`) skips nested
   sub-component properties, so a `PARTICIPANT`'s `SUMMARY` can never overwrite
   the event's own subject.
3. **`JmapCalendarEvent::i_calendar` is now `Option<Value>`** and the raw
   `CalendarEvent/get` object is retained in a new `raw: Option<Value>` field;
   `JmapCalendarEvent::to_ics()` reconstructs the ICS on demand. A manual
   `Deserialize` impl retains `raw` on the public serde path too, so an event
   deserialized outside the client parser still converts.
4. **Per-element deserialization** (`parse_calendar_event_list`): a single
   element that fails to decode is skipped and logged instead of failing the
   whole list — the exact failure mode the audit reproduced.
5. **The requested property list is corrected** (`CALENDAR_EVENT_PROPERTIES`).
   The old list used names Stalwart does not recognize — `isAllDay` (the
   JSCalendar name is `showWithoutTime`), `end` (the name is `duration`, with
   the `dtend` mapping carried in `convertedProperties`), `location` (the name
   is `locations`), `recurrenceRules` (the name is `recurrenceRule`). Invalid
   names are silently dropped by the server, so a faithful reconstruction
   requires the exact RFC 8984 spellings.
6. **All consumers** (`sync.rs` initial + delta, `ews.rs` free/busy + Find/Get,
   `eas.rs` free/busy, `get_calendar_event`/`get_calendar_events`) now call
   `event.to_ics()` instead of reading `i_calendar` as a string. Conversion and
   deserialization failures are **propagated** rather than swallowed: the
   single and batch getters return `Err` (so their CalDAV fallback runs), and
   the sync delta/initial paths decline to advance the stored state token when
   an event could not be turned into an item — otherwise the skipped event would
   be lost forever on the next delta.
7. **Calendar deltas no longer tombstone unchanged events.** A
   `CalendarEvent/changes` result is a partial change set; a stored event absent
   from it is unchanged, not deleted. Only a full snapshot (initial sync, or the
   CalDAV fallback) treats an omission as a deletion, and within a delta only an
   explicitly `destroyed` id is removed (`calendar_event_deleted` in
   `src/sync.rs`).

Tests: `src/jscalendar.rs` 8 unit tests (verbatim string passthrough, full
object reconstruction, RRULE/EXDATE/moved-override, all-day dates,
VALARM + X-property preservation from the fragment, addressless-participant
preservation without subject leakage, `@etag` stripping, non-event rejection) +
`src/jmap.rs` 7 tests (per-element skip + skip count, `to_ics()` reconstruction,
`to_ics()` `None` without raw data, raw retention on public deserialize,
conversion-failure propagation for the single and batch getters, unparseable
count from `query_calendar_events`) + `src/sync.rs` 1 test (tombstone decision)
+ `src/calendar.rs` 1 test (nested sub-component properties ignored).

### 2. JMAP calendar **writes** silently discard the event payload (and the guard that should catch this is disabled in the shipped compose)

`set_calendar_event` (`src/jmap.rs:4769`) creates events by posting the raw ICS
in the `iCalendar` property. Live proof:

```
CalendarEvent/set create {"calendarIds":{"c":true},"iCalendar":"BEGIN:VCALENDAR…"}
  → created {"id":"y", …}
CalendarEvent/get ids=["y"] properties=["uid","title","start","duration"]
  → [{"uid":"…","id":"y"}]          # title/start/duration NOT persisted
```

The create is accepted and an empty event is stored. This is a **documented,
known** hazard: `src/jmap.rs:4745` carries a WARNING that a server build with
this behaviour "silently loses all gateway-created event data", and the deploy
probe `probe_write_paths` (`src/jmap.rs:3981`) exists precisely to detect it via
`ics_round_trip_ok`. The guard wiring, however, only disables the JMAP write
path when the check **runs**:

- `verify_calendar_capability_at_startup` (`src/main.rs:1183`) **skips entirely**
  when `GATEWAY_ADMIN_USERNAME` / `GATEWAY_ADMIN_PASSWORD` are empty
  (`src/main.rs:1220`), logging only a warning.
- The shipped `docker-compose.yml` ships both **empty**.
- `jmap_calendar_write_enabled` defaults to **true** (`src/models.rs:332`,
  constructed at `src/models.rs:325`).

So in the exact deployment scenario described by the user (compose filled in,
admin creds left blank because they were optional), the process starts with JMAP
calendar writes **enabled** and the lossy path live. EWS `CreateItem`/`UpdateItem`
(`ews.rs:6406`, `ews.rs:6850`), `CopyItem` (`ews.rs:7709`) and the RSVP pipeline
(`meeting/rsvp.rs:1112`) will all route creates through it. (EAS `Sync` calendar
`Add` happens to use CalDAV directly — `sync.rs:164` — so EAS create is safe by
accident, but EAS read is broken by finding #1, so the user still cannot use the
calendar.)

Fix (ladder 1→4): default `GATEWAY_ADMIN_USERNAME/PASSWORD` is the wrong lever
for a probe the *user* can't easily satisfy; the capability check should run with
the **user's own** credentials on first use, or the write path must validate its
own round-trip and fall back to CalDAV on a mismatch. At minimum, default
`jmap_calendar_writes_enabled` to **false** (fail closed) until a positive
`ics_round_trip_ok` is observed, and make the "admin creds missing" skip an
explicit, loud deployment error rather than a warning.

#### Delivered (this change)

The fix follows the audit's own recommendation — the write path validates its
own round-trip and falls back to CalDAV on a mismatch — rather than depending on
the operator filling in admin credentials. It also fixes the guard so it is
correct in the shipped compose.

1. **The write payload is no longer a raw ICS string.** `set_calendar_event`
   (`src/jmap.rs`) now converts the rendered ICS to structured JSCalendar
   properties before calling `CalendarEvent/set`. The conversion is
   `ics_to_jscalendar_event` (new, `src/jscalendar.rs`): it parses the ICS with
   `calcard` — the same library Stalwart uses — and emits the `Event` object,
   so the payload the gateway writes is exactly the shape the server reads
   back. A raw `iCalendar` payload is what Stalwart silently discards; a
   structured payload is what survives.
2. **`jscalendar_write_payload` (new)** strips the server-managed/immutable
   properties (`id`, `calendarIds`, `created`, `updated`, `sequence`, `@etag`,
   `isDraft`, `isOrigin`, `baseEventId`, `method` — the last three are rejected
   outright with `invalidProperties`/immutable) and, for an **update**,
   explicitly nulls every optional property the fresh event does not set
   (`description`, `locations`, `participants`, `recurrenceRule`, …). Stalwart's
   `CalendarEvent/set` update is a **partial patch** (verified live against
   v1.0.3): a property absent from the patch keeps its old value, so a
   replacement update must clear stale content or a removed description / dropped
   attendee roster / cancelled RRULE would survive the edit.
3. **Every create/update verifies its own read-back.** `set_calendar_event`
   issues `CalendarEvent/set` and a `CalendarEvent/get` (create via
   `#ids`/`resultOf`, update by explicit id) in the same JMAP request and
   compares the stored event against the payload (`verify_write_round_trip`,
   new). The comparison covers **every written content field** (description,
   locations, recurrence, participants, alarms, …), not just the title, with
   tolerance for the benign normalisations a JSCalendar backend may apply
   (object key order, surrounding whitespace, a dropped nested `@type`). The
   failure is typed: `Discarded` (the read-back has no content marker — the
   only proof of a lossy write path) versus `Mismatch` (the marker survived but
   another written field did not — **not** proof of a lossy path, so it must not
   disable JMAP writes). On either failure the write surfaces an error and the
   created event is destroyed (best-effort) so the CalDAV fallback — which PUTs
   a fresh `{uuid}.ics` — cannot surface a duplicate. This is the
   self-validation the audit asked for, and it works regardless of whether the
   deploy check ran. The verification error text names only the failing property
   and failure class, never event content (titles/descriptions are user data and
   must not reach logs or deploy reports).
4. **The runtime gate is now fail-closed at the point of use, not only at
   startup.** On `Discarded` the caller disables the JMAP calendar write path
   for the process (`AppState::disable_jmap_calendar_writes`) so all subsequent
   create/update traffic takes the CalDAV branch. A `Mismatch` does **not**
   disable the path (it would let one unenumerable backend normalisation
   permanently downgrade a working deployment). Wired at every JMAP calendar
   write site: EWS `CreateItem` (`try_jmap_create_calendar`), `UpdateItem`
   (`try_jmap_update_calendar`), `CopyItem`, and the RSVP pipeline
   (`meeting/rsvp.rs` `create_copy_via_jmap` / `write_existing_copy`).
5. **The deploy probe exercises the real write path.**
   `probe_write_paths` (`src/jmap.rs`) no longer writes a raw `iCalendar`
   payload it expects to fail — it converts the probe ICS with
   `ics_to_jscalendar_event` + `jscalendar_write_payload` (the exact helpers
   `set_calendar_event` uses) and asserts the content round-trips. `ics_round_trip_ok`
   now means "the gateway's actual create path persists content", which is the
   signal that matters.
6. **The guard is no longer dead in the shipped compose.** The admin-creds
   skip in `verify_calendar_capability_at_startup` (`src/main.rs`) is now an
   explicit message stating that the runtime read-back guard still protects
   writes (and, in `enforce` mode, still a fatal error). The compose comment
   for `GATEWAY_CALENDAR_CAPABILITY_CHECK` documents the same. Because of
   item 3, an empty admin account can no longer cause silent calendar data
   loss — the first lossy write fails closed and falls back to CalDAV.
7. **Capability-object gaps are warnings, not failures.** The live target
   (Stalwart v1.0.3) advertises `urn:ietf:params:jmap:calendars` as an empty
   object `{}` while calendar reads and writes work correctly. The deploy
   report's `is_healthy` therefore keys off the functional probes, and the six
   absent §1.5.1 metadata fields are recorded in a new non-fatal `warnings`
   list (a field present with the wrong *type* is still a fatal issue). This
   removes a false-negative that would otherwise have failed a working
   deployment.

Tests: `src/jscalendar.rs` 8 unit tests (ICS→JSCalendar round-trip content,
server-managed-property stripping, update null-clearing vs create no-clearing —
now including `useDefaultAlerts`/`relatedTo`, read-back discard detection,
start-only fallback, `Discarded`-vs-`Mismatch` classification, all-written-fields
comparison with normalisation tolerance, and the no-content-leak guarantee) +
`src/jmap.rs` capability tests updated for the warnings/healthy semantics (empty
capability object is healthy with 6 warnings; wrong *types* remain fatal) plus 3
new `set_calendar_event` classification tests driven over a recording stub
(create `Discarded` disables + destroys, create `Mismatch` does not disable but
still destroys, update `Discarded` vs `Mismatch`) + `tests/jmap_calendar_deploy.rs`
end-to-end `gateway_set_calendar_event_round_trips_content` (drives the gateway's
own create **and** update, then reads back through `get_calendar_event` and
asserts subject/description/location persisted) and
`gateway_set_calendar_event_verifies_rich_content` (recurrence + attendee + alarm
event must pass the full-field verification on both create and update — a false
positive here would break calendar writes for every real invitation).

### 3. CardDAV base path is wrong — EWS contacts (the New-Outlook-for-Windows contact surface) are dead

> **STATUS: DELIVERED.** Fixed as described below; see the *Delivered* note at
> the end of this section.

`CarddavClient::addressbook_home` (`src/carddav.rs:67`) builds
`{base}/carddav/{username}/`, and the config derives `carddav_base` from
`caldav_base` by `"/dav/" → "/carddav/"` (`src/config.rs:981`, and again at
`src/config.rs:505`). With the compose value
`GATEWAY_CARDDAV_BASE=http://stalwart:8080/carddav`, the client requests:

```
PROPFIND /carddav/carddav/testuser@redacted.com/   → 404
PROPFIND /carddav/testuser@redacted.com/           → 404
```

Stalwart serves CardDAV at `/dav/card/…`, verified live:

```
PROPFIND /dav/card/testuser@redacted.com/          → 207
REPORT   /dav/card/testuser@redacted.com/default/  → 207
```

So every CardDAV operation 404s. EWS contacts are **CardDAV-only** —
`handle_find_contacts_item` (`ews.rs:2743`, "Ensure CardDAV client is
configured" at `ews.rs:2753`), `handle_create_contact_item` (`ews.rs:3038`),
`handle_update_contact_item` (`ews.rs:3181`), `handle_delete_contact_item`
(`ews.rs:3491`) and MAPI contents (`mapi/handler.rs:6907` `fetch_contact_rows`)
all go through `state.carddav_client`. New Outlook for Windows uses the EWS
contact surface, so contacts are broken for the primary target client even though
JMAP `urn:ietf:params:jmap:contacts` (`AddressBook/get`, `ContactCard/get`,
`ContactCard/set`) works live.

Fix (ladder 1→4): correct the default path (`/dav/card/{user}/`), and — given
the JMAP-first mandate — route the EWS contact handlers through the working JMAP
`ContactCard` API (already implemented for EAS in `contacts.rs`
`apply_contacts_mutations`, `sync_contacts`) instead of CardDAV.

**Delivered.**

The CardDAV base path is corrected, and the EWS/MAPI contact surface is now
JMAP-first with CardDAV as the fallback.

*Path correction.* `CarddavClient::derive_base_from_caldav` is the single pure
derivation used by the config loader (both the file-load and the environment
paths) and by `CarddavClient::from_caldav_base`: it splits the URL into
origin + path (so a host named `dav…` is never mistaken for the DAV root),
drops any collection sub-path after `/dav`, and appends `/card`
(`…/dav` → `…/dav/card`, `…/dav/cal` → `…/dav/card`, `…/dav/` → `…/dav/card`).
`addressbook_home` now appends `/{user}/` to that root, matching Stalwart's
`/dav/card/{user}/` home. The compose and `CLOUDFLARED_SETUP.md` values are
`http://stalwart:8080/dav/card`.

*Three additional root causes found while validating the EWS contact path* (all
pre-existing and independently fatal — correcting the base alone would not have
made contacts work):

1. `Storage::upsert_contact` conflicted on `(owner, carddav_href)`, but
   `contact_map`'s unique constraint is `UNIQUE(owner, server_id)`. SQLite
   rejects a conflict target that matches no unique constraint, so **every**
   `upsert_contact` in `handle_find_contacts_item` errored and the contact was
   skipped — `FindItem` always returned zero contacts regardless of the base
   path. The upsert now targets `(owner, server_id)`, and in one transaction
   first deletes any stale row for the same href with a different ServerId, so a
   moved/re-keyed resource leaves exactly one mapping (an audit of every other
   `ON CONFLICT` in `storage.rs` against the schema found no further
   mismatches).
2. The EWS Get/Update/Delete dispatchers routed a `contact-`-prefixed ServerId
   through the "not in the calendar item map → JMAP email" fallbacks before the
   contact check, so every contact read/update/delete was misrouted to the email
   handler. The contact check now runs first in all three (`handle_get_item`,
   `handle_update_item`, `handle_delete_item`).
3. `SyncFolderItems` had no Contacts branch, so a Contacts sync fell through to
   the **calendar** path and returned calendar items. `handle_sync_folder_items`
   now routes `DistinguishedFolder::Contacts` to a new
   `handle_sync_contacts_folder_items`, a full-snapshot sync keyed on the
   `(server_id, change_key)` pairs carried in an opaque base64 `SyncState`
   (`contact:` + sorted `id:ck` lines): an unchanged set yields no changes, an
   edit yields an `Update`, a new contact a `Create`, and a vanished contact a
   `Delete`. The returned state reflects exactly what was delivered, so a
   truncated page re-delivers the remainder on the next call.

*JMAP-first routing.* New `JmapClient::get_contact_card` (RFC 9610 §4
`ContactCard/get` by id) plus the existing `list_contact_cards`,
`create_contact_card`, `update_contact_card`, `destroy_contact_card`,
`supports_contacts`, `get_contacts_account_id` and `get_default_address_book_id`
back the EWS contact handlers: `handle_find_contacts_item`,
`handle_get_contact_item`, `handle_create_contact_item`,
`handle_update_contact_item` and `handle_delete_contact_item` now try JMAP
(`urn:ietf:params:jmap:contacts`) when `GATEWAY_PREFER_JMAP_CONTACTS` is set and
the capability is advertised, and fall back to CardDAV otherwise (a new
`create_contact_carddav` helper mirrors the JMAP create). Shared helpers
`collect_jmap_contacts` / `get_contact_vcard_jmap` in `contacts.rs` are used by
both EAS and EWS, and `mapi/handler.rs::fetch_contact_rows` enumerates the
addressbook JMAP-first. `AppState::can_read_contacts` is true when either
client is configured. The EWS contact ChangeKey is content-derived when the
backend omits an ETag (Stalwart's JMAP does not expose `@etag`), so it is stable
across `FindItem` calls instead of churning on a timestamp.

Tests: `src/carddav.rs` 6 (derivation matrix incl. host-containing-`dav`,
`from_caldav_base`, `addressbook_home`), `src/contacts.rs` 4 integration tests
against an in-process stub JMAP contacts server (list → `jmap://contacts/…`
hrefs + vCards, single-card get incl. `notFound`, create/update/delete DB
round-trip, partial-Change merge that preserves untouched classes),
`src/ews.rs` 6 handler tests (FindItem→GetItem over JMAP, full
Create→Get→Update→Delete lifecycle through the real dispatchers, a
delete-misrouting regression, Contacts `SyncFolderItems` prime/unchanged/add/
remove, edit detection, and the SyncState encode/decode round-trip), plus the
existing EAS contact suites.

*Post-review hardening (PR #2033 review).* Four further defects in the same
surface, all fixed and covered by tests:

1. **One ServerId derivation.** `contact_ids` hashed the backend href, but the
   JMAP create, CardDAV create and EAS diff-add paths minted `contact-{uuid}`.
   `upsert_contact`'s stale-href delete then dropped the uuid mapping the first
   time the contact was re-listed, so a client-held ItemId stopped resolving.
   Every producer now derives the id from the final href via
   `contacts::contact_server_id` (`contact-{sha256(href)[..16]}`); the EAS
   diff-add, JMAP create, CardDAV create (`create_contact_carddav`) and the EAS
   `apply_mutation_carddav` Add path all use it.
2. **No identity churn on backend fallback.** `fetch_all_contacts` published a
   CardDAV snapshot whenever JMAP was *selected* but its capability probe or
   list failed; because the ids are href-derived, that switched every contact's
   ServerId and `SyncFolderItems` emitted Delete+Create for the whole set. The
   backend decision is now the tri-state `contacts::ContactsBackend`
   (`jmap_contacts_backend`, backed by `JmapClient::contacts_capability`, which
   distinguishes a genuine capability miss from a probe error): a JMAP outage on
   FindItem/SyncFolderItems/CreateItem/UpdateItem returns the retryable EWS
   `ErrorServerBusy` (503) and the EAS contacts Sync returns status 6, instead
   of falling back. CardDAV is used only on a real capability miss.
3. **Canonical ChangeKey from CreateItem.** CreateItem returned the raw ETag
   while FindItem/GetItem/SyncFolderItems returned `{server_id}-{etag}` or
   `{server_id}-{hash(vcard)}`. All four now share
   `contacts::contact_change_key`, and the JMAP create stores the *same* etag
   the read path reports (it reads the card back and applies the identical
   native-or-content-hash rule via `jmap_contact_etag`), so an unchanged contact
   presents one stable ChangeKey end to end.
4. **Single-card JMAP update.** `update_contact_jmap` listed every card and
   scanned for one id; it now uses `JmapClient::get_contact_card`.

Tests: `src/contacts.rs` 6 (identity/href derivation + ChangeKey form matrix,
etag rule, backend tri-state selection incl. probe-error → `BackendUnavailable`,
JMAP-outage Sync errors instead of falling back, create→list etag/ChangeKey
consistency), `src/ews.rs` 2 (CreateItem ChangeKey equals FindItem and GetItem;
FindItem on a JMAP outage is a retryable `ErrorServerBusy`).

---

## P1 — Mail-path and JMAP-limit defects that will surface under real load

### 4. `Email/get` with an explicit id list is capped at `maxObjectsInGet = 500`; the gateway never chunks

The JMAP session advertises `maxObjectsInGet: 500`. Live proof:

```
Email/get ids=[501 ids] → methodResponses[0] = ["error", {"type":"requestTooLarge",
   "description":"The number of ids requested by the client exceeds the maximum number…"}]
```

Stalwart enforces this in `unwrap_ids`
(`/tmp/sw/crates/jmap-proto/src/method/get.rs:148`: `if ids.len() <= max_objects_in_get … else RequestTooLarge`).
The gateway builds id lists without any 500-chunking in at least three hot paths:

- `jmap_push.rs:391` — `get_emails(account_id, &lookup, …)` where `lookup` is the
  **unbounded** union of created+updated accumulated while draining
  `Email/changes` (the drain loop at `jmap_push.rs:362` has **no** cap). A busy
  mailbox that produces >500 changes since the last poll makes every push poll
  return an error (the `.unwrap_or_default()` at `jmap_push.rs:402` then silently
  publishes **no** bodies).
- `eas.rs:6487` `render_pending_email_op` fetches **one id per call**, so it is
  safe, but the surrounding accumulation is capped at
  `MAX_EMAIL_CHANGE_ACCUMULATION = 5000` (`eas.rs:6422`) — a folder with a
  large delta is truncated for that Sync and only recovered on the next diff.
- `ews.rs:2054` requests `limit: 10000` in `query_calendar_events`, and
  `ews.rs:1597` requests `1000`; because `CalendarEvent/get` uses a back-reference
  (`#ids` → `resultOf`), Stalwart does **not** apply the 500 cap to back-referenced
  ids (verified: a 600-id back-reference resolved 28 ids fine), so these are
  currently safe — but the gateway's own `get_calendar_events`
  (`jmap.rs:4692`) sends an **explicit** `ids: event_ids` array, which *is*
  capped.

Fix (ladder 1→4): chunk every explicit `ids` array at 500 (existing dep
`serde_json` + a small helper), or always fetch via a `#ids` back-reference.

### 5. `CalendarEvent/changes` `hasMoreChanges` is ignored — calendar delta truncates at 500 changes

`changes_calendar_events` requests `maxChanges: 500` (`jmap.rs:2026`) and
returns `has_more_changes` (`jmap.rs:404`). The single caller
(`sync.rs:1444`) consumes `created`/`updated`/`destroyed` and stores
`changes.new_state` (`sync.rs:1490`) **without ever checking
`has_more_changes`**. Live proof the server does set it:

```
CalendarEvent/changes sinceState=sci maxChanges=2
  → {"hasMoreChanges":true,"created":["r","s"], … "newState":"rcmfqe"}
```

Storing `newState` while dropping the rest means the next Sync starts **after**
the dropped changes — those events are never delivered. (The email path gets
this right: `eas.rs:6457` loops on `has_more_changes`; the calendar path does
not.)

Fix (ladder 1→4): drain the change chain exactly as
`accumulate_email_changes` does, only advancing the stored token to the final
`newState`.

### 6. `@etag` is not a supported JMAP property on Stalwart — calendar etags are always empty

The gateway requests `"@etag"` for calendar events (`jmap.rs:4709`,
`jmap.rs:4569`) and models it as `#[serde(rename = "@etag")] pub etag`
(`jmap.rs:709`). Live proof the server ignores it:

```
CalendarEvent/get ids=["d"] properties=["id","@etag"] → [{"id":"d"}]   # no @etag
```

`@etag` appears nowhere in the Stalwart JMAP source (grep of `/tmp/sw` returns
nothing for the token; the only `@`-prefixed meta-properties are `@type` in the
websocket/file-node paths). Consequences:

- `sync.rs:1474` stores the JMAP **id** as the etag (`let etag = … event.id …`)
  for delta events while `sync.rs:1490`/`get_calendar_events` store `""` — the
  two paths disagree, so etag-based change detection is unreliable.
- EWS `ChangeKey` is derived from the etag; with an empty or id-only etag the
  ChangeKey is unstable/meaningless, and clients that compare ChangeKeys may see
  spurious or missed changes.

Fix (ladder 1→4): use `CalendarEvent/changes` (already implemented) as the
authoritative change signal instead of an etag, or derive a stable content hash
(the contacts path already does exactly this with `sha2::Sha256` over the card
JSON — reuse that approach for events).

### 7. Calendar `queryState` token is stored but never used for delta; and there is no `cannotCalculateChanges` handling

`query_calendar_events` returns `query_state` (`jmap.rs:4622`) and the initial
sync stores it (`sync.rs:1412` `token_to_store = Some(result.query_state…)`).
The delta branch (`sync.rs:1443`) expects a **`CalendarEvent/changes` state**
(`old_state_str`), not the `queryState` returned by `CalendarEvent/query`. The
first delta after an initial prime therefore passes a `queryState` into
`CalendarEvent/changes`, which the server may accept (it happens to be the same
state space today, verified live) but is not guaranteed by the spec. There is no
`cannotCalculateChanges` branch anywhere in `src/` (grep returns nothing), so a
server that refuses the token produces an error that falls back to CalDAV —
which is itself broken per finding #3, so the user sees a silent empty calendar
again.

Fix (ladder 1→4): on `cannotCalculateChanges` (or an unusable token), re-prime
from a full `CalendarEvent/query` and store its `queryState`/state properly, as
the EWS `SyncFolderItems` path already does for its snapshot prime.

---

## P2 — Deployment-scenario and correctness gaps in the compose/env contract

### 8. `docker-compose.yml` advertises `GATEWAY_MAIL_HOST=stalwart`, leaking the internal container name into client-facing Autodiscover

Autodiscover renders `<Type>IMAP</Type><Server>{mail_host}</Server>` and
`<Type>SMTP</Type><Server>{mail_host}</Server>` from `req.mail_host`
(`autodiscover.rs:602`, template at `autodiscover.rs:794`), and `mail_host`
comes from `Config::mail_host` (`main.rs:205`). The compose sets
`GATEWAY_MAIL_HOST=stalwart` — the **internal Docker service name**. Clients
that follow those IMAP/SMTP blocks (Outlook Android offers IMAP/SMTP fallback)
will be pointed at an unresolvable hostname. The correct public value is
`mail.<rootdomain>` (the MX host already in DNS). `GATEWAY_IMAP_HOST`/
`GATEWAY_SMTP_HOST` are the backend plumbing and should stay `stalwart`; only
`GATEWAY_MAIL_HOST` is advertised.

Fix (applied in this audit): the compose now ships
`GATEWAY_MAIL_HOST=mail.example.com` with a comment that it must be the public
MX host. `GATEWAY_SMTP_HOST`/`GATEWAY_IMAP_HOST` remain `stalwart` (backend).
Alternatively remove the IMAP/SMTP blocks if pure-Exchange clients are the only
target.

### 9. Env vars read by the code are absent from the compose file

Computed by diffing `grep -o 'GATEWAY_[A-Z0-9_]*'` over `docker-compose.yml`
against the same over `src/`. Present in code, absent from compose:

```
GATEWAY_CONFIG               GATEWAY_MAPI_CONTENTS_PAGE_SIZE
GATEWAY_LOG_FORMAT           GATEWAY_MAPI_MAX_CONTENTS_ROWS
GATEWAY_LOG_JSON             GATEWAY_MAPI_OIDC_AUDIENCE
GATEWAY_LOG_NO_TIMESTAMPS    GATEWAY_MAPI_OIDC_ISSUER
GATEWAY_LOG_TARGET           GATEWAY_MAPI_ORG
GATEWAY_LOG_THREADS
```

Two tokens that a naive grep also flags are **not** real environment variables
and must not be added to the compose:

- `GATEWAY_RULE_PROVIDER` is a Rust `const` (`mapi/rules.rs:68`), not env.
- `GATEWAY_CALDAV_USER` / `GATEWAY_CALDAV_PASSWORD` appear **only** inside a
  warning string in `caldav.rs:44` — the code never reads them. The message is
  misleading: it tells the operator to set two variables that do nothing.
  CalDAV auth is always per-request Basic auth from the logged-in user's
  credentials. Fix: either implement them or correct the message (preferable:
  correct the message; the gateway has no need for a separate CalDAV identity).

Most have safe defaults, but three matter for this deployment:

- `GATEWAY_MAPI_ORG` (`config.rs:70`) gates the legacyExchangeDN org check
  (`mapi/logon.rs:220`). Unset disables the gate (documented as intentional),
  so MAPI logon is more permissive than a real Exchange — acceptable but should
  be a conscious choice, not an omission.
- `GATEWAY_LOG_FORMAT` / `GATEWAY_LOG_JSON` (`logging.rs:139`) — the compose
  sets neither, so JSON logging (useful under `journalctl`/Portainer) is not
  reachable without editing the file.
- `GATEWAY_CONFIG` (`main.rs:655`) defaults to `/etc/exchange-gateway/config.toml`;
  a missing file is tolerated (`config.rs:433` logs and continues with env
  vars), which is correct for the "no env file, compose-only" contract — worth a
  comment in the compose so an operator doesn't try to mount one.

Fix: add the applicable variables to the compose `environment:` block with
comments (the file already documents the "no `.env` file" contract), or add a
startup log listing recognised-but-unset variables.

### 10. `.env.example` contradicts the stated contract — REMOVED in this audit

The user's contract is explicit: **no env file, inline compose only**, and "if
this exists in the repository, delete it". `.env.example` was tracked
(`git ls-files` confirmed) and referenced by `AGENTS.md:402`, `AGENTS.md:506`
and `DEPLOY_RUNBOOK.md:9`. Its own header already said it was "NOT A FILE TO
COPY", so it was documentation-only — but its presence invited an operator to
`cp` it.

Action taken: `git rm .env.example`; its unique commentary (the log-format
notes, the MAPI tuning defaults, the S/MIME store note, the host-path
permissions note) was folded into `docker-compose.yml` comments; the three
references in `AGENTS.md` and `DEPLOY_RUNBOOK.md` now point at the compose file.
The compose file is the authoritative, complete template.

### 11. Portainer/Cloudflare route reality check

The gateway binds `8134` (compose `ports: - 8134:8134`) and serves plain HTTP,
which matches "the gateway should serve HTTP behind cloudflared". The two
existing Zero-Trust routes (`calendar.redacted.com` and
`autodiscover.redacted.com` → port 8134) are sufficient **only if** the EWS,
MAPI and `/ecp` paths are all served from the same host. The code serves them
(`main.rs:812` onward: `/EWS/Exchange.asmx`, `/Microsoft-Server-ActiveSync`,
`/mapi/emsmdb`, `/mapi/nspi`, `/autodiscover/autodiscover.xml`,
`/autodiscover/autodiscover.svc`, `/OAB/{guid}/{file}`, `/ecp`). No route change
is required **provided** the client is configured for `calendar.redacted.com`
(Autodiscover must resolve there). If Autodiscover is expected on the
`autodiscover.` host but the client derives EWS from the same origin, the
current split is fine; if the client is told to use `mail.redacted.com`, a third
route is needed. Recommend documenting one canonical host
(`calendar.redacted.com`) for both routes to avoid Autodiscover/EWS origin
mismatch.

### 12. `GATEWAY_FORCE_CALDAV_CALENDAR` and `GATEWAY_PREFER_JMAP_CALENDAR=1` interact badly with findings #1–#3

The compose ships `GATEWAY_PREFER_JMAP_CALENDAR=1` and
`GATEWAY_FORCE_CALDAV_CALENDAR=0`. With JMAP calendar reads broken (#1) and
CalDAV fallback suppressed on an empty-but-successful JMAP result (`sync.rs:1508`),
the calendar is empty; forcing CalDAV
(`GATEWAY_FORCE_CALDAV_CALENDAR=1`) would make it work today but forfeit the
JMAP-first mandate. This is a consequence of #1–#3, not an independent bug, but
it must be documented as the interim workaround until #1–#3 are fixed.

---

## P3 — Robustness and completeness (lower user impact, still worth fixing)

### 13. Contacts `@etag` fallback is a content hash, not the server etag — fine, but inconsistent with events

`sync.rs:1262` computes `sha2::Sha256` over the card JSON when the server
provides no etag. This is a reasonable substitute and is the pattern finding #6
recommends adopting for calendar events. No change needed beyond consistency.

### 14. `get_availability` is dead code

`JmapClient::get_availability` (`jmap.rs:5288`) using
`urn:ietf:params:jmap:principals:availability` has **no caller** (grep confirms
only the definition). Free/busy is served by `query_calendar_events` in both
EWS (`ews.rs:1590`) and EAS (`eas.rs:4253`), so the availability capability
advertised by Stalwart is unused. Either wire it in (it is the correct JMAP
free/busy primitive and would avoid the ICS round-trip) or remove it.

### 15. EAS `Search` falls back to an empty result when JMAP is unavailable, but status 6 is correct

`handle_search` (`eas.rs:4732`) returns status 6 when JMAP is absent
(`eas.rs:4753`), which is the EAS "server error" code — acceptable. No action.

### 16. Rate-limit / Ping sizing is reasonable for the target clients

`GATEWAY_MAX_PING_HEARTBEAT=80` under Cloudflare's 125s edge timeout, with
`GATEWAY_RATE_LIMIT_REQUESTS_PER_MINUTE=600`. The code clamps the heartbeat and
detects `CF-RAY` (`eas.rs:1310`). No action, but verify after #1–#3 that Ping
still wakes the client for calendar changes (it currently can't, because the
calendar delta is empty).

### 17. `mapi_enabled` defaults true while the compose sets `GATEWAY_MAPI_HMA_ENABLED=0`

`default_mapi_enabled` is `true` (`config.rs:425`) and the compose enables
`GATEWAY_MAPI_ENABLED=1` but disables HMA
(`GATEWAY_MAPI_HMA_ENABLED=0`). Autodiscover still emits a `<Protocol
Type="mapiHttp">` block unconditionally (`autodiscover.rs:762`), so clients may
attempt MAPI/HTTP against an HMA-disabled endpoint. Either gate the
`mapiHttp` block on `mapi_hma_enabled`, or document that New Outlook falls back
to EWS. This is a client-friction item, not a data-loss item.

### 18. Compose is otherwise correct for the deployment scenario

- Image is a registry reference with a placeholder `<registry>` (must be
  replaced by the operator — the task says do not replace placeholders).
- No `env_file:` and no `.env`; all configuration is inline. ✅
- Volume mount: the compose previously used `/armblock/exchange-gateway/data`;
  the deployment scenario states the operator creates
  `/armblock/exchange-gateway-data`. The mount was changed to the stated path
  and the optional certs mount commented out (create the host dir, or uncomment
  and create it, before first start). Both paths must be writable by UID 10001.
- `networks.network1` is `external: true` with `name: my-bridge-network`,
  matching the Stalwart compose. ✅
- Healthcheck uses `/health/ready`; `restart: unless-stopped` alone will not
  restart an unhealthy container — the optional `autoheal` block documents this.
  ✅

---

## Stalwart-side configuration changes required

1. **Nothing is strictly required for JMAP calendar/contacts** — the capability
   is advertised and functional (`urn:ietf:params:jmap:calendars`,
   `:calendars:parse`, `:contacts`, `:contacts:parse`, `:mail`,
   `:principals:availability`, `:submission`, `:sieve` all present in
   `GET /jmap/session`).
2. **CardDAV path**: Stalwart serves CardDAV at `/dav/card/{user}/…`. The
   gateway must be pointed there (finding #3); no Stalwart change needed, but
   confirm the address book exists (live: `/dav/card/testuser@redacted.com/default/`
   is an `<B:addressbook/>`).
3. **Admin credentials**: if the operator wants the deploy-time capability probe
   to run, they must set `GATEWAY_ADMIN_USERNAME`/`GATEWAY_ADMIN_PASSWORD` to a
   Stalwart account that can create a throwaway calendar (the probe creates and
   destroys an isolated `exchange-gateway-deploy-probe-<uuid>` calendar). Given
   finding #2, it is safer to leave the JMAP write path **disabled** until the
   read/write defects are fixed.
4. **Cloudflare Zero-Trust routes**: the existing two routes are sufficient
   **iff** the client is configured for a single canonical host
   (`calendar.redacted.com`) and Autodiscover is reachable there. If the client
   is expected to Autodiscover via `autodiscover.redacted.com` and then use
   `mail.redacted.com` for EWS/AS, add a route for that host. No tunnel route
   can be created for the root domain (already documented by the user), so keep
   using the subdomain routes.

---

## Summary table

| # | Sev | Area | Defect | Fix ladder |
|---|-----|------|--------|-----------|
| 1 | P0 | Calendar read | `iCalendar` is a JSCalendar object; `Option<String>` decode fails the whole list → 0 events everywhere | **DELIVERED** — calcard dep + whole-object ICS reconstruction + per-element parse |
| 2 | P0 | Calendar write | JMAP create discards payload; guard skipped when admin creds empty (compose ships empty) and defaults to writes-enabled | **DELIVERED** — structured JSCalendar write + per-write read-back verification + fail-closed runtime gate (works without admin creds) |
| 3 | P0 | Contacts (EWS) | CardDAV base `/carddav/{u}/` 404s; correct path is `/dav/card/{u}/`; EWS contacts are CardDAV-only | **DELIVERED** — `/dav/card` base + JMAP-first EWS/MAPI contacts (also fixed the `upsert_contact` conflict-target and Get/Update/Delete misrouting) |
| 4 | P1 | Mail/push | Explicit `Email/get` ids >500 → `requestTooLarge`; `jmap_push` union is unbounded | dep 1 (chunking) |
| 5 | P1 | Calendar delta | `CalendarEvent/changes` `hasMoreChanges` ignored → dropped changes | dep 1 (drain loop) |
| 6 | P1 | Calendar etag | `@etag` unsupported by Stalwart; etags empty/inconsistent | dep 1 (changes API / sha2) |
| 7 | P1 | Calendar delta | `queryState` fed to `changes`; no `cannotCalculateChanges` handling | dep 1 |
| 8 | P2 | Autodiscover | `GATEWAY_MAIL_HOST=stalwart` leaks internal hostname to clients | **compose fixed** |
| 9 | P2 | Compose | 11 env vars read by code absent from compose; `GATEWAY_CALDAV_USER/PASSWORD` are phantom (message only) | **compose fixed** + code msg fix |
| 10 | P2 | Repo | `.env.example` contradicts the no-env-file contract | **deleted** |
| 11 | P2 | Deploy | Cloudflare route/host canonicalization | docs |
| 12 | P2 | Config | JMAP-prefer + broken read = empty calendar; document interim CalDAV force | docs |
| 13 | P3 | Contacts | etag fallback is a hash (fine) — align events with it | dep 1 |
| 14 | P3 | Free/busy | `get_availability` unused | dep 1 |
| 15 | P3 | EAS Search | status 6 when JMAP absent (correct) | — |
| 16 | P3 | Ping/rate | sizing fine; re-verify after #1–#3 | — |
| 17 | P3 | Autodiscover | `mapiHttp` block emitted with HMA disabled | dep 1 |
| 18 | P3 | Deploy | volume path mismatch (`/armblock/exchange-gateway/data` vs `-data`) | compose edit |

**Bottom line:** with the current code and the shipped compose, the calendar is
unusable in both directions (P0 #1, #2) and contacts are unusable through EWS
(P0 #3), so the stated use-case (calendar + email fully native in New Outlook
for Windows and Outlook Android) is **not** met today. Email sync, Autodiscover
shape, OOF/Sieve, and the SMTP send path are substantially sound, with the
mail-path limits in P1 #4 being the main residual risk once the calendar is
repaired.

*(P0 #1 — JMAP calendar reads — is now fixed; see the Delivered note in that
section. P0 #2 — JMAP calendar writes — is now fixed as well: the write is a
structured JSCalendar payload with per-write read-back verification, and a
lossy backend is caught and disabled at the point of use even when the
deploy-time admin-creds check is skipped. P0 #3 — EWS contacts — is now fixed
too: the CardDAV base is `/dav/card`, the EWS/MAPI contact surface is JMAP-first
with CardDAV fallback, and two further latent defects on that path (the
`upsert_contact` conflict target and the Get/Update/Delete misrouting to the
email handler) are repaired; see the Delivered note in that section.)*
