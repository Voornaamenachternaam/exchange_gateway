# DEPLOY_RUNBOOK.md — End-to-end runbook for the target use-case

This is the single page that maps the target use-case onto concrete steps:
mailbox domain → DNS → cloudflared → Android "Exchange" account → New Outlook
for Windows. It assumes the gateway image is already published to a registry
and Stalwart Mailserver v1.0.1 is running.

> Configuration lives **inline in `docker-compose.yml`**. There is no `.env`
> file. See `CLOUDFLARED_SETUP.md` for the tunnel details and `.env.example`
> for the meaning of every variable.

---

## 0. Prerequisites

| Item | Value |
|---|---|
| Gateway image | `<registry>/exchange-gateway:0.1.0` (multi-arch: amd64 + arm64) |
| Stalwart | `stalwartlabs/stalwart` pinned to the **v1.0.1** digest |
| Docker network | `my-bridge-network` (external) — shared by Stalwart and the gateway |
| Public EAS host | `calendar.example.com` → `GATEWAY_HOST` |
| Mail domain | `example.com` → `GATEWAY_MAIL_DOMAIN` |
| Tunnel | cloudflared on the Ubuntu host, TLS terminated at Cloudflare |

Pin Stalwart in **its** compose (`image: stalwartlabs/stalwart:v1.0.1@sha256:…`)
— `:latest` will silently drift past the version the gateway was validated
against.

---

## 1. DNS records (Cloudflare)

| Name | Type | Content |
|---|---|---|
| `calendar` | CNAME | `<tunnel-uuid>.cfargotunnel.com` (proxied) |
| `autodiscover` | CNAME | `<tunnel-uuid>.cfargotunnel.com` (proxied) |
| `example.com` (root) | CNAME | `<tunnel-uuid>.cfargotunnel.com` (proxied) — optional fallback |

Android's Exchange wizard probes
`https://autodiscover.<GATEWAY_MAIL_DOMAIN>/autodiscover/autodiscover.xml`
first (MS-OXDSCLI §2.2.3), so the `autodiscover` record is **required**, not
optional.

---

## 2. cloudflared ingress

Render `cloudflared/config.yml` with the tunnel UUID and both hostnames
(there is no `.env` — read `GATEWAY_HOST` and `GATEWAY_MAIL_DOMAIN` from
`docker-compose.yml`). All three hostnames forward to
`http://localhost:8134`. Verify the rendered file has **no** leftover
`example.com` placeholders before starting the tunnel.

---

## 3. Start the gateway

```bash
# From the directory containing docker-compose.yml
docker compose pull
docker compose up -d
docker compose ps           # expect: healthy
curl -fsS http://localhost:8134/health/ready && echo OK
```

If `health/ready` is not 200, the gateway cannot reach Stalwart's JMAP auth
backend — check `GATEWAY_JMAP_BASE`/`GATEWAY_CALDAV_BASE` and that the two
containers share `my-bridge-network`.

---

## 4. Add the account on Android ("Exchange")

1. Settings → Accounts → Add account → **Exchange**.
2. Email: `user@example.com`; choose **Manual setup / Exchange** (not Outlook).
3. Password: the Stalwart password.
4. Server: `calendar.example.com` (the `GATEWAY_HOST`). Accept the
   **device-admin consent** screen: the gateway advertises a policy, but the
   advertised policy is a **no-op** and **no remote wipe is possible**.
5. Choose what to sync: Mail, Calendar, Contacts, Tasks, Notes.

The gateway answers the initial Provision (PolicyKey `0`) with a fresh policy
key and an all-off policy document; Android then proceeds to FolderSync/Sync.

### PolicyKey enforcement (optional)

By default the gateway does **not** enforce the PolicyKey on non-Provision
commands: the advertised policy is a no-op, so refusing a client for a stale key
would only add friction. If a client ever refuses to proceed without an
enforced key, set `GATEWAY_EAS_REQUIRE_POLICY_KEY=1` in `docker-compose.yml`.
The gateway then answers any non-`Provision` command whose `X-MS-PolicyKey` does
not match the stored key with HTTP 200 + global Status 142, which prompts the
client to (re-)provision. `Provision` itself is always allowed so the client can
bootstrap.

---

## 5. Add the account in New Outlook for Windows

1. Add account → enter `user@example.com`.
2. New Outlook discovers the service via Autodiscover
   (`https://autodiscover.example.com/autodiscover/autodiscover.json/v1.0/…`
   and the XML/`.svc` endpoints). The gateway serves the `mobilesync`
   response pointing at `https://calendar.example.com/Microsoft-Server-ActiveSync`.
3. **Microsoft cloud sync path:** for a non-Microsoft (Stalwart) account, New
   Outlook and Outlook Android may sync through Microsoft's cloud sync service
   rather than directly from the device. The credentials you enter transit that
   service. If this is unacceptable, use an account/flow that syncs directly.
   This is a property of the clients, not the gateway.

---

## 6. Verify

```bash
# Local end-to-end smoke (Autodiscover + OPTIONS + a Ping), from the repo:
tests/outlook_cloudflare_smoke.sh https://calendar.example.com user@example.com
```

Then confirm in each client: send/receive mail, create/move an event, add a
contact, create a task and a note, and check that a change made in Stalwart
webmail shows up (push via Ping) within ~one heartbeat.

---

## 7. Troubleshooting

| Symptom | Likely cause | Check |
|---|---|---|
| Android wizard: "couldn't connect" | Autodiscover DNS/ingress missing or placeholder hostnames | `curl -fsS https://autodiscover.example.com/autodiscover/autodiscover.xml` |
| Mail arrives late / battery drain | Ping cut by Cloudflare (HTTP 524) | ensure `GATEWAY_MAX_PING_HEARTBEAT=80`; look for 524 in `docker compose logs` |
| Random 429s, sync stalls | rate limiter too small for one egress IP | raise `GATEWAY_RATE_LIMIT_REQUESTS_PER_MINUTE` |
| 503 with `Retry-After` | Stalwart backend unreachable | `docker compose ps`, `curl http://stalwart:8080/jmap/.well-known/jmap` from the gateway net |
| Calendar events lost on create | JMAP calendar write discarded by backend | check `GATEWAY_CALENDAR_CAPABILITY_CHECK` findings in the startup log |
| `health/ready` flapping | readiness mirrors the JMAP arbiter | pin Stalwart version; verify JMAP up |
| Outlook re-prompts for password | backend outage answered as 401 (must not happen) | confirm the gateway returns 503, not 401, during a Stalwart outage |

## 8. Rollback

```bash
docker compose down
# redeploy the previous pinned image tag
docker compose up -d
```
