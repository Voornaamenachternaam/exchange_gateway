# Cloudflare deployment profile (free-tier compatible)

## Worker

- **Name:** `exchange-gateway-edge`
- **Code file:** `worker/index.js`
- **Purpose:**
  - Outlook autodiscover responses.
  - Exchange client traffic forwarding (`/EWS/*`, `/Microsoft-Server-ActiveSync`) to your gateway origin.
  - Edge rate limiting for forwarded EWS/EAS requests.
  - Typed secure API (`/api/*`) used by the Rust gateway storage layer.

## D1

- **Database binding name:** `EXCHANGE_DB`
- **Schema file:** `d1_schema.sql`

## KV (for edge rate limiting)

- **Namespace to create:** `EXCHANGE_RATE_LIMIT_KV`
- **Worker binding name:** `RATE_LIMIT_KV`

## Worker Variables and Secrets

### Secrets

1. `GATEWAY_SECRET`
   - Type: secret
   - Value: long random secret (same value as `worker_secret` in `config.toml`)

### Text variables

1. `GATEWAY_HOST`
   - Type: text
   - Value: your public Exchange host, example: `exchange.example.com`

2. `ORIGIN_BASE_URL`
   - Type: text
   - Value: origin endpoint to forward EWS/EAS traffic to, example: `https://exchange-origin.example.com`

3. `RATE_LIMIT_ENABLED`
   - Type: text
   - Value: `true`

4. `RATE_LIMIT_MAX`
   - Type: text
   - Value: `120`

5. `RATE_LIMIT_WINDOW_SEC`
   - Type: text
   - Value: `60`

## Domains & Routes

Attach the worker to all of the following routes on the same hostname:

1. `exchange.example.com/EWS/*`
2. `exchange.example.com/Microsoft-Server-ActiveSync*`
3. `exchange.example.com/Autodiscover/*`
4. `exchange.example.com/autodiscover/*`
5. `exchange.example.com/api/*`

## Tunnel and origin model

- Keep `cloudflared` on Ubuntu host.
- Publish an origin hostname (for example `exchange-origin.example.com`) through Cloudflare Tunnel to `http://exchange_gateway:8134`.
- Use this origin hostname as `ORIGIN_BASE_URL` in the worker.
- Public clients use `exchange.example.com` (worker route hostname).
- Rust gateway uses `worker_url = "https://exchange.example.com/api"`.

## Rust gateway config alignment

`config.toml` should use:

- `worker_url = "https://exchange.example.com/api"`
- `worker_secret = "<same-as-GATEWAY_SECRET>"`
- `bind = "0.0.0.0:8134"`



## Free-plan hardening profile (March 2026)

Set these additional Worker text variables:

1. `SQL_API_ENABLED`
   - Type: text
   - Value: `false` (recommended in production)

2. `MAX_FORWARD_BODY_BYTES`
   - Type: text
   - Value: `1048576`

3. `MAX_API_BODY_BYTES`
   - Type: text
   - Value: `262144`

Notes:
- The generic `/api/*` SQL endpoint is now disabled unless `SQL_API_ENABLED=true`.
- When enabled, it permits only `SELECT` queries.
- Typed APIs used by the Rust gateway remain available and authenticated via `GATEWAY_SECRET`.


## Stalwart v0.15.5 compatibility notes

- Keep Stalwart calendar backend on CalDAV as configured in the Rust gateway (`caldav_base`).
- Keep Stalwart basic username/password auth enabled for Outlook profile compatibility.
- Keep TLS termination at Cloudflare edge; origin behind tunnel can remain HTTP on private Docker network.
- For cloudflared ingress, use a dedicated origin hostname for gateway forwarding, for example:

```yaml
ingress:
  - hostname: exchange-origin.example.com
    service: http://exchange_gateway:8134
  - service: http_status:404
```

- Ensure Worker `ORIGIN_BASE_URL` points to that origin hostname, and `GATEWAY_HOST` points to the public Exchange hostname.


## Allowed forwarded methods

Forwarded Exchange routes accept only: `OPTIONS`, `POST`, `GET`, `HEAD`.
