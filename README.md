# Exchange Gateway (EWS/EAS ↔ CalDAV) for Stalwart Mailserver

This repository implements an Exchange-compatible gateway in Rust that translates Outlook EWS and ActiveSync calendar operations to CalDAV operations against a Stalwart Mailserver instance.

## Components

- Rust gateway service (`src/*`) exposing:
  - `/EWS/Exchange.asmx`
  - `/Microsoft-Server-ActiveSync`
  - `/autodiscover/*`
  - `/OAB/*`
  - `/health`
- SQLite database for sync state and caching

## Configuration

All configuration is done via environment variables. See `CLOUDFLARED_SETUP.md` for the complete setup guide including Docker Compose configuration.

## Quick Start

1. Set environment variables (see `CLOUDFLARED_SETUP.md` for required variables)
2. Build and run:
   ```bash
   docker compose up -d --build
   ```
3. Basic endpoint checks:
   ```bash
   curl -i http://127.0.0.1:8134/Microsoft-Server-ActiveSync
   curl -i -X POST http://127.0.0.1:8134/EWS/Exchange.asmx
   curl -i http://127.0.0.1:8134/health
   ```
