# exchange_gateway — agent memory

## Project
Rust gateway (~96k LOC) translating Microsoft Exchange/Outlook protocols (EAS via
WBXML, EWS, MAPI/HTTP, Autodiscover, SMTP) to a Stalwart Mailserver v0.16.22
backend (JMAP-first, CalDAV fallback for calendar). Target clients ONLY:
New Outlook for Windows (20251205004.10), Outlook Android (5.2634.2).
Specs: `exchange_protocols/` (v20250520 .txt docs). Toolchain: Rust 1.98.1.
EAS specs v20250520 (16.1); older EAS compat NOT needed.

## Build / test
- `cargo test` — lib (844+) + integration suites (protocol_fixtures 22, snapshots 11, jmap_calendar_deploy 2, doc 1). All must pass.
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
  always declare all namespaces they use.
- EAS request/response XML parsing uses quick-xml LOCAL names (prefix-agnostic);
  string helpers (`extract_tag_block`, `extract_all_tag_blocks`) match literal
  unqualified tag names — safe because decode output keeps unqualified names
  only where they're the document default namespace.
- Context structs to dodge clippy arg limits: `SyncCtx`, `EmailSyncCtx` (state,
  jmap, account_id, username, password, collection_id, state_collection_id,
  window, options, conversation_mode — Copy, destructured with `*ctx`).

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
- Next likely audit items: §11 WBXML conformance hardening (full-table diff
  test vs MS-ASWBXML), §12 ItemOperations attachment ranges.
