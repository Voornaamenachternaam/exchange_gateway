# DEPLOY_RUNBOOK.md — End-to-end runbook for the target use-case

This is the single page that maps the target use-case onto concrete steps:
mailbox domain → DNS → cloudflared → Android "Exchange" account → New Outlook
for Windows. It assumes the gateway image is already published to a registry
and Stalwart Mailserver v1.0.3 is running.

> Configuration lives **inline in `docker-compose.yml`**. There is no `.env`
> file. See `CLOUDFLARED_SETUP.md` for the tunnel details; the meaning of every
> variable is documented inline in `docker-compose.yml`.

> **Placeholder convention — read before deploying.** Every `example.com`
> hostname in this repository is a **placeholder** and must be replaced in
> your own copy before deployment (the repository templates keep the
> placeholders on purpose and must never be deployed as-is):
>
> | Placeholder | Stands for |
> |---|---|
> | `example.com` | your real root / mail domain — the domain part of your users' email addresses (`GATEWAY_MAIL_DOMAIN`) |
> | `mail.example.com` | your real mailserver hostname / MX record (`GATEWAY_MAIL_HOST`) |
> | `calendar.example.com` | `calendar.` + your real domain — the public EAS/EWS/MAPI host (`GATEWAY_HOST`) |
> | `autodiscover.example.com` | `autodiscover.` + your real domain — the host Android's "Exchange" wizard probes first |
> | `stalwart.example.com` | `stalwart.` + your real domain — the Stalwart web UI host |
>
> After replacement these invariants must hold:
> `GATEWAY_HOST == calendar.<GATEWAY_MAIL_DOMAIN>`,
> Autodiscover host `== autodiscover.<GATEWAY_MAIL_DOMAIN>`, and
> `GATEWAY_MAIL_HOST == <your real MX hostname>` (matching the existing MX
> record). No tunnel route is ever created for the bare root domain — it
> already has A/AAAA records. Only the `calendar.` and `autodiscover.`
> routes to port 8134 are needed. A `.env` file is forbidden in this
> deployment; every value lives inline in `docker-compose.yml`.

---

## 0. Prerequisites

| Item | Value |
|---|---|
| Gateway image | `<registry>/exchange-gateway:0.1.0` (multi-arch: amd64 + arm64) |
| Stalwart | `stalwartlabs/stalwart` pinned to the **v1.0.3** digest |
| Docker network | `my-bridge-network` (external) — shared by Stalwart and the gateway |
| Public EAS host | `calendar.example.com` *(placeholder for `calendar.<your real domain>`)* → `GATEWAY_HOST` |
| Mail domain | `example.com` *(placeholder for your real root / mail domain)* → `GATEWAY_MAIL_DOMAIN` |
| Mailserver hostname (MX) | `mail.example.com` *(placeholder for your real MX host)* → `GATEWAY_MAIL_HOST` |
| Tunnel | cloudflared on the Ubuntu host, TLS terminated at Cloudflare |

Pin Stalwart in **its** compose (`image: stalwartlabs/stalwart:v1.0.3@sha256:…`)
— `:latest` will silently drift past the version the gateway was validated
against.

---

## 1. DNS records (Cloudflare)

| Name | Type | Content |
|---|---|---|
| `calendar` | CNAME | `<tunnel-uuid>.cfargotunnel.com` (proxied) |
| `autodiscover` | CNAME | `<tunnel-uuid>.cfargotunnel.com` (proxied) |

> **Do not create a record or tunnel route for the bare root domain**
> (`example.com` = your real root domain). It already has A/AAAA records
> and cannot be tunnel-routed; the root-domain Autodiscover fallback is
> not needed because Android's primary probe
> (`autodiscover.example.com`, placeholder for `autodiscover.` + your
> real domain) is served by the `autodiscover` record above.

Android's Exchange wizard probes
`https://autodiscover.<GATEWAY_MAIL_DOMAIN>/autodiscover/autodiscover.xml`
first (MS-OXDSCLI §2.2.3), so the `autodiscover` record is **required**, not
optional.

---

## 2. cloudflared ingress

Render `cloudflared/config.yml` with the tunnel UUID and both hostnames
(there is no `.env` — read `GATEWAY_HOST` and `GATEWAY_MAIL_DOMAIN` from
`docker-compose.yml`). Both hostnames forward to
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

> **First start only:** set `GATEWAY_HMAC_SECRET` in `docker-compose.yml` to
> `openssl rand -hex 32` output (≥32 chars, must not start with `REPLACE_`) —
> the placeholder shipped in the template fails startup on purpose.

If `health/ready` is not 200, the gateway cannot reach Stalwart's JMAP auth
backend — check `GATEWAY_JMAP_BASE`/`GATEWAY_CALDAV_BASE` and that the two
containers share `my-bridge-network`.

---

## 4. Add the account on Android ("Exchange")

1. Settings → Accounts → Add account → **Exchange**.
2. Email: `user@example.com` *(placeholder for your real address)*; choose **Manual setup / Exchange** (not Outlook).
3. Password: the Stalwart password.
4. Server: `calendar.example.com` *(placeholder for `calendar.<your real domain>`)* (the `GATEWAY_HOST`). Accept the
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

1. Add account → enter `user@example.com` *(placeholder for your real address)*.
2. New Outlook discovers the service via Autodiscover
   (`https://autodiscover.example.com/autodiscover/autodiscover.json/v1.0/…`
   *(placeholder for `autodiscover.<your real domain>`)* and the XML/`.svc`
   endpoints). The gateway serves the `mobilesync` response pointing at
   `https://calendar.example.com/Microsoft-Server-ActiveSync` *(placeholder
   for `calendar.<your real domain>`)*.
3. **Microsoft cloud sync path:** for a non-Microsoft (Stalwart) account, New
   Outlook and Outlook Android may sync through Microsoft's cloud sync service
   rather than directly from the device. The credentials you enter transit that
   service. If this is unacceptable, use an account/flow that syncs directly.
   This is a property of the clients, not the gateway.

---

## 6. Verify

```bash
# Local end-to-end smoke (Autodiscover + OPTIONS + a Ping), from the repo:
tests/outlook_cloudflare_smoke.sh https://calendar.example.com user@example.com  # example.com = placeholder for your real domain
```

Then confirm in each client: send/receive mail, create/move an event, add a
contact, create a task and a note, and check that a change made in Stalwart
webmail shows up (push via Ping) within ~one heartbeat.

---

## 7. Troubleshooting

| Symptom | Likely cause | Check |
|---|---|---|
| Android wizard: "couldn't connect" | Autodiscover DNS/ingress missing or placeholder hostnames | `curl -fsS https://autodiscover.example.com/autodiscover/autodiscover.xml` *(placeholder for `autodiscover.<your real domain>`)* |
| Mail arrives late / battery drain | Ping cut by Cloudflare (HTTP 524) | ensure `GATEWAY_MAX_PING_HEARTBEAT=80`; look for 524 in `docker compose logs` |
| Random 429s, sync stalls | rate limiter too small for one egress IP | raise `GATEWAY_RATE_LIMIT_REQUESTS_PER_MINUTE` |
| 503 with `Retry-After` | Stalwart backend unreachable | `docker compose ps`, `curl http://stalwart:8080/jmap/.well-known/jmap` from the gateway net |
| Calendar events lost on create | JMAP calendar write discarded by backend | the write path self-verifies and auto-disables JMAP writes on a discard; look for "stored an empty event; disabling JMAP calendar writes" in the log, then confirm `GATEWAY_CALENDAR_CAPABILITY_CHECK` findings in the startup log |
| `health/ready` flapping | readiness mirrors the JMAP arbiter | pin Stalwart version; verify JMAP up |
| Gateway unhealthy after a Stalwart restart | expected: readiness requires the backend | wait for Stalwart to return; do not restart the gateway manually (stock Compose never restarts an unhealthy container) |
| Outlook re-prompts for password | backend outage answered as 401 (must not happen) | confirm the gateway returns 503, not 401, during a Stalwart outage |

## 8. Rollback

```bash
docker compose down
# redeploy the previous pinned image tag
docker compose up -d
```
