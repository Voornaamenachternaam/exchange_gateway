# Exchange Gateway

A production-ready Exchange ActiveSync (EAS) and EWS gateway for Stalwart Mail Server, enabling full calendar synchronization with Outlook clients on Windows 11 and Android 15.

## Overview

This gateway provides Exchange protocol compatibility for Stalwart Mail Server v0.15.5, allowing Outlook clients to sync calendars without client-side extensions. It runs as a Docker container alongside Stalwart and integrates with Cloudflare services for TLS termination and edge caching.

## Architecture

```
┌─────────────────┐     ┌─────────────────┐     ┌─────────────────┐
│ Outlook Client  │────▶│ Cloudflare      │────▶│ Exchange        │
│ (Windows/Android│◄────│ (Worker/Tunnel) │◄────│ Gateway         │
│                 │     │                 │     │ (Rust/Docker)   │
└─────────────────┘     └─────────────────┘     └────────┬────────┘
                                                         │
                                                         ▼
                                                ┌─────────────────┐
                                                │ Stalwart        │
                                                │ Mail Server     │
                                                │ (CalDAV)        │
                                                └─────────────────┘
```

## Components

### Rust Gateway (Docker Container)
- **Protocol Support**: Exchange ActiveSync (EAS), EWS, Autodiscover
- **Features**: Calendar sync, meeting responses, device provisioning
- **Security**: Basic auth, HMAC-based server IDs, non-root runtime

### Cloudflare Worker
- **Edge Location**: Global request routing
- **Database**: D1 SQLite for sync state persistence
- **Rate Limiting**: KV-based request throttling
- **TLS Termination**: Automatic HTTPS via Cloudflare

## Quick Start

### Prerequisites
- Ubuntu Server 24 LTS
- Docker with Docker Compose
- Cloudflare account with:
  - D1 database
  - KV namespace
  - Worker deployment

### 1. Configure Cloudflare Worker

```bash
# Create D1 database
wrangler d1 create exchange-gateway

# Apply schema
wrangler d1 execute exchange-gateway --file=d1_schema.sql

# Create KV namespace
wrangler kv namespace create RATE_LIMIT_KV

# Set secret
wrangler secret put GATEWAY_SECRET

# Deploy worker
wrangler deploy
```

### 2. Configure Rust Gateway

Create `config.toml`:

```toml
bind = "0.0.0.0:8134"
caldav_base = "http://stalwart:8080/dav/"
worker_url = "https://exchange.example.com/api"
worker_secret = "your-gateway-secret"
hmac_secret = "your-long-random-hex-string"
```

### 3. Deploy with Docker Compose

```bash
# Create network
docker network create my-bridge-network --subnet=172.28.0.0/16

# Start gateway
docker-compose up -d
```

### 4. Configure Cloudflare Tunnel

Add to your cloudflared config:

```yaml
- hostname: exchange.example.com
  service: http://localhost:8134
```

## Protocol Support

### Exchange ActiveSync (EAS)
- FolderSync, Sync, GetItemEstimate
- MeetingResponse for calendar invitations
- Provision for device management
- Settings for device information
- Ping for folder monitoring

### EWS (Exchange Web Services)
- GetFolder, FindItem, GetItem
- CreateItem, UpdateItem, DeleteItem
- SyncFolderItems
- GetServerTimeZones
- ResolveNames

### Autodiscover
- XML, JSON, and SOAP formats
- Automatic endpoint configuration

## Security

- Basic authentication (username/password)
- Bearer token authentication for Worker API
- Rate limiting per IP address
- Hop-by-hop header sanitization
- Security headers (X-Frame-Options, etc.)
- Non-root container execution

## Environment Variables

| Variable | Description | Default |
|----------|-------------|---------|
| `GATEWAY_BIND` | Bind address | `0.0.0.0:8134` |
| `CALDAV_BASE` | Stalwart CalDAV URL | Required |
| `WORKER_URL` | Cloudflare Worker URL | Required |
| `WORKER_SECRET` | Shared secret | Required |
| `HMAC_SECRET` | HMAC signing key | Required |
| `RUST_LOG` | Log level | `info` |

## License

MIT License
