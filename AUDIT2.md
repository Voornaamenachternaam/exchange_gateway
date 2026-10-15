# AUDIT2.md — Independent Audit: Outstanding Issues for the Target Use-Case

**Method.** This is a fresh, code-level audit of the repository at its current
working state (`main` + uncommitted `src/eas.rs`, `src/email.rs`,
`src/jmap.rs`, `src/sync.rs`). `AUDIT.md` was **not** trusted; every claim below
was re-derived from source, the in-repo specs (`exchange_protocols/*.txt`),
`docker-compose.yml`, `Dockerfile`, `cloudflared/config.yml`, `CLOUDFLARED_SETUP.md`
and `.env.example`. Where `AUDIT.md` already records a delivered fix, that is
noted and the item is re-scoped to what is *still* missing.

**Verification performed in this session (toolchain Rust 1.99.0):**
- `cargo build --release --locked` — clean (warning-free).
- `cargo test` — **1104 lib + 7 + 2 + 22 + 11 + 1 doc = 1147 green, 0 failed.**
- `cargo clippy --all-targets` — **0 warnings.**
- `rustfmt --edition 2024 --check` on every touched file — clean.
- `docker compose config` — renders, 40 inline `GATEWAY_*` vars, no `.env`.
- `docker build` — image builds; the container starts, listens on 8134, serves
  `/health/live` 200 and `/health/ready` 503 with no backend (correct).
- Deployment artifacts read line-by-line; env-knob parity computed by
  diffing `ENV_*` constants in `src/config.rs` against `docker-compose.yml`.

**Target restated.** A single gateway container, run beside
`stalwartlabs/stalwart` (the user's compose runs `:latest`, currently
v0.16.26), exposing Stalwart's mail + calendar + contacts to:
- New Outlook for Windows (October 7 2026 build),
- Outlook Android (5.2639.2),

added on Android as an "Exchange" account, behind cloudflared (gateway serves
plain HTTP). JMAP must be preferred wherever Stalwart supports it. No `.env`
file is allowed — all configuration travels in `docker-compose.yml`
`environment:`.

**Priority key:** `P0` blocks the use-case; `P1` correctness/completeness gaps
the two named clients will hit; `P2` robustness/operations; `P3` polish.

---

## P0 — Blocks the use-case outright

### 1. Deployment/compose must be rewritten for the real target (version pin, `:latest` drift, no `.env` file, registry image)
**Status: DELIVERED THIS SESSION.**

`docker-compose.yml` is now a **pull-only, fully-inline, self-contained
artifact**:
- `build:` removed; `image: <registry>/exchange-gateway:0.1.0` with the local
  build recipe kept as a comment for development.
- **40** environment variables written **inline** under `environment:` — no
  `${VAR:-default}` interpolation, no `.env`, no `env_file:` (verified by
  parsing the YAML).
- Literal `dns: - redacted` placeholders replaced with real resolvers
  (`1.1.1.1` / `8.8.8.8`) plus a note to delete the block to inherit the
  daemon's resolvers.
- References the operator's existing external `my-bridge-network` and the
  `stalwart` service by name.
- Healthcheck upgraded to `/health/ready`; the commented `autoheal` service
  documents that Compose alone never restarts an *unhealthy* container.
- Header instructs the operator to pin **their** Stalwart to the v0.16.26
  digest (the gateway cannot pin a service it does not define).

**Still open:** the `image:` tag is a placeholder (`<registry>/…`); it must be
replaced once the registry is chosen (#6).

### 2. `.env.example` exists but the shipped contract is still env-file-shaped; first-boot template must move into the compose
**Status: DELIVERED THIS SESSION.**

`.env.example` is now a **reference document**, not a file to copy: its header
states plainly that all configuration lives inline in `docker-compose.yml`,
that there is no `.env`/`env_file:`, and that the authoritative values are the
ones in the compose. The env-parity gaps named in the first pass are closed —
`docker-compose.yml` now sets `GATEWAY_MAX_PING_HEARTBEAT`,
`GATEWAY_RATE_LIMIT_*`, `GATEWAY_EMAIL_ENABLED`, `GATEWAY_MAPI_*` (incl.
`_HMA_ENABLED`/`_ORG`/`_OIDC_*`/`_CONTENTS_PAGE_SIZE`/`_MAX_CONTENTS_ROWS`),
`GATEWAY_FORCE_CALDAV_CALENDAR`, `GATEWAY_PREFER_CALDAV_FREEBUSY`,
`GATEWAY_SERVER_VERSION`, `GATEWAY_SERVER_EXCHANGE_VERSION` and
`GATEWAY_CALENDAR_CAPABILITY_CHECK`, and `.env.example` documents each one.

### 3. Cloudflare Ping timeout — implemented, but **not wired into the shipped compose and not documented in the runbook**
**Status: DELIVERED THIS SESSION.**

`docker-compose.yml` now sets `GATEWAY_MAX_PING_HEARTBEAT=80` explicitly, and
both `.env.example` and `DEPLOY_RUNBOOK.md` state the Cloudflare 125s Proxy
Read Timeout assumption and that `CF-RAY` auto-detection is the fallback when
the knob is unset. The code (`effective_ping_heartbeat`, `via_cloudflare`) was
already correct and tested.

### 4. Rate limiter — per-IP keying delivered, but defaults will still throttle the two clients behind Cloudflare
**Status: DELIVERED THIS SESSION (sized + wired).**

The shipped compose now sets `GATEWAY_RATE_LIMIT_ENABLED=1`,
`GATEWAY_RATE_LIMIT_REQUESTS_PER_MINUTE=600` and
`GATEWAY_RATE_LIMIT_MAX_CONCURRENT=2000`, with a comment explaining the
"several devices behind one Microsoft egress IP" sizing rationale, so the
operator can tune it without editing Rust defaults. The code-level two-tier
(pre-auth IP bucket + post-auth per-user bucket) refinement remains a possible
future improvement but is not required at this sizing.

### 5. Stalwart version not pinned / backend capability contract unverified against v0.16.26
**Status: PARTIALLY DELIVERED.**

`GATEWAY_CALENDAR_CAPABILITY_CHECK=warn` is now set in the shipped compose, so
the startup capability verification always runs and logs findings against the
actual backend. The compose header and `DEPLOY_RUNBOOK.md` instruct the operator
to pin **their** Stalwart service to the v0.16.26 digest (the gateway compose
cannot pin a service it does not define). Stale in-code version comments
(`v0.16.20`, `v0.16.10`) were updated to v0.16.26 (see #29).

**Still open:** nothing in the repo can pin a service defined in the operator's
separate Stalwart compose; the runbook step is the enforceable control.

### 6. Gateway image publishing pipeline vs. registry pull
**Status: PARTIALLY DONE (unchanged).**

`.github/workflows/docker-image.yml` publishes on release and the shipped
compose now pulls instead of building. Verify the published image is
**multi-arch** (`linux/amd64` + `linux/arm64`) so `docker compose pull` works on
the operator's Ubuntu host regardless of architecture, and replace the
`<registry>` placeholder in `docker-compose.yml`.

### 7. Autodiscover DNS/tunnel prerequisites for Android signup
**Status: DELIVERED THIS SESSION.**

`CLOUDFLARED_SETUP.md` defines the three hostnames and
`cloudflared/config.yml` implements them. This session added:
- `DEPLOY_RUNBOOK.md`, which lists the DNS records explicitly, states that the
  `autodiscover` record is **required** (Android's wizard probes it first), and
  gives a verification command.
- A **startup warning** when `GATEWAY_HOST` is still a placeholder
  (`config::is_placeholder_host`, 2 tests; wired in `main.rs`), so a silent DNS
  misconfiguration becomes an actionable log line.
- A **smoke-test guard** (`tests/outlook_cloudflare_smoke.sh`) that refuses to
  run against a placeholder `GATEWAY_BASE_URL` and probes that
  `autodiscover.<mail-domain>` resolves before the Autodiscover assertions.
- The cloudflared template's substitution recipe was corrected to read the
  hostnames from `docker-compose.yml` instead of a (non-existent) `.env`.


---

## P1 — Correctness/completeness the two named clients will hit

### 8. Email-class Sync client mutations — **just fixed in this session; needs a regression guard**
**Status: DELIVERED THIS SESSION.**

`apply_email_client_mutations` (src/eas.rs:6470) + `apply_eas_email_mutations` /
`parse_eas_email_flag_patch` / `render_eas_email_mutation_responses` (src/email.rs)
now apply flag/read `<Change>` and `<Delete>` before rendering the delta and
splice `<Responses>` after `<Status>` ([MS-ASCMD] §6.46). Two real defects were
found and fixed while writing the tests:
- **`Email/set` update shape**: the patch was keyed by the *field* name at the
  top level (`{"keywords/$seen": …}`) instead of by *email id*
  (`{id: {"keywords/$seen": …}}`) — RFC 8621 §5.3. This silently failed every
  flag/read mutation.
- **`updated` parsing**: `parse_email_set_outcome` (src/jmap.rs:5340) read
  `updated` as a JSON **array**; RFC 8621 §5.3 defines it as a **map** of
  id→null, which is what Stalwart returns. Fixed with an array fallback for
  lenient servers; the unit test was corrected to the map form.
Remaining work: the `<Responses>` splice uses a literal `</Status>` anchor
(src/email.rs `inject_collection_responses`) — safe today because the gateway
generates the document, but it should be a structured insert at the render site
so a future `<Status>` inside `<Commands>` cannot misplace the block.

### 9. `<Responses>` for Email/Calendar/Contacts/Tasks mutations — **verified present in all four classes**
**Status: DELIVERED; ONE ROBUSTNESS NIT.**

Re-checked against source, contrary to the first pass: every mutating class
emits the block.
- Email: `render_eas_email_mutation_responses` spliced by
  `email::inject_collection_responses` (src/eas.rs:5115).
- Calendar: `sync::render_client_mutation_responses` passed as
  `client_mutation_responses` and rendered after `<Status>` in **both**
  `perform_sync` paths (src/sync.rs:1344 empty-commands early return, and
  src/sync.rs:1836 delta path).
- Contacts: `render_contacts_mutation_responses`, placed after `MoreAvailable`
  (src/eas.rs:5446).
- Tasks/Notes: `tasks::render_mutation_responses` (src/eas.rs:5736/5786).
Ordering matches [MS-ASCMD] §2.2.3.154 (a child of `<Collection>`, before
`<Commands>`), and §2.2.3.154's "present only if the server processed an
operation" is honored by each renderer returning `""` on an empty result.

**Remaining nit:** the Email splice anchors on a literal `</Status>` in
`inject_collection_responses`. It is safe today because the gateway generates
the document and the anchor is the Collection-level Status, but a structured
insert at the render site (as Calendar/Contacts/Tasks already do) removes the
fragility. Low priority.

### 10. Provision policy / policy-key enforcement for Android device-admin consent
**Status: DELIVERED — permissive posture documented + opt-in enforcement knob.**

`eas_provision_doc_xml` (src/eas.rs) returns a coherent all-off policy
(`DevicePasswordEnabled=0`, `RequireDeviceEncryption=0`, `AllowSimpleDevicePassword=1`,
`MaxInactivityTimeDeviceLock=9999`). The two-phase PolicyKey flow
(src/eas.rs, `provision_state` table) is implemented.

[MS-ASPROV] §4.1.1 Phase 1 specifies the enforcement answer as **HTTP 200 with
global Status 142** (449 is the *Provision-required* HTTP status some clients
use, and the gateway already returns the policy on `X-MS-PolicyKey: 0`).
`get_provision_policy` is consulted inside `handle_provision`; non-Provision
commands are **not** gated by default. This is a **deliberate permissive
posture** and is correct for this use-case: no real device policy is enforced,
so refusing a client for a stale key would add friction with zero security
benefit.

This session added:
- `GATEWAY_EAS_REQUIRE_POLICY_KEY` (default `0`), wired through `config.rs`
  (env + default + startup), `docker-compose.yml` and `DEPLOY_RUNBOOK.md`. When
  set to `1`, `policy_key_accepted` (src/eas.rs) answers any non-`Provision`
  command whose `X-MS-PolicyKey` does not match the stored key with HTTP 200 +
  global Status 142. `Provision`/`OPTIONS` are always allowed so the client can
  bootstrap. Tests: `test_eas_require_policy_key_env_override` (config) and
  `test_policy_key_accepted_gate_matrix` (eas).
- `DEPLOY_RUNBOOK.md` now states (a) Android will show a device-admin grant
  screen because the gateway advertises a policy, (b) the advertised policy is a
  no-op, and (c) **no remote wipe is possible**.

### 11. `Search` — Mailbox and GAL only; verify the two clients' exact search shapes
**Status: PARTIAL.**

`handle_search` (src/eas.rs:4596) serves `Mailbox` (JMAP query) and `GAL`
(directory), returns status 11 for `DocumentLibrary`. Outlook Android's search
sends `Search` with `<Store><Name>Mailbox</Name><Query><FreeText>`; New Outlook
Windows uses `Search` + `ItemOperations` fetch of results. Confirm the
`<Options><Range>` / `MaxItems` echo and the `Search:LongId` round-trip
(`ItemOperations` Fetch by `LongId`, per the §12 notes in AGENTS.md) work for a
*search-result* id, not just a mailbox id.

### 12. EWS `MoveItem` for calendar items — **implemented; the doc comment is stale**
**Status: DELIVERED (documentation nit only).**

Correction to the first pass (which trusted the TODO comment at
`src/ews.rs:7239`): the calendar branch of `handle_move_item` is **fully
implemented**, not a stub.
- JMAP-backed items (`resource_href` starts with `jmap://`) with
  `prefer_jmap_calendar` are treated as a no-op move (a single exposed calendar,
  so a move within it changes nothing) and return a `ModifiedEvent` with a
  recomputed ChangeKey.
- CalDAV-backed items run a real `CaldavClient::move_event`, returning the new
  ETag as the ChangeKey and publishing a `ModifiedEvent`.
- Destination validation returns `ErrorInvalidOperation` for a non-calendar
  folder and `ErrorItemNotFound` for a missing source.

The only genuine limitation is **single-calendar scope** (destination collection
is forced to the source collection), which is correct for this use-case.

**Remaining action:** delete or fix the misleading `TODO: not fully implemented,
returns success for now` doc comment on `handle_move_item`.

### 13. EWS `ResolveNames` does not expand distribution lists
**Status: NOT DONE (explicit TODO).**

`src/ews.rs:8077`. New Outlook's address book will not resolve DLs. Low
frequency but a visible gap; JMAP has no native DL primitive on Stalwart, so
this needs a directory-side expansion or a documented limitation.

### 14. `ItemOperations`/Fetch attachment correctness
**Status: DELIVERED (per AGENTS.md §12), re-verified.**

Range windows, the full §2.2.3.177.8 status table, `MS-ASAcceptMultiPart`
multipart delivery, the §2.2.3.67.1 response-Fetch shape, and the cumulative
per-request budget are present. Only open thread: confirm the streaming
`download_blob_range` total/EOF fixes (PR #1980) are in the committed tree —
they are in this working tree (src/jmap.rs), so this is a **commit-hygiene**
item, not a code gap.

### 15. WBXML conformance
**Status: DELIVERED (per AGENTS.md §11), re-verified by the green conformance suite.**

Full-table diff vs `[MS-ASWBXML].txt` (608 tokens both directions),
byte-array→OPAQUE inventory, §3 byte-exact example, fail-closed decode/encode.
No outstanding action.

### 16. Timezone fidelity
**Status: DELIVERED (per AGENTS.md §14), re-verified.**

`zone_transitions` is the single authoritative source feeding the EAS TZI blob,
EWS `StartTimeZone`/`EndTimeZone`, and the synthesised VTIMEZONE; 139-zone
round-trip pinned. No outstanding action.

### 17. Tasks/Notes CalDAV mirroring
**Status: DELIVERED (per AGENTS.md §15), re-verified.**

VTODO/VJOURNAL write-through, reconcile-on-Sync, Ping backend probe, fail-closed
routing, migration. No outstanding action.

---

## P2 — Robustness / operations

### 18. Watermark pruning vs. long-offline devices
**Status: DELIVERED (per AGENTS.md §16), re-verified.** No outstanding action.

### 19. SQLite concurrency
**Status: DELIVERED (per AGENTS.md §19), re-verified.** No outstanding action.

### 20. Health readiness vs. liveness
**Status: DELIVERED (per AGENTS.md §20), re-verified.** No outstanding action.

### 21. Auth negative-caching, circuit breaker, and shared session-auth cache
**Status: DELIVERED (per AGENTS.md §17/§18), re-verified.** No outstanding action.

### 22. Secrets hygiene
**Status: DELIVERED THIS SESSION (`_FILE` injection); SafeDebug pass still open.**

`config.rs` now supports `<VAR>_FILE` companions for the three
credential-bearing variables — `GATEWAY_HMAC_SECRET`, `GATEWAY_ADMIN_USERNAME`,
`GATEWAY_ADMIN_PASSWORD` — via `get_secret_env` / `read_env_file`. A mounted
secret file takes precedence over the inline value, one trailing newline is
trimmed, and an unreadable path is logged (path only, never contents) and falls
back to the inline value so a misconfigured mount never bricks startup. Three
unit tests pin the precedence, the inline fallback, and the unreadable-file
fallback. `CLOUDFLARED_SETUP.md` and `DEPLOY_RUNBOOK.md` document the Docker
secrets form.

**Still open:** the SafeDebug coverage pass over request structs carrying the
EAS `Authorization` header — verify no `Debug` impl can log the Basic payload.

### 23. Surface gating (MAPI/HTTP, NSPI, OAB, ECP, full EWS)
**Status: DELIBERATELY KEPT ON (policy decided this session); EWS narrowing optional.**

The first pass proposed shipping with MAPI/NSPI/OAB/ECP off. On reflection this
**contradicts the stated requirement** ("exhaustively, perfectly implement all
relevant Microsoft Exchange protocols") and the in-code default
(`mapi_enabled` defaults to `true`, src/config.rs:409). The shipped compose
therefore sets `GATEWAY_MAPI_ENABLED=1` (matching the code) and documents how to
turn it off for a minimal-surface deployment. Rationale:
- New Outlook for Windows can fall back to MAPI/HTTP for some operations, so
  disabling it risks a client-visible gap in the exact target client.
- The gateway sits behind Cloudflare + Basic auth, so the marginal surface is
  small; the *correctness* risk of disabling a path the target client might use
  is larger than the *security* risk of leaving it on.

`GATEWAY_MAPI_HMA_ENABLED=0` is the one exception (HMA/OIDC hybrid auth is an
Entra-specific flow with no relevance here; the code already defaults it false).

**Still open (optional):** narrowing EWS to the operations Outlook actually uses
is a code change in `src/main.rs`; not required for correctness.

### 24. Metrics/observability
**Status: PARTIAL (unchanged).**

Prometheus is wired (`src/metrics.rs`, `/metrics` unmetered). Per-EAS-command
latency histograms and a Ping wait distribution, with a check that no label
carries username/device-id (cardinality + privacy), remain to be added.

### 25. Response compression / Ping flushing
**Status: NEEDS VERIFICATION (unchanged).**

Confirm no `tower-http` buffering layer holds the Ping interim/`Status 1`
response, and that `Content-Length` vs chunked WBXML streams flush promptly
behind Cloudflare.

### 26. Log redaction / query-string credentials
**Status: VERIFIED — no action needed.**

Checked this session: the gateway reads credentials **only** from the
`Authorization` header (`parse_basic_auth`, src/eas.rs:604); the legacy
`?User=&Password=` query-string mode is not parsed anywhere, so a client that
tries it simply fails authentication rather than leaking the password into the
request line. The only request logger is `TraceLayer::new_for_http()`
(src/main.rs:873), whose default `OnRequest` emits the method/URI at **DEBUG**
level; with the shipped `GATEWAY_LOG_LEVEL=info` the query string is not logged
at all, and even at debug the EAS query string carries `User`/`DeviceId`/`Cmd`,
never a password.

### 27. Dependency currency & supply chain
**Status: PARTIALLY DELIVERED.**

The Dockerfile build step is now `cargo build --release --locked`, so the
published image pins the dependency graph to `Cargo.lock` (matching the earlier
`cargo fetch --locked`). **Still open:** add `cargo-deny`/`cargo-audit` to CI,
and consider narrowing the permissive `^` ranges in `Cargo.toml`.

### 28. Documentation runbook for the exact target
**Status: DELIVERED THIS SESSION.**

`DEPLOY_RUNBOOK.md` was added: it maps mailbox domain → DNS records (with the
required `autodiscover` record) → cloudflared ingress → Android "add Exchange
account" (including the device-admin consent screen and the no-wipe posture) →
New Outlook for Windows (including the Microsoft-cloud sync path caveat) → a
verification command → a troubleshooting table keyed on symptoms.

---

## P3 — Polish / hardening

### 29. Stale version comments
**Status: DELIVERED THIS SESSION.** `src/carddav.rs` and the `src/config.rs`
comments now reference Stalwart v0.16.26 (the target); `docker-compose.yml` was
rewritten with no version string baked in (it instructs the operator to pin
their Stalwart service to the v0.16.26 digest).

### 30. `render_calendar_app_data` test-only attribute
`src/sync.rs` marks the legacy renderer `#[cfg(test)]`; the build is clean, so
no non-test caller remains. Consider deleting it if the legacy path is truly
dead.

### 31. `dns:` placeholders in compose
**Status: DELIVERED THIS SESSION.** The `dns:` block now carries real resolvers
(`1.1.1.1` / `8.8.8.8`) with a comment explaining how to remove it.

---

## Verification matrix (this session)

| Check | Command | Result |
|---|---|---|
| Lib tests | `cargo test --lib` | **1104 passed / 0 failed** |
| Integration | `cargo test` (bins) | 7 + 2 + 22 + 11 + 1 doc passed |
| Lints | `cargo clippy --all-targets` | **0 warnings** |
| Format | `rustfmt --edition 2024 --check` (touched files) | clean |
| Release build | `cargo build --release --locked` | clean (7m12s) |
| Compose render | `docker compose config` | exit 0, 40 env vars |
| Image build | `docker build -t exchange-gateway:test .` | exit 0 |
| Container start | `docker run …` (no backend) | listens on 8134, readiness 503, `/health/live` 200 |
| Placeholder warning | `GATEWAY_HOST=calendar.example.com` | WARN emitted at startup |

## Highest-leverage first moves
1. **#6** — replace the `<registry>` placeholder in `docker-compose.yml` and
   confirm the published image is multi-arch; the code and the deployment
   artifact are otherwise ready.
2. **#23 (EWS narrowing)** — narrow EWS to the operations Outlook needs.
3. **#7 (startup host check)** — DONE this session (placeholder warning); the
   remaining work is the smoke-test Autodiscover assertion.
4. **#22 (SafeDebug) / #24–#26** — the verification/observability pass.

## Delivered in this session (commit-hygiene pending)
- **Deployment artifact rewritten** for the exact target: pull-only, 40 inline
  env vars, no `.env`/`env_file:`, real resolvers, `my-bridge-network`, pinned
  Stalwart guidance, `/health/ready` healthcheck, optional autoheal block.
- `GATEWAY_MAX_PING_HEARTBEAT=80`, sized rate limiter, surface gating
  (`GATEWAY_MAPI_ENABLED=1` to match the exhaustive-protocol requirement) and
  `GATEWAY_CALENDAR_CAPABILITY_CHECK=warn` wired into the shipped compose.
- `.env.example` repurposed as a reference document; `DEPLOY_RUNBOOK.md` added.
- `_FILE`-based secret injection (`get_secret_env`/`read_env_file`) for
  `GATEWAY_HMAC_SECRET` / `GATEWAY_ADMIN_USERNAME` / `GATEWAY_ADMIN_PASSWORD`,
  with 3 tests.
- Startup warning when `GATEWAY_HOST` is still a template placeholder
  (`is_placeholder_host`, 2 tests).
- `cargo build --release --locked` in the Dockerfile; `.dockerignore` added.
- Stale Stalwart version comments (`v0.16.10`, `v0.16.20`) reconciled to
  v0.16.26.
- Email-class Sync client mutations (flag/read `<Change>`, `<Delete>`) now apply
  before the delta and splice `<Responses>` ([MS-ASCMD] §6.46); two real bugs
  fixed (`Email/set` patch keyed by email id per RFC 8621 §5.3; `updated` parsed
  as a **map** per §5.3), with 7 end-to-end stub-server tests.
- Suite: **1104 lib + 43 integration green, 0 failed**, clippy 0 warnings,
  release build warning-free, image builds and runs.
