# Cloudflare Tunnel Setup Guide for Exchange Gateway

This guide configures Cloudflare Tunnel to expose the Exchange Gateway container via HTTPS. cloudflared runs natively on Ubuntu Server 24.04 LTS (not as a container). All container configuration is done via environment variables in Docker Compose.

**No config files, credentials files, or Cloudflare Workers are needed.**

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

---

## Step 2: Configure Public Hostname

In the same tunnel settings:

1. Go to the **Public Hostname** tab
2. Click **Add a public hostname**
3. Configure:
   - **Domain**: `calendar.stalwart.example.com` (subdomain of your choice)
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

## Step 4: Create cloudflared systemd Service

Create the service file:

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

Replace `<YOUR_TUNNEL_TOKEN>` with the token from Step 1, or use the environment variable approach below.

### Using Environment Variable for Token

To avoid hardcoding the token, edit `/etc/systemd/system/cloudflared.service`:

```ini
[Service]
Environment="TUNNEL_TOKEN=<YOUR_TUNNEL_TOKEN>"
ExecStart=/usr/local/bin/cloudflared tunnel run --token ${TUNNEL_TOKEN}
```

Then enable and start:

```bash
sudo systemctl daemon-reload
sudo systemctl enable cloudflared
sudo systemctl start cloudflared
sudo systemctl status cloudflared
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
      - RUST_LOG=${GATEWAY_RUST_LOG:-info}
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
GATEWAY_HOST=calendar.stalwart.example.com
GATEWAY_MAIL_DOMAIN=example.com

# Optional Exchange Gateway Settings
GATEWAY_RUST_LOG=info
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
curl -v https://calendar.stalwart.example.com/health
curl -v https://calendar.stalwart.example.com/EWS/Exchange.asmx
curl -v https://calendar.stalwart.example.com/autodiscover/autodiscover.xml
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
dig calendar.stalwart.example.com
nslookup calendar.stalwart.example.com
```

---

## Resources

- [Cloudflare Tunnel Documentation](https://developers.cloudflare.com/cloudflare-one/connections/connect-networks/)
- [Cloudflare Dashboard](https://dash.cloudflare.com/)
