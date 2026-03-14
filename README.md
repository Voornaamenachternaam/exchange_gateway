# Exchange Gateway (EWS/EAS ↔ CalDAV) for Stalwart Mailserver
[![FOSSA Status](https://app.fossa.com/api/projects/git%2Bgithub.com%2FVoornaamenachternaam%2Fexchange_gateway.svg?type=shield)](https://app.fossa.com/projects/git%2Bgithub.com%2FVoornaamenachternaam%2Fexchange_gateway?ref=badge_shield)


This repository implements an Exchange-compatible gateway in Rust that translates Outlook EWS and ActiveSync calendar operations to CalDAV operations against a Stalwart Mailserver instance.

## Quick start

1. Edit `config.toml` and set `bind`, `caldav_base`, `worker_url`, `worker_secret`, and a strong `hmac_secret`.
2. Build:
   ```bash
   docker build -t exchange-gateway:latest .
3. Start with docker-compose:
   docker compose up -d
4. ActiveSync endpoint check:
   curl -i http://localhost:8134/Microsoft-Server-ActiveSync


## License
[![FOSSA Status](https://app.fossa.com/api/projects/git%2Bgithub.com%2FVoornaamenachternaam%2Fexchange_gateway.svg?type=large)](https://app.fossa.com/projects/git%2Bgithub.com%2FVoornaamenachternaam%2Fexchange_gateway?ref=badge_large)