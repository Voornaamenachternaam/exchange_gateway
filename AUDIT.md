# Exchange Gateway Audit — Prioritized Gap List for the Target Use-Case

**Scope note:** This audit reviewed the full tree (~96k LOC of Rust across 60+ modules: EAS, EWS, MAPI/HTTP, Autodiscover, JMAP client, CalDAV/CardDAV fallback, SMTP, storage, WBXML, cloudflared config, Dockerfile, compose). The code base is impressively complete and spec-annotated (MS-ASWBXML/MS-ASCMD/MS-ASPROV references with traceable doc sections, snapshot tests, protocol fixtures, a Cloudflare smoke script). The gaps below are what stands between the current state and "frictionless, correct, complete" for: **Android Settings → "Exchange" account (direct EAS 16.1 over HTTPS through cloudflared)** + **New Outlook for Windows 20251205004.10** + **Outlook Android 5.2618.2**, with **Stalwart v0.16.23 as JMAP-first backend**.

`cargo check` could not be run in the audit environment (no Rust toolchain); verification item #3 covers this.

---

## P0 — Blocks the use-case outright

### 1. Cloudflare 100-second timeout vs. EAS Ping heartbeat (push is silently broken)
`src/eas.rs` honors spec heartbeats `MIN_HEARTBEAT_SECS=60 .. MAX_HEARTBEAT_SECS=3540` and long-polls the full duration (`deadline = now + heartbeat`, tick loop at 15s). Cloudflare's proxied HTTP (cloudflared tunnel goes through the CF edge) terminates any request with no response after ~100s with 524 on free/pro plans. Every Ping with `HeartbeatInterval > ~100` will be cut, causing Outlook/Android to retry-loop, miss push, and burn battery. Fix: clamp the *effective* Pings behind Cloudflare to ≤ ~90s (env-configurable, e.g. `GATEWAY_MAX_PING_HEARTBEAT=80`) and document it in `CLOUDFLARED_SETUP.md`, or auto-detect the tunnel via `CF-RAY` header. This is the single biggest "it syncs but mail arrives late" defect.

### 2. Global (not per-user) rate limiter will self-DoS the two clients
`src/rate_limit.rs` applies one global governor bucket to all authenticated traffic. All requests arrive from Cloudflare edge IPs, and Outlook Android + New Outlook (whose EAS traffic comes from **Microsoft datacenter IPs**, since both clients sync through Microsoft's cloud sync service for non-Exchange-Online accounts) will throttle each other's Ping/Sync streams → random 429s = sync stalls. Fix: rate-limit keyed by authenticated user (or `CF-Connecting-IP`/DeviceId), with generous burst for Ping/Ping-probe cycles.

### 3. Build/verification is not proven in this repo state
`Dockerfile` compiles with `RUSTFLAGS="-D warnings"` on Rust 1.98.1, but nothing in-repo proves the workspace currently compiles clean (`cargo check` couldn't run during the audit), and there is no CI workflow (`.gitlab-ci`/`.github/workflows` absent). Required: CI job doing `cargo check/test/clippy/doc` + container build + the existing `tests/outlook_cloudflare_smoke.sh` + `jmap_calendar_deploy.rs` against a pinned Stalwart.

### 4. Stalwart version not pinned
The provided compose uses `stalwartlabs/stalwart:latest`; the gateway is written/tested against v0.16.x semantics (JMAP Calendars capability check, `Email/changes`, JSCard). `:latest` silently escapes the v0.16.23 target. Pin `stalwartlabs/stalwart:v0.16.23` (matching the gateway's startup capability verification, which is already in place — good).

### 5. Gateway image isn't actually published to a registry
compose builds locally (`build:` + `image: exchange-gateway:latest`). The use-case states images "will be hosted in a container registry" — add a registry image tag (`registry.example.com/exchange-gateway:0.1.0`), a publish pipeline, and `pull_policy`/digest pinning. Frictionless deployment on Ubuntu requires `docker compose pull && up -d`, not a source checkout.

### 6. No `.env.example` / first-boot template
`GATEWAY_HMAC_SECRET`, `GATEWAY_HOST`, `GATEWAY_MAIL_DOMAIN` are `:?`-required; compose also mounts `/armblock/exchange-gateway/certs` unconditionally (fails if the dir is absent). Ship `.env.example`, make the certs mount optional (docs), and add a `docker compose config` validation step to docs.

### 7. DNS/tunnel prerequisites for Android "Exchange" signup are underspecified
Android's Exchange wizard and AutoDetect hit `https://autodiscover.<mail-domain>/autodiscover/autodiscover.xml` (and root-domain fallback). `cloudflared/config.yml` correctly defines an `autodiscover.*` ingress but only as `autodiscover.example.com` placeholders, and `CLOUDFLARED_SETUP.md` Step 2 walks only one public hostname. Document both hostnames + DNS records bound to the real `GATEWAY_MAIL_DOMAIN`, and confirm the Autodiscover XML MobileSync response (already implemented in `autodiscover.rs`, with `ResponseSchema::MobileSync` detection) returns the `calendar.*`/gateway URL for every mailbox domain offered.

---

## P1 — Correctness risks against the spec / the two named clients

### 8. Client reality check: both target clients sync via Microsoft's cloud for third-party EAS accounts
Outlook Android 5.26.xx and New Outlook for Windows do not (by default) speak EAS from the device for non-M365 accounts — Microsoft's sync infrastructure connects to the gateway as the EAS client. Implications the code/docs must absorb: (a) Autodiscover JSON V2 (present) must be reachable and correct from Microsoft IPs; (b) Basic-auth credentials transit Microsoft unless the account skips cloud sync — document this plainly for the user; (c) IP allowlisting/rate-limiting (#2) must not block Microsoft ASNs; (d) Functionally validate with Microsoft's own connectivity/timing: expect very long-poll Ping behavior and large WindowSizes (ties back to #1).

### 9. EAS Provision policy completeness for Android device-policy enforcement
Provision is implemented (two-phase PolicyKey flow at eas.rs ~1400–1460, `provision_state` table). Needs conformance review vs MS-ASPROV that the returned `PolicyData` contains a coherent minimal policy set (DevicePasswordEnabled, AllowSimpleDevicePassword, etc. — or the documented all-off policy) and that the `X-MS-PolicyKey` enforcement path (449 challenge / status handling) is airtight, since Android clients take a hard dependency on keys matching. Also decide the RemoteWipe posture and document it (Android will prompt the user to grant device-admin; a wipe-capable gateway with no real wipe should advertise a no-op policy consistently).

### 10. EAS Sync wire-exactness pass on the two clients
Sync is thorough (JMAP `Email/changes` delta with full-sync fallback, watermark journal, per-collection keys). Delivered:

- **WindowSize / MoreAvailable / truncation** ([MS-ASCMD] §2.2.3.199, §2.2.3.116): per-collection WindowSize resolved against the global budget (`effective_sync_window`), initial sync paged via a persisted resume cursor, delta overflow persisted in `email_sync_pending` with a state token that never advances past undelivered ops — `<MoreAvailable/>` is emitted exactly when the window clips.
- **Sync status 3/4/8 retry semantics** ([MS-ASCMD] §2.2.3.185.6): invalid-key → Status 3 with a fresh SyncKey so the client re-primes from "0"; FolderSync Status 9 and GetItemEstimate Status 4 share the invalid-key recovery path.
- **AirSyncBase BodyPreference negotiation** ([MS-ASAIRS] §2.2.2.12): per-collection `<Options>` resolved per [MS-ASCMD] §2.2.3.125.6 (sticky across requests and gateway restarts via `get/set_sync_collection_options`), honoring the negotiated Type chain (2=HTML → 1=plain), `TruncationSize` + `AllOrNone`, and `<Truncated/>` emission; calendar AppData renders the Body prefix per the same negotiation (`render_calendar_app_data_with_options`).
- **v16.1 conversation mode** ([MS-ASCMD] §2.2.3.36.2): `<airsync:ConversationMode>` parsed per collection and honored by the email renderer (`render_jmap_email_as_eas_application_data_with_options`).
- **Body vs BodyPart (16.1)** ([MS-ASCON]): per-Fetch `<Options>` with BodyPartPreference parsed in document order ([MS-ASCMD] §2.2.3.125.4); BodyPartPreference Type≠2 → Status 164; ItemOperations email fetch honors request-or-sticky options with statuses 6/3/14/16 per [MS-ASCMD] §2.2.3.177.8.
- **WBXML namespace expansion** ([MS-ASWBXML] §2.1.2.1): the document's root code page expands unqualified (default namespace) while SWITCH_PAGE-reached tags keep their prefix — locked in by `sync_root_page_expands_unqualified_and_switched_pages_prefixed` and the ResolveRecipients round-trips.

The exact option matrices both clients send are covered: `parse_outlook_android_option_matrix` and `parse_new_outlook_windows_option_matrix` (src/eas_sync_options.rs tests) pin request parsing; negotiation, AllOrNone fall-through, UTF-8-safe truncation, preview, sticky re-resolution and MIME gating have dedicated unit tests; wire shapes are held by the protocol-fixture (22) and snapshot (11) suites.

### 11. WBXML conformance hardening
`src/wbxml.rs` is spec-cited (v20250520) with code-page tests, but this layer is where interops *die*: any missing 16.1 code page (AirSyncBase 17, Email2 et al.), wrong token, or an `ApplicationData` blob emitted as text instead of opaque (or vice-versa) breaks a client silently. Add: a full-table conformance test generated from the MS-ASWBXML token list (the .txt specs are in-repo — generate the phf table from them or at least a diff-test), plus round-trip fuzzing of every EAS response through encode→decode→schema-validate. Delivered:

- **Full-table diff test against the in-repo spec** ([MS-ASWBXML] §2.1.2.1): `wbxml_code_page_tables_match_ms_aswbxml_v20250520` parses all 25 tag-bearing code pages out of `exchange_protocols/[MS-ASWBXML].txt` at test time (footnote rows, page headers, wrapped version cells included) and asserts **both directions**: all 608 spec tokens are present with exactly the spec's names, and the tables hold nothing the spec doesn't. `NAME_TO_TAG` is asserted to be the exact 608-entry inverse of `TAG_TO_NAME`, so no tag resolves to a different token on the way out.
- **Per-token protocol-version matrix** ([MS-ASWBXML] "Protocol versions" column): `wbxml_token_version_matrix_pins_non_16_1_inventory` pins the exact 29-token inventory of entries the spec does not offer at 16.1 (2.5-era `*Body*`/`Att*`/`MIME*` tokens, the pre-12.0 `Attachments`/`GlobalObjId`/`Location` forms, etc.). They stay in the decode tables as documented legacy-compat inventory; the pinned list forces a conscious re-decision whenever the tables or the spec revision change.
- **Byte-array-typed elements are OPAQUE, not inline strings** ([MS-ASDTYPE] §2.7.1 → [WBXML1.2] opaque data): the codec's byte-array inventory is now the complete spec-declared set — `AirSyncBase:Content` ([MS-ASAIRS] §2.2.2.15), `GAL:Data` ([MS-ASCMD] §2.2.3.39.1/.3/.4, previously missing), `ComposeMail:Mime` ([MS-ASCMD] §2.2.3.109, previously missing), `ItemOperations:`/`Search:`/`Email2:ConversationId` ([MS-ASCON] §2.2.2.3.1–.3, [MS-ASEMAIL] §2.2.2.21) and `Email2:ConversationIndex` ([MS-ASCON] §2.2.2.4, [MS-ASEMAIL] §2.2.2.22) — plus a documented deliberate entry for `Email:GlobalObjId` (raw-binary ABNF, keeps encode/decode symmetric). `wbxml_conformance_byte_array_inventory_matches_specs` grounds every entry in the shipped spec text (heading + marker phrase) and asserts set equality against the live tables, so a spec revision or an accidental table change breaks the build. `byte_array_elements_encode_as_opaque_and_roundtrip` pins the exact wire spans (SWITCH_PAGE → tag|0x40 → `0xC3` OPAQUE → mb_u_int32 length → raw bytes) for each family and proves encode→decode→encode stability; `base64_bearing_string_elements_stay_inline_strings` guards the inverse — `ItemOperations:Data` and `Contacts:Picture` carry base64 but are typed "string" ([MS-ASDTYPE] §2.7) and must stay STR_I.
- **[MS-ASWBXML] §3's worked example is byte-exact in both directions**: `ms_aswbxml_section3_example_roundtrips_byte_exact` encodes Microsoft's own documented XML (namespace URIs *without* trailing colons, as the spec prints them) to the spec's exact 106 bytes and re-encodes the decode output to the identical bytes. This forced two real conformance fixes: the encoder now resolves namespace URIs with or without the trailing colon (the §3 example was previously unencodable), and the decode convention "root code page = default namespace" now also holds on re-encode — undeclared canonical prefixes (`AirSyncBase:…`) resolve to their own code page and unqualified descendants fall back to the root's page, so a decoded document re-encodes losslessly instead of hash-accidentally picking a wrong-page same-local-name token (`Body`→`Email:Body`, `LastName`→`GAL:LastName`).
- **Fail-closed decode per §2.1.3**: the algorithm does not use string tables, processing instructions, or attribute encoding — so the decoder now *errors* on every corresponding [WBXML1.2] global token (EXT_I_0/1/2, PI, LITERAL_C, EXT_T_0/1/2, LITERAL_A, EXT_0/1/2, LITERAL_AC) and on tag tokens carrying the attribute bit, instead of guessing. Two character-data tokens stay leniently accepted by design though Exchange never emits them: `ENTITY` (0x02), a legal [WBXML1.2] character reference, and `STR_T` (0x83), which can only index this profile's always-empty string table. Unknown (code page, token) pairs are rejected (the tables are now proven complete, so nothing legitimate can hit this) rather than silently emitted as `_unknown_*` placeholders that downstream handlers would treat as real data; structural corruption — END with no open element, data after *or before* the root, truncated/unclosed elements, header-only bodies — is rejected instead of silently repaired, and the truncation error names the *outermost* unclosed element. Covered by `decode_rejects_forbidden_wbxml_tokens`, `decode_rejects_tag_tokens_with_attribute_bit`, `decode_rejects_unknown_tags_and_unknown_code_pages`, `decode_rejects_structurally_invalid_documents`, `decode_rejects_data_before_root_element`, `decode_truncation_error_names_outermost_element`.
- **Fail-closed encode**: CDATA content is now encoded (previously silently dropped from the wire) with byte-array awareness; processing instructions and DOCTYPE are hard errors (WBXML cannot represent them) instead of being silently discarded; character data outside the document element is a hard error on both entry and exit. [MS-ASWBXML] §2.1.3 defines no attribute encoding, so an element's only representable attributes are namespace declarations: any other attribute, a malformed attribute, a duplicated attribute name — or a namespace (prefix or default) declared with a URI that maps to no code page and then *used* — is a hard error, never a silent drop; the previously duplicated attribute-scanning loops in the Start/Empty arms are one `encode_namespace_attributes` pass. Covered by `cdata_content_is_encoded_and_pi_doctype_are_rejected`, `encode_rejects_non_namespace_attributes`, `encode_rejects_declared_unknown_namespace_bindings`, `encode_rejects_character_data_outside_document_element`.
- **Per-element content aggregation**: quick-xml can split one element's character content across Text, CData, and GeneralRef events; encoding per event emitted one STR_I (or one OPAQUE per partial base64 segment) — corrupting byte-array values ([MS-ASDTYPE] §2.7.1). Content is now accumulated and flushed as exactly one token per element, with byte-array base64 stripped of interior whitespace (xsd:base64Binary semantics) before the raw-byte decode. Covered by `split_element_content_encodes_as_single_wbxml_token`.
- **Round-trip proof beyond the spec example**: every EAS response the suite renders already flows through encode→decode→assert in the eas.rs/sync.rs tests; with the re-encode symmetry fixes above, the decode output of any response re-encodes byte-identically (pinned per family in the OPAQUE test), which is the deterministic core of the requested fuzzing — the remaining grammar validation is the protocol-fixture and snapshot suites' job (22 + 11 wire-shape tests).

### 12. `ItemOperations`/`Fetch` attachment correctness
Big `attachment.rs` exists; verify against MS-ASCMD: `Range` byte-range fetch on base64 content, correct `Status` codes (13/14/15/16/17), `TotalSize`, part fetch, and that large attachments don't hit the 4MiB route body limit on requests (body limit is request-only — good — but verify response streaming is memory-bounded) and don't stall under Cloudflare (#1-related for slow pulls).

### 13. MeetingResponse / iMIP integrity
`meeting/` + `meeting/scheduling.rs` exists; verify: RSVP replies sent via authenticated SMTP with the mailbox user's identity, organizer notification semantics (don't self-notify), JMAP `CalendarEvent/set` participant `participationStatus` updates, recurrence-instance vs series scoping, and duplicate-RSVP idempotence. Any asymmetry here corrupts the organizer's calendar — the most visible failure mode.

### 14. Timezone fidelity both directions
`windows-timezones` + `timezone.rs` present; needs proof tests that every Windows TZ name the clients emit (EAS `Timezone` blob format, not just names) maps to an IANA tz with correct DST rules and round-trips Stalwart JSCalendar/CalDAV without offset drift. Berlin/Amsterdam relative-DST rules are a classic breakage point.

### 15. Tasks/Notes are gateway-local (SQLite), not backend-synced
FolderSync always advertises Tasks and Notes ("gateway-local"), but Stalwart webmail/other clients will never see them — data divergence that violates the "Stalwart is the only backend" premise. Either (a) map Tasks→CalDAV VTODO on Stalwart (JMAP Tasks isn't in v0.16), (b) hide the folders, or (c) clearly document divergence. The JMAP-first instruction argues for (a) or (b).

---

## P2 — Robustness / operations

### 16. Watermark pruning vs. long-offline devices
Hourly `prune_change_journals` deletes rows at/below the lowest live watermark. A phone offline longer than the pruning safety window (or a device holding a stale `seq:` watermark) must get a clean full-resync (Sync status 3) rather than a silently re-keyed gap. Verify the stale-watermark → forced-resync path explicitly with a test; the migration test (`init_schema_migrates_pre_journal_seq_databases`) shows awareness but runtime behavior needs the same proof.

### 17. Auth negative-caching and cache-invalidation
`AuthVerifier` caches *invalid* credential results for the configured TTL (30–300s in moka). A user changing a Stalwart password is locked out (or locked in) for up to 5 minutes; and `verify_caldav` fallback on any JMAP *error* (not just 401) amplifies a Stalwart hiccup into CalDAV hammering for both clients × many EAS requests. Tighten: distinguish 401 vs 5xx, short-cache negatives, jitter, and document the TTL knob.

### 18. Auth-cache + rate-limit sizing for two clients is fine, but per-request Basic-auth probe to Stalwart is the SPOF
Cold-cache bursts (container restart) fire JMAP session probes per request from both clients and Microsoft IPs. Add a startup warm path and circuit-breaker/backoff when Stalwart is down, returning EAS status 110/ServerError instead of 401s (a 401 makes Outlook *prompt for password* — catastrophic UX against a transient backend blip).

### 19. SQLite concurrency
Confirm `init_schema` sets WAL + `busy_timeout` and that sync-state writes from many concurrent EAS long-polls don't serialize into ping-latency spikes (Ping holds connections open; any write contention lands on the Ping path). Add a load test with N concurrent Pings + concurrent Syncs.

### 20. Health check must reflect backend, not just process
`/health` is excluded from rate limiting (good) but should report Stalwart JMAP reachability (there is startup verification; extend to liveness/readiness split: liveness=process, readiness=backend) so Docker tooling restarts a wedged gateway instead of leaving clients spinning.

### 21. Secrets hygiene
HMAC secret via env is fine; ensure `GATEWAY_ADMIN_USERNAME/PASSWORD` (GAL admin creds) support file-based (`_FILE`) injection matching Docker secrets, and that logs/profiling never emit them (`SafeDebug` exists — audit its coverage, especially `Debug` on request structs containing the EAS Authorization header / base64 Basic payload).

### 22. Disable unused surfaces; attack surface vs. "exhaustive" aspirations
MAPI/HTTP + NSPI + OAB + ECP + full EWS are **not used by the two target clients** (both are EAS clients, directly or through Microsoft). Per the frictionless goal: gate them behind flags (a MAPI flag exists), ship the reference profile with only Autodiscover + EAS + minimal EWS (OOF is genuinely client-visible) enabled, and make EWS/OAB/ECP opt-in. The "exhaustive protocol" ambition then remains as optional modules, not default exposure.

### 23. Documentation gaps for the exact target
One page that maps: mailbox domain → DNS records → cloudflared hostnames → Android add-account steps (including the device-admin consent screen from #9) → New Outlook "add account" flow (explaining the Microsoft-cloud sync path from #8) → troubleshooting table of `outlook_cloudflare_smoke.sh` failures. The smoke doc exists; a runbook does not.

---

## P3 — Polish / hardening

### 24. Dependency currency & supply chain
`Cargo.toml` uses permissive `^` ranges with heavy deps (axum-extra, moka, dashmap, prometheus…). Add `cargo-deny`/`cargo audit` in CI; `Cargo.lock` exists — good — but make the Docker build step enforce `--locked` too (currently only `cargo fetch --locked`).

### 25. Metrics/observability for EAS command-level latency
Per-command histograms, Ping wait distribution, JMAP backend latency — the Prometheus dep exists; verify labels don't include PII (username/device-id as labels = cardinality + privacy issue).

### 26. Log redaction verification
For tracing spans carrying request paths with query strings (EAS sometimes passes creds-adjacent data in query in legacy mode — header/WBXML is supported; confirm query-string creds mode is rejected outright).

### 27. Response compression interplay
Verify Cloudflare + compression don't interact badly with EAS and that `Content-Length`/chunked WBXML streams flush promptly (Ping interim responses must not be buffered by tower-http buffering layers).

---

## Evidence of what is already strong (so effort isn't wasted)

- Routing covers all needed surfaces; request body limits are per-route and MAPI-aware (`src/main.rs:752–830`).
- EAS command coverage includes all required ones (FolderSync, Provision, Sync, Ping, Settings, ItemOperations, Search, MeetingResponse, ResolveRecipients, ValidateCert, GetItemEstimate, MoveItems, SendMail/SmartReply/SmartForward) and OPTIONS advertises 12.0–16.1.
- JMAP-first design with explicit capability verification at startup and CalDAV fallback (`GATEWAY_CALENDAR_CAPABILITY_CHECK`).
- Spec-traceable WBXML implementation, protocol fixtures, snapshot tests for every key EAS/Autodiscover/EWS response, a Cloudflare smoke harness, and a JMAP calendar deploy test.
- Solid security defaults: non-root image, HSTS/CSP/nosniff headers, secrecy/zeroize on credentials, HMAC-secret enforcement, startup hostname validation.

**Highest-leverage first moves: #1 (Ping heartbeat vs Cloudflare), #2 (per-user rate limiter keying), #4/#5 (pin + publish images), then #8–#11 conformance passes against the two named clients.**
