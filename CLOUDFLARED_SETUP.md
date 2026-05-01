# Cloudflare Tunnel Setup Guide for Exchange Gateway

This guide configures Cloudflare Tunnel (cloudflared) to provide TLS termination and route traffic to the Exchange Gateway container. No Cloudflare Workers are required.

## Architecture Overview

```
Outlook Client (HTTPS)
        ↓
Cloudflare Edge (TLS terminated by default)
        ↓
Cloudflare Tunnel (encrypted HTTP/2)
        ↓
cloudflared (your server)
        ↓
Exchange Gateway Container (HTTP on port 8134)
```

Cloudflare automatically terminates TLS at their edge. The tunnel provides encrypted transport from edge to your origin.

---

## Prerequisites

- Cloudflare account with domain (e.g., stalwart.example.com)
- cloudflared installed on your server
- Docker and Docker Compose installed
- Exchange Gateway container running

---

## Step 1: Install cloudflared

### Ubuntu/Debian

```bash
curl -L https://github.com/cloudflare/cloudflared/releases/latest/download/cloudflared-linux-amd64 -o /usr/local/bin/cloudflared
chmod +x /usr/local/bin/cloudflared
```

### Verify Installation

```bash
cloudflared --version
```

---

## Step 2: Create a Cloudflare Tunnel

### Option A: Using Cloudflare Dashboard (Recommended for Free Tier)

1. Log into [Cloudflare Dashboard](https://dash.cloudflare.com/)
2. Navigate to **Networks → Tunnels**
3. Click **Create a tunnel**
4. Select **Cloudflared** as the connector type
5. Choose your account/domain
6. Name your tunnel (e.g., `exchange-gateway-tunnel`)
7. Copy the tunnel token (you'll need it below)

### Option B: Using Command Line

```bash
# Authenticate with Cloudflare
cloudflared tunnel login

# Create tunnel
cloudflared tunnel create exchange-gateway-tunnel

# Note the tunnel ID from output
```

---

## Step 3: Configure Ingress Rules

Create the cloudflared configuration file:

```bash
sudo mkdir -p /etc/cloudflared
sudo nano /etc/cloudflared/config.yml
```

### Configuration File: `/etc/cloudflared/config.yml`

```yaml
# Cloudflare Tunnel Configuration for Exchange Gateway
# This file configures routing for calendar protocols through the tunnel

# Tunnel connection details
tunnel: <TUNNEL_ID>
credentials-file: /etc/cloudflared/credentials.json

# Ingress rules define how requests are routed
# Rules are matched in order, first match wins
ingress:
  # Rule 1: All calendar protocol paths to Exchange Gateway
  # Matches: /EWS/*, /ews/*, /autodiscover/*, /Microsoft-Server-ActiveSync/*, /OAB/*
  - hostname: calendar.stalwart.example.com
    path: "(^/EWS.*|^/ews.*|^/autodiscover.*|^/Microsoft-Server-ActiveSync.*|^/OAB.*|^/Autodiscover.*)"
    service: http://exchange-gateway:8134
    originRequest:
      noTLSVerify: false
      connectTimeout: 30s
      tlsTimeout: 10s

  # Rule 2: Health check endpoint
  - hostname: calendar.stalwart.example.com
    path: "^/health$"
    service: http://exchange-gateway:8134
    originRequest:
      noTLSVerify: false

  # Rule 3: Root path with redirect to health
  - hostname: calendar.stalwart.example.com
    path: "^/$"
    service: http_status:302
    originRequest:
      redirect: https://calendar.stalwart.example.com/health

  # Rule 4: Catch-all for unhandled paths (return 404)
  - service: http_status:404
```

### Alternative: Simple Configuration (Single Rule)

For simpler setups without regex path matching:

```yaml
tunnel: <TUNNEL_ID>
credentials-file: /etc/cloudflared/credentials.json

ingress:
  # Route all traffic for this hostname to the Exchange Gateway
  - hostname: calendar.stalwart.example.com
    service: http://exchange-gateway:8134
    originRequest:
      noTLSVerify: false
      connectTimeout: 30s

  # Default fallback
  - service: http_status:404
```

---

## Step 4: Run cloudflared

### As a Systemd Service (Recommended for Production)

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
ExecStart=/usr/local/bin/cloudflared tunnel run --config /etc/cloudflared/config.yml
Restart=on-failure
RestartSec=5s
User=root

[Install]
WantedBy=multi-user.target
```

Enable and start the service:

```bash
sudo systemctl daemon-reload
sudo systemctl enable cloudflared
sudo systemctl start cloudflared
sudo systemctl status cloudflared
```

### Using Docker Compose

Add cloudflared to your Docker Compose configuration:

```yaml
# docker-compose.yml (partial)
services:
  exchange-gateway:
    image: exchange-gateway:latest
    container_name: exchange-gateway
    ports:
      - "127.0.0.1:8134:8134"
    volumes:
      - ./config.toml:/etc/exchange-gateway/config.toml:ro
      - ./data:/var/lib/exchange-gateway
    restart: unless-stopped
    healthcheck:
      test: ["CMD", "curl", "-f", "http://localhost:8134/health"]
      interval: 30s
      timeout: 5s
      retries: 3

  cloudflared:
    image: cloudflare/cloudflared:latest
    container_name: cloudflared-tunnel
    restart: unless-stopped
    command: tunnel run --config /etc/cloudflared/config.yml
    volumes:
      - ./cloudflared/config.yml:/etc/cloudflared/config.yml:ro
      - ./cloudflared/credentials.json:/etc/cloudflared/credentials.json:ro
    depends_on:
      - exchange-gateway
    network_mode: service:exchange-gateway
```

Create the credentials file:

```bash
# Get credentials from Cloudflare Dashboard
# Networks → Tunnels → Your Tunnel → Access Token
cat > ./cloudflared/credentials.json << 'EOF'
{
  "AccountTag": "<YOUR_ACCOUNT_TAG>",
  "TunnelID": "<TUNNEL_ID>",
  "TunnelName": "exchange-gateway-tunnel",
  "TunnelSecret": "<YOUR_TUNNEL_SECRET>"
}
EOF
chmod 600 ./cloudflared/credentials.json
```

---

## Step 5: Configure DNS in Cloudflare Dashboard

After creating the tunnel, you need to create a DNS record:

1. Navigate to **Websites → stalwart.example.com → DNS**
2. Click **Add record**
3. Select **CNAME** record
4. Set:
   - **Name**: `calendar`
   - **Target**: `<your-tunnel-id>.cfargotunnel.com`
   - **Proxy status**: DNS only (for now) OR Proxied (after testing)

5. Click **Save**

### Verify Tunnel Connection

```bash
# Check tunnel status
sudo journalctl -u cloudflared -f

# Or directly
cloudflared tunnel list
cloudflared tunnel info exchange-gateway-tunnel
```

---

## Step 6: Test the Setup

### Local Test

```bash
# Test health endpoint locally
curl -v http://localhost:8134/health

# Test with headers showing origin
curl -v -H "Host: calendar.stalwart.example.com" http://localhost:8134/health
```

### Remote Test (After DNS Propagation)

```bash
# Test EWS endpoint
curl -v https://calendar.stalwart.example.com/EWS/Exchange.asmx

# Test AutoDiscover
curl -v https://calendar.stalwart.example.com/autodiscover/autodiscover.xml

# Verify TLS certificate (should show Cloudflare)
openssl s_client -connect calendar.stalwart.example.com:443 -servername calendar.stalwart.example.com 2>/dev/null | openssl x509 -noout -dates
```

---

## Security Considerations

### TLS Configuration

Cloudflare handles TLS automatically. For enhanced security:

1. **Always use Proxied mode** (orange cloud) - enables Cloudflare's DDoS protection
2. **Enable TLS 1.3 only** in Cloudflare Dashboard:
   - SSL/TLS → Overview → Custom certificate (optional)
   - SSL/TLS → Edge Certificates → TLS 1.3

### Ingress Security

```yaml
ingress:
  - hostname: calendar.stalwart.example.com
    service: http://exchange-gateway:8134
    originRequest:
      # Verify TLS certificate from origin (if origin uses HTTPS)
      noTLSVerify: false
      
      # Restrict IP ranges (optional)
      # originIP: "10.0.0.0/8"  # Only allow from internal network
      
      connectTimeout: 30s
      tlsTimeout: 10s
```

### Firewall Rules

Cloudflare Dashboard → Security → WAF:

```bash
# Recommended Cloudflare Rules
# Block all non-Cloudflare traffic at firewall level
# This ensures only Cloudflare can reach your origin
```

---

## Troubleshooting

### Common Issues

#### 1. Tunnel Connection Fails

```bash
# Check logs
sudo journalctl -u cloudflared -n 100

# Verify credentials
cat /etc/cloudflared/credentials.json

# Test tunnel manually
cloudflared tunnel run --config /etc/cloudflared/config.yml --logle debug
```

#### 2. 502 Bad Gateway

- Verify Exchange Gateway is running: `docker ps`
- Check Exchange Gateway logs: `docker logs exchange-gateway`
- Test locally: `curl http://exchange-gateway:8134/health`

#### 3. DNS Not Resolving

```bash
# Check DNS propagation
dig calendar.stalwart.example.com

# Verify CNAME target
nslookup calendar.stalwart.example.com
```

#### 4. TLS Certificate Errors

Cloudflare handles TLS at edge. If you see certificate errors:
- Ensure DNS is proxied (orange cloud)
- Check SSL/TLS mode in Cloudflare Dashboard (Full or Full Strict recommended)

### Debug Commands

```bash
# Full tunnel diagnostics
cloudflared tunnel diagnostics <TUNNEL_ID>

# Test ingress rules
cloudflared tunnel ingress test https://calendar.stalwart.example.com/EWS/Exchange.asmx

# Check Cloudflare edge connectivity
curl -v https://calendar.stalwart.example.com/health \
  -H "CF-Ray: test" \
  -H "CF-Connecting-IP: 1.1.1.1"
```

---

## Stalwart Configuration

To enable CalDAV access for the Exchange Gateway, add these settings to your Stalwart `config.toml`:

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

Replace `example.com` with your actual mail domain.

## Database

The Exchange Gateway uses SQLite at `/var/lib/exchange-gateway/gateway.db`. Schema is auto-initialized on first startup.

---

## Full Docker Compose Example

```yaml
# docker-compose.yml
services:
  exchange-gateway:
    build:
      context: .
      dockerfile: Dockerfile
    container_name: exchange-gateway
    image: exchange-gateway:latest
    restart: unless-stopped
    ports:
      - "127.0.0.1:8134:8134"  # Local-only, accessed via tunnel
    environment:
      - RUST_LOG=info
      - TZ=UTC
    volumes:
      - ./config.toml:/etc/exchange-gateway/config.toml:ro
      - ./data:/var/lib/exchange-gateway
    healthcheck:
      test: ["CMD", "curl", "-f", "http://localhost:8134/health"]
      interval: 30s
      timeout: 5s
      retries: 3
      start_period: 10s

  cloudflared:
    image: cloudflare/cloudflared:latest
    container_name: cloudflared-tunnel
    restart: unless-stopped
    depends_on:
      exchange-gateway:
        condition: service_healthy
    command: tunnel run --config /etc/cloudflared/config.yml
    volumes:
      - ./cloudflared/config.yml:/etc/cloudflared/config.yml:ro
      - ./cloudflared/credentials.json:/etc/cloudflared/credentials.json:ro
    network_mode: service:exchange-gateway

networks:
  default:
    name: exchange-gateway-network
```

---

## Environment Variables Reference

### Exchange Gateway Configuration

All Exchange Gateway settings can be configured via environment variables (prefixed with `GATEWAY_`) or via the TOML config file. Environment variables take precedence over config file values.

| Variable | Description | Default | Required |
|----------|-------------|---------|----------|
| `GATEWAY_BIND` | Listen address (host:port) | `[::]:8134` | Yes |
| `GATEWAY_CALDAV_BASE` | Stalwart CalDAV URL | - | Yes |
| `GATEWAY_DATABASE_PATH` | SQLite database path | `/var/lib/exchange-gateway/gateway.db` | No |
| `GATEWAY_HMAC_SECRET` | HMAC signing secret (min 32 chars) | - | Yes |
| `GATEWAY_HOST` | Public hostname for Autodiscover | Auto-detected | Yes |
| `GATEWAY_MAIL_DOMAIN` | Mail domain for calendar items | - | Yes |
| `GATEWAY_MAX_ATTACHMENT_BYTES` | Max attachment size in bytes | `5242880` (5MB) | No |
| `GATEWAY_ROOM_BOOKING_ENABLED` | Enable room/resource booking | `true` | No |
| `GATEWAY_AUTH_CACHE_TTL_SECS` | Auth cache TTL in seconds | `300` | No |
| `GATEWAY_AUTH_CACHE_MAX_ENTRIES` | Max auth cache entries | `10000` | No |

### Boolean Values

For boolean environment variables (like `GATEWAY_ROOM_BOOKING_ENABLED`), the following values are accepted as `true`:

- `1`, `true`, `yes`, `on`, `enabled` (case-insensitive)

Any other value is interpreted as `false`.

### Example Docker Compose Configuration

```yaml
services:
  exchange-gateway:
    environment:
      # Required
      - GATEWAY_CALDAV_BASE=http://stalwart:8080/dav/
      - GATEWAY_HMAC_SECRET=your-32-character-minimum-secret-here
      - GATEWAY_HOST=calendar.stalwart.example.com
      - GATEWAY_MAIL_DOMAIN=example.com

      # Optional
      - GATEWAY_BIND=[::]:8134
      - GATEWAY_MAX_ATTACHMENT_BYTES=5242880
      - GATEWAY_ROOM_BOOKING_ENABLED=true
```

### Cloudflare Tunnel Variables

| Variable | Description | Required |
|----------|-------------|----------|
| `TUNNEL_ID` | Your Cloudflare tunnel ID | Yes |
| `TUNNEL_SECRET` | Tunnel authentication secret | Yes |
| `CLOUDFLARED_ORIGIN_CERT` | Origin certificate for mTLS | No |

---

## Support and Resources

- [Cloudflare Tunnel Documentation](https://developers.cloudflare.com/cloudflare-one/connections/connect-networks/)
- [Cloudflare Dashboard](https://dash.cloudflare.com/)
- [cloudflared GitHub Releases](https://github.com/cloudflare/cloudflared/releases)

---

## Alternative: Environment-Variable-Only Configuration (Docker Compose)

For setups where **all cloudflared variables are set via Docker Compose** (no config files):

### Configuration Approach

1. **No config.yml needed** - ingress rules are configured via Cloudflare Dashboard
2. **No credentials.json needed** - tunnel token is passed directly
3. **All variables via Docker Compose** - using `${VARIABLE}` syntax

### Cloudflare Dashboard Setup

1. Navigate to **Networks → Tunnels**
2. Click **Create a tunnel** → select **Cloudflared**
3. Name your tunnel (e.g., `exchange-gateway-tunnel`)
4. **Copy the tunnel token** (long string starting with `ey...`)
5. In tunnel settings, go to **Public Hostname** tab
6. Add route:
   - **Hostname**: `calendar.stalwart.example.com`
   - **Type**: HTTP
   - **Service**: `http://exchange-gateway:8134`
   - **Path** (optional): `^/EWS.*|^/ews.*|^/autodiscover.*|^/Microsoft-Server-ActiveSync.*|^/OAB.*|^/Autodiscover.*`

### Docker Compose Configuration

```yaml
# docker-compose.yml
services:
  exchange-gateway:
    build: .
    container_name: exchange-gateway
    image: exchange-gateway:latest
    restart: unless-stopped
    expose:
      - "8134"
    environment:
      - GATEWAY_CALDAV_BASE=${GATEWAY_CALDAV_BASE}
      - GATEWAY_HMAC_SECRET=${GATEWAY_HMAC_SECRET}
      - GATEWAY_HOST=${GATEWAY_HOST}
      - GATEWAY_MAIL_DOMAIN=${GATEWAY_MAIL_DOMAIN}
    volumes:
      - exchange-gateway-data:/var/lib/exchange-gateway
    healthcheck:
      test: ["CMD", "curl", "-f", "http://localhost:8134/health"]
      interval: 30s
      timeout: 5s
      retries: 3
    networks:
      - exchange-gateway-network

  cloudflared:
    image: cloudflare/cloudflared:${CLOUDFLARED_TAG:-latest}
    container_name: cloudflared-tunnel
    restart: unless-stopped
    depends_on:
      exchange-gateway:
        condition: service_healthy
    command: >
      tunnel run
      --token ${TUNNEL_TOKEN}
    environment:
      - TUNNEL_LOG_LEVEL=${CLOUDFLARED_LOG_LEVEL:-info}
    network_mode: service:exchange-gateway

volumes:
  exchange-gateway-data:

networks:
  exchange-gateway-network:
    name: exchange-gateway-network
    driver: bridge
```

### .env File

```bash
# .env - Keep this file secure and never commit to version control

# Exchange Gateway Configuration
GATEWAY_CALDAV_BASE=http://stalwart:8080/dav/
GATEWAY_HMAC_SECRET=your-32-character-minimum-secret-key-here
GATEWAY_HOST=calendar.stalwart.example.com
GATEWAY_MAIL_DOMAIN=example.com

# Optional Exchange Gateway Settings
GATEWAY_RUST_LOG=info
GATEWAY_TZ=UTC
GATEWAY_BIND=[::]:8134
GATEWAY_MAX_ATTACHMENT_BYTES=5242880
GATEWAY_ROOM_BOOKING_ENABLED=true

# Cloudflare Tunnel Configuration
# Get this from: Networks → Tunnels → Your Tunnel → Access Token
TUNNEL_TOKEN=eyJhIjoiMTIzNDU2NzgtYWJjZC0xMjM0LTEyMzQtYWJjZEV5MzQ1Njc4OSIsInQiOiIxMjM0NTY3ODkwMDExMjEzMTQyNTE0MTc4OT
CLOUDFLARED_TAG=latest
CLOUDFLARED_LOG_LEVEL=info
```

### Key Differences from Traditional Setup

| Aspect | Traditional (config.yml) | Environment-Variable-Only |
|--------|-------------------------|---------------------------|
| Config file | Required | Not needed |
| Credentials | credentials.json file | TUNNEL_TOKEN directly |
| Ingress rules | In config.yml | Cloudflare Dashboard |
| Maintenance | Edit local files | Dashboard-driven |
| Version control | config.yml may be committed | Safe to commit (no secrets) |

---

## Summary: Why No Worker?

Cloudflare Tunnel provides:

1. **TLS Termination**: Automatic at Cloudflare edge
2. **Encryption**: HTTP/2 with TLS between edge and origin
3. **Routing**: Ingress rules for path-based routing
4. **High Availability**: Built-in failover

Cloudflare Workers add complexity without benefit for this use case. The tunnel handles everything needed for calendar protocol proxying.