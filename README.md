<!-- README.md -->
# Exchange Gateway (EWS/EAS ↔ CalDAV) for Stalwart Mailserver

This repository implements an Exchange-compatible gateway in Rust that translates Outlook EWS and ActiveSync calendar operations to CalDAV operations against a Stalwart Mailserver instance.

## Components

- Rust gateway service (`src/*`) exposing:
  - `/EWS/Exchange.asmx`
  - `/Microsoft-Server-ActiveSync`
  - `/autodiscover/*`
  - `/OAB/*`
  - `/health`
- Cloudflare Tunnel (`cloudflared`) for TLS termination
- SQLite database for sync state and caching

## Quick Start

1. Update `config.toml` values (`bind`, `caldav_base`, `hmac_secret`).
2. Set up Cloudflare Tunnel (see `CLOUDFLARED_SETUP.md`).
3. Build and run:
   ```bash
   docker compose -f examples/docker-compose.yml up -d --build
   ```
4. Basic endpoint checks:
   ```bash
   curl -i http://127.0.0.1:8134/Microsoft-Server-ActiveSync
   curl -i -X POST http://127.0.0.1:8134/EWS/Exchange.asmx
   curl -i http://127.0.0.1:8134/health
   ```

## Cloudflare Deployment

See `CLOUDFLARED_SETUP.md` for the full production deployment profile using Cloudflare Tunnel for TLS termination. No Cloudflare Workers are required.
