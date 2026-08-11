# exchange_gateway - agent notes

## Calendar/timezone fidelity (audit gap #1 — VTIMEZONE/SRCAL round-trip)

The EAS/EWS timezone rendering no longer derives Windows `TZI` transition rules
from a hardcoded EU/US approximation (`dst_rules_for`/`EU_STD`/`EU_DST`/`US_DST`
consts are GONE). `src/timezone.rs` now derives ALL transition data by sampling
the zone's actual local offsets across a reference year with `chrono_tz`
(`zone_transitions` → `scan_full_year` → `encode_boundary` → `transition_hour`).
This is the ONE authoritative source feeding BOTH:

- `iana_to_windows_params` → the base64 EAS `Timezone` blob
  (`iana_to_eas_timezone_blob`) AND the Windows TZI `StandardDate`/`DaylightDate`
  SYSTEMTIME records, and
- `render_vtimezone_block(iana)` → the synthesised iCalendar `VTIMEZONE` block
  `render_ics` emits when an EWS/gateway-origin item has no authoritative CalDAV
  VTIMEZONE blob.

`transition_hour` is robust to both gaps (spring-forward, where the transition
hour is `None`) and folds (fall-back, where it repeats): it reports the **naive
boundary hour at which the offset first leaves the outgoing phase**, which is
the documented Windows `TZI` `wHour` convention (e.g. 02:00 Eastern both
directions, 01:00 GMT spring / 02:00 BST fall). The `wHour` is returned in the
literal `0..=23` range — a `SYSTEMTIME` `wHour` legitimately carries `0`, so a
zone whose transition falls at midnight (e.g. America/Santiago's autumn std
resume) is encoded correctly; the previous `.clamp(1, 23)` (and the matching
`.max(1)` in `encode_boundary`) shifted a midnight boundary an hour late (the
C7 correctness fix verified via a `chrono_tz` probe: `TR 2026-04-05 h00
-180→-240`). The week derivation in `encode_boundary` is intentionally
`date.day().div_ceil(7).min(4)` for the nth-weekday (1..=4) branch and `5` for
the "last weekday" branch (`date.day() + 7 > dim`); the `.min(4)` is NOT a bug —
it caps the nth-weekday index so a 5th-of-month day that is not the last weekday
of the month does not emit an out-of-spec week-5 value (week 5 means "last",
which the `+7 > dim` branch already covers). A non-week-5 nth weekday is also
clamped to the month length in `match_rule_weekday_of_month` so a malformed
4th-occurrence-in-February rule never yields an out-of-range day. Legacy IANA
alias resolution (`windows_timezone_name_for_iana`) collapses `Asia/Kolkata` ↔
`Asia/Calcutta` (`chrono_tz` exposes them as distinct enum variants) by
comparing resolved offsets at four representative instants, not enum identity.

`scan_full_year` walks `Jan 1 → Dec 31` of a fixed reference year sampling the
local offset at 12:00 each day and stops at the year boundary (a `while let`
loop with an explicit `next.year() != year` guard), so a January-1 boundary of
the following year is never mis-encoded under this year's month/week. The full
year-scan is memoised per IANA id (`TZ_PARAMS_CACHE`, a process-lifetime
`LazyLock<Mutex<HashMap>>`), so a calendar folder of N events of the same zone
triggers ONE scan, not N (the per-item render hot loop). `fixed_offset_zone`
short-circuits only the genuinely-fixed IANA categories (`UTC`, `Etc/*`, `GMT`)
to a clean zeroed SYSTEMTIME; any zone that *might* observe DST (e.g.
`Africa/Cairo`, which resumed DST in 2023) is computed from its sampled offsets
by `zone_transitions` rather than suppressed by a stale hard-coded list.

`render_vtimezone_block` emits each `STANDARD`/`DAYLIGHT` `DTSTART` as the
transition's **local** wall-clock time anchored at the epoch year 1970 (a naive
value with no trailing `Z` — RFC 5545 §3.6.5 forbids a UTC-suffixed `DTSTART`
inside a `VTIMEZONE` subcomponent; the previous UTC-suffixed emission was the
C13 correctness fix) and self-validates the result structurally
(`BEGIN:VTIMEZONE`/`TZID:`/`END:VTIMEZONE`) so the caller never receives an
unusable block.

EAS rendering (`src/sync.rs::render_calendar_app_data`): the standalone
`<Calendar:StartTimeZone>`/`<Calendar:EndTimeZone>` now carry the base64 Windows
TZI blob (`iana_to_eas_timezone_blob`) per MS-ASCAL §2.2.3.12/§2.2.3.13 — NOT the
bare IANA id (the legacy string made Outlook Android mis-derive recurrence/
exception wall-clock times for non-UTC events). The blob is `xml_escape`-d
consistently across `Timezone`/`StartTimeZone`/`EndTimeZone` (the base64
alphabet carries no XML metacharacters, but the configuration stays uniform).
UTC events omit all three elements (`is_utc_zone` gates the canonical UTC ids —
`UTC`/`Etc/UTC`/`GMT`/`Etc/GMT`/`Etc/GMT0`/`Etc/GMT±0` — since their TZI would
be all-zero and several clients reject a zeroed blob; MS-ASCAL §2.2.3.9 marks
`Timezone` OPTIONAL). The malformed `<StartTimeZone>`/`<EndTimeZone>` children
INSIDE `<Calendar:Recurrence>` were REMOVED (they are item-level properties, not
legal Recurrence children per MS-ASCAL §2.2.3.8). `map_rrule_to_recurrence_xml`
drops its unused `_timezone`/`_all_day` params (single call site, simplified).

EWS rendering (`src/ews.rs::render_ews_calendar_item_xml_with_shape`):
`<t:StartTimeZone>`/`<t:EndTimeZone>` now emit the canonical attribute form
`<t:StartTimeZone Id="..." Name="..."/>` / `<t:EndTimeZone Id="..." Name="..."/>`
(`Id`/`Name` are *attributes* of `TimeZoneDefinitionType` per the EWS schema and
the EWS Managed API, NOT the `<t:Id>`/`<t:Name>` child-element shape), and
`<t:MeetingTimeZone>` emits the Windows id in its `TimeZoneName` **attribute**
(`<t:MeetingTimeZone TimeZoneName="..."/>`, per the legacy `SerializableTimeZone`)
— NOT the raw `timezone_blob` (which for a CalDAV-origin item is the multi-line
authoritative iCalendar VTIMEZONE block and would corrupt the EWS envelope as
element text). The inbound parse path (`extract_ews_timezone_field_doc`) reads
the id from element text first (back-compat with the legacy emit and
`<t:Value>` children) then falls back to the `Id`/`TimeZoneName` attribute, so a
real Outlook `GetItem`/`UpdateItem` echo of the attribute form round-trips.

CalDAV round-trip (`src/calendar.rs::render_ics`): when `item.timezone_blob`
parses as a real VTIMEZONE, it is re-emitted byte-for-byte (the authoritative
TZID/RRULE UNTIL boundaries CalDAV stored are preserved). When it does NOT
parse (e.g. an EWS-origin item where `timezone_blob` was a bare Windows name
captured from `MeetingTimeZone` — including the `Calendar::from_str` `Err`
branch), OR when `timezone_blob` is `None` entirely, `render_ics` synthesises a
canonical VTIMEZONE from `item.timezone` (IANA) via `render_vtimezone_block`
(via a consolidated `push_vtimezones` helper that wraps a raw block in a
throwaway VCALENDAR and returns whether any VTIMEZONE was pushed) so the edited
event round-trips with a real, RFC 5545-valid zone definition whose transition
RRULE agrees with the Windows TZI blob the EAS/EWS path advertises (no drift
between transports). A guard ensures no orphan `DTSTART;TZID=...` is emitted: if
neither the authoritative blob nor the synthesised fallback produces a
VTIMEZONE, the master DTSTART falls back to the absolute UTC instant (with `Z`)
rather than referencing a missing VTIMEZONE.

Tests added: 8 in `timezone.rs` (US Eastern 2nd-Sun-Mar / Santiago southern
hemisphere + midnight std-resume / Sydney / London last-Sun / Kolkata no-DST /
EAS blob round-trip / direct `render_vtimezone_block` US-Eastern local-naive
no-`Z` / Kolkata fixed-offset block), 3 in `sync.rs` (EAS base64 StartTimeZone +
UTC omission gate for `timezone: None` + explicit UTC-id omission gate), 4 in
`calendar.rs` (DST-crossover weekly-recurrence round-trip through
render_ics→parse_ics_event with synthesized VTIMEZONE; authoritative CalDAV
VTIMEZONE preserved byte-for-byte; VTIMEZONE DTSTART local-naive no-`Z`
regression; malformed-blob → synthesized-VTIMEZONE no-orphan round-trip). 600
lib + 11 snapshot = 611 green, `RUSTFLAGS="-D warnings" cargo build --bin
exchange_gateway` + `cargo clippy --all-targets` clean. `TZ_BLOB_LEN` is
`pub(crate)` so the EAS test can assert
the documented 172-byte blob length.

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
- **OAB (Offline Address Book)** — `src/oab.rs` (MS-OXOAB + MS-OXWOAB). Closes
  audit gap §1.1. Autodiscover advertises `<OABUrl>` (in EXCH + EXPR Protocol
  blocks of `handle_outlook_xml`) plus `OABUrl`/`ExternalOABUrl`/`InternalOABUrl`
  SOAP user settings — built by `autodiscover::oab_url(host)` →
  `https://{host}/OAB/{OAB_SERVER_GUID}/` (the GUID constant lives in
  `autodiscover.rs`; it is intentionally stable so a client cache survives a
  container restart). Route `/OAB/{guid}/{file}` → `main::oab_download` →
  `oab::handle_oab`, which rejects unknown GUIDs with 404, authenticates via
  Basic auth + the shared `AuthVerifier` (no creds ⇒ 401 `Basic realm=...`),
  serves `oab.xml` (the OAB manifest with `<OABVersion>3`, `<ContentVersion>`,
  `<Size>`, `<Hash>` = SHA-256, ETag/Last-Modified) and the binary full OAB
  generated as an **OAB v3 details file** (MS-OXOAB §3.2 — `OAB_HDR` +
  `B2_REC` per recipient carrying X500 DN via `synth_dn`, SMTP, display name,
  alias; bDispType=DT_MAIL_USER, bObjType=MAPI_MAILUSER). v3 details is the
  uncompressed OAB container (v4 `.lzx` needs an LZX delta codec that no Rust
  crate provides). Payload is built from `AppState.directory` via
  `search_blocking("*", Some(5000))` on the blocking pool (the directory
  rejects `""` as `InvalidQuery`; an unsupported wildcard falls back to a
  header-only empty OAB). Size + ETag are derived from the SAME built bytes
  (helpers `payload_etag`, `oab_size_and_etag`) so the manifest's `<Size>`/
  `<Hash>` match the served file. Conditional GET (`If-None-Match`) ⇒ 304;
  single contiguous `Range` honoured (multi-range unsupported, falls back to
  whole file). NOT yet wired: the audit's broader §2d GAL/NSPI stub (out of
  scope for the OABUrl gap).

- **ECP (Exchange Control Panel) settings surface** -- `src/ecp.rs` (closes
  audit gap §1.3 / "No `<EcpUrl>` real value"). Autodiscover now advertises
  a real ECP base URL instead of the EWS SOAP endpoint: `autodiscover::ecp_url(host)`
  -> `https://{host}/ecp/` (trailing slash; it is a virtual directory like `<OABUrl>`).
  `handle_outlook_xml` emits `<EcpUrl>{ecp_url}</EcpUrl>` in BOTH EXCH + EXPR Protocol
  blocks, and `handle_autodiscover_soap` emits `ExternalEcpUrl`/`InternalEcpUrl` user
  settings carrying the same value (no longer `/EWS/Exchange.asmx`). Outlook / New
  Outlook deep-link OOF / OptIn / Regional panels by appending to `<EcpUrl>`; the
  endpoint `/ecp`, `/ecp/`, `/ecp/{*path}` (GET) -> `main::ecp_root`/`ecp_path` ->
  `ecp::handle_ecp` serves the backing virtual directory so the panels never 404.
  Auth: Basic auth + the shared `AuthVerifier` (mirrors `oab::handle_oab`; no creds
  => 401 `Basic realm="Exchange Gateway"`). The page is a **fully static, semantic
  HTML** document with NO inline scripts/styles/external resources (the gateway's
  global CSP is `default-src 'none'; frame-ancestors 'none'; sandbox`, so a
  resource-free doc is the only CSP-clean shape that always renders) and NO form
  (`form-action` defaults to `none` under that CSP). The trailing deep-link path is
  HTML-escaped (5-metachar escaper `html_escape`) and echoed as page context only;
  authoritative OOF/regional writes stay on EWS SOAP (`SetUserOofSettings`).
- **Autodiscover AuthPackage / HMA advertisement** — closes audit gap §1.2.
  `handle_outlook_xml` takes an `&AuthAdvert` describing what the EXCH + EXPR
  `<Protocol>` blocks advertise under `<AuthPackage>`. `AuthAdvert::Basic`
  keeps the legacy `<AuthPackage>Basic</AuthPackage>` (default,
  backwards-compatible). `AuthAdvert::Modern { oauth_url }` renders
  `<AuthPackage>OAuth2/CertificateBased</AuthPackage>` plus sibling
  `<OauthUrl>{oauth_url}</OauthUrl>` and
  `<CompactDomain>{issuer_host}</CompactDomain>` (issuer host extracted by
  `host_authority`) in BOTH blocks so New Outlook for Windows provisions via
  Modern Auth (HMA). `main::autodiscover_auth_advert(&cfg)` builds the advert
  from `Config`: `Modern` when `mapi_hma_enabled && !mapi_oidc_issuer.is_empty()`
  (the same gate that wires `oidc::TokenVerifier` on the bearer path), else
  `Basic`. The SOAP `handle_autodiscover_soap` advertises Basic as before
  (EWS clients use the SOAP GetUserSettings surface, not the EXCH/EXPR
  Protocol blocks, so HMA advertisement there is not required for §1.2).
- **Mobilesync `<User>/<DisplayName>` resolution** — closes audit gap §1.5
  ("the mobilesync path doesn't include a `<DisplayName>`/picture and the
  `User` block is minimal"). The mobilesync `User` block's only children per
  MS-ASCMD §2.2.3.189 are `<DisplayName>` (§2.2.3.49.1 — "the user's display
  name in the directory service", optional 0…1) and `<EMailAddress>`; there is
  NO picture element in this schema (a client's contact photo arrives via the
  EAS Settings/ResolveRecipients/Find `Picture` element, not mobilesync
  Autodiscover). The handler previously hard-coded the gateway product brand
  "Stalwart Mail" as `<DisplayName>`, which is not the user's name. Now
  `handle_mobilesync_xml(host, email, accept_language, display_name)` renders
  the *user's* resolved name: `autodiscover::resolve_user_display_name(dir,
  email)` runs `DirectoryLookup::resolve_email_blocking(email)` (on a
  `spawn_blocking` task in `main::autodiscover_xml`, never on the async
  runtime) → `Contact.display_name`; when no directory is configured or the
  account is unresolvable it falls back to `autodiscover::derive_display_name
  (email)` (title-cased local-part of the *client-supplied request* email,
  capped at 512 chars, never leaks anything the client didn't send). An empty
  resolved name **omits** the optional `<DisplayName>` (spec 0…1) so the
  response never advertises a fabricated identity; a non-empty name is
  XML-escaped via `xml_escape`. The `Outlook` desktop path
  (`handle_outlook_xml`) keeps its own server-brand `<DisplayName>`. Because
  dispatching needs 8 inputs (host/body/email/accept_language/mail_host/
  include_imap_smtp/auth_advert/mobilesync_display_name), `handle_autodiscover_xml`
  now takes a single `AutodiscoverXmlRequest<'a>` struct built by a struct
  literal at each call site (intentionally no positional constructor, so
  clippy's argument-count threshold is not re-tripped; no `#[allow]`
  suppression).
  - **Review hardening (PR #1821)** — `main::autodiscover_xml` gates displayName
    resolution three ways: (1) **schema-gated** — `autodiscover::is_mobilesync_schema
    (body)` pre-checks so Outlook-desktop requests (which never read
    `mobilesync_display_name`) skip the work entirely, and `derive_display_name`
    (the no-directory fallback, pure string work) is never offloaded to
    `spawn_blocking`; only the directory `resolve_email_blocking` path uses a
    blocking thread. (2) **security-gated** — the directory is consulted ONLY when
    the request carries Basic creds that authenticate against Stalwart via
    `AuthVerifier::verify` AND the authenticated principal's canonical email
    (`util::canonicalize_username(user, mail_domain)`) matches the requested
    email; anonymous or mismatched callers get only the disclosure-free
    `derive_display_name` fallback (built solely from the client-supplied email),
    closing a directory-name enumeration / PII-disclosure vector. (3)
    `spawn_blocking` `JoinError`s are logged (`tracing::warn!`, redacted email)
    rather than silently dropped (`.await.ok()` removed) and fall back to
    `derive_display_name`. `derive_display_name` now takes the **leading run** of
    name chars (stops at the first disallowed char) and caps the **rendered
    output** to 512 chars after case expansion (not the input bytes), so the
    documented bound holds even for `ß`→`SS`-style Unicode expansion.
    `resolve_user_display_name` drops the intermediate `trim().to_string()`
    (one allocation instead of two). A follow-up commit on PR #1821 further
    hardened the auth gate: the old `extract_basic_credentials` was folded
    into a single shared `decode_basic_auth` decoder (`extract_basic_password`
    delegates to it, so MAPI and Autodiscover can no longer drift in
    malformed-header handling); the gating `&&` chain was reordered to
    short-circuit the canonical-principal match BEFORE the Stalwart
    `verifyCredentials` round-trip (DoS-amplification guard), it now verifies
    the canonical username (consistent with every other authenticated path),
    and the password is held in a zeroized `secrecy::SecretString` for the
    lifetime of the check rather than a bare `String`.
- **Server version advertisement — single source of truth** — closes audit
  gap §4 ("`ServerVersion` is hard-coded to `15.20.0.0` and `Exchange2016`").
  `src/version.rs` owns `ServerVersion`, the ONE place the Exchange server
  version is defined; every EWS `<t:ServerVersionInfo>`,
  Autodiscover SOAP `<a:ServerVersionInfo>`, Autodiscover Outlook
  `<ServerVersion>` (emitted in BOTH the EXCH and EXPR Protocol blocks), and
  the `ExternalEwsVersion`/`InternalEwsVersion`/`EwsSupportedSchemas` user
  settings render from `version::current()` — the old hard-coded `15.20.0.0`
  *build stamp* literal is gone from every stamp site. (`Exchange2016` is NOT
  a stray literal here: it is the intentionally-advertised EWS schema token and
  the intentional top of the supported-schema matrix — see below.)
  Defaults match the latest stable on-premises build the gateway emulates:
  **Exchange Server SE** `15.2.2562.45` (Major=15 Minor=2 MajorBuildNumber=2562
  MinorBuildNumber=45). The advertised EWS schema token is `Exchange2016` — the
  highest universally-valid `RequestServerVersion` (`ExchangeVersionType`) enum
  value. There is NO published `Exchange2019` enum member: real 15.2.x servers
  reject `RequestServerVersion Version="Exchange2019"` with
  `ErrorInvalidRequest`, so `Exchange2016` is the correct on-premises schema
  token even though the *build* is the SE 15.2.x line — the gateway stamps the
  SE build number and product name but advertises the `Exchange2016` schema.
  Operators can pin a different build via `GATEWAY_SERVER_VERSION`
  ("Major.Minor.Build.Revision") and `GATEWAY_SERVER_EXCHANGE_VERSION` (any valid
  EWS enum token at or below `Exchange2016`); both are validated in
  `Config::validate` (fail-closed at startup — pinning `Exchange2019` is
  rejected). `EwsSupportedSchemas` is truncated at the configured schema token
  so a pinned older build never advertises schemas newer than itself.
  `version::init` is called once from `main` after `Config::load`;
  `version::current()` lazily defaults to the SE build when a caller (e.g. a
  unit test) never calls `init`, so leaf render helpers in `ews.rs`,
  `autodiscover.rs`, and `protocol_fixtures.rs` always emit a valid stamp with
  NO per-caller plumbing. When adding a new EWS/Autodiscover envelope, do NOT
  inline a `<ServerVersionInfo>` literal — call
  `version::current().render_ews_header(EWS_TYPE_NS)` (or
  `render_autodiscover_soap_header()` / `render_server_version_element()`).

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
  SaveChangesMessage-stub, DeleteMessages-stub, **and the full stream family**
  (OpenStream·0x2B, ReadStream·0x2C, WriteStream·0x2D, SeekStream·0x2E,
  SetStreamSize·0x2F, CommitStream·0x5D, GetStreamSize·0x5E — audit gap §2a).
  The stream arms resolve `PR_BODY`/`PR_BODY_HTML`/`PR_RTF_COMPRESSED`
  (empty) via `store::email_body_stream_bytes` lifted straight off the cached
  `JmapEmail`, and `PR_ATTACH_DATA_BIN` via `store::email_attachment_blob`
  + a lazy `JmapClient::download_blob` on the first ReadStream (the blob id is
  packed into `Handle::Stream.backend_id` as `<emailId>\x1F<blobId>` so the
  stream survives a `RopRelease` of the source Message handle). Writes are
  staged in the handle buffer and flushed at SaveChangesMessage. **All other
  ~50 ROPs hit the `_ => NotFound` fallback** — see task tracker P2-4 for the
  mail write/delete/movecopy path.
  - **Table-navigation + FastTransfer ROPs (audit gap §2b)** — `execute_one_rop`
    now also drives the table-navigation family: `RopSortTable`·0x13,
    `RopRestrict`·0x14, `RopQueryPosition`·0x17, `RopSeekRow`·0x18,
    `RopSeekRowBookmark`·0x19, `RopSeekRowFractional`·0x1A,
    `RopCreateBookmark`·0x1B, `RopFreeBookmark`·0x89, `RopResetTable`·0x81.
    `Handle::Table` carries `restriction: SRestriction`, `sort_orders:
    Vec<SortOrder>`, `next_bookmark: u64`; the `RopQueryRows` arm honours the
    active restriction by building a filtered row-index view
    (`matcher_cells` + `SRestriction::matches`), and `RopSortTable` stable-sorts
    the row buffer in place via `sort_rows`/`scalar_ord_for_sort` (bit 0x01 of
    `SortOrder.sort_flags` ⇒ descending). `RopSeekRow*` clamp the cursor at the
    (post-restrict) table bounds and report `has_sought_less` on clamping.
    `RopCreateBookmark` packs `(row_index | (next_id<<32))` into the 8-byte
    bookmark; `RopFreeBookmark` is a stateless ack. The FastTransfer *source*
    family (`RopFastTransferSourceCopy{Messages,Folder,To,Properties}`
    0x4B/0x4C/0x4D/0x69 + `RopFastTransferSourceGetBuffer` 0x4E) install a
    `Handle::FastTransferSource` carrying the ICS byte stream built by
    `build_ics_stream`; GetBuffer serves chunks up to `BufferSize` and flips
    `TransferStatus` to Done. The FastTransfer *destination* +
    `RopSynchronization*` upload ROPs (0x53/0x54/0x70/0x72-0x78/0x77/0x80)
    accumulate onto `Handle::FastTransferDestination` and tokenise via
    `fxics::Tokenizer`; the end-of-stream PutBuffer now drives
    `apply_fasttransfer_upload` — the Phase-2 FXICS upload apply -> JMAP
    write-back bridge (audit gap #2, closing the old "best-effort; no JMAP
    write-back bridge wired" stub). It tokenises the upload byte stream and
    dispatches the FXICS spans to JMAP reusing the existing `JmapClient`
    primitives (NO new deps): `IncrSyncRead` -> a single batched `Email/set {
    keywords/$seen }` update; `IncrSyncDel` -> a single `Email/destroy`;
    `IncrSyncMessage` (inside `IncrSyncChg`) -> a property `Email/set` `update`
    patch for the cleanly-mappable MAPI subset (read flag via
    `PR_MESSAGE_FLAGS`/`PR_READ`, follow-up via `PR_FLAG_STATUS`, importance via
    `PR_IMPORTANCE`, subject via `PR_SUBJECT`), AND a cross-folder `mailboxIds`
    patch when the bag carries a `PR_FOLDER_ID` differing from the parent. The
    MAPI-mid -> JMAP-id reverse map is built ONCE per apply by enumerating the
    parent folder (`list_email_ids_in_mailbox`) and matching by
    `message_id_from_jmap` (the one-way FNV-1a hash can't be reversed) — the
    same pattern `RopDeleteMessages` uses, so the upload amortises to ONE folder
    query plus one `Email/set` per batched span. A mid that does not resolve is
    skipped, not failed. Each event is best-effort: an untranslatable item is
    `warn!`-logged and skipped so a single bad item never aborts the rest of
    the upload; a malformed FXICS byte stream fails closed (`Err(DecodeError)`
    -> dispatcher `DiskError`). When JMAP/creds/`account_id` are absent
    (unit-test / no-backend) the apply tokenises + logs but issues no writes
    (the established "no-backend -> tokenize-only" contract). `Marker::end_marker()`
    now pairs `IncrSyncMessage` -> `EndMessage` per MS-OXCFXICS §2.2.3.2.4 (the
    gateway's download `build_ics_stream_iter` already emits
    `IncrSyncMessage` ... `EndMessage`), so a client echoing that shape on
    upload tokenises instead of failing closed at the unmatched `EndMessage`.
    The full-message *create* over the bulk upload (brand-new unresolved mid)
    needs the MIME/body Blob-upload write-back bridge (audit gap #3) and is
    intentionally best-effort here; pure-create-over-FXICS with summary cells
    cannot synthesise a full JMAP Email object. Tests: 12 added in
    `mapi::handler::tests`. 612 lib + 11 snapshot = 623 green,
    `RUSTFLAGS=-D warnings cargo build --bin exchange_gateway` + `cargo clippy
    --all-targets` clean. The decoders follow the body-only convention; the
    dispatcher consumes the 3- or 4-byte ROP header (LogonId + InputHandleIndex
    [+ OutputHandleIndex]) before `*::decode`/`*::decode_after_ropid` reads
    the body — exactly like every other arm, so adding a new FastTransfer arm
    MUST `cur.take_u8()` the LogonId/handle bytes first or the chain desyncs
    by one byte.
  - **Stream-ROP review hardening (PR #1830)** — `RopSeekStream::resolve`
    now works in `u64`/`Option<u64>` end-to-end (never reinterprets a `>i64::MAX`
    `u64` back through `as i64`, which bitwise-wraps to negative) and clamps by
    the *requested offset sign* on `checked_add_signed` overflow, so negative /
    positive overflow lands at `0` / `len` predictably instead of a stale raw
    offset. `Handle::Stream` carries an optional `known_len: Option<u64>`
    (the JMAP-declared attachment size captured at `RopOpenStream`) so an
    attachment blob that has not yet been downloaded still reports a real
    `RopGetStreamSize`/`OpenStream` size, and OpenStream caps a not-yet-fetched
    attachment against `cfg.max_attachment_bytes()` (rejects over-ceiling with
    `NotEnoughMemory`). Per-stream write size is capped (item L); `SetStreamSize`
    over the spec ceiling (`2^31`) AND the configured cap ⇒ `NotEnoughMemory`.
    All five stream-success encoders early-return on a non-Success `ReturnValue`
    so no body bytes ever leak past an error (items B/K). `RopSaveChangesMessage`
    with a *dirty body stream* now returns `NoSupport` instead of a silent
    Success (the body write-back bridge is not yet wired), while a clean
    message falls through to the backend create path. `email_body_stream_bytes`
    encodes `PTYP_STRING`/`PTYP_WSTRING` bodies as **UTF-16LE** (item I) and
    multi-attachment selection fails closed as ambiguous `NoSupport` (item N).
  - **Attachment-ROP review hardening (PR #1832)** — the
    `RopDeleteAttachment`/`RopSaveChangesAttachment` dispatcher arms MUST
    consume the per-ROP `LogonId` byte (`let _logon = cur.take_u8()?;`)
    immediately before `<Req>::decode_after_ropid(cur)` — `decode_after_ropid`
    reads body-only (after RopId+LogonId), exactly like every other
    `decode_after_ropid` arm (`RopCommitStream`/`RopReadStream`/…). Skipping
    the consume shifts every request field by one byte and desyncs the ROP
    chain; this is the #1 architectural trap for new arms (caught by cubic +
    coderabbit on #1832). `Handle::Attachment` now carries
    `size: Option<u64>` captured at `RopOpenAttachment` from JMAP
    `attachments[].size`, so the Attachment-handle `RopOpenStream`
    fast-path reports a real `StreamSize`/`RopGetStreamSize` AND caps the
    blob against `cfg.max_attachment_bytes()` → `NotEnoughMemory` (a
    `RopErrorResponse`, not a success-shape envelope) before any download.
    That fast-path is **gated to `PR_ATTACH_DATA_BIN` + `PTYP_BINARY`** (a
    non-data-bin/metadata query on an Attachment handle falls through to the
    legacy path → `NoSupport`, never the binary payload), requires a
    **non-empty `blob_id`** (an empty blob_id falls through rather than
    packing `<email>` alone), and packs `backend_id` as the invariant
    `<emailId>\x1F<blobId>`. `RopGetAttachmentTable`/`RopGetValidAttachments`
    **split `Ok(None)` (empty result) from `Err` (typed `DiskError` + `warn!`)**
    rather than masking a transient JMAP failure as "no attachments"
    (`RopOpenAttachment` already did this; both now match).
    `RopSetProperties` on an `Handle::Attachment` returns a typed `NoSupport`
    (not a silent `NotFound`) since attachment metadata is read-only JMAP
    capture until the body/MIME-rewrite bridge lands. `RopGetPropertiesSpecific`
    on an Attachment handle serves cells from the **cached handle metadata**
    (name/content_type/size, rebuilt into a `JmapAttachment` for
    `store::attachment_to_cells`) with NO `Email/get` round-trip — reserving the
    fetch as a fallback only for a degenerate handle. `store::PR_ATTACH_SIZE`
    uses `saturate_i32` (not `as i32`) so a ≥2 GiB attachment never wraps
    negative; `PR_ATTACH_EXTENSION` keeps the **leading dot** (`.txt`, not
    `txt`) per MS-OXPROPS. The `JmapClient::upload_blob`/`UploadedBlob`
    draft was **removed** as dead code (no caller; `Blob/upload` write-back
    is not wired) — re-add when the MAPI→JMAP attachment write-back bridge
    lands and wire+test the caller at the same time.
  - **Calendar/Contacts MAPI cell materialisation (audit §2c, PR #1842)** —
    `src/mapi/converters.rs` owns the CalDAV `CalendarItem` → `IPM.Appointment`
    cell converter (`calendar_to_cells`) and the CardDAV vCard → `IPM.Contact`
    cell converter (`contact_to_cells`). The contents-table builder in
    `src/mapi/handler.rs` (`fetch_calendar_rows`/`fetch_contact_rows`) materialises
    `TableRow`s carrying a cached `Arc<CalendarItem>` / `Arc<String>` (raw vCard)
    source so `RopQueryRows`/`RopGetPropertiesSpecific` lazily run these pure
    converters with NO extra backend round-trip per row (mirrors the email
    `Arc<JmapEmail>` source pattern). The synthetic Calendar/Contacts folder
    rows (`synth_folder_row`) now carry the role/store-backend id constants
    (`store::CALENDAR_BACKEND_ID`/`store::CONTACTS_BACKEND_ID`) — NOT string
    literals — and `store::folder_kind_for_role` / `folder_kind_for_backend_id`
    resolve those to `FolderKind::Calendar`/`Contacts` so `mailbox_to_cells`
    renders `PR_CONTAINER_CLASS=IPF.Appointment`/`IPF.Contact` and the
    contents-table-open step picks the right kind. **Review hardening (PR
    #1842 follow-up)** on the converters:
    (a) `PR_APPOINTMENT_SUB_TYPE` (was mis-typed `PPR_APPOINTMENT_SUB_TYPE`)
    and `PR_INITIALS` (id was `0x800A`, corrected to `0x3A0A`); added
    `PR_MIDDLE_NAME` (`0x3A44`), `PR_GENERATION` (`0x3A05`),
    `PR_OTHER_ADDRESS_{STREET,CITY,STATE,POSTAL,COUNTRY}` (`0x3A5C`/`0x3A5F`/
    `0x3A5E`/`0x3A5D`/`0x3A60`),
    `PR_PREDECESSOR_CHANGE_LIST` (`0x65E8`) wired into both calendar and
    contact converters (empty XID list — no predecessor change keys).
    (b) The MS-OXOCAL `AppointmentRecurrencePattern` blob
    (`recurrence_pattern_bytes`) now serialises the *full* documented
    structure: RecurrenceType/PatternType/CalendarType/FirstDateTime/
    Interval/WeekIndex/FirstDOW/OuterDuration/**AdditionalFlags**/
    PatternSpecific/**EndTime**/**OccurrenceCount**/ModifiedInstanceCount/
    DeletedInstanceCount/[DeletedInstanceDates...]. `parse_until` now
    resolves iCalendar **compact-basic** UNTIL (`20251231T235959Z`) AND
    date-only forms (the old `chrono::DateTime::parse_from_rfc3339` rejected
    the compact form, silently dropping the bound); the END_DATE flag
    (AdditionalFlags bit `0x01`) is set exactly when EndTime != 0; COUNT maps
    to OccurrenceCount (EndTime 0, no END_DATE flag); EXDATE deletions plus
    `deleted` exceptions now serialise into the `DeletedInstanceDates[]`
    array (one 8-byte FILETIME each, matching `DeletedInstanceCount`) so the
    blob is structurally consistent (Outlook's decoder always reads exactly
    `DeletedInstanceCount` FILETIMEs after the count — a count != 0 with no
    trailing dates previously made Outlook drop the recurrence). The dead
    `until_ft != 0 && false` placeholder block is removed.
    (c) `global_object_id` now serialises the full MS-OXOCAL §2.2.5.1
    ByteArrayStructure: fixed 16-byte meeting-namespace ByteArrayID
    (`04 00 00 00 82 00 E0 00 74 C5 B7 10 1A 82 E0 08`) + Year(2 LE)/Month(1)/
    Day(1) (derived from `item.start`) + CreationTime(8 FILETIME) +
    Reserved(8) + Size(4 LE) + Data (the iCalendar UID) + terminating NUL —
    replacing the previous minimal prefix blob. `change_key` mixes
    `item.dtstamp` (which CalDAV bumps on every mutation) on top of the
    stable UID so an edited appointment yields a different key.
    (d) Contact `PR_CHANGE_KEY` (`contact_change_key`) now mixes a stable
    digest of the **raw vCard body** (.lines() with BEGIN/END stripped,
    trimmed; `DefaultHasher` is a fixed-key SipHash13 so it is build-stable)
    on top of the immutable vCard UID — a UID-only key would hide edits from
    Outlook's stale-row + conflict-detection. `PR_RECORD_KEY`/`PR_SEARCH_KEY`
    remain the stable UID. (e) The calendar contact GetPropertiesSpecific
    fallback arms in `handler.rs` pass `store::CALENDAR_BACKEND_ID`/
    `store::CONTACTS_BACKEND_ID` as `mailbox_id` to
    `calendar_to_cells`/`contact_to_cells` (was `""`), so PR_ENTRYID is
    synthesised with the correct store id. (f)
    `parse_calendar_multistatus` now `buf.clear()`s the quick-xml scratch
    buffer each iteration (matches the established `src/caldav.rs` pattern)
    so the buffer never retains memory proportional to the largest event.
    (g) `fetch_calendar_rows` widened to ±730 days (~4-year span) so Outlook's
    contents-table view shows upcoming + recent appointments. Disputed
    QR1 (`expose_secret()` → `&str` password for CaldavClient/CarddavClient)
    is the established codebase pattern (JmapClient/ews.rs/calendar.rs pass
    `&str` from `expose_secret()`; `SecretString` minimises dwell + clears on
    drop; HTTP Basic fundamentally requires plaintext at the wire boundary) —
    NOT refactored, answered in the PR comment instead.

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
5. ~~Calendar contents-table rows must enumerate via
   `JmapClient::query_calendar_events` (JMAP Calendar) or fall back to
   `CaldavClient::query_events`; each row converts an iCalendar VEVENT to
   MAPI `IPM.Appointment` cells (PR_START, PR_END, PR_LOCATION, etc.).~~ — **DONE
   Phase-2 (audit gap §2c)**. `src/mapi/converters.rs::calendar_to_cells`
   renders an `IPM.Appointment` row from a `CalendarItem`
   (PR_SUBJECT/PR_LOCATION/PR_START/PR_END FILETIME/PR_BUSY_STATUS/
   PR_RESPONSE_STATUS/PR_RECURRING + PR_RECURRENCE_PATTERN binary per
   MS-OXOCAL §2.2.4 with Daily/Weekly/Monthly/Yearly RecurrenceType and
   INTERVAL/OCCURRENCES/OCCURRENCE_COUNT/UNTIL termination, GlobalObjectId +
   CleanGlobalObjectId per §2.2.5, PR_CHANGE_KEY/PR_PREDECESSOR_CHANGE_LIST
   per MS-OXCDATA §2.12.2). `handler.rs::fetch_calendar_rows` queries the
   CalDAV collection (wide ±2yr window via `CaldavClient::query_events`) and
   parses the `<C:calendar-data>` multistatus (`parse_calendar_multistatus`)
   into `TableRow`s whose `source` caches the `Arc<CalendarItem>`; the
   `RopQueryRows` lazy materialiser downcasts that cache into
   `IPM.Appointment` cells for the live column set. A synthetic
   `__calendar__` JmapMailbox folder row is injected into the hierarchy table
   (`synth_folder_row`, container class `IPF.Appointment`) so Outlook sees the
   Calendar folder; `folder_kind_for_backend_id(CALENDAR_BACKEND_ID)`
   (`"__calendar__"`) routes `RopGetContentsTable` on that folder handle to
   `fetch_calendar_rows`. GetPropertiesSpecific on a Calendar Message handle
   re-queries the CalDAV window and matches by FNV-1a row id (the stable
   mapping of the iCalendar UID) as a fallback when the row cache is absent.
6. ~~Contacts contents-table rows must enumerate via
   `CarddavClient::list_contacts` and convert each vCard to MAPI
   `IPM.Contact` cells (PR_FILE_AS, PR_EMAIL_*, PR_GIVEN_NAME, etc.).~~ — **DONE
   Phase-2 (audit gap §2c)**. `src/mapi/converters.rs::contact_to_cells`
   parses a raw vCard (vCard 3.0 + Outlook-style `X-MS-` extensions) and
   renders an `IPM.Contact` row (PR_DISPLAY_NAME/PR_FILE_AS with FILE-AS
   precedence explicit FILE-AS/X-FILEAS → "Family, Given" → FN → email,
   PR_GIVEN_NAME/PR_SURNAME/PR_MIDDLE_NAME/PR_GENERATION from `N:`,
   PR_EMAIL_ADDRESS + PR_EMAIL1/2/3_ADDRESS, PR_BUSINESS_TEL/PR_HOME_TEL/
   PR_MOBILE_TEL/PR_OTHER_TEL/PR_HOME_FAX with TYPE label routing,
   PR_TITLE/PR_COMPANY_NAME/PR_DEPARTMENT_NAME from `ORG`/`TITLE`,
   PR_HOME_ADDRESS_* / PR_OTHER_ADDRESS_* from `ADR`,
   PR_ENTRYID synthesized from the vCard UID, PR_CHANGE_KEY). `handler.rs::fetch_contact_rows`
   enumerates via `CarddavClient::list_contacts` and caches the raw vCard
   `String` on each `TableRow`; the `RopQueryRows` lazy materialiser
   downcasts that cache into `IPM.Contact` cells. A synthetic `__contacts__`
   JmapMailbox folder row (container class `IPF.Contact`) is injected into
   the hierarchy table so Outlook sees the Contacts folder;
   `folder_kind_for_backend_id(CONTACTS_BACKEND_ID)` (`"__contacts__"`)
   routes `RopGetContentsTable` on that folder handle to `fetch_contact_rows`.
   GetPropertiesSpecific on a Contacts Message handle re-enumerates CardDAV
   and matches by FNV-1a row id when the row cache is absent.
7. ~~MAPI property restrictions (`RopRestrict` / SRestriction in
   MS-OXCDATA §2.12.3)~~ — **DONE Phase-2**: `src/mapi/restrict.rs` owns the
   typed `SRestriction` codec (And/Or/Not/Exist/{Property,Content,BitMask,Size,
   CompareProperties}Restriction/SubRestriction/Comment/Count) with a
   `matches(&[CellForMatcher])` evaluator (RelOp over i64/f64/bool/string
   scalars, FL_IGNORECASE substring/prefix content match, bitmask EqZero/EqNonZero,
   size relops). `RopRestrict` stores the restriction on `Handle::Table` and the
   `RopQueryRows` arm now serves only rows the active restriction admits (cursor
   indexes the filtered view; `total` re-derived to the filtered count). Audit
   gap §2b `RopRestrict` closed.
8. ~~`RopNotify` / `RopRegisterNotification` / `NotificationWait`~~ — **Phase-2
   DONE (audit gap §2e)**: real per-session notification delivery is now wired.
   `src/mapi/session.rs` owns `NotificationRegistry` + `MapiNotificationSink`
   (a `tokio::sync::broadcast::Receiver` reused from the shared
   `SubscriptionManager` via `subscribe_raw()` — the SAME feed the EWS
   `subscribe/get_events/get_streaming_events` path publishes to, closing the
   "EWS events not fed into MAPI queues" half of the gap; no separate JMAP
   `Email/changes` + CalDAV `sync-collection` poller is added because item-CRUD
   handlers already `publish_event` into this feed). `MapiNotificationSink::
   accepts(&NotificationEvent)` filters by owner (canonical principal email) +
   requested `NotificationTypes` bitmask + `NotificationScope` (WholeStore or a
   single folder backend id); `pump()` drains the broadcast receiver into a
   bounded `pending: VecDeque<NotificationEvent>` (`SINK_PENDING_CAP`) and
   silently resyncs on `RecvError::Lagged`. The `RopRegisterNotification` arm
   in `execute_one_rop` builds the sink from the request's
   (notification_types | scope | want_whole_store | folder_id | message_id)
   and registers it under the client's `OutputHandleIndex`.
   `handle_notification_wait` (`NotificationWait` RPC, `handler.rs`) is a real
   long-poll: it pumps every sink for the session, returns
   `EventPending=1` immediately if any event is queued, otherwise blocks up to
   `NOTIFICATION_WAIT_MAX` (5 min, spec ceiling) on a probe `broadcast::Receiver`
   obtained from `subscribe_raw()`; each owner-matching probe event re-pumps the
   session sinks and returns as soon as one admits it, otherwise the deadline
   expires and it returns `EventPending=0`. The probe is subscribed BEFORE the
   initial pump so an event published in that window is not missed; a
   `RecvError::Lagged` probe immediately re-pumps (resync) rather than sleeping
   the budget, and only `RecvError::Closed` honours the remaining budget. The
   per-session `session_has_sinks(session_id)` guard short-circuits an idle
   session (a chatty neighbour must not push it into a 5-minute probe), and the
   folder scope is honoured by MAPI row id (no open `Handle::Folder` dependency,
   no whole-store widening). The transport body is
   `notification_wait_success_body`/`notification_wait_failure_body` (§2.2.4.4):
   `StatusCode(4)·ErrorCode(4)·EventPending(4)·AuxBufSize(4)`. The
   `handle_execute` ROP-chain loop prepends queued events BEFORE the client
   ROPs via `emit_pending_notifications`: each drained event encodes a
   `RopNotifyResponse` (RopId 0x2A · `NotificationHandle`(4 LE, = the
   registration's handle index) · `ReturnValue`=Success(4 LE) · `LogonId` ·
   `NotificationData` built by `build_notification_data` — flags =
   `NotificationType | 0x8000` message bit, FolderId/MessageId from
   `store::folder_id_from_backend`/`message_id_from_jmap`, and the MANDATORY
   OldFolderId/OldMessageId for ObjectMoved/ObjectCopied sentinelled `0` when
   the source id is unknown, so a move/copy `RopNotify` is never truncated); if
   MORE events remain queued after draining `MAX_NOTIFY_PER_EXECUTE`, a trailing
   `RopPendingResponse` (RopId 0x6E · SessionIndex 0) is appended so the
   client re-issues Execute to pump the rest. `RopNotifyResponse` was corrected
   from the phase-1 1-byte handle to a **4-byte** `NotificationHandle` per
   MS-OXCROPS §2.2.14.2.1. `RopRelease` unregisters the sink for the freed
   handle index; `SessionManager::remove` (used by `handle_disconnect`) +
   `sweep_idle` clear all sinks for the session so a dropped client never
   leaks a broadcast receiver. The owner match (`accepts`/probe) is canonicalised
   (case-folded + trimmed) so a divergence between the session principal email
   and the `publish_event` owner never drops a notification. The EWS
   `publish_event` path is unchanged — it now transparently feeds MAPI sessions
   too, so New Outlook's `NotificationWait` toast fires in real time on item
   CRUD instead of the old empty-immediate success that forced aggressive
   polling.
9. ~~FXICS (`fxics.rs`) bulk message/folder sync (MS-OXCFXICS)~~ — **Phase-2
   download path DONE**: the Execute dispatcher now drives the FastTransfer
   *source* ROPs (`RopFastTransferSourceCopy{Messages,Folder,To,Properties}`
   0x4B/0x4C/0x4D/0x69 + `RopFastTransferSourceGetBuffer` 0x4E). A source
   handle is installed under the client's `OutputHandleIndex` carrying the
   fully-serialised ICS byte stream built by `build_ics_stream` from the
   table's cached rows via `fxics::IcsStreamBuilder`
   (`IncrSyncChg`/`IncrSyncMessage`/`propValue`/`EndMessage`/`IncrSyncEnd`);
   `GetBuffer` serves successive chunks up to the client `BufferSize` and
   transitions `TransferStatus` to `Done` (1) when exhausted. The
   FastTransfer *destination* + `RopSynchronization*` upload ROPs
   (0x53/0x54/0x70/0x72-0x78/0x77/0x80) are wired to accumulate the upload
   stream on `Handle::FastTransferDestination` and the end-of-stream
   `RopFastTransferDestinationPutBuffer` drives `apply_fasttransfer_upload`
   which tokenises via `fxics::Tokenizer` and applies the ICS deltas to JMAP
   (`IncrSyncRead` -> batched `Email/set { keywords/$seen }`,
   `IncrSyncDel` -> `Email/destroy`, `IncrSyncMessage` -> `Email/set` patch
   / cross-folder `mailboxIds` move) — the Phase-2 #4 FXICS upload apply ->
   JMAP write-back bridge (audit gap #2), reusing the existing `JmapClient`
   primitives (no new deps; the MAPI-mid -> JMAP-id reverse map is built ONCE
   per apply via `list_email_ids_in_mailbox` + `message_id_from_jmap`). Audit
   gap §2b FXICS download closed; audit gap #2 FXICS upload apply closed.
10. **NSPI / GAL / address-book surface (audit gap §2d, PR #1845)** —
    `/mapi/nspi` is now served (was rejected as `InvalidRequestType`).
    `src/mapi/nspi.rs` owns the MS-OXNSPI/MS-OXOABK wire codecs + RPC dispatch.
    `transport.rs` carries `RpcKind::AddressBook(AddressBookRpc)` (the enum
    covers Bind/Unbind/UpdateStat/QueryRows/DnToMinId/ResolveNames/GetMatches/
    GetProps/GetPropList/GetSpecialTable/SeekEntries/QueryColumns/CompareMIds/
    ResortRestriction/ModLinkAtt/ModProps/GetTemplateInfo/GetMailboxUrl/
    GetAddressBookUrl); `parse_request` accepts address-book RPCs (verb parsed
    off the `X-RequestType`/Action header) and main.rs populates
    `MapiRequest.username`/`password` from the shared `decode_basic_auth` so the
    NSPI auth gate has the creds. `nspi::handle_address_book(rpc, req, state)`
    is the entry point dispatched from `handler::handle`. The GAL container is
    built per-RPC by `assemble_gal(state, principal)`: it consults a shared
    TTL-cached directory snapshot (`MapiState.gal_cache: Option<Arc<GalCache>>`,
    60s TTL — allocated in `MapiState::with_directory`) so a multi-RPC Outlook
    address-book handshake reuses ONE `search_blocking("*", Some(5000))`
    resolution instead of re-querying the admin API per RPC; on cache miss it
    snapshots the operator-configured directory
    (`state.directory: Option<Arc<dyn DirectoryLookup>>`, wired in `models.rs`
    via `MapiState::with_directory`) on a `spawn_blocking` task, de-dups by
    lowercased SMTP, **always includes the authenticated caller's own entry**
    (`self_entry`), and assigns 1-based Minimal Entry IDs (MId) in alphabetical
    order (stable across container restarts only as long as membership is
    stable; NOT a persisted MId — Outlook re-resolves via
    `DnToMinId`/`ResolveNames`). Each GAL entry is a `MAPI_MAILUSER` row:
    `PR_OBJECT_TYPE=MAPI_MAILUSER(6)`, `PR_DISPLAY_TYPE=DT_MAILUSER(0)` and
    `PR_DISPLAY_TYPE_EX=DT_MAILUSER(0)` — both render the same `DT_MAILUSER`
    value `entry_property` returns (no `DTE_FLAG_ACL_CAPABLE` 0x40 bit), the
    two display-type tags are `0x3900` / `0x3905` respectively per MS-OXPROPS.
    `PR_INSTANCE_KEY`/ `PR_RECORD_KEY` = packed MId, `PR_ENTRYID` synthesised as an
    `AddressBookEntryId` (the "Permanent Entry ID" for a mail user,
    MS-OXCDATA §2.6.4 — `abook_entry_id`) carrying the EntryID flags +
    AB provider UID + Version + Type + the
    `/o=Stalwart/ou=Exchange Administrative Group (FYDIBOHF23SPDLT)/cn=Recipients/cn=<localpart>`
    legacyExchangeDN that mirrors `oab::synth_dn`). STAT is the 9-DWORD
    (36-byte) MS-OXNSPI §2.2.8 struct (SortType/ContainerID/CurrentRec/
    Delta/NumPos/TotalRecs/CodePage/TemplateLocale/SortLocale); over MAPI/HTTP
    the `/mapi/nspi` Execute body carries the NSPI method call directly
    (`Flags(4) + [State(36)] + RPC fields + Aux(4+var)`) — there is NO
    `HasState(1)` selector byte (that is the EMSMDB ROP-layer framing, not the
    MAPI/HTTP NSPI body), so handlers decode Flags then [optional]_STAT then
    method fields. `Stat::query_window(count)` derives the QueryRows slice
    respecting current_rec + signed delta, capping forward counts at
    `MAX_QUERY_ROWS` and using `unsigned_abs()` for backward counts (no
    i32::MIN overflow). The rowset codec uses the
    `AddressBookFlaggedPropertyValue` (caller-supplied tags, Flag byte
    0x0=value/0x1=absent/0xA+HRESULT) shape for `QueryRows`/`ResolveNames`/
    `GetMatches`, and `…WithType` (prepends a 2-byte `PropertyType` WORD — the
    low half of the packed tag, per MS-OXCDATA §2.9) for `GetProps`. An
    unknown/unsupported tag serialises as a present cell carrying the
    `MAPI_E_NOT_FOUND` HRESULT (Flag 0x0 + 4-byte value), NEVER a zero-payload
    present cell (`PropertyValue::Null` is malformed). `CompareMIds` echoes
    `"CompareMIds"`; `ResolveNames` treats an empty needle as no-match (an
    empty `starts_with` would otherwise match the whole GAL). The PropertyTag
    wire layout is the same as everywhere else: `PropertyTag::decode` reads
    type-first then id, so the NSPI-packed u32 is `(id << 16) | type` (`pack_tag`).
    Auth gate: EVERY NSPI RPC re-uses `AuthVerifier::verify` (canonicalised
    username via `util::canonicalize_username` + the password held in
    `secrecy::SecretString` for the check only); anonymous or failed auth
    returns transport `ResponseCode::NoPrivilege` (11) **before** any directory
    I/O, so recipient PII never leaks to an unauthenticated caller (the
    empty-creds short-circuit means the gate needs no Stalwart network
    round-trip — tested by `address_book_rejects_anonymous_request`).
    `GetUserPhoto` (EWS) is **disclosure-free**: it validates the recipient
    email SYNTAX only (`is_valid_smtp_address`) and returns the spec "no photo
    published" shape (`HasChanged=false`, empty `PictureData`) for every
    syntactically-valid recipient / `ErrorInvalidSmtpAddress` for malformed.
    It deliberately does NOT consult `state.directory`: no Stalwart-native
    photo backend exists, and probing the directory would (a) let any mailbox
    user enumerate the Stalwart account set via the Success/
    ErrorNoSuchEmailAddress split (the same disclosure vector the Autodiscover
    auth gate guards), (b) misclassify every recipient as "no such address"
    when no directory is configured, and (c) silently drop `spawn_blocking`
    `JoinError`s into that error code. Outlook renders the default avatar
    rather than erroring. Remaining §2d gaps (admin-only verbs
    ModLinkAtt/ModProps are WRITE operations routed to a no-op success shape,
    and GetTemplateInfo returns a STAT rather than the full template row —
    none is exercised by Outlook's address-book/GAL flow, but they are not
    truly "closed"); an operator-supplied photo backend would slot into the
    `GetUserPhoto` syntax-only path. The NSPI read surface (Bind/QueryRows/
    DnToMinId/ResolveNames/GetMatches/GetProps/GetSpecialTable/SeekEntries/
    QueryColumns/UpdateStat/CompareMIds/ResortRestriction/GetMailboxUrl/
    GetAddressBookUrl) + a disclosure-free GetUserPhoto are wired; the
    admin-only write verbs (ModLinkAtt/ModProps/GetTemplateInfo) remain
    best-effort successes, listed here as TODOs.

11. **Misc MAPI correctness risks (audit gap §2f)** — three sub-issues closed.
    - §2f.1 **200-row contents-table truncation** — `fetch_email_rows`
      (`src/mapi/handler.rs`) previously hard-capped at `EMAIL_SYNC_PAGE_SIZE`
      (200) per folder, silently truncating large mailboxes. It now accepts
      `&Config` and pages through the JMAP `Email/query` result via a
      `position` + `limit` loop, bounded by `cfg.mapi_max_contents_rows` (env
      `GATEWAY_MAPI_MAX_CONTENTS_ROWS`, default 10_000) and driven in pages of
      `cfg.mapi_contents_page_size` (env `GATEWAY_MAPI_CONTENTS_PAGE_SIZE`,
      default 256). The two knobs are wired in `src/config.rs` via the shared
      `apply_env_usize` helper (separate `if` blocks, not array closures, so
      clippy's ` Blocks in Conditions` lint is not re-tripped). All call sites
      pass `cfg`.
    - §2f.2 **PR_CHANGE_KEY / PR_PREDECESSOR_CHANGE_LIST synthesis** —
      `store.rs::change_key_for(&JmapEmail)` now emits a proper 24-byte XID
      (`STORE_CHANGE_KEY_NAMESPACE_GUID` + an 8-byte `LocalId` derived from the
      JMAP email id folded with a digest of the mutable fields the gateway
      edits via `Email/set`: `keywords`, `mailboxIds`, `subject`, `preview`,
      `blobId` — RFC 8621 `receivedAt` is a delivery timestamp and does NOT
      change on edit, so it is deliberately NOT the revision signal) per
      MS-OXCDATA §2.12.2 / MS-OXCFXICS §2.2.2.2 rather than a short placeholder
      hash, and `predecessor_change_list_for(&JmapEmail)` builds a real
      `PredecessorChangeList` (a single seeded `SizedXid` = XidSize(1) + the
      24-byte change-key XID) per MS-OXCFXICS §2.2.2.3.
      `cell_for_email` now serves `PR_PREDECESSOR_CHANGE_LIST` (was missing);
      `PR_CHANGE_KEY` was already wired. Unit tests in `store.rs` cover the XID
      shape (`email_change_key_is_valid_xid` asserts a 24-byte XID = 16-byte
      GUID + 8-byte LocalId with a non-zero namespace GUID), that a real edit
      flips the change key (`email_change_key_differs_on_edit` flips a
      `$seen` keyword — exactly what `Email/set` does — and asserts the key
      changes, while an immutable `receivedAt` mutation does NOT), the
      predecessor list shape (`email_predecessor_change_list_is_sized_xid_list`),
      and the cells surface both (`email_cells_emit_change_key_and_predecessor_list`).
      Outlook uses these for conflict resolution on multi-device edits; missing
      change keys caused "item changed" sync errors.
    - §2f.3 **TNEF decode/encode (MS-OXTNEF)** — new module `src/mapi/tnef.rs`
      owns the full TNEF stream codec: `parse` reader (TNEFSignature
      `0x223E9F78` = wire bytes `78 9F 3E 22`, LegacyKey, TNEFVersion +
      OEMCodePage leading attrs — the reader fail-closed-rejects any
      `attTnefVersion` data other than the spec-mandated `00 00 01 00`
      (`TnefError::BadVersion`), the flat `(level, id, data, checksum)`
      attribute loop with `attFrom` TRP-structure + `dtr` date parsing +
      `attMsgProps`/`attAttachment` property-list decoding into
      `TnefMessage`/`TnefAttachment`, attachment boundary handling via
      `attAttachRendData`/`attAttachment` — `attAttachRendData` is decoded
      into the attachment's `render_position` (AttachPosition at body offset 2)
      so a parse->build round-trip preserves attachment placement), and
      `build` writer that round-trips a `TnefMessage` (subject/body/
      message-class/sender/dates + named/standard property lists + nested
      attachments), with mod-65536 checksums and bounded attribute/prop
      counts (`MAX_ATTR_DATA`/`MAX_PROP_COUNT`) and a typed `TnefError`
      (`thiserror`, no panics). Scalar property encoding follows the spec:
      `PtypBoolean` and `PtypInteger16` are 2-byte values padded to a 4-byte
      boundary (the reader consumes the pad so the next property tag stays
      aligned), `PtypNull`/`PtypUnspecified` carry 2 bytes + 2-byte pad.
      Multi-value properties strip the `0x1000` MV flag (`ptype & 0x0FFF`)
      to derive the element type and round-trip variable elements with exactly
      one per-element `UINT32` size prefix each (no double-prefix on re-encode;
      `read_one_mv_element` returns payload-only bytes). Unknown property types
      fail closed (`BadPropType`) rather than swallowing the rest of the
      property list — a real peer blob does not emit documented copies of
      unsupported types into a property list, and silently absorbing trailing
      data would desync the rest of the message. Attribute strings are
      transcoded to/from Windows-1252 (`encode_cp1252`/`decode_cp1252_byte`)
      to match the `attOemCodepage` 1252 header (so non-ASCII subjects/senders
      do not render as mojibake). Reader tolerates every documented
      message/attachment attribute the gateway does not yet model
      (`attOwner`/`attSentFor`/`attDelegate`/`attRecipTable`/service dates/
      request-res/aid-owner/render-meta-file) — never rejecting a valid stream
      over a documented attribute — so the spec-declared attribute-ID
      constants stay referenced (no `#[allow(dead_code)]`).
      `tnef_correlation_property(bytes)` builds the
      `PidTagTnefCorrelationKey` standard property (tag `0x007F`,
      `PtypBinary`) carrying the iCalendar UID (it is NOT a named property —
      `named` is `None`); `meeting_property_guid()` returns the canonical
      `PSETID_Meeting` GUID `{6ED8DA90-450B-101B-98DA-00AA003F1305}` used by
      named voting/meeting properties. The codec is wired into
      `src/smtp.rs::SmtpClient::send_imip`: iMIP replies now attach a
      `winmail.dat` (`application/ms-tnef`) TNEF part alongside the
      authoritative `text/calendar` part — carrying the encapsulated reply
      subject/body/sender + the UID-keyed correlation property + message class
      `IPM.Schedule.Meeting.Resp` — so a recipient Outlook/Exchange client
      surfaces the voting/response surface that the plain RFC-6047 iCalendar
      REPLY alone does not carry. `build_imip_tnef` is fail-soft: it always
      yields a well-formed blob (a missing/empty UID yields a key-less blob
      rather than a broken message). 17 TNEF unit tests
      (signature/checksum/dtr/round-trip/integer+binary props/named string
      prop/correlation key/truncated-reject/bad-signature-reject +
      signature+version wire bytes / non-canonical version rejection /
      boolean-scalar alignment / MV_BINARY round-trip / attachment
      render-position round-trip / CP1252 attribute round-trip /
      PSETID_Meeting GUID) + 3 smtp tests
      (`build_imip_tnef_is_parseable_and_carries_correlation_key`
      round-tripping the iMIP blob through the reader +
      `parse_addr_splits_name_and_email`) added; all green under
      `RUSTFLAGS=-D warnings`.
      Builds + tests: `RUSTFLAGS="-D warnings" cargo build --bin
      exchange_gateway`, `cargo clippy --all-targets`, `cargo test --lib`,
      `cargo test --test snapshot_tests` all clean (585 lib + 11 snapshot = 596
      tests green, 0 failures, 0 warnings). A MAPI-side `winmail.dat`
      *decoder-to-named-properties* wire (surfacing inbound TNEF attachment
      props through `RopGetAttachmentTable`/`RopOpenAttachment`) is intentionally
      NOT added: Outlook deserialises `winmail.dat` itself, so the gateway only
      needs the encode side for outbound iMIP/voting fidelity; the decode side
      is available as a library leaf for a future inbound-voting-props bridge.
