<!-- tests/OUTLOOK_CLOUDFLARE_SMOKE.md -->
# Outlook + Cloudflare + Stalwart smoke harness

This repository now includes a live-environment smoke script for the specific deployment shape targeted by `exchange_gateway`:

- existing **Stalwart Mailserver v0.15.5** with Basic auth,
- **free Cloudflare services** in front,
- Ubuntu Server 24 LTS host,
- `cloudflared` already present on the host,
- native Outlook calendar access through EWS / EAS / Autodiscover.

## Purpose

The script is not a substitute for real Outlook desktop/mobile testing, but it gives the repository a repeatable probe that validates the same externally published gateway surface Outlook depends on:

- ActiveSync `OPTIONS`,
- ActiveSync `FolderSync` bootstrap and invalid `SyncKey` rejection,
- Autodiscover XML and SOAP,
- Autodiscover JSON,
- EWS `GetFolder`,
- EWS `GetUserAvailability`,
- optional EWS `CreateItem` / `UpdateItem` / `DeleteItem` write-through probe.

## Usage

```bash
export GATEWAY_BASE_URL="https://mail.example.com"
export GATEWAY_USER="user@example.com"
export GATEWAY_PASS="password"
bash tests/outlook_cloudflare_smoke.sh
```

To include the live mutation probe:

```bash
export RUN_MUTATION_PROBE=1
bash tests/outlook_cloudflare_smoke.sh
```

## What the mutation probe verifies

When `RUN_MUTATION_PROBE=1`, the script:

1. creates a calendar item through EWS `CreateItem`,
2. updates that same item through EWS `UpdateItem`,
3. deletes it through EWS `DeleteItem`.

That gives a reproducible live smoke check for the Stalwart + Cloudflare + gateway path without requiring Outlook automation or any non-`cloudflared` server connector.
