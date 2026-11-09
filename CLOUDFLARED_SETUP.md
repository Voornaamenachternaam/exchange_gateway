# CLOUDFLARED_SETUP.md

# Cloudflare Tunnel Setup Guide for Exchange Gateway

This guide configures Cloudflare Tunnel to expose the Exchange Gateway container via HTTPS. cloudflared runs natively on Ubuntu Server 24.04 LTS (not as a container). All container configuration is done via environment variables in Docker Compose.

> **Placeholder convention - read before deploying.** Every `example.com`
> hostname in this file is a **placeholder** for your real domain and must
> be replaced in your own copy before deployment. `example.com` stands for
> your real root / mail domain (`GATEWAY_MAIL_DOMAIN`), `mail.example.com`
> for your real mailserver hostname / MX record (`GATEWAY_MAIL_HOST`),
> `calendar.example.com` for `calendar.` + your real domain (`GATEWAY_HOST`),
> and `autodiscover.example.com` for `autodiscover.` + your real domain.
> After replacement: `GATEWAY_HOST` must equal `calendar.` + the mail
> domain, and `GATEWAY_MAIL_HOST` must equal your real MX hostname. No
> tunnel route is ever created for the bare root domain (it already has
> A/AAAA records) - only the `calendar.` and `autodiscover.` routes to
> port 8134 are needed.

**Two deployment options are supported:**
1. **Token-based (recommended)**: Use `--token <TUNNEL_TOKEN>` in systemd service - no config file needed
2. **Config-file based**: Use `--config /path/to/config.yml` with `cloudflared/config.yml` from this repository

---

## Architecture

```
Outlook Client (HTTPS)
        ↓
Cloudflare Edge (TLS terminated automatically)
        ↓
Cloudflare Tunnel (encrypted HTTP/2)
        ↓
cloudflared (host: Ubuntu Server 24.04 LTS, systemd service)
        ↓
Exchange Gateway Container (HTTP on port 8134)
```

Cloudflare terminates TLS at the edge. The tunnel provides encrypted transport from edge to your origin.

---

## Prerequisites

- Ubuntu Server 24.04 LTS with cloudflared installed
- Docker and Docker Compose on the host
- Cloudflare account with a domain
- Exchange Gateway container built

---

## Step 1: Create Tunnel in Cloudflare Dashboard

1. Log into [Cloudflare Dashboard](https://dash.cloudflare.com/)
2. Go to **Networks → Tunnels**
3. Click **Create a tunnel**
4. Select **Cloudflared** as the connector
5. Choose your account/domain
6. Name your tunnel (e.g., `exchange-gateway`)
7. **Copy the tunnel token** — you'll use this when starting cloudflared
8. **Save the tunnel UUID** — you'll use this if deploying with config file

---

## Step 2: Configure Public Hostnames (Android "Exchange" signup requires exactly TWO)

Android's "Exchange" account wizard and Microsoft AutoDetect (the cloud
probing service used by Outlook Android and New Outlook for Windows)
derive discovery addresses from **the domain part of the email address the
user types** — i.e. your `GATEWAY_MAIL_DOMAIN` — per MS-OXDSCLI §2.2.3:

1. `https://autodiscover.example.com/autodiscover/autodiscover.xml`
   (*placeholder for `autodiscover.` + your real domain*; primary probe -
   **mandatory**).
2. `https://calendar.example.com/Microsoft-Server-ActiveSync`
   (*placeholder for `calendar.` + your real domain* = `GATEWAY_HOST`; the
   URL the gateway's Autodiscover response points at).

Both probes must reach the gateway, and the gateway's Autodiscover response
(per `src/autodiscover.rs`, `ResponseSchema::MobileSync` detection) answers
MobileSync clients with `<Url>https://$GATEWAY_HOST/Microsoft-Server-ActiveSync</Url>`
— so `GATEWAY_HOST` itself must also be a public hostname on the tunnel.

**Scope note - one discovery setup per mailbox domain.** The sign-up flow
described here works for mailboxes under `GATEWAY_MAIL_DOMAIN` (placeholder
`example.com` = your real root / mail domain), because that is the (only)
domain for which Steps 2-3 publish discovery DNS records, tunnel ingress
routes, and TLS. The mobilesync response itself returns the `GATEWAY_HOST`
URL for **any** email domain (verified by
`mobilesync_response_points_at_gateway_host_for_any_mailbox_domain` in
`src/autodiscover.rs`), but that only means no per-domain changes are needed
*in the gateway response* - it does not make `autodiscover.<other domain>`
resolvable. To also support Android signup for an additional mailbox domain,
repeat Hostnames A/B and the CNAME records below **for that domain** (with
`example.com` replaced by that domain): add `autodiscover.<that domain>`
and `calendar.<that domain>` as public hostnames on the tunnel with
matching ingress entries and Cloudflare-proxied DNS records, and ensure
Cloudflare issues a TLS certificate for those hostnames (Universal SSL on
that zone, or move the zone to Cloudflare). Without those per-domain
records, Android's first probe for that domain never reaches the gateway,
regardless of what the Autodiscover response contains.

In the same tunnel settings (**Public Hostname** tab), add exactly **two**
entries, each pointing at the gateway:

#### Hostname A - EAS service hostname (`GATEWAY_HOST`)

- **Subdomain/Domain**: `calendar.example.com` (*placeholder for
  `calendar.` + your real domain*; the value you will set as `GATEWAY_HOST`
  in `docker-compose.yml`)
- **Type**: HTTP
- **Service**: `http://localhost:8134`

#### Hostname B - Autodiscover subdomain (`autodiscover.` + `GATEWAY_MAIL_DOMAIN`) - REQUIRED for Android

- **Subdomain/Domain**: `autodiscover.example.com` *(placeholder for
  `autodiscover.` + your real domain*, i.e. `autodiscover.` + the exact
  `GATEWAY_MAIL_DOMAIN` from `docker-compose.yml`)
- **Type**: HTTP
- **Service**: `http://localhost:8134`

#### No Hostname C - do not create a root-domain route

There is **no** third public hostname for the bare root domain
(`example.com` = your real root domain). The root domain already has
A/AAAA records serving other content, so the root is not routed through
the tunnel in this deployment: a root CNAME to the tunnel would conflict
with those existing records. This is an operator constraint for this
deployment, not a Cloudflare limitation in general. Android's primary
Autodiscover probe is Hostname B (`autodiscover.` + the mail domain, per
MS-OXDSCLI §3.2.5 / §4), and the Android wizard proceeds via Hostname B
alone when B answers. The root-domain autodiscovery attempt
(`https://example.com/autodiscover/autodiscover.xml`) will fail because
no tunnel route serves the root; some Android builds may take longer to
sign up as a result.

### One canonical host serves every gateway surface — no per-protocol routes

The gateway binds a single HTTP port (`GATEWAY_BIND`, `8134` in the shipped
compose) and serves **every** client-facing surface from that one origin
(see the route table in `src/main.rs`):

| Path | Surface |
|------|---------|
| `/Microsoft-Server-ActiveSync` | EAS (ActiveSync) |
| `/EWS/Exchange.asmx`, `/EWS/{*path}` | EWS SOAP |
| `/autodiscover/autodiscover.xml`, `/autodiscover/autodiscover.svc`, `/autodiscover/autodiscover.json` | Autodiscover (V1 XML/SOAP + V2 JSON) |
| `/mapi/emsmdb`, `/mapi/nspi` | MAPI/HTTP + NSPI |
| `/OAB/{guid}/{file}` | Offline Address Book download |
| `/ecp`, `/ecp/{*path}` | Exchange Control Panel |
| `/health`, `/health/live`, `/health/ready`, `/metrics` | Health + metrics |

Autodiscover renders every service URL from the single `GATEWAY_HOST`
(e.g. `<EwsUrl>https://{GATEWAY_HOST}/EWS/Exchange.asmx</EwsUrl>`,
`<Url>https://{GATEWAY_HOST}/Microsoft-Server-ActiveSync</Url>`), so the
EWS, MAPI, OAB and ECP endpoints all live on **Hostname A**
(`GATEWAY_HOST`; `calendar.example.com` in the examples is a placeholder
for `calendar.` + your real domain) - no additional tunnel route is needed
for any of them. The two existing Zero-Trust routes
(`calendar.example.com` and `autodiscover.example.com` to port 8134;
placeholders for `calendar.` + your real domain and `autodiscover.` +
your real domain) are therefore sufficient **provided the client is
configured for the canonical `GATEWAY_HOST`**: Autodiscover must be
reachable on the `autodiscover.example.com` host (Hostname B) and the
response must point at `GATEWAY_HOST`. The gateway never advertises
`mail.example.com` (placeholder for your real MX hostname, the Stalwart
mail endpoint) for EAS/EWS, so no route to the MX hostname is part of
this deployment; if a client is ever configured to use a non-canonical
host for EAS/EWS instead of `GATEWAY_HOST`, that host needs its own
explicit route - use a separate non-MX hostname for that, never the MX
hostname. No root-domain route exists or may be created (see "No
Hostname C" above): the bare root domain already has A/AAAA records.

### `GATEWAY_MAIL_HOST` must be the public MX host (AUDIT3 §8)

Autodiscover's IMAP/SMTP `<Protocol>` blocks advertise `GATEWAY_MAIL_HOST`
(`src/autodiscover.rs` renders `<Server>{mail_host}</Server>` from it).
Set it to the **publicly resolvable MX host** (`mail.example.com` =
placeholder for your real MX hostname, matching your MX record) — never the internal Docker service name
(`stalwart`), which IMAP/SMTP-capable clients cannot resolve. The backend
plumbing variables `GATEWAY_SMTP_HOST`/`GATEWAY_IMAP_HOST` stay on the
internal service name and are never advertised.

---

## Step 3: Create DNS Records

Create one **CNAME** per public hostname from Step 2 (exactly two -
there is no root-domain record), all pointing at the same tunnel target
(`<tunnel-id>.cfargotunnel.com`, shown in the tunnel settings). With the
placeholders as shipped (`GATEWAY_MAIL_DOMAIN=example.com`,
`GATEWAY_HOST=calendar.example.com`):

| Type  | Name                        | Target                              | Proxy status              |
|-------|-----------------------------|-------------------------------------|---------------------------|
| CNAME | `calendar` (GATEWAY_HOST)   | `<tunnel-id>.cfargotunnel.com`      | Proxied                   |
| CNAME | `autodiscover`              | `<tunnel-id>.cfargotunnel.com`      | Proxied                   |

> **Do not create an `@` (root) CNAME.** The bare root domain already has
> A/AAAA records and must not be pointed at the tunnel.

Both records MUST be **Proxied** from the start. A DNS-only record
pointing at `*.cfargotunnel.com` never reaches the tunnel: tunnel routing
happens only inside Cloudflare's edge, so a DNS-only hostname resolves to a
CF any-cast address that does not answer HTTP for this service and the
verification checks below (and the Android wizard itself) would fail
against the intended public HTTPS path. Public TLS is provided by
Cloudflare's Proxied mode via Universal SSL; the gateway's Cloudflare-mode
Ping clamp documented below likewise only engages on Proxied traffic.

**Verification (from any network):**

```bash
MOBILESYNC_XML='<?xml version="1.0"?><Autodiscover xmlns="http://schemas.microsoft.com/exchange/autodiscover/mobilesync/requestschema/2006"><Request><EMailAddress>user@example.com</EMailAddress><AcceptableResponseSchema>http://schemas.microsoft.com/exchange/autodiscover/mobilesync/responseschema/2006</AcceptableResponseSchema></Request></Autodiscover>'
EXPECTED_URL='<Url>https://calendar.example.com/Microsoft-Server-ActiveSync</Url>'

# Autodiscover subdomain (Hostname B) — must answer the POST with the
# gateway's mobilesync XML; --fail-with-body aborts on any HTTP error so a
# broken edge route cannot masquerade as success.
curl --fail-with-body -sS -X POST \
  -H 'Content-Type: text/xml' \
  -d "$MOBILESYNC_XML" \
  https://autodiscover.example.com/autodiscover/autodiscover.xml \
  | grep -F "$EXPECTED_URL"
# The <Url> in the response MUST be https://calendar.example.com/Microsoft-Server-ActiveSync
# (i.e. GATEWAY_HOST; calendar.example.com = placeholder for
# calendar.<your real domain>), regardless of the email domain queried.

# There is deliberately no root-domain check: no route for the bare root
# domain exists (it already has A/AAAA records), and Android's primary
# probe is the autodiscover. hostname above.

# EAS service hostname (Hostname A)
curl --fail-with-body -sS https://calendar.example.com/health
```

---

## Step 4: Deploy cloudflared

### Option A: Token-Based Deployment (Recommended)

Create the systemd service file:

```bash
sudo nano /etc/systemd/system/cloudflared.service
```

```ini
[Unit]
Description=Cloudflare Tunnel for Exchange Gateway
After=network-online.target docker.service
Wants=network-online.target

[Service]
Type=simple
ExecStart=/usr/local/bin/cloudflared tunnel run --token <YOUR_TUNNEL_TOKEN>
Restart=on-failure
RestartSec=5s
User=root

[Install]
WantedBy=multi-user.target
```

Replace `<YOUR_TUNNEL_TOKEN>` with the token from Step 1.

Then enable and start:

```bash
sudo systemctl daemon-reload
sudo systemctl enable cloudflared
sudo systemctl start cloudflared
sudo systemctl status cloudflared
```

### Option B: Config-File Based Deployment

If you prefer using a config file instead of tokens:

1. Prepare the cloudflared config directory:
```bash
mkdir -p ~/.cloudflared
```

2. Render the template into `~/.cloudflared/config.yml` (see the
   "HOSTNAME SUBSTITUTION" block at
   the top of `cloudflared/config.yml`). Use the one-liner there — it
   substitutes the tunnel UUID and both hostnames from `docker-compose.yml` plus an
   exported `TUNNEL_ID` in a single `sed` pass, writing the result to
   `~/.cloudflared/config.yml` directly from the template:
   - `<YOUR-TUNNEL-UUID>` → your tunnel UUID from Step 1 (`TUNNEL_ID`)
   - `<GATEWAY_MAIL_DOMAIN>` -> the `GATEWAY_MAIL_DOMAIN` from `docker-compose.yml`
     (drives the `autodiscover.` ingress entry; the shipped placeholder
     value is `example.com` = your real root / mail domain)
   - `<GATEWAY_HOST>` -> the `GATEWAY_HOST` from `docker-compose.yml`
     (the shipped placeholder value is `calendar.example.com` =
     `calendar.` + your real domain)
   (the config file carries exactly the two required hostnames from Step 2;
   there is no root-domain ingress entry - do not add one)
   Do NOT edit the UUID into `~/.cloudflared/config.yml` first and then run
   the one-liner: the command re-reads the template (which always contains
   the literal `<YOUR-TUNNEL-UUID>`) and would overwrite your edit, leaving
   an unfilled placeholder in `tunnel:` and `credentials-file:`.

3. Run the tunnel:
```bash
cloudflared tunnel run --config ~/.cloudflared/config.yml exchange-gateway
```

Or create a systemd service:
```ini
[Unit]
Description=Cloudflare Tunnel for Exchange Gateway
After=network-online.target docker.service
Wants=network-online.target

[Service]
Type=simple
ExecStart=/usr/local/bin/cloudflared tunnel run --config /root/.cloudflared/config.yml exchange-gateway
Restart=on-failure
RestartSec=5s
User=root

[Install]
WantedBy=multi-user.target
```

---

## Step 5: Docker Compose Configuration

### `docker-compose.yml`

The repository's `docker-compose.yml` is the authoritative, self-contained
deployment artifact. It is **pull-only** (no `build:`), pins the registry image,
and carries **every** environment variable inline — there is deliberately **no
`.env` file and no `env_file:`**. Edit the values in that file directly. The
essential shape:

```yaml
services:
  exchange-gateway:
    image: <registry>/exchange-gateway:0.1.0   # pinned registry image
    container_name: exchange-gateway
    restart: unless-stopped
    environment:
      - TZ=Europe/Amsterdam
      - GATEWAY_LOG_LEVEL=info
      - GATEWAY_BIND=[::]:8134
      - GATEWAY_CALDAV_BASE=http://stalwart:8080/dav
      - GATEWAY_JMAP_BASE=http://stalwart:8080/jmap
      - GATEWAY_CARDDAV_BASE=http://stalwart:8080/dav/card
      - GATEWAY_HMAC_SECRET=REPLACE_WITH_A_RANDOM_32_CHAR_MINIMUM_SECRET
      - GATEWAY_HOST=calendar.example.com       # placeholder for calendar.<your real domain>
      - GATEWAY_MAIL_DOMAIN=example.com          # placeholder for your real root / mail domain
      # Keep the EAS Ping long-poll under Cloudflare's 125s Proxy Read Timeout.
      - GATEWAY_MAX_PING_HEARTBEAT=80
      - GATEWAY_ALLOW_INSECURE_HTTP=1
    ports:
      # Loopback only: host-run cloudflared reaches the container via
      # http://localhost:8134; never publish on a wildcard bind.
      - 127.0.0.1:8134:8134
    volumes:
      # Host path the operator creates (must be writable by UID 10001 —
      # `chown 10001:10001 /armblock/exchange-gateway-data` before first start).
      - /armblock/exchange-gateway-data:/var/lib/exchange-gateway
    networks:
      - network1
    healthcheck:
      test: ["CMD", "curl", "-f", "http://localhost:8134/health/ready"]
      interval: 30s
      timeout: 5s
      retries: 3
      start_period: 10s

networks:
  network1:
    name: my-bridge-network
    external: true
```

> **Note on Database Path:** The database will be created at `/var/lib/exchange-gateway/gateway.db` inside the container. With the bind mount above, data persists across container restarts. Ensure the host path exists and is writable by the container's `gateway` user (UID 10001).

### Secrets (no `.env`)

Because there is no `.env` file, the credential-bearing variables
(`GATEWAY_HMAC_SECRET`, `GATEWAY_ADMIN_USERNAME`, `GATEWAY_ADMIN_PASSWORD`) live
inline in `docker-compose.yml`. Restrict that file's permissions
(`chmod 600 docker-compose.yml`). As an alternative, each of those variables has
a `<VAR>_FILE` companion that reads the value from a mounted file (Docker
secrets / mounted secret files), which takes precedence over the inline value.
The compose file is the authoritative template: every variable the binary
reads is listed there (active or commented with its default), so no `.env`
file is ever needed — do not create one.

```yaml
    environment:
      - GATEWAY_HMAC_SECRET_FILE=/run/secrets/gateway_hmac
    secrets:
      - gateway_hmac
secrets:
  gateway_hmac:
    file: ./secrets/gateway_hmac
```

### JMAP-first calendar (AUDIT3 §12)

The shipped compose sets `GATEWAY_PREFER_JMAP_CALENDAR=1` with
`GATEWAY_FORCE_CALDAV_CALENDAR=0`: calendar reads, writes and free/busy use
JMAP first with CalDAV as fallback. That configuration is correct **now**
that the JMAP calendar read/write defects are fixed (AUDIT3 §1–§2, §5–§7).
`GATEWAY_FORCE_CALDAV_CALENDAR=1` is an emergency escape hatch only: it
forces the CalDAV path and forfeits the JMAP-first mandate. Use it
temporarily if the backend's JMAP calendar path regresses — the
create/update write path also self-verifies every create/update by
read-back and auto-disables lossy JMAP writes at runtime (failing closed to
CalDAV for that request); deletes have no read-back verification and are
confirmed via `destroyed` — and remove the flag once the backend is
restored.

---

## Step 6: Start Services

```bash
# Pull the pinned image and start the container (no local build)
docker compose pull && docker compose up -d

# Start/restart cloudflared tunnel
sudo systemctl restart cloudflared

# Check cloudflared status
sudo systemctl status cloudflared
sudo journalctl -u cloudflared -f
```

---

## Step 7: Verify

### Local
```bash
curl -v http://127.0.0.1:8134/health
```

### Remote (after DNS propagates)
```bash
curl -v https://calendar.example.com/health
curl -v https://calendar.example.com/EWS/Exchange.asmx
curl -v https://calendar.example.com/autodiscover/autodiscover.xml
```

---

## Environment Variables Reference

### Exchange Gateway

| Variable | Description | Required | Default |
|----------|-------------|----------|---------|
| `GATEWAY_BIND` | Listen address | Yes | `[::]:8134` |
| `GATEWAY_CALDAV_BASE` | Stalwart CalDAV URL | Yes | - |
| `GATEWAY_HMAC_SECRET` | HMAC key (min 32 chars) | Yes | - |
| `GATEWAY_HOST` | Public hostname | Yes | - |
| `GATEWAY_MAIL_DOMAIN` | Mail domain | Yes | - |
| `GATEWAY_DATABASE_PATH` | SQLite path | No | `/var/lib/exchange-gateway/gateway.db` |
| `GATEWAY_MAX_ATTACHMENT_BYTES` | Max attachment size | No | `5242880` |
| `GATEWAY_ROOM_BOOKING_ENABLED` | Enable room booking | No | `true` |
| `GATEWAY_AUTH_CACHE_TTL_SECS` | Positive auth cache TTL (seconds; minimum `30`) | No | `300` |
| `GATEWAY_AUTH_CACHE_NEGATIVE_TTL_SECS` | Negative auth cache TTL base (seconds; up to 25% deterministic jitter is added, so the effective lifetime is `base`..`base`+25%); `0` disables negative caching | No | `30` |
| `GATEWAY_AUTH_CACHE_MAX_ENTRIES` | Auth cache max entries (also bounds the shared JMAP session-auth cache) | No | `10000` |
| `GATEWAY_AUTH_PROBE_MIN_INTERVAL_MS` | Minimum spacing between consecutive cold backend credential probes for one user (ms); bounds the per-request Basic-auth probe. Per-user, not global | No | `1000` |
| `GATEWAY_AUTH_BACKEND_FAILURE_THRESHOLD` | Consecutive backend-auth failures that open a user's circuit breaker (per-user, not global) | No | `5` |
| `GATEWAY_AUTH_BACKEND_MAX_BACKOFF_SECS` | Cap on the circuit breaker's exponential backoff (seconds); `0` disables the breaker | No | `60` |
| `GATEWAY_ALLOW_INSECURE_HTTP` | Permit plain HTTP to the backend (JMAP) | No | `false` |

### Boolean Values

Accepted as `true`: `1`, `true`, `yes`, `on`, `enabled` (case-insensitive)

---

## Database

The Exchange Gateway uses SQLite at `/var/lib/exchange-gateway/gateway.db`. The schema is auto-initialized on first startup.

---

## Security

### The Microsoft cloud sync path (read this before adding an account)

The Outlook **Android app** does **not** talk to the gateway directly from
the device for third-party (non-Microsoft-365) accounts. The account is
connected through **Microsoft's cloud sync infrastructure**: the app sends
the credentials you enter to Microsoft, and a Microsoft datacenter service
is then the EAS client that opens the connection to your gateway. The
Windows Outlook-class clients that use this gateway's EWS/MAPI/HTTP and
Autodiscover endpoints connect directly from the machine instead.

**New Outlook for Windows cannot reach this EAS endpoint at all.** Per
Microsoft's documentation:

- new Outlook for Windows supports Microsoft 365, Outlook.com/Hotmail,
  Gmail, Yahoo, iCloud and other accounts added via **IMAP or POP** — there
  is no EAS account type, and "On-premises Exchange accounts aren't
  supported" ([supported account
  types](https://learn.microsoft.com/en-us/microsoft-365-apps/outlook/get-started/supported-account-types));
- its "sync your account to the Microsoft Cloud" feature is documented for
  **Gmail and Yahoo** accounts only, while Outlook for Android (and iOS/Mac)
  additionally cover iCloud and IMAP ([sync to the Microsoft
  Cloud](https://support.microsoft.com/en-us/outlook/getstarted/sync-your-account-in-outlook-to-the-microsoft-cloud)).

For the clients that do use the cloud-sync route, that means:

- **Your mailbox password and the mail, calendar and contact data transit
  Microsoft's servers.** Do not deploy an account you are not willing to
  expose to Microsoft in this way.
- If the account offers "skip cloud sync" / direct-connection mode in the
  client UI and you enable it, the device connects itself and nothing
  transits Microsoft — but that is not the default behavior.
- Traffic arriving at the gateway therefore comes from **Microsoft ASNs**
  (and Cloudflare's edge in front of them), not from your phone's IP. Any
  firewall/allowlist must not block Microsoft's datacenter ranges, and the
  gateway's rate limiter (per edge-client-IP, default 120 requests/minute
  with a large burst) is sized so a Microsoft connector's Ping/Sync cadence
  never trips it. For many mailboxes on one gateway, raise
  `GATEWAY_RATE_LIMIT_REQUESTS_PER_MINUTE` if several users sync
  simultaneously.
- Microsoft's connector behaves like a patient, long-running EAS client:
  long-poll Pings (held below Cloudflare's 125-second edge timeout,
  see the *EAS Ping* section below) and large sync windows (the gateway
  honors `WindowSize` up to the protocol maximum of 512 items per
  collection, sending `<MoreAvailable/>` and a resumable SyncKey when a
  folder is bigger than one window). No configuration is needed on either
  side.

### Cloudflare Dashboard Settings

1. **Proxy status**: Set to Proxied (orange cloud) for DDoS protection
2. **SSL/TLS**: Set to "Full" or "Full Strict"
3. **TLS 1.3**: Enable in SSL/TLS → Edge Certificates

### Firewall

Block non-Cloudflare traffic to your server at the firewall level. Cloudflare tunnel traffic appears as localhost traffic from cloudflared.

---

## Troubleshooting

### Tunnel won't connect

```bash
# Check cloudflared logs
sudo journalctl -u cloudflared -f

# Verify token is correct
cloudflared tunnel info

# Check if cloudflared is running
ps aux | grep cloudflared
```

### TLS/SSL Handshake Failure

**Symptoms:** `SSL routines::sslv3 alert handshake failure` or `ERR_SSL_VERSION_OR_CIPHER_MISMATCH`

**Common causes:**

1. **Hostname not covered by SSL certificate:**

   Cloudflare Universal SSL certificates (for `*.example.com`) do **NOT** cover third-level subdomains. For example:
   - `calendar.example.com` is covered by `*.example.com`
   - `calendar.example.com` is NOT covered

   **Fix:** Use a second-level subdomain (e.g., `calendar.example.com`) or purchase a dedicated SSL certificate for the specific hostname.

2. **SSL certificate not yet provisioned:**

   Wait 5-15 minutes for Cloudflare to provision the Universal SSL certificate.

3. **SSL/TLS mode mismatch:**

   Ensure SSL/TLS mode is set to "Full" or "Flexible" (not "Full (strict)") when using HTTP origin.

### Certificate Coverage Reference

| Certificate Type | Covers |
|-----------------|--------|
| `*.example.com` | `a.example.com`, `calendar.example.com` |
| `*.stalwart.example.com` | `calendar.example.com`, `mail.stalwart.example.com` |
| Dedicated cert for `calendar.example.com` | Only `calendar.example.com` |

**Recommendation:** Use second-level subdomains (e.g., `calendar.example.com`) to ensure compatibility with Cloudflare's free Universal SSL certificates.

### 502 Bad Gateway

```bash
# Verify Exchange Gateway is running
docker compose ps

# Check gateway logs
docker compose logs exchange-gateway

# Test locally
curl http://127.0.0.1:8134/health
```

### DNS not resolving

```bash
dig calendar.example.com
nslookup calendar.example.com
```

---

## Cloudflare Tunnel Ingress Configuration Reference

The `cloudflared/config.yml` file provides a template for ingress rules. Key configuration options:

| Field | Description |
|-------|-------------|
| `tunnel` | Your tunnel UUID |
| `credentials-file` | Path to tunnel credentials JSON |
| `hostname` | Public hostname for this route |
| `service` | Backend service URL (http://localhost:8134) |
| `originRequest.noTLSVerify` | Whether to skip TLS verification (false = verify) |
| `originRequest.connectTimeout` | Connection timeout to origin |
| `originRequest.httpHostHeader` | Host header to send to origin |
| `originRequest.originServerName` | Expected TLS certificate CN/SAN |

---

## EAS Ping vs. Cloudflare's Proxy Read Timeout

Cloudflare's proxied HTTP (which every cloudflared tunnel request traverses)
terminates any request that receives no response within the default
**125-second Proxy Read Timeout** with **HTTP 524** (the default is not
adjustable on free/pro plans; Enterprise zones can raise it). EAS Ping asks
the server to hold the connection for `HeartbeatInterval` seconds (legal
range 60–3540, and both Outlook clients request far more than 125s), so
without mitigation every long Ping is cut at the edge: push appears "synced" but mail arrives late,
and the clients retry-loop on 524s (battery drain).

The gateway handles this automatically:

- **Detection:** requests arriving through Cloudflare carry a `CF-RAY`
  header. When detected, the effective Ping hold time is clamped to
  **80 seconds** and the Ping answers `Status 1` before the edge cutoff; the
  client immediately re-issues Ping, which is fully spec-legal (MS-ASCMD
  allows the server to end a Ping at any time). The protocol-level heartbeat
  negotiation is untouched — direct (non-Cloudflare) connections keep the
  full client-requested heartbeat.
- **Override:** set `GATEWAY_MAX_PING_HEARTBEAT=<seconds>` on the gateway to
  choose the cap yourself. When set, the cap applies to **all** Pings (with
  or without `CF-RAY` detection); it must be `> 0`. Keep it below the
  125-second Proxy Read Timeout on the Cloudflare edge.

No client-side configuration is required; Outlook for Windows and Outlook
Android simply re-ping slightly more often.

---

## Resources

- [Cloudflare Tunnel Documentation](https://developers.cloudflare.com/cloudflare-one/connections/connect-networks/)
- [Cloudflare Dashboard](https://dash.cloudflare.com/)