# exchange_gateway

An Exchange protocol translation gateway that fronts a [Stalwart Mailserver v0.16.18]
container and exposes a native Microsoft Exchange surface (MAPI/HTTP, EWS and
Exchange ActiveSync) so that **New Outlook for Windows** and **Android's
Exchange account** connect to it directly and use Stalwart purely as the
backend, with no client-side extensions.

## Backend architecture

The gateway is a protocol-translation layer. Each Exchange data domain maps to
exactly one authoritative Stalwart backend:

| Domain                    | Backend          | Stalwart capability                              |
| ------------------------- | ---------------- | ------------------------------------------------ |
| Email (read/write)        | JMAP             | `urn:ietf:params:jmap:mail`                      |
| Calendar events           | JMAP Calendar    | `urn:ietf:params:jmap:calendars`                 |
| Free/busy (availability)  | JMAP Calendar    | `urn:ietf:params:jmap:principals:availability`   |
| **Contacts (address book)** | **CardDAV**     | `/carddav/{user}/` (vCard)                      |
| Directory (GAL / resolve) | JMAP (directory) | `urn:stalwart:jmap` `x:Account`/`x:MailingList` |
| Out-of-office             | JMAP (Sieve)     | `urn:ietf:params:jmap:sieve`                     |

### Email — JMAP

Email read, search and sync go through JMAP `Email/get`, `Email/query` and
`Email/changes`. Submission goes through JMAP `Email/submission` with a fallback
to SMTP (`src/email.rs`).

### Calendar and free/busy — JMAP Calendar (single authoritative source)

Calendar events are served from JMAP Calendar, and free/busy (EWS
`GetUserAvailability` and EAS availability) is served from the **same** JMAP
Calendar backend by default. This removes the historical dual-path ambiguity in
which calendar events came from JMAP while free/busy came from CalDAV, which
risked divergence between the two data sources.

Configuration flags:

- `prefer_jmap_calendar` (default `true`) — serve calendar events from JMAP
  Calendar; falls back to CalDAV only if JMAP Calendar is unavailable or fails.
- `prefer_caldav_freebusy` (default `false`) — when `true`, free/busy is served
  from CalDAV first (with JMAP Calendar as fallback). This is a legacy opt-out
  for deployments where CalDAV must remain the availability source of truth.
- `force_caldav_calendar` (default `false`) — force calendar events to CalDAV.

With the defaults, the free/busy *read* path and the calendar-event *read* path
share one authoritative JMAP Calendar source, so there is no divergence between
what Outlook displays in the calendar and what it reports as busy.

> Note on calendar *writes* and scheduling: when a calendar item carries
> attendees and the client requests invitation delivery
> (`SendMeetingInvitationsOrCancellations != SendToNone`), the gateway routes the
> write through CalDAV `PUT` because Stalwart's CalDAV scheduler (RFC 6638)
> auto-delivers `REQUEST`s to attendees, whereas a JMAP Calendar `iCalendar`
> blob write does not trigger scheduling. Both paths target Stalwart's single
> calendar store, so event data remains consistent; the CalDAV routing is a
> scheduling-transport concern, not a second source of truth for free/busy.

### Contacts — CardDAV (deliberate, documented dependency)

Contacts are read and written **exclusively** through the Stalwart CardDAV
endpoint via `CarddavClient` (`src/carddav.rs`), surfaced to EAS Sync via
`src/contacts.rs` (MS-ASCNTC).

This is intentional: Stalwart v0.16 exposes its address book over CardDAV, not
over a JMAP Contacts surface (`urn:ietf:params:jmap:contacts` is not advertised).
CardDAV fully covers the required contact semantics — `addressbook-query` REPORT
for listing, `PUT`/`POST` for create/update, `DELETE` for removal, and ETag-based
change detection/conflict handling — so no functional gap remains for the
supported clients.

Consequences of this dependency, documented so they are explicit:

- Contact data is **not** on the same JMAP surface as email/calendar. This is
  acceptable and, by design, does not reduce functionality.
- `carddav_base` must be configured (or auto-derived from `caldav_base` by
  substituting `/dav/` → `/carddav/`).
- If Stalwart ever advertises `urn:ietf:params:jmap:contacts`, migrating contacts
  to a JMAP Contacts backend would reduce the number of moving parts; this is
  tracked as a future enhancement, not a current requirement.

## TLS termination

The gateway binds plain HTTP (port `8134`). TLS termination is expected at a
reverse proxy (see `CLOUDFLARED_SETUP.md` for the cloudflared topology). This is
an operational prerequisite, not configured in-repo.