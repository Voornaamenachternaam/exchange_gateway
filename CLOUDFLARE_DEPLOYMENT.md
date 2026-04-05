<!-- CLOUDFLARE_DEPLOYMENT.md -->
# Cloudflare deployment profile (free-tier compatible)

## Worker

- **Name:** `exchange-gateway-db`
- **Code file:** `worker/index.js`
- **Purpose:**
  - Outlook Autodiscover responses (XML, SOAP, JSON v2).
  - Exchange client traffic forwarding (`/EWS/*`, `/Microsoft-Server-ActiveSync`) to your gateway origin.
  - Edge rate limiting for forwarded EWS/EAS requests.
  - Typed secure API (`/api/*`) used by the Rust gateway storage layer.

## D1

- **Database binding name:** `EXCHANGE_DB`
- **Database name:** `exchange_gateway_db`
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

- Keep `cloudflared` on Ubuntu host (same instance already serving Stalwart webui).
- **Add a new ingress rule** for the gateway alongside the existing Stalwart rule
  (see **Adding to existing cloudflared tunnel** below).
- Publish an origin hostname (e.g. `exchange-origin.example.com`) through the
  Cloudflare Tunnel to `http://localhost:8134` (the host-bound gateway port).
- Use this origin hostname as `ORIGIN_BASE_URL` in the worker.
- Public clients use `exchange.example.com` (worker route hostname).
- Rust gateway uses `worker_url = "https://exchange.example.com/api"`.
- Example files for this exact layout are provided in:
  - `examples/cloudflared-exchange-origin.yml`
  - `examples/exchange-gateway.config.toml`
  - `examples/wrangler.toml`

## Rust gateway config alignment

`config.toml` should use:

- `worker_url = "https://exchange.example.com/api"`
- `worker_secret = "<same-as-GATEWAY_SECRET>"`
- `bind = "0.0.0.0:8134"`

---

## Adding to existing cloudflared tunnel

Your existing cloudflared is already configured to route:
```
https://stalwart.example.com → http://localhost:8080  (Stalwart webui)
```

**Do not replace** the existing config. Instead, **add** a new ingress rule for the
gateway. Edit your cloudflared config (typically
`/etc/cloudflare-one/config.yml` or the file referenced by your service unit):

```yaml
tunnel: <your-tunnel-id>
credentials-file: /home/<user>/.cloudflared/<tunnel-id>.json

ingress:
  # Existing Stalwart webui rule — leave this unchanged
  - hostname: stalwart.example.com
    service: http://localhost:8080

  # NEW: Exchange gateway origin rule
  # Outlook EWS/EAS traffic arrives at exchange.example.com (the Worker hostname).
  # The Worker forwards /EWS/* and /Microsoft-Server-ActiveSync to this hostname via tunnel.
  - hostname: exchange-origin.example.com
    service: http://localhost:8134

  # Catch-all (required by cloudflared)
  - service: http_status:404
```

Then restart cloudflared:
```bash
sudo systemctl restart cloudflared
```

And in the Cloudflare dashboard, ensure both `stalwart.example.com` AND
`exchange-origin.example.com` are DNS-proxied CNAME records pointing to
`<your-tunnel-id>.cfargotunnel.com`.

---

## Free-plan hardening profile (March 2026)

Set these additional Worker text variables:

1. `SQL_API_ENABLED`
   - Type: text
   - Value: `false` (recommended in production)

2. `MAX_FORWARD_BODY_BYTES`
   - Type: text
   - Value: `4194304`

3. `MAX_API_BODY_BYTES`
   - Type: text
   - Value: `262144`

Notes:
- The generic `/api/*` SQL endpoint is disabled unless `SQL_API_ENABLED=true`.
- When enabled, it permits only `SELECT` queries.
- Typed APIs used by the Rust gateway remain available and authenticated via `GATEWAY_SECRET`.

---

## Stalwart v0.15.5 compatibility notes

- Keep Stalwart calendar backend on CalDAV as configured in the Rust gateway (`caldav_base`).
- Keep Stalwart basic username/password auth enabled for Outlook profile compatibility.
- Keep TLS termination at Cloudflare edge; origin behind tunnel can remain HTTP on the
  private host network (port 8134, localhost only — Docker maps `8134:8134`).
- For cloudflared ingress, use a dedicated origin hostname for gateway forwarding:

```yaml
ingress:
  - hostname: exchange-origin.example.com
    service: http://localhost:8134
  - service: http_status:404
```

- Ensure Worker `ORIGIN_BASE_URL` points to that origin hostname, and `GATEWAY_HOST`
  points to the public Exchange hostname.

---

## Allowed forwarded methods

Forwarded Exchange routes accept only: `OPTIONS`, `POST`.

---

## Deployment checklist

```
[ ] 1. wrangler d1 create exchange_gateway_db
[ ] 2. wrangler d1 execute exchange_gateway_db --file=d1_schema.sql
[ ] 3. wrangler kv namespace create RATE_LIMIT_KV
[ ] 4. Update wrangler.toml with real D1 database_id and KV id
[ ] 5. wrangler secret put GATEWAY_SECRET
[ ] 6. Set GATEWAY_HOST and ORIGIN_BASE_URL variables in Cloudflare dashboard
[ ] 7. wrangler deploy (or deploy via Cloudflare dashboard)
[ ] 8. Add exchange-origin.example.com ingress rule to cloudflared config
[ ] 9. Restart cloudflared: sudo systemctl restart cloudflared
[ ] 10. Add exchange-origin.example.com CNAME in Cloudflare DNS (proxied)
[ ] 11. Create Worker routes for exchange.example.com paths listed above
[ ] 12. Update config.toml on Ubuntu host with correct values
[ ] 13. docker compose up -d --build
[ ] 14. Run smoke test: bash tests/outlook_cloudflare_smoke.sh
```
