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
  `MKCALENDAR` with `VEVENT`; Type 15 (Tasks) → `VTODO`; Type 16/17
  (Journal/Notes) → `VJOURNAL`. Type 14 (Contacts) is not creatable (CardDAV
  collection creation is unsupported): `FolderCreate` rejects it up front with
  Status 10 rather than backing a "Contacts" folder with a JMAP mail mailbox.
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
  `<Add>`/`<Update>`/`<Delete>` rather than a stale `<Count>0</Count>`.
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

- **Email property (flag) updates.** `Email/queryChanges` reports only
  membership of the query result set (RFC 8620 §5.6), so a message whose flags
  change without leaving the mailbox (mark read/flagged, ...) never appears in
  its `added`/`removed` lists and would otherwise be dropped by both the EAS
  and EWS email deltas. The EAS delta now keeps a per-collection **keyword
  baseline** (`email_keyword_baseline`, one row per JMAP Email id holding its
  sorted keyword list) captured at the start of every initial sync — the
  snapshot is taken before the first window is fetched, so a flag edit during
  pagination is still reported as a `<Change>` by the first delta rather than
  being folded into the baseline as already-synchronized; the capture is
  best-effort (a backend hiccup leaves the previous baseline, and the first
  delta then captures it without reporting the intervening edits). The delta
  then diffs the live mailbox against this baseline, emitting a `<Change>` for
  each id whose keyword set changed and was not already queued. The diff is
  **bounded per delta**: the scan walks at most a fixed number of pages
  (`KEYWORD_SCAN_MAX_PAGES`) from a per-collection rotating position persisted
  in `email_keyword_scan` (wrapping to the top at the end), so a delta never
  pays O(mailbox) in JMAP calls — every message is re-examined once per sweep,
  with the sweep's cost amortized across deltas. The observed baseline rows,
  the rows for messages the feed reported as having left the mailbox (they can
  never be observed by a later scan page, so they are pruned rather than
  leaking), and the next position are persisted only after the whole change set
  is delivered, so a crash re-scans the same window rather than swallowing an
  update; an overflowed delta (the change set exceeds the EAS window) persists
  them when the remainder is durably queued, so the drain and every later delta
  do not re-queue the same flag change. A backend hiccup during the scan
  degrades to serving the query-feed changes without persisting that page's
  rows.
  `Email/queryChanges` has no `hasMoreChanges` field (RFC 8620 §5.6), so
  `has_more_changes` is a heuristic — set when a page returns exactly at the
  `maxChanges` cap — and the drain loop plus the EWS
  `IncludesLastItemInRange` reflect a *possibly* clipped page (one extra page
  is fetched to confirm the feed drained) rather than a fixed `false`.

**Status codes.** Per [MS-ASCMD] §2.2.3.177.8 / §6.10–§6.19: 1 success,
2 invalid/duplicate or special system folder, 3 special system folder
(delete), 4 not found, 5 invalid parent, 6 server error, 10 malformed request.

**Tests.** 26 in `src/eas_folders.rs` (classification helpers, response
rendering + a WBXML round-trip of the FolderHierarchy-rooted responses, and
end-to-end create/delete/update against a combined JMAP+CalDAV stub asserting
the exact `Mailbox/set`/`MKCALENDAR`/`DELETE`/`PROPPATCH` wire calls and the
persisted `folder_map` rows, plus the hierarchy-change journal watermark). The
`eas::tests::test_folder_sync_create_then_rename_reaches_new_device_as_add`
test drives a real `handle_folder_sync` prime + delta to pin the
create-then-rename compaction (a folder added and then renamed before a second
device's first incremental FolderSync reaches it as an `<Add>` with the latest
DisplayName, never an `<Update>` for a folder it never received). The
email-delta scoping adds `eas::tests::test_classify_query_change_ops_maps_add_remove_and_in_place`
(pure classification) and
`eas::tests::test_email_delta_sync_uses_mailbox_scoped_query_changes` (an
end-to-end delta against a JMAP stub asserting the wire call carries
`filter.inMailbox`, never `Email/changes`, and renders Add/Delete), plus three
`jmap::tests::query_email_changes_*` tests (mailbox scoping + classification,
`cannotCalculateChanges` and Stalwart `invalidArguments` both downcasting to
the typed `CannotCalculateChanges`).

**Operator change list.** None required for Cloudflare or Stalwart: the new
commands use the already-configured JMAP and CalDAV endpoints. No new
environment variables.

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
substantially sound (AUDIT3 P0 calendar/contacts are delivered), but the use-case
is **not** deployment-ready: the EAS folder-management and attachment commands are
stubs (P0 #2), the hostname pairing is not enforced (P0 #3), SMTP TLS will fail
against the internal bridge name (P0 #10), and the Stalwart backend version
contract is unvalidated for v1.1.0 (P0 #1). Resolving P0 items 1–10, plus the
operator change list above, is required before the container can be deployed via
Portainer for the stated use-case.
