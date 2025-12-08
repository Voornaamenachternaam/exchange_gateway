# Exchange Gateway (EWS/EAS ↔ CalDAV) for Stalwart Mailserver

This repository implements an Exchange-compatible gateway in Rust 1.91.1 (edition 2024) that translates Outlook EWS and ActiveSync calendar operations to CalDAV operations against a Stalwart Mailserver instance.

## Quick start

1. Edit `config.toml` and set `caldav_base`, `db_path`, and a strong `hmac_secret`.
2. Build:
   ```bash
   docker build -t exchange-gateway:latest .
