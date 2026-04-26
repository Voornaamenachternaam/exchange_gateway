<!-- CLOUDFLARE_DEPLOYMENT.md -->

# Cloudflare Deployment Guide

## Prerequisites

- Cloudflare account (free tier)
- Domain managed by Cloudflare DNS
- Stalwart Mailserver v0.15.5 running with RocksDB and ACME TLS
- Exchange Gateway Docker container running alongside Stalwart

## Step 1: Create D1 Database

```bash
wrangler d1 create exchange_gateway_db
```

Record the `database_id` from the output and set it in `wrangler.toml`.

Initialize the schema:

```bash
wrangler d1 execute exchange_gateway_db --file=d1_schema.sql
```

## Step 2: Create KV Namespace

```bash
wrangler kv:namespace create RATE_LIMIT_KV
```

Record the `id` from the output and set it in `wrangler.toml`.

## Step 3: Set Secrets

```bash
wrangler secret put GATEWAY_SECRET
```

Enter the same secret value configured in `exchange-gateway.config.toml` under `worker_secret`.

## Step 4: Configure DNS

Add the following DNS records in your Cloudflare dashboard:

| Type  | Name                   | Content                        | Proxy |
|-------|------------------------|--------------------------------|-------|
| A     | exchange               | <your-server-ipv4>            | On    |
| AAAA  | exchange               | <your-server-ipv6>            | On    |
| A     | exchange-origin        | <your-server-ipv4>            | Off   |
| AAAA  | exchange-origin        | <your-server-ipv6>            | Off   |
| CNAME | autodiscover           | exchange.example.com          | On    |
| CNAME | _autodiscover._tcp     | _acme.example.com (SRV target)| On    |

The `exchange` subdomain must be proxied (orange cloud) to route through Cloudflare Workers.

The `exchange-origin` subdomain is the direct origin that Cloudflare Tunnel connects to and must not be proxied.

## Step 5: Configure Cloudflare Tunnel

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

Alternatively, deploy as a Docker service alongside the gateway.

## Step 6: Configure Routes

The `wrangler.toml` file includes route patterns that direct Exchange protocol traffic to the Worker:

- `exchange.example.com/EWS/*`
- `exchange.example.com/OAB/*`
- `exchange.example.com/Microsoft-Server-ActiveSync`
- `exchange.example.com/autodiscover/*`
- `exchange.example.com/Autodiscover/*`
- `exchange.example.com/health`

Update the `zone_name` in each `[[routes]]` entry to match your Cloudflare zone.

## Step 7: Deploy the Worker

```bash
wrangler deploy
```

## Step 8: Verify Deployment

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

## Rate Limiting

The Worker enforces per-IP rate limiting using KV storage with the following defaults (configurable in `wrangler.toml`):

- `RATE_LIMIT_ENABLED`: `true`
- `RATE_LIMIT_MAX`: `120` requests per window
- `RATE_LIMIT_WINDOW_SEC`: `60` seconds

## Free-Plan Constraints

The Cloudflare free tier includes:

- 100,000 Worker requests per day
- 10 ms CPU time per invocation
- 5 million D1 reads per month
- 100,000 D1 writes per month
- 1 GB KV storage
- 1,000 KV reads per day (100,000 writes)

For a single-user or small-team calendar setup, these limits are sufficient. Monitor usage in the Cloudflare dashboard.

## Hardening Controls

1. **Authentication**: All EWS and ActiveSync requests require Basic authentication. The Worker validates credentials against the Stalwart IMAP backend via the Rust gateway.

2. **TLS**: Cloudflare provides automatic TLS termination. The tunnel between Cloudflare and the origin uses `https` with the ACME-provisioned certificate.

3. **Rate Limiting**: Per-IP rate limiting prevents abuse. Adjust limits in `wrangler.toml` as needed.

4. **Security Headers**: The Rust gateway sets the following response headers:
   - `X-Content-Type-Options: nosniff`
   - `X-Frame-Options: DENY`
   - `Content-Security-Policy: default-src 'none'`
   - `Strict-Transport-Security: max-age=63072000; includeSubDomains`
   - `Referrer-Policy: strict-origin-when-cross-origin`
   - `Cache-Control: private, no-store`

5. **Request Size Limits**: The Worker and gateway both enforce a 4 MB maximum body size for forwarded requests and 256 KB for API requests.

## Stalwart Configuration Additions

Add the following to your Stalwart `config.toml` to enable calendar integration:

```toml
[calendar]
enabled = true

[server.socket."0.0.0.0:8134"]
protocol = "http"

[server.socket."[::]:8134"]
protocol = "http"
```

See `stalwart-additions.toml` for the complete set of additions.