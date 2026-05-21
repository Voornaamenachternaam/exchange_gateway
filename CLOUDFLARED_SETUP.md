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

## Step 2: Configure Public Hostname

In the same tunnel settings:

1. Go to the **Public Hostname** tab
2. Click **Add a public hostname**
3. Configure:
   - **Domain**: `calendar.example.com` (subdomain of your choice)
   - **Type**: HTTP
   - **Service**: `http://localhost:8134`
4. Click **Save hostname**

---

## Step 3: Create DNS Record

1. Go to **Websites → yourdomain.com → DNS**
2. Click **Add record**
3. Select **CNAME**
4. Configure:
   - **Name**: `calendar`
   - **Target**: `<tunnel-id>.cfargotunnel.com` (shown in tunnel settings)
   - **Proxy status**: DNS only (initially), switch to Proxied after testing
5. Save

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

1. Copy the ingress configuration:
```bash
mkdir -p ~/.cloudflared
cp cloudflared/config.yml ~/.cloudflared/config.yml
```

2. Edit `~/.cloudflared/config.yml` to replace:
   - `<YOUR-TUNNEL-UUID>` with your tunnel UUID from Step 1
   - `calendar.example.com` with your actual hostname

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

## Resources

- [Cloudflare Tunnel Documentation](https://developers.cloudflare.com/cloudflare-one/connections/connect-networks/)
- [Cloudflare Dashboard](https://dash.cloudflare.com/)