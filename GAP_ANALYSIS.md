# Binder1 Full Gap Analysis for exchange_gateway

## Scope and method

- Source corpus reviewed: full `Binder1.txt` (458,405 lines) and all repository source/config/deployment files.

- Goal checked: native Outlook Windows + Outlook Android calendar interoperability via Exchange-style endpoints over Cloudflare + Stalwart backend.

- Important limitation: this analysis is exhaustive on *documented gaps*; it cannot prove 100% protocol perfection because the current implementation is intentionally partial and non-conformant in multiple mandatory areas.

## Executive verdict

## Implementation update (this revision)

- Implemented command-aware EAS dispatch (MS-ASCMD-focused) using XML root and `Cmd` query fallback, plus explicit `OPTIONS` capability headers.
- Expanded calendar payload mapping for sync responses (MS-ASCAL-focused), including additional fields such as `DtStamp`, `BusyStatus`, and `Sensitivity`.
- Expanded WBXML namespace/tag page coverage and fixed token decoding for content-bit tokens (MS-ASAIRS/MS-ASWBXML path).

Current repository is **not production-ready** for the stated use-case and is **not protocol-complete** for Exchange ActiveSync/EWS expectations from modern Outlook clients. Major blockers include protocol-surface incompleteness, route/config mismatches, persistence API mismatch, and auth/header incompatibility across components.

## Highest-priority blockers (must-fix)

1. **Gateway binding and tunnel mismatch**: `config.toml` binds `0.0.0.0:8133`, while expected tunnel route targets 8134 and compose maps 8134:8134. This prevents deterministic routing in stated deployment. (config.toml / docker-compose.yml / src/main.rs).
2. **Runtime config model mismatch**: `main.rs` loads file-based `/etc/exchange-gateway/config.toml`, but compose injects env vars for a different architecture (JMAP/CF_D1 API), creating deployment drift. (src/main.rs / docker-compose.yml).
3. **Storage API contract mismatch**: Rust `Storage` calls REST paths like `/set_sync_key`, `/upsert_item_map`, `/list_changes_since`; worker only implements generic SQL endpoint `/api/*`. (src/storage.rs / worker/index.js).
4. **Worker auth scheme mismatch**: Rust storage sends `x-gateway-secret`; worker requires `Authorization: Bearer <secret>`. (src/storage.rs / worker/index.js).
5. **EWS surface is stub-level**: EWS handler only identifies a few action names and only implements a static `GetFolder`; other operations return generic empty success, not valid per-operation semantics. (src/ews.rs).
6. **ActiveSync surface is narrow**: EAS supports only naive `FolderSync`, `Provision`, `Sync`, `Ping` via string matching; lacks command/query handling, status taxonomy, robust collection semantics, conflict/state handling expected by ASCMD/ASHTTP. (src/eas.rs).
7. **WBXML implementation non-complete**: Custom WBXML codec handles limited token map and unknown tags heuristically; full MS-ASWBXML conformance requires stricter token/string table/codepage behavior and error handling. (src/wbxml.rs).
8. **Data model persistence gaps**: Current D1 schema models `sync_state`/`ews_sync_state`/`device_info`, but worker has no endpoint-level logic that enforces model invariants required by Rust callers. (d1_schema.sql / worker/index.js / src/storage.rs).
9. **Autodiscover placement risk**: Autodiscover only exists in worker and not in Rust gateway; routing depends entirely on Cloudflare worker path handling and may miss Outlook variants (root/autodiscover subpaths/redirect/legacy variants). (worker/index.js).

## Protocol-family coverage assessment against Binder1

- Total Microsoft protocol specs discovered in Binder1: **129**.
- Implemented/attempted in repo: limited subset of ActiveSync command handling + minimal EWS SOAP + Autodiscover responses + custom WBXML.
- Effective conformance status for target use-case: **Partial/Insufficient**.
### Full protocol inventory and gap classification

| Protocol | Title (Binder1) | Relevance to stated use-case | Current gateway status | Gap classification |
|---|---|---|---|---|
| MS-ASAIRS | Exchange ActiveSync: AirSyncBase Namespace Protocol | Critical | Partially implemented | Major functional gaps |
| MS-ASCAL | Exchange ActiveSync: Calendar Class Protocol | Critical | Partially implemented | Major functional gaps |
| MS-ASCMD | Exchange ActiveSync: Command Reference Protocol | Critical | Partially implemented | Major functional gaps |
| MS-ASCNTC | Exchange ActiveSync: Contact Class Protocol | High | Not explicitly implemented | Unsupported command/class surface |
| MS-ASCON | Exchange ActiveSync: Conversations Protocol | High | Not explicitly implemented | Unsupported command/class surface |
| MS-ASDOC | Exchange ActiveSync: Document Class Protocol | High | Not explicitly implemented | Unsupported command/class surface |
| MS-ASDTYPE | Exchange ActiveSync: Data Types | Critical | Not explicitly implemented | Unsupported command/class surface |
| MS-ASEMAIL | Exchange ActiveSync: Email Class Protocol | Critical | Not explicitly implemented | Unsupported command/class surface |
| MS-ASHTTP | Exchange ActiveSync: HTTP Protocol | Critical | Partially implemented | Major functional gaps |
| MS-ASMS | Exchange ActiveSync: Short Message Service (SMS) Protocol | High | Not explicitly implemented | Unsupported command/class surface |
| MS-ASNOTE | Exchange ActiveSync: Notes Class Protocol | High | Not explicitly implemented | Unsupported command/class surface |
| MS-ASPROV | Exchange ActiveSync: Provisioning Protocol | Critical | Partially implemented | Major functional gaps |
| MS-ASRM | Exchange ActiveSync: Rights Management Protocol | High | Not explicitly implemented | Unsupported command/class surface |
| MS-ASTASK | Exchange ActiveSync: Tasks Class Protocol | High | Not explicitly implemented | Unsupported command/class surface |
| MS-ASWBXML | Exchange ActiveSync: WAP Binary XML (WBXML) Algorithm | Critical | Partially implemented | Major functional gaps |
| MS-MCI | Microsoft ZIP (MSZIP) Compression and Decompression Data Structure | Low/Indirect | Not implemented | Out of current implementation scope |
| MS-OXABREF | Address Book Name Service Provider Interface (NSPI) Referral Protocol | Low/Indirect | Not implemented | Out of current implementation scope |
| MS-OXBBODY | Best Body Retrieval Algorithm | Low/Indirect | Not implemented | Out of current implementation scope |
| MS-OXCDATA | Data Structures | Low/Indirect | Not implemented | Out of current implementation scope |
| MS-OXCEXT | Client Extension Message Object Protocol | Low/Indirect | Not implemented | Out of current implementation scope |
| MS-OXCFOLD | Folder Object Protocol | Low/Indirect | Not implemented | Out of current implementation scope |
| MS-OXCFXICS | Bulk Data Transfer Protocol | Low/Indirect | Not implemented | Out of current implementation scope |
| MS-OXCICAL | iCalendar to Appointment Object Conversion Algorithm | Medium | Not implemented | Out of current implementation scope |
| MS-OXCMAIL | RFC 2822 and MIME to Email Object Conversion Algorithm | Low/Indirect | Not implemented | Out of current implementation scope |
| MS-OXCMAPIHTTP | Messaging Application Programming Interface (MAPI) Extensions for HTTP | Low/Indirect | Not implemented | Out of current implementation scope |
| MS-OXCMSG | Message and Attachment Object Protocol | Low/Indirect | Not implemented | Out of current implementation scope |
| MS-OXCNOTIF | Core Notifications Protocol | Low/Indirect | Not implemented | Out of current implementation scope |
| MS-OXCPERM | Exchange Access and Operation Permissions Protocol | Low/Indirect | Not implemented | Out of current implementation scope |
| MS-OXCPRPT | Property and Stream Object Protocol | Low/Indirect | Not implemented | Out of current implementation scope |
| MS-OXCROPS | Remote Operations (ROP) List and Encoding Protocol | Low/Indirect | Not implemented | Out of current implementation scope |
| MS-OXCRPC | Wire Format Protocol | Low/Indirect | Not implemented | Out of current implementation scope |
| MS-OXCSPAM | Spam Confidence Level Protocol | Low/Indirect | Not implemented | Out of current implementation scope |
| MS-OXCSTOR | Store Object Protocol | Low/Indirect | Not implemented | Out of current implementation scope |
| MS-OXCTABL | Table Object Protocol | Low/Indirect | Not implemented | Out of current implementation scope |
| MS-OXDISCO | Autodiscover HTTP Service Protocol | Critical | Partially implemented | Major functional gaps |
| MS-OXDSCLI | Autodiscover Publishing and Lookup Protocol | Low/Indirect | Not implemented | Out of current implementation scope |
| MS-OXIMAP4 | Internet Message Access Protocol Version 4 (IMAP4) Extensions | Low/Indirect | Not implemented | Out of current implementation scope |
| MS-OXLDAP | Lightweight Directory Access Protocol (LDAP) Version 3 Extensions | Low/Indirect | Not implemented | Out of current implementation scope |
| MS-OXMSG | Outlook Item (.msg) File Format | Low/Indirect | Not implemented | Out of current implementation scope |
| MS-OXNSPI | Exchange Server Name Service Provider Interface (NSPI) Protocol | Low/Indirect | Not implemented | Out of current implementation scope |
| MS-OXOABK | . PidTagObjectType ([MS-OXOABK] section 2.2.3.10) | Low/Indirect | Not implemented | Out of current implementation scope |
| MS-OXOAB | Offline Address Book (OAB) File Format and Schema | Low/Indirect | Not implemented | Out of current implementation scope |
| MS-OXOABKT | Address Book User Interface Templates Protocol | Low/Indirect | Not implemented | Out of current implementation scope |
| MS-OXOCAL | Appointment and Meeting Object Protocol | Medium | Not implemented | Out of current implementation scope |
| MS-OXORMDR | . PidLidReminderSet ([MS-OXORMDR] section 2.2.1.1) | Low/Indirect | Not implemented | Out of current implementation scope |
| MS-OXOCFG | Configuration Information Protocol | Low/Indirect | Not implemented | Out of current implementation scope |
| MS-OXOCNTC | Contact Object Protocol | Low/Indirect | Not implemented | Out of current implementation scope |
| MS-OXODLGT | Delegate Access Configuration Protocol | Low/Indirect | Not implemented | Out of current implementation scope |
| MS-OXOSFLD | . Calendar | Low/Indirect | Not implemented | Out of current implementation scope |
| MS-OXODOC | Document Object Protocol | Low/Indirect | Not implemented | Out of current implementation scope |
| MS-OXOFLAG | Informational Flagging Protocol | Low/Indirect | Not implemented | Out of current implementation scope |
| MS-OXOTASK | . PidLidTaskStartDate ([MS-OXOTASK] section 2.2.2.2.4) | Low/Indirect | Not implemented | Out of current implementation scope |
| MS-OXOJRNL | Journal Object Protocol | Low/Indirect | Not implemented | Out of current implementation scope |
| MS-OXOMSG | Email Object Protocol | Low/Indirect | Not implemented | Out of current implementation scope |
| MS-OXONOTE | Note Object Protocol | Low/Indirect | Not implemented | Out of current implementation scope |
| MS-OXOPFFB | Public Folder-Based Free/Busy Protocol | Low/Indirect | Not implemented | Out of current implementation scope |
| MS-OXOPOST | Post Object Protocol | Low/Indirect | Not implemented | Out of current implementation scope |
| MS-OXORMMS | Rights-Managed Email Object Protocol | Low/Indirect | Not implemented | Out of current implementation scope |
| MS-OXORSS | RSS Object Protocol | Low/Indirect | Not implemented | Out of current implementation scope |
| MS-OXORULE | Email Rules Protocol | Low/Indirect | Not implemented | Out of current implementation scope |
| MS-OXOSMIME | S/MIME Email Object Algorithm | Low/Indirect | Not implemented | Out of current implementation scope |
| MS-OXOSMMS | Short Message Service (SMS) and Multimedia Messaging Service (MMS) Object Protocol | Low/Indirect | Not implemented | Out of current implementation scope |
| MS-OXOSRCH | Search Folder List Configuration Protocol | Low/Indirect | Not implemented | Out of current implementation scope |
| MS-OXOUM | Voice Mail and Fax Objects Protocol | Low/Indirect | Not implemented | Out of current implementation scope |
| MS-OXPFOAB | Offline Address Book (OAB) Public Folder Retrieval Protocol | Low/Indirect | Not implemented | Out of current implementation scope |
| MS-OXPHISH | Phishing Warning Protocol | Low/Indirect | Not implemented | Out of current implementation scope |
| MS-OXPOP3 | Post Office Protocol Version 3 (POP3) Extensions | Low/Indirect | Not implemented | Out of current implementation scope |
| MS-OXPROPS | Exchange Server Protocols Master Property List | Low/Indirect | Not implemented | Out of current implementation scope |
| MS-OXPROTO | Exchange Server Protocols System Overview | Low/Indirect | Not implemented | Out of current implementation scope |
| MS-OXPSVAL | Email Postmark Validation Algorithm | Low/Indirect | Not implemented | Out of current implementation scope |
| MS-OXRTFCP | Rich Text Format (RTF) Compression Algorithm | Low/Indirect | Not implemented | Out of current implementation scope |
| MS-OXRTFEX | Rich Text Format (RTF) Extensions Algorithm | Low/Indirect | Not implemented | Out of current implementation scope |
| MS-OXSHARE | Sharing Message Object Protocol | Low/Indirect | Not implemented | Out of current implementation scope |
| MS-OXSHRMSG | Sharing Message Attachment Schema | Low/Indirect | Not implemented | Out of current implementation scope |
| MS-OXSMTP | Simple Mail Transfer Protocol (SMTP) Extensions | Low/Indirect | Not implemented | Out of current implementation scope |
| MS-OXTNEF | Transport Neutral Encapsulation Format (TNEF) Data Algorithm | Low/Indirect | Not implemented | Out of current implementation scope |
| MS-OXVCARD | vCard to Contact Object Conversion Algorithm | Low/Indirect | Not implemented | Out of current implementation scope |
| MS-OXWAVLS | Availability Web Service Protocol | Medium | Not implemented | Out of current implementation scope |
| MS-OXWCONFIG | Web Service Configuration Protocol | Critical | Partially implemented | Major functional gaps |
| MS-OXWMT | Mail Tips Web Service Extensions | Low/Indirect | Not implemented | Out of current implementation scope |
| MS-OXWOAB | Offline Address Book (OAB) Retrieval File Format | Low/Indirect | Not implemented | Out of current implementation scope |
| MS-OXWOOF | Out of Office (OOF) Web Service Protocol | Low/Indirect | Not implemented | Out of current implementation scope |
| MS-OXWSADISC | Autodiscover Publishing and Lookup SOAP-Based Web Service Protocol | Low/Indirect | Not implemented | Out of current implementation scope |
| MS-OXWSARCH | Archiving Web Service Protocol | Low/Indirect | Not implemented | Out of current implementation scope |
| MS-OXWSATT | Attachment Handling Web Service Protocol | Low/Indirect | Not implemented | Out of current implementation scope |
| MS-OXWSBTRF | Bulk Transfer Web Service Protocol | Low/Indirect | Not implemented | Out of current implementation scope |
| MS-OXWSCDATA | Common Web Service Data Types | Low/Indirect | Not implemented | Out of current implementation scope |
| MS-OXWSCEXT | Client Extension Web Service Protocol | Low/Indirect | Not implemented | Out of current implementation scope |
| MS-OXWSCONT | Contacts Web Service Protocol | Low/Indirect | Not implemented | Out of current implementation scope |
| MS-OXWSCONV | Conversations Web Service Protocol | Low/Indirect | Not implemented | Out of current implementation scope |
| MS-OXWSCORE | Core Items Web Service Protocol | Low/Indirect | Not implemented | Out of current implementation scope |
| MS-OXWSCOS | Unified Contact Store Web Service Protocol | Low/Indirect | Not implemented | Out of current implementation scope |
| MS-OXWSCVTID | Convert Item Identifier Web Service Protocol | Low/Indirect | Not implemented | Out of current implementation scope |
| MS-OXWSDLGM | Delegate Access Management Web Service Protocol | Low/Indirect | Not implemented | Out of current implementation scope |
| MS-OXWSDLIST | Distribution List Creation and Usage Web Service Protocol | Low/Indirect | Not implemented | Out of current implementation scope |
| MS-OXWSEDISC | Electronic Discovery (eDiscovery) Web Service Protocol | Low/Indirect | Not implemented | Out of current implementation scope |
| MS-OXWSFOLD | Folders and Folder Permissions Web Service Protocol | Low/Indirect | Not implemented | Out of current implementation scope |
| MS-OXWSGNI | Nonindexable Item Web Service Protocol | Low/Indirect | Not implemented | Out of current implementation scope |
| MS-OXWSGTRM | Get Rooms List Web Service Protocol | Low/Indirect | Not implemented | Out of current implementation scope |
| MS-OXWSGTZ | Get Server Time Zone Web Service Protocol | Low/Indirect | Not implemented | Out of current implementation scope |
| MS-OXWSITEMID | Web Service Item ID Algorithm | Low/Indirect | Not implemented | Out of current implementation scope |
| MS-OXWSLVID | Federated Internet Authentication Web Service Protocol | Low/Indirect | Not implemented | Out of current implementation scope |
| MS-OXWSMSG | Email Message Types Web Service Protocol | Low/Indirect | Not implemented | Out of current implementation scope |
| MS-OXWSMSHR | Folder Sharing Web Service Protocol | Low/Indirect | Not implemented | Out of current implementation scope |
| MS-OXWSMTGS | Calendaring Web Service Protocol | Low/Indirect | Not implemented | Out of current implementation scope |
| MS-OXWSMTRK | Message Tracking Web Service Protocol | Low/Indirect | Not implemented | Out of current implementation scope |
| MS-OXWSNTIF | Notifications Web Service Protocol | Low/Indirect | Not implemented | Out of current implementation scope |
| MS-OXWSOLPS | Online Personal Search Web Service Protocol | Low/Indirect | Not implemented | Out of current implementation scope |
| MS-OXWSPED | Password Expiration Date Web Service Protocol | Low/Indirect | Not implemented | Out of current implementation scope |
| MS-OXWSPERS | Persona Web Service Protocol | Low/Indirect | Not implemented | Out of current implementation scope |
| MS-OXWSPHOTO | Photo Web Service Protocol | Low/Indirect | Not implemented | Out of current implementation scope |
| MS-OXWSPOST | Post Items Web Service Protocol | Low/Indirect | Not implemented | Out of current implementation scope |
| MS-OXWSPSNTIF | Push Notifications Web Service Protocol | Low/Indirect | Not implemented | Out of current implementation scope |
| MS-OXWSRSLNM | Resolve Recipient Names Web Service Protocol | Low/Indirect | Not implemented | Out of current implementation scope |
| MS-OXWSRULES | Inbox Rules Web Service Protocol | Low/Indirect | Not implemented | Out of current implementation scope |
| MS-OXWSSMBX | Site Mailbox Web Service Protocol | Low/Indirect | Not implemented | Out of current implementation scope |
| MS-OXWSSRCH | Mailbox Search Web Service Protocol | Low/Indirect | Not implemented | Out of current implementation scope |
| MS-OXWSSYNC | Mailbox Contents Synchronization Web Service Protocol | Low/Indirect | Not implemented | Out of current implementation scope |
| MS-OXWSTASK | Tasks Web Service Protocol | Low/Indirect | Not implemented | Out of current implementation scope |
| MS-OXWSURPT | Retention Tag Web Service Protocol | Low/Indirect | Not implemented | Out of current implementation scope |
| MS-OXWSUSRCFG | User Configuration Web Service Protocol | Low/Indirect | Not implemented | Out of current implementation scope |
| MS-OXWSXPROP | Extended Properties Structure | Low/Indirect | Not implemented | Out of current implementation scope |
| MS-OXWUMS | Voice Mail Settings Web Service Protocol | Low/Indirect | Not implemented | Out of current implementation scope |
| MS-PATCH | LZX DELTA Compression and Decompression | Low/Indirect | Not implemented | Out of current implementation scope |
| MS-XJRNL | Journal Record Message File Format | Low/Indirect | Not implemented | Out of current implementation scope |
| MS-XLOGIN | Simple Mail Transfer Protocol (SMTP) AUTH LOGIN Extension | Low/Indirect | Not implemented | Out of current implementation scope |
| MS-XOAUTH | OAuth 2.0 Authorization Protocol Extensions | Low/Indirect | Not implemented | Out of current implementation scope |
| MS-XWDCAL | Web Distributed Authoring and Versioning (WebDAV) Extensions for Calendar Support | Low/Indirect | Not implemented | Out of current implementation scope |
| MS-XWDVSEC | Web Distributed Authoring and Versioning (WebDAV) Protocol Security Descriptor Extensions | Low/Indirect | Not implemented | Out of current implementation scope |

## Concrete code-level gap map

### A) Endpoint and transport gaps

- No explicit `OPTIONS` handling/CORS policy surface for EAS endpoint in current code path.
- No request throttling, per-device state machine, or backoff semantics for mobile sync behavior.
- No TLS termination in Rust service itself (depends on Cloudflare), which is acceptable only if end-to-end trust/path constraints are explicitly satisfied.

### B) EAS protocol gaps

- Command detection is XML substring-based; no robust command parsing by query `Cmd` and canonical namespaces.
- Sync collection identity is hardcoded (`collection_id = "1"`), not negotiated per-folder/account model.
- Provision policy response is static and not persisted/validated across policy keys/device ids.
- Missing broad command surface: `ItemOperations`, `MeetingResponse`, `SendMail`, `Settings`, `Search`, etc.
- Missing robust status/error codes and server-side semantics expected by Outlook clients.

### C) EWS protocol gaps

- No operation-specific schema validation for EWS request/response bodies.
- `FindItem`/`SyncFolderItems` are detected but not truly implemented with item deltas, sync states, and paging semantics.
- Response envelopes are mostly static; IDs/change keys are placeholders.

### D) Data and consistency gaps

- Storage abstraction expects strongly-typed worker API routes that do not exist.
- D1 schema is SQL-only; no worker business logic layer to preserve invariants and perform transactional checks used by sync flows.
- No migration/versioning workflow or idempotency guarantees across retries.

### E) Deployment/configuration gaps for stated Cloudflare + Stalwart setup

- Port mismatch across config/tunnel/compose.
- Compose points to env vars not consumed by current code architecture.
- Worker DB binding names in user scenario (`EXCHANGE_DB`) differ from Rust storage secret/header expectations without adapter logic.

## Required remediation backlog (ordered)

1. Unify architecture: pick one backend contract (typed worker API vs raw SQL proxy) and implement it end-to-end consistently.
2. Fix deployment contract: align `config.toml` bind port with Cloudflare tunnel routes and compose mappings.
3. Implement proper worker endpoints (`set_sync_key`, `upsert_item_map`, `delete_item_by_server_id`, `list_changes_since`) with auth, validation, and transactional guarantees.
4. Replace substring-based EAS parsing with namespace-aware parser supporting command/query semantics and full status handling.
5. Implement real EWS operations required by Outlook (at least GetFolder, FindFolder, FindItem, GetItem, SyncFolderItems, CreateItem, UpdateItem, DeleteItem) with proper SOAP fault handling.
6. Expand protocol coverage to critical ActiveSync classes for calendar invitations/updates and device lifecycle.
7. Add conformance test harness (golden WBXML vectors, EAS command integration tests, EWS SOAP fixtures, Outlook interoperability matrix).
8. Add observability and hardening (structured logs, request IDs, timeout/retry policy, rate limiting, security headers, panic-free error paths).
9. Validate against Binder1 protocol requirements with a traceability matrix mapping each MUST/SHOULD to code/tests.

## Definition of done for your specific use-case

- Outlook Windows + Android can autodiscover and authenticate without manual protocol hacks.
- Calendar CRUD, recurrence, attendee updates, meeting responses, and deletion round-trip correctly between Outlook clients and Stalwart CalDAV data.
- Sync state is stable across restarts and multi-device use.
- Negative paths (invalid creds, malformed WBXML/SOAP, stale sync keys, concurrent edits) are protocol-correct and deterministic.
- End-to-end deployment docs exactly match Cloudflare Tunnel + Worker + D1 + Stalwart runtime settings.