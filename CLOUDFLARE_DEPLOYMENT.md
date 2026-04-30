# Cloudflare Deployment Guide

## Overview

The Exchange Gateway uses a simplified architecture with local SQLite database and Cloudflare Worker for TLS termination. All Exchange protocol traffic is proxied through the Cloudflare Worker to the Exchange Gateway container running locally.

## Prerequisites

- Cloudflare account (free tier)
- Domain managed by Cloudflare DNS
- Stalwart Mailserver v0.15.5 running with RocksDB and ACME TLS
- Exchange Gateway Docker container running alongside Stalwart
- cloudflared installed on the Docker host

## Architecture

```
Outlook/Exchange Client
        |
        v (HTTPS)
Cloudflare Worker (TLS Termination + Proxy)
        |
        v (HTTP via Cloudflare Tunnel)
cloudflared Tunnel -> exchange-gateway container
        |                          |
        +-----------v--------------+
                      |
                      v
              SQLite Database
                      |
                      v
              Stalwart CalDAV
```

## Step 1: Configure DNS

Add the following DNS records in your Cloudflare dashboard:

| Type | Name | Content | Proxy |
|-------|------------------------|--------------------------------|-------|
| A | exchange | <your-server-ipv4> | On |
| AAAA | exchange | <your-server-ipv6> | On |
| A | exchange-origin | <your-server-ipv4> | Off |
| AAAA | exchange-origin | <your-server-ipv6> | Off |
| CNAME | autodiscover | exchange.example.com | On |
| CNAME | _autodiscover._tcp | _acme.example.com (SRV target)| On |

The `exchange` subdomain must be proxied (orange cloud) to route through Cloudflare Workers.

The `exchange-origin` subdomain is the direct origin that Cloudflare Tunnel connects to and must not be proxied.

The `_autodiscover._tcp` SRV record enables Outlook auto-discovery for the domain.

## Step 2: Configure Cloudflare Tunnel

Create a tunnel in the Cloudflare Zero Trust dashboard:

```bash
cloudflared tunnel create exchange-gateway
```

Configure `cloudflared-exchange-origin.yml` with the tunnel credentials and origin URL:

```yaml
tunnel: <TUNNEL_ID>
credentials-file: /etc/cloudflared/<TUNNEL_ID>.json

ingress:
- hostname: exchange-origin.example.com
  service: http://localhost:8134
- service: http_status:404
```

Run the tunnel connector on the same host as the Exchange Gateway container:

```bash
cloudflared tunnel run --config cloudflared-exchange-origin.yml exchange-gateway
```

## Step 3: Configure Worker Routes

Deploy the Worker and create custom routes in `wrangler.toml`:

```toml
name = "exchange-gateway"
main = "worker/index.js"
compatibility_date = "2024-01-01"

routes = [
  { pattern = "exchange.example.com/ews/*", zone_name = "example.com" },
  { pattern = "exchange.example.com/EWS/*", zone_name = "example.com" },
  { pattern = "exchange.example.com/Microsoft-Server-ActiveSync", zone_name = "example.com" },
  { pattern = "exchange.example.com/autodiscover/*", zone_name = "example.com" },
  { pattern = "exchange.example.com/Autodiscover/*", zone_name = "example.com" },
  { pattern = "exchange.example.com/oab/*", zone_name = "example.com" },
  { pattern = "exchange.example.com/OAB/*", zone_name = "example.com" },
  { pattern = "exchange.example.com/health", zone_name = "example.com" }
]
```

Deploy the Worker:

```bash
wrangler deploy
```

## Step 4: Verify Deployment

1. Test Autodiscover:

```bash
curl -X POST https://exchange.example.com/Autodiscover/Autodiscover.xml \
  -H "Content-Type: text/xml" \
  -d '<Autodiscover xmlns="http://schemas.microsoft.com/exchange/autodiscover/outlook/requestschema/2006"><Request><EMailAddress>user@example.com</EMailAddress><AcceptableResponseSchema>http://schemas.microsoft.com/exchange/autodiscover/outlook/responseschema/2006a</AcceptableResponseSchema></Request></Autodiscover>'
```

2. Test EWS connectivity:

```bash
curl -u user@example.com:password \
  https://exchange.example.com/EWS/Exchange.asmx
```

3. Test ActiveSync:

```bash
curl -u user@example.com:password \
  https://exchange.example.com/Microsoft-Server-ActiveSync
```

4. Test OAB:

```bash
curl https://exchange.example.com/OAB/oab.xml
```

5. Test health endpoint:

```bash
curl https://exchange.example.com/health
```

## Free-Plan Constraints

The Cloudflare free tier includes:

- 100,000 Worker requests per day
- 10 ms CPU time per invocation

For a single-user or small-team calendar setup, these limits are sufficient. Monitor usage in the Cloudflare dashboard.

## Security Model

1. **TLS**: Cloudflare provides automatic TLS termination for all client-facing endpoints. The Cloudflare Tunnel between Cloudflare and the origin container uses HTTP over the private network. The gateway container runs plain HTTP with no TLS certificates needed.

2. **Authentication**: All EWS and ActiveSync requests require Basic authentication. The Worker validates credentials against the Stalwart IMAP backend via the Rust gateway.

3. **Security Headers**: The Rust gateway sets the following response headers:
   - `X-Content-Type-Options: nosniff`
   - `X-Frame-Options: DENY`
   - `Content-Security-Policy: default-src 'none'`
   - `Strict-Transport-Security: max-age=63072000; includeSubDomains`
   - `Referrer-Policy: strict-origin-when-cross-origin`
   - `Cache-Control: private, no-store`

4. **Request Size Limits**: The gateway enforces a 4 MB maximum body size for forwarded requests.

## Stalwart Configuration Additions

Add the following to your Stalwart `config.toml` to enable JMAP and CalDAV access for the Exchange Gateway:

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

The Exchange Gateway connects to Stalwart's CalDAV endpoint at `http://stalwart:8080/dav/` (configured via `caldav_base` in `exchange-gateway.config.toml`). Stalwart v0.15.5 serves CalDAV natively on its HTTP listener when `[dav]` is enabled. Replace `example.com` with your actual mail domain.

See `stalwart-additions.toml` for the complete set of additions.

## Database

The Exchange Gateway uses SQLite as its local embedded database. The database file is stored at `/var/lib/exchange-gateway/gateway.db` (configurable via `database_path` in config.toml).

The schema is automatically initialized on first startup. The `d1_schema.sql` file contains the complete schema definition.

## Troubleshooting

### Connection Issues

1. Verify the Cloudflare Tunnel is running:
   ```bash
   cloudflared tunnel list
   ```

2. Check the tunnel logs:
   ```bash
   cloudflared tunnel run --config cloudflared-exchange-origin.yml exchange-gateway --loglevel debug
   ```

3. Verify the Docker container is running:
   ```bash
   docker logs exchange_gateway
   ```

### Authentication Issues

1. Verify credentials work with Stalwart directly:
   ```bash
   curl -u user@example.com:password http://stalwart:8080/dav/cal/user@example.com/
   ```

2. Check the Exchange Gateway logs for authentication errors.

### Database Issues

1. Verify the database file exists and is writable:
   ```bash
   docker exec exchange_gateway ls -la /var/lib/exchange-gateway/
   ```

2. Check the SQLite database integrity:
   ```bash
   docker exec exchange_gateway sqlite3 /var/lib/exchange-gateway/gateway.db "PRAGMA integrity_check;"
   ```