# CLOUDFLARED_SETUP.md

# Cloudflare Tunnel Setup Guide for Exchange Gateway

This guide configures Cloudflare Tunnel to expose the Exchange Gateway container via HTTPS. cloudflared runs natively on Ubuntu Server 24.04 LTS (not as a container). All container configuration is done via environment variables in Docker Compose.

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
7. **Copy the tunnel token** — you'll use this in your `.env` file
8. **Save the tunnel UUID** — you'll use this if deploying with config file

---

## Step 2: Configure Public Hostnames (Android "Exchange" signup requires TWO)

Android's "Exchange" account wizard and Microsoft AutoDetect (the cloud
probing service used by Outlook Android and New Outlook for Windows)
derive discovery addresses from **the domain part of the email address the
user types** — i.e. your `GATEWAY_MAIL_DOMAIN` — per MS-OXDSCLI §2.2.3:

1. `https://autodiscover.<mail-domain>/autodiscover/autodiscover.xml`
   (primary probe — **mandatory**),
2. `https://<mail-domain>/autodiscover/autodiscover.xml`
   (root-domain fallback probe — some Android builds try it when the first
   fails or stalls; publishing it makes signup succeed faster and more
   predictably).

Both probes must reach the gateway, and the gateway's Autodiscover response
(per `src/autodiscover.rs`, `ResponseSchema::MobileSync` detection) answers
MobileSync clients with `<Url>https://$GATEWAY_HOST/Microsoft-Server-ActiveSync</Url>`
— so `GATEWAY_HOST` itself must also be a public hostname on the tunnel.

**Scope note — one discovery setup per mailbox domain.** The sign-up flow
described here works for mailboxes under `GATEWAY_MAIL_DOMAIN`, because that
is the (only) domain for which Steps 2–3 publish discovery DNS records,
tunnel ingress routes, and TLS. The mobilesync response itself returns the
`GATEWAY_HOST` URL for **any** email domain (verified by
`mobilesync_response_points_at_gateway_host_for_any_mailbox_domain` in
`src/autodiscover.rs`), but that only means no per-domain changes are needed
*in the gateway response* — it does not make `autodiscover.<some other
domain>` resolvable. To also support Android signup for an additional
mailbox domain (e.g. `user@example.org`), repeat Hostnames B/C and the
CNAME records below **for that domain**: add `autodiscover.example.org`
(and optionally the `example.org` apex) as public hostnames on the tunnel
with matching ingress entries and Cloudflare-proxied DNS records, and
ensure Cloudflare issues a TLS certificate for those hostnames (Universal
SSL on that zone, or move the zone to Cloudflare). Without those per-domain
records, Android's first probe for that domain never reaches the gateway,
regardless of what the Autodiscover response contains.

In the same tunnel settings (**Public Hostname** tab), add **two** (or
**three**) entries, each pointing at the gateway:

#### Hostname A — EAS service hostname (`GATEWAY_HOST`)

- **Subdomain/Domain**: `calendar.example.com` (the value you will set as
  `GATEWAY_HOST` in `.env`)
- **Type**: HTTP
- **Service**: `http://localhost:8134`

#### Hostname B — Autodiscover subdomain (`autodiscover.<GATEWAY_MAIL_DOMAIN>`) — REQUIRED for Android

- **Subdomain/Domain**: `autodiscover.example.com`
  (i.e. `autodiscover.` + the exact `GATEWAY_MAIL_DOMAIN` from `.env`)
- **Type**: HTTP
- **Service**: `http://localhost:8134`

#### Hostname C — root-domain fallback (`<GATEWAY_MAIL_DOMAIN>`) — recommended

- **Subdomain/Domain**: `example.com` (the bare `GATEWAY_MAIL_DOMAIN`)
- **Type**: HTTP
- **Service**: `http://localhost:8134`

> Only add Hostname C if the bare mail domain may be served by this tunnel
> (skip it if the root domain must serve a website or other content).
> The Android wizard proceeds via Hostname B alone when B answers.

---

## Step 3: Create DNS Records

Create one **CNAME** per public hostname from Step 2, all pointing at the
same tunnel target (`<tunnel-id>.cfargotunnel.com`, shown in the tunnel
settings). For `GATEWAY_MAIL_DOMAIN=example.com` and
`GATEWAY_HOST=calendar.example.com`:

| Type  | Name                        | Target                              | Proxy status              |
|-------|-----------------------------|-------------------------------------|---------------------------|
| CNAME | `calendar` (GATEWAY_HOST)   | `<tunnel-id>.cfargotunnel.com`      | Proxied                   |
| CNAME | `autodiscover`              | `<tunnel-id>.cfargotunnel.com`      | Proxied                   |
| CNAME | `@` (root, optional)        | `<tunnel-id>.cfargotunnel.com`      | Proxied (CNAME flattening)|

All three records MUST be **Proxied** from the start. A DNS-only record
pointing at `*.cfargotunnel.com` never reaches the tunnel: tunnel routing
happens only inside Cloudflare's edge, so a DNS-only hostname resolves to a
CF any-cast address that does not answer HTTP for this service and the
verification checks below (and the Android wizard itself) would fail
against the intended public HTTPS path. Public TLS is provided by
Cloudflare's Proxied mode via Universal SSL; the gateway's Cloudflare-mode
Ping clamp documented below likewise only engages on Proxied traffic.

The apex (`@` / root) record relies on Cloudflare's CNAME flattening, which
is supported automatically on Cloudflare-hosted zones.

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
# (i.e. GATEWAY_HOST), regardless of the email domain queried.

# Root-domain fallback (Hostname C, only if added) — Android POSTs the same
# MobileSync Autodiscover request here per MS-OXDSCLI fallback rules, so
# verify the identical POST + URL assertion instead of a bare GET (a GET
# carries no AcceptableResponseSchema and cannot prove the POST probe
# Android actually sends will succeed).
curl --fail-with-body -sS -X POST \
  -H 'Content-Type: text/xml' \
  -d "$MOBILESYNC_XML" \
  https://example.com/autodiscover/autodiscover.xml \
  | grep -F "$EXPECTED_URL"

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
   substitutes the tunnel UUID and both hostnames from your `.env` plus an
   exported `TUNNEL_ID` in a single `sed` pass, writing the result to
   `~/.cloudflared/config.yml` directly from the template:
   - `<YOUR-TUNNEL-UUID>` → your tunnel UUID from Step 1 (`TUNNEL_ID`)
   - `<GATEWAY_MAIL_DOMAIN>` → the `GATEWAY_MAIL_DOMAIN` from `.env`
     (drives the `autodiscover.` and root-fallback ingress entries)
   - `<GATEWAY_HOST>` → the `GATEWAY_HOST` from `.env`
   (the config file carries the two required hostnames from Step 2 plus the
   optional root-domain fallback entry — delete that ingress block if the
   bare mail domain must serve other content)
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

```yaml
# docker-compose.yml
services:
  exchange-gateway:
    build: .
    container_name: exchange-gateway
    image: exchange-gateway:latest
    restart: unless-stopped
    ports:
      - 8134:8134
    environment:
      - GATEWAY_LOG_LEVEL=${GATEWAY_LOG_LEVEL:-info}
      - TZ=${GATEWAY_TZ:-UTC}
      - GATEWAY_BIND=${GATEWAY_BIND:-[::]:8134}
      - GATEWAY_CALDAV_BASE=${GATEWAY_CALDAV_BASE}
      - GATEWAY_HMAC_SECRET=${GATEWAY_HMAC_SECRET}
      - GATEWAY_HOST=${GATEWAY_HOST}
      - GATEWAY_MAIL_DOMAIN=${GATEWAY_MAIL_DOMAIN}
      - GATEWAY_DATABASE_PATH=${GATEWAY_DATABASE_PATH:-/var/lib/exchange-gateway/gateway.db}
      - GATEWAY_MAX_ATTACHMENT_BYTES=${GATEWAY_MAX_ATTACHMENT_BYTES:-5242880}
      - GATEWAY_ROOM_BOOKING_ENABLED=${GATEWAY_ROOM_BOOKING_ENABLED:-true}
      - GATEWAY_AUTH_CACHE_TTL_SECS=${GATEWAY_AUTH_CACHE_TTL_SECS:-300}
      - GATEWAY_AUTH_CACHE_MAX_ENTRIES=${GATEWAY_AUTH_CACHE_MAX_ENTRIES:-10000}
      - GATEWAY_ALLOW_INSECURE_HTTP=${GATEWAY_ALLOW_INSECURE_HTTP:-1}
    volumes:
      - exchange-gateway-data:/var/lib/exchange-gateway
    healthcheck:
      test: ["CMD", "curl", "-f", "http://localhost:8134/health"]
      interval: 30s
      timeout: 5s
      retries: 3
      start_period: 10s

volumes:
  exchange-gateway-data:
    driver: local
```

> **Note on Database Path:** The database will be created at `/var/lib/exchange-gateway/gateway.db` inside the container. With the named volume above, data persists across container restarts. If using a bind mount instead (e.g., `- /path/on/host:/var/lib/exchange-gateway`), ensure the host path exists and is writable by the container's `gateway` user (UID 10001).

### `.env` File

```bash
# Exchange Gateway Configuration (Required)
GATEWAY_CALDAV_BASE=http://stalwart:8080/dav/
GATEWAY_HMAC_SECRET=your-32-character-minimum-secret-key-here
GATEWAY_HOST=calendar.example.com
GATEWAY_MAIL_DOMAIN=example.com

# Optional Exchange Gateway Settings
GATEWAY_LOG_LEVEL=info
GATEWAY_TZ=UTC
GATEWAY_BIND=[::]:8134
GATEWAY_DATABASE_PATH=/var/lib/exchange-gateway/gateway.db
GATEWAY_MAX_ATTACHMENT_BYTES=5242880
GATEWAY_ROOM_BOOKING_ENABLED=true
GATEWAY_AUTH_CACHE_TTL_SECS=300
GATEWAY_AUTH_CACHE_MAX_ENTRIES=10000
# Plain HTTP to the Stalwart backend (the JMAP URL derived from CALDAV_BASE)
# carries credentials in cleartext. Set to 1 to permit the non-TLS
# http://stalwart:8080 model only when the gateway and Stalwart share a
# trusted private network; otherwise use HTTPS (e.g. https://stalwart:8443).
GATEWAY_ALLOW_INSECURE_HTTP=1
```

---

## Step 6: Start Services

```bash
# Start the Exchange Gateway container
docker compose up -d --build

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
| `GATEWAY_AUTH_CACHE_TTL_SECS` | Auth cache TTL | No | `300` |
| `GATEWAY_AUTH_CACHE_MAX_ENTRIES` | Auth cache max entries | No | `10000` |
| `GATEWAY_ALLOW_INSECURE_HTTP` | Permit plain HTTP to the backend (JMAP) | No | `false` |

### Boolean Values

Accepted as `true`: `1`, `true`, `yes`, `on`, `enabled` (case-insensitive)

---

## Database

The Exchange Gateway uses SQLite at `/var/lib/exchange-gateway/gateway.db`. The schema is auto-initialized on first startup.

---

## Security

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