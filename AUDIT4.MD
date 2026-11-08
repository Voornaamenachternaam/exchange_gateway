# AUDIT4 — Full codebase audit for the Stalwart v1.0.3 + Outlook use-case

**Scope.** The `exchange_gateway` container must let the calendar and email of an
existing Stalwart Mailserver v1.0.3 account be used fully, natively, without
client-side extensions in:

- New Outlook for Windows (new) — build 20251205004.10 dated 7 Oct 2026
  (classic Outlook 2408 / 16.0.17932.21056 may be used temporarily to resolve
  compatibility issues),
- Outlook Android 5.2641.1, adding the account via Android Settings as an
  “Exchange” account,

with the clients talking only to the gateway and Stalwart as backend only.
JMAP (as implemented by Stalwart v1.0.3) must be preferred wherever feasible.
Backwards compatibility and EAS < 16.1 are not required, except where the
specific use-case needs them. The deployment scenario is: Stalwart container
already running; operator creates `/armblock/exchange-gateway-data`, fills the
inline `docker-compose.yml` (no env file), and deploys via Portainer by pulling
the image from a registry. Building the image is out of scope except for
`Dockerfile` and `docker-compose.yml` in this repo. The gateway should serve
HTTP because `cloudflared` on the Ubuntu host terminates TLS.

**Placeholder convention.** Every `example.com` hostname in the
deployment templates and docs (`docker-compose.yml`,
`cloudflared/config.yml`, `CLOUDFLARED_SETUP.md`, `DEPLOY_RUNBOOK.md`,
and this file) is a **placeholder** for the operator's real domain and
must be replaced in the operator's own copy before deployment; the
repository templates keep the placeholders on purpose and must never be
deployed as-is:

- `example.com` = the operator's real root / mail domain (the domain part
  of the users' email addresses; `GATEWAY_MAIL_DOMAIN`),
- `mail.example.com` = the operator's real mailserver hostname / MX
  record (`GATEWAY_MAIL_HOST`),
- `calendar.example.com` = `calendar.` + the real domain (`GATEWAY_HOST`),
- `autodiscover.example.com` = `autodiscover.` + the real domain (the
  host Android's "Exchange" wizard probes first),
- `stalwart.example.com` = `stalwart.` + the real domain (Stalwart web UI).

After replacement: `GATEWAY_HOST` must equal `calendar.` + the mail
domain, the Autodiscover host must equal `autodiscover.` + the mail
domain, and `GATEWAY_MAIL_HOST` must equal the real MX hostname. No tunnel
route is ever created for the bare root domain (it already has A/AAAA
records); only the `calendar.` and `autodiscover.` routes to port 8134 are
needed. No `.env` file is used in this deployment.

**Method.** Every finding below was re-derived from the source in
`/workspace/project/exchange_gateway` (127 690 lines across `src/`), the
`exchange_protocols/` specs (v20250520), `docker-compose.yml`, `Dockerfile`,
`CLOUDFLARED_SETUP.md`, `DEPLOY_RUNBOOK.md`, `cloudflared/config.yml`, and
`AUDIT3.MD`. No finding relies on prior `AUDIT.md` prose. Line numbers are
from the current `main` HEAD. Docker daemon was available (`docker 29.9.3`)
but no Stalwart container / `/armblock` exists in this sandbox, so live
Stalwart probes were not possible; Stalwart v1.0.3 contracts are cited from
`AUDIT3.MD`’s live-probe evidence and from the gateway’s own stubbed tests.

**Priority ordering** is by user-visible impact on the stated use-case.
For each gap the fix-priority ladder is applied:

1. existing dependencies,
2. new high-quality dependencies,
3. Rust 1.99.1 std API,
4. custom code.

The list is numbered top-to-bottom in priority order. Items marked
**DELIVERED** are already fixed in the current tree and are listed for
completeness so the operator does not re-investigate them; the remainder are
the outstanding issues that must be resolved/implemented to realise the
use-case.

---

## How to read this audit

- **P0** — blocks the use-case (a client cannot provision, sync, send, or
  persist data, or data is silently lost).
- **P1** — degrades the use-case (a surface is unreachable, inconsistent, or
  will fail under the stated client versions / Cloudflare topology).
- **P2** — robustness / friction (deployment, operability, docs, or
  correctness under edge conditions).

Each item gives: *Evidence* (file:line), *Impact* on the two target clients,
and *Fix* with the ladder step. Where the fix is a compose/env change, the
exact value to set is given. Section 12 is the operator’s change list for
Cloudflare Zero-Trust and for the Stalwart container; Section 13 is the
compose env-var inventory.

---

## P0 — Blocks the use-case (items 1–10)

Items 11–14 are P1/P2 (see below) and are placed after the P0 block so
each item’s section matches its severity in the summary table.

### 1. The Cloudflare ingress hostnames and the Autodiscover contract — OPEN until the startup pairing check exists (the placeholder convention alone is not enforced)

**Evidence.**

- Operator's existing Zero-Trust routes (per the task): `calendar.example.com`
  -> port 8134, `autodiscover.example.com` -> port 8134. In this file
  `example.com` is the placeholder for the operator's real root / mail
  domain, so these are `calendar.` + the real domain and `autodiscover.` +
  the real domain.
- `docker-compose.yml` ships `GATEWAY_HOST=calendar.example.com` and
  `GATEWAY_MAIL_DOMAIN=example.com`; `CLOUDFLARED_SETUP.md`,
  `cloudflared/config.yml`, and `DEPLOY_RUNBOOK.md` use the same
  `example.com` placeholders throughout. The operator replaces every
  `example.com` with the real domain in their own copy before deploying.
  The invariants that must hold after replacement are:
  `GATEWAY_HOST == calendar.` + `GATEWAY_MAIL_DOMAIN`, and
  `autodiscover.` + `GATEWAY_MAIL_DOMAIN` is the Autodiscover host.
- `src/autodiscover.rs` documents the contract: Android probes
  `https://autodiscover.example.com/autodiscover/autodiscover.xml`
  (placeholder for `autodiscover.` + the real mail domain) where the mail
  domain is the domain part of the typed email address
  (`GATEWAY_MAIL_DOMAIN`), and the mobilesync response points back at
  `https://{GATEWAY_HOST}/Microsoft-Server-ActiveSync`.
  `src/main.rs` renders every Autodiscover URL from the single
  `gateway_host` config value; there is no per-protocol host override.

**Impact.** With the placeholder convention above, the two existing
Zero-Trust routes (`calendar.` and `autodiscover.` to port 8134) are
sufficient and correct for both pinned clients: discovery is derived from
the mailbox domain, the `autodiscover.` route answers the primary probe,
and the response points at `calendar.` (`GATEWAY_HOST`). No route for the
bare root domain is needed or possible - it already has A/AAAA records -
and none is documented as required anywhere in the rewritten docs. This
item is **OPEN, not resolved**: the placeholder convention is a
documentation rule that nothing enforces, and the startup pairing check
in the Fix below does not exist yet, so a left-in placeholder still fails
silently in the Android wizard.

**Fix (ladder 1 - existing code, docs-only).**

1. Keep `GATEWAY_HOST=calendar.example.com` / `GATEWAY_MAIL_DOMAIN=example.com`
   as placeholders in the repo template, with an explicit placeholder
   convention at the top of `docker-compose.yml`, `CLOUDFLARED_SETUP.md`,
   and `DEPLOY_RUNBOOK.md` stating that `example.com` stands for the real
   root / mail domain and must be replaced before deployment.
2. `CLOUDFLARED_SETUP.md` and `DEPLOY_RUNBOOK.md` document exactly two
   public hostnames (`calendar.` and `autodiscover.`); there is no
   "Hostname C" root-domain route and no root CNAME.
3. `cloudflared/config.yml` carries exactly the two ingress entries
   (autodiscover + gateway host) and an explicit "do not add a root-domain
   ingress entry" comment.
4. A startup check in `src/main.rs` (extending the existing
   `is_placeholder_host` warning) warns if `gateway_host` is not
   `calendar.` + the mail domain or if the mail domain is empty, so a
   left-in placeholder is loud at deploy time rather than silent in the
   Android wizard.

> **Scope note - per-mail-domain discovery.** The pairing
> (`calendar.` + the mail domain, `autodiscover.` + the mail domain) is
> correct for the single `GATEWAY_MAIL_DOMAIN` this deployment serves:
> every mailbox whose domain equals `GATEWAY_MAIL_DOMAIN` is discovered
> by the `autodiscover.` route, and the mobilesync response points at
> `https://{GATEWAY_HOST}/Microsoft-Server-ActiveSync` for *any* mailbox
> domain. That response rule is *not* a substitute for per-domain
> discovery: Android probes `autodiscover.` + the entered email domain,
> so a second mailbox domain needs its own public `autodiscover.` alias
> (plus its own Autodiscover handling, because Autodiscover otherwise
> returns Stalwart IMAP/SMTP protocol blocks advertising
> `GATEWAY_MAIL_HOST`) - otherwise the client pairs the Stalwart
> IMAP/SMTP blocks with the gateway's `mapiHttp`/`mobilesync` blocks and
> fails. Multi-domain support is a separate configuration concern and is
> not claimed here.

---

### 2. `GATEWAY_MAIL_HOST` and the public MX hostname — OPEN until the `mail_host` startup check exists (the placeholder convention alone is not enforced)

**Evidence.** `docker-compose.yml` ships
`GATEWAY_MAIL_HOST=mail.example.com`. `src/config.rs` derives `mail_host`
as `mail.` + the mail domain only when the env var is empty; when set, the
value is used verbatim. `src/autodiscover.rs` renders
`<Server>{mail_host}</Server>` in the IMAP (993) and SMTP (465) protocol
blocks whenever `include_imap_smtp` is true.

**Impact.** `mail.example.com` is the placeholder for the operator's real
mailserver hostname / MX record. An operator who copies the compose and
only edits `GATEWAY_HOST` / `GATEWAY_MAIL_DOMAIN` will advertise the
placeholder - an unresolvable host - to any client that reads the
IMAP/SMTP blocks. The placeholder convention documents that the values
must be replaced in the operator's copy, but it enforces nothing: this
item stays **OPEN** until the `mail_host` startup check in the Fix
exists (today only `gateway_host` is checked, `src/main.rs`).

**Fix (ladder 1).** The compose comment at `GATEWAY_MAIL_HOST` states
that `mail.example.com` is a placeholder for the real MX hostname and
must match the MX record. A startup warning in `src/main.rs` when
`is_placeholder_host(&config.mail_host)` is true (mirroring the existing
`gateway_host` warning) makes a left-in placeholder loud at deploy time.

---

### 3. The gateway serves plain HTTP only — there is no TLS listener, and the `x-forwarded-proto` enforcement is inconsistent across surfaces

**Evidence.**

- `src/main.rs:959-967` (`serve_plain`): `TcpListener::bind(addr)` +
  `axum::serve(listener, app)` — plain HTTP. There is no `rustls`,
  `tokio-rustls`, or `axum-server` listener anywhere in `Cargo.toml`;
  `rustls` appears only as a *client* dependency (reqwest, lettre, sqlx).
- `src/ews.rs:293-298` and `src/eas.rs:7326-7327` reject requests whose
  `x-forwarded-proto` header is present and not `https` (400). When the
  header is absent, the request passes (`src/ews.rs:483-488`,
  `src/eas.rs:1324-1330` — “Missing x-forwarded-proto must pass (direct
  HTTP access)”).
- Cloudflare Tunnel (`cloudflared`) forwards to `http://localhost:8134`
  (`cloudflared/config.yml:62,77,90`) and sets `CF-RAY` (used at
  `src/eas.rs:1316` for the Ping clamp) but does *not* reliably set
  `x-forwarded-proto` to `https` on every ingress path; the gateway must
  not depend on it.

**Impact.** The task says “if possible/feasible the exchange_gateway-container
should ‘serve’ http” because `cloudflared` terminates TLS. The current
posture is correct for the tunnel, but two gaps remain:

1. A client that reaches the gateway *directly* (port 8134 published in
   compose at `docker-compose.yml:170`) over plain HTTP gets a full
   authenticated session — credentials in cleartext — with no warning.
   The `x-forwarded-proto` check only fires when the header is present;
   a direct HTTP request omits it and passes.
2. If the operator later puts a TLS-terminating reverse proxy in front
   (not cloudflared), the `x-forwarded-proto` check may reject legitimate
   traffic or accept illegitimate traffic depending on the proxy’s header
   behaviour, with no config switch to control it.

**Fix (ladder 2 → 3).**

- Ladder 2 (preferred): add `tokio-rustls` + `rustls` as *server* deps and
  a `--tls` / `GATEWAY_TLS_CERT` / `GATEWAY_TLS_KEY` config path that
  serves HTTPS on the same port when certs are present, falling back to
  plain HTTP otherwise. This keeps the cloudflared HTTP path working while
  giving a direct-HTTPS option. (Custom code, ladder 4, is the fallback
  if the dep is rejected.)
- Ladder 1 (immediate, no new dep): in `src/main.rs`, log a startup warning
  when the bind address is not loopback and no TLS is configured; and in
  `src/validation.rs::validate_request`, reject `x-forwarded-proto: http`
  *and* warn when the header is absent on a non-loopback bind. Do not
  change the EAS/EWS `forwarded_https_enforced` semantics without a test —
  the current “absent = pass” is load-bearing for the tunnel.

---

### 4. The compose publishes port 8134 to the host, which bypasses Cloudflare Tunnel and exposes plain HTTP to the LAN/WAN

**Evidence.** `docker-compose.yml:169-170`:
`ports: - 8134:8134` (wildcard bind, i.e. all host interfaces).
`CLOUDFLARED_SETUP.md:346` repeats it. The tunnel ingress targets
`http://localhost:8134` on the *host* (`cloudflared/config.yml:62,77,90`:
`service: http://localhost:8134`). `cloudflared` runs on the Ubuntu host
and makes an outbound tunnel connection; it never routes through the
Docker bridge `my-bridge-network`.

**Impact.** The wildcard publish exposes plain HTTP + Basic auth on the
host’s 8134 to the LAN/WAN, bypassing Cloudflare edge access control.
The publish itself is *not* optional for the stated topology: it is the
only host-local path `cloudflared` has to the container on 8134.

**Fix (ladder 1 — compose only).** Retain the publish but restrict it to
loopback: `ports: - 127.0.0.1:8134:8134`. This keeps
`http://localhost:8134` reachable for `cloudflared` (and for host-local
`curl` health checks) while closing the LAN/WAN exposure. Do *not* remove
the `ports:` block outright (that would break the host tunnel routes),
and do not add an alternative tunnel route to the container’s bridge
address — host networking and bridge networking are different networks;
the tunnel’s `localhost` target resolves on the host, not in the bridge.
Keep `GATEWAY_BIND=[::]:8134` (the in-container bind). If the operator
needs external direct access, they can re-add an explicit wildcard publish.

---

### 5. EAS command surface is incomplete for the [MS-ASCMD] §2.2.1 command set — `FolderCreate`, `FolderDelete`, `FolderUpdate`, `GetAttachment`, `GetHierarchy`, and `Find` return “Status 5” (unsupported)

**Evidence.** `src/eas.rs:1456` advertises
`MS-ASProtocolCommands: Sync,FolderSync,Provision,MeetingResponse,Settings,Ping,ItemOperations,Search,ResolveRecipients,GetItemEstimate,ValidateCert,SendMail,SmartReply,SmartForward`.
`src/eas.rs:7433-7610` dispatches exactly those; any other `Cmd` hits
`unsupported_command_response` (`src/eas.rs:1590-1602`) which returns
`<Status>5</Status>`. The [MS-ASCMD] §2.2.1 command list (from the spec TOC
at `exchange_protocols/[MS-ASCMD].txt:206-236`) also includes `Find`
(§2.2.1.2), `FolderCreate` (§2.2.1.3), `FolderDelete` (§2.2.1.4),
`FolderUpdate` (§2.2.1.6), `GetAttachment` (§2.2.1.7), and `GetHierarchy`
(§2.2.1.8). None of those strings appear in `src/eas.rs` dispatch.

**Impact.** No client-trace evidence has been captured showing either
pinned client issues `GetHierarchy` for 14.0–16.1 during provisioning;
FolderSync is the hierarchy source for those versions and is implemented.
`GetHierarchy` support is required for 2.5/12.0/12.1 only
(`[MS-ASCMD] §2.2.1.8` protocol table: 14.0–16.1 leave the cell blank and
state “the FolderSync command is used to retrieve email folders”). The
P0 gap is therefore **not** `GetHierarchy` on the pinned versions — it is
`FolderCreate`/`FolderDelete`/`FolderUpdate`, which hide folder-management
breakage behind the misleading `MS-ASProtocolCommands` advertisement, plus
`Find` routing for mailbox/GAL search. Returning Status 5 is *correct* per
spec for an unsupported command; the defect is that the advertised command
list (`MoveItems` is dispatched at `src/eas.rs:7586` but absent from the
advertised `MS-ASProtocolCommands` string) does not match the dispatch
table, so clients may assume folder/attachment flows exist when they do
not. `GetAttachment` (2.5/12.x legacy, reached via `ItemOperations` Fetch on
14.0+) is a lower-priority gap.

**Fix (ladder 1 → 4).**

1. `GetHierarchy` (ladder 1): implement it for the 2.5/12.0/12.1 contract
   only; do not treat its absence on 14.0–16.1 as a provisioning blocker.
2. `Find` (ladder 1): `Search` is already implemented (`handle_search`,
   `src/eas.rs:4764`) and dispatches GAL vs Mailbox on the Store name
   (`handle_gal_search`, `src/eas.rs:4885`). `Find` ([MS-ASCMD] §2.2.1.2)
   has its own request/response shape (`<Find>` with
   `GALSearchCriterion`/`MailBoxSearchCriterion`, `<ExecuteSearch>`,
   `<Response>/<Result>` carrying `airsync:Class`/`airsync:CollectionId`,
   and per-result `email:*` vs `gal:*` property blocks). Reusing the
   `Search` envelopes verbatim or always routing to the GAL path is
   wrong: dispatch each Find form — GAL criteria to the GAL path
   (§4.13 equivalent), mailbox criteria to the mailbox path — and render
   the Find response shape each form requires.
3. `FolderCreate/Delete/Update` (ladder 4 — custom): these need a
   JMAP `Mailbox/set` (create/destroy/update) for email folders and a
   `Calendar/set` / `CalendarEvent` path for calendar folders. The
   existing `JmapClient` already does `Email/set` for mutations
   (`src/email.rs`), so this is a new handler, not a new backend.
4. `GetAttachment` (ladder 1): `handle_email_attachment_fetch`
   (`src/eas.rs:3140`) already serves the byte range; expose it under
   `GetAttachment` as well.
5. Update `MS-ASProtocolCommands` (`src/eas.rs:1456`) to advertise exactly
   what is dispatched (including `MoveItems`) and nothing that is not, so
   clients do not probe unimplemented commands.

---

### 6. New Outlook for Windows does not use EAS for third-party accounts — the gateway’s EAS surface is irrelevant to it, and the MAPI/HTTP surface is the real New Outlook path

**Evidence.** `src/eas.rs:6632-6640` (comment):
“New Outlook for Windows, by contrast, is not an EAS client at all for
third-party accounts — Microsoft documents no EAS account type and no
on-premises Exchange support for it — so it never reaches this path.”
`src/config.rs:230-232,420-421`: “New Outlook for Windows (20251205004.10)
and Outlook Android connect over MAPI/HTTP, not EWS.” The MAPI/HTTP surface
is gated by `GATEWAY_MAPI_ENABLED` (default 1, `src/config.rs:420`) and
advertised in Autodiscover via the `mapiHttp` `<Protocol>` block
(`src/autodiscover.rs:783-791`, `src/main.rs:206-215`).

**Impact.** The task requires *both* clients to work. For New Outlook for
Windows, the **MAPI/HTTP path is unproven**: `DEPLOY_RUNBOOK.md` §5 states
the Autodiscover/mobilsync flow, but no trace, test, or client session
proves New Outlook for Windows (20251205004.10) completes provisioning
against a third-party (Stalwart-backed) account through this gateway. The
codebase comment itself records that Microsoft documents no on-premises
Exchange support for New Outlook (`src/eas.rs:6632-6640`), so whether the
`mapiHttp` route works at all is an open use-case question, not a settled
one. The `mapiHttp` block being advertised does not prove the client
accepts it. Record this as a **use-case blocker**: validate the exact
New Outlook provisioning flow (Autodiscover JSON → `mapiHttp` block →
`/mapi/emsmdb` + `/mapi/nspi` → `RopLogon` round-trip) against the pinned
build before claiming the MAPI path works or that items 1–13 suffice. The
EAS work (WBXML, Sync, Ping) serves Outlook Android’s “Exchange” wizard
and the native Android account. The MAPI/HTTP surface is 11 065 lines
(`src/mapi/handler.rs`) plus 6 016 lines (`src/mapi/rops.rs`) — it is the
largest single risk area for New Outlook and is *not* covered by the EAS
conformance tests.

**Fix (ladder 1).** Add a New Outlook provisioning checklist to
`DEPLOY_RUNBOOK.md` §5 that exercises: Autodiscover JSON v1.0 path form
(`src/main.rs:349`), `mapiHttp` block present, and a `RopLogon`
round-trip (the smoke script `tests/outlook_cloudflare_smoke.sh` already
checks Autodiscover JSON but does not check MAPI). Extend the smoke
script with a MAPI `emsmdb`/`nspi` probe that sends valid **POST**
requests (the registered routes are POST-only, `src/main.rs:844-856`);
OPTIONS probes are not a valid check of those routes.

---

### 7. Outlook Android 5.2641.1 vs the validated 5.2634.2 — the gateway’s option matrices pin the older build

**Evidence.** `AGENTS.md:7` and `AUDIT3.MD:5-6` pin Outlook Android
5.2634.2. The task pins 5.2641.1. `src/eas_sync_options.rs:950` pins “New
Outlook for Windows (20251205004.10) Sync prime: 16.1” and the test
`parse_outlook_android_option_matrix` pins the Android option matrix for
5.2634.2. There is no matrix for 5.2641.1.

**Impact.** A client version bump can change `<Options>` negotiation
(`BodyPreference`, `MIMETruncation`, `ConversationMode`). If 5.2641.1
sends a new option combination, the gateway falls back to defaults
(`AllOrNone` fall-through in `eas_sync_options.rs`), which is safe but may
truncate bodies differently.

**Fix (ladder 1).** Re-run the Android option-matrix capture against
5.2641.1 and add a `parse_outlook_android_5_2641_option_matrix` test;
update `AGENTS.md` and `AUDIT3.MD` headers to the new pinned version.

---

### 8. The “Exchange” account on Android is provisioned via Microsoft’s cloud sync connector, not directly — credentials transit Microsoft

**Evidence.** `src/eas.rs:6632-6636`: “Outlook Android adds third-party
accounts through Microsoft's cloud sync connector, and Android's own
Exchange account wizard talks EAS directly”. `DEPLOY_RUNBOOK.md:105-108`
documents the same. The task’s use-case explicitly says “adding the
Stalwart Mailserver-account via android settings as an ‘Exchange’-account” —
that is the *native Android* wizard, which talks EAS directly to the
gateway. Whether credentials “transit Microsoft” on the cloud path is a
client claim not verified in this repository; the audit does not assert
credential-transit, only that the cloud-connector path introduces a
shared-egress-IP and cloud dependency the native wizard avoids.

**Impact.** If the operator tests with Outlook Android (the app) instead
of Android Settings → Accounts → Exchange, the account is provisioned via
Microsoft’s cloud, the gateway sees Microsoft’s egress IP, and the
`CF-Connecting-IP` rate limiter (`src/rate_limit.rs:19-22`) keys on that
IP — the 600 req/min limit (`docker-compose.yml:79`) may throttle. The
native Android wizard avoids this.

**Fix (ladder 1 — docs).** Add an explicit callout in `DEPLOY_RUNBOOK.md`
§4: *use Android Settings → Accounts → Add account → Exchange, not the
Outlook Android app, if you want direct EAS to the gateway; the Outlook
Android app provisions via Microsoft’s cloud sync and credentials transit
Microsoft.*

---

### 9. `GATEWAY_ADMIN_USERNAME` / `GATEWAY_ADMIN_PASSWORD` ship empty, which disables the full GAL and the deploy-time JMAP capability check

**Evidence.** `docker-compose.yml:160-161` ships both empty.
`src/main.rs:1197` reads `GATEWAY_CALENDAR_CAPABILITY_CHECK`;
`src/main.rs:1190-1262` (`verify_calendar_capability_at_startup`) skips
the check with a warning when admin creds are empty. `src/directory.rs`
falls back to a self-only minimal GAL stub when no admin account is
configured. The compose comment at `docker-compose.yml:156-159` says
“Leave empty to fall back to the self-only minimal GAL stub.”

**Impact.** The GAL (NSPI + OAB) will contain only the authenticated user.
New Outlook’s recipient resolution (`ResolveNames`, `FindPeople`,
`GetPersona`) will return empty for other users, and the OAB download
(`src/oab.rs`) will be minimal. The deploy-time JMAP calendar write probe
(`probe_write_paths`, `src/jmap.rs`) will not run, so a lossy JMAP write
path is only caught at runtime by the read-back guard
(`verify_write_round_trip`). That guard is correct (AUDIT3 §2 DELIVERED),
but the startup signal is lost.

**Fix (ladder 1).** Document in the compose that the operator should set
a Stalwart admin account (with `sysAccount*` / `sysMailingList*`
permissions, per `src/config.rs:188-193`) if they want the full GAL, and
that empty is a deliberate self-only fallback. Add a startup log line when
admin creds are empty stating the GAL is self-only.

---

### 10. Stalwart version drift: the repo validates against v1.0.1 / v1.0.3, the operator runs v1.0.3, and the compose pins `:latest`

**Evidence.** `AGENTS.md:5` says v1.0.1. `AUDIT3.MD:282` cites v1.0.3 live
probes. `DEPLOY_RUNBOOK.md:19,25` pins `stalwartlabs/stalwart:v1.0.1@sha256:…`.
The operator’s Stalwart compose (in the task) uses
`image: stalwartlabs/stalwart:latest`. The gateway’s compose
(`docker-compose.yml:6`) uses `<registry>/exchange-gateway:0.1.0`.

**Impact.** `:latest` will drift past the validated version. The JMAP
contracts that AUDIT3 fixed (JSCalendar object vs string, partial-patch
`CalendarEvent/set`, empty capability object `{}`) are version-sensitive.
A future Stalwart release that changes any of them will silently break
the gateway.

**Fix (ladder 1 — operator action, documented).** Pin Stalwart in *its*
compose to `stalwartlabs/stalwart:v1.0.3@sha256:<digest>` (the operator
must supply the digest; the task says do not replace placeholders, so this
is an operator step, not a repo edit). Update `DEPLOY_RUNBOOK.md:19` and
`AGENTS.md:5` to v1.0.3. The compose comment at `docker-compose.yml:6`
already says to pin the gateway image; add the same guidance for Stalwart.

---

## P1 — Degrades the use-case (items 11–13, 15–25)

### 11. The gateway’s `GATEWAY_JMAP_BASE` / `GATEWAY_CALDAV_BASE` / `GATEWAY_CARDDAV_BASE` point at `stalwart:8080`, but the operator’s Stalwart exposes 8080 *and* 8133 — the gateway must use the bridge-network service name, not the host port

**Evidence.** Operator’s Stalwart compose publishes `8080:8080` and
`8133:8133`. `docker-compose.yml:32-36` sets
`GATEWAY_CALDAV_BASE=http://stalwart:8080/dav`,
`GATEWAY_JMAP_BASE=http://stalwart:8080/jmap`,
`GATEWAY_CARDDAV_BASE=http://stalwart:8080/dav/card`. Both containers are
on `my-bridge-network` (`docker-compose.yml:183-184,212-213`; Stalwart
compose `networks: - network1`).

**Impact.** `stalwart:8080` resolves via Docker DNS on the bridge network
to the Stalwart container’s 8080, which is correct. The 8133 port is
Stalwart’s *other* HTTP listener (JMAP/CalDAV on a different path in some
Stalwart builds) and must not be used. The current values are correct, but
there is no comment explaining why 8080 and not 8133, and a future editor
might “fix” them to 8133.

**Fix (ladder 1 — docs).** Add a compose comment: *the bridge-network
service name `stalwart` and port 8080 are the JMAP/CalDAV/CardDAV
listener; 8133 is Stalwart’s alternate listener and must not be used.*

---

### 12. No `env_file` / `.env` in the repo (correct), but `cloudflared/config.yml` still references “the gateway’s .env file”

**Evidence.** `ls .env*` → no such file (correct per the task).
`docker-compose.yml` has no `env_file:` (correct). `cloudflared/config.yml:28,36`
still says “One-liner using the gateway’s .env file” and “There is no .env
file: read the two hostnames from docker-compose.yml” — the comment is
self-contradictory and references a file that must not exist.

**Impact.** An operator following the one-liner will look for a `.env`
that the task forbids, or will create one.

**Fix (ladder 1).** Edit `cloudflared/config.yml:28-36` to remove the
`.env` reference and keep only the “read from docker-compose.yml” path.

---

### 13. Dockerfile healthcheck and compose healthcheck both use `/health/ready`, which requires the Stalwart backend — a Stalwart outage marks the gateway unhealthy but does not restart it

**Evidence.** `Dockerfile:76-77` and `docker-compose.yml:191` both
`curl -f http://localhost:8134/health/ready`. `src/main.rs:1009-1107`
(`health_check`) checks SQLite + JMAP (sole arbiter when configured) +
CalDAV. `docker-compose.yml:197-209` documents the optional `autoheal`
service. `restart: unless-stopped` does not restart unhealthy containers.

**Impact.** If Stalwart restarts, the gateway becomes unhealthy and stays
unhealthy until Stalwart is back; Docker will not restart it. This is
*correct* per AUDIT3 §20 (readiness vs liveness), but the operator must
understand that `docker compose ps` showing “unhealthy” after a Stalwart
restart is expected and will clear when Stalwart is back — no action
needed.

**Fix (ladder 1 — docs).** Add a `DEPLOY_RUNBOOK.md` §7 row: “Gateway
unhealthy after Stalwart restart → wait for Stalwart; do not restart the
gateway manually.” Keep `/health/live` for process liveness.

---

### 14. The gateway image is built from `rust:1.99.1-bookworm` but the runtime is `debian:bookworm-slim` — no `ca-certificates` issue, but the binary is dynamically linked

**Evidence.** `Dockerfile:5` (builder), `Dockerfile:32` (runtime),
`Dockerfile:35-40` (runtime deps: `ca-certificates`, `tzdata`,
`libsqlite3-0`, `curl`). `Dockerfile:29` builds with
`cargo build --release --locked --bin exchange_gateway`.

**Impact.** `libsqlite3-0` is required because sqlx links SQLite
dynamically. If a future Rust toolchain changes the glibc version, the
binary may fail to start in the slim runtime. This is a minor robustness
risk, not a current failure.

**Fix (ladder 1).** Add a `ldd /usr/local/bin/exchange_gateway` smoke
check to the image publish pipeline, or switch to a static musl target
(ladder 2 — new dep `tikv-jemallocator` is not needed; the change is the
`--target x86_64-unknown-linux-musl` flag, ladder 3). Not blocking.

---

### 15. EWS `GetStreamingEvents` holds a connection for up to 30 minutes, which exceeds the Cloudflare 125s Proxy Read Timeout

**Evidence.** `src/ews.rs:6432-6600` (`handle_get_streaming_events`):
`ConnectionTimeout` clamped to 1..=30 minutes (`src/ews.rs:6460-6464`),
`deadline_total = timeout_minutes * 60`, turn timeout 10s.
`src/config.rs:62-65` documents the 125s Cloudflare cut for EAS Ping.
`docker-compose.yml:65-72` sets `GATEWAY_MAX_PING_HEARTBEAT=80` for Ping.

**Impact.** New Outlook for Windows uses EWS `Subscribe` /
`GetStreamingEvents` for calendar push. Whether the connection is cut at
the Cloudflare Proxy Read Timeout (HTTP 524) depends on zone
response-body-buffering and inspection settings, which are not observable
from this repository: the missing streaming headers below are a risk
factor, not proof that Cloudflare buffers this response. The handler
already emits a per-turn `StatusEvent` keep-alive (10s turn) and a
terminal `Closed` fragment, but `handle_ews` returns a `ReceiverStream`
body with no `Cache-Control: no-transform`/`X-Accel-Buffering: no`
streaming headers, so intermediary buffering between the tunneled origin
and the client *can* hold the keep-alives until a 524 reset. Verify with
an edge test (or a buffering rule) before asserting the 524 behavior. If
buffering occurs, the client will reconnect, but push latency degrades to
“reconnect every 2 minutes”.

**Fix (ladder 1).** Clamp `ConnectionTimeout` for Cloudflare-proxied
requests (detect `CF-RAY`, as Ping does at `src/eas.rs:1316`) to ≤80s,
mirroring `GATEWAY_MAX_PING_HEARTBEAT`. Add a config knob
`GATEWAY_MAX_STREAMING_TIMEOUT_SECS` (default 80 when Cloudflare-proxied).

---

### 16. Rate limiter keys on `CF-Connecting-IP`, but Microsoft’s cloud sync egress shares IPs across many users

**Evidence.** `src/rate_limit.rs:19-22`: “cloudflared tunnel, and
Cloudflare overwrites `CF-Connecting-IP`”. `docker-compose.yml:79-80`:
`GATEWAY_RATE_LIMIT_REQUESTS_PER_MINUTE=600`,
`GATEWAY_RATE_LIMIT_MAX_CONCURRENT=2000`.

**Impact.** If Outlook Android provisions via Microsoft’s cloud (item 8),
many users share one egress IP and the 600/min limit throttles them.
If the operator uses the native Android wizard (item 8), each device has
its own IP and the limit is fine.

**Fix (ladder 1).** Document the interaction in the compose comment at
`docker-compose.yml:74-77` and in `DEPLOY_RUNBOOK.md` §7. No code change.

---

### 17. `GATEWAY_ALLOW_INSECURE_HTTP=1` is required for the bridge-network HTTP backend, but it also disables the HTTPS check for *all* JMAP URLs

**Evidence.** `docker-compose.yml:106` sets `GATEWAY_ALLOW_INSECURE_HTTP=1`.
`src/config.rs:1278-1298` (`validate_jmap_url`): when `allow_insecure_http`
is true, `http://` is accepted for `jmap_base`, `caldav_base`, and
`carddav_base`. The comment at `docker-compose.yml:104-106` says “Plain
HTTP to Stalwart only over the private bridge network”.

**Impact.** The flag is correct for the stated topology, but it is a
global switch: a typo that points `GATEWAY_JMAP_BASE` at a public host
would send Basic credentials in cleartext. The startup log
(`src/config.rs:530-550`) logs the sanitized base URLs, so the typo is
visible, but not blocked.

**Fix (ladder 1).** Add a startup warning when `allow_insecure_http` is
true and any base URL’s host is not `stalwart`, `localhost`, or a
RFC1918 address. No behaviour change.

---

### 18. IMAP is advertised in Autodiscover but there is no IMAP client in the gateway — `GATEWAY_IMAP_HOST`/`GATEWAY_IMAP_PORT` are dead config

**Evidence.** `docker-compose.yml:95-96` sets `GATEWAY_IMAP_HOST=stalwart`,
`GATEWAY_IMAP_PORT=993`. `src/config.rs:42-43,139,949` reads them.
`src/autodiscover.rs:801-823` advertises IMAP (993) in the `<Protocol>`
blocks when `include_imap_smtp` is true. `src/main.rs:206` sets
`include_imap_smtp: state.smtp_client.is_some()` — i.e. IMAP is advertised
whenever SMTP is configured, regardless of `GATEWAY_IMAP_HOST`.
`grep -n 'imap_host\|imap_client' src/*.rs` shows no IMAP client is ever
constructed; `src/models.rs` builds only `SmtpClient` and `JmapClient`.

**Impact.** The IMAP block points clients at `mail.example.com:993` (placeholder for the real MX hostname), which
is Stalwart’s IMAP listener (published in the operator’s Stalwart
compose as `993:993`). That is actually correct — the client connects
*directly* to Stalwart IMAP, bypassing the gateway. But the gateway’s
`GATEWAY_IMAP_HOST`/`GATEWAY_IMAP_PORT` are misleading: they suggest the
gateway proxies IMAP, which it does not. If the operator expects the
gateway to mediate IMAP, they will be confused.

**Fix (ladder 1).** Rename the compose comment to state that IMAP/SMTP
in Autodiscover point directly at Stalwart (the gateway is not in the
IMAP path), and that `GATEWAY_IMAP_HOST`/`GATEWAY_IMAP_PORT` are only
used to decide whether to advertise the blocks. Or remove the IMAP block
from Autodiscover if the use-case does not need it (the task’s clients
are Exchange/EAS/MAPI, not IMAP).

---

### 19. SMTP submission uses `lettre` with implicit TLS on 465, but the compose sets `GATEWAY_SMTP_HOST=stalwart` (bridge name) — TLS verification will fail

**Evidence.** `docker-compose.yml:93-94`:
`GATEWAY_SMTP_HOST=stalwart`, `GATEWAY_SMTP_PORT=465`.
`src/smtp.rs:100-113` (`build_transport`): port 465 → `relay()` (implicit
TLS). `lettre`’s `relay()` verifies the server certificate against the
hostname (`stalwart`). Stalwart’s 465 listener presents a certificate
for `mail.example.com` (placeholder for the real MX hostname), not for `stalwart`, so verification fails.

**Impact.** Simple text/html `SendMail`/`SmartReply`/`SmartForward` (EAS)
and EWS `SendItem` can use the JMAP `EmailSubmission/set` path
(`src/email.rs:1234-1276`), which is preferred when JMAP is configured.
Complex EAS MIME submissions (attachments, S/MIME, TNEF, nested messages)
*bypass* JMAP and relay the raw MIME via SMTP directly
(`send_eas_mime_message`, `src/email.rs:2773+`:
“for anything that cannot be re-expressed losslessly … the bytes are
relayed via SMTP”). Those messages fail with the TLS error above *even
when* JMAP submission is available, so this is P1 (send-path breakage for
the common “send a message with an attachment” case), not a fallback-only
failure.

**Fix (ladder 1).** Set `GATEWAY_SMTP_HOST=mail.example.com` (the placeholder for the public MX
host, matching `GATEWAY_MAIL_HOST`) so TLS verification succeeds, or
configure Stalwart to present a cert valid for `stalwart` (not possible
with a public CA). The compose comment at `docker-compose.yml:91-92`
already says “keep them on the Docker service name” — that comment is
*wrong* for TLS. Correct it.

---

### 20. JMAP session `apiUrl` is overridden to the internal base, but the `downloadUrl` / `uploadUrl` / `eventSourceUrl` templates are only partially internalised

**Evidence.** `src/jmap.rs:1876-1980` (`get_session`): `api_url` is
overridden wholesale to `base_url`; `download_url` and `event_source_url`
are passed through `internalize_template` (scheme+host+port swap, path
preserved). `upload_url` is *not* internalised in `get_session` — it is
handled later in `upload_blob` (`src/jmap.rs:1757-1820`) via a
*same-origin guard*, not a rewrite: the session-advertised `uploadUrl`
template is used as-is and rejected (`anyhow!`) unless its scheme, host,
and port match the configured `base_url` origin.

**Impact.** If Stalwart’s session advertises an `uploadUrl` on an origin
different from the bridge-network `base_url` (e.g. a public hostname or
port 8133), `upload_blob` fails outright instead of rewiring to the
bridge — blob upload (attachments, body blobs) breaks while `downloadUrl`
(direct blob fetch) keeps working. That failure is fail-closed and
security-correct; the risk is the *inconsistent* treatment: downloads
survive an origin mismatch, uploads do not.

**Fix (ladder 1).** No code change; add a comment in `docker-compose.yml`
at `GATEWAY_JMAP_BASE` stating that the gateway rewrites `downloadUrl`
and `eventSourceUrl` to the bridge-network base, while `uploadUrl` is
not rewritten and must be advertised on the same origin as `base_url`
(mismatched `uploadUrl` origins are rejected in `upload_blob`).

---

### 21. Calendar free/busy prefers JMAP `get_availability`, but the compose sets `GATEWAY_PREFER_CALDAV_FREEBUSY=0` while the code defaults to JMAP — the compose value is redundant but the fallback is CalDAV, not JMAP

**Evidence.** `docker-compose.yml:119` sets `GATEWAY_PREFER_CALDAV_FREEBUSY=0`.
`src/config.rs:49,1030` reads it. The knob is honored in the **EAS** path
(`src/eas.rs:4350`: skipped when `prefer_caldav_freebusy` is true) but
**ignored** in the **EWS** path — `merged_freebusy_for_mailbox`
(`src/ews.rs:1670`) and `fetch_freebusy_jmap` (`src/ews.rs:1586-1600`)
have no `prefer_caldav_freebusy` check at all; EWS always tries JMAP
first (`supports_availability()` → `get_availability()`, else the JMAP
event-query path) and only falls through to CalDAV when JMAP errors.
`src/jmap.rs:4336-4342` (`supports_availability`) checks for
`urn:ietf:params:jmap:principals:availability`.

**Impact.** Setting `GATEWAY_PREFER_CALDAV_FREEBUSY=1` does not change
EWS free/busy behavior, only EAS. The compose value is misleading as a
global knob.

**Fix (ladder 1).** Clarify the compose comment: `PREFER_CALDAV_FREEBUSY`
is EAS-only; EWS free/busy is JMAP-first unconditionally, with CalDAV as
the error fallback.

---

### 22. Tasks and Notes are CalDAV-backed (VTODO/VJOURNAL), not JMAP — the JMAP-first mandate cannot be met for Tasks/Notes on Stalwart v1.0.3

**Evidence.** `src/tasks.rs:5-8`: “Stalwart has no native Tasks or Notes
object type (no JMAP Task in v0.16)”. `src/eas.rs:2201,2234`: Tasks/Notes
reconcile via CalDAV `calendar-query` REPORT. The compose ships
`GATEWAY_CALDAV_BASE=http://stalwart:8080/dav` (required for Tasks/Notes).

**Impact.** This is not a bug — it is a Stalwart limitation. The task
says “wherever possible/feasible/preferable JMAP must be used”. For
Tasks/Notes, JMAP is not feasible on v1.0.3, so CalDAV is the correct
fallback. The risk is an operator disabling CalDAV (`GATEWAY_CALDAV_BASE`
empty) to “force JMAP”, which would break Tasks/Notes entirely.

**Fix (ladder 1 — docs).** Add a compose comment at
`GATEWAY_CALDAV_BASE`: *required for Tasks/Notes (VTODO/VJOURNAL) — Stalwart
v1.0.3 has no JMAP Task/Note type; do not empty this variable.*

---

### 23. The EWS `GetServiceConfiguration` response advertises URLs built from `gateway_host`

**Evidence.** `src/ews.rs` `handle_get_service_configuration` renders
`https://{gateway_host}/EWS/Exchange.asmx` etc. from
`state.cfg.gateway_host`. `src/main.rs` warns when the host looks like a
placeholder but does not fail.

**Impact.** If the placeholder `calendar.example.com` is left in the
operator's copy, EWS clients are pointed at an unresolvable host. The
placeholder convention (top of `docker-compose.yml`) requires the
operator to replace `calendar.example.com` with
`calendar.` + the real domain before deploying; the startup placeholder
warning makes a left-in value loud at deploy time.

**Fix (ladder 1).** Make the placeholder check fatal when
`GATEWAY_MAIL_DOMAIN` is also a placeholder (both must be edited together),
or at least return 503 from `GetServiceConfiguration` with a clear error
when the host is a placeholder.

---

### 24. `GATEWAY_SERVER_VERSION=15.2.2562.45` / `GATEWAY_SERVER_EXCHANGE_VERSION=Exchange2016` advertise Exchange Server SE, but the clients are 2025/2026 builds

**Evidence.** `docker-compose.yml:153-154`. `src/config.rs:86-95` documents
the defaults. `src/version.rs` renders the version stamps.

**Impact.** New Outlook for Windows (2025) and Outlook Android (2026) may
expect a newer `ServerVersion` / `EwsSupportedSchemas`. The gateway
advertises `Exchange2016` as the highest `RequestServerVersion` token
because there is no `Exchange2019` token (`src/config.rs:289-294`). This
is correct per MS-OXWSCORE, but a client that checks `ServerVersion` >=
a threshold may refuse.

**Fix (ladder 1).** Verify against the real clients (DEPLOY_RUNBOOK §6
smoke) that `15.2.2562.45` is accepted; if not, bump to the latest
Exchange Server SE build the clients accept. No code change.

---

### 25. The gateway binds `[::]:8134` (dual-stack) but the compose `ports: 8134:8134` publishes IPv4 only by default

**Evidence.** `docker-compose.yml:20` (`GATEWAY_BIND=[::]:8134`),
`docker-compose.yml:170` (`8134:8134`). `tests/outlook_cloudflare_smoke.sh`
checks both A and AAAA for the public host.

**Impact.** If the host’s Docker is IPv4-only for published ports, the
IPv6 gateway listener is unreachable from the host. Cloudflare Tunnel
connects via `localhost:8134` (IPv4 or IPv6 depending on `getent`), so
this is minor.

**Fix (ladder 1).** Apply the item 4 fix — retain the publish bound to
loopback (`127.0.0.1:8134:8134`). Do not remove it (host-run
`cloudflared` needs the published port to reach the container).

---

## P2 — Robustness / friction

### 26. `GATEWAY_CONFIG` (TOML file) is supported but the task forbids env files — the compose must not set it, and the code must not require it

**Evidence.** `src/main.rs:665` reads `GATEWAY_CONFIG`; `src/config.rs:430-445`
(`Config::load`) tolerates a missing file and falls back to env vars.
`docker-compose.yml:24` comments it out.

**Impact.** None today — the code correctly falls back to env vars.
The risk is a future operator mounting a TOML file that diverges from the
compose.

**Fix (ladder 1).** Keep the compose line commented. Add a code comment
in `src/config.rs:430` stating the deployment contract is compose-only.

---

### 27. `GATEWAY_HMAC_SECRET` is required to be ≥32 chars and not start with `REPLACE_`, but the compose ships `REPLACE_WITH_A_RANDOM_32_CHAR_MINIMUM_SECRET` — a copy-paste without editing fails at startup with a clear error

**Evidence.** `docker-compose.yml:52`. `src/config.rs:620-630` validates
length and `REPLACE_` prefix and fails closed.

**Impact.** Correct fail-closed behaviour. The operator must generate a
secret (`openssl rand -hex 32`) and replace the placeholder. This is
documented but is a common deploy-time failure.

**Fix (ladder 1).** Add a `DEPLOY_RUNBOOK.md` §3 step:
`openssl rand -hex 32` and paste into the compose.

---

### 28. `GATEWAY_ADMIN_PASSWORD` in cleartext in the compose file

**Evidence.** `docker-compose.yml:161`. `src/config.rs:54,1060-1064` reads
it via `get_secret_env` (supports `GATEWAY_ADMIN_PASSWORD_FILE`).
The compose comment at `docker-compose.yml:157-159` already warns:
“these sit in cleartext in this file — restrict the file’s permissions
(chmod 600) or use Docker secrets”.

**Impact.** The task forbids env files but does not forbid Docker
secrets. The operator must `chmod 600 docker-compose.yml` on the host
(Portainer host) or use the `_FILE` variant. The `_FILE` variant is code-
supported but not documented in the compose.

**Fix (ladder 1).** Add the `_FILE` lines as comments in the compose:
`# - GATEWAY_HMAC_SECRET_FILE=/run/secrets/gateway_hmac` etc.

---

### 29. The Dockerfile `HEALTHCHECK` uses `curl`, which is installed in the runtime image (`Dockerfile:39`) — correct, but the compose healthcheck duplicates it

**Evidence.** `Dockerfile:76-77` and `docker-compose.yml:191` both
`curl -f http://localhost:8134/health/ready`.

**Impact.** Harmless duplication. If one is changed, the other drifts.

**Fix (ladder 1).** Keep both but add a comment in each pointing at the
other.

---

### 30. `GATEWAY_MAX_ATTACHMENT_BYTES=5242880` (5 MiB) may be too small for Outlook attachments

**Evidence.** `docker-compose.yml:63`. `src/config.rs:26,776-790` reads it,
floors at 1024, caps at 50 MiB (`src/config.rs:633-637`). Route body limits
are hardcoded at 4 MiB (`src/main.rs:31` `MAX_BODY_BYTES`, applied to the
EAS/EWS/Autodiscover routes at `src/main.rs:828-870`). The code's own
comment notes the 4 MiB cap must not silently reject large `RopLogon`
batches.

**Impact.** Outlook Android and New Outlook for Windows routinely send
>5 MiB attachments. Even if the operator raises
`GATEWAY_MAX_ATTACHMENT_BYTES`, a request carrying an attachment at the
limit never reaches the handler: the 4 MiB `RequestBodyLimitLayer` rejects
it first (Fetch responses are the opposite direction — the gateway *sends*
attachment bytes, so `TooLarge`/`Status 11` there is a fetch-time reject,
not a request-size reject). The attachment default and the route body
limit must be raised together, or large-attachment sends fail at the
edge.

**Fix (ladder 1).** Raise the compose default to `10485760` (10 MiB) or
`52428800` (50 MiB, the code cap) *and* raise `MAX_BODY_BYTES` in
`src/main.rs` to a value that covers the configured maximum attachment
plus encoding overhead (at least `max_attachment_bytes` × 4/3 for
base64 MIMEData, plus WBXML/soap envelope margin). Document the trade-off
with `GATEWAY_MAX_PING_HEARTBEAT`.

---

### 31. `GATEWAY_RATE_LIMIT_REQUESTS_PER_MINUTE=600` is per-IP; a single device behind NAT may exceed it during initial sync

**Evidence.** `docker-compose.yml:79`. `src/rate_limit.rs` keys on
`CF-Connecting-IP`.

**Impact.** Initial sync of a large mailbox (thousands of items) can
burst >600/min from one IP. The client gets 429 and retries.

**Fix (ladder 1).** Raise to 1200 or document the knob. No code change.

---

### 32. The `AutoDetect` `WWW-Authenticate: Bearer` header advertises `authorization_uri` for Microsoft’s cloud, which is misleading for a self-hosted gateway

**Evidence.** `src/eas.rs:1373-1427` (`unauth_response`): includes
`Bearer` with `authorization_uri` pointing at Microsoft’s AutoDetect.
`tests/outlook_cloudflare_smoke.sh` checks for it.

**Impact.** This is required for AutoDetect compatibility (without it,
AutoDetect reports “missing authorization URL” and falls back to IMAP).
It is correct but confusing: the gateway is not Microsoft.

**Fix (ladder 1).** No change; add a comment in the compose at the
rate-limit section explaining the Bearer header is for AutoDetect only.

---

### 33. `GATEWAY_MAPI_ENABLED=1` with `GATEWAY_MAPI_HMA_ENABLED=0` advertises `mapiHttp` in Autodiscover and accepts Basic auth — this is the shipped posture and is correct for the use-case, but it is a security trade-off

**Evidence.** `docker-compose.yml:141-142`. `src/autodiscover.rs:566-570`,
`src/main.rs:206-215` (AUDIT3 #17). `src/mapi/logon.rs` verifies Basic via
`AuthVerifier`.

**Impact.** New Outlook for Windows needs MAPI/HTTP with Basic (no HMA
tenant). This is the intended posture. The trade-off is that Basic auth
over the tunnel is only as secure as the tunnel + Cloudflare edge.

**Fix (ladder 1).** No change; ensure the compose comment is accurate
(it is).

---

### 34. The repo has no `README.md` — the entry point for a new operator is `DEPLOY_RUNBOOK.md`, which is not linked from the compose

**Evidence.** `ls *.md` → `AGENTS.md`, `CLOUDFLARED_SETUP.md`,
`DEPLOY_RUNBOOK.md`, `AUDIT3.MD`, `AUDIT4.MD` (this file). No `README.md`.

**Impact.** A new operator landing on the repo does not know where to
start.

**Fix (ladder 1).** Add a `README.md` that points to `DEPLOY_RUNBOOK.md`
for the use-case and to `CLOUDFLARED_SETUP.md` for the tunnel.

---

### 35. `AGENTS.md` still says Stalwart v1.0.1 and Outlook Android 5.2634.2, while the task pins v1.0.3 and 5.2641.1

**Evidence.** `AGENTS.md:5,7`. Task pins v1.0.3 / 5.2641.1 / New Outlook
20251205004.10 (7 Oct 2026).

**Impact.** Documentation drift; a future agent may validate against the
wrong versions.

**Fix (ladder 1).** Update `AGENTS.md`, `DEPLOY_RUNBOOK.md:6,19`, and
`AUDIT3.MD:5-6` headers to the pinned versions.

---

### 36. The smoke script `tests/outlook_cloudflare_smoke.sh` uses `GATEWAY_BASE_URL` / `GATEWAY_USER` / `GATEWAY_PASS` env vars, but the deployment forbids env files

**Evidence.** `tests/outlook_cloudflare_smoke.sh:5-7`:
`: "${GATEWAY_BASE_URL:?set GATEWAY_BASE_URL, …}"`.

**Impact.** The script is a test harness, not the deployment, so env
vars are acceptable there. But the task says “The use of an environmental
file is forbidden” — the script does not use a file, only the shell env,
so it is compliant. The confusion is worth a comment.

**Fix (ladder 1).** Add a header comment to the script: “Test harness
only; the deployment itself uses no env file (compose-inline only).”

---

## 12. Operator change list — Cloudflare Zero-Trust and Stalwart container

### 12.1 Cloudflare Zero-Trust (webui only, as configured)

The operator’s existing routes are:

| Hostname | Service | Status |
|----------|---------|--------|
| `https://stalwart.example.com/admin` *(placeholder for `stalwart.` + your real domain)* | Stalwart webui | keep |
| `https://calendar.example.com/*` *(placeholder for `calendar.` + your real domain)* | `http://localhost:8134` (gateway) | keep |
| `https://autodiscover.example.com/*` *(placeholder for `autodiscover.` + your real domain)* | `http://localhost:8134` (gateway) | keep |

**No new tunnel route is required and none can be created for the root
domain.** The two gateway routes are sufficient *provided*:

1. `GATEWAY_HOST=calendar.example.com` (placeholder for `calendar.` +
   your real domain; matches the `calendar.` route),
2. `GATEWAY_MAIL_DOMAIN=example.com` (placeholder for your real root /
   mail domain, so `autodiscover.example.com` matches the Android probe
   `autodiscover.` + the mail domain),
3. `GATEWAY_MAIL_HOST=mail.example.com` (placeholder for your real MX
   hostname; matches the existing MX record).

**Do not create** a route for the bare root domain (`example.com` =
placeholder for your real root domain) — it already has A/AAAA records
and cannot be tunnel-routed. The root-domain Autodiscover fallback will
fail, but the primary probe (`autodiscover.` + the mail domain)
succeeds, so Android proceeds. This is the documented behaviour in
`CLOUDFLARED_SETUP.md`.

**DNS (Cloudflare, proxied):** ensure CNAMEs exist for `calendar` and
`autodiscover` pointing at `<tunnel-id>.cfargotunnel.com` (proxied).
The `mail` record is the existing MX host and must *not* be proxied to
the tunnel — it is Stalwart’s SMTP/IMAP host.

### 12.2 Stalwart container (operator’s compose, in the task)

Current operator compose (from the task):

```yaml
services:
  stalwart:
    image: stalwartlabs/stalwart:latest
    ports: [8080, 8133, 25, 465, 587, 993, 4190]
    environment: [TZ=UTC, STALWART_PUBLIC_URL=https://stalwart.example.com/]  # placeholder for stalwart.<your real domain>
    hostname: mail.example.com  # placeholder for your real MX hostname
    volumes: [/armblock/stalwart_v1/etc:/etc/stalwart, /armblock/stalwart_v1/data:/var/lib/stalwart]
    networks: [network1]
```

Required changes:

1. **Pin the image** to `stalwartlabs/stalwart:v1.0.3@sha256:<digest>`
   (operator supplies the digest). `:latest` will drift past the version
   the gateway was validated against (item 10).
2. **No port is needed for the gateway on 8133.** The gateway uses
   `stalwart:8080` over the bridge network. Keep 8133 published only if
   the operator uses it for something else.
3. **`STALWART_PUBLIC_URL`** is correct (`https://stalwart.example.com/`
   placeholder for `stalwart.` + your real domain) and matches the
   `stalwart.example.com/admin` Zero-Trust route.
4. **No Stalwart config change is required for JMAP calendar/contacts**
   — Stalwart v1.0.3 advertises `urn:ietf:params:jmap:calendars`,
   `:calendars:parse`, `:contacts`, `:contacts:parse`, `:mail`,
   `:principals:availability`, `:submission`, `:sieve` in
   `GET /jmap/session` (AUDIT3 §“Stalwart-side configuration”).
5. **CardDAV** is served at `/dav/card/{user}/` — the gateway’s
   `GATEWAY_CARDDAV_BASE=http://stalwart:8080/dav/card` is correct
   (AUDIT3 §3 DELIVERED). Confirm the address book exists at
   `/dav/card/<user>/default/`.
6. **Admin account** (optional, for the full GAL): create a Stalwart
   account with `sysAccount*` / `sysMailingList*` permissions and set
   `GATEWAY_ADMIN_USERNAME` / `GATEWAY_ADMIN_PASSWORD` in the gateway
   compose if the operator wants the full GAL (item 9). The deploy-time
   capability probe creates a throwaway
   `exchange-gateway-deploy-probe-<uuid>` calendar.

### 12.3 Gateway compose (this repo, `docker-compose.yml`)

The operator must fill in (placeholders must be replaced — the task says
do not replace placeholders *in the repo template*, but the operator edits
their *copy* before deploying via Portainer):

| Variable | Value to set | Why |
|----------|--------------|-----|
| `image` | `<registry>/exchange-gateway:0.1.0` → real registry | task: image from registry |
| `GATEWAY_HOST` | `calendar.example.com` *(placeholder for `calendar.` + your real domain)* | matches Zero-Trust `calendar.` route |
| `GATEWAY_MAIL_DOMAIN` | `example.com` *(placeholder for your real root / mail domain)* | mailbox domain; drives `autodiscover.` probe |
| `GATEWAY_MAIL_HOST` | `mail.example.com` *(placeholder for your real MX hostname)* | matches MX record; advertised in Autodiscover |
| `GATEWAY_HMAC_SECRET` | `openssl rand -hex 32` output (≥32 chars) | required; must not start with `REPLACE_` |
| `GATEWAY_CALDAV_BASE` | `http://stalwart:8080/dav` (keep) | CalDAV + Tasks/Notes (VTODO/VJOURNAL) |
| `GATEWAY_JMAP_BASE` | `http://stalwart:8080/jmap` (keep) | JMAP-first backend |
| `GATEWAY_CARDDAV_BASE` | `http://stalwart:8080/dav/card` (keep) | CardDAV fallback for contacts |
| `GATEWAY_SMTP_HOST` | `mail.example.com` *(placeholder for your real MX hostname)* (NOT `stalwart`) | TLS verification for submission (item 19) |
| `GATEWAY_ALLOW_INSECURE_HTTP` | `1` (keep) | bridge-network HTTP backend |
| `GATEWAY_MAPI_ENABLED` | `1` (keep) | New Outlook for Windows needs MAPI/HTTP |
| `GATEWAY_MAPI_HMA_ENABLED` | `0` (keep) | Basic auth; no HMA tenant |
| `GATEWAY_ADMIN_USERNAME/PASSWORD` | empty (self-only GAL) or a Stalwart admin | item 9 |

The volume `/armblock/exchange-gateway-data:/var/lib/exchange-gateway`
already matches the task’s stated path. The host directory must be
writable by UID 10001 (the gateway user in the Dockerfile). Retain the
`ports:` mapping but bind it to loopback — `127.0.0.1:8134:8134` (item 4).
Do *not* remove the mapping: the host-run `cloudflared` targets
`http://localhost:8134`, and removing the publish disconnects both
gateway tunnel routes from the container.

---

## 13. Compose env-var inventory (code vs shipped compose)

Every `GATEWAY_*` variable read by the code (`src/config.rs`,
`src/main.rs`, `src/logging.rs`) is listed here. “Compose” means the
variable appears in `docker-compose.yml` (active or commented).

| Variable | Code default | Compose | Notes |
|----------|--------------|---------|-------|
| `GATEWAY_BIND` | `[::]:8134` | active | in-container bind |
| `GATEWAY_CALDAV_BASE` | — (required) | active | `http://stalwart:8080/dav` |
| `GATEWAY_DATABASE_PATH` | — (required) | active | `/var/lib/exchange-gateway/gateway.db` |
| `GATEWAY_HMAC_SECRET` | — (required, ≥32) | active (placeholder) | operator must replace |
| `GATEWAY_HOST` | — (warn if placeholder) | active (placeholder) | must be `calendar.` + the mail domain |
| `GATEWAY_MAIL_DOMAIN` | — (required) | active (placeholder) | mailbox domain |
| `GATEWAY_MAX_ATTACHMENT_BYTES` | 5 MiB | active | raise (item 30) |
| `GATEWAY_ROOM_BOOKING_ENABLED` | true | active | |
| `GATEWAY_AUTH_CACHE_TTL_SECS` | 300 | active | |
| `GATEWAY_AUTH_CACHE_NEGATIVE_TTL_SECS` | 30 | active | |
| `GATEWAY_AUTH_CACHE_MAX_ENTRIES` | 10000 | active | |
| `GATEWAY_AUTH_PROBE_MIN_INTERVAL_MS` | 1000 | active | |
| `GATEWAY_AUTH_BACKEND_FAILURE_THRESHOLD` | 5 | active | |
| `GATEWAY_AUTH_BACKEND_MAX_BACKOFF_SECS` | 60 | active | |
| `GATEWAY_SMTP_HOST` | — | active | **change to `mail.example.com` (placeholder for the real MX host, item 19)** |
| `GATEWAY_SMTP_PORT` | 465 | active | |
| `GATEWAY_IMAP_HOST` | — | active | dead config (item 18) |
| `GATEWAY_IMAP_PORT` | 993 | active | dead config (item 18) |
| `GATEWAY_JMAP_BASE` | derived from CalDAV | active | `http://stalwart:8080/jmap` |
| `GATEWAY_CARDDAV_BASE` | derived from CalDAV | active | `http://stalwart:8080/dav/card` |
| `GATEWAY_EMAIL_ENABLED` | true | active | |
| `GATEWAY_MAIL_HOST` | `mail.` + mail domain | active (placeholder) | must be MX host (item 2) |
| `GATEWAY_FORCE_CALDAV_CALENDAR` | false | active | emergency hatch only |
| `GATEWAY_PREFER_CALDAV_FREEBUSY` | false | active | |
| `GATEWAY_PREFER_JMAP_CALENDAR` | true | active | |
| `GATEWAY_PREFER_JMAP_CONTACTS` | true | active | |
| `GATEWAY_ALLOW_INSECURE_HTTP` | false | active (`1`) | bridge-network HTTP |
| `GATEWAY_EAS_REQUIRE_POLICY_KEY` | false | active | |
| `GATEWAY_ADMIN_USERNAME` | — | active (empty) | self-only GAL if empty (item 9) |
| `GATEWAY_ADMIN_PASSWORD` | — | active (empty) | cleartext; chmod 600 |
| `GATEWAY_RATE_LIMIT_ENABLED` | true | active | |
| `GATEWAY_RATE_LIMIT_REQUESTS_PER_MINUTE` | 120 | active (600) | |
| `GATEWAY_RATE_LIMIT_MAX_CONCURRENT` | 1000 | active (2000) | |
| `GATEWAY_MAX_PING_HEARTBEAT` | — (80 if CF-proxied) | active (80) | |
| `GATEWAY_MAPI_ENABLED` | true | active | |
| `GATEWAY_MAPI_HMA_ENABLED` | false | active | |
| `GATEWAY_MAPI_OIDC_ISSUER` | — | commented | only with HMA |
| `GATEWAY_MAPI_OIDC_AUDIENCE` | — | commented | only with HMA |
| `GATEWAY_MAPI_ORG` | — | commented | |
| `GATEWAY_SMIME_CERT_STORE_DIR` | — | active (empty) | |
| `GATEWAY_MAPI_MAX_CONTENTS_ROWS` | 10000 | commented | |
| `GATEWAY_MAPI_CONTENTS_PAGE_SIZE` | 256 | commented | |
| `GATEWAY_SERVER_VERSION` | `15.2.2562.45` | active | |
| `GATEWAY_SERVER_EXCHANGE_VERSION` | `Exchange2016` | active | |
| `GATEWAY_LOG_LEVEL` | `info` | active | |
| `GATEWAY_LOG_FORMAT` | `pretty` | commented | |
| `GATEWAY_LOG_JSON` | — | commented | deprecated |
| `GATEWAY_LOG_NO_TIMESTAMPS` | — | commented | |
| `GATEWAY_LOG_THREADS` | — | commented | |
| `GATEWAY_LOG_TARGET` | — | commented | |
| `GATEWAY_CALENDAR_CAPABILITY_CHECK` | `warn` | active | |
| `GATEWAY_CONFIG` | — | commented | **must stay unset (compose-only contract)** |
| `GATEWAY_*_FILE` (`_FILE` secrets) | — | not listed | supported by `get_secret_env`; document (item 28) |

`GATEWAY_RULE_PROVIDER` is *not* an env var — it is a Rust const
(`src/mapi/rules.rs:68`) and must not be added to the compose.

No `env_file:` and no `.env` exist in the repo (verified:
`ls .env*` → none; `grep -rn env_file` → none). This complies with the
task’s “no env-variable file” requirement.

---

## 14. Summary table

| # | Sev | Area | Defect | Fix ladder |
|---|-----|------|--------|------------|
| 1 | P0 | Cloudflare / Autodiscover | Ingress hostnames must match the `autodiscover.` + mail-domain / `GATEWAY_HOST` contract (`example.com` placeholders) — **OPEN**: the placeholder convention is unenforced; the startup pairing check does not exist yet | docs + startup check (1) |
| 2 | P0 | Autodiscover | `GATEWAY_MAIL_HOST` placeholder leaks NXDOMAIN to IMAP/SMTP clients — **OPEN**: only `gateway_host` is startup-checked today, `mail_host` is not | compose + startup check (1) |
| 3 | P0 | HTTP/TLS | Gateway serves plain HTTP only; `x-forwarded-proto` enforcement inconsistent | TLS listener (2) / warning (1) |
| 4 | P0 | Compose | Port 8134 published on wildcard bind, bypasses tunnel | loopback-only publish (1) |
| 5 | P0 | EAS | `FolderCreate/Delete/Update`, `GetAttachment`, `GetHierarchy`, `Find` unimplemented (Status 5) | handlers (1→4) |
| 6 | P0 | New Outlook Win | MAPI/HTTP is the real path, not EAS; no MAPI smoke coverage | smoke script + docs (1) |
| 7 | P0 | Outlook Android | Option matrix pinned to 5.2634.2, task pins 5.2641.1 | re-capture matrix (1) |
| 8 | P0 | Android provisioning | Outlook Android app goes via Microsoft cloud; native wizard is direct | docs (1) |
| 9 | P0 | GAL / deploy check | Admin creds empty → self-only GAL, no JMAP probe | docs + log (1) |
| 10 | P0 | Version drift | Stalwart `:latest` vs validated v1.0.3; docs say v1.0.1 | pin image (1) |
| 11 | P1 | Backend ports | 8080 vs 8133 undocumented | compose comment (1) |
| 12 | P1 | Docs | `cloudflared/config.yml` references a forbidden `.env` | docs (1) |
| 13 | P1 | Healthcheck | `/health/ready` unhealthy during Stalwart restart; no auto-restart | docs (1) |
| 14 | P2 | Dockerfile | Dynamic linking; no static target | smoke / musl (1→3) |
| 15 | P1 | EWS streaming | 30-min `GetStreamingEvents` exceeds CF 125s cap | CF-aware clamp (1) |
| 16 | P1 | Rate limit | Per-IP limit throttles Microsoft cloud egress | docs (1) |
| 17 | P1 | Insecure HTTP | Global flag disables HTTPS check for all backends | startup warning (1) |
| 18 | P1 | IMAP | Advertised but no gateway IMAP client; config dead | docs / remove block (1) |
| 19 | P1 | SMTP | `stalwart:465` TLS verification fails; must be `mail.example.com` (placeholder for the real MX host) | compose value (1) |
| 20 | P1 | JMAP session | URL internalisation scattered across three sites | docs (1) |
| 21 | P1 | Free/busy | `PREFER_CALDAV_FREEBUSY` comment misleading | docs (1) |
| 22 | P1 | Tasks/Notes | CalDAV-backed, not JMAP — Stalwart limitation | docs (1) |
| 23 | P1 | EWS config | Placeholder `GATEWAY_HOST` advertised in `GetServiceConfiguration` | fail-closed (1) |
| 24 | P1 | Version stamp | Exchange SE 15.2 vs 2025/2026 clients | verify (1) |
| 25 | P1 | Bind | Dual-stack bind vs IPv4 publish mismatch | compose (1) |
| 26 | P2 | Config | `GATEWAY_CONFIG` TOML supported but must stay unset | docs (1) |
| 27 | P2 | Secrets | `GATEWAY_HMAC_SECRET` placeholder; startup fails if unedited | runbook step (1) |
| 28 | P2 | Secrets | Admin password cleartext in compose; `_FILE` undocumented | docs (1) |
| 29 | P2 | Healthcheck | Duplicated in Dockerfile + compose | comment (1) |
| 30 | P2 | Attachments | 5 MiB cap too small for Outlook | raise default (1) |
| 31 | P2 | Rate limit | 600/min may throttle initial sync | raise / document (1) |
| 32 | P2 | Auth | Bearer header advertises Microsoft AutoDetect | comment (1) |
| 33 | P2 | MAPI auth | Basic + no HMA is intended but a trade-off | comment (1) |
| 34 | P2 | Docs | No `README.md` | add (1) |
| 35 | P2 | Docs | Version pins stale (v1.0.1 / 5.2634.2) | update (1) |
| 36 | P2 | Test harness | Smoke script uses env vars (compliant, but confusing) | header comment (1) |

**Bottom line.** The gateway’s core protocol work (EAS WBXML/Sync, EWS,
MAPI/HTTP, Autodiscover, JMAP calendar/contacts, Tasks/Notes via CalDAV)
is substantially sound and the AUDIT3 P0s are delivered. The remaining
P0s are *deployment-contract* failures, not protocol failures: the
`example.com` placeholders must be replaced with the operator's real
domain before deployment (items 1-2, 23 — still **OPEN**, because the
placeholder convention is unenforced and the startup checks do not exist
yet), the compose publishes a port on a wildcard bind that bypasses the
tunnel, the SMTP host breaks TLS, and the EAS command surface is missing
the folder-management and search commands the clients use (with
`GetHierarchy` scoped to the 2.5/12.x contract only).
Fixing P0 items 1–10 plus P1 items 11–13 (mostly
compose/docs, plus the EAS handlers in item 5) is **not** by itself
sufficient to guarantee the use-case: item 6 records the New Outlook for
Windows (MAPI/HTTP) provisioning flow as **unproven** and must be
validated against build 20251205004.10 before the use-case can be
claimed complete; items 15–25 are P1 (degrades the use-case) and items
14, 26–36 are P2 (robustness and friction), per the summary table.

---

## Appendix A — What “serve http” means for this deployment

The gateway binds `[::]:8134` and serves plain HTTP (`src/main.rs:959`).
`cloudflared` on the Ubuntu host terminates TLS at the Cloudflare edge and
forwards to `http://localhost:8134` over the tunnel. This is the correct
posture and matches the task’s “if possible/feasible the
exchange_gateway-container should ‘serve’ http”. The gateway must *not*
be changed to serve HTTPS directly unless the operator removes
`cloudflared` — the two are mutually exclusive on the same port. If
direct HTTPS is later required, add a TLS listener as a separate bind
(item 3) rather than replacing the HTTP listener.

## Appendix B — JMAP-first compliance

| Surface | Backend used | JMAP? | Notes |
|---------|--------------|-------|-------|
| Email read/sync | JMAP `Email/*` | yes | `src/jmap.rs` |
| Email send | JMAP `EmailSubmission/set` for simple text/html; raw-MIME SMTP relay for complex MIME | partial | `src/email.rs:1234`, `src/email.rs:2773` |
| Calendar read/write | JMAP `CalendarEvent/*`, CalDAV fallback | yes | `src/jmap.rs`, `src/jscalendar.rs` |
| Calendar free/busy | JMAP `getAvailability`, event-derivation fallback | yes | `src/ews.rs:1586` |
| Contacts | JMAP `ContactCard/*` (RFC 9610), CardDAV fallback | yes | `src/contacts.rs` |
| Tasks | CalDAV VTODO | **no** | Stalwart has no JMAP Task type (item 22) |
| Notes | CalDAV VJOURNAL | **no** | Stalwart has no JMAP Note type (item 22) |
| Auth | JMAP `GET /session` (Basic), CalDAV fallback | yes | `src/auth.rs` |
| GAL / directory | JMAP `x:Account/*`, `x:MailingList/*` | yes | `src/directory.rs` |
| OOF / Sieve | JMAP Sieve | yes | `src/oof.rs` |
| Push | JMAP EventSource, EAS Ping | yes | `src/jmap_push.rs` |

The only non-JMAP backends are Tasks/Notes (Stalwart limitation) and SMTP
for send: simple text/html messages use JMAP `EmailSubmission/set`, but
complex MIME (attachments, S/MIME, TNEF, nested messages) is always
relayed via SMTP (`send_eas_mime_message`, `src/email.rs:2773`) even when
JMAP submission is available. This satisfies “wherever
possible/feasible/preferable JMAP must be used” for the simple-message
path; the complex-MIME path is a documented SMTP exception (item 19).

## Appendix C — Placeholder convention and files changed

This audit's placeholder convention applies to every hostname in the
deployment templates and docs. The files changed by the PR that
introduced it are `AUDIT4.MD`, `docker-compose.yml`,
`cloudflared/config.yml`, `CLOUDFLARED_SETUP.md`, and
`DEPLOY_RUNBOOK.md`. The convention at the top of this file is the
single rule for those hostnames: `example.com` (and `mail.example.com`,
`calendar.example.com`, `autodiscover.example.com`,
`stalwart.example.com`) are placeholders for the operator's real domain /
MX hostname and are replaced in the operator's own copy before
deployment. The operator's changes in §12 are applied to *their* copies
of `docker-compose.yml` (gateway and Stalwart), not to the repo
template placeholders, per the task's "do not replace any placeholders".
No real domain appears in any of these files.
