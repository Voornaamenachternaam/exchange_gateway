# exchange_gateway — agent memory

## Project
Rust gateway (~96k LOC) translating Microsoft Exchange/Outlook protocols (EAS via
WBXML, EWS, MAPI/HTTP, Autodiscover, SMTP) to a Stalwart Mailserver v1.0.3
backend (JMAP-first, CalDAV fallback for calendar). Target clients ONLY:
New Outlook for Windows (20251205004.10), Outlook Android (5.2641.1).
Specs: `exchange_protocols/` (v20250520 .txt docs). Toolchain: Rust 1.99.1.
EAS specs v20250520 (16.1); older EAS compat NOT needed.

## Build / test
- `cargo test` — lib + integration suites (`main.rs` bin tests, `protocol_fixtures`, `snapshots`, `jmap_calendar_deploy`, doc). All must pass. (Run it for the current count; exact numbers are not pinned here.)
- `cargo clippy --all-targets` — must be 0 warnings. `#[allow(clippy::too_many_arguments)]` is the accepted escape for irreducible request params (email.rs, jmap.rs, eas.rs handle_email_sync).
- `cargo build --release` — ~5min, must be warning-free.
- Precedence for gaps: existing deps → new quality deps → Rust 1.99.1 std → custom code.

## Hard-won conventions
- **WBXML decode namespace rule (src/wbxml.rs)**: a document's ROOT code page
  is its default namespace → tags on the root page expand UNQUALIFIED;
  SWITCH_PAGE-reached tags keep the `Prefix:` form (e.g. `<AirSyncBase:Body>`
  inside a Sync doc rooted on page 0). Regressions guarded by
  `sync_root_page_expands_unqualified_and_switched_pages_prefixed` and the
  ResolveRecipients round-trip tests. Code page 0 names are stored unqualified
  in TAG_TO_NAME; every other page is `Prefix:Local`.
- **WBXML encode prefix rule**: a `Prefix:Tag` element needs its prefix DECLARED
  (`xmlns:AirSyncBase="AirSyncBase:"`) — undeclared prefixes resolve to the
  ambient default namespace and can fail tag lookup. eas.rs response templates
  always declare all namespaces they use. §11 refinements: namespace URIs are
  accepted with OR without the trailing colon (`namespace_uri_to_code_page` —
  [MS-ASWBXML] §3 prints URIs bare); an undeclared CANONICAL prefix
  (`AirSyncBase:`) resolves to its own code page, and unqualified descendants
  fall back to the implicit ROOT code page — so decoded XML (no xmlns)
  re-encodes byte-identically instead of hash-resolving wrong-page
  same-local-name tokens (Body→Email:Body, LastName→GAL:LastName). Pinned by
  `ms_aswbxml_section3_example_roundtrips_byte_exact` (spec's 106-byte §3
  example, encode + decode→re-encode).
- **WBXML byte-array OPAQUE rule (§11, delivered)**: byte-array-typed elements
  ([MS-ASDTYPE] §2.7.1) travel as OPAQUE raw bytes (`0xC3` + mb_u_int32 len +
  bytes); base64 exists ONLY in the in-memory XML form. `is_byte_array_element`
  is the complete spec-declared inventory: (2,0x34) GlobalObjId [deliberate],
  (15,0x20) Search:ConversationId, (16,0x12) GAL:Data, (17,0x1F)
  AirSyncBase:Content, (20,0x18) ItemOperations:ConversationId, (21,0x10)
  ComposeMail:Mime, (22,0x09/0x0A) Email2:ConversationId/Index. String-typed
  base64 bearers (ItemOperations:Data, Contacts:Picture) stay STR_I;
  `wbxml_conformance_byte_array_inventory_matches_specs` grounds each entry in
  the in-repo spec text and asserts set equality.
- **WBXML decode fail-closed (§11, delivered)**: per [MS-ASWBXML] §2.1.3 the
  decoder rejects EXT_I/PI/LITERAL_C, EXT_T, LITERAL_A/EXT, LITERAL_AC tokens,
  tag tokens with the attribute bit, unknown (page, token) pairs, END
  underflow, data after root, truncated/unclosed documents. ENTITY (0x02) and
  STR_T (0x83) remain handled leniently (pre-existing; string table is always
  empty from Exchange). Encode now errors on PI/DOCTYPE (unrepresentable) and
  encodes CDATA content (previously dropped). Conformance suite also includes
  the full-table diff vs [MS-ASWBXML].txt (608/608 both directions,
  NAME_TO_TAG exact inverse) and the pinned 29-token non-16.1 legacy inventory.
- **WBXML encode fail-closed + content aggregation (§11, delivered)**: an
  element's only encodable attributes are namespace declarations — any other
  attribute, malformed/duplicate attributes, and a *used* namespace (prefix or
  default) bound to an unknown URI are hard errors (previously silently
  dropped / hash-fallback); character data outside the document element is
  rejected before and after the root. Element content is aggregated across
  quick-xml's Text/CData/GeneralRef events into exactly ONE token per element
  (STR_I, or one OPAQUE for byte-array elements with interior whitespace
  stripped from the base64, xsd:base64Binary semantics) — encoding per event
  emitted one token per segment and corrupted split byte-array values.
  Pinned by `encode_rejects_non_namespace_attributes`,
  `encode_rejects_declared_unknown_namespace_bindings`,
  `encode_rejects_character_data_outside_document_element`,
  `split_element_content_encodes_as_single_wbxml_token`.
- EAS request/response XML parsing uses quick-xml LOCAL names (prefix-agnostic);
  string helpers (`extract_all_tag_blocks`, `extract_first_tag_text`,
  `extract_all_tag_text`) match literal unqualified tag names — safe because
  decode output keeps unqualified names only where they're the document
  default namespace.
- Context structs to dodge clippy arg limits: `SyncCtx`, `EmailSyncCtx` (state,
  jmap, account_id, username, password, collection_id, state_collection_id,
  window, options, conversation_mode — Copy, destructured with `*ctx`).
- **Body-value opt-in (jmap.rs)**: `get_emails`/`get_email` take a
  `fetch_bodies: bool` mapping to RFC 8621 §4.4.1
  `fetchTextBodyValues`/`fetchHTMLBodyValues` — pass `true` only from callers
  that render body text (Sync delta/ItemOperations Fetch, EWS GetItem/
  SyncFolderItems, MAPI body-stream + cell materialization); metadata-only
  callers (threading headers, push mailboxIds, attachment rosters, submit
  envelope) pass `false`. New callers must pick deliberately, not copy the
  nearest flag.

## Sticky Sync options (§10 work, delivered)
- Per-collection `<Options>` ([MS-ASCMD] §2.2.3.125.6) resolve sticky and
  persist per device-collection via storage `get/set_sync_collection_options`
  (scoped key `{collection_id}::{device_id}`), surviving gateway restarts.
- src/eas_sync_options.rs: `EasSyncCollectionOptions`, negotiate_body
  (AllOrNone fall-through, Type chain 2→1), UTF-8-safe truncation, preview;
  tests `parse_outlook_android_option_matrix` / `parse_new_outlook_windows_option_matrix`
  pin both clients' exact option matrices.
- Renderers: email `render_jmap_email_as_eas_application_data_with_options`
  (Body + BodyPart per [MS-ASCON]); calendar
  `render_calendar_app_data_with_options` (sync.rs, Body prefix per [MS-ASAIRS]).
- Status codes: Sync invalid key → 3 (re-prime SyncKey 0), FolderSync 9,
  GetItemEstimate 4; ItemOperations Fetch: 2/3/6/14/16/164 per [MS-ASCMD] §2.2.3.177.8.

## ItemOperations/Fetch (§12 work, delivered)
- **Response Fetch child shape** ([MS-ASCMD] §2.2.3.67.1): response `<Fetch>`
  has NO `<Store>` echo; `Status` leads, then the address elements the request
  used — `<AirSyncBase:FileReference>`, or `<AirSync:CollectionId>`+
  `<AirSync:ServerId>`, or `<AirSync:CollectionId>`+`<Search:LongId>` — then
  `<AirSync:Class>` (§2.2.3.27.3: airsync namespace, NOT page 20) and
  `<Properties>`. eas.rs ItemOperations response templates must declare
  xmlns:AirSync="AirSync:" and xmlns:Search="Search:". Bare `<Class>`/
  `<CollectionId>`/`<ServerId>` inside an ItemOperations-rooted (code page 20)
  document resolve to NO [MS-ASWBXML] token → WBXML encode failure → 500; this
  was a live bug for every as_wbxml item fetch until §12.
- `Range` request parse: inclusive `m-n` → half-open window; response echoes the
  AUTHORITATIVE window in `<Range>m-n</Range>` (only when the request had a
  Range) + `<Total>` (whole-item size). Statuses per §2.2.3.177.8: 9/2/8/11/10/
  15/16/3/6/14/17 (17 = window clamped at EOF = partial success).
- Multipart ([MS-ASHTTP] §2.2.1.1.2.5 `MS-ASAcceptMultiPart: T`):
  `multipart_item_operations_response` builds §2.2.1.10.1.1 layout (PartsCount
  u32 LE + PartMetaData{Offset,Length} + parts; part 0 = WBXML, parts 1.. =
  raw bytes). `<Part>n</Part>` replaces attachment `Data` (direct Properties
  child) and the email body's `AirSyncBase:Data` (direct AirSyncBase:Body
  child, per the §4.10.5.2 decoded example).
- `JmapClient::download_blob_range` (jmap.rs): streaming windowed blob fetch
  with typed `BlobFetchError` (InvalidBlobId/NotFound/AccessDenied/Server/
  TooLarge/RangeTooLarge/RangeStartsPastEof), Content-Length short-circuit,
  mid-stream budget abort. `calendar_attachment_outcome` (eas.rs) mirrors it
  for gateway-managed calendar attachments. Budget = `max_attachment_bytes`
  (default 5 MiB, floor 1024).

## Session notes
- AUDIT5.MD §2 (EAS folder-management commands) is COMPLETE and its section
  documents the delivered behavior. New module `src/eas_folders.rs` (schema
  v11 `folder_map`) implements `FolderCreate`/`FolderDelete`/`FolderUpdate`
  against Stalwart as the system of record: mail folders → JMAP `Mailbox/set`
  (create/update/destroy, with `onDestroyRemoveEmails` and deepest-first
  subtree destruction), calendar-family folders (Types 13/15/16/17) →
  CalDAV `MKCALENDAR`/`DELETE`/`PROPPATCH` displayname, persisting the mapping
  in `folder_map` so user-created folders survive restarts and surface in
  `FolderSync`/`GetHierarchy`. Type 14 (Contacts) is rejected (Status 10)
  because CardDAV collection creation is unsupported. The EAS ServerId of a
  mail folder is the JMAP mailbox id itself; a CalDAV folder's is its
  collection href (kept stable across a rename). An EWS `UpdateFolder` rename
  of an EAS-created mail folder is mirrored into `folder_map` and journaled as
  `folder-update` (no-op when no mapping row exists; a failed mirror write is a
  retryable error, never a silent success). The rename is held to the SAME name
  rules EAS `FolderUpdate` enforces (reserved system name / >64 chars /
  case-insensitive sibling collision) BEFORE the backend is touched, and
  `update_mailbox`'s bool is honoured (mirror only on `Ok(true)`; `Ok(false)` is
  a retryable error). `GetHierarchy` now appends
  user-created folders; the incremental `FolderSync` reports folder-hierarchy
  changes journaled in `change_journal` (`resource_href='folder'`, filtered out
  of the EWS item-delta feed) so a mutation reaches an already-primed client as
  Add/Update/Delete. `GetAttachment` streams the blob through
  `JmapClient::download_blob_range` as the response body + blob Content-Type
  (missing name → Status 2; missing/empty → HTTP 500). `Find` stays the legacy
  GAL/mailbox compatibility shape. Status codes per [MS-ASCMD]
  §2.2.3.177.8/§6.10–§6.19 (1/2/3/4/5/6/10). The EAS and EWS email deltas now
  drive the mailbox-scoped `Email/queryChanges` feed (`filter.inMailbox` +
  the prime's `receivedAt` sort), never the account-wide `Email/changes`, so a
  single collection never receives another folder's mail; a `queryChanges`
  membership feed misses in-place flag edits, so BOTH the EAS and EWS email
  deltas keep a per-collection keyword baseline and diff it as `<Change>`/
  `<t:Update>` — the diff is bounded per delta (a rotating per-collection scan
  position in `email_keyword_scan` walks at most `KEYWORD_SCAN_MAX_PAGES` pages,
  wrapping at the end), so a delta never pays O(mailbox) in JMAP calls. Tests: `src/eas_folders.rs` (a
  combined JMAP+CalDAV axum stub records the exact wire calls; session apiUrl
  must equal the base origin so the client POSTs to `/`, and CalDAV paths are
  percent-encoded by reqwest's Url::join) plus the `src/sync.rs` and `src/eas.rs`
  delta/scoping tests. clippy 0 (`-D warnings`), rustfmt clean on ALL touched
  files, release warning-free.
- AUDIT5.MD §2 PR-review hardening (delivered). `JmapClient::destroy_mailbox`
  returns a typed `MailboxDestroyOutcome` (`Destroyed`/`AlreadyGone`/`Refused`):
  a JMAP method-level `error` and a `Mailbox/set` result with no `destroyed`
  array are hard `Err`s, a `notDestroyed` `notFound` entry is `AlreadyGone`
  (RFC 8620 §5.3 — the desired end state, so an EAS FolderDelete/EWS/MAPI
  subtree retry after a partial delete resumes), and any other `notDestroyed`
  type is `Refused` (the mailbox still exists — the mirror must NOT be pruned,
  else the next reconcile resurrects it). Callers (eas_folders, ews_folder_ops,
  mapi handler) match the typed outcome. `query_all_mailboxes` pages
  `Mailbox/query` at `MAILBOX_QUERY_PAGE` (500) and stops on the requested
  `total` (calculateTotal), NOT a "short page means done" heuristic — a server
  that clamps `limit` below 500 would otherwise truncate the subtree walk.
  MAPI `RopDeleteFolder` ([MS-OXCFOLD] §2.2.1.3) now rejects any
  `DeleteFolderFlags` bit outside 0x01|0x04|0x10 (ecInvalidParam, store
  untouched), accepts 0x10 as a valid no-op bit, and reports
  `PartialCompletion` (new `RopDeleteFolderResponse`, MS-OXCROPS §2.2.4.3.2)
  when it succeeds for only a subset of the subtree. The EAS email delta's
  keyword baseline seeds a row for every delivered `<Add>` (`render_pending_email_op`)
  so a flag edit before the rotating scan reaches the message is still reported
  — a missing row on the scan's first observation is recorded WITHOUT a
  `<Change>` (the Add already carried the current flags; emitting a Change there
  fired for messages the client never received). A scoped folder's delta is the
  account-wide calendar feed, so an event moved OUT of that folder's calendar
  lingers on the device until a client resync; tombstoning the scoped-filtered
  ids is NOT done because `item_map` is owner-global (no collection column) and
  calendar-event hrefs are account-scoped
  (`jmap://calendar/{account}/{id}`), so tombstoning them would delete every
  OTHER calendar's events from the shared rows. The residual is bounded and
  cross-calendar-safe; a correct fix needs a per-collection calendar scope on
  `item_map`. Folder mutations journal atomically with the mapping:
  `Storage::upsert_folder_journaled` (folder-add/folder-update) and
  `delete_folders_journaled` write the `folder_map` row(s) AND the
  `change_journal` row(s) in ONE transaction, so a FolderSync success is
  returned only once an already-primed device can learn about the change (the
  old separate `record_hierarchy_change` calls could ack a create/delete whose
  journal row was never written). MAPI
  `RopHardDeleteMessagesAndSubfolders` ([MS-OXCFOLD] §2.2.1.10) removes the
  folder's messages AND its whole descendant subtree (the folder itself
  survives), walking it deepest-first with `onDestroyRemoveEmails` set on every
  child destroy (a child that still holds mail is otherwise refused). The EAS
  keyword baseline's rotating scan holds its position (does not advance past
  the window) when a baseline ROW READ fails — `Storage::get_email_keyword`
  propagates a decode error as `Err` rather than treating it as a missing row,
  so a flag change is never silently swallowed; and the per-delta scan's
  upserts SKIP ids delivered as an `<Add>` this delta, so the Add's fresh seed
  wins over the older scan snapshot (no duplicate flag `<Change>` next delta).
  The overflow branch persists the keyword baseline ONLY once the undelivered
  remainder is durably queued AND the (best-effort) sync-token write has
  committed — a failed queue write holds the baseline (and fails the delta
  closed), and a failed token write still acknowledges the clipped window but
  holds the baseline, so the re-diff re-derives the discarded flag changes.
  The completing branch's token write is fail-closed: a failed `set_sync_key`
  returns a retryable error and never advances the baseline.
- AUDIT.md §10 (EAS Sync wire-exactness) is COMPLETE and its section now
  documents the delivered behavior — keep it accurate when touching Sync.
- AUDIT.md §11 (WBXML conformance hardening) is COMPLETE (commit 7b7d242):
  full-table diff test parses exchange_protocols/[MS-ASWBXML].txt at test
  time (608 tokens, both directions, NAME_TO_TAG exact inverse); version
  matrix pins the 29 non-16.1 legacy tokens; byte-array→OPAQUE inventory
  completed + spec-grounded; §3 example byte-exact both directions; decode
  fail-closed per §2.1.3; encode fails on PI/DOCTYPE, encodes CDATA. Suite:
  901 tests green, clippy 0, release build warning-free.
- AUDIT.md §12 (ItemOperations/Fetch attachment correctness) is COMPLETE:
  Range byte-range fetch (both JMAP-blob and gateway-managed calendar
  stores), full §2.2.3.177.8 status table, MS-ASAcceptMultiPart multipart
  delivery, memory-bounded base64 (push_base64) + streaming budget abort,
  §2.2.3.67.1 response-Fetch shape fix (Store echo removed, Status first,
  AirSync-namespace Class/CollectionId/ServerId, Search:LongId echo), and two
  dead attachment.rs renderers removed. Suite: 928 tests green
  (lib 892 + fixtures 22 + snapshots 11 + jmap_calendar_deploy 2 + doc 1),
  clippy 0 warnings, release build warning-free.
- AUDIT.md §13 (MeetingResponse/iMIP integrity) is COMPLETE and its section
  documents the delivered behavior. One RSVP pipeline (meeting/rsvp.rs
  `apply_rsvp`) serves both front doors (EAS MeetingResponse [MS-ASCMD]
  §2.2.1.11 via eas.rs, EWS AcceptItem/TentativelyAcceptItem/DeclineItem via
  ews.rs `handle_meeting_response_object`): organizer self-response rejected
  pre-side-effect (status 2 / ErrorCalendarIsOrganizer*), instance scoping
  per §2.2.3.92.1 (series master vs exception, RECURRENCE-ID REPLY with the
  instance's own times), attendee copy PARTSTAT/ResponseType/X-MS-APPOINTMENT-
  REPLY-TIME write-through with SCHEDULE-AGENT=CLIENT pinning, SMTP delivery
  with the user's own identity, duplicate-RSVP idempotence via the
  `meeting_rsvp` store (schema v9).
- §13 code-review triage COMPLETE (5 substantive findings, all fixed):
  (1) idempotence is revision-aware — the stored row carries the REQUEST
  SEQUENCE it answered; `is_duplicate_rsvp` suppresses only an exact
  (decision, SEQUENCE) repeat, so an organizer reschedule (SEQUENCE bump,
  RFC 5546 §3.2.1.4) re-delivers the same decision; (2) REPLY and COUNTER are
  split SMTP senders (`ItipSendCtx` Copy-context + (ctx, invitation, req)
  signatures) with per-method addressing and a shared `reply_window` helper
  for instance times; the COUNTER is never recorded so a failed COUNTER
  retry re-proposes without a second REPLY; (3) recurrence expansion is
  DST-aware — the event's TZID (chrono-tz) anchors the wall clock (weekly
  09:00 Europe/Berlin → 08:00Z winter, 07:00Z summer; the summer instant is
  accepted), unparseable timezones fail OPEN, and the rrule-iterator scan
  terminates at the first occurrence past the target (rrule's `before()`
  does NOT apply to direct iteration — verified in the 0.14.0 source);
  (4) organizer-bound patching (Sourcery tampering concern) — the stored
  copy is re-read before patching and only patched when its organizer
  matches the REQUEST's (`same_meeting_organizer`, normalized: mailto:
  stripped, case-insensitive), so UID collisions/forged REQUESTs can't
  clobber unrelated events and the decision lands on the copy as it exists
  now; (5) EWS response-object XML is extracted nesting-aware
  (`extract_first_tag_block` in ews.rs) — reply Body and ProposedStart/End
  are read from the AcceptItem/TentativelyAcceptItem/DeclineItem block; the
  old whole-envelope scan matched the SOAP `s:Body` and leaked envelope text
  into the reply email. Storage: `MeetingRsvpRecord<'a>` params struct
  (MeetingStateParams convention) replaces the 8-arg upsert. Suite after
  fixes: 974 green (lib 938 + fixtures 22 + snapshots 11 +
  jmap_calendar_deploy 2 + doc 1), clippy 0, fmt clean on touched files
  (storage.rs, rsvp.rs, ews.rs), release warning-free.
- PR #1969 bot-review triage COMPLETE (commit resolving 31 inline findings:
  28 fixed, 3 refuted with [MS-ASWBXML]/[MS-ASCMD] evidence — HasAttachments
  has no code-page-2 token; per-Fetch `<Options>` parsing is depth-agnostic;
  MIMETruncation can't apply since Type 4 is unreachable). 10 issue comments
  were non-actionable (billing-blocked bots, CI acks, summaries).
- PR #1980 bot-review triage COMPLETE (all 13 comments audited: 10
  non-actionable bots/acks; 3 substantive CodeRabbit findings, all concurred
  with and fixed in commit 0657fb4 on eas-itemoperations-fetch-conformance):
  (1) CWE-770 cumulative per-request content budget in the ItemOperations
  loop — `max_attachment_bytes` now caps the SUM across Fetches (attachment
  windows both modes + multipart email body parts), overflowing Fetch gets
  Item-scoped status 11, remaining Fetches still execute in request order,
  single-Fetch behavior unchanged (per-fetch cap already guarantees it; the
  email-body arm needs the explicit `served > 0` guard since negotiated
  bodies have no per-fetch cap); (2) jmap.rs streamed-total bug — the
  `if total_size.is_none()` in-loop update froze the total at the FIRST
  chunk's end for no-Content-Length responses (wrong `<Total>`); fixed by
  carrying the declared total in `stop_after` and deriving the EOF total via
  `content_length.unwrap_or(position)`; (3) jmap.rs streamed past-EOF range —
  without Content-Length the EOF branch built an INVERTED window (garbage
  `<Range>500-299</Range>`, u64 underflow on empty bodies); fixed by
  rejecting `r.start >= position` with `RangeStartsPastEof` (status 8, same
  semantics as the Content-Length precheck). 3 new tests pin all three:
  `handle_item_operations_attachment_cumulative_budget`,
  `download_blob_range_reports_total_for_multichunk_stream`,
  `download_blob_range_rejects_range_past_eof_without_content_length`.
  Suite after fixes: 931 green (lib 895 + fixtures 22 + snapshots 11 +
  jmap_calendar_deploy 2 + doc 1), clippy 0, fmt clean on touched files,
  release warning-free. No comment replies (resolve via code/push only).
- §14 Timezone fidelity both directions is COMPLETE and its AUDIT.md section
  documents the delivered behavior. Design rule: `timezone.rs::zone_transitions`
  (chrono-tz sampling) is the SINGLE authoritative zone source — it feeds the
  EAS Calendar:Timezone TZI blob, the EWS StartTimeZone/EndTimeZone ids, and
  the synthesised VTIMEZONE. Delivered: 139-zone blob↔IANA→blob round-trip +
  chrono-tz no-offset-drift matrix; name→IANA tiers (registry id → display
  description → fixed-offset LAST; substring heuristic guards short UTC*/GMT*
  tokens); VTIMEZONE STRUCTURAL matching in resolve_ical_tzid (custom TZIDs
  resolve via their block's transitions, EU last-Sunday rules match exactly);
  gap/fold-tolerant naive localization (fold→earlier, gap→pre-transition offset)
  shared by iCalendar and EWS naive values, explicit Z always wins;
  EWS render pairs Id+Name from ONE variant (`windows_variant_for_iana` +
  `iana_to_windows_display_description` — Name is the "(UTC±hh:mm) …" display
  description, never the registry id twice); GetServerTimeZones full matrix
  ([MS-OXWSGTZ] §3.1.4.1.3.3): every variant served, names-only attribute
  form, Ids filter in request order, unknown ids dropped silently.
  Round-trip semantics: Windows zones are COARSER than IANA —
  Europe/Amsterdam renders W. Europe Standard Time and re-parses to
  Europe/Berlin (canonical representative), offset-identical at every
  quarterly sample; instants always preserved. Tests: 9 new (timezone 2,
  calendar 3, sync 1 EAS blob render round-trip, ews 2 render round-trip +
  1 GetServerTimeZones names-only/Ids fixed). Suite: 973 lib + 22 fixtures +
  11 snapshots + 2 deploy + 1 doc, clippy 0, fmt clean on touched files
  (calendar/timezone/sync/ews — eas.rs:2284 remains the pre-existing §10
  exception), release warning-free.
- PR #1985 (§14) review triage COMPLETE (commits 080d7a3 + 7ac61dc, all
  threads replied): 6 inline findings + 1 Zenable outside-diff finding.
  Fixed: (1) fail-closed unknown-zone naive values — parse half
  (`parse_datetime_with_tzid` → None) AND UpdateItem half: `EwsRequestZone`
  tri-state (Absent/Resolved/Unresolved) from `ews_update_request_zone`
  (calendar.rs); `Unresolved` + naive Start/End →
  `EwsUpdateError::TimezoneUnresolved` → ErrorInvalidRequest (200), no field
  applied; both `handle_update_item` branches share
  `parse_request_zone_datetime` (ews_update.rs, pub(crate)); Absent keeps
  Exchange's UTC default, explicit-offset values keep their instant in every
  state. (2) xs:boolean names-only parsing (`is_xs_boolean_false`: false|0
  after trim; roxmltree attribute + lenient literal fallback). (3) gap probe
  ladder 12h→48h (Pacific/Apia 24h date-line skip test). (4) per-property
  TZID via `resolve_property_zone` (flight-style DTSTART;TZID=A +
  DTEND;TZID=B; first-wins `resolved_zone` state removed). (5) EWS Id/Name
  pair from ONE variant (`iana_to_windows_timezone_pair`, both render sites,
  raw-value fallback). Refuted: timezone.rs "mojibake" — bytes are clean
  UTF-8 (E2 80 94 em-dash etc.); the artifact is the review tool decoding
  UTF-8 as cp437. Zone-change extraction fix riding along (7ac61dc): both
  Outlook wire shapes — `<t:StartTimeZone Id="…"/>` ATTRIBUTE form (incl.
  quick-xml Event::Empty for self-closing tags; unescape via
  `normalized_value(XmlVersion::Implicit1_0)`, NOT resolve_xml_reference
  which takes a GeneralRef entity NAME) and `<t:StartTimeZoneId>` element
  form — previously both silently dropped the timezone change.
  Suite after: 986 lib + 22 fixtures + 11 snapshots + 2 deploy + 1 doc,
  clippy 0, fmt clean on touched files, release warning-free.

- §15 Tasks/Notes CalDAV backend mirroring is COMPLETE and its AUDIT.md
  section documents the delivered behavior. Design: Stalwart is the system
  of record — EAS Tasks bridge to VTODO (RFC 5545 §3.6.2), Notes to
  VJOURNAL (§3.6.3), in per-user collections /{cal}/{user}/Tasks/ and
  /{cal}/{user}/Notes/ (MKCALENDAR-ensured). task_map/note_map are a
  mirror/journal cache with caldav_href/etag/uid (PRAGMA-probed ALTER TABLE
  migration for pre-§15 DBs; owner-scoped href/uid indexes;
  set_*_backend_ref bookkeeping writes NO journal event).
  Write-through in apply_task_mutation/apply_note_mutation (tasks.rs, take
  password; routed by backend Some/None + backend_configured): Add PUTs the
  VTODO/VJOURNAL first (ServerId = iCalendar UID, URL-safe filename or uuid)
  and mirrors only after success; Change re-reads the authoritative object,
  patch_component_ics rewrites ONLY the mapped property lines (foreign
  RRULE/ATTENDEE/URL/X-* survive verbatim; DTSTAMP bumped; VALARM rewritten
  as a unit; COMPLETED→NEEDS-ACTION reset on un-complete) and PUTs back with
  If-Match; mirrored Delete deletes the backing resource first. Configured-
  but-unreachable backend FAILS CLOSED (status 6, mirror untouched — no
  divergence, no resurrect/duplicate on reconcile); legacy no-href rows stay
  local and are pushed by reconcile. Reconcile runs on every Tasks/Notes
  Sync (eas.rs handle_local_content_sync, AFTER mutations, BEFORE the
  journal read; gated on !caldav_base.is_empty()): calendar-query REPORT
  (comp-filter, no time-range) imports new/changed (journal upsert), adopts
  moved hrefs by UID (keeps EAS ServerId), tombstones rows whose href
  vanished (remote delete → client-visible Remove), and pushes never-pushed
  rows. Reconcile failure DEGRADES to serving the mirror (warn + Status 1).
  PING backend wake (also §15, delivered): each Ping tick probes the CalDAV
  etag inventory (query_calendar_etags — getetag-only REPORT, no
  calendar-data, no MKCALENDAR) via tasks::backend_inventory_changed +
  inventory_differs (missing etag on either side = NO signal, not a diff —
  prevents a probe/reconcile loop on servers that omit getetag); on
  divergence the Ping runs the SAME reconcile (journals → the per-folder
  journal check in the SAME pass fires Status 2 within one tick). Probe or
  reconcile failure sets backend_probe_disabled for the REST of that Ping
  (journal detection unaffected) — a dead backend is never re-hit per tick
  and the Ping degrades to clean Status 1 at the heartbeat.
  Wire fidelity: X-SGW-* carry the exact EAS wire date strings while
  DTSTART/DUE carry absolute instants; Importance↔PRIORITY 9/5/1 bands;
  Sensitivity↔CLASS with X-PERSONAL; RFC 5545 §3.3.11 escaping both ways;
  75-octet UTF-8-safe folding; REPORT parser canonicalizes hrefs to the same
  path-only form put_event returns. Tests: 18 new tasks.rs (VTODO/VJOURNAL
  round-trips, foreign-property preservation, completed→reset, matrices,
  datetime conversions, escaping, folding, filenames, fail-closed routing
  matrix, legacy-row delete, unconfigured local mode) + 1 eas.rs outage
  resilience test (Sync Status 1 + mirror served with backend down) + Ping
  probe tests (inventory_diff matrix incl. missing-etag semantics ×1,
  backend_inventory_changed fail-closed ×2, eas.rs
  test_ping_tasks_folder_survives_backend_outage_and_fires_on_local_change
  — Phase 1: outage Ping ends Status 1 at the cap; Phase 2: journal change
  after a Sync-set watermark still fires Status 2 naming the Tasks folder).
  NOTE the Ping test pattern: mid-heartbeat writes are MISSED because the
  tick equals the remaining heartbeat (one journal pass per short-heartbeat
  Ping) — to test journal wake deterministically, Sync first (persists the
  watermark), journal the change, THEN ping: the first pass fires.
  Suite: 1009 lib + 22 fixtures + 11 snapshots + 2 deploy + 1 doc, clippy 0,
  fmt clean on touched files (tasks.rs, caldav.rs — eas.rs untouched by fmt
  per the §10 exception), release warning-free.
- §16 Watermark pruning vs. long-offline devices is COMPLETE and its AUDIT.md
  section documents the delivered behavior. Design: `prune_change_journal`
  records the deleted floor per owner in `journal_prune_state` (schema v10,
  monotonic MAX upsert) in the SAME transaction as the DELETE (a committed
  delete with an uncommitted floor would let stale resumes silently miss rows),
  exposed by `pruned_through(owner)`; `journal_window_stale(owner, since)`
  = `since < pruned_through` (a resumer from `since` needs every row `id > since`;
  pruning removed every row `id <= floor`; intact iff `since >= floor`). EVERY
  stale check is fail-closed (`unwrap_or(true)`). Forced resync at the EAS Sync
  front doors: `handle_local_content_sync` (Tasks/Notes) computes the exact
  resume position (continuation cursor `base_seq`, else the `seq:` token, else
  0 — the SAME default the delta branches use for an unreadable token) and,
  running BEFORE client mutations are parsed, returns Sync status 3 + SyncKey 0
  (+ conditionally clears the in-flight window cursor via the CAS
  `clear_local_sync_cursor_if`, matching the scanned `(last_key, base_seq)` so a
  concurrent `SyncKey 0` prime's cursor is never dropped) when stale;
  `incoming_key == "0"` is never rejected. Delivery half: the Ping per-folder
  check treats a stale watermark as a change (`stale || <normal>`) so a
  long-offline device is woken within a tick and its follow-up Sync re-primes.
  GetItemEstimate reports status 4 for a stale window; EWS SyncFolderItems
  returns ErrorInvalidSyncStateData for a CLIENT-SUPPLIED cursor below the
  floor, but a stale GATEWAY-PERSISTED cursor (`ews_sync_state` is outside the
  floor scan, which reads only `seq:`/`journal_seq` in `sync_state`) is NOT
  rejected — rejecting it would spin an unrecoverable re-prime loop — it is
  instead treated as a full SNAPSHOT PRIME that keyset-enumerates the live
  `item_map` (`list_changes_after_server_id`, `server_id` ASC, MaxChangesReturned
  bound) rather than walking the journal; the baseline head is frozen in
  `local_sync_cursor.base_seq` across pages so a mid-prime change is delivered by
  the first delta after the prime completes, a null SyncState mid-prime restarts
  the snapshot, and a supplied cursor continues its keyset window. The calendar
  `perform_sync` needs NO guard: its delta is the backend-snapshot diff vs
  `item_map` (etag), never the client watermark, and its token is often an
  opaque JMAP `query_state` that would make a naive guard fire forever
  (documented in place of a guard). Tests: storage.rs 4 (floor/stale boundary +
  monotonicity, watermark-less prune fabricates no floor, CAS cursor clear,
  keyset live-set enumeration); eas.rs 5 (forced Status 3 + clean re-prime,
  stale-key WITH mutations applies nothing + no `<Responses>`, healthy delta NOT
  forced, Ping stale-wake, GetItemEstimate status 4); ews.rs 1 (client-supplied
  pruned-cursor rejection, null SyncState not rejected, stale persisted cursor
  re-primes). Suite: 1027 lib + 22 fixtures + 11 snapshots + 2 deploy + 1 doc =
  1063 green, clippy 0, fmt clean on ALL touched files, release warning-free.
- §18 Auth-cache warm path + per-request Basic-auth probe SPOF is COMPLETE and
  its AUDIT.md section documents the delivered behavior. Design: the
  per-request Basic-auth probe to Stalwart is the SPOF fronting every EAS/EWS
  request; it is now protected by a four-layer funnel in `auth.rs::warm_verify`
  BEFORE the backend is touched: (1) per-user circuit breaker
  (`auth_backend_failure_threshold` default 5, `auth_backend_max_backoff_secs`
  default 60, `0` DISABLES the breaker — do NOT clamp the cap to ≥1) — after N
  consecutive `BackendUnavailable` a user's circuit opens for an exponentially
  growing capped window and probes are refused with `BackendUnavailable`
  without contacting Stalwart; any `Verified`/`InvalidCredentials` resets it
  (`update_breaker`); (2) in-flight dedup keyed by CREDENTIAL
  (`username:hash(password)`), NOT user — keying by user would suppress the
  §17 start-order race test, which needs two same-user/different-password
  probes genuinely concurrent; a waiter that acquires the mutex re-checks the
  credential cache under the lock; the slot is released on EVERY path
  (`release_inflight`); (3) per-user probe rate cap
  (`auth_probe_min_interval_ms` default 1000) spaces consecutive COLD probes
  only — a cached outcome is never gated; the post-sleep re-check makes the cap
  a dedup barrier, not a redundant-probe generator; (4) the probe itself goes
  through `JmapClient::probe_auth`, whose `session_auth` DashMap caches
  credential outcomes per `username:hash` (positive TTL 300s, negative 30s,
  BackendError NEVER cached; a successful probe evicts the user's other
  credentials via `invalidate_other_session_auth`). Client sharing:
  `AppState::new` builds the JMAP client BEFORE the verifier and passes it to
  `AuthVerifier::new_from_topology`, so probes reuse one HTTP pool + one
  session-auth cache; `AuthVerifier::new` (tests) leaves it None and builds a
  throwaway client per probe. CRITICAL: `AuthVerifier::invalidate_user` MUST
  also call `client.invalidate_user_session_auth(username)` — the verifier
  cache and the shared client cache are separate, and a stale client positive
  would shadow the invalidation for up to 300s. `main.rs::warm_backend_session`
  primes the admin session at startup (never fails startup). Metrics
  (PII-free, label `backend` = jmap/caldav/none):
  `auth_backend_unavailable_total`, `auth_circuit_open_total`. EWS
  `ews_backend_unavailable` now also sends `Retry-After`
  (`eas::AUTH_BACKEND_RETRY_AFTER_SECS`); EAS keeps 503 + Status 111 only for
  ≥14.0 (audit's "status 110" is not a defined retry code; 111 is the retry
  code — see AUDIT.md §18 note). Config knobs wired (env + default + startup
  log) and documented in docker-compose.yml, CLOUDFLARED_SETUP.md.
  Tests: 6 new in src/auth.rs (breaker opens/skips, breaker resets, zero-cap
  disables, concurrent same-credential burst = 1 probe, rate cap spacing,
  invalidate purges shared client cache) + 5 in src/jmap.rs (`probe_auth`
  positive TTL, negative + eviction, backend-error-not-cached, capacity bound,
  injected positive TTL governs expiry) + 3 in src/models.rs (shared client
  present whenever `jmap_base` set even with `email_enabled=false`, email
  accessors stay gated, absent without `jmap_base`) + 2 config env
  tests + EWS Retry-After assertion. Suite: 1079 lib + 2 + 22 + 11 + 1 doc =
  1115 green, clippy 0, rustfmt clean on touched files, release warning-free.
  PR #1996 review triage (Phase 2): `JmapClient::probe_auth`'s session-auth
  cache is now a capacity-bounded moka cache whose positive/negative TTLs are
  injected by `AppState` from `GATEWAY_AUTH_CACHE_TTL_SECS` /
  `..._NEGATIVE_TTL_SECS` / `..._MAX_ENTRIES` (new constructor
  `JmapClient::with_session_auth`), so it can never outlive or grow past the
  verifier cache contract; `AppState::new` builds the shared JMAP auth client
  whenever `jmap_base` is set regardless of `email_enabled` while keeping the
  email accessors gated; the per-user (not global) breaker/rate-cap scope is
  documented in code as a deliberate correctness trade-off.
- §19 SQLite concurrency is COMPLETE and its AUDIT.md section documents the
  delivered behavior. Key lesson: `PRAGMA busy_timeout`/`foreign_keys` in
  sqlite_schema.sql apply ONLY to the connection that ran the schema — they
  are per-connection; `journal_mode=WAL` is the only file-persistent one.
  `Storage::new` now builds the pool via `SqliteConnectOptions`
  (`.journal_mode(Wal)`, `.busy_timeout(5s)`, `.synchronous(Full)`,
  `.foreign_keys(true)`, explicit `POOL_SIZE=10`) so every pooled connection
  carries the semantics. Ping's tick loop is READ-ONLY against SQLite (the
  watermark/key writes live on the Sync completion path,
  `render_delta_collection_xml`), so WAL guarantees Ping never blocks on the
  writer lock — keep it that way when editing eas.rs. Load-test lesson:
  pacing writer loops (1ms/iter like real Sync round-trips) is required —
  a lock-free write busy-loop starves a writer past busy_timeout (SQLite
  grants no writer fairness).
  Tests: per-connection pragma assertion +
  `concurrent_sync_writes_and_ping_reads_never_fail_or_stall` load test
  (production-shaped read/write mix, read monotonicity + exact counts).
- §20 Health readiness vs liveness is COMPLETE and its AUDIT.md section
  documents the delivered behavior. Design: `/health/live` is process-only
  (no dependency touched — never a restart reason); `/health/ready` (+ the
  kept `/health` alias) checks SQLite + the configured auth backend and
  MIRRORS `verify_backend`'s arbiter rules (§17): JMAP configured → JMAP
  is the SOLE arbiter (its outage = 503 even if CalDAV answers — there is
  no "degraded" 200 branch; PR #2000 review caught the mismatch), CalDAV
  only → CalDAV arbiter, no backend → 503. Both endpoints + `/metrics`
  bypass rate limiting via exact-match `is_unmetered_path` (prefix
  collisions stay metered). The readiness JMAP probe reuses the auth
  verifier's shared client: `AuthVerifier::shared_jmap_client` is `pub`
  (was `#[cfg(test)]`) — pub(crate) on a LIB item is invisible to the bin
  crate (main.rs), which is a separate crate. Reuse is OBSERVABLE via
  `JmapClient::health_probe_count` (AtomicU64), or the regression test
  can't catch a fall back to throwaway clients. Dockerfile `HEALTHCHECK`
  and docker-compose.yml curl `/health/ready`; stock Compose only MARKS
  unhealthy (restart: unless-stopped does NOT restart unhealthy
  containers) — the compose file documents this and ships a commented-out
  willfarrell/autoheal block. Tests: 7 in main.rs (in-process 401-stub
  HTTP servers drive the arbiter matrix; bin-crate `mod tests` runs via
  cargo's bin target) + 1 in rate_limit.rs.
- Next likely audit items: §21 secrets hygiene, §22 surface gating.
- AUDIT3.MD (independent Stalwart-vs-gateway audit, live-probed against a real
  Stalwart instance) found THREE P0 contract mismatches that prior in-repo
  audits missed — all are Stalwart wire-contract errors, not logic errors:
  (1) JMAP `CalendarEvent/get`'s `iCalendar` is a **JSCalendar OBJECT**
  (`{"name":"vevent","convertedProperties":{...}}`), NOT an ICS string — any
  event with `convertedProperties`/`properties` returns a dict, and
  `JmapCalendarEvent.i_calendar: Option<String>` (jmap.rs:706) makes
  `serde_json::from_value::<Vec<_>>` fail for the WHOLE list → `.ok()
  .unwrap_or_default()` yields ZERO events. Kills every JMAP calendar read
  (EAS Sync, EWS Find/Get, free/busy, MAPI). Confirmed by live probe (8/17
  events return a dict) and a Rust 1.99.1 repro ("invalid type: map, expected
  a string"). (2) JMAP `CalendarEvent/set` with an ICS `iCalendar` string is
  ACCEPTED but the payload is DISCARDED (empty event stored) — the code's own
  WARNING documents this; the guard (`probe_write_paths`→`ics_round_trip_ok`)
  only runs with GATEWAY_ADMIN_USERNAME/PASSWORD set, which the compose ships
  EMPTY, and `jmap_calendar_write_enabled` defaults TRUE → lossy writes live.
  (3) Stalwart CardDAV lives at `/dav/card/{user}/`, but
  `CarddavClient::addressbook_home` (carddav.rs:68) appends `/carddav/{user}/`
  → every CardDAV op 404s; EWS contacts are CardDAV-ONLY (Find/Create/Update/
  Delete in ews.rs) even though JMAP `ContactCard` works. Also P1: explicit
  `Email/get` ids are capped at 500 (`maxObjectsInGet`, back-refs exempt);
  `CalendarEvent/changes` `hasMoreChanges` ignored in sync.rs; `@etag` is NOT
  a supported JMAP property on Stalwart (etags empty). Compose fixes applied:
  `GATEWAY_MAIL_HOST` must be the public MX host (was `stalwart`, leaked into
  Autodiscover IMAP/SMTP blocks); volume path aligned to
  `/armblock/exchange-gateway-data`; missing log/MAPI env vars documented;
  `.env.example` DELETED (repo contract is compose-inline only). NOTE
  `GATEWAY_CALDAV_USER`/`GATEWAY_CALDAV_PASSWORD` are PHANTOM — referenced
  only in a caldav.rs:44 warning, never read; `GATEWAY_RULE_PROVIDER` is a
  Rust const, not env.
- AUDIT3.MD §1 (JMAP calendar reads always zero events) is COMPLETE and its
  section documents the delivered behavior. Root cause: Stalwart's
  `CalendarEvent/get` `iCalendar` property is a JSCalendar OBJECT
  (`{"convertedProperties":…,"name":"vevent"}`), not an ICS string; the old
  `JmapCalendarEvent.i_calendar: Option<String>` made `from_value::<Vec<_>>`
  fail for the WHOLE list → `.ok().unwrap_or_default()` = zero events.
  Delivered: new dep `calcard = "0.3.14"` (Stalwart's own iCalendar⇄JSCalendar
  lib, Apache-2.0 OR MIT); new module `src/jscalendar.rs`
  (`jscalendar_event_to_ics`) wraps the whole event object as a JSCalendar
  `Group` and converts via calcard — the `iCalendar` fragment alone carries
  only the converted-property MAP, NOT the data, so the whole object must be
  reassembled; JMAP-only properties (`id`/`calendarIds`/`isDraft`/…) are
  stripped or calcard emits `JSPROP;JSPTR=…` junk; participants without a
  `calendarAddress` CANNOT become ATTENDEEs (calcard's exporter asserts in
  debug builds) and are instead pinned to a `PARTICIPANT` component so their
  name/properties survive — the ICS parser (`calendar.rs::parse_event_lines`)
  now skips ALL nested sub-component properties by depth, so a PARTICIPANT's
  `SUMMARY` can never overwrite the event's subject (verified bug, fixed).
  `i_calendar` is now `Option<Value>` + a `raw: Option<Value>` field +
  `to_ics()`; a manual `Deserialize` impl retains `raw` on the public serde
  path; `parse_calendar_event_list` decodes PER ELEMENT (skip+log a bad
  one, never drop the list) and returns the skip count, which
  `query_calendar_events` surfaces as `CalendarEventListResult.unparseable`.
  `CALENDAR_EVENT_PROPERTIES` fixes the requested
  names: `showWithoutTime` (NOT `isAllDay`), `duration` (NOT `end`),
  `locations` (NOT `location`), `recurrenceRule` (NOT `recurrenceRules`) —
  invalid names are silently dropped by Stalwart. All consumers
  (sync initial+delta, ews free/busy+Find/Get, eas free/busy,
  get_calendar_event(s)) call `event.to_ics()`, and conversion/deserialize
  failures PROPAGATE (getters → Err so CalDAV fallback runs; sync delta/initial
  declines to advance the state token, else the skipped event is lost on the
  next delta). Calendar deltas no longer tombstone unchanged events: a
  `CalendarEvent/changes` result is PARTIAL, so `calendar_event_deleted`
  (`sync.rs`) removes a stored event only when the delta is a full snapshot
  (initial sync / CalDAV fallback) or the id is explicitly in `destroyed`.
  Tests: 8 in src/jscalendar.rs
  (incl. an end-to-end `to_ics`→`parse_ics_event` chain test asserting UID/
  subject/description/location/RRULE/DTSTART+DTEND instants, and
  addressless-participant preservation asserting NO subject leakage) + 7 in
  src/jmap.rs (per-element skip + count, `to_ics()` reconstruction,
  `None` without raw, raw retention, single/batch conversion-failure
  propagation, `query_calendar_events` unparseable count) + 1 in src/sync.rs
  (tombstone decision matrix) + 1 in src/calendar.rs (nested sub-component
  properties ignored). Suite:
  1137 lib + 7 + 2 + 22 + 11 + 1 doc, clippy 0 (`-D warnings`), rustfmt clean
  on all touched files, release build warning-free.
  LESSON: the `iCalendar` fragment is NOT the event — it is a
  converted-property map; a faithful ICS needs the full requested property
  list AND the whole-object reassembly. calcard folds at 75 octets (unfold
  before asserting on long lines) and appends `;JSID=<id>` to LOCATION lines.
- AUDIT3.MD §3 (CardDAV base path wrong → EWS contacts dead) is COMPLETE and
  its section documents the delivered behavior. THREE defects, not one:
  (1) `CarddavClient` built `{base}/carddav/{user}/` while Stalwart serves
  CardDAV at `/dav/card/{user}/` — `addressbook_home` now appends just
  `/{user}/` and the base is `…/dav/card`; config derives it via the new pure
  `CarddavClient::derive_base_from_caldav` (PATH-aware: `/dav` → `/dav/card`,
  `/dav/cal` → `/dav/card`, a host literally named `dav…` is never matched);
  compose + CLOUDFLARED_SETUP.md updated to `GATEWAY_CARDDAV_BASE=
  http://stalwart:8080/dav/card`.
  (2) `upsert_contact` conflicted on `(owner, carddav_href)` but the schema's
  unique constraint is `UNIQUE(owner, server_id)` — every contact upsert
  errored, so contacts always returned 0; now `ON CONFLICT(owner, server_id)`
  in a transaction that first drops a stale href row when the deterministic
  ServerId changed (href move / id-scheme change), so exactly one row points
  at each href.
  (3) the EWS Get/Update/Delete dispatchers fell through to the "not in
  calendar item map → JMAP email" path before the `contact-` check, misrouting
  every contact read/update/delete to the email handler; the contact check now
  runs first, and `SyncFolderItems` gained a Contacts branch (it previously
  fell through to the CALENDAR path and returned calendar items) —
  `handle_sync_contacts_folder_items` is a full-snapshot sync keyed on the
  `(server_id, change_key)` pairs carried in an opaque base64 `SyncState`
  (`contact:` + sorted `id:ck` lines): unchanged set = no changes, edit =
  Update, new = Create, vanished = Delete; the returned state reflects exactly
  what was delivered so a truncated page re-delivers the remainder next call.
  JMAP-first contacts everywhere (RFC 9610 `ContactCard/get`/`set` via new
  `JmapClient::get_contact_card` + shared `contacts::{list_contacts_jmap,
  get_contact_vcard_jmap, create_contact_jmap, update_contact_jmap,
  jmap_contacts_available}`), CardDAV as fallback; `AppState::can_read_contacts`
  now true when EITHER client is configured. Contact ServerId is
  `contact-{sha256(href)}` (stable across FindItem calls); ChangeKey prefers
  the backend ETag and falls back to `sha256(vcard)` since Stalwart's JMAP
  exposes no `@etag`. Tests: 3 carddav derivation + 4 contacts stub-JMAP
  integration + 6 ews handlers (incl. SyncFolderItems prime/unchanged/add/
  remove, edit detection, SyncState round-trip). Suite: 1163 lib + 7 + 5 + 22
  + 11 + 1 doc = 1209 green, clippy 0 (`-D warnings`), rustfmt clean on all
  touched files, release build warning-free.
  PR #2033 review hardening (4 findings, all fixed + tested): (a) ONE ServerId
  derivation — `contact_ids` hashed the href but the JMAP/CardDAV create and
  EAS diff-add paths minted `contact-{uuid}`, which the stale-href delete then
  dropped on the first re-list; every producer now uses
  `contacts::contact_server_id`. (b) NO identity churn on backend fallback —
  `fetch_all_contacts` now uses the tri-state `ContactsBackend`
  (`jmap_contacts_backend` + `JmapClient::contacts_capability`, which
  distinguishes a genuine capability miss from a probe ERROR) and returns the
  retryable `ErrorServerBusy` (503) on a JMAP outage instead of publishing a
  CardDAV snapshot whose href-derived ids would Delete+Create the whole set;
  EAS contacts Sync likewise returns status 6. (c) canonical ChangeKey —
  CreateItem now returns `contacts::contact_change_key` like
  Find/Get/SyncFolderItems, and the JMAP create reads the card back and stores
  the SAME etag the read path reports (`jmap_contact_etag` native-or-hash), so
  one unchanged contact has one stable ChangeKey. (d) `update_contact_jmap`
  uses `JmapClient::get_contact_card` (single-id) instead of listing + scanning
  every card. Suite after: 1172 lib + 7 + 5 + 22 + 11 + 1 = 1218 green,
  clippy 0, rustfmt clean on touched files, release warning-free.
- §17 Auth negative-caching and cache-invalidation is COMPLETE and its AUDIT.md
  section documents the delivered behavior. Design: `AuthVerifier` now returns
  a tri-state `AuthOutcome` (Verified / InvalidCredentials / BackendUnavailable)
  instead of a bool — the whole point is that a *backend outage* must never be
  answered with a 401 (which makes Outlook re-prompt for a password).
  `JmapClient::check_credentials` and `CaldavClient::check_credentials` return a
  tri-state `CredentialCheck` (Valid / Invalid on an authoritative 401 /
  BackendError on any other status or transport failure; a CalDAV 403 is a
  non-auth failure, NOT Invalid — it means authenticated-but-forbidden and must
  not return/cache a 401 for a valid password). KEY RULES: (1) a JMAP *backend
  error* short-circuits to BackendUnavailable and does NOT consult CalDAV
  (previously any JMAP error hammered CalDAV for every EAS request — the
  amplification this item exists to kill); when JMAP is configured, CalDAV is
  consulted ONLY after JMAP authoritatively rejects, and a CalDAV backend error
  there cannot undo JMAP's negative; a CalDAV-only deployment (no JMAP) probes
  CalDAV directly as its sole arbiter. (2)
  Only Verified/InvalidCredentials are cached; BackendUnavailable is never
  cached, so the next request re-probes on recovery. A JMAP 2xx whose body is
  not a valid `JmapSession` is a BackendError, never a cached positive. (3)
  positive TTL = `GATEWAY_AUTH_CACHE_TTL_SECS` (floor 30s), negative TTL = NEW
  `GATEWAY_AUTH_CACHE_NEGATIVE_TTL_SECS` (default 30, 0 disables negatives) plus
  a deterministic per-key SHA-256 jitter of ≤25% (`jitter_for`), so a fixed
  password stops being locked out in seconds and concurrent negatives don't
  expire in lockstep. (4) a successful login evicts every other cached
  credential for the same username (`invalidate_other_passwords_for_user`), so
  a changed Stalwart password takes effect on first use; `invalidate_user` is
  the broader explicit form. RACE-SAFE ordering: concurrent verifications for a
  user are ordered by START time via a monotonic ticket (`ticket` atomic;
  `commit_result` / `record_committed_generation` record, per user, the newest
  ticket whose result was committed — a bounded `committed_generation` moka
  cache keyed by username, TTL 1h + capacity; a mutex serializes the
  check-and-commit so two finishers can't both write), so a slower in-flight
  old-password check cannot repopulate the cache after a newer login already
  evicted it. A cancelled verification never commits, so it cannot suppress
  later logins. Front doors: EAS answers a backend outage with
  503 + `Retry-After` (`AUTH_BACKEND_RETRY_AFTER_SECS=12`,
  `service_unavailable_response`) with a `Status` 111 body ONLY for protocol
  version ≥14.0 (`eas_supports_server_error_retry`; [MS-ASCMD] §2.2.3.177.8
  specifies a bare 503 for 2.5/12.0/12.1, which the gateway advertises), EWS
  with a 503 `ErrorServerBusy` SOAP fault (`ews_backend_unavailable`), whose
  faultcode is derived from status in `soap_fault` (5xx → `s:Server`, 4xx →
  `s:Client`) so a backend outage is server-side and does not read as the
  non-retryable `s:Client`; neither sends
  `WWW-Authenticate`. Surfaces that
  only need yes/no (OAB/ECP/NSPI/MAPI/autodiscover-directory, main.rs) keep
  `verify` → bool (BackendUnavailable = false). Config knob is wired in
  config.rs (env + default + startup log) and documented in docker-compose.yml,
  docker-compose.yml, CLOUDFLARED_SETUP.md. Tests: 18 in src/auth.rs (stub
  backend with mutable status + hit counters: positive/negative caching,
  negative expiry, TTL-0, backend-error-not-cached, no-CALDAV-on-5xx, CALDAV
  arbitration, CALDAV-only paths, empty-credential rejection, password-change
  eviction, invalidate_user scope, jitter determinism/spread) + `check_credentials`
  classification matrix in src/jmap.rs (status stub ×401/403/5xx/transport;
  403 is a BackendError, not Invalid) + EAS/EWS front-door 503-vs-401
  integration tests. Suite: 1054 lib + 2 + 22 +
  11 + 1 doc = 1090 green, clippy 0, rustfmt clean on touched files
  (auth/jmap/caldav/ews/eas), release warning-free.
- PR #1989 (§15) review triage COMPLETE: cubic 10 issues + CodeRabbit 3 +
  LlamaPReview 3 substantive, all fixed. Highlights: (1) P0 pre-§15 DB
  migration — the four `idx_{task,note}_map_{href,uid}` indexes are created in
  `init_schema` AFTER the PRAGMA-probed ALTERs (declaring them in
  sqlite_schema.sql aborted schema execution with "no such column" before the
  migration could run); migration test
  `init_schema_migrates_pre_backend_task_note_columns` opens a true pre-§15
  shape and asserts re-init idempotence + surviving legacy rows + indexes.
  (2) `calendar-query` REPORT now REQUIRES 207 (a 2xx-with-no-body parsed as an
  empty inventory would tombstone the whole mirror). (3) `ensure_calendar_collection`
  MKCALENDARs `/Tasks/` (trailing slash, same URI as tasks/notes_collection_href).
  (4) `parse_calendar_query_items` is fail-closed `Result` and decodes XML
  GeneralRefs inside `<href>` (`a&amp;b.ics`); malformed body → reconcile fails
  closed (serves mirror) instead of tombstoning from a truncated list.
  (5) `resolve_resource_url` treats a leading-`/` href as an absolute path on
  the same host (RFC 3986 §4.2) instead of re-joining it under the collection —
  fixes the EAS Change PUT doubling the path (`<collection>/dav/cal/u/Tasks/x.ics`);
  bare leaf filenames still join, absolute URLs pass through. (6) `fold_ical_document`
  no longer `trim_end()`s segments (RFC 5545 §3.1 unfolding restores only
  CRLF+space) — trailing-space values survive the round trip. (7) Ping probe
  per-class failure tracking (`BackendProbe{tasks,notes}_{changed,failed}`) so a
  404 Tasks collection never masks Notes, and the whole probe + each reconcile
  is bounded by the remaining heartbeat via `tokio::select!` on the supersede
  token (supersession cancels in-flight backend work). (8) Add retries
  idempotent by ClientId: the `(ClientId→ServerId)` record is written the
  instant the CalDAV PUT succeeds — BEFORE the mirror upsert — so a mirror
  failure still leaves a replayable identity and the retry does not PUT a
  second resource under a fresh ServerId; the next reconcile adopts the
  orphaned resource by UID (`used_server_ids.insert(uid)`). (9) VTODO
  un-complete preserves a foreign non-COMPLETED STATUS (IN-PROCESS) instead of
  dropping it. (10) AUDIT.md §15 claims narrowed to match code (indexes after
  migration, per-class probe disable, foreign STATUS preserved, missing etag
  suppresses only same-href comparison). Suite: 1017 lib + 22 fixtures + 11
  snapshots + 2 deploy + 1 doc, clippy 0, fmt clean on ALL touched files
  (eas.rs now fully rustfmt'd too), no comment replies (resolve via code/push).
- Toolchain note: rustup components clippy/rustfmt must be installed in a
  fresh container (`rustup component add clippy rustfmt`); project edition is
  2024 (rustfmt needs `--edition 2024` for let-chains). `cargo fmt --check`
  flags eas.rs in the committed tree (pre-existing §10 merge state) — fmt is
  NOT a repo gate; only fmt files you touch.

## §13 MeetingResponse/iMIP integrity (DELIVERED)
- One shared pipeline `meeting::rsvp::apply_rsvp` backs both doors: EAS
  `MeetingResponse` (eas.rs `handle_meeting_response`, 7 params incl.
  `&Wbxml` receiver pattern `Wbxml::new()` in tests) and EWS
  `AcceptItem`/`TentativelyAcceptItem`/`DeclineItem` (ews.rs
  `handle_meeting_response_object`, reached from the CreateItem dispatch by
  `body.contains("<t:AcceptItem")` etc.).
- Order of guards in apply_rsvp: lone-proposed-time → 2; resolve (Email→JMAP
  blob+`extract_meeting_request_ics` requires METHOD:REQUEST + organizer,
  CalendarItem→item_map row then JMAP `jmap://` href or CalDAV read);
  InstanceId-on-email → 2; no-RRULE-with-InstanceId → 146; organizer
  self-response → 2 (+`self_notify_suppressed`, EWS maps to
  ErrorCalendarIsOrganizer{Accept,Decline,Tentative}); instance existence →
  2; duplicate read; local copy update (failure → 3, fatal); iTIP delivery
  (send failed → 3, retry safe); RSVP recorded only AFTER successful send.
- Key helpers: `instance_key_for` (None→"" series, Some(dt)→
  `%Y%m%dT%H%M%SZ`), `instance_exists` (pre-start/EXDATE/deleted-exception
  rejection, `rrule` crate expansion, fail-open `unwrap_or(true)` on
  uninterpretable rules), `patch_instance_response` (exception created when
  absent with `start=Some(instance_id)`, roster inherited from master;
  master roster NEVER rewritten by instance responses),
  `apply_attendee_decision` (case-insensitive normalize_email match;
  unknown responder appended with SCHEDULE-AGENT CLIENT),
  `mark_scheduling_client_side` (RFC 6638 §7.1 pin on ALL attendees).
- Declines never create a copy (Exchange semantics) → `CalendarId` only for
  accept/tentative. COUNTER only when BOTH proposed times present; delivered
  as a second send_imip with `method: Some("COUNTER")`.
- Response WBXML: code page 8 (MeetingResponse root). eas.rs handler tests
  drive `handle_meeting_response` with `&Wbxml::new()` and
  `&SecretString::from(...)` (handler takes refs!). extract_first_tag_text is
  quick-xml LOCAL-name based, so `b"Data"` matches `<AirSyncBase:Data>`.
- EWS error mapping: NotFound→ErrorItemNotFound (200), InvalidItem→
  ErrorInvalidRequest (200), ServerError→ErrorInternalServerError (500),
  all as ResponseClass="Error" ResponseMessages (EWS does NOT use soap:Fault
  for operation errors).
- New tests this item: 7 eas.rs handler tests (incl. full as_wbxml round-trip
  decode of a real response), 4 ews.rs handler tests (needs meeting_test_state
  harness: `Config{jmap_base: "http://127.0.0.1:1", email_enabled: true,
  hmac_secret: 32×'a', ..Default}`, AppState::new(cfg, Arc(storage)) — jmap
  client is auto-created when email_enabled && jmap_base set), 1 storage.rs
  idempotence-store test. Suite: 964 green (928 lib + 22 fixtures + 11
  snapshots + 2 jmap_calendar_deploy + 1 doc), clippy 0, fmt clean on
  rsvp.rs/message.rs/ews.rs/storage.rs (eas.rs pre-existing exception).

- PR #2046 (`eas-folder-management-and-mailbox-scoped-email-sync`) review
  triage COMPLETE: 4 substantive findings, all concurred with and fixed
  (no comment replies). (A/§2-l) `render_pending_email_op` is now
  `anyhow::Result<(String, Option<String>)>` and fail-closed: a transient
  `Email/get` error on a queued created/changed op and a failed
  keyword-baseline seed for a delivered `<Add>` both propagate, so the delta
  does NOT acknowledge the window or consume the op (no token advance, no
  overflow queue, no baseline write); the old code rendered empty on a fetch
  error (treated as a vanished item → silent loss) and only logged a failed
  seed (→ stale client flags). (B/§2-k) the email delta's mailbox-role
  resolution no longer flattens a `Mailbox/query` failure with `.ok()` to
  `None` (which cleared the stored query state and answered an empty success
  window, losing every change during the outage): the error propagates as
  Status 6 with the token untouched; a genuinely mailbox-less role is
  `Ok(empty)` and still scopes to nothing. (C/§2-j) `FolderCreate` rejects a
  missing/non-numeric `<Type>` (required, [MS-ASCMD] §2.2.3.186.2) as Status
  10 instead of defaulting to 12, and `is_creatable_folder_type` (1/12/13)
  gates `create_folder` so an unrecognised numeric Type no longer falls
  through `backend_for_type` to JMAP and creates a mail mailbox. Tests: 2
  eas_folders (`create_rejects_unrecognised_folder_types`,
  `creatable_folder_types_match_spec`) + 4 eas
  (`test_folder_create_rejects_missing_or_malformed_type`,
  `test_email_sync_mailbox_resolution_failure_preserves_token`,
  `test_email_delta_fetch_error_fails_closed_not_vanished`,
  `test_email_delta_add_seed_failure_fails_closed`). Suite: 1314 lib + 7 + 5
  + 22 + 11 + 1 doc = 1360 green, clippy 0, fmt clean on touched files.
  NOTE the §19 `storage::tests::concurrent_sync_writes_and_ping_reads_never_fail_or_stall`
  load test is timing-sensitive under a fully parallel `cargo test` (a
  "database is locked" panic is possible); it passes reliably in isolation
  and is unrelated to this change.
