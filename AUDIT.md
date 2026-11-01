# AUDIT.md — Exchange Gateway Outstanding Issues

Exhaustive audit of the exchange_gateway codebase against the specific use-case:

> Act, in a container, to enable calendar and email of an existing Stalwart
> Mailserver v1.0.1 account to be used full, natively, perfectly, without
> client-side extensions in Outlook-client on Windows 11 as well as Android,
> by adding the Stalwart Mailserver-account via Android settings as an
> 'Exchange'-account. The exchange_gateway container runs alongside Stalwart
> Mailserver v1.0.1.

## Scope & Constraints

- Target clients ONLY: New Outlook for Windows (20251205004.10 / Oct-2026 build), Outlook Android (5.2641.1)
- EAS v16.1 only; no older EAS compatibility required
- Backend: Stalwart Mailserver v1.0.1 (JMAP-first, CalDAV fallback for calendar)
- Serve HTTP only (cloudflared terminates TLS on the Ubuntu host)
- No `.env` file; all configuration inline in `docker-compose.yml`
- Image pulled from a container registry; Dockerfile and docker-compose.yml must be provided
- cloudflared routes already configured: `https://stalwart.redacted.com/admin`, `https://calendar.redacted.com/*`, `https://autodiscover.redacted.com/*` → port 8134
- JMAP (as implemented by Stalwart v1.0.1) preferred over any other standard/protocol wherever feasible

## Code Priority for Resolving Gaps

1. Existing dependencies
2. New high-quality dependencies
3. Rust 1.99.1 std API
4. Custom code

---

## Outstanding Issues (Priority Ordered)

### P0 — Deployment-blocking (must be resolved before the container can serve clients)

**1. docker-compose.yml uses placeholder image reference**
- `image: <registry>/exchange-gateway:0.1.0` is a literal placeholder; the operator must replace `<registry>` with their actual registry host/path before Portainer can pull the image.
- No documented placeholder-substitution procedure exists for the registry portion (unlike hostnames which are flagged by `is_placeholder_host` at startup).
- Fix: Replace with a documented, concrete multi-arch image reference. Add a comment noting the operator must substitute their registry. Do NOT use `.env` or `env_file`.

**2. docker-compose.yml uses placeholder hostnames/secrets**
- `GATEWAY_HOST=calendar.example.com`, `GATEWAY_MAIL_DOMAIN=example.com`, `GATEWAY_HMAC_SECRET=REPLACE_WITH_A_RANDOM_32_CHAR_MINIMUM_SECRET` are all literal placeholders.
- `GATEWAY_ADMIN_USERNAME=` and `GATEWAY_ADMIN_PASSWORD=` are empty (no admin account → no full GAL/NSPI/OAB, no directory-backed display-name resolution in Autodiscover).
- `GATEWAY_CALDAV_BASE=http://stalwart:8080/dav`, `GATEWAY_JMAP_BASE=http://stalwart:8080/jmap`, `GATEWAY_CARDDAV_BASE=http://stalwart:8080/carddav` assume a service named `stalwart` on the same Docker network — correct for the deployment scenario but must be verified against the actual Stalwart container name (`stalwart`) and network (`my-bridge-network`).
- Fix: Operator must fill in real values. The compose file must document each placeholder clearly (it already does via comments), but `GATEWAY_HMAC_SECRET` must not be left as `REPLACE_*` — `Config::validate()` rejects it at startup.

**3. docker-compose.yml port mapping does not match cloudflared route targets**
- The compose exposes `8134:8134` on the gateway container.
- The user's cloudflared routes point `calendar.redacted.com/*` and `autodiscover.redacted.com/*` to port 8134 on the Ubuntu host.
- This is consistent, but the compose's `GATEWAY_BIND=[::]:8134` binds IPv6 only — cloudflared on the host connects to `localhost:8134` (IPv4). The container's `[::]:8134` should accept IPv4-mapped IPv6 on Linux, but this is not guaranteed in all container runtimes.
- Fix: Change `GATEWAY_BIND` to `0.0.0.0:8134` or `[::]:8134` with dual-stack confirmation. Document that cloudflared's `service: http://localhost:8134` must match the container's published port.

**4. No `.env.example` deletion enforcement**
- The user's requirement: "If this exists in the repository, delete it." `.env.example` exists and documents env vars but states it is NOT a file to copy. The deployment contract forbids env files.
- Fix: Delete `.env.example` from the repository (or keep it as documentation only but clearly state it must not be used). Per the user's instruction, delete it.

**5. Stalwart Mailserver container configuration gaps (operator-side)**
- The supplied Stalwart compose uses `image: stalwartlabs/stalwart:latest` — must be pinned to `stalwartlabs/stalwart:v1.0.1` (the gateway was validated against v1.0.1). `:latest` will drift past the validated version.
- Stalwart exposes ports 8080 (HTTP CalDAV/CardDAV/JMAP), 8133, 25, 465, 587, 993, 4190. The gateway needs `8080` (JMAP/CalDAV/CardDAV) reachable over the private bridge network as `http://stalwart:8080`.
- `STALWART_PUBLIC_URL=https://stalwart.redacted.com/` is set — this is the Stalwart webui URL, not the gateway's. No conflict, but the gateway's `GATEWAY_HOST` must be `calendar.redacted.com` (matching the cloudflared route).
- No JMAP Calendars capability is explicitly enabled on Stalwart — the gateway does deploy-time verification (`GATEWAY_CALENDAR_CAPABILITY_CHECK=warn` by default) and falls back to CalDAV if JMAP calendars are unavailable.
- Fix: Operator must (a) pin Stalwart to v1.0.1 digest, (b) ensure JMAP is enabled on Stalwart (it is by default), (c) ensure the `calendar.redacted.com` and `autodiscover.redacted.com` cloudflared routes forward to the gateway container on port 8134 (already configured per the user's statement), (d) optionally set `GATEWAY_CALENDAR_CAPABILITY_CHECK=warn` to detect JMAP calendar support at startup.

**6. Cloudflare Zero Trust route additions required**
- Existing routes: `https://stalwart.redacted.com/admin` (Stalwart webui), `https://calendar.redacted.com/*` → port 8134 (gateway EAS), `https://autodiscover.redacted.com/*` → port 8134 (gateway Autodiscover).
- Missing: A route for `https://autodiscover.<GATEWAY_MAIL_DOMAIN>/autodiscover/autodiscover.xml` — this is the primary Android "Exchange" wizard probe per MS-OXDSCLI §2.2.3. The existing `autodiscover.redacted.com/*` route covers this if `GATEWAY_MAIL_DOMAIN=redacted.com`.
- Missing: A route for `https://<GATEWAY_MAIL_DOMAIN>/autodiscover/autodiscover.xml` (root-domain fallback probe). Some Outlook Android builds try the bare domain when the `autodiscover.` subdomain stalls.
- Missing: DNS records: CNAME for `calendar.redacted.com`, `autodiscover.redacted.com`, and optionally `@` (root) all pointing at the tunnel target, all Proxied.
- Fix: Add cloudflared ingress entries for `autodiscover.<domain>` and optionally `<domain>` root, both → `http://localhost:8134`. Add corresponding Proxied CNAME DNS records. Document these additions for the operator.

### P1 — Protocol correctness (affects client experience but not initial connection)

**7. EAS Sync calendar delta rendering completeness**
- The AGENTS.md documents §10 (Sticky Sync options) and §14 (Timezone fidelity) as delivered, including `render_calendar_app_data_with_options` and the 139-zone blob↔IANA round-trip.
- Need to verify: recurring-event exception handling, EXDATE propagation, attendee PARTSTAT write-through for calendar items created via Outlook.
- Fix: Add integration tests covering recurring series + exception + attendee response round-trip through EAS Sync → CalDAV/JMAP calendar → back to EAS Sync.

**8. EAS ItemOperations/Fetch attachment delivery**
- §12 (ItemOperations/Fetch attachment correctness) is documented as delivered: multipart response, byte-range fetch, status table, response-Fetch shape fix.
- Need to verify: large attachment (>5MiB) streaming via `push_base64` + `JmapClient::download_blob_range`, mid-stream budget abort with `max_attachment_bytes`.
- Fix: Add an end-to-end test for a multi-megabyte attachment fetch via ItemOperations with `MS-ASAcceptMultiPart: T`.

**9. MeetingResponse / iMIP integrity (§13)**
- Documented as delivered: one shared pipeline `meeting::rsvp::apply_rsvp` for EAS MeetingResponse and EWS AcceptItem/TentativelyAcceptItem/DeclineItem.
- Need to verify: organizer self-response rejection, instance scoping per §2.2.3.92.1, attendee copy PARTSTAT write-through with SCHEDULE-AGENT=CLIENT pinning, SMTP delivery with user's own identity, duplicate-RSVP idempotence via `meeting_rsvp` store (schema v9).
- Fix: Add a test simulating an Outlook Android MeetingResponse for a recurring series with an exception instance, verifying the REPLY is delivered once and a duplicate is suppressed.

**10. EAS Provision / PolicyKey**
- The gateway advertises an all-off, no-op policy. `GATEWAY_EAS_REQUIRE_POLICY_KEY=0` (default) means no enforcement.
- Android "Exchange" wizard shows a device-admin consent screen; the advertised policy being a no-op means no remote wipe is possible.
- Fix: Document this behavior for the operator. If a client refuses to proceed without an enforced key, set `GATEWAY_EAS_REQUIRE_POLICY_KEY=1`.

**11. Autodiscover V2 JSON for Outlook mobile / AutoDetect**
- `handle_autodiscover_json` serves ActiveSync, Ews, AutodiscoverV1 protocols; rejects Rest, Exchange, missing/unrecognized protocol.
- The V2 JSON response contains only `Protocol` + `Url` (no V1-era fields like `ActiveSyncUrl`).
- Need to verify: the `autodiscover.json/v1.0/{email}` path variant and `autodiscover.svc` SOAP endpoint are reachable through cloudflared.
- Fix: Add a smoke test hitting `https://autodiscover.<domain>/autodiscover/autodiscover.json?Protocol=ActiveSync` through the tunnel.

**12. EWS surface completeness for Outlook**
- EWS implements GetFolder, FindFolder, FindItem, GetItem, SyncFolderItems, CreateItem, UpdateItem, DeleteItem, SendItem, MoveItem, CopyItem, ResolveNames, GetUserAvailability, GetServerTimeZones, GetRoomLists/GetRooms, delegate ops, attachment ops, user configuration, OOF, etc.
- EWS `forwarded_https_enforced` requires `x-forwarded-proto: https` — cloudflared sets this header, so EWS through the tunnel works. Direct (non-cloudflared) access without the header gets 400.
- Fix: Document that cloudflared must preserve `X-Forwarded-Proto: https` (it does by default).

**13. MAPI/HTTP surface**
- `GATEWAY_MAPI_ENABLED=1` in the compose (matches code default `true`). New Outlook for Windows can fall back to MAPI/HTTP for some operations.
- `GATEWAY_MAPI_HMA_ENABLED=0` — no Modern Auth; Basic auth is advertised.
- MAPI/HTTP implements Connect/Disconnect/Execute skeleton; deeper ROPs return typed `NoSupport` rather than `NotFound` (audit gap #9 pattern).
- Fix: Keep MAPI enabled (default `1`) for the target clients. Document that disabling it (`GATEWAY_MAPI_ENABLED=0`) makes the route 404/invisible and Autodiscover omits the MAPI `<Protocol>` block.

### P2 — Robustness & resilience

**14. Auth circuit breaker & rate limiting**
- Implemented: per-user circuit breaker (`auth_backend_failure_threshold=5`, `auth_backend_max_backoff_secs=60`), per-user probe rate cap (`auth_probe_min_interval_ms=1000`), in-flight dedup by credential, shared JMAP client with session-auth cache.
- `AuthVerifier::invalidate_user` MUST also call `client.invalidate_user_session_auth(username)` — the verifier cache and the shared client cache are separate.
- Fix: Verify this cross-cache invalidation is wired in `models.rs`/`auth.rs`. Add a test proving a password change takes effect on first use.

**15. Health endpoints**
- `/health/live` is process-only (no dependency touched).
- `/health/ready` (+ `/health` alias) checks SQLite + the configured auth backend, mirroring `verify_backend` arbiter rules: JMAP configured → JMAP is sole arbiter; CalDAV only → CalDAV arbiter; no backend → 503.
- Both endpoints + `/metrics` bypass rate limiting via exact-match `is_unmetered_path`.
- Fix: Verify the readiness probe reuses the auth verifier's shared JMAP client (observable via `JmapClient::health_probe_count`). Already tested per AGENTS.md.

**16. Change-journal pruning vs long-offline devices (§16)**
- `prune_change_journal` records the deleted floor per owner in `journal_prune_state` (schema v10) in the same transaction as the DELETE.
- `journal_window_stale(owner, since) = since < pruned_through`. Every stale check is fail-closed (`unwrap_or(true)`).
- Forced resync at EAS Sync front doors: status 3 + SyncKey 0 when stale; `incoming_key == "0"` is never rejected.
- Ping treats stale watermark as a change (wakes within a tick).
- GetItemEstimate reports status 4 for a stale window.
- EWS SyncFolderItems: client-supplied pruned cursor → ErrorInvalidSyncStateData; stale gateway-persisted cursor → treated as full snapshot prime (not rejected, to avoid unrecoverable re-prime loop).
- Fix: All documented as delivered. Verify the `clear_local_sync_cursor_if` CAS doesn't drop a concurrent `SyncKey 0` prime's cursor.

**17. SQLite concurrency (§19)**
- `Storage::new` builds the pool via `SqliteConnectOptions` (`.journal_mode(Wal)`, `.busy_timeout(5s)`, `.synchronous(Full)`, `.foreign_keys(true)`, `POOL_SIZE=10`).
- Ping's tick loop is read-only against SQLite.
- Load test: `concurrent_sync_writes_and_ping_reads_never_fail_or_stall`.
- Fix: Already delivered. Keep Ping read-only when editing `eas.rs`.

### P3 — Frictionless deployment & documentation

**18. `.env.example` must be deleted**
- User requirement: "If this exists in the repository, delete it, and be certain the compose file-template is correct and complete."
- Action: Delete `/workspace/project/exchange_gateway/.env.example`.

**19. docker-compose.yml completeness audit**
Reviewing the current compose against the deployment scenario:

| Variable | Status | Notes |
|---|---|---|
| `TZ=UTC` | Set | |
| `GATEWAY_LOG_LEVEL=info` | Set | |
| `GATEWAY_BIND=[::]:8134` | IPv6-only | Consider `0.0.0.0:8134` for cloudflared localhost |
| `GATEWAY_CALDAV_BASE=http://stalwart:8080/dav` | Set | Matches Stalwart container name |
| `GATEWAY_JMAP_BASE=http://stalwart:8080/jmap` | Set | Matches Stalwart container name |
| `GATEWAY_CARDDAV_BASE=http://stalwart:8080/carddav` | Set | Matches Stalwart container name |
| `GATEWAY_CALENDAR_CAPABILITY_CHECK=warn` | Set | |
| `GATEWAY_HMAC_SECRET=REPLACE_...` | Placeholder | Must be replaced; `validate()` rejects `REPLACE_*` |
| `GATEWAY_HOST=calendar.example.com` | Placeholder | Must be `calendar.redacted.com` |
| `GATEWAY_MAIL_DOMAIN=example.com` | Placeholder | Must be `redacted.com` |
| `GATEWAY_DATABASE_PATH=/var/lib/exchange-gateway/gateway.db` | Set | |
| `GATEWAY_MAX_ATTACHMENT_BYTES=5242880` | Set | 5 MiB default |
| `GATEWAY_MAX_PING_HEARTBEAT=80` | Set | Below Cloudflare 125s edge timeout |
| `GATEWAY_RATE_LIMIT_*` | Set | All set |
| `GATEWAY_AUTH_*` | Set | All set |
| `GATEWAY_SMTP_*` | Set | 465/993 |
| `GATEWAY_MAIL_HOST=stalwart` | Set | |
| `GATEWAY_ALLOW_INSECURE_HTTP=1` | Set | Needed for plain HTTP to Stalwart over private network |
| `GATEWAY_PREFER_JMAP_CALENDAR=1` | Set | JMAP-first |
| `GATEWAY_PREFER_JMAP_CONTACTS=1` | Set | JMAP-first contacts |
| `GATEWAY_PREFER_CALDAV_FREEBUSY=0` | Set | |
| `GATEWAY_FORCE_CALDAV_CALENDAR=0` | Set | |
| `GATEWAY_ROOM_BOOKING_ENABLED=true` | Set | |
| `GATEWAY_EAS_REQUIRE_POLICY_KEY=0` | Set | Permissive default |
| `GATEWAY_MAPI_ENABLED=1` | Set | Needed for New Outlook fallback |
| `GATEWAY_MAPI_HMA_ENABLED=0` | Set | Basic auth |
| `GATEWAY_EMAIL_ENABLED=1` | Set | |
| `GATEWAY_SERVER_VERSION=15.2.2562.45` | Set | |
| `GATEWAY_SERVER_EXCHANGE_VERSION=Exchange2016` | Set | |
| `GATEWAY_ADMIN_USERNAME=` | Empty | No full GAL without admin creds |
| `GATEWAY_ADMIN_PASSWORD=` | Empty | No full GAL without admin creds |
| `GATEWAY_SMIME_CERT_STORE_DIR=` | Empty | Valid (no S/MIME certs) |
| Ports `8134:8134` | Set | Matches cloudflared routes |
| Volumes `/armblock/exchange-gateway/data` | Set | Matches user's deployment path |
| Volumes `/armblock/exchange-gateway/certs` | Set | Optional S/MIME cert store |
| DNS `1.1.1.1`, `8.8.8.8` | Set | |
| Network `network1` external `my-bridge-network` | Set | Matches Stalwart's network |
| Healthcheck `/health/ready` | Set | |
| Autoheal (commented) | Set | Documented as optional |

Missing from compose:
- No `GATEWAY_MAPI_OIDC_ISSUER`, `GATEWAY_MAPI_OIDC_AUDIENCE`, `GATEWAY_MAPI_ORG` — not needed with HMA disabled.
- No `GATEWAY_MAPI_CONTENTS_PAGE_SIZE`, `GATEWAY_MAPI_MAX_CONTENTS_ROWS` — defaults apply.
- No `GATEWAY_AUTH_CACHE_*_FILE` variants — Docker secrets not required per user's "no env file" rule.

**20. docker-compose.yml is missing GATEWAY_ADMIN credentials documentation**
- The compose leaves `GATEWAY_ADMIN_USERNAME=` and `GATEWAY_ADMIN_PASSWORD=` empty with a comment about cleartext.
- Fix: Document that for a full GAL (NSPI + OAB), the operator must set these to a Stalwart admin account. Without them, the GAL falls back to a self-only minimal stub.

**21. cloudflared/config.yml placeholders**
- The file uses `<YOUR-TUNNEL-UUID>`, `<GATEWAY_MAIL_DOMAIN>`, `<GATEWAY_HOST>` placeholders.
- Per user constraint: "Do not replace any placeholders nor the redacted content in any docker compose." This applies to `docker-compose.yml`. The cloudflared config is a separate file on the Ubuntu host.
- Fix: Document that the operator must render `cloudflared/config.yml` with their tunnel UUID, `GATEWAY_MAIL_DOMAIN`, and `GATEWAY_HOST` before starting cloudflared.

### P4 — Code quality & conventions

**22. `cargo fmt --check` flags eas.rs (pre-existing §10 merge state)**
- AGENTS.md notes: "`cargo fmt --check` flags eas.rs in the committed tree (pre-existing §10 merge state) — fmt is NOT a repo gate; only fmt files you touch."
- Fix: Leave eas.rs unfmt'd (pre-existing exception). Format any new files touched.

**23. `clippy --all-targets` must be 0 warnings**
- Already 0 warnings per AGENTS.md. `#[allow(clippy::too_many_arguments)]` is the accepted escape for irreducible request params.

**24. `cargo build --release` must be warning-free**
- Already warning-free per AGENTS.md (~5min build).

**25. Test suite must be fully green**
- Suite counts per AGENTS.md §16: 1027 lib + 22 fixtures + 11 snapshots + 2 deploy + 1 doc = 1063 green, clippy 0, fmt clean on ALL touched files, release warning-free.
- Fix: Run `cargo test` after any change and confirm all suites pass.

---

## Operator Action Checklist

### Before deploying (on the Ubuntu server)

1. Delete `.env.example` from the repository (user requirement: no env file).
2. Edit `docker-compose.yml`: replace all placeholders:
   - `GATEWAY_HOST=calendar.redacted.com`
   - `GATEWAY_MAIL_DOMAIN=redacted.com`
   - `GATEWAY_HMAC_SECRET=<32+ char random string, not starting with "REPLACE_">`
   - Optionally `GATEWAY_ADMIN_USERNAME=` / `GATEWAY_ADMIN_PASSWORD=` for full GAL
3. Pin Stalwart in its compose: `image: stalwartlabs/stalwart:v1.0.1@sha256:…` (not `:latest`).
4. Verify Docker network: both containers on `my-bridge-network` (external).
5. Add cloudflared routes (Zero Trust dashboard):
   - `https://autodiscover.redacted.com/*` → `http://localhost:8134` (already configured per user)
   - `https://calendar.redacted.com/*` → `http://localhost:8134` (already configured per user)
   - Add `https://redacted.com/autodiscover/*` (root-domain fallback) if the root domain serves no other content.
6. Add DNS records (Cloudflare, all Proxied):
   - CNAME `calendar.redacted.com` → `<tunnel-id>.cfargotunnel.com`
   - CNAME `autodiscover.redacted.com` → `<tunnel-id>.cfargotunnel.com`
   - CNAME `@` (root, optional) → `<tunnel-id>.cfargotunnel.com`
7. Render `cloudflared/config.yml`: substitute `<YOUR-TUNNEL-UUID>`, `<GATEWAY_MAIL_DOMAIN>`, `<GATEWAY_HOST>`.
8. Start cloudflared as a systemd service with `--token` or `--config`.
9. Deploy gateway: `docker compose pull && docker compose up -d`.
10. Verify: `curl -fsS http://localhost:8134/health/ready` → 200.
11. Add account on Android: Settings → Accounts → Add account → Exchange → `user@redacted.com` → Manual setup → Server: `calendar.redacted.com`.
12. Add account in New Outlook for Windows: Add account → `user@redacted.com` → discovers via Autodiscover.

### Cloudflare Zero Trust additions required

| Route | Target | Required? |
|---|---|---|
| `https://calendar.redacted.com/*` | port 8134 (gateway) | Already configured |
| `https://autodiscover.redacted.com/*` | port 8134 (gateway) | Already configured |
| `https://redacted.com/autodiscover/*` | port 8134 (gateway) | Recommended — root-domain Autodiscover fallback |

### Stalwart Mailserver v1.0.1 configuration changes

| Setting | Value | Required? |
|---|---|---|
| Image | `stalwartlabs/stalwart:v1.0.1@sha256:…` | Required (pin, not `:latest`) |
| JMAP enabled | yes (default) | Required |
| CalDAV enabled | yes (default) | Required |
| `STALWART_PUBLIC_URL` | `https://stalwart.redacted.com/` | Already set |
| Port 8080 exposed | yes | Required (gateway reaches JMAP/CalDAV/CardDAV here) |
| Network | `my-bridge-network` | Required (shared with gateway) |
| Admin account for GAL | set `GATEWAY_ADMIN_*` in gateway compose | Optional (enables full GAL/NSPI/OAB) |

---

## References

- `AGENTS.md` — project conventions, WBXML rules, delivered sections (§10–§20)
- `CLOUDFLARED_SETUP.md` — cloudflared tunnel setup guide
- `DEPLOY_RUNBOOK.md` — end-to-end deployment runbook
- `.env.example` — environment variable reference (to be deleted)
- `Dockerfile` — multi-stage build, Rust 1.99.1, Debian bookworm-slim runtime
- `cloudflared/config.yml` — tunnel ingress template (placeholders to be rendered)
- `exchange_protocols/*.txt` — MS Exchange protocol specs (v20250520)
- `src/sqlite_schema.sql` — SQLite schema (schema v10: journal pruning floor)