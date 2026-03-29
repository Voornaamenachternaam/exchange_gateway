# GAP ANALYSIS — Exchange Gateway

Analysis date: 2026-03-29  
Protocol reference: Binder1.txt (MS-ASAIRS, MS-ASCAL, MS-ASCMD, MS-ASPROV,
MS-ASWBXML, MS-OXWSCAL, MS-OXWSCORE, MS-OXWSFOLD, MS-OXWSSYNC, and related specs)

---

## CRITICAL — Code Will Not Compile

### GAP-01 `src/timezone.rs` is missing
`main.rs` declares `mod timezone;` but the file was never created.
**STATUS: CLOSED** — file created with EAS TimeZone blob decoder.

### GAP-02 `AutodiscoverJsonParams` struct and `handle_autodiscover_json` missing
`main.rs` references `autodiscover::AutodiscoverJsonParams` and
`autodiscover::handle_autodiscover_json`, but neither existed in `autodiscover.rs`.
**STATUS: CLOSED** — both added with full Autodiscover v2 (JSON) implementation.

---

## CRITICAL — Wrong WBXML Code-Page Mappings (MS-ASWBXML)

### GAP-03 Code page 8 (MeetingResponse) completely wrong
Per MS-ASWBXML the correct mapping is:
`CalendarId=0x05, CollectionId=0x06, MeetingResponse=0x07, RequestId=0x08,
Request=0x09, Result=0x0A, Status=0x0B, UserResponse=0x0C, InstanceId=0x0E`

The old code had `MeetingResponse=0x05, Request=0x06, …` — offset by two positions.
Every WBXML MeetingResponse decoded/encoded with wrong tag names, silently breaking
all meeting accept/decline flows on WBXML clients (Outlook Android 15, etc.).
**STATUS: CLOSED** — full code page 8 rewritten to spec.

### GAP-04 Code page 7 (FolderHierarchy) missing three tags
`FolderCreate=0x13`, `FolderDelete=0x14`, `FolderUpdate=0x15` absent.
**STATUS: CLOSED** — added.

### GAP-05 Code pages 15 (Search) and 20 (ItemOperations) entirely absent
WBXML encoding for Search and ItemOperations responses emitted "Unknown tag" warnings
and produced malformed/empty binary payloads.
**STATUS: CLOSED** — full code pages 15 and 20 added.

### GAP-06 Code page 13 (Ping) missing `MaxFolders=0x0D`
Ping status-6 (folder count exceeded) WBXML could not encode MaxFolders.
**STATUS: CLOSED** — added.

### GAP-07 Code page 18 (Settings) severely incomplete
Only 3 of ~35 tags present. `Get`, `Set`, `DeviceInformation`, `UserInformation`,
`EmailAddresses`, `SMTPAddress`, `PrimarySmtpAddress`, `Accounts`, `Account`,
`AccountId`, `AccountName`, `UserDisplayName`, and many more were absent,
causing garbled Settings WBXML responses.
**STATUS: CLOSED** — complete Settings code page 18 added.

### GAP-08 Code page 10 (ResolveRecipients) missing availability tags
`To=0x10`, `RecipientCount=0x12`, `Availability=0x16`, `StartTime=0x17`,
`EndTime=0x18`, `MergedFreeBusy=0x19` missing. Free/busy WBXML was malformed.
**STATUS: CLOSED** — full code page 10 rewritten to spec.

### GAP-09 Code page 17 (AirSyncBase) missing `AllOrNone=0x08`
**STATUS: CLOSED** — added.

### GAP-10 Code page 0 (AirSync) missing modern Outlook tags
`GetChanges=0x13`, `MoreAvailable=0x14`, `WindowSize=0x15`, `FilterType=0x18`,
`DeletesAsMoves=0x1E`, `Supported=0x20`, `SoftDelete=0x21`, `MIMESupport=0x22`,
`MIMETruncation=0x23`, `Wait=0x24`, `Limit=0x25`, `Partial=0x26`,
`ConversationMode=0x27`, `MaxItems=0x28`, `HeartbeatInterval=0x29` absent.
**STATUS: CLOSED** — all added.

---

## HIGH — Protocol Correctness (MS-ASCAL §2.2.2.2, §2.2.2.40)

### GAP-11 `validate_payload` did not reject server-only fields in requests
MS-ASCAL §2.2.2.2: "A command request MUST NOT include the AppointmentReplyTime."
MS-ASCAL §2.2.2.40: "A command request MUST NOT include the ResponseType element."
Existing test `validates_sync_rejects_response_only_calendar_fields` expected
rejection but `validate_payload` never performed the check — test was failing.
**STATUS: CLOSED** — check added; both server-only fields now rejected in requests.

### GAP-12 Autodiscover JSON v2 handler was undefined stub
New Outlook Windows 11 and Android 15 issue Autodiscover v2 (JSON) before falling
back to v1 XML. The route existed in `main.rs` but the handler was missing.
**STATUS: CLOSED** — full handler with Protocol= query param dispatch added.

### GAP-13 Settings WBXML response silently empty due to missing code page 18
Covered by GAP-07. **STATUS: CLOSED**.

---

## HIGH — Cloudflare / Deployment Configuration

### GAP-15 Worker name mismatch
`CLOUDFLARE_DEPLOYMENT.md` and `wrangler.toml` used `exchange-gateway-edge` /
`exchange-gateway`; user specified `exchange-gateway-db`.
**STATUS: CLOSED** — all references updated to `exchange-gateway-db`.

### GAP-16 D1 database name mismatch
Previous database name was `exchange-gateway`; user specified `exchange_gateway_db`.
**STATUS: CLOSED** — binding and database name updated throughout.

### GAP-17 `docker-compose.yml` did not forward `MAIL_DOMAIN`
`.env` defines `MAIL_DOMAIN` but it was not passed to the container.
**STATUS: CLOSED** — env label added.

### GAP-18 `cloudflared-exchange-origin.yml` example wrong for host-mode cloudflared
The example targeted `http://exchange_gateway:8134` (Docker DNS), but `cloudflared`
on Ubuntu runs on the host, not inside Docker, so it cannot resolve that name.
**STATUS: CLOSED** — corrected to `http://localhost:8134` with explanatory comment.

---

## MEDIUM — Robustness / Completeness

### GAP-19 `timezone.rs` lacked EAS TimeZone blob decoder (MS-ASDTYPE §2.7.6)
The EAS Timezone element carries a 172-byte little-endian binary blob. Passing it
through opaquely caused DTSTART/DTEND to be mis-stamped for clients relying on the
blob bias rather than IANA TZID.
**STATUS: CLOSED** — `decode_eas_timezone_bias()` and `eas_timezone_blob_to_iana()`
helpers implemented in `timezone.rs`.

### GAP-20 WBXML encoder crashed on unrecognised tags
Unknown tags returned `Err`, aborting the entire response instead of skipping.
**STATUS: CLOSED** — encoder logs warning and continues for unknown tags.

### GAP-21 Autodiscover v1 XML EXPR block lacked `<LoginName>`
Outlook Windows 11 uses `<LoginName>` to pre-fill the username.
**STATUS: CLOSED** — added to EXPR block.

### GAP-22 `wrangler.toml` body-size limit inconsistency
Preview env had 4 MiB but production had 1 MiB vs. Rust gateway constant 4 MiB.
**STATUS: CLOSED** — aligned to 4 MiB everywhere.

### GAP-24 `CLOUDFLARE_DEPLOYMENT.md` lacked additive tunnel ingress guidance
User's existing cloudflared serves Stalwart webui; guide did not explain adding a
second ingress rule without breaking the first.
**STATUS: CLOSED** — extended with additive ingress example.

---

## LOW — Remaining Open Gaps

### GAP-25 EWS push/streaming notifications (MS-OXWSPSNTIF) not implemented
Outlook falls back to polling SyncFolderItems automatically.
**OPEN** — out of scope for CalDAV bridge.

### GAP-26 EWS `GetAttachment` / `CreateAttachment` not implemented
Calendar attachments not bridged. Outlook displays items but cannot access
attachments. **OPEN** — beyond CalDAV bridge scope.

### GAP-27 EAS Email class sync is stub only
By design; use-case is calendar-only. **OPEN**.

### GAP-28 WBXML code pages 22 (Email2), 25 (Find) not implemented
Not needed for calendar sync. **OPEN**.

### GAP-29 EAS TimeZone blob round-trip for Windows-only TZIDs
When Outlook sends a Windows TZID not in the IANA database, UTC bias fallback is
used. DST transitions may be slightly wrong until Outlook re-sends the event.
**OPEN** — acceptable for current use-case.

### GAP-30 IPv6 CalDAV connectivity not tested
`reqwest::Client` supports IPv6 but `caldav_base` uses a literal IPv4 address. If
the Stalwart container moves to a dual-stack network, update `config.toml` manually.
**OPEN** — documentation-only mitigation.
