# AUDIT.md — Outstanding Issues for the Target Use-Case

**Method.** Fresh, code-level audit of the entire repository (~96k LOC of Rust
across 60+ modules). The previous AUDIT.md / AUDIT2.md files were **completely
disregarded**; every item below was re-derived from source, the in-repo specs
(`exchange_protocols/*.txt`), `docker-compose.yml`, `Dockerfile`,
`cloudflared/config.yml`, `CLOUDFLARED_SETUP.md`, `.env.example` and
`DEPLOY_RUNBOOK.md`.

**Target restated.** A single `exchange-gateway` container run beside
Stalwart Mailserver v0.16.27, exposing Stalwart's mail + calendar + contacts
to Outlook clients added as "Exchange" accounts:

- Outlook for Windows (new), October 7 2026 build
- Outlook Android 5.2641.1

Both clients reach the gateway through Cloudflare Tunnel (cloudflared running
on the Ubuntu host). The gateway serves plain HTTP on port 8134; cloudflared
terminates HTTPS at the edge. JMAP (as implemented by Stalwart v0.16.27) is
preferred over every other standard/protocol wherever Stalwart supports it.

**Verification performed.** `cargo build --release --locked` runs warning-free;
`cargo clippy --all-targets` is clean; `cargo test` passes. No `.env` file
exists; all 40 `GATEWAY_*` variables are written inline in
`docker-compose.yml` (`environment:` block).

**Priority key.** `P0` = blocks the use-case outright (clients cannot add the
account, mail/calendar never arrives, or push is silently broken).
`P1` = correctness/completeness gaps the two named clients will hit.
`P2` = robustness/operations gaps (deployment, observability, hardening).
`P3` = polish / supply-chain hardening.

---

## P0 — Blocks the use-case outright

### 1. Gateway image is not published / registry placeholder

`docker-compose.yml` carries `image: <registry>/exchange-gateway:0.1.0` — a
placeholder the operator must replace. There is no publish pipeline in the
repo (no `.github/workflows/docker-image.yml`, no buildx/push target), so the
operator cannot `docker compose pull && up -d` without building the image
themselves, which contradicts the deployment scenario ("the container image
will be pulled from the docker registry I am using").

- **Action:** Add a GitHub Actions workflow that builds multi-arch
  (`linux/amd64` + `linux/arm64`) and publishes to a registry on release tag.
- **Code priority:** existing deps (Docker, buildx) — no code change; add CI
  workflow file.

### 2. `.env.example` ships as a real file (forbidden by deployment scenario)

The deployment scenario explicitly forbids an env-file: "No env-variable file
shall be used. If this exists in the repository, delete it."
`.env.example` is present in the repo root. While it is a *reference* document
(not copied at runtime), its mere existence violates the letter of the
requirement and risks an operator creating `.env` from it and breaking the
inline-only contract.

- **Action:** Delete `.env.example` from the repo. Keep its content as a
  reference section inside `docker-compose.yml` comments (already done there).
- **Code priority:** none (file deletion).

### 3. Dockerfile bakes in `TZ="UTC"` but the use-case is `Europe/Amsterdam`

The Stalwart compose sets `TZ=Europe/Amsterdam`. The gateway Dockerfile hardcodes
`ENV TZ="UTC"`. Calendar/TZ correctness for the Amsterdam operator depends on
the gateway sharing Stalwart's zone for any local date rendering.

- **Action:** Change `ENV TZ="UTC"` to `ENV TZ=Europe/Amsterdam` (or make it a
  `GATEWAY_TZ` env var, default `Europe/Amsterdam`). The operator already sets
  `TZ=Europe/Amsterdam` in the Stalwart compose; mirror it here.
- **Code priority:** existing deps (Debian tzdata already installed).

### 4. No CI workflow proving the workspace compiles against the shipped deps

There is no `.github/workflows/` at all. `cargo check` / `cargo test` /
`cargo clippy` / `cargo build --release` are never run in CI, so a regressing
dependency or a `RUSTFLAGS="-D warnings"` violation in the Dockerfile only
surfaces at the operator's deploy time.

- **Action:** Add `.github/workflows/ci.yml` running `cargo test`,
  `cargo clippy --all-targets`, `cargo build --release --locked`, and
  `docker build` on push/PR.
- **Code priority:** existing deps (GitHub Actions).

---

## P1 — Correctness/completeness the two named clients will hit

### 5. Stalwart version pinned to v0.16.26 in docs/comments but target is v0.16.27

`Cargo.toml`, `docker-compose.yml` header, `CLOUDFLARED_SETUP.md`,
`DEPLOY_RUNBOOK.md` and `config.rs` comments all reference Stalwart v0.16.26.
The operator's Stalwart container runs v0.16.27 (`stalwartlabs/stalwart:latest`
currently resolves to v0.16.27). JMAP Calendars capability, `Email/changes`,
JSCard, and CalDAV semantics can drift between point releases.

- **Action:** Update every in-repo reference to Stalwart v0.16.27. Pin the
  operator's Stalwart compose to `stalwartlabs/stalwart:v0.16.27@sha256:…`
  (digest to be filled by the operator). Add a startup capability check that
  fails fast (`GATEWAY_CALENDAR_CAPABILITY_CHECK=enforce`) or at least logs a
  clear version mismatch.
- **Code priority:** existing deps — config string edits only.

### 6. Autodiscover response must advertise the gateway's public hostname for every mailbox domain

Android's "Exchange" wizard and Microsoft AutoDetect probe
`https://autodiscover.<mail-domain>/autodiscover/autodiscover.xml`. The
cloudflared config and DNS records only cover `autodiscover.<GATEWAY_MAIL_DOMAIN>`
and `calendar.example.com` — the deployment scenario uses one mailbox domain
(`example.com`), but a second domain (e.g. `user@example.org`) would never be
discoverable because no `autodiscover.example.org` DNS/tunnel entry exists.

- **Action:** Document in `DEPLOY_RUNBOOK.md` that each additional mailbox
  domain needs its own `autodiscover.<domain>` public hostname + CNAME on the
  tunnel, and that Cloudflare must issue a TLS cert for it (Universal SSL).
  The gateway response itself already returns `GATEWAY_HOST` for any domain
  (verified by `mobilesync_response_points_at_gateway_host_for_any_mailbox_domain`),
  so no code change is needed — only the DNS/tunnel/records per extra domain.
- **Code priority:** existing deps — documentation only.

### 7. EWS namespace limited to `Exchange2016` — fine for on-premises SE but verify against New Outlook October 2026

The gateway advertises `RequestServerVersion="Exchange2016"` and build
`15.2.2562.45`. New Outlook for Windows (October 7 2026) connecting via MAPI/HTTP
may negotiate a newer schema. The code path `version::current()` is a single
source of truth, so the fix is config-driven.

- **Action:** No code change required — `GATEWAY_SERVER_EXCHANGE_VERSION` is
  already overridable. Document in `DEPLOY_RUNBOOK.md` that if New Outlook
  rejects `Exchange2016`, the operator may bump it (but note no published
  `Exchange2019` enum token exists, so only `Exchange2016` or below is safe).
- **Code priority:** existing deps — documentation only.

---

## P2 — Robustness/operations (deployment, observability, hardening)

### 8. Compose `GATEWAY_JMAP_BASE`/`GATEWAY_CALDAV_BASE` point at `stalwart:8080` (HTTP, plaintext credentials over Docker network)

The shipped compose sets `GATEWAY_JMAP_BASE=http://stalwart:8080/jmap` and
`GATEWAY_CALDAV_BASE=http://stalwart:8080/dav`, with
`GATEWAY_ALLOW_INSECURE_HTTP=1`. This is the documented private-bridge-network
model (TLS terminates at cloudflared in front of the gateway; gateway→Stalwart
stays on the internal Docker network). It is acceptable **only** because the
two containers share `my-bridge-network` and no other peer sits on that
network. If the operator ever adds a third container or routes through a
service mesh, Basic credentials to JMAP/CalDAV travel in cleartext.

- **Action:** Document this assumption explicitly in `DEPLOY_RUNBOOK.md` and
  `CLOUDFLARED_SETUP.md`: the bridge network must be dedicated to
  `stalwart` + `exchange-gateway`; never attach other containers to it. If a
  non-trusted network is ever used, switch both bases to `https://` and set
  `GATEWAY_ALLOW_INSECURE_HTTP=0`.
- **Code priority:** existing deps — documentation only.

### 9. Compose `image:` placeholder `<registry>/exchange-gateway:0.1.0` must be replaced

Related to item #1 but deployment-facing: the operator cannot deploy until this
placeholder is substituted with their registry reference. The compose also has
`dns:` set to `1.1.1.1` / `8.8.8.8` — the operator must replace with their own
resolvers (or delete the block to inherit the Docker daemon's resolvers).

- **Action:** No code change — the operator fills `<registry>` and the `dns:`
  values in `docker-compose.yml` before `docker compose up -d`. Document both
  substitutions in `DEPLOY_RUNBOOK.md`.
- **Code priority:** none.

### 10. Cloudflare zero-trust routes: operator must add two missing routes

The user's existing cloudflared routes are:
- `https://stalwart.redacted.com/admin` → Stalwart admin (port 8080 presumably)
- `https://calendar.redacted.com/*` → port 8134
- `https://autodiscover.redacted.com/*` → port 8134

For the gateway to work, the operator MUST add:
- `https://autodiscover.<GATEWAY_MAIL_DOMAIN>/*` → port 8134 (REQUIRED for
  Android "Exchange" signup per MS-OXDSCLI §2.2.3)
- Optionally `https://<GATEWAY_MAIL_DOMAIN>/*` → port 8134 (root-domain
  Autodiscover fallback, speeds up Android signup)

And DNS records:
- CNAME `autodiscover.<mail-domain>` → `<tunnel-id>.cfargotunnel.com` (Proxied)
- CNAME `<mail-domain>` → `<tunnel-id>.cfargotunnel.com` (Proxied, optional)

If `GATEWAY_MAIL_DOMAIN=redacted.com`, the routes become:
- `https://autodiscover.redacted.com/*` → port 8134 (already listed by user!)
- Optionally `https://redacted.com/*` → port 8134

The user already has `autodiscover.redacted.com` configured, which covers
this if `GATEWAY_MAIL_DOMAIN=redacted.com`. The operator should confirm the
mail domain used for account signup matches the DNS domain of that route.

- **Action:** Document in `DEPLOY_RUNBOOK.md` exactly which cloudflared routes
  and DNS records to add. If `GATEWAY_MAIL_DOMAIN` differs from `redacted.com`,
  add `autodiscover.<GATEWAY_MAIL_DOMAIN>` + optional `<GATEWAY_MAIL_DOMAIN>`.
- **Code priority:** existing deps — documentation only.

### 11. Stalwart Mailserver container config: must enable JMAP (default on) and CalDAV/CardDAV

The operator's Stalwart compose already exposes ports 8080 (HTTP/JMAP/CalDAV/
CardDAV) and 8133. For the gateway to work via JMAP-first:

- JMAP must be enabled (Stalwart default: yes, on `/jmap`).
- CalDAV must be enabled (default: yes, on `/dav`).
- CardDAV must be enabled (default: yes, on `/carddav`).
- SMTP (465/587) and IMAP (993) are exposed — the gateway uses SMTP as a
  fallback for email submission (JMAP submission is preferred).

No Stalwart config changes are strictly required beyond what's already
exposed, but the operator should verify:
- `STALWART_PUBLIC_URL=https://stalwart.redacted.com/admin` is set (already is).
- JMAP is reachable at `http://stalwart:8080/jmap` from the gateway container
  on `my-bridge-network`.
- An admin account exists with `sysAccount*`/`sysMailingList*` permissions if
  the operator wants the full GAL (`GATEWAY_ADMIN_USERNAME/PASSWORD`).

- **Action:** Document the Stalwart verification steps in `DEPLOY_RUNBOOK.md`.
- **Code priority:** existing deps — documentation only.

### 12. No per-user rate-limit keying — all traffic from Cloudflare/Microsoft IPs shares one bucket

`rate_limit.rs` keys buckets on `CF-Connecting-IP` / `X-Forwarded-For`. All
Outlook traffic arrives via Cloudflare edge IPs and/or Microsoft datacenter IPs
(cloud sync path). One bucket means the two target clients (and all other
users behind the same egress IP) throttle each other → random 429s → sync
stalls. Default `GATEWAY_RATE_LIMIT_REQUESTS_PER_MINUTE=120` is far too low
for Ping/Sync cadence of even a single device.

- **Action:** The compose already raises defaults to 600 rpm / 2000 concurrent
  (delivered in prior audit). Code-level per-user keying remains a future
  improvement but is not required at this sizing. Document that the operator
  should raise `GATEWAY_RATE_LIMIT_REQUESTS_PER_MINUTE` if several users sync
  simultaneously.
- **Code priority:** existing deps — docs/tuning only.

### 13. Health check (`/health/ready`) uses JMAP as sole arbiter — correct, but operator must ensure JMAP is up before gateway is considered healthy

`/health/ready` returns 503 when JMAP is unreachable (even if CalDAV answers).
This is correct per the "JMAP-first" design, but it means if the operator's
Stalwart container is restarting, the gateway reads unhealthy and Docker marks
it unhealthy. `restart: unless-stopped` does **not** restart unhealthy
containers, so the operator needs the optional `autoheal` controller (commented
out in the compose) or an external watcher.

- **Action:** Document the autoheal option and that Docker Compose alone never
  restarts an unhealthy container. Already documented in `docker-compose.yml`
  comments; reinforce in `DEPLOY_RUNBOOK.md`.
- **Code priority:** existing deps — documentation only.

### 14. Outgoing SMTP to Stalwart (port 465 TLS) for email submission fallback

The gateway prefers JMAP submission (`EmailSubmission/set`) for sending, but
falls back to SMTP via `GATEWAY_SMTP_HOST=stalwart:465`. If Stalwart's SMTP
requires auth and the gateway's credentials aren't configured for submission,
sending mail fails. JMAP submission is the correct path and is implemented.

- **Action:** Verify JMAP submission works against Stalwart v0.16.27 (it does
  per `jmap.rs` `submit_email`). Document that SMTP is a fallback only and
  JMAP submission should be used. No code change needed.
- **Code priority:** existing deps — documentation only.

### 15. `GATEWAY_HMAC_SECRET` default is a weak placeholder

The compose sets `GATEWAY_HMAC_SECRET=REPLACE_WITH_A_RANDOM_32_CHAR_MINIMUM_SECRET`.
If the operator doesn't replace it, session/cookie security is weakened. The
code enforces `>= 32` chars and rejects `REPLACE_` prefix (validated in
`Config::validate` — need to confirm).

- **Action:** Keep the startup warning for placeholder hosts (already in
  `main.rs`). Add a similar warning for `GATEWAY_HMAC_SECRET` starting with
  `REPLACE_`. Document `chmod 600` on `docker-compose.yml` since it contains
  secrets in cleartext.
- **Code priority:** existing deps — add one warning check in `main.rs`/`config.rs`.

### 16. Missing `.dockerignore` exclusion for `AUDIT.md`/`AUDIT2.md` (already handled)

The `.dockerignore` already excludes `*.md`, `AUDIT.md`, `AUDIT2.md`. Good —
no action needed. (Confirmed present.)

---

## P3 — Polish / supply-chain hardening

### 17. Add `cargo-deny`/`cargo-audit` to CI

Permissive `^` ranges in `Cargo.toml` with heavy deps (axum, moka, dashmap,
tokio, etc.). `Cargo.lock` exists and the Dockerfile uses `--locked`, but no
vulnerability scanning runs.

- **Action:** Add `cargo-audit` / `cargo-deny` to the CI workflow.
- **Code priority:** existing deps — CI addition.

### 18. Per-EAS-command latency metrics (Ping wait distribution, JMAP backend latency)

Prometheus is wired (`/metrics` unmetered) but there are no per-command
histograms or Ping wait distributions. Useful for debugging the "mail arrives
late" symptom against Cloudflare's 125s edge timeout.

- **Action:** Add histograms keyed by EAS command (no username/device-id
  labels — cardinality + privacy). Use `prometheus` crate already in
  `Cargo.toml`.
- **Code priority:** existing deps — prometheus crate already present.

### 19. `cargo build --release` uses `RUSTFLAGS="-D warnings"` — any future warning fails the build

Correct and intentional, but the operator building locally will hit the same
strictness. Ensure all code stays warning-free (it does today).

- **Action:** None — already correct. Document in contributing guidance.
- **Code priority:** none.

---

## Changes the operator must make (summary)

### Cloudflare Zero Trust routes (add to existing)

| Route | → | Port | Required? |
|-------|---|------|-----------|
| `https://autodiscover.<GATEWAY_MAIL_DOMAIN>/*` | `http://<gateway-host>:8134` | 8134 | **YES** (Android Exchange wizard) |
| `https://<GATEWAY_MAIL_DOMAIN>/*` (optional) | `http://<gateway-host>:8134` | 8134 | Recommended (root-domain fallback) |

If `GATEWAY_MAIL_DOMAIN=redacted.com`, `autodiscover.redacted.com/*` is already
configured — confirm it points at port 8134.

### DNS records (Cloudflare)

| Name | Type | Target | Proxy |
|------|------|--------|-------|
| `autodiscover.<mail-domain>` | CNAME | `<tunnel-id>.cfargotunnel.com` | Proxied |
| `<mail-domain>` (root) | CNAME | `<tunnel-id>.cfargotunnel.com` | Proxied (optional) |

### Stalwart Mailserver container (verify, no config change required)

- JMAP at `http://stalwart:8080/jmap` reachable from gateway on `my-bridge-network`
- CalDAV at `http://stalwart:8080/dav` reachable
- CardDAV at `http://stalwart:8080/carddav` reachable (contacts)
- Admin account with `sysAccount*`/`sysMailingList*` if full GAL wanted
- Pin to `stalwartlabs/stalwart:v0.16.27@sha256:…` in **its** compose

### `docker-compose.yml` edits (operator fills in before `up -d`)

1. `image:` → `<registry>/exchange-gateway:0.1.0` (their registry)
2. `GATEWAY_HMAC_SECRET` → >= 32 random chars, not starting with `REPLACE_`
3. `GATEWAY_HOST` → public EAS hostname (e.g. `calendar.redacted.com`)
4. `GATEWAY_MAIL_DOMAIN` → mailbox domain (e.g. `redacted.com`)
5. `dns:` → operator's resolvers (or delete block)
6. Optionally `GATEWAY_ADMIN_USERNAME/PASSWORD` for full GAL

No `.env` file shall be created or used.

---

## Implementation code-priority summary

For every gap above, resolution follows this order:
1. **Existing dependencies** — configure/env/docs changes using crates already in
   `Cargo.toml` (tokio, axum, reqwest, prometheus, moka, dashmap, governor, etc.)
2. **New high-quality dependencies** — only if no existing crate covers the need
   (e.g., `cargo-audit`/`cargo-deny` for CI scanning)
3. **Rust 1.99.0 std API** — prefer std over custom code where 1.99 adds the
   capability (project edition 2024, rust-version 1.99.0)
4. **Custom code** — last resort, minimal and spec-traceable
