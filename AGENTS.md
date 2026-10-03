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
- PR #1969 bot-review triage COMPLETE (commit resolving 31 inline findings:
  28 fixed, 3 refuted with [MS-ASWBXML]/[MS-ASCMD] evidence — HasAttachments
  has no code-page-2 token; per-Fetch `<Options>` parsing is depth-agnostic;
  MIMETruncation can't apply since Type 4 is unreachable). 10 issue comments
  were non-actionable (billing-blocked bots, CI acks, summaries).
- Next likely audit items: §12 ItemOperations attachment ranges, §13
  MeetingResponse/iMIP integrity, §14 timezone blob fidelity.
- Toolchain note: rustup components clippy/rustfmt must be installed in a
  fresh container (`rustup component add clippy rustfmt`); project edition is
  2024 (rustfmt needs `--edition 2024` for let-chains). `cargo fmt --check`
  flags eas.rs in the committed tree (pre-existing §10 merge state) — fmt is
  NOT a repo gate; only fmt files you touch.
