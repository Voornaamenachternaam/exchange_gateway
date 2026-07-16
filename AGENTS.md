# exchange_gateway - agent notes

## GitHub push auth (IMPORTANT - ghu_ app-installation tokens)
The `GITHUB_TOKEN` env var in this workspace is a GitHub App installation token
(prefix `ghu_`, 40 chars). It does NOT authenticate the usual ways:

- `curl -H "Authorization: token $GITHUB_TOKEN"` -> 401 (wrong scheme).
- `git push https://$GITHUB_TOKEN@github.com/owner/repo.git` -> 401
  "Invalid username or token. Password authentication is not supported."
  (git sends the token as a basic-auth username, which GitHub rejects for app tokens.)
- `git -c http.extraHeader="Authorization: Bearer $GITHUB_TOKEN" push` -> 401
  "invalid credentials" via the default helper path.

What DOES work (use this exact method - it's how `gh` pushes app tokens):

```sh
# (A) API calls / gh-style: use Bearer (not "token")
curl -H "Authorization: Bearer $GITHUB_TOKEN"   https://api.github.com/repos/Voornaamenachternaam/exchange_gateway

# (B) git push: pass the token as the password with username x-access-token
GIT_TERMINAL_PROMPT=0 GIT_ASKPASS=/bin/true git   -c credential.helper='!f() { echo "username=x-access-token"; echo "password=$GITHUB_TOKEN"; }; f'   push https://github.com/Voornaamenachternaam/exchange_gateway.git <branch>
```

Equivalent one-liner using `gh` as the credential helper:
`printf 'protocol=https\nhost=github.com\n\n' | gh auth git-credential get`
returns `username=x-access-token` + `password=<token>`.

So: if a `ghu_` token 401s with `Authorization: token ...` or via the
`https://<token>@host` URL, switch to `Authorization: Bearer` for REST and
`username=x-access-token / password=<token>` for git. Don't conclude the
token is invalid until you've tried the Bearer scheme.

## Build toolchain (IMPORTANT - persistent location)
The container's `$HOME` (and therefore `~/.cargo`/`~/.rustup`) is volatile and
gets wiped between shell sessions. To avoid re-installing Rust every command:

- Rust is installed persistently under `/workspace`:
  - `CARGO_HOME=/workspace/.cargo`
  - `RUSTUP_HOME=/workspace/.rustup`
  - Toolchain: Rust 1.97.0 (matches the project requirement).
- ALWAYS prefix build/test commands with:
  `export CARGO_HOME=/workspace/.cargo RUSTUP_HOME=/workspace/.rustup && . /workspace/.cargo/env`
- If `cargo` is reported missing, reinstall once with (the rustup-init binary
  does NOT accept `--cargo-home`/`--rustup-home` flags, so set them as env
  vars first, and add `clippy` so `cargo clippy` works):
  `export RUSTUP_HOME=/workspace/.rustup CARGO_HOME=/workspace/.cargo &&`
  `curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --default-toolchain 1.97.0 --profile minimal --component clippy`
  (deploys into `/workspace/.cargo`/`/workspace/.rustup` so it survives future sessions).

## Build / test
- Clean build (no warnings, no suppression): `RUSTFLAGS="-D warnings" cargo build --bin exchange_gateway`
- Tests: `RUSTFLAGS="-D warnings" cargo test --lib` (lib unit tests) + `cargo test --test snapshot_tests` (insta snapshots).
- Long compiles: run cargo in the background and poll; redirect logs to a file
  under `/workspace/` (NOT `/tmp` - `/tmp` is also volatile across sessions).

## Architecture notes
- EWS notifications (MS-OXWSNTIF) live in `src/notifications.rs`
  (`SubscriptionManager`, `NotificationEvent`) and are rendered/wired in
  `src/ews.rs` (`handle_subscribe`, `handle_unsubscribe`, `handle_get_events`,
  `handle_get_streaming_events`, `publish_event`, `render_notification`,
  `render_notification_event`, `encode_watermark`/`decode_watermark`,
  `parse_subscription_request`). Item CRUD handlers (Create/Update/Delete/Move/Copy
  for email, calendar, contact) publish `NotificationEvent`s via `publish_event`.
- Backend: JMAP for email read/sync/send, SMTP for send + iMIP meeting transport,
  CalDAV authoritative for calendar/free-busy, CardDAV for contacts. Do NOT use
  IMAP (vestigial/unused).

## MAPI/HTTP dispatcher & ROP codec conventions (IMPORTANT)
- **RPC types** (transport.rs): Connect / Execute / Disconnect / NotificationWait / PING. The
  AddressBook `Bind`/`QueryRows`/`DnToMId`/`GetMatches`/`ResolveNames` RPCs are
  rejected as `RpcKind::AddressBook` (Phase 0); only Mailbox RPCs are live.
- **Dispatcher header convention** (handler.rs `execute_one_rop`): each ROP arm
  reads the per-spec header fields (`LogonId`, `InputHandleIndex` and, where the
  spec puts them, `OutputHandleIndex` / `ReplyCode` for some arms) directly off
  the cursor via `cur.take_u8()`, THEN calls the corresponding `*Request::decode`
  which decodes ONLY the body fields AFTER that header.
- **Therefore `*Request::decode` impls MUST NOT re-take `input_handle_index`** —
  the dispatcher consumes it; the decoder reads body-only. Decoders that still call
  `cur.take_u8()` for `input_handle_index` produce off-by-one garbage on the wire.
  Corrected so far: `RopSetColumns`, `RopQueryRows`, `RopGetStatus`,
  `RopGetPropertiesSpecific`, `RopGetPropertiesAll`, `RopSetMessageReadFlag`.
- `rops.rs` is UTF-8 sensitive: stray `\xb7` (middle-dot) bytes from earlier
  paste breaks `file_editor` reads; fix with Python byte-surgery replacing lone
  `\xb7` (not preceded by `\xc2`) with `\xc2\xb7`. Do NOT re-introduce via editor.
- **PropertyTag wire** (MS-OXCDATA §2.9): `PropertyType(2 LE) + PropertyId(2 LE)`,
  i.e. type first. `PropertyTag::decode` already reads type-first; tests MUST
  build bytes in this order or they decode to the wrong tag.
- **MAPI handle model** (session.rs): `Handle::Message { backend_id }`,
  `Handle::Folder { backend_id, kind }`, `Handle::Table { column_set, rows,
  cursor, parent_backend_id, .. }`. `TableRow` carries a `source:
  Option<Arc<dyn Any + Send + Sync>>` holding a cached `Arc<JmapEmail>` or
  `Arc<JmapMailbox>` so `RopQueryRows`/`RopGetPropertiesSpecific` can lazily
  materialise cells via `store::email_to_cells` / `store::mailbox_to_cells`
  without an extra backend round-trip per row.
- **ROps wired** (handler.rs arms): Release, OpenFolder, GetHierarchyTable,
  GetContentsTable, SetColumns, QueryRows, GetStatus, GetPropertiesSpecific,
  GetPropertiesAll, SetMessageReadFlag, CreateMessage-stub (ENUM only pending),
  SaveChangesMessage-stub, DeleteMessages-stub. **All other ~60 ROPs hit the
  `_ => NotFound` fallback** — see task tracker P2-4 for the mail write/delete/movecopy path.

## MAPI/HTTP (MS-OXCMAPIHTTP) architecture notes

The MAPI/HTTP server-side lives under `src/mapi/`:
- `transport.rs` — MS-OXCMAPIHTTP framing, `MapiRequest` (carries
  `client_info`, `password: Option<String>`, `body`), `MapiResponse`,
  `RpcKind`, `parse_request`, the Connect/Execute/Disconnect/NotificationWait/
  PING request-type dispatch.
- `logon.rs` — `RopLogon` bootstrap (MS-OXCROPS §2.2.3.1). `logon_basic`
  resolves legacyExchangeDN -> `local@mail_domain`, verifies via
  `AuthVerifier`, creates a `Session`, then `set_logon_id` + seeds handle 0
  with a synthetic `Folder { "ROOT", Root }` so the first
  `RopGetHierarchyTable` has an input handle.
- `session.rs` — `SessionManager` (RwLock<HashMap<Uuid,Session>>) + `Session`
  with a `HashMap<u8, Handle>` handle table keyed by the client-chosen
  index. `Handle` is an enum: `Folder { backend_id, kind }`, `Message
  { backend_id, mailbox_id, kind, is_new }`, `Table { kind, parent_handle,
  column_set, rows, cursor, total }`. Mutation path is `with_session_mut`
  (closure runs under the write lock, touch+lock-local ops only); read
  helpers are `with_handle` (closure borrows one handle) and `get`
  (returns an owned `SessionSnapshot`). `Session::alloc_handle/free_handle/
  set_handle/handle_mut` run inside `with_session_mut`. Passwords are NOT
  stored in the session — they arrive per-request as `MapiRequest.password`
  and are converted to `SecretString` for JMAP/CardDAV/CalDAV calls.
- `rops.rs` — the ROP codec layer. `Buf` is the fail-closed cursor
  (`take_u8/u16_le/u32_le/u64_le/i64_le`, `position`, `remaining`,
  `take_remaining`). `RopId` is a `u8` newtype. `RopHeader` (3-byte
  RopId+LogonId+handle) and `RopHeader4` (4-byte
  RopId+LogonId+InputHandleIndex+OutputHandleIndex, used by OpenFolder,
  GetHierarchyTable, GetContentsTable, CreateMessage, RegisterNotification).
  Codecs: `RopLogon(Success)`, `RopOpenTableRequest`, `RopOpenTableSuccess`,
  `RopSetColumnsRequest`/`RopSetColumnsSuccess`, `RopQueryRowsRequest`/
  `RopQueryRowsSuccess`, `RopGetStatusRequest`/`RopGetStatusSuccess`,
  `RopGetPropertiesSpecificRequest` / `RopGetPropertiesAllRequest` /
  shared `RopGetPropertiesSuccess`, `RopReleaseRequest`/
  `RopReleaseResponse`, `RopOpenFolderRequest`/`RopOpenFolderSuccess`,
  `RopCreateMessageRequest`/`RopCreateMessageSuccess`,
  `RopSaveChangesMessageRequest`/`RopSaveChangesMessageSuccess`,
  `RopDeleteMessagesRequest`/`RopDeleteMessagesResponse`,
  `RopGetMessageStatusRequest`/`RopGetMessageStatusSuccess`,
  `RopRegisterNotificationRequest`/`RopRegisterNotificationResponse`,
  `RopNotifyResponse`, `RopSetMessageReadFlagRequest`, `RopErrorResponse`.
  `RopErrorCode` is the typed MAPI HRESULT subset (Success/AccessDenied/
  InvalidParameter/NotEnoughMemory/ObjectChanged/NetworkError/
  InvalidObject/NotFound/NotInitialized/NoSupport/DiskError/Collision/
  SubmitNotSupported/Unknown).
- `store.rs` — the PURE (no async / no network I/O) bridge between MAPI
  property tags and the Stalwart backends. Well-known `PR_*` constants
  (PR_FOLDER_ID, PR_DISPLAY_NAME, PR_SUBJECT, PR_BODY, PR_BODY_HTML,
  PR_SENDER_*, PR_MESSAGE_DELIVERY_TIME, PR_MESSAGE_FLAGS, PR_ENTRYID,
  PR_MID, PR_CHANGE_KEY, PR_CONVERSATION_ID, ...). Converters:
  `email_to_cells(JmapEmail, &column_set, FolderKind, mailbox_id) ->
  Vec<PropertyValue>`, `mailbox_to_cells(JmapMailbox, &column_set)`,
  `cells_to_row`, `oneoff_entry_id`/`message_entry_id`/`folder_entry_id`
  (MS-OXCDATA §2.6.3 entry-id synthesis), `folder_id_from_backend` /
  `message_id_from_jmap` (FNV-1a 64-bit stable mapping, low bit reserved),
  `folder_kind_for_role` (JMAP mailbox role -> FolderKind),
  `container_class_for`/`message_class_for`, `folder_display_name`.
  `iso8601_to_filetime` converts RFC 3339 -> MAPI FILETIME (100-ns ticks
  since 1601-01-01). Backend FETCH is done in `handler.rs` which hands
  typed JmapEmail/JmapMailbox objects to these pure converters.
- `handler.rs` — the orchestrator. `MapiState { cfg, auth: Arc<AuthVerifier>,
  sessions }`. `handle` dispatches by RpcKind. `handle_execute` parses the
  session id out of `X-ClientInfo` (`{{uuid}}:routing`), looks up the
  session snapshot, builds a `JmapClient::new(&cfg.jmap_base)` on demand
  (the JMAP session cache inside it amortises per-username for 5 min), and
  runs a ROP-chain loop calling `execute_one_rop` per RopId. Each arm
  reads its own LogonId + handle indices per spec, mutates the session
  handle table via `with_session_mut`, optionally calls JMAP/CardDAV/
  CalDAV, and writes the ROP response bytes. Unknown ROPs return a single
  `RopErrorResponse { NotFound }` and break the chain. A decode failure
  mid-chain emits `InvalidParameter` and stops.

### Backend pick (JMAP vs IMAP/SMTP/CalDAV/CardDAV)
The MAPI dispatcher reads/sends mail over JMAP (`Email/query`+`Email/get`,
`Mailbox/query`, `Email/set` for read-flag/draft, `EmailSubmission/set` for
send). It enumerates calendars via the JMAP Calendar extension when
available (`query_calendar_events`/`get_calendar_events`) and falls back to
CalDAV (`src/caldav.rs::CaldavClient`) for free-busy/`REPORT` operations
that JMAP does not expose. Contacts enumerate via CardDAV
(`src/carddav.rs::CarddavClient::list_contacts`); the CardDAV client maps a
vCard to MAPI `IPM.Contact` rows. SMTP (`src/smtp.rs`) is used ONLY as the
iMIP/meeting-transport fallback (RFC 6047) — the primary send path goes
through JMAP `EmailSubmission/set`. Do NOT use IMAP for MAPI: JMAP is the
authoritative mail/sync read+write path and Stalwart v0.16.12 exposes a
fully-conformant JMAP endpoint; IMAP remains vestigial/unused.

### Phase-1 vs Phase-2 gaps (path to 100% Outlook fidelity)
DONE in this phase:
- Full execute-time ROP codec set + ROP-chain Execute dispatcher.
- Handle table (Folder/Message/Table with column set + cursor + rows).
- Hierarchy-table + contents-table + SetColumns + QueryRows round-trip
  against JMAP (mail mailboxes + mail contents; calendar/contacts
  contents-table still materialise row ids only).
- Read-flag + OpenFolder handle installation.
- Read-only message GetPropertiesSpecific/All (returns typed NULLs for
  Phase-1; Phase-2 fills cells from JmapEmail via store.rs
  `email_to_cells`).
- 327 lib unit tests + 11 snapshot tests green, `RUSTFLAGS=-D warnings
  cargo build` + `cargo clippy --all-targets` clean.

STILL GAPS to 100% perfect Outlook-for-Windows + Outlook-Android fidelity:
1. ~~RopQueryRows row cells~~ — **DONE Phase-2**: cells materialised from
   cached `Arc<JmapEmail>`/`Arc<JmapMailbox>` row sources via
   `store::email_to_cells`/`mailbox_to_cells` for the live column set
   (test `query_rows_materialises_email_subject_and_mid`).
2. ~~RopGetPropertiesSpecific/All live fetch~~ — **DONE Phase-2**: the
   message handle arm fetches the full `JmapEmail` (or `JmapMailbox`) and
   runs `email_to_cells`/`mailbox_to_cells` for the requested tags (typed
   NULL fallback for unknown/unsupported properties).
3. ~~RopSetMessageReadFlag Email/set $seen~~ — **DONE Phase-2**: resolves
   the message backend id from the input handle, calls
   `JmapClient::update_email` with `{keywords/$seen: true|null}`, emits
   Success.
4. `RopCreateMessage`/`RopSaveChangesMessage`/`RopDeleteMessages` handlers
   are not yet decoded in the Execute loop (`execute_one_rop` match arms
   for `ROP_CREATE_MESSAGE`/`ROP_SAVE_CHANGES_MESSAGE`/`ROP_DELETE_MESSAGES`
   fall through to the unknown-ROP `NotFound` path). The codecs exist in
   `rops.rs`; wire them in and bridge to `Email/set` +
   `EmailSubmission/set` + `Email/destroy` respectively. Drafts save to the
   JMAP `\Drafts` mailbox; Send does `EmailSubmission/set` then moves to
   `\Sent`. **(NEXT — P2-4)**
5. Calendar contents-table rows must enumerate via
   `JmapClient::query_calendar_events` (JMAP Calendar) or fall back to
   `CaldavClient::query_events`; each row converts an iCalendar VEVENT to
   MAPI `IPM.Appointment` cells (PR_START, PR_END, PR_LOCATION, etc.).
6. Contacts contents-table rows must enumerate via
   `CarddavClient::list_contacts` and convert each vCard to MAPI
   `IPM.Contact` cells (PR_FILE_AS, PR_EMAIL_*, PR_GIVEN_NAME, etc.).
7. MAPI property restrictions (`RopRestrict` / SRestriction in
   MS-OXCDATA §2.12.3) — Outlook filters message tables with these;
   Phase-1 ignores them and returns the whole contents set, capped at 200
   rows (`fetch_contents_rows`). Add a restriction-to-JMAP-filter
   translator.
8. `RopNotify` / `RopRegisterNotification` / `NotificationWait` — the
   codecs and envelope exist but the dispatcher returns an empty
   notification; Phase-2 needs a per-session notification queue fed by JMAP
   `Email/changes` + CalDAV `sync-collection`.
9. FXICS (`fxics.rs`) bulk message/folder sync (MS-OXCFXICS) — the codecs
   exist; the Execute dispatcher does not yet drive them. New Outlook
   falls back to ROP-by-ROP when FXICS is `NotFound`'d, so this is not
   day-one-blocking, but it is the bandwidth-optimal sync path.
10. Address-book endpoint (`RpcKind::AddressBook`) is rejected by the
    transport; Phase-2 needs an offline GAL stub so Outlook can resolve
    sender addresses against the JMAP `Mailbox/get` ACL set.

