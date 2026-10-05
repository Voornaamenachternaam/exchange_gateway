# exchange_gateway — agent memory

## Project
Rust gateway (~96k LOC) translating Microsoft Exchange/Outlook protocols (EAS via
WBXML, EWS, MAPI/HTTP, Autodiscover, SMTP) to a Stalwart Mailserver v0.16.22
backend (JMAP-first, CalDAV fallback for calendar). Target clients ONLY:
New Outlook for Windows (20251205004.10), Outlook Android (5.2634.2).
Specs: `exchange_protocols/` (v20250520 .txt docs). Toolchain: Rust 1.98.1.
EAS specs v20250520 (16.1); older EAS compat NOT needed.

## Build / test
- `cargo test` — lib (871) + integration suites (protocol_fixtures 22, snapshots 11, jmap_calendar_deploy 2, doc 1). All must pass.
- `cargo clippy --all-targets` — must be 0 warnings. `#[allow(clippy::too_many_arguments)]` is the accepted escape for irreducible request params (email.rs, jmap.rs, eas.rs handle_email_sync).
- `cargo build --release` — ~5min, must be warning-free.
- Precedence for gaps: existing deps → new quality deps → Rust 1.98 std → custom code.

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
- Next likely audit items: §13 MeetingResponse/iMIP integrity, §14
  timezone blob fidelity, §15 Tasks/Notes backend story.
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
