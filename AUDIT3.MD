# AUDIT3.MD — Outstanding Issues for the Exchange Gateway Target Use-Case

**Scope:** This audit reviews the full repository (~121k LOC across EAS, EWS, MAPI/HTTP, Autodiscover, JMAP client, CalDAV/CardDAV, SMTP, WBXML, storage, Dockerfile, docker-compose.yml) against the specific use-case:

> Run `exchange_gateway` in a container alongside Stalwart Mailserver v0.16.27 so that Outlook for Windows (new, October 7 2026) and Outlook Android (5.2639.2) can connect as native Exchange accounts (EAS v16.1 only) with calendar and email working perfectly, no client-side extensions, JMAP preferred wherever feasible, serving plain HTTP (TLS terminated by cloudflared on the Ubuntu host), container image pulled from a registry, no `.env` file, all configuration inline in `docker-compose.yml`.

**Method:** Exhaustive codebase audit. AUDIT.md and AUDIT2.md are completely disregarded per instruction. Issues are numbered 1..N and listed top-to-bottom in priority order (P0 = blocks the use-case outright, P1 = correctness/completeness risk, P2 = robustness/friction). Each issue cites the code priority to apply for resolution: 1) existing dependencies, 2) new high-quality dependencies, 3) Rust 1.99.0 API, 4) custom code.

---

## P0 — Blocks the use-case outright

### 1. `docker-compose.yml` does not define the Stalwart Mailserver service

**File:** `docker-compose.yml`

The compose defines only the `exchange-gateway` service. It references `stalwart:8080` as an upstream (`GATEWAY_CALDAV_BASE=http://stalwart:8080/dav`, `GATEWAY_JMAP_BASE=http://stalwart:8080/jmap`, `GATEWAY_CARDDAV_BASE=http://stalwart:8080/carddav`, `GATEWAY_SMTP_HOST=stalwart`, `GATEWAY_IMAP_HOST=stalwart`, `GATEWAY_MAIL_HOST=stalwart`) but no `stalwart` service exists in the file. The two containers cannot run together on `my-bridge-network` unless Stalwart is also declared.

**Required:** Add a `stalwart` service to `docker-compose.yml` (image `stalwartlabs/stalwart:v0.16.27`, pinned digest), exposing ports 8080 (JMAP/CalDAV/CardDAV HTTP), 25/465/587 (SMTP), 993 (IMAP), 4190 (JMAP/Sieve), on `network1`, with the same volumes as the operator's Stalwart compose (`/armblock/stalwart/etc`, `/armblock/stalwart/data`). Do NOT replace placeholders/redacted content.

**Code priority:** custom code (docker-compose is configuration, not Rust).

### 2. `docker-compose.yml` uses an unresolvable image placeholder

**File:** `docker-compose.yml` line 40

`image: <registry>/exchange-gateway:0.1.0` — `<registry>` is a literal placeholder that `docker compose pull` cannot resolve. The use-case requires the image to be pulled from a container registry. No `build:` alternative is uncommented.

**Required:** Replace `<registry>` with the actual registry host (or document that the operator must substitute it). Provide a digest-pinned tag (`@sha256:…`) where possible. Keep the `build:` block commented (development-only).

**Code priority:** custom code (compose file).

### 3. Required environment variables are placeholders that must be replaced before first boot

**File:** `docker-compose.yml` lines 65–67, 44–45

- `GATEWAY_HMAC_SECRET=REPLACE_WITH_A_RANDOM_32_CHAR_MINIMUM_SECRET` — must be >= 32 chars and not start with `REPLACE_`.
- `GATEWAY_HOST=calendar.example.com` — placeholder public hostname.
- `GATEWAY_MAIL_DOMAIN=example.com` — placeholder mail domain.
- `GATEWAY_ADMIN_USERNAME=` / `GATEWAY_ADMIN_PASSWORD=` — empty (minimal GAL stub only).

The container will start but Autodiscover will advertise unresolvable URLs and authentication will not work against real Stalwart credentials until these are edited inline.

**Required:** Document that every placeholder (`REPLACE_*`, `example.com`, `calendar.example.com`) must be replaced with production values before `docker compose up -d`. Keep all values inline (no `.env` file, no `env_file:`).

**Code priority:** custom code (compose file + documentation).

### 4. EAS Folder management commands are not handled

**File:** `src/eas.rs`

The WBXML tables in `src/wbxml.rs` define tokens for `FolderHierarchy:FolderCreate` (7,0x13), `FolderHierarchy:FolderDelete` (7,0x14), `FolderHierarchy:FolderUpdate` (7,0x15), and `ItemOperations:EmptyFolderContents` (20,0x12), but `command_grammar()` in `src/eas.rs` has no entries for `foldercreate`, `folderdelete`, `folderupdate`, or `emptyfoldercontents`, and the `handle()` dispatch has no arms for them. Outlook for Windows and Outlook Android both issue folder management operations (create subfolders, delete folders, empty junk/deleted items). These commands will hit the `_ => unsupported_command_response()` arm and fail.

**Required:** Add `foldercreate`, `folderdelete`, `folderupdate`, `emptyfoldercontents` to `command_grammar()` and dispatch them in `handle()`. Implement the handlers translating to Stalwart via JMAP (preferred: create/update/delete mailbox/calendar/addressbook) or CalDAV fallback. JMAP `Mailbox/set` and `Calendar/set` are the preferred paths; CardDAV address-book create/delete via `jscard`/CardDAV where no JMAP equivalent exists.

**Code priority:** 1) existing dependencies (JMAP client already in `src/jmap.rs`), 2) new high-quality dependencies if needed, 3) Rust 1.99.0 API, 4) custom code.

### 5. No CI/CD pipeline for building and publishing the container image to a registry

**File:** `.github/workflows/` (present but unrelated to container publishing)

The use-case requires images hosted in a container registry and pulled by `docker compose`. The repo has GitHub Actions workflows (clippy, gemini, deps, protocols, etc.) but none build/publish a multi-arch Docker image. There is no `docker buildx`/`build-publish` workflow, no registry credential handling, no digest pinning.

**Required:** Add a GitHub Actions workflow that builds the Dockerfile multi-arch (amd64 + arm64), pushes to the configured registry, and records the image digest. The compose should reference the digest-pinned image.

**Code priority:** custom code (GitHub Actions YAML).

---

## P1 — Correctness / completeness risks against the two named clients

### 6. EAS `Sync` does not advertise or expose the Tasks (7) and Notes (10) collections in FolderSync

**File:** `src/eas.rs`, `src/tasks.rs`

`TASKS_COLLECTION_ID = "7"` and `NOTES_COLLECTION_ID = "10"` exist in `src/tasks.rs`, but FolderSync renders collections from storage-backed data. Outlook Android and New Outlook expect to see Tasks and Notes folders in the folder hierarchy so they can sync them. If FolderSync does not include Collections 7 and 10, the clients will not offer Tasks/Notes sync.

**Required:** Ensure FolderSync emits `<Collection>` entries for Tasks (7) and Notes (10) with proper `<Class>` (`Tasks`/`Notes`), `<SyncKey>`, `<CollectionId>`, and `<Status>`. Back them by the JMAP-first CalDAV store (VTODO/VJOURNAL) as already implemented in `src/tasks.rs`, or by the gateway-local mirror when CalDAV is unconfigured.

**Code priority:** 1) existing dependencies (JMAP/CalDAV clients), 3) Rust 1.99.0 API, 4) custom code.

### 7. Rate limiter falls back to a single global bucket when no client-IP identity is available

**File:** `src/rate_limit.rs`

`rate_limit_key()` returns `GLOBAL_KEY` (`__global__`) when `CF-Connecting-IP`, `Forwarded`, and `X-Forwarded-For` are all absent. In the reference deployment all traffic arrives via Cloudflare (which sets `CF-Connecting-IP`), so this path is rarely hit. But if cloudflared is misconfigured or a client connects directly, all unauthenticated/pre-auth requests share one bucket and can throttle each other into 429s — exactly the self-DoS scenario the comment warns about.

**Required:** Keep per-IP keying as the primary path. Add a secondary per-user-keyed bucket (using the authenticated username after the auth middleware runs) as a fallback for authenticated requests, so a single shared IP cannot starve one user. Per-user keying is safe post-authentication.

**Code priority:** 1) existing dependencies (`governor` already supports keyed buckets), 4) custom code.

### 8. EAS Ping heartbeat clamp depends on `CF-RAY` detection or explicit `GATEWAY_MAX_PING_HEARTBEAT`

**File:** `src/eas.rs`, `CLOUDFLARED_SETUP.md`

Cloudflare's edge terminates unanswered requests at ~125s (HTTP 524). EAS Ping heartbeats requested by Outlook clients commonly exceed 125s. The code clamps to 80s when `CF-RAY` is detected or when `GATEWAY_MAX_PING_HEARTBEAT` is set. If neither is in effect (e.g., non-Cloudflare direct exposure, or the header is stripped), a long Ping will be cut at the edge, breaking push.

**Required:** Make the clamp unconditional (always apply `GATEWAY_MAX_PING_HEARTBEAT`, default 80s) regardless of `CF-RAY` detection, with an opt-out escape hatch for trusted direct deployments. Document this in `CLOUDFLARED_SETUP.md`.

**Code priority:** 1) existing dependencies, 4) custom code.

### 9. Autodiscover V2 JSON and `.svc` endpoints exist but are not exercised by any documented cloudflared ingress path

**File:** `src/main.rs` routes, `cloudflared/config.yml`

The gateway serves `/autodiscover/autodiscover.json`, `/autodiscover/autodiscover.json/v1.0/{email}`, and `.svc` endpoints, but `cloudflared/config.yml` only defines ingress for `autodiscover.<domain>`, `<domain>`, and `<GATEWAY_HOST>`. All three share the same service (`http://localhost:8134`), so routing works, but the documentation does not explicitly confirm that the JSON V2 path (used by AutoDetect for Outlook mobile/New Outlook discovery) is reachable through the tunnel.

**Required:** Document in `CLOUDFLARED_SETUP.md` that all three hostnames route to the same gateway service and that Autodiscover V1 XML, V2 JSON, and `.svc` are all served on each. No code change needed if the routes already exist (they do).

**Code priority:** custom code (documentation).

---

## P2 — Robustness / friction

### 10. No `.env.example` clarity: the file exists but says "NOT A FILE TO COPY" while the user forbids `.env` files

**File:** `.env.example`

The file is a reference only; all config must be inline in `docker-compose.yml`. This is already the current design. No gap in code, but the operator experience could be clearer: a `docker compose config` validation step should be documented to catch typos before startup.

**Required:** Add to `DEPLOY_RUNBOOK.md` a step: `docker compose config` (validates the YAML and all inline env vars) before `docker compose up -d`.

**Code priority:** custom code (documentation).

### 11. `GATEWAY_MAPI_ENABLED` default inconsistency between `.env.example` and `Config`

**File:** `.env.example` line 104, `src/config.rs`

`.env.example` shows `GATEWAY_MAIL_ENABLED=1` and `GATEWAY_MAPI_ENABLED=0`. The code's `default_mapi_enabled()` may default to `true` (per the memory context: "mapi_enabled defaults to true"). If `.env.example` says 0 but the Rust default is true, an operator copying `.env.example` values inline gets a different behavior than an operator setting nothing.

**Required:** Align `.env.example` with the code defaults. Document the effective default for each boolean. For the target use-case (Outlook Android + New Outlook Windows), MAPI/HTTP is not needed; keeping `GATEWAY_MAPI_ENABLED=0` reduces attack surface. New Outlook for Windows can fall back to MAPI/HTTP for some operations, so document the trade-off.

**Code priority:** custom code (config defaults + documentation).

### 12. No documented database migration path for schema upgrades

**File:** `sqlite_schema.sql`, `src/storage.rs`

The schema has version tracking (`schema_version` table, up to v10). There is no documented migration procedure for existing deployments when the schema version advances. An operator upgrading the gateway container could end with a stale schema.

**Required:** Document the migration story: `Storage::init_schema()` is idempotent and applies missing versions; PRAGMA-probed `ALTER TABLE` migrations run automatically. No code change needed if already the case; document it.

**Code priority:** custom code (documentation).

### 13. S/MIME certificate store mount is unconditional in the compose

**File:** `docker-compose.yml` line 159

`- /armblock/exchange-gateway/certs:/var/lib/exchange-gateway/certs` is always mounted. If the host directory does not exist, Docker creates it as root-owned, and the container's `gateway` user (UID 10001) may not be able to read it, causing startup failure or silent S/MIME degradation.

**Required:** Document that the host certs directory must exist and be readable by UID 10001 before startup, or remove the mount line and document it as optional.

**Code priority:** custom code (compose + documentation).

### 14. EWS operations for folder management are partially stubbed

**File:** `src/ews_folder_ops.rs`

Operations like `CreateFolder`, `UpdateFolder`, `DeleteFolder`, `MoveFolder`, `CopyFolder`, `EmptyFolder`, `MarkAllItemsAsRead`, `FindConversation`, `ExpandDL`, `GetUserRetentionPolicyTags`, `GetSearchableMailboxes`, `GetSharingMetadata`, `GetSharingFolder`, `RefreshSharingFolder`, `GetMessageTrackingReport`, `SetUserConfiguration`, `DeleteUserConfiguration`, `UploadItems`, `ExportItems` are dispatched to `crate::ews_folder_ops` handlers. Several of these may return "not supported" or stub responses. Outlook for Windows uses several of these (folder operations, configuration, conversation view).

**Required:** Audit each `ews_folder_ops` handler against the EWS spec and the two target clients. Where a handler is a stub, implement it via JMAP (preferred: `Mailbox/set` for folders, `Email/set` for messages) or CalDAV (calendar folders), returning proper EWS response XML.

**Code priority:** 1) existing dependencies (JMAP client), 4) custom code.

---

## Summary priority list (top-to-bottom)

| # | Priority | Issue | File(s) | Code priority |
|---|----------|-------|---------|---------------|
| 1 | P0 | `docker-compose.yml` does not define the Stalwart Mailserver service | `docker-compose.yml` | custom code |
| 2 | P0 | `docker-compose.yml` image placeholder `<registry>/exchange-gateway:0.1.0` unresolvable | `docker-compose.yml` | custom code |
| 3 | P0 | Required env vars are placeholders (`REPLACE_*`, `example.com`, `calendar.example.com`) | `docker-compose.yml` | custom code |
| 4 | P0 | EAS FolderCreate/FolderDelete/FolderUpdate/EmptyFolderContents not handled | `src/eas.rs`, `src/wbxml.rs` | 1) existing deps, 4) custom code |
| 5 | P0 | No CI/CD pipeline to build/publish container image to registry | `.github/workflows/` | custom code |
| 6 | P1 | FolderSync may not expose Tasks (7) / Notes (10) collections | `src/eas.rs`, `src/tasks.rs` | 1) existing deps, 4) custom code |
| 7 | P1 | Rate limiter falls back to a single global bucket without client IP | `src/rate_limit.rs` | 1) existing deps, 4) custom code |
| 8 | P1 | EAS Ping heartbeat clamp depends on `CF-RAY` detection or explicit env | `src/eas.rs` | 1) existing deps, 4) custom code |
| 9 | P1 | Autodiscover V2 JSON reachability through cloudflared not documented | `cloudflared/config.yml`, `CLOUDFLARED_SETUP.md` | custom code |
| 10 | P2 | No `docker compose config` validation step documented | `DEPLOY_RUNBOOK.md` | custom code |
| 11 | P2 | `GATEWAY_MAPI_ENABLED` default inconsistency between `.env.example` and code | `.env.example`, `src/config.rs` | custom code |
| 12 | P2 | No documented database migration path for schema upgrades | `sqlite_schema.sql`, `src/storage.rs` | custom code |
| 13 | P2 | S/MIME certs mount is unconditional; may break on missing host dir | `docker-compose.yml` | custom code |
| 14 | P2 | EWS folder/configuration operations may be stubbed | `src/ews_folder_ops.rs` | 1) existing deps, 4) custom code |

---

## Resolution guidance (code priority order)

For each issue, resolve using this order:

1. **Existing dependencies** — prefer crates already in `Cargo.toml` (e.g., `jmap` client in `src/jmap.rs`, `governor` for rate limiting, `reqwest` for HTTP, `quick-xml` for XML, `sqlx` for storage, `chrono-tz`/`windows-timezones` for timezones, `lettre` for SMTP, `moka` for caches, `dashmap` for concurrent maps, `tower-http` for middleware).
2. **New high-quality dependencies** — only add a crate if no existing dependency can do the job (e.g., a dedicated CalDAV client if `reqwest`-based `src/caldav.rs` is insufficient).
3. **Rust 1.99.0 API** — prefer standard-library features available in Rust 1.99.0 over custom implementations.
4. **Custom code** — last resort; keep minimal and well-tested.

## JMAP-first directive

Wherever possible/feasible/preferable, JMAP (as implemented by Stalwart Mailserver v0.16.27) must be used instead of any other standard/protocol:

- Email read/sync → JMAP `Email/query`, `Email/get`, `Email/changes`, `Email/set` (RFC 8621) — already the primary path in `src/jmap.rs` and `src/email.rs`.
- Calendar → JMAP Calendar (`urn:ietf:params:jmap:calendars`) with CalDAV fallback — already preferred via `GATEWAY_PREFER_JMAP_CALENDAR=1`.
- Contacts → JMAP Contacts (`urn:ietf:params:jmap:contacts`, RFC 9610) with CardDAV fallback — already preferred via `GATEWAY_PREFER_JMAP_CONTACTS=1`.
- Free/busy → JMAP `Principal/getAvailability` with CalDAV freebusy fallback.
- Authentication → JMAP session (`/jmap`) as the sole arbiter when configured.
- Sieve/OOF → JMAP `urn:ietf:params:jmap:sieve`.
- Directory/GAL → JMAP `urn:stalwart:jmap` `x:Account/*` / `x:MailingList/*`.

SMTP (port 465/587) remains for email submission only when JMAP submission is not viable; CalDAV remains the fallback for calendar when JMAP calendar writes are disabled by the startup capability check.