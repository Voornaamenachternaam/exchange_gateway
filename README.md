# Exchange Gateway (EWS/EAS ↔ CalDAV) for Stalwart Mailserver

This repository implements an Exchange-compatible gateway in Rust that translates Outlook EWS and ActiveSync calendar operations to CalDAV operations against a Stalwart Mailserver instance.

## Components

- Rust gateway service (`src/*`) exposing:
  - `/EWS/Exchange.asmx`
  - `/Microsoft-Server-ActiveSync`
- Cloudflare Worker (`worker/index.js`) for:
  - Typed storage API backed by D1 (`/api/*`)
  - Autodiscover endpoints for Outlook
  - Optional edge forwarding + rate limiting for EWS/EAS
- D1 schema (`d1_schema.sql`) for sync/provisioning/item mappings.

## Quick start

1. Update `config.toml` values (`bind`, `caldav_base`, `worker_url`, `worker_secret`, `hmac_secret`).
2. Build and run:
   ```bash
   docker compose up -d --build
   ```
3. Basic endpoint checks:
   ```bash
   curl -i http://127.0.0.1:8134/Microsoft-Server-ActiveSync
   curl -i -X POST http://127.0.0.1:8134/EWS/Exchange.asmx
   ```

## Cloudflare deployment

See `CLOUDFLARE_DEPLOYMENT.md` for the full production deployment profile (Workers, KV, D1, routes, tunnel, rate-limits, and free-plan hardening controls).

## Rust version

- `rust-version = "1.94.0"`
- `edition = "2024"`

## License

MIT
