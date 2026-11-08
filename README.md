# exchange_gateway

Microsoft Exchange protocol gateway that fronts a Stalwart Mailserver
v1.0.3 backend. Outlook clients (New Outlook for Windows 20251205004.10
and Outlook Android 5.2641.1) connect to the gateway over HTTPS via a
Cloudflare Tunnel; the gateway speaks EAS (WBXML), EWS, Autodiscover,
MAPI/HTTP and SMTP to the clients and translates to JMAP / CalDAV /
CardDAV against Stalwart.

Building the container image is out of scope for the operator task. The
published image is pulled from the container registry the operator uses;
the operator only runs the container.

## Deployment

The deployment instructions for this specific use-case live in
[`DEPLOY_RUNBOOK.md`](DEPLOY_RUNBOOK.md). Follow that file to create the
`/armblock/exchange-gateway-data` folder, fill in `docker-compose.yml`
with the required and optional environment variables inline (no `.env`
file is used), and deploy the container with Portainer.

Cloudflare Tunnel and Zero-Trust ingress configuration live in
[`CLOUDFLARED_SETUP.md`](CLOUDFLARED_SETUP.md). That file describes the
routes the operator must configure in the Cloudflare Zero Trust
dashboard and in the existing Stalwart container. No Cloudflare Tunnel
route for the root domain is needed; the root domain's A/AAAA records
already serve other content.
