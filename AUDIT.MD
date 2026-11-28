# AUDIT.MD — Outstanding work to deliver the stated use case

**Target use case.** One container (`exchange_gateway`) next to an existing
**Stalwart Mailserver v1.1.0** container must let *Outlook for Windows (new,
October 2026; classic 2408/16.0.17932.21056 as a temporary fallback)* and
*Outlook for Android (5.2645.1)* use one Stalwart mailbox — **calendar and
email, fully and natively, with no client-side extensions** — after the account
is added as an **Exchange** account in Android settings. Public entry is
`cloudflared` on the Ubuntu host; Cloudflare Zero Trust routes already exist
for `calendar.<domain>/*` and `autodiscover.<domain>/*` → `:8134`. Deployment is
Portainer + a registry image + `docker-compose.yml` with inline environment
only (no env file).

**Method.** Full read of the crate (≈96k LOC) plus a *contract-level* comparison
against the Stalwart **v1.1.0** sources (`stalwart-src` @ `e7cde9b3`) and the
in-repo Microsoft specs (`exchange_protocols/`, v20250520). Every finding below
cites either a gateway code site, a Stalwart v1.1.0 code site, or a spec clause.
Findings are ordered strictly by priority: **1 = fix first**.

**Scope note.** This document supersedes every other audit file in the
repository for the purpose of *this* deployment. It was written from the code
and the v1.1.0 backend contract only; it deliberately does not build on
`AUDIT3.MD` / `AUDIT4.MD` / `AUDIT5.MD`.

**Revision (2026-11-27).** Every finding was re-validated against the tree and
the **Stalwart v1.1.0** sources; this revision contains only findings that
survived that check, renumbered **1–38**. Three entries from the previous
revision are gone: the EAS `Ping` heartbeat clamp
(`effective_ping_heartbeat`, `src/eas.rs:2443`, with the 80 s Cloudflare
default in `src/config.rs:65`) and the JSCalendar-native calendar write path
(`src/jscalendar.rs:83`/`:146` plus the typed read-back guard in
`set_calendar_event`) are already implemented and are kept as verified
behaviour in Part E; the separate OOF `BodyType` render-site note is folded
into finding 14, whose *Where* already cites the same lines. Eight
descriptions were rewritten where the earlier evidence was wrong (3, 8, 17,
19, 23, 28, 33, 37).

**How each finding is written**

* *Where* — code sites (file:line at the time of this audit).
* *Evidence* — spec clause or backend code that makes this a defect, not taste.
* *Impact* — what the operator or client sees.
* *Fix* — solution, chosen with the mandated precedence:
  1) existing dependency → 2) new high-quality dependency → 3) Rust 1.99.1 std
  → 4) custom code.
* *Verify* — the check that proves the fix (ideally a live test).

---

## Index

| # | Prio | Area | Title |
|---|------|------|-------|
| 1 | P0 | Mail send | JMAP `EmailSubmission/set` omits `identityId` — every JMAP send is rejected |
| 2 | P0 | Mail send | SMTP fallback verifies TLS against `stalwart` — attachments/S-MIME cannot be sent at all |
| 3 | P0 | OOF | Out-of-office is a silent no-op (Sieve id, property, account, activation, swallowed errors) |
| 4 | P0 | MAPI rules | Inbox rules are broken by the same Sieve API misuse |
| 5 | P0 | MAPI/HTTP | MAPI/HTTP never returns `401` — New Outlook for Windows cannot authenticate |
| 6 | P0 | MAPI/HTTP | `NotificationWait` is a 5-minute buffered response — Cloudflare 524, and not the spec's chunked shape |
| 7 | P1 | Backend contract | Nothing is verified against **Stalwart v1.1.0** (repo asserts v1.0.3); add a live contract suite |
| 8 | P1 | Mail send | No JMAP-first attachment send (`Blob/upload` + `Email/import`); size limits not negotiated |
| 9 | P1 | Deployment | Admin credentials ship empty → OOF/rules/GAL/OAB degrade silently; OOF must not need admin at all |
| 10 | P1 | Deployment | `GATEWAY_BIND=[::]:8134` has no IPv4 fallback — container can fail to start |
| 11 | P1 | EAS | `ItemOperations` implements only `Fetch` (`EmptyFolderContents`, `Move` → HTTP 400) |
| 12 | P1 | Edge | Cloudflare bot/WAF/security level can challenge EAS/EWS/MAPI; SRV record missing |
| 13 | P1 | Robustness | 60 s request-body timeout vs 25 MiB SendMail uploads from mobile |
| 14 | P1 | OOF | OOF fidelity: `BodyType` always 1, no HTML, `OofState=2` scheduling unverified |
| 15 | P1 | Robustness | `X-Forwarded-Proto` is a hard 400 gate with no override |
| 16 | P1 | EWS | `IncludeMimeContent` ignored (MS-OXWSMSG) — breaks "forward as attachment"/save-as |
| 17 | P1 | Discovery | Autodiscover/path-shape robustness for the two target clients |
| 18 | P1 | Limits | Attachment/body limits incoherent across gateway, compose and Stalwart |
| 19 | P2 | MAPI/HTTP | MAPI endpoint paths lack the trailing-slash tolerance IIS has |
| 20 | P2 | EAS | Path case/trailing-slash parity (`/Microsoft-Server-ActiveSync/`) |
| 21 | P2 | Config | `GATEWAY_IMAP_HOST/PORT` are dead configuration |
| 22 | P2 | Docs | Version drift across README/RUNBOOK/AGENTS/compose/health (v1.0.3, v0.16.22, "AUDIT4") |
| 23 | P2 | Docs | Tunnel docs describe local `config.yml` ingress; the deployment uses a dashboard-managed tunnel |
| 24 | P2 | Security | Log hygiene: URIs with `User=<email>` can reach logs; only `Authorization` is redacted |
| 25 | P2 | Ops | `/health/smoke` leaks a stale build tag and is not a readiness probe |
| 26 | P2 | Deployment | Compose hardening (read-only rootfs, caps, log rotation) |
| 27 | P2 | Deployment | Compose volume comment references a non-existent `user:` field |
| 28 | P2 | Features | Contact photos: `GetUserPhoto` exists, but nothing populates it (no `SetUserPhoto`, no photo import) |
| 29 | P2 | Contacts | `ContactCard` write must read back and reject silently dropped fields |
| 30 | P2 | Tasks/Notes | CalDAV-only by design — document as accepted deviation and verify collections exist |
| 31 | P2 | MAPI | `body.to_vec()` copies up to 128 MiB per request |
| 32 | P2 | MAPI | MAPI body cap (128 MiB) exceeds the Cloudflare 100 MB request limit |
| 33 | P2 | EAS | `Settings`: `DevicePassword` answers `Status 1`, `DeviceInformation` is not echoed at all |
| 34 | P2 | EAS | Provisioning policy document must not impose device-password friction |
| 35 | P2 | EWS | Send/SaveCopy duplicate-avoidance across the JMAP and SMTP paths |
| 36 | P2 | Performance | OOF/rules `block_on` a second Tokio runtime — parks a worker per call |
| 37 | P3 | Tests | Live smoke script misses every surface this deployment depends on |
| 38 | P3 | Docs | `README.md` contradicts the compose on `GATEWAY_SMTP_HOST` |

---

# Part A — Priority-ordered findings

## Tier A (P0) — the use case cannot be delivered until these are fixed

## 1. JMAP `EmailSubmission/set` omits `identityId` — every JMAP send is rejected

**Where**
* `src/jmap.rs:5286`–`5312` — the `EmailSubmission/set` create body carries
  `emailId`, `envelope`, `onSuccessUpdateEmail` and **no `identityId`**.
* `src/email.rs:1248`–`1301` — `send_email` tries JMAP first and falls back to
  SMTP on any error.

**Evidence**
* RFC 8621 §7.5 makes `identityId` a **required** argument of every
  `EmailSubmission` creation.
* Stalwart v1.1.0 enforces it:
  `crates/jmap/src/submission/set.rs:552`–`559` returns
  `SetError::invalid_properties().with_property("identityId")`
  ("emailId and identityId properties are required"), so the creation lands in
  `notCreated` and the gateway's `submit_email` returns `Err`.
* Stalwart also requires the envelope sender to **equal** the identity's
  address: `crates/jmap/src/submission/set.rs:585`–`593` answers
  `ForbiddenFrom` ("Envelope mailFrom does not match identity email address")
  and, when no envelope is supplied, defaults `mailFrom` to the identity's
  address. The identity must therefore be selected **from** the `From`
  address, not the other way round.
* Stalwart's `Identity` object (`crates/jmap-proto/src/object/identity.rs:17`)
  exposes `id`, `name`, `email`, `replyTo`, `bcc`, signatures — **no
  `allowedFrom`** (so identity selection must match on `email`).

**Impact**
Every send from both clients takes the JMAP path, fails, and silently degrades
to the SMTP path — which is itself broken (finding 2). Result: **mail cannot be
sent at all** in the shipped configuration, and even when SMTP is repaired the
JMAP-first mandate is unmet and every send pays a wasted round-trip plus a
`WARN` log.

**Fix**
1. Cache `Identity/get` per account (the identities change rarely; reuse the
   existing `get_account_id` cache pattern in `src/jmap.rs`).
2. Pick the identity whose `email` equals the envelope `From`
   (case-insensitive, `mailto:` stripped); if several, prefer the one that
   equals the authenticated user's primary address; if none matches, return a
   typed error (`FromAddressNotPermitted`) instead of falling through to SMTP,
   because SMTP will reject the same envelope anyway.
3. Put the resolved id in the `EmailSubmission` create as `"identityId"`.
4. Reuse the same identity for the `envelope.mailFrom`, so the envelope and the
   identity can never disagree (`forbiddenFrom`).

**Verify**
Live test against v1.1.0: `EmailSubmission/set` answers `created.s0` and the
message arrives in Sent; a `From` outside the identity set answers the typed
error, not a silent fallback.

---

## 2. SMTP fallback verifies TLS against `stalwart` — attachments and S/MIME cannot be sent at all

**Where**
* `docker-compose.yml:141` — `GATEWAY_SMTP_HOST=stalwart` (the bridge service
  name), `GATEWAY_SMTP_PORT=465` (implicit TLS).
* `src/smtp.rs:91`–`113` — `build_transport` calls
  `AsyncSmtpTransport::relay(&self.host)` / `starttls_relay(&self.host)`, and
  lettre derives **both** the TCP target and the TLS verification name (SNI)
  from that single string.
* `src/email.rs:3137`–`3200` — the complex-MIME path (attachments, S/MIME,
  TNEF, nested messages) *only* relays through SMTP; there is no JMAP
  equivalent.

**Evidence**
* lettre verifies the certificate against the name it derives from the relay
  host. Stalwart serves a certificate for `mail.<domain>` (ACME), never for the
  container name `stalwart`, so the handshake fails with a certificate
  name-mismatch (`InvalidCertificate(NotValidForName)`).
* `README.md:35`–`39` states the opposite requirement ("`GATEWAY_SMTP_HOST` must
  be the public MX hostname … TLS verification requires it"), so the compose
  and the README contradict each other and one of them is wrong by construction.

**Impact**
Attachments, inline images and S/MIME **cannot be sent** (no JMAP path exists
for them), and every simple send that fell back from JMAP (finding 1) fails
too. This alone blocks "email used fully".

**Fix (existing dependency — lettre already supports this)**
* Add `GATEWAY_SMTP_TLS_SERVER_NAME` (default: `GATEWAY_MAIL_HOST`) and build
  the transport with explicit TLS parameters while keeping the bridge target:
  `relay(...)`/`starttls_relay(...)` → `.port(self.port)` →
  `.tls(Tls::Wrapper(TlsParameters::new(name.into())?))` for 465 (or
  `Tls::Required(params)` for 587), using
  `lettre::transport::smtp::client::{Tls, TlsParameters}`. This is the standard
  "TLS SNI/verification override" for an internal connection whose certificate
  names the public host, and it needs **no new crate**.
* Validate at startup: if the override is empty and the SMTP host is not a
  dotted public name, log a `WARN` naming the exact risk.
* Alternative (weaker): set `GATEWAY_SMTP_HOST=mail.<domain>` and rely on
  hairpin NAT — adds a public round trip per send and breaks if the bridge
  cannot resolve the public name; do not ship this as the default.

**Verify**
`openssl s_client -starttls` equivalent in a live test: a send with one
attachment succeeds, and the gateway log shows the verified peer name
`mail.<domain>`.

---

## 3. Out-of-office is a silent no-op (Sieve id, property, account, activation, swallowed errors)

**Where**
* `src/oof.rs:492`–`522` — `get_script_blocking` sends
  `SieveScript/get {"ids": [<username>]}` and reads `entry["script"]`.
* `src/oof.rs:525`–`556` — `set_script_blocking` sends
  `SieveScript/set {"update": {<username>: {"script": …}}}`.
* `src/oof.rs:684`–`714` — the manager is built with the **admin**
  credentials and never with the mailbox owner's.
* `src/models.rs:187`–`198` — the manager exists only when both admin values
  are non-empty; `docker-compose.yml:216`–`217` ship them **empty**.
* `src/eas.rs:3331` — `let _ = oof_mgr.set_oof_settings(username, settings);`
  with the comment "Ignore result; we'll still respond success".

**Evidence (Stalwart v1.1.0)**
* `Sieve::Id = Id` (a server-assigned numeric id — `Id(u64)`, base32-encoded,
  `crates/types/src/id.rs:27`–`41`; `crates/jmap-proto/src/object/sieve.rs:228`),
  and `crates/jmap/src/sieve/get.rs:69`–`96` resolves ids as document ids. A
  username is therefore **not a valid id**: `GetRequest::unwrap_ids`
  (`crates/jmap-proto/src/method/get.rs:148`–`172`) classifies an unparseable
  id as `MaybeInvalid::Invalid`, so the answer is
  `{"list":[],"notFound":["<username>"]}` — *not* a method-level error.
  `get_sieve_script` (`src/jmap.rs:4937`) sees the username in `notFound` and
  returns `Ok(None)` ("no script" = "OOF disabled"); `set_sieve_script`
  (`src/jmap.rs:4991`) sends `{"update": {<username>: …}}`, which lands in
  `notUpdated` and returns `Err`. A method-level argument-parse failure would
  instead be `RequestMethod::invalid` → `InvalidArguments`
  (`crates/jmap-proto/src/request/parser.rs:475`).
* The script **content is a blob**: `SieveProperty` is
  `Id | Name | BlobId | IsActive` (`sieve.rs:24`), the get path emits
  `blobId` (`get.rs:117`), and the set path reads `name` + `blobId`
  (`crates/jmap/src/sieve/set.rs:637`, `:677`). There is **no `script`
  property**, so even with the right id the update would fail.
* Activation is a separate argument: `onSuccessActivateScript` /
  `onSuccessDeactivateScript` (`sieve.rs:184`–`187`), or `isActive` in an
  update (`set.rs:474`–`516`).
* `get_script_blocking`/`set_script_blocking` use the admin's credentials with
  `get_account_id(admin)`, so they address the **admin's** account, not the
  mailbox owner's (Stalwart cross-account access is ACL-based,
  `crates/common/src/auth/access_token.rs:1032`).

**Impact**
Automatic Replies never turn on and `GetUserOofSettings`/`GetMailTips` always
report "disabled" — while EAS `Settings` answers `Status 1` (success). The user
believes OOF is active; nothing is ever sent. This is the worst failure mode
(silent data-less success). Two further defects ride along, both verified:
`set_sieve_script` only ever sends `update` (never `create`), so even with a
valid numeric id it can never create the first script; and the EAS `Settings`
response renders the **Get** shape (`<Oof><Status>1</Status><Get>…`) even for a
Set request, which [MS-ASCMD] §2.2.3.122 answers as
`<Oof><Status>1</Status></Oof>` alone.

**Fix (JMAP-first, per the mandate)**
1. **Primary: `VacationResponse/set`** (RFC 8621 §8,
   `urn:ietf:params:jmap:vacationresponse`). Stalwart implements it as its own
   managed Sieve script (`crates/jmap/src/vacation/set.rs:104`–`126`), separate
   from the user's rules script, so OOF and rules cannot clobber each other.
   Fields: `isEnabled`, `fromDate`, `toDate`, `subject`, `textBody`,
   `htmlBody` (all present in
   `crates/jmap-proto/src/object/vacation_response.rs:20`). It removes the
   whole `date`/`currentdate` script-surgery surface (no scheduling bugs, no
   escaping bugs, no script parse failures).
   Read it back with `VacationResponse/get` (`singleton`), which replaces
   `parse_vacation_replies`/`parse_vacation_window`.
2. **Rules path: fix the Sieve calls** (`src/jmap.rs:4937`–`5034`):
   `SieveScript/get` with **no `ids`** and
   `properties: ["id","name","isActive","blobId"]` → choose the active script
   (create one named e.g. `exchange-gateway` when none exists); fetch content
   with `Blob/get {"ids":[blobId],"properties":["data:asText"]}`; write with
   `Blob/upload` (simple upload, `data:asText`) then
   `SieveScript/set {"update":{"<id>":{"blobId":"#b0"}},"onSuccessActivateScript":"<id>"}`
   (or create + `onSuccessActivateScript: "#s0"`). Treat `notCreated`,
   `notUpdated` **and** a method-level `error` as hard failures.
3. **Credentials**: perform these calls with the *user's* credentials on the
   user's own account (the gateway has them on every request), so no admin
   account is required for OOF or rules and no cross-account ACL question
   arises.
4. **EAS**: propagate the outcome — `Status 1` only on success, `Status 6`
   (server error) otherwise. EWS already maps errors correctly
   (`src/ews.rs:11014`), it was only ever being lied to.

**Verify**
Live: EAS `Settings` Oof Set → `VacationResponse/get` shows the values; an
inbound message receives the auto-reply; disable → replies stop. Same via EWS
`SetUserOofSettings`/`GetUserOofSettings` and Outlook's Automatic Replies
dialog.

---

## 4. Inbox rules are broken by the same Sieve API misuse

**Where**
* `src/mapi/handler.rs:6321` (`RopGetRulesTable` → `get_sieve_script`),
  `:6482` (`RopModifyRules` read), `:6597` (write).
* `src/jmap.rs:4937`–`5034` — the same username-as-id and `script`-property
  defects as finding 3 (this path does use the caller's own credentials, so the
  admin-account defect does not apply here).

**Evidence**
Same Stalwart contract as finding 3. `get_sieve_script` returns
`Err("SieveScript/get rejected: …")` for the invalid id, which
`RopGetRulesTable` converts to `RopErrorCode::DiskError`
(`src/mapi/handler.rs:6345`) — Outlook's Rules and Alerts dialog then shows a
server error; a write with the invalid id lands in `notUpdated` and
`set_sieve_script` returns `Err`.

**Impact**
Rules can never be listed, created, edited or deleted. Classic Outlook (the
declared fallback client) and New Outlook both expose Rules and Alerts; the
failure is visible but unfixable by the user.

**Fix**
Share one corrected Sieve helper (finding 3, step 2) between OOF-rules and MAPI
rules; keep the existing `parse_rules`/`render_rules` block format so the
stored script stays readable. Chunk at Stalwart's `maxSetObjects` (500) — a
rules table is far below that, so no change needed there.

**Verify**
`RopGetRulesTable` on an empty mailbox returns an empty table with
`Success`; `RopModifyRules` creates a rule that appears in the next
`RopGetRulesTable` and in `SieveScript/get`.

---

## 5. MAPI/HTTP never returns `401` — New Outlook for Windows cannot authenticate

**Where**
* `src/main.rs:468`–`560` — `mapi_http_path` decodes Basic auth and on any
  failure renders a MAPI error envelope with **HTTP 200** (or 404 when the
  surface is disabled).
* `src/mapi/handler.rs` — a failed `RopLogon` is answered as a ROP error code
  inside a 200 response.
* There is no `WWW-Authenticate` header anywhere on the MAPI paths
  (`grep` finds none).

**Evidence**
[MS-OXCMAPIHTTP] §2.2.2 (in-repo text, `exchange_protocols/[MS-OXCMAPIHTTP].txt:1073`–`1077`):
> "The server returns a '200 OK' HTTP status code even if the request failed
> for all non-exceptional failures. The server deviates from a '200 OK' HTTP
> status code **only for authentication ("401 Access Denied")**, redirection
> ("302 Redirect"), or … exceptional condition."

Outlook's HTTP stack only shows a credential prompt (and only retries with
credentials) when the origin answers `401` with a `WWW-Authenticate: Basic`
challenge. A 200 with a ROP-level logon failure is treated as a permanent
protocol error. Two facts make this worse:
`src/mapi/logon.rs:148` authenticates with `AuthVerifier::verify`
(`src/auth.rs`), which collapses the tri-state to a bool — so a **backend
outage** (`BackendUnavailable`) is answered as `AccessDenied` (a wrong
password) instead of a retryable 503; and Autodiscover advertises
`<ServerExclusiveConnect>on</ServerExclusiveConnect>`
(`src/autodiscover.rs:782`), which tells Outlook to use **only** MAPI/HTTP for
this mailbox — so a broken MAPI surface does not silently fall back to EWS.

**Impact**
New Outlook for Windows — the client this deployment exists for — cannot
complete the MAPI/HTTP connect sequence, even though Autodiscover advertises
the `mapiHttp` protocol block (`src/autodiscover.rs:781`, enabled by
`GATEWAY_MAPI_ENABLED=1` in the compose). This is very likely the reason the
use case currently "needs classic Outlook".

**Fix**
* When `Authorization` is absent or unparseable on `/mapi/emsmdb` or
  `/mapi/nspi`, answer **HTTP 401** with
  `WWW-Authenticate: Basic realm="exchange-gateway"` and an empty body — before
  any ROP parsing (MS-OXCMAPIHTTP §2.2.2, §3.2.2).
* When the credentials are present but the backend answers
  `InvalidCredentials`, answer 401 again; when the backend is **unavailable**,
  answer 503 with `Retry-After` (mirroring the EWS/EAS contract in
  `src/auth.rs`), never 401 (a 401 would make Outlook discard the password).
* Keep the existing "endpoint disabled → 404" behaviour.
* While MAPI/HTTP is incomplete, advertise
  `<ServerExclusiveConnect>off</ServerExclusiveConnect>` (or omit the element)
  so Outlook can fall back to EWS instead of failing the whole profile; turn it
  back `on` once the MAPI surface is proven.

**Verify**
`curl -i -X POST https://calendar.<domain>/mapi/emsmdb` with no auth returns
`401` + `WWW-Authenticate`; with a wrong password returns `401`; with the right
password returns a 200 `Connect` response; with Stalwart stopped returns 503.

---

## 6. `NotificationWait` is a 5-minute buffered response — Cloudflare 524, and not the spec's chunked shape

**Where**
* `src/mapi/handler.rs:423` — `NOTIFICATION_WAIT_MAX` = 5 minutes.
* The handler awaits the event and returns one `MapiResponse`.

**Evidence**
* [MS-OXCMAPIHTTP] §3.1.5.5 / §3.2.5.5 and §3.2.5.2 (in-repo text at
  `[MS-OXCMAPIHTTP].txt:5123`–`5170`, `5525`–`5580`): the server **must** answer
  long-running requests with a **chunked** response — an immediate `PROCESSING`
  chunk, `PENDING` keep-alive chunks every `X-PendingPeriod` (default 30 000 ms,
  example 15 000 ms), then a final `DONE` chunk with the body and `0`.
* Cloudflare returns **HTTP 524** when the origin sends nothing for ~100 s
  (configurable up to 125 s only on Enterprise plans).

**Impact**
Every notification long-poll dies at the edge after ~100 s. New Outlook for
Windows then sees a broken notification channel: delayed mail, "connecting…"
states, and re-poll storms. It also breaks the spec contract independently of
Cloudflare.

**Fix**
Implement the spec's streaming shape (this is the correct fix and it removes the
edge-timeout problem entirely):
1. Return `200 OK`, `Transfer-Encoding: chunked`, `Content-Type:
   application/mapi-http`, `X-PendingPeriod: 15000`, `X-RequestType:
   NotificationWait`, `X-RequestId`, `X-ResponseCode: 0`.
2. Write the `PROCESSING` chunk immediately, then a `PENDING` chunk every
   `X-PendingPeriod` while waiting (axum `Body::from_stream` +
   `tokio::time::interval`), and finally the `DONE` chunk with the real body.
3. Keep the 5-minute ceiling, but make it configurable
   (`GATEWAY_MAPI_NOTIFICATION_WAIT_SECS`, default 300) so an operator behind a
   stricter proxy can shorten it.
4. If a streaming implementation is not wanted yet, the *minimum* fix is to
   clamp the wait to ≤ 90 s **and** document it; but that violates §3.2.5.2 and
   wastes a poll per 90 s, so it should only be a stop-gap.

**Verify**
`curl -N` against `/mapi/emsmdb` with a `NotificationWait` body: chunks arrive
every 15 s and the connection survives > 100 s through Cloudflare; a mail
delivery in another client completes the wait early.

---

## Tier B (P1) — high; the use case works only partially or unreliably

## 7. Nothing is verified against Stalwart **v1.1.0** — add a live contract suite

**Where**
* `AGENTS.md:5` and `docker-compose.yml:173` assert "Stalwart v1.0.3".
* `DEPLOY_RUNBOOK.md:6`, `:61`, `:68` pin v1.0.3; `tests/jmap_calendar_deploy.rs:4`
  even says "Stalwart v0.16.22".
* `Cargo.toml:60` — `calcard = "^0.3.14"` (crates.io release) while Stalwart
  v1.1.0 vendors **`calcard 1.1.0`** (`stalwart-src/crates/calcard/Cargo.toml`,
  `publish = false`) for the same JSCalendar⇄iCalendar conversion the gateway
  duplicates in `src/jscalendar.rs`.
* Only `tests/jmap_calendar_deploy.rs` talks to a live server, and only about
  the calendar capability.

**Evidence (v1.1.0 facts the gateway does not encode today)**
* JMAP core defaults (`crates/registry/src/schema/structs_impl.rs:40845`):
  `maxRequestSize = 10 000 000`, `maxUploadSize = 50 000 000`,
  `maxGetResults = 500`, `maxSetObjects = 500`, `maxQueryResults = 5000`,
  `maxMethodCalls = 16`, `maxConcurrentRequests = 4`.
* `EmailSubmission` requires `identityId` (finding 1); `Identity` has no
  `allowedFrom` (finding 1).
* `Sieve` ids are numeric and script content is a blob (finding 3).
* `VacationResponse` supports `fromDate`/`toDate` (finding 3).
* `ContactCard` / `Contact` property names differ between RFC 9610 and the
  server in places; contacts are JMAP-first here (`GATEWAY_PREFER_JMAP_CONTACTS=1`).
* `CalendarEvent/get`'s `iCalendar` is a JSCalendar **object** (already known)
  and Stalwart writes are JSCalendar-native; the gateway's CalDAV fallback
  writes ICS.

**Impact**
Every "delivered" claim in the repository is a claim about v1.0.3. The
operator runs v1.1.0, so each contract change is a potential silent regression
(the three P0s above are exactly this class of failure: they were never
live-tested). Without a contract suite the deployment cannot be maintained.

**Fix**
1. Add `tests/stalwart_contract.rs` (gated on `STALWART_JMAP_URL`/`_USER`/`_PASS`
   like the existing deploy test) covering, at minimum: session capabilities and
  limits; `Identity/get`; `EmailSubmission/set` with and without a matching
   identity; `Email/set` + `Blob/upload` + `Email/import`; `Mailbox/get` roles;
  `CalendarEvent/get|set|query` including a recurring event with an exception;
  `ContactCard/get|set`; `SieveScript/get|set` + `Blob/upload` + activation;
  `VacationResponse/get|set`; `Email/queryChanges` semantics; `@etag` absence.
2. Re-read limits from the session (`maxObjectsInGet` is already honoured by
   `session_max_objects_in_get`, `src/jmap.rs:127`); extend that to
   `maxSizeUpload`, `maxSizeRequest`, `maxConcurrentRequests`, `maxMethodCalls`
   and gate requests against them.
3. Evaluate replacing the hand-rolled `src/jscalendar.rs` bridge with the
   backend's own `calcard` version line (crates.io `calcard 1.1.x` if published)
   so both sides convert identically — precedence rule 2 (a high-quality
   dependency already used by the backend) over hand-written conversion.

**Verify**
The suite runs green against a v1.1.0 container in CI (compose service) and
fails loudly on any contract drift.

---

## 8. No JMAP-first attachment send; size limits are not negotiated

**Where**
* `src/email.rs:3120`–`3178` — `send_eas_mime_message` relays **complex** MIME
  (attachments, S-MIME, TNEF, nested messages) through SMTP only; the simple
  branch funnels into `send_email` (`src/email.rs:1249`).
* `src/jmap.rs:5177`–`5320` — `submit_email` builds `bodyValues` from text/HTML
  only; `SubmitEmailParams` (`src/jmap.rs:736`–`752`) has **no attachment
  field**, so the structured compose path (EWS `CreateItem`+`SendItem`, MAPI
  submit) cannot carry an attachment at all. There is no `Blob/upload`, no
  `Email/import` and no `bodyStructure`/`attachments` in the submission path.
* `src/ews.rs:13123` — EWS `CreateAttachment` *is* implemented (it stages the
  blob via `attachment_manager`), so the gap is the send, not the staging.

**Evidence**
* RFC 8621 §4.6 (`Email/import`) and §4.1.4 (`bodyStructure` + `blobId`) are
  the JMAP-native ways to submit a message with attachments; `Blob/upload`
  (RFC 9404) is the transport for the raw MIME. Stalwart implements both
  (`crates/jmap/src/blob`, `crates/jmap/src/email/import.rs`).
* Stalwart caps `maxUploadSize` at 50 MB and `maxRequestSize` at 10 MB by
  default: a raw MIME must be **uploaded as a blob** and referenced, never
  inlined in the request body.

**Impact**
With finding 2 unfixed there is no send path with attachments at all; with
finding 2 fixed the path still bypasses JMAP (violating the mandate), loses the
server-side Sent copy semantics, and has no size guard — an oversized send is
rejected by Stalwart with an opaque 413/`limit` error that the gateway maps to
a generic failure.

**Fix**
1. Add a JMAP raw-MIME submission path: `Blob/upload` the exact RFC 5322 bytes
   the client supplied (EAS `SendMail` `MIMEData`, EWS `MimeContent`,
   `CreateItem` with attachments after MIME assembly), then
   `Email/import` into Drafts + `EmailSubmission/set` (`identityId` from
   finding 1, `emailId` = the imported id) + the existing
   `onSuccessUpdateEmail` move to Sent. Keep the SMTP relay as the fallback.
2. Before uploading, compare the byte length with the session's
   `maxSizeUpload` and the base64-inflated size with `maxSizeRequest`; on
   overflow answer the protocol-correct "too large" status — EAS `SendMail`
   answers a global `<Status>` (use `Status 5`, invalid content, per
   [MS-ASCMD] §2.2.3.177.8's message-too-large semantics; EWS answers
   `ErrorItemTooLarge`/`ErrorMessageSizeExceeded`) with a clear log line naming
   the limit, never a generic 500.
3. Keep the header rewrite (Bcc strip, In-Reply-To/References) that the SMTP
   path already performs — apply it to the uploaded bytes.

**Verify**
A send with a 10 MiB attachment produces exactly one `Blob/upload` +
`Email/import` + `EmailSubmission/set` in a stub trace, arrives at the
recipient, and appears once in Sent; a 60 MiB attachment fails fast with the
"too large" status.

---

## 9. Admin credentials ship empty → OOF/rules/GAL/OAB degrade silently

**Where**
* `docker-compose.yml:216`–`217` — `GATEWAY_ADMIN_USERNAME=` / `GATEWAY_ADMIN_PASSWORD=`
  (empty, with a comment saying "leave empty to fall back to the self-only
  minimal GAL stub").
* `src/models.rs:180`–`198` — the OOF manager, the directory client and the
  OAB source are all built only when both values are non-empty.
* `src/eas.rs:3331` — EAS OOF silently succeeds with no manager
  (`NullOofManager`); `src/ews.rs:11009` answers `ErrorNotSupported`.

**Impact**
With the shipped template the operator gets: no GAL beyond the user's own
entry (recipient autocomplete in New Outlook degrades to recently-used only),
no OAB, no rules, no OOF — all without any startup error.

**Fix**
* Make the pair **required** for this deployment: validate at startup and
  either refuse to start (strict) or log a single `ERROR` line that names every
  degraded surface (chosen posture: `WARN` + a `/health/ready` detail field, so
  a backend-less dev start still works).
* Document in the compose comment (and the runbook) that these are mandatory
  for OOF/rules/GAL/OAB and how to create a Stalwart admin (Settings → Account
  → role `admin`).
* Decouple OOF/rules from admin entirely (finding 3, step 3) so only
  GAL/OAB/NSPI needs the admin.

**Verify**
Start with the values empty → the log lists exactly which surfaces are
degraded; start with them set → `x:Account/query` returns the domain's accounts
and `ResolveRecipients`/NSPI autocomplete work.

---

## 10. `GATEWAY_BIND=[::]:8134` has no IPv4 fallback — the container can fail to start

**Where**
* `docker-compose.yml:52` — `GATEWAY_BIND=[::]:8134`.
* `src/main.rs:867`, `:1023`–`1026` — `TcpListener::bind(addr).await?` with no
  fallback: a failed bind aborts startup.

**Evidence**
Docker's default bridge network is IPv4-only and sets
`net.ipv6.conf.all.disable_ipv6=1` inside the container's netns in many
configurations; `bind("[::]:8134")` then fails with `EADDRNOTAVAIL`. The
published mapping `127.0.0.1:8134:8134` is IPv4, so even a successful
`V6ONLY` bind would be reachable only via dual-stack (`net.ipv6.bindv6only=0`,
the Linux default, but not guaranteed).

**Impact**
"Container starts then exits" — the hardest failure for an operator using
Portainer, with a log line (`Cannot assign requested address`) that does not
name the cause.

**Fix**
* Ship `GATEWAY_BIND=0.0.0.0:8134` in the compose (the container is reached
  over IPv4 by Docker and by cloudflared on the host loopback).
* In `serve_plain`, on `EADDRNOTAVAIL` for an IPv6 address, retry once with the
  IPv4 equivalent (`0.0.0.0:<port>`) and log a `WARN` naming the fallback.

**Verify**
Start the container with IPv6 disabled in the daemon → it binds `0.0.0.0` and
`/health/ready` answers.

---

## 11. `ItemOperations` implements only `Fetch` — `EmptyFolderContents` and `Move` are rejected with HTTP 400

**Where**
* `src/eas.rs:4173`–`4188` — `handle_item_operations` parses only `Fetch`
  blocks and returns `bad_request_response(... "ItemOperations requires at
  least one Fetch")` for anything else.

**Evidence**
[MS-ASCMD] §2.2.1.10 / §2.2.3.67 define `Fetch`, `Move` and
`EmptyFolderContents` as the ItemOperations operations; Outlook for Android's
"Empty folder" and several "move to folder" flows use `EmptyFolderContents`
and `ItemOperations/Move` (the 16.x replacement for `MoveItems`).

**Impact**
HTTP 400 on a legitimate command → the client shows a sync/move error and may
retry in a loop; "Empty folder" is impossible from the device.

**Fix**
* Parse `<EmptyFolderContents>` (`<CollectionId>`, `<Options><DeleteSubFolders/>`)
  → resolve the collection, delete the messages via the existing folder-delete
  machinery (`onDestroyRemoveEmails`) and answer per §2.2.3.177.8 (`Status 1`,
  or per-item status), not HTTP 400.
* Parse `<Move>` (`SrcMsgId`, `SrcFldId`, `DstFldId`) → reuse the `MoveItems`
  implementation and answer `Status 1`/`3`.
* For a request with no recognised operation, answer a WBXML
  `<Status>`-bearing response (protocol error `Status 2`) rather than an HTTP
  400, so the client can display something meaningful.

**Verify**
`ItemOperations` with `EmptyFolderContents` empties the folder (client shows
0 items) and with `Move` moves one message; both answer HTTP 200 + WBXML.

---

## 12. Cloudflare bot/WAF/security level can challenge EAS/EWS/MAPI; SRV record missing

**Where** — deployment, not code. See §B.1 for the exact configuration to apply.

**Evidence**
* EAS/EWS/MAPI clients are non-browser HTTP stacks: a managed challenge
  (Bot Fight Mode, "Security Level: High", "I'm Under Attack", an AI-bot
  block, or a WAF managed rule with *Managed Challenge*) answers HTML/JS or
  403, which Outlook surfaces as "server unavailable".
* This deployment cannot use the apex-domain Autodiscover fallback
  (`https://<domain>/autodiscover/autodiscover.xml`) because the apex already
  serves other content (documented in `cloudflared/config.yml`), so
  `autodiscover.<domain>` must be reachable **and** discoverable — which
  normally means the `_autodiscover._tcp` SRV record.

**Impact**
Intermittent, hard-to-diagnose failures ("works from a browser, not from the
phone"); Android's wizard may never find the autodiscover host on networks that
only use SRV.

**Fix** — see §B.1: skip rules for the two hostnames + the SRV record.

**Verify**
`curl -A "Microsoft-Outlook-Android"` (and with no UA) against
`/Microsoft-Server-ActiveSync?Cmd=OPTIONS` returns WBXML 200, never HTML/403.

---

## 13. 60 s request-body timeout vs 25 MiB SendMail uploads from mobile

**Where**
* `src/main.rs:38`–`39`, `:989`–`:991` — `RequestBodyTimeoutLayer(60 s)` applies to every
  route, including EAS `SendMail`/`SmartReply`/`SmartForward` (40 MiB ceiling,
  `src/eas.rs:58`) and EWS `CreateItem`/`SendItem` (40 MiB).

**Evidence**
40 MiB at an effective 4 Mbit/s uplink takes 84 s; Outlook for Android on
mobile networks routinely sees less. Cloudflare itself allows uploads up to
100 MB with no short read timeout, so the gateway is the only limiter.

**Impact**
A large send fails with a 408 after the client has already uploaded most of the
body — the user retries and fails again; the attachment size the compose
advertises (25 MiB) is unreachable in practice.

**Fix**
* Make the body-read timeout per-route: 60 s for control commands, 300 s for
  the MIME-carrying routes (EAS `SendMail`/`SmartReply`/`SmartForward`, EWS
  `CreateItem`/`SendItem`/`UpdateItem`, MAPI `Execute`).
* Expose it as `GATEWAY_BODY_TIMEOUT_SECS` (default 60) and
  `GATEWAY_BODY_TIMEOUT_LARGE_SECS` (default 300) and document both in the
  compose.

**Verify**
Throttle a client to 2 Mbit/s and send a 20 MiB attachment: it completes.

---

## 14. OOF fidelity: `BodyType` always 1, no HTML, `OofState=2` scheduling unverified

**Where**
* `src/eas.rs:3408`, `:3449`–`3453` — every `OofMessage` is rendered with
  `<BodyType>1</BodyType>` (plain text) regardless of the client's request.
* `src/eas.rs:3305`–`3319` — `OofState=2` (scheduled) maps to
  `start_time`/`end_time` and the OOF manager renders a Sieve `date`
  (`currentdate`) wrapper (`src/oof.rs` `build_sieve_script`).
* EAS parse side: `oof_audience_from_oof_messages` reads audiences but the
  `BodyType` of the *request* is discarded.

**Evidence**
[MS-ASCMD] §2.2.3.123 (OofMessage) carries `BodyType` 1 = text, 2 = HTML;
Outlook's Automatic Replies dialog offers rich text and expects HTML when it
sends `BodyType 2`. Stalwart's Sieve engine does support the `date` extension
(`stalwart-src/crates/sieve/src/compiler/grammar/tests/test_date.rs`), so
scheduled OOF *can* work — but with `VacationResponse/set` (finding 3) the
scheduling is expressed as `fromDate`/`toDate` and needs no script at all.

**Impact**
HTML auto-replies arrive as plain text (visible formatting loss); a scheduled
window may be silently ignored if the script wrapper is ever rejected.

**Fix**
* Store and render the per-audience `BodyType` (1 or 2) and the HTML body when
  the client sent one; render `<BodyType>2</BodyType>` and the HTML in
  `<ReplyMessage>` for those audiences.
* Prefer `VacationResponse/set` (`textBody` **and** `htmlBody`) so both
  representations exist; keep the Sieve path only if per-audience texts are
  required (Stalwart's vacation response is single-audience).

**Verify**
Enable OOF with rich text from Outlook → the auto-reply is `multipart/alternative`
with the HTML part; schedule a window → replies start/stop at the boundaries.

---

## 15. `X-Forwarded-Proto` is a hard 400 gate with no override

**Where**
* `src/eas.rs:10463` — `if !forwarded_https_enforced(&headers) { return
  bad_request_response(...) }`.
* The same helper gates EWS and the MAPI handler.

**Evidence**
The helper passes when the header is absent and fails only when it says
`http`. Cloudflare always sends `X-Forwarded-Proto: https`, so the shipped path
works — but any change of front door (a second proxy, a direct LAN test, a
different tunnel product, or Cloudflare adding a header variant) turns every
client request into a 400 with no configuration escape.

**Impact**
A single misconfiguration of the edge silently breaks all three protocols at
once; the operator has no knob.

**Fix**
* Add `GATEWAY_REQUIRE_HTTPS_HEADER` (default `1`) and
  `GATEWAY_TRUSTED_PROXY_HEADER` (default `X-Forwarded-Proto`); when disabled,
  accept any scheme (documented as "only behind a TLS-terminating proxy").
* Keep the secure default so the shipped posture is unchanged.

**Verify**
With the knob set to 0, a request with `X-Forwarded-Proto: http` is served;
with 1 it is rejected exactly as today.

---

## 16. `IncludeMimeContent` ignored (MS-OXWSMSG) — breaks "forward as attachment" and save-as

**Where**
* `src/ews.rs:239`–`245` — "`IncludeMimeContent` is allowed but will be
  ignored".

**Evidence**
[MS-OXWSMSG] §3.1.4.1 (GetItem) and the `ItemType` `MimeContent` element
require the server to include the raw MIME when the client asks for it.
Classic Outlook (the declared fallback client) requests it for "Forward as
attachment", "Save as .eml/.msg" and some print flows; New Outlook uses it for
"Download message".

**Impact**
Those actions produce a message without the original MIME (re-synthesised,
losing exact headers/encoding), or fail.

**Fix**
Serve `MimeContent` from the JMAP blob (`Email/get` `blobId` →
`download_blob_range` already exists) when `IncludeMimeContent` is present, for
`GetItem`, `SyncFolderItems` and `FindItem` (the latter two only when the
client asked). Base64-encode per the EWS `xs:base64Binary` contract.

**Verify**
`GetItem` with `<t:IncludeMimeContent>true</t:IncludeMimeContent>` returns a
`<t:MimeContent>` whose bytes equal the original message.

---

## 17. Autodiscover and path-shape robustness for the two target clients

**Where**
* `src/main.rs:898`–`970` — routes are registered only in the exact case and
  without trailing slashes: `/Microsoft-Server-ActiveSync`,
  `/EWS/Exchange.asmx`, `/EWS/{*path}`, `/mapi/emsmdb`, `/mapi/nspi`,
  `/autodiscover/autodiscover.xml` (+ `/Autodiscover/...`), `.../autodiscover.svc`,
  `.../autodiscover.json`, `.../autodiscover.json/v1.0/{email}`.
* `src/autodiscover.rs:779`–`797` — the `mapiHttp` block advertises
  `<Server>{host}</Server>` (bare hostname), `<ServerExclusiveConnect>on</ServerExclusiveConnect>`,
  `<ExternalUrl>https://{host}/mapi/emsmdb?MailboxId={email}</ExternalUrl>` and
  the matching `/mapi/nspi` address-book URLs.

**Evidence**
* IIS (the real Exchange front end) is case-insensitive and tolerates a
  trailing slash; Android's Exchange wizard and Outlook's AutoDiscover client
  probe several spellings, including
  `POST /autodiscover/autodiscover.json/v1.0/<email>` (V2 JSON) and
  `GET /autodiscover/autodiscover.xml?Email=...`.
* [MS-OXCMAPIHTTP] §2.2.1.1 prints the example URL **with** a trailing slash
  (`/mapi/emsmdb/?MailboxId=…`), so a client may append it.

**Impact**
A client that appends `/` or differs in case gets a 404 and falls back to a
less capable protocol (or fails the account setup).

**Fix**
* Register the trailing-slash variants (`/Microsoft-Server-ActiveSync/`,
  `/mapi/emsmdb/`, `/mapi/nspi/`, `/EWS/Exchange.asmx/`).
* Add a normalising middleware that lowercases the path for the known protocol
  prefixes only (never for `/OAB/{guid}/{file}`, which is case-sensitive by
  contract) — or register the mixed-case variants explicitly.
* Add `POST /autodiscover/autodiscover.json` (some clients POST the V2 JSON
  request) and verify the `mobilesync` response points at
  `https://<GATEWAY_HOST>/Microsoft-Server-ActiveSync`.

**Verify**
A matrix test over the spellings above returns the same response as the
canonical one; Android's wizard completes with the SRV record present and
absent.

---

## 18. Attachment/body limits are incoherent across gateway, compose and Stalwart

**Where**
* `src/eas.rs:52` `MAX_BODY_SIZE` = 1 MiB (non-MIME commands),
  `:58` `MAX_MIMEDATA_BODY_SIZE` = 40 MiB; `src/main.rs:38`
  `MAX_BODY_BYTES` = 40 MiB; `src/mapi/transport.rs:28`
  `MAX_MAPI_BODY_BYTES` = 128 MiB.
* `docker-compose.yml:108` — `GATEWAY_MAX_ATTACHMENT_BYTES=26214400` (25 MiB).
* Stalwart: `maxUploadSize = 50 MB`, `maxRequestSize = 10 MB`.

**Evidence**
Base64 inflates by 4/3: a 25 MiB attachment is ~33.4 MiB of `MIMEData`
(fits 40 MiB), but the code's own cap is 50 MiB of attachment (≈67 MiB of
`MIMEData` — rejected by `MAX_MIMEDATA_BODY_SIZE`), and the MAPI cap (128 MiB)
exceeds Cloudflare's 100 MB request limit, so the edge rejects before the
gateway can answer properly.

**Impact**
The documented knob can be set to a value the code silently refuses; oversized
requests fail at the edge with an HTML error instead of a protocol status.

**Fix**
* Derive every ceiling from one knob:
  `MAX_MIMEDATA_BODY_SIZE = ceil(max_attachment_bytes * 4 / 3) + 1 MiB` and
  `MAX_BODY_BYTES` likewise, computed at startup (clap/serde default cannot
  express it; compute in `Config::finalize`).
* Clamp `MAX_MAPI_BODY_BYTES` to ≤ 90 MB (or make it configurable) and document
  the Cloudflare 100 MB ceiling in the compose.
* Validate against the session's `maxSizeUpload`/`maxSizeRequest` at startup and
  log the effective maximum.

**Verify**
Set `GATEWAY_MAX_ATTACHMENT_BYTES=50 MiB` → the effective `MIMEData` ceiling
becomes ≈ 68 MiB and a 45 MiB attachment send reaches Stalwart (or fails with a
clear "backend upload limit" status, never an edge HTML error).

---

## Tier C (P2) — medium; correctness/robustness gaps that surface in daily use

## 19. MAPI endpoint paths lack IIS trailing-slash tolerance

**Where** — `src/autodiscover.rs:779` advertises
`<ExternalUrl>https://<host>/mapi/emsmdb?MailboxId=<email></ExternalUrl>` (no
trailing slash); `src/main.rs:916`–`925` registers `/mapi/emsmdb` and
`/mapi/nspi` only.

**Evidence** — [MS-OXCMAPIHTTP] §2.2.1.1 prints `/mapi/emsmdb/?MailboxId=…`.
Real Exchange accepts both spellings; a client that appends `/` (or a proxy
that does) gets 404 and MAPI/HTTP is silently abandoned in favour of EWS. The
gateway's own advertisement omits the slash, so its own discovery round-trip is
self-consistent — this is a tolerance hardening, not a live break.

**Fix** — register both spellings; keep the advertised form without a slash (it
matches what the gateway serves).

**Verify** — `POST /mapi/emsmdb/` and `/mapi/emsmdb` behave identically.

---

## 20. EAS path case and trailing-slash parity

**Where** — `src/main.rs:906` — only `/Microsoft-Server-ActiveSync`.

**Evidence** — IIS paths are case-insensitive; some clients/proxies normalise
the path or append `/`. A 404 on the EAS endpoint makes the client fall back to
EWS (New Outlook) or fail account setup (Android).

**Fix** — add `/Microsoft-Server-ActiveSync/` and a case-insensitive match for
the exact literal (a small `tower` middleware comparing the lowercased path
against a fixed allow-list, then rewriting to the canonical form).

**Verify** — `/microsoft-server-activesync?Cmd=OPTIONS` returns WBXML 200.

---

## 21. `GATEWAY_IMAP_HOST` / `GATEWAY_IMAP_PORT` are dead configuration

**Where** — `src/config.rs:139`–`141`, `:957`–`964` define and parse them;
nothing else in the crate reads them (`grep imap_host` shows only config).
`docker-compose.yml:143`–`144` set them with a comment implying the gateway
connects over IMAP.

**Impact** — the operator tunes a knob that has no effect; the compose comment
is misleading (the gateway never speaks IMAP; Autodiscover merely *advertises*
IMAP to clients).

**Fix** — remove both keys and the compose lines, or keep them but mark them
"advertised only, unused" and stop parsing them.

**Verify** — `cargo clippy` shows no dead-code allowance needed after removal;
the compose has no unused variable.

---

## 22. Version drift across docs, compose and health endpoints

**Where** — `AGENTS.md:5`, `docker-compose.yml:173`, `README.md:4`,
`DEPLOY_RUNBOOK.md:6/61/68` ("v1.0.3"; the runbook even instructs pinning
`stalwartlabs/stalwart:v1.0.3@sha256:…`, while the operator's live container is
`stalwartlabs/stalwart:latest` = v1.1.0); `tests/jmap_calendar_deploy.rs:4`
("v0.16.22"); `src/main.rs:891` (`/health/smoke` returns `"build":"AUDIT4"`).

**Impact** — an operator reading the docs believes the backend contract was
verified against their version; the smoke tag suggests a build identity that
does not exist.

**Fix** — replace every version pin with "Stalwart Mailserver v1.1.0" and cite
the contract suite from finding 7 as the source of truth; make `/health/smoke`
report the crate version (`env!("CARGO_PKG_VERSION")`) instead of a literal.

**Verify** — `grep -rn "v1.0.3\|v0.16" --include=*.md --include=*.yml .` returns
nothing.

---

## 23. Tunnel docs describe local `config.yml` ingress, but the deployment uses a dashboard-managed tunnel

**Where** — `cloudflared/config.yml` (full ingress file),
`CLOUDFLARED_SETUP.md` ("run `cloudflared tunnel run --config …`"),
`docker-compose.yml:24`–`27` comments.

**Evidence** — the operator's Cloudflare Zero Trust routes are configured in
the **dashboard** (`calendar.<domain>/*`, `autodiscover.<domain>/*` → port
8134). A dashboard-managed tunnel is started with a **token**
(`cloudflared service install <TOKEN>`); the local `config.yml` ingress is then
not used, and shipping a competing ingress file invites an operator to run a
second, differently-configured connector.

**Impact** — operator confusion and two competing ingress definitions (the
template itself is a catch-all `hostname: ${GATEWAY_HOST}` with an optional
path list, so it does not *omit* any gateway path — the problem is purely that
the operator is told to use a file their tunnel never reads).

**Fix** — restate the tunnel section around the dashboard-managed model:
`cloudflared service install <TOKEN>`, no local `config.yml`, routes already
present; keep the file only as an *optional* reference (clearly labelled
"ignored when the tunnel is token-managed"). This is what the operator asked
for ("this may not be required in any documentation"). Add one DNS item to the
runbook: `autodiscover.<domain>` needs a **proxied A/AAAA record** (the tunnel
only routes hostnames that exist in DNS); `calendar.<domain>` already has one.

**Verify** — following the runbook verbatim on a host with a token-managed
tunnel yields a working EAS `OPTIONS` without editing any local file.

---

## 24. Log hygiene: request URIs can carry mailbox addresses

**Where** — `src/main.rs:988` — `TraceLayer::new_for_http()`;
`SetSensitiveRequestHeadersLayer([AUTHORIZATION])` (`:984`–`:986`) redacts only
the `Authorization` header.

**Evidence** — EAS is a query-string protocol:
`/Microsoft-Server-ActiveSync?Cmd=FolderSync&User=alice%40example.com&DeviceId=…`.
`TraceLayer` emits the full URI at `DEBUG`/`TRACE`; the operator's compose sets
`GATEWAY_LOG_LEVEL=info`, but a temporary `debug` for troubleshooting writes
mailbox addresses and device ids to the Portainer log (which is often retained
and shared).

**Fix** — wrap `TraceLayer` with a `make_span_with` that logs the path only
(never the query) at `INFO`+ and redacts `User`/`DeviceId` at `DEBUG`; or add a
`GATEWAY_LOG_REDACT_QUERY=1` default. Keep the `Authorization` redaction.

**Verify** — a `debug`-level run of a FolderSync produces no email address in
the log.

---


## 25. `/health/smoke` leaks a stale build tag

**Where** — `src/main.rs:888`–`896`.

**Impact** — cosmetic/ops confusion; also an unauthenticated endpoint that
looks like a health probe but touches nothing.

**Fix** — remove it, or make it report `CARGO_PKG_VERSION` + git sha and mark it
`no-store`.

---

## 26. Compose hardening

**Where** — `docker-compose.yml` service block.

**Evidence** — the image runs as UID 10001 and writes only to
`/var/lib/exchange-gateway`, so a read-only root filesystem is feasible; the
gateway needs no Linux capabilities.

**Fix** — add `read_only: true`, `security_opt: ["no-new-privileges:true"]`,
`cap_drop: ["ALL"]`, `tmpfs: ["/tmp"]`, and a `logging:` block with
`max-size`/`max-file` so Portainer logs cannot fill the disk. All are inline
compose keys (no env file), consistent with the deployment rules.

**Verify** — the container starts, `/health/ready` is 200, a send + sync works,
and `docker inspect` shows `ReadonlyRootfs: true`.

---

## 27. Compose volume comment references a non-existent `user:` field

**Where** — `docker-compose.yml:243`–`247` — "must be writable by UID 10001; …
the container runs as user 10001, see `user:`", but there is no `user:` key.

**Impact** — an operator searching for the referenced key concludes the file is
incomplete (or adds `user: 10001:10001`, which is harmless but redundant).

**Fix** — reword to "the image sets `USER gateway` (UID 10001); `chown 10001`
the host directory", or add the explicit `user: "10001:10001"` key.

---

## 28. Contact photos — `GetUserPhoto` exists but has no population path

**Where** — `src/ews.rs:12004`–`12090` `handle_get_user_photo` (MS-OXWSPHOTO
§3.1.4.1: `SizeRequested`, `ErrorInvalidSmtpAddress`, `HasChanged` +
base64 `PictureData`, deliberately disclosure-free), backed by
`storage::user_photo`; `src/ews.rs` has **no** `SetUserPhoto`.

**Evidence** — `GetUserPhoto` itself is compliant and routed
(`src/ews.rs:407`, `:546`), so the gap is the photo *source*, in three places:
(a) no write path — there is no `SetUserPhoto`, and nothing imports a
CardDAV/JMAP `photo`, so `storage::user_photo` is only ever filled by an
operator/seed; (b) the MAPI store exposes no `PR_ATTACHMENT_CONTACTPHOTO`
stream; (c) the EWS `Contact` item does not render its `<t:Picture>` element,
so a photo stored in Stalwart never reaches the contact card in either
client.

**Impact** — New Outlook and Outlook Android show monogram avatars. Cosmetic,
but it is part of "natively, perfectly"; the `GetUserPhoto` half of the
protocol is already compliant.

**Fix** — 1) populate `user_photo` from the Stalwart contact/principal photo
(JMAP `ContactCard` `media`/`photo` blob via `JmapClient::download_blob_range`)
on first `GetUserPhoto` for an address that has a contact with a photo, and on
contact create/update; 2) add `SetUserPhoto` → store locally (Stalwart has no
avatar object); 3) render `<t:Picture>` on the EWS `Contact` shape and expose
the same bytes as `PR_ATTACHMENT_CONTACTPHOTO` for MAPI.

**Verify** — a contact with a photo shows it in both clients after a sync; a
photo set through `SetUserPhoto` survives a gateway restart.

---

## 29. `ContactCard` writes must read back and reject silently dropped fields

**Where** — `src/contacts.rs` (JMAP path), `src/jmap.rs` `get_contact_card` /
`create_contact_jmap` / `update_contact_jmap`.

**Evidence** — RFC 9610 is newer than Stalwart's implementation; property names
(`emails/*/address`, `phones/*/number`, `name/full`, `organizations`,
`addresses`, `media`) and the `@type` discriminator must match what the server
accepts. An unknown property is silently dropped by Stalwart, which yields a
"successful" create with missing fields. The one structural requirement is
already met — a `ContactCard` must carry `addressBookIds`, and both write paths
set it (`src/contacts.rs:1488` on create, `:1583` preserves the server's map on
update) — so the remaining risk is exactly that silent drop.

**Impact** — contacts created/edited in Outlook lose fields silently.

**Fix** — read back every created/updated card and compare the round-tripped
fields with what was sent; on divergence fail the operation (same discipline
already applied to calendar writes). Cover it in the contract suite. Do **not**
add a blanket `addressBookIds` rewrite: the update path already preserves the
server's map.

**Verify** — create a contact with name/emails/phones/address/notes/birthday →
read back → every field present.

---

## 30. Tasks/Notes are CalDAV-only — document as accepted deviation, verify collections

**Where** — `src/tasks.rs` (VTODO/VJOURNAL over CalDAV),
`docker-compose.yml:174`–`178`.

**Evidence** — Stalwart v1.1.0 has no JMAP Task/Note object type, so
CalDAV/VTODO+VJOURNAL is the only backend; the JMAP-first mandate cannot be
met here.

**Impact** — none functionally, but the operator must know that Tasks/Notes
depend on the CalDAV path and on the two per-user collections existing.

**Fix** — keep the current design (already documented) and add to the runbook:
the gateway MKCALENDARs `/Tasks/` and `/Notes/` on first use, and the operator
should confirm both collections appear in Stalwart's CalDAV listing.

**Verify** — create a task in Outlook Android → a VTODO appears in the `Tasks`
collection; complete it → `STATUS:COMPLETED` + `COMPLETED:` set.

---


## 31. MAPI request bodies are copied (`body.to_vec()`)

**Where** — `src/main.rs:506` — `parse_request(&headers, body.to_vec(), enabled)`
copies the entire body (up to 128 MiB) before the size check inside
`parse_request` can reject it.

**Impact** — an extra full-size allocation and copy per MAPI request, on the
hottest large-payload path. This directly contradicts the performance mandate
(minimise allocations/copies).

**Fix** — change `parse_request` to take `bytes::Bytes` (the type axum already
hands out) and slice it; keep the existing `MAX_MAPI_BODY_BYTES` check. No
behaviour change.

**Verify** — `benches/` shows no regression and the copy disappears from the
profile.

---

## 32. MAPI body cap (128 MiB) exceeds the Cloudflare 100 MB request limit

**Where** — `src/mapi/transport.rs:28`.

**Impact** — a request between 100 MB and 128 MB is rejected by Cloudflare with
an HTML 413 before the gateway can answer a MAPI error; the client shows a
transport failure.

**Fix** — clamp to ≤ 90 MB (configurable), and document the edge limit in the
compose next to `GATEWAY_MAX_ATTACHMENT_BYTES`.

---

## 33. `Settings`: `DeviceInformation` is never echoed (`DevicePassword` must stay `Status 1`)

**Where** — `src/eas.rs` `handle_settings`: the `DevicePassword` branch answers
`<Status>1</Status>` (`DevicePassword` response is a status-only document), and
the `DeviceInformation` branch is not rendered at all — the section is simply
absent from the response.

**Impact** — `DevicePassword` already answers `Status 1`, so it needs no work.
The **omitted** `DeviceInformation` response is the conformance gap
([MS-ASCMD] §2.2.1.13 / §2.2.3.31: the response echoes `<Set><Status>` per
requested element); Outlook Android sends `DeviceInformation` at first sync and
tolerates a missing section, but a client that requires the echo can loop. (The Provision policy is
all-off, so accepting-and-ignoring is the correct behaviour.)

**Fix** — render `<DeviceInformation><Status>1</Status></DeviceInformation>`
when the request carried a `Set`, and assert in a test that both sections answer
`Status 1` and that no `WWW-Authenticate`/142 is emitted
(`GATEWAY_EAS_REQUIRE_POLICY_KEY=0`).

---

## 34. Provisioning policy document must not impose device-password friction

**Where** — `src/eas.rs:1676`–`1717` (`EASProvisionDoc`).

**Impact** — even with `GATEWAY_EAS_REQUIRE_POLICY_KEY=0`, the *policy document*
itself is consumed by the client: if it requests a device password, encryption
or an alphanumeric PIN, Android/Outlook will enforce it locally (or refuse).
Every such bit is already `0` in the template, so the documented posture
holds; what is missing is a test that would catch a future regression and
device-level evidence that neither client enforces anything.

**Fix** — keep every `DevicePasswordEnabled`/`AlphanumericDevicePasswordRequired`/
`RequireStorageCardEncryption` bit at 0 and pin it with a test that decodes the
WBXML `Provision` response and asserts the all-off matrix for both clients.

---

## 35. Send/SaveCopy duplicate avoidance across the JMAP and SMTP paths

**Where** — `src/email.rs:1248`–`1301` (JMAP → SMTP fallback),
`src/email.rs:3137`+ (SMTP relay + `import` into Sent).

**Evidence** — the JMAP path creates a Draft and moves it to Sent on success;
the SMTP path sends the raw message and then imports a copy. If a send falls
back after the JMAP `EmailSubmission/set` was actually accepted (only the
`SubmissionUnconfirmed` case is currently guarded), the message can be sent
twice or appear twice in Sent.

**Fix** — deduplicate by `Message-ID` when importing the Sent copy (skip the
import if a message with that id already exists in Sent), and extend the
`SubmissionUnconfirmed` guard to cover the "Email/set created but submission
not confirmed" case explicitly.

**Verify** — a forced SMTP fallback after a confirmed JMAP submission produces
exactly one Sent copy and one delivery.

---

## 36. OOF/rules blocking calls park a Tokio worker thread

**Where** — `src/oof.rs:486`–`490` builds a **second** multi-threaded
`Runtime` (`Runtime::new()`), and `get_script_blocking`/`set_script_blocking`
(`:492`–`:556`) call `rt.block_on(...)` from inside the async EAS/EWS handler.

**Evidence** — `OofManager` is a *sync* trait (`src/oof.rs:70`–`94`), so the
async handlers must bridge. `block_on` on a *different* runtime is legal but
blocks the calling Tokio worker for the whole JMAP round trip (handshake +
`SieveScript/get` + `SieveScript/set` + blob upload once finding 3 is fixed) —
and it also allocates a whole runtime (thread pool) at startup. This directly
contradicts the performance mandate ("low latency, high throughput, no
unnecessary blocking"), and under a burst of OOF toggles it can starve the
worker pool that serves EAS `Sync`/Ping.

**Fix** — make the trait async (`async fn get/set_oof_settings`) and `await`
the existing `JmapClient` calls directly; delete the second runtime. Where a
sync caller genuinely exists, use `tokio::task::spawn_blocking` +
`Handle::block_on` rather than a private runtime. Precedence: rule 3 (std/Rust
API) before rule 4.

**Verify** — a test that toggles OOF 50 times concurrently while a Ping holds
its heartbeat completes with no worker starvation, and the process creates
exactly one Tokio runtime.

---

## Tier D (P3) — polish and documentation

## 37. The live smoke script does not exercise the surfaces this deployment depends on

**Where** — `tests/OUTLOOK_CLOUDFLARE_SMOKE.md` and
`tests/outlook_cloudflare_smoke.sh` (OPTIONS, FolderSync, Autodiscover,
GetFolder, GetUserAvailability, optional calendar CRUD).

**Evidence** — the script takes `GATEWAY_BASE_URL` from the environment and
refuses to run while the value still contains `example.com`, so it is
correctly targeted; the gap is coverage. It never exercises EAS `Sync`/`Ping`,
send mail, OOF, `ItemOperations`, MAPI/HTTP, contacts, tasks or
`/health/ready` — the exact surfaces findings 1–6 concern, so the one script an
operator would run against the live deployment cannot detect any of them.

**Impact** — the one script an operator would run against the real deployment
cannot detect any of the P0 defects in this document.

**Fix** — add probes for: EAS `Sync` prime + delta, `SendMail` (to a local
recipient), `Settings` OOF set/get/disable, `ItemOperations` Fetch, EWS
`CreateItem`+`SendItem`, `GetUserOofSettings`, `ResolveNames`, `/health/live`
and `/health/ready`, MAPI `Connect` (expect 401 then 200 once finding 5 lands),
and a `NotificationWait` chunked read (finding 6).

## 38. `README.md` contradicts the compose on `GATEWAY_SMTP_HOST`

**Where** — `README.md:35`–`39` vs `docker-compose.yml:141`.

**Impact** — an operator following the README sets the public MX host (breaking
the bridge path) or following the compose keeps the container name (breaking
TLS) — both are wrong today (finding 2).

**Fix** — after finding 2 lands, document the final contract in both places:
"connect to `stalwart:465`, verify as `GATEWAY_SMTP_TLS_SERVER_NAME`
(default `GATEWAY_MAIL_HOST`)".

---

# Part B — Changes required outside this repository

## B.1 Cloudflare (Zero Trust routes, DNS, zone settings)

The two existing routes are correct in shape; the following must be checked or
added. None of these requires a repository change beyond documentation.

1. **Keep** `calendar.<domain>/*` and `autodiscover.<domain>/*` → service
   `http://<ubuntu-LAN-IP>:8134` (plain HTTP origin — the gateway serves HTTP
   by design; `noTLSVerify` is irrelevant for an `http://` service).
2. **Do not add** a root-domain (`<domain>`) route: the apex has its own
   A/AAAA content and MS-OXDSCLI's apex probe is expected to fail and fall
   through to `autodiscover.<domain>`.
3. **Turn off challenge-type protections for these two hostnames.** In
   *Security → WAF* and *Security → Bots*:
   * Bot Fight Mode / "Block AI bots" → **Off** for the zone (or scoped off by
     a Configuration Rule for `calendar.*` and `autodiscover.*`);
   * Security Level → **Essentially Off** for these two hostnames (a
     Configuration Rule), or a WAF custom rule with action **Skip** covering
     *Managed Rules*, *Bot Fight Mode*, *Security Level*, *Rate Limiting* and
     *Browser Integrity Check* for `http.host in {"calendar.<domain>"
     "autodiscover.<domain>"}`.
   Rationale: EAS/EWS/MAPI are non-browser stacks; any JS/managed challenge or
   BIC block answers HTML/403 and Outlook reports "cannot connect".
4. **No caching**: ensure no "Cache Everything"/"Cache Rules" apply to those
   two hostnames (POSTs are not cached by default; a page rule could change
   that).
5. **Do not enable** "Always Use HTTPS" *redirects* on these hostnames in a way
   that answers 301 for POST: Cloudflare's "Always Use HTTPS" converts POST to
   GET for 301/302; use 307/308 semantics if such a rule is enabled, or leave
   the setting off (the tunnel is already TLS-only from the client side).
6. **Add the Autodiscover SRV record** (DNS, not a tunnel route):
   `_autodiscover._tcp.<domain>. 3600 IN SRV 0 0 443 autodiscover.<domain>.`
   This makes discovery work for clients that never try the apex.
7. **Response timeout**: no plan change is needed once finding 6 is
   implemented (chunked keep-alives). If the code fix is deferred, the only
   edge-side mitigation is the Enterprise "Proxy Read Timeout" (max 125 s) —
   still insufficient, so treat the code fix as mandatory.
8. **Request body limit**: the Free/Pro 100 MB edge limit is above every
   gateway cap once finding 32 is applied. Do not raise
   `GATEWAY_MAX_ATTACHMENT_BYTES` beyond ~65 MiB without also raising
   Stalwart's `maxUploadSize`.
9. **Do not** put the gateway behind Cloudflare Access (a Zero Trust
   *application* policy): Access intercepts the request with a login page that
   Outlook cannot complete. The tunnel routes must remain plain (no Access
   application attached).
10. **Minimum TLS**: 1.2 is sufficient for both target clients; no client
    certificate requirement (mTLS) on these hostnames.

## B.2 Stalwart Mailserver v1.1.0

1. **HTTP listener reachable from the gateway container** on the shared bridge
   network (`my-bridge-network`), port 8080, serving JMAP (`/jmap`), CalDAV
   (`/dav/cal`) and CardDAV (`/dav/card`). The gateway uses plain HTTP there
   (`GATEWAY_ALLOW_INSECURE_HTTP=1`); TLS is terminated by Cloudflare.
2. **Calendar scheduling (iTip) enabled** — this is the single most important
   backend setting for "calendar used fully". Stalwart's registry object
   `CalendarScheduling` (`crates/registry/src/schema/structs.rs:917`) must have
   `enable = true` (send invitations/responses for CalDAV/CalDAV-scheduling
   writes), and `autoAddInvitations = true` is recommended so an attendee's
   copy exists even if the gateway's copy write fails. `maxItipSize` must be
   large enough for your largest invitation (default is generous).
   Without `enable`, an event created by the user never reaches the attendees.
3. **A sending identity per user** whose `email` equals the user's address
   (and, for aliases, an identity or `allowedFrom` entry covering each alias).
   `EmailSubmission/set` requires `identityId` and rejects a `From` outside the
   identity set (`forbiddenFrom`).
4. **JMAP limits**: `maxUploadSize` ≥ the largest MIME you intend to send
   (default 50 MB is fine for 25 MiB attachments); raise `maxRequestSize`
   (default 10 MB) **only if** the gateway ever inlines a large body — with
   finding 8 (blob upload) it does not need to. Keep `maxGetResults` ≥ 500
   (default) and `maxConcurrentRequests` ≥ 4 if you run several devices.
5. **Sieve enabled** for the account (the script engine is on by default) —
   needed only for MAPI rules after finding 3/4; OOF should use
   `VacationResponse`, which is on by default.
6. **SMTP submission with a valid certificate** for `mail.<domain>` on port
   465 (implicit TLS) — the gateway connects on the bridge but verifies against
   `mail.<domain>` after finding 2. Ensure the ACME certificate covers
   `mail.<domain>` and that the chain is complete.
7. **Admin/service account** for GAL/NSPI/OAB (`GATEWAY_ADMIN_USERNAME`/
   `PASSWORD`): a Stalwart account with permission to query
   `x:Account/query` for the domain. Required for recipient autocomplete in
   New Outlook and for the OAB.
8. **Do not rate-limit the gateway's source IP below its fan-out** (JMAP
   `maxConcurrentRequests` = 4 per account; the gateway also opens SMTP/CalDAV
   connections). Stalwart's default per-connection rate (2000/60 s) is ample.
9. **`STALWART_PUBLIC_URL`** must be the real public URL (used in generated
   links, sharing and iTip RSVP links) — keep it as configured.
10. **Optional**: enable `httpRsvpEnable` if you want attendees to respond via
    a web link in addition to iMIP; not required for Outlook.

---

# Part C — Deployment-scenario conformance (compose / Portainer)

Checked against the operator's stated constraints:

| Constraint | Status |
|---|---|
| Image pulled from a registry (build out of scope) | OK — `image: <registry>/exchange-gateway:0.1.0`, no `build:` key |
| No env file, all variables inline | OK — no `env_file:`, no `.env*` in the repo, every knob inline |
| Volume `/armblock/exchange-gateway-data` | OK — mounted at `/var/lib/exchange-gateway` (UID 10001 must own it) |
| Portainer-deployable | OK — no `depends_on`, no `build`, no `configs`/`secrets` files |
| Shared network with Stalwart | OK — external `my-bridge-network` |
| Serves HTTP (TLS at the edge) | OK — `127.0.0.1:8134:8134`, plain HTTP origin |
| Placeholders preserved | OK — `example.com`, `mail.example.com`, `calendar.example.com`, `REPLACE_…` untouched |
| `GATEWAY_BIND` | **Change to `0.0.0.0:8134`** (finding 10) |
| `GATEWAY_SMTP_HOST=stalwart` | **Correct once finding 2 lands** (add the TLS-name override) |
| `GATEWAY_ADMIN_USERNAME/PASSWORD` empty | **Must be filled** for GAL/OAB/rules/OOF (finding 9) |
| Healthcheck binary | OK — `curl` is in the runtime image (`Dockerfile:36`) |
| Compose hardening / log rotation | Missing (finding 26) |

Two operational notes that are *not* code changes but decide success:

* **Volume ownership**: `chown 10001:10001 /armblock/exchange-gateway-data`
  before the first start, otherwise SQLite cannot create `gateway.db`.
* **Restart semantics**: `restart: unless-stopped` does not restart an
  *unhealthy* container. Either accept "marked unhealthy, not restarted" or
  enable the commented-out `autoheal` service.

---

# Part D — Verification plan (prove the use case end to end)

Run in this order; each step must pass before the next.

1. **Backend contract suite** (finding 7) against the v1.1.0 container: green.
2. **Autodiscover**: `curl -s 'https://autodiscover.<domain>/autodiscover/autodiscover.xml?Email=user@<domain>'`
   → XML with EAS/EWS/MAPI/OAB URLs on `calendar.<domain>`.
3. **EAS bootstrap**: `OPTIONS`, `Provision`, `FolderSync` 0 → 1, `Sync` 0 →
   prime for Email/Calendar/Contacts, then a delta after a change made in
   Stalwart's web UI.
4. **EAS push**: `Ping` held ~80 s through Cloudflare, returns `Status 2` when
   a mail arrives in another client.
5. **Send**: plain send, send with a 10 MiB attachment, reply with quoting,
   `SmartForward`; verify one delivery and one Sent copy each (finding 35).
6. **OOF**: enable/disable via EAS `Settings` and EWS `SetUserOofSettings`;
   verify an auto-reply arrives with the right body type (finding 14).
7. **Rules**: create a rule in classic Outlook, verify it appears in
   `SieveScript/get` and filters mail (finding 4).
8. **Calendar**: create/update/delete a single event, a recurring event with
   one modified instance, and an invitation to a second mailbox (attendee
   receives an iMIP mail and the event lands in their calendar — this is the
   `itip` setting in §B.2 item 2).
9. **Tasks/Notes**: create/complete a task, create a note; verify VTODO and
   VJOURNAL exist in Stalwart.
10. **MAPI/HTTP** (New Outlook for Windows): add the account, verify the
    `Connect` sequence (401 → 200), notification delivery, folder sync, send.
11. **Android wizard**: add as Exchange account on a clean device with the SRV
    record present; verify mail/calendar/contacts appear without any manual
    server entry.
12. **Edge health**: 24 h soak with both clients; assert no 5xx, no 524, no
    429, and a stable Ping/Sync cadence in the gateway metrics.

---

# Part E — Verified as correct (no action needed)

These were checked explicitly and are *not* gaps, so they are not numbered:

* **Capability strings** — `urn:ietf:params:jmap:{core,mail,submission,calendars,contacts,sieve,vacationresponse}`
  and `urn:stalwart:jmap` all still exist in v1.1.0
  (`crates/jmap-proto/src/request/capability.rs:65`–`105`).
* **`x:Account/query`** — still a valid registry method in v1.1.0
  (`ObjectType::Account`, `crates/registry/src/schema/properties.rs:193`;
  generic `x:` dispatch at `crates/jmap-proto/src/request/method.rs:371`).
* **`maxObjectsInGet` handling** — `session_max_objects_in_get` +
  `chunk_ids` (`src/jmap.rs:64`, `:127`) already honour the server value.
* **Calendar query paging** — `PRIME_QUERY_PAGE_SIZE = 100`
  (`src/sync.rs:1742`) is below `maxGetResults`/`maxQueryResults`.
* **`RequestTimeoutLayer`** — `src/main.rs:989` wraps only the request **body
  read**, not the response; EAS Ping and EWS streaming are therefore not cut by
  it (the 60 s value is still too short for large uploads — finding 13).
* **WAL/busy-timeout pooling** — `Storage::new` applies the pragmas per
  connection (`src/storage.rs`), so Ping's read-only tick loop cannot block on
  the writer.
* **Auth tri-state** — a backend outage is answered 503 (+`Retry-After`), never
  401, on EAS/EWS (`src/auth.rs`), which is what stops Outlook from
  re-prompting for a password.
* **WBXML byte-array/OPAQUE inventory, fail-closed decode, namespace rules** —
  covered by the conformance suite and consistent with
  `exchange_protocols/[MS-ASWBXML].txt`.
* **`<Range>`/`<Total>`/multipart ItemOperations Fetch** — matches
  [MS-ASCMD] §2.2.3.67.1/§2.2.3.177.8 and [MS-ASHTTP] §2.2.1.10.1.1.
* **No `.env` file** exists in the repository (deployment rule satisfied).
* **Dockerfile** — non-root UID 10001, `curl` present for the healthcheck,
  `--locked` release build, no toolchain in the runtime image.
* **EAS Ping heartbeat clamp** — `effective_ping_heartbeat`
  (`src/eas.rs:2443`) already returns `min(client heartbeat, configured cap)`
  (60 s minimum, 3540 s maximum) and defaults the cap to 80 s for
  Cloudflare-fronted requests (`src/config.rs:65`), so the hold can never reach
  the ~100 s origin timeout. No change required.
* **JMAP calendar writes are JSCalendar-native** — `ics_to_jscalendar_event` +
  `jscalendar_write_payload` (`src/jscalendar.rs:83`, `:146`) emit the server's
  native object (never an ICS string), and `set_calendar_event`
  (`src/jmap.rs:7340`) classifies `payloadDiscarded` vs mismatch on read-back.
  No change required; prove it in the contract suite (finding 7).
* **`ContactCard` structural requirements** — `addressBookIds` is set on create
  (`src/contacts.rs:1488`) and preserved on update (`:1583`); see finding 29.
* **EWS `GetUserPhoto`** — implemented and routed (`src/ews.rs:407`, `:546`,
  `:12004`), MS-OXWSPHOTO-shaped and disclosure-free; only the photo *source*
  is missing (finding 28).
* **`ItemOperations` Fetch wire shape** — response `<Fetch>` without `<Store>`,
  `Status` first, AirSync-namespaced `Class`/`CollectionId`/`ServerId`,
  `Search:LongId` echo, authoritative `<Range>` + `<Total>`; matches
  [MS-ASCMD] §2.2.3.67.1 / §2.2.3.177.8.
* **`EmailSubmission` contract in Stalwart v1.1.0** — re-read in the source:
  `emailId` + `identityId` are required and the envelope `mailFrom` must equal
  the identity's address (`crates/jmap/src/submission/set.rs:552`, `:585`).

---

# Appendix A — Stalwart v1.1.0 facts this audit relies on

| Fact | Source (stalwart-src @ e7cde9b3) |
|---|---|
| `EmailSubmission` requires `emailId` **and** `identityId`; envelope `mailFrom` must equal the identity address | `crates/jmap/src/submission/set.rs:552`, `:585` |
| `Identity` = `id,name,email,replyTo,bcc,signatures,mayDelete` (no `allowedFrom`) | `crates/jmap-proto/src/object/identity.rs:17` |
| `Sieve` id is a numeric `Id`; content is `blobId`; activation via `onSuccessActivateScript` | `crates/jmap-proto/src/object/sieve.rs:24`, `:184`, `:228`; `crates/jmap/src/sieve/get.rs:69`–`117`; `crates/jmap/src/sieve/set.rs:637`, `:677` |
| `VacationResponse` has `isEnabled/fromDate/toDate/subject/textBody/htmlBody` and is stored as its own managed script | `crates/jmap-proto/src/object/vacation_response.rs:20`; `crates/jmap/src/vacation/set.rs:104` |
| JMAP defaults: request 10 MB, upload 50 MB, get 500, set 500, query 5000, methodCalls 16, concurrent 4 | `crates/registry/src/schema/structs_impl.rs:40845`–`40865` |
| Capability strings (`…:sieve`, `…:contacts`, `…:calendars`, `…:vacationresponse`, `urn:stalwart:jmap`) | `crates/jmap-proto/src/request/capability.rs:65`–`105` |
| `x:<Object>/<fn>` generic registry dispatch | `crates/jmap-proto/src/request/method.rs:371` |
| `CalendarScheduling.enable` / `autoAddInvitations` gate iTip | `crates/registry/src/schema/structs.rs:917`; `crates/common/src/config/groupware.rs:47`, `:158` |
| MAPI/HTTP answers 401 **only** for authentication | `exchange_protocols/[MS-OXCMAPIHTTP].txt:1073` |
| Chunked `PROCESSING`/`PENDING`/`DONE` + `X-PendingPeriod` for long requests | `exchange_protocols/[MS-OXCMAPIHTTP].txt:5123`, `:5336`, `:5525`, `:1363` |
| Stalwart vendors `calcard 1.1.0` (unpublished) for JSCalendar⇄iCalendar | `crates/calcard/Cargo.toml` |
| Sieve `date` extension (for scheduled OOF scripts) is supported | `crates/sieve/src/compiler/grammar/tests/test_date.rs` |

---

*End of audit. Findings 1–6 must be closed before this deployment can be
considered usable; 7–18 and 36 before it can be considered reliable; 19–36 are
required for correctness and maintainability in daily use; 37–38 are polish.*
