<!-- CLOUDFLARE_DEPLOYMENT.md -->

# Cloudflare Deployment Guide

## Overview

The Exchange Gateway uses Cloudflare Tunnel (cloudflared) for TLS termination. Cloudflare terminates TLS at their edge network, and the tunnel provides encrypted transport to your origin server. No Cloudflare Workers are required.

## Architecture

```
Outlook/Exchange Client
        |
        v (HTTPS)
Cloudflare Edge (TLS terminated automatically)
        |
        v (Encrypted HTTP/2 via tunnel)
cloudflared Tunnel -> exchange-gateway container (HTTP on port 8134)
        |
        v
SQLite Database
        |
        v
Stalwart CalDAV
```

## Prerequisites

- Cloudflare account (free tier)
- Domain managed by Cloudflare DNS
- Stalwart Mailserver v0.15.5 running with RocksDB and ACME TLS
- Exchange Gateway Docker container running alongside Stalwart
- cloudflared installed on the Docker host

## Step 1: Install cloudflared

```bash
curl -L https://github.com/cloudflare/cloudflared/releases/latest/download/cloudflared-linux-amd64 -o /usr/local/bin/cloudflared
chmod +x /usr/local/bin/cloudflared
cloudflared --version
```

## Step 2: Create a Cloudflare Tunnel

### Option A: Using Cloudflare Dashboard

1. Log into [Cloudflare Dashboard](https://dash.cloudflare.com/)
2. Navigate to **Networks → Tunnels**
3. Click **Create a tunnel**
4. Select **Cloudflared** as the connector type
5. Choose your account/domain
6. Name your tunnel (e.g., `exchange-gateway-tunnel`)
7. Copy the tunnel credentials JSON file

### Option B: Using Command Line

```bash
# Authenticate with Cloudflare
cloudflared tunnel login

# Create tunnel
cloudflared tunnel create exchange-gateway-tunnel

# Note the tunnel ID from output
cloudflared tunnel list
```

## Step 3: Configure Ingress Rules

Create `/etc/cloudflared/config.yml`:

```yaml
tunnel: <TUNNEL_ID>
credentials-file: /etc/cloudflared/credentials.json

ingress:
  - hostname: calendar.stalwart.example.com
    path: "(^/EWS.*|^/ews.*|^/autodiscover.*|^/Microsoft-Server-ActiveSync.*|^/OAB.*|^/Autodiscover.*)"
    service: http://exchange-gateway:8134
    originRequest:
      noTLSVerify: false
      connectTimeout: 30s

  - hostname: calendar.stalwart.example.com
    path: "^/health$"
    service: http://exchange-gateway:8134

  - hostname: calendar.stalwart.example.com
    path: "^/$"
    service: http_status:302
    originRequest:
      redirect: https://calendar.stalwart.example.com/health

  - service: http_status:404
```

## Step 4: Configure DNS

1. In Cloudflare Dashboard, go to your domain's **DNS** settings
2. Add a CNAME record:
   - **Name**: `calendar`
   - **Target**: `<TUNNEL_ID>.cfargotunnel.com`
   - **Proxy status**: Proxied (orange cloud)

## Step 5: Run cloudflared

### As Systemd Service (Recommended)

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
ExecStart=/usr/local/bin/cloudflared tunnel run --config /etc/cloudflared/config.yml
Restart=on-failure
RestartSec=5s

[Install]
WantedBy=multi-user.target
```

```bash
sudo systemctl daemon-reload
sudo systemctl enable cloudflared
sudo systemctl start cloudflared
sudo systemctl status cloudflared
```

### Using Docker Compose

Use the provided `examples/docker-compose.yml`:

```bash
docker compose -f examples/docker-compose.yml up -d
docker compose -f examples/docker-compose.yml logs -f cloudflared
```

## Step 6: Verify Deployment

1. Test health endpoint:
```bash
curl https://calendar.stalwart.example.com/health
```

2. Test Autodiscover:
```bash
curl -X POST https://calendar.stalwart.example.com/Autodiscover/Autodiscover.xml \
  -H "Content-Type: text/xml" \
  -d '<Autodiscover xmlns="http://schemas.microsoft.com/exchange/autodiscover/outlook/requestschema/2006"><Request><EMailAddress>user@example.com</EMailAddress><AcceptableResponseSchema>http://schemas.microsoft.com/exchange/autodiscover/outlook/responseschema/2006a</AcceptableResponseSchema></Request></Autodiscover>'
```

3. Verify TLS (should show Cloudflare certificate):
```bash
openssl s_client -connect calendar.stalwart.example.com:443 -servername calendar.stalwart.example.com 2>/dev/null | openssl x509 -noout -dates
```

## Security Model

1. **TLS**: Cloudflare provides automatic TLS termination. All client-facing endpoints use HTTPS.

2. **Authentication**: EWS and ActiveSync requests require Basic authentication validated against Stalwart.

3. **Security Headers**: The gateway sets:
   - `X-Content-Type-Options: nosniff`
   - `X-Frame-Options: DENY`
   - `Content-Security-Policy: default-src 'none'`
   - `Strict-Transport-Security: max-age=63072000; includeSubDomains`
   - `Referrer-Policy: strict-origin-when-cross-origin`
   - `Cache-Control: private, no-store`

4. **Request Size Limits**: 4 MB maximum body size enforced.

## Free-Tier Limits

Cloudflare free tier limits:
- Unlimited tunnels
- 3 tunnels per account
- No bandwidth limits for tunnels

The tunnel approach has no usage-based limits, unlike Workers (100,000/day).

## Troubleshooting

### Tunnel Connection Fails
```bash
# Check logs
sudo journalctl -u cloudflared -n 100

# Test manually
cloudflared tunnel run --config /etc/cloudflared/config.yml --logle debug
```

### 502 Bad Gateway
- Verify Exchange Gateway is running: `docker ps`
- Check container logs: `docker logs exchange-gateway`
- Test locally: `curl http://localhost:8134/health`

### DNS Not Resolving
```bash
dig calendar.stalwart.example.com
nslookup calendar.stalwart.example.com
```

## Stalwart Configuration Additions

Add to your Stalwart `config.toml`:

```toml
[server.socket."0.0.0.0:8080"]
protocol = "http"

[server.socket."[::]:8080"]
protocol = "http"

[authentication.fallback-admin]
enable = true

[storage]
backend = "rocksdb"

[certificate.acme]
enable = true

[lookup.default]
domain = "example.com"

[jmap]
enable = true

[dav]
enable = true
```

See `stalwart-additions.toml` for the complete set of additions.

## Database

The Exchange Gateway uses SQLite at `/var/lib/exchange-gateway/gateway.db`. Schema is auto-initialized on first startup.