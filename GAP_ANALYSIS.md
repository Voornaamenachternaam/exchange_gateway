# GAP ANALYSIS — Exchange Gateway

Analysis date: 2026-03-29 (updated)
Protocol reference: Binder1.txt (MS-ASAIRS, MS-ASCAL, MS-ASCMD, MS-ASPROV,
MS-ASWBXML, MS-ASHTTP, MS-ASDTYPE, MS-OXWSCAL, MS-OXWSCORE, MS-OXWSFOLD,
MS-OXWSSYNC, MS-OXWSMTGS, MS-OXWSADISC, MS-OXWSCDATA, and related specs)

---

## PREVIOUS GAPS (Session 1 — all carried over)

### GAP-01 `src/timezone.rs` is missing
**STATUS: CLOSED** — file created with EAS TimeZone blob decoder.

### GAP-02 `AutodiscoverJsonParams` struct and `handle_autodiscover_json` missing
**STATUS: CLOSED** — both added with full Autodiscover v2 (JSON) implementation.

### GAP-03 Code page 8 (MeetingResponse) completely wrong
**STATUS: CLOSED** — full code page 8 rewritten to spec.

### GAP-04 Code page 7 (FolderHierarchy) missing three tags
**STATUS: CLOSED** — `FolderCreate`, `FolderDelete`, `FolderUpdate` added.

### GAP-05 Code pages 15 (Search) and 20 (ItemOperations) entirely absent
**STATUS: CLOSED** — full code pages 15 and 20 added.

### GAP-06 Code page 13 (Ping) missing `MaxFolders=0x0D`
**STATUS: CLOSED** — added.

### GAP-07 Code page 18 (Settings) severely incomplete
**STATUS: CLOSED** — complete Settings code page 18 added.

### GAP-08 Code page 10 (ResolveRecipients) missing availability tags
**STATUS: CLOSED** — full code page 10 rewritten to spec.

### GAP-09 Code page 17 (AirSyncBase) missing `AllOrNone=0x08`
**STATUS: CLOSED** — added.

### GAP-10 Code page 0 (AirSync) missing modern Outlook tags
**STATUS: CLOSED** — all modern Outlook AirSync tags added.

### GAP-11 `validate_payload` did not reject server-only fields in requests
**STATUS: CLOSED** — `AppointmentReplyTime` and `ResponseType` now rejected in Sync requests.

### GAP-12 Autodiscover JSON v2 handler was undefined stub
**STATUS: CLOSED** — full handler with Protocol= query param dispatch added.

### GAP-13 Settings WBXML response silently empty due to missing code page 18
**STATUS: CLOSED** — covered by GAP-07.

### GAP-15 Worker name mismatch
**STATUS: CLOSED** — all references updated to `exchange-gateway-db`.

### GAP-16 D1 database name mismatch
**STATUS: CLOSED** — binding and database name updated to `exchange_gateway_db`.

### GAP-17 `docker-compose.yml` did not forward `MAIL_DOMAIN`
**STATUS: CLOSED** — env label added.

### GAP-18 `cloudflared-exchange-origin.yml` example wrong for host-mode cloudflared
**STATUS: CLOSED** — corrected to `http://localhost:8134`.

### GAP-19 `timezone.rs` lacked EAS TimeZone blob decoder (MS-ASDTYPE §2.7.6)
**STATUS: CLOSED** — `decode_eas_timezone_bias()` and `eas_timezone_blob_to_iana()` implemented.

### GAP-20 WBXML encoder crashed on unrecognised tags
**STATUS: CLOSED** — encoder logs warning and continues for unknown tags.

### GAP-21 Autodiscover v1 XML EXPR block lacked `<LoginName>`
**STATUS: CLOSED** — added to EXPR block.

### GAP-22 `wrangler.toml` body-size limit inconsistency
**STATUS: CLOSED** — aligned to 4 MiB everywhere.

### GAP-24 `CLOUDFLARE_DEPLOYMENT.md` lacked additive tunnel ingress guidance
**STATUS: CLOSED** — extended with additive ingress example.

### GAP-25 EWS push/streaming notifications (MS-OXWSPSNTIF) not implemented
**OPEN** — out of scope for CalDAV bridge; Outlook falls back to polling.

### GAP-26 EWS `GetAttachment` / `CreateAttachment` not implemented
**OPEN** — beyond CalDAV bridge scope.

### GAP-27 EAS Email class sync is stub only
**OPEN** — by design; use-case is calendar-only.

### GAP-28 WBXML code pages 22 (Email2), 25 (Find) not implemented
**OPEN** — not needed for calendar sync.

### GAP-29 EAS TimeZone blob round-trip for Windows-only TZIDs
**OPEN** — UTC bias fallback acceptable for current use-case.

### GAP-30 IPv6 CalDAV connectivity not tested
**OPEN** — documentation-only mitigation.

---

## NEW GAPS (Session 2 — found 2026-03-29)

### GAP-31 EWS SOAP responses missing `<s:Header>` with `ServerVersionInfo`
Per MS-OXWSCORE §3.1.4 and all EWS specs, every SOAP response MUST include a
`<s:Header>` with `<t:ServerVersionInfo>`. Without it, new Outlook for Windows 11
(v20251205004.10+) cannot determine the server's EWS schema version and falls back
to reduced protocol capability, causing calendar sync failures.
**STATUS: CLOSED** — `soap_ok` and `soap_fault` updated to include proper SOAP header.

### GAP-32 EAS Sync WindowSize not respected; MoreAvailable never emitted
Per MS-ASCMD §2.2.3.199, the server MUST honour the WindowSize element in Sync
requests. Per §2.2.3.116, `<MoreAvailable/>` MUST be returned when the number of
pending changes exceeds WindowSize. The gateway's `perform_sync` ignored the
`_window_size` parameter and never emitted `<MoreAvailable/>`. With large calendars
this causes Outlook to believe it has received all items when it has not.
**STATUS: CLOSED** — `perform_sync` now parses WindowSize from request, respects the
limit, and emits `<MoreAvailable/>` when additional changes remain.

### GAP-33 Provision initial response missing `<Data><EASProvisionDoc>` element
Per MS-ASPROV §2.2.2.28 and §3.1.5.1.1, the server's response to the initial
Provision request (PolicyKey=0) MUST include a `<Data>` element containing an
`<EASProvisionDoc>` element with the server's security policy. Without this, Android
15 Outlook (v5.2607.0+) aborts provisioning with a protocol error before any
calendar sync takes place.
**STATUS: CLOSED** — `handle_provision` now returns a complete EASProvisionDoc that
allows unrestricted device use (no PIN required, no wipe on failed login, etc.).

### GAP-34 DeviceInformation in Provision request not stored
Per MS-ASPROV §3.1.5.1.1 and MS-ASCMD §2.2.1.18, when protocol version 14.1,
16.0, or 16.1 is used, the `<DeviceInformation>` element MUST be present as a
child of the `<Provision>` element in the initial request. The gateway discarded
this data entirely. While not strictly required for calendar sync, not storing it
prevents future diagnostics and denies the gateway a device-friendly-name for
logging.
**STATUS: CLOSED** — device FriendlyName, Model, OS, IMEI parsed from Provision
DeviceInformation block and stored via the `upsert_device_info` Worker route.

### GAP-35 AllDayEvent + Timezone constraint not validated (MS-ASCAL §2.2.2.1)
Per MS-ASCAL §2.2.2.1 and §2.2.2.44: "If a client includes an Add or Change
element in a Sync request with AllDayEvent set to 1, the client MUST NOT include
the Timezone element." The gateway did not validate this constraint, silently
accepting malformed payloads that could corrupt the CalDAV store.
**STATUS: CLOSED** — `validate_payload` now returns `Err` when `<AllDayEvent>1`
coexists with `<Timezone>` in a Sync Add or Change.

### GAP-36 CalDAV calendar discovery uses PROPFIND Depth:0 (finds home, not collections)
Per RFC 4791 §7.8 and Stalwart v0.15.5 documentation, a `calendar-query` REPORT
is valid only on a calendar *collection*, not on a calendar home-set. The gateway
issued PROPFIND Depth:0 on the calendar home `/dav/cal/{user}/`, confirmed the home
exists, then issued REPORT Depth:1 on that home URL. For Stalwart, VEVENTs reside
in sub-collections (e.g. `/dav/cal/{user}/default/`); the REPORT on the home at
Depth:1 does not descend into sub-collections to return individual calendar items.
This is the root cause of empty calendar sync responses on fresh deployments.
**STATUS: CLOSED** — `find_user_calendars` now issues PROPFIND Depth:1 to discover
actual calendar collections (resourcetype includes `<C:calendar/>`). Falls back to
well-known `/dav/cal/{user}/default/` path if discovery returns no collections.

### GAP-37 `ews.rs` compile errors: malformed `validate_requested_folder` and incomplete `operation_error_response`
Two compile-blocking defects in `ews.rs`:
1. `operation_error_response` body contained `let resp = match action {…};` and then
   immediately a nested `fn validate_requested_folder` definition. The outer function
   had no return statement, causing a type error.
2. `validate_requested_folder` contained a duplicate `for` loop where the first
   `if let` block was never closed before the second loop began, producing mismatched
   braces.
3. Two extra closing braces at the end of the file caused spurious parse errors.
**STATUS: CLOSED** — `operation_error_response` is now a complete standalone function
returning proper EWS error SOAP envelopes. `validate_requested_folder` is a correct
standalone function with the duplicate for-loop collapsed into one clean pass.

### GAP-38 Sync `GetChanges` element not parsed or respected (MS-ASCMD §2.2.3.84)
Per MS-ASCMD §2.2.3.84: "If the client does not want server changes returned, the
request MUST include the GetChanges element with a value of 0 (FALSE)." The gateway
ignored this element entirely, always returning server changes regardless of client
instruction. This breaks Outlook's write-only push path where the client uploads
mutations but does not want a full change set in return.
**STATUS: CLOSED** — `handle` in `eas.rs` parses `<GetChanges>` from the Sync body;
`perform_sync` receives and respects the flag, skipping the CalDAV REPORT fetch when
GetChanges=false and SyncKey is non-zero.

### GAP-39 WBXML-decoded XML fails namespace validation check
The `validate_payload` function checked `xml.contains(grammar.namespace)` (e.g.
`"AirSync:"`) to reject requests with wrong namespaces. But the WBXML decoder
produces bare tag names without any xmlns declarations (e.g. `<Sync>` instead of
`<Sync xmlns="AirSync:">`), so all valid WBXML Sync requests were rejected with
"Request missing expected command namespace". Android 15 Outlook uses WBXML for all
EAS traffic.
**STATUS: CLOSED** — namespace check now also accepts the root tag name pattern
(e.g. `<Sync`) so that WBXML-decoded XML passes validation.

### GAP-40 FilterType element for calendar Sync not implemented (MS-ASCMD §2.2.3.68)
Per MS-ASCMD §2.2.3.68 and MS-ASCAL §2.2.2.1: "Calendar items that are in the
future or that have recurrence but no end date are sent to the client regardless of
the FilterType element value." The gateway ignored FilterType entirely, always
querying ±52 weeks. While future items are always included (correct), the past
window was fixed at 52 weeks regardless of the client's request. Outlook for Android
15 typically sends FilterType=5 (1 month back), and FilterType=0 means all items.
**STATUS: CLOSED** — `perform_sync` now parses `<FilterType>` from the Sync request
Options element and adjusts the CalDAV query start time accordingly. Values:
0=all, 1=1 day, 2=3 days, 3=1 week, 4=2 weeks, 5=1 month, 6=3 months, 7=6 months.
Future items and recurring items without end date are always included per spec.

### GAP-41 Provision grammar validation rejects valid EAS 16.x Provision requests
The `validate_payload` grammar for "provision" required `<Policies>` and `<Policy>`
in the body. However EAS 16.x clients (both Outlook Windows 11 and Android 15) send
a Provision request that contains `<DeviceInformation>` and optionally `<Policies>`,
but the ordering and presence may vary. Strict validation blocked initial pairing.
**STATUS: CLOSED** — Provision grammar validation now only requires the `<Provision>`
namespace; individual required sub-elements are validated inside `handle_provision`.

### GAP-42 `upsert_device_info` route missing from Worker (needed for GAP-34)
No `/api/upsert_device_info` endpoint existed in `worker/index.js`, so device
information from Provision requests could not be persisted.
**STATUS: CLOSED** — endpoint added to `worker/index.js` and corresponding
`upsert_device_info` method added to `storage.rs`.

---

## OPEN GAPS (carried from session 1, confirmed still open)

- **GAP-25** EWS push/streaming notifications — out of scope.
- **GAP-26** EWS GetAttachment / CreateAttachment — out of scope.
- **GAP-27** EAS Email class sync stub — by design.
- **GAP-28** WBXML code pages 22, 25 — not needed for calendar.
- **GAP-29** EAS TimeZone blob round-trip for Windows-only TZIDs — UTC fallback acceptable.
- **GAP-30** IPv6 CalDAV connectivity — documentation mitigation only.
