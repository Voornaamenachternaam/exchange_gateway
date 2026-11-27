# Performance Audit — Checkpoint

Repository: `exchange_gateway`
Audited revision: `7b88218` on branch `eas-folder-management-and-mailbox-scoped-email-sync`
Toolchain actually executed: `rustc 1.99.1 (58cf8f9c3 2026-10-13)`, `cargo 1.99.1 (5f94df478 2026-08-27)`

This file preserves the audit checkpoint requested during the review. Later sections are appended; this checkpoint is not replaced. This is a review only: no production fix is implied or made.

## Scope and evidence standard

The review covers production Rust modules, algorithmic and allocation behavior, async/runtime boundaries, backend HTTP/WebDAV/JMAP I/O, SQLite access, caches/statics/lifetimes, and release/build configuration. Recommendations are ranked only after tracing their callers and relevant limits. SQLite claims use `EXPLAIN QUERY PLAN`, not the mere presence of an index.

Impact labels describe expected benefit under the stated workload, not a measured production speedup. Confidence labels distinguish code-proven costs from effects that still require production profiling. A temporary allocation spike is not called a leak; a bounded cache or one-time `LazyLock` is not called unbounded memory growth.

## Verification completed at checkpoint

- `cargo test` completed successfully. The complete run comprised **1,613 passing tests, 0 failures, 0 ignored**: 1,507 library tests, 36 binary tests, 3 concurrency-model tests, 10 cross-user authorization tests, 6 HTTP-fault tests, 5 JMAP-calendar deployment tests, 10 property tests, 15 protocol-fixture tests, 11 snapshot tests, 1 storage-fault test, 8 transport-fault tests, and 1 doc-test. The library target finished in 17.51 seconds; the slowest integration target finished in 30.03 seconds.
- The Clippy check completed without diagnostics in the captured run.
- A targeted benchmark command exited successfully, but its result table was not retained. No timing or throughput claim is therefore made from that invocation.
- No production source or dependency change was made for this audit.

## Established SQLite query-plan evidence

The following plans were obtained directly with `EXPLAIN QUERY PLAN` against the repository schema and representative predicates:

| Query label | Observed plan | Established conclusion |
| --- | --- | --- |
| `item_uid_lookup` | `SEARCH item_map USING INDEX idx_item_map_uid (owner=? AND uid=?)`; also `USE TEMP B-TREE FOR ORDER BY` | The owner/UID lookup avoids a full table scan, but its current `ORDER BY` is not satisfied by that index and requires a separate sort. The exact SQL and result-cardinality requirement must be revisited before choosing a composite index or query rewrite. |
| `item_server_lookup` | Uses `sqlite_autoindex_item_map_1 (owner=? AND server_id=?)` | The owner/server-id lookup uses the table's unique index and avoids a full scan. No additional index is justified by this query alone. |
| `journal_delta` | Uses `idx_change_journal_owner` with `owner` and `id` bounds | The delta query is index-ranged by owner and journal id. |
| `journal_prune` | Uses `idx_change_journal_owner` with `owner` and `id` bounds | The pruning predicate is index-ranged; it does not require a whole-journal scan. |

These results deliberately do **not** imply that adding an index to every table would improve throughput. Composite-index order, sort direction, `LIMIT`, covering behavior, write amplification, and actual caller frequency remain part of the validation.

## Preliminary supported optimization directions

These directions are retained for final validation and ranking; they are not yet presented as measured wins.

1. **Bound request work independently of response windows where full snapshots are currently required.** `src/contacts.rs:1337-1358` lists contact cards and then awaits each photo download serially. `src/contacts.rs:1142-1203` subsequently builds owner-wide href/server-id indexes and clones changed/added contacts before applying the delivery window at `src/contacts.rs:1198-1203`. This is temporary, request-limited memory and serial backend latency, not evidence of a permanent leak. Candidate changes: defer photo fetching to contacts actually rendered where protocol semantics permit, use bounded concurrency for independent downloads, and stream or index the diff without cloning whole vCards for undelivered entries. Preserve fail-closed backend selection and stable contact identity.
2. **Avoid cloning JSON container subtrees solely to deserialize them.** Production examples include `src/jmap.rs:1254`, `:2525`, `:2885`, `:3083`, `:3087`, `:3091`, `:3230`, `:3376`, `:3380`, `:3384`, `:5673`, `:5816`, and `:6814`. The calendar event path at `src/jmap.rs:1149` also clones `raw` for a shadow decode. Candidate change: move owned `Value`s out of response envelopes or deserialize through references/map removal where the surrounding response is no longer needed. This is allocation overhead, not retained growth.
3. **Size large protocol output buffers from known content.** There are already good examples such as `src/eas.rs:3913` and `src/sync.rs:2794`. Other large response builders start with `String::new()`, including `src/contacts.rs:1203`. Candidate change: use conservative capacity estimates only where profiling shows repeated growth; do not replace every empty string indiscriminately.

## Memory-growth classification at checkpoint

- **High-confidence permanent production memory growth:** none established at this checkpoint.
- **Bounded caches/statics:** `src/email.rs:866-868` and `src/timezone.rs:267` use `LazyLock`; these initialize reusable data and are not, by themselves, leaks or unbounded caches.
- **Candidate requiring call-site proof:** `src/jmap.rs:11982-11983` contains `Box::leak` for `&'static [String]` and `&'static SecretString`. Its test-only/one-shot versus repeatable-production status must be established before classifying it.
- **Temporary/request-scoped overhead:** the full contact snapshots and cloned JSON values above can increase peak RSS and allocator work while a request is active, but their allocations are dropped afterward.

## Candidates explicitly not yet findings

- Several JMAP paths call `resp.text().await.unwrap_or_default()` (including `src/jmap.rs:1750`, `:1799`, `:1919`, `:2130`, and `:2137`). Response-size limits, content-length checks, timeouts, and the sensitivity of each caller must be inspected before claiming unbounded downloads or recommending a single shared cap.
- Candidate linear scans in `src/eas.rs` and repeated XML scans in `src/util.rs` require caller and input-size evidence. A linear scan over a small protocol structure is not automatically a material performance issue.
- `spawn_blocking` around `src/mapi/nspi.rs:730` and `:765` correctly identifies blocking directory work as a runtime concern; queueing, concurrency bounds, and work size still determine whether a change is warranted.
- `src/storage.rs:870-892` creates task/note href and UID indexes after compatible column migration. Index existence alone says nothing about whether the corresponding SQL uses them.

## Final-audit work remaining

Resolve exact source references and call frequencies, complete SQL plan coverage for material storage queries, separate proven hot-path costs from optional micro-optimizations, and append the ranked recommendations, Rust 1.99.1-specific opportunities, rejected candidates, validation results, and a complete module-by-module coverage record.

---

# Final audit — resolved references, complete plan coverage, ranked recommendations

Appended to the checkpoint above; the checkpoint content is unchanged. Same review-only terms: nothing in this section was implemented.

## Toolchain and validation actually executed for this section

| Step | Command | Result |
| --- | --- | --- |
| Toolchain | `rustc 1.99.1 (58cf8f9c3 2026-10-13)`, `cargo 1.99.1 (5f94df478 2026-08-27)` | Confirmed from the binaries used by the builds below. |
| Library suite | `cargo test --lib --quiet` | **1507 passed; 0 failed; 0 ignored**, finished in 17.52 s. |
| Query plans | `EXPLAIN QUERY PLAN` against `sqlite_schema.sql` (in-memory SQLite) | Plans quoted in the table below are the literal output. |
| Benchmarks | `cargo bench --bench hot_paths -- --test` (smoke) and `cargo bench --bench hot_paths -- --warm-up-time 0.5 --measurement-time 2 --noplot` (measured) | Timings in "Benchmark numbers actually measured"; single-machine numbers, not a production throughput claim. |

The full workspace suite was green at the checkpoint (1613 tests). This section re-ran the library target to confirm the tree is still green at `b9c8a6e`, and adds the query plans and code-path evidence the checkpoint listed as outstanding.

## Complete SQLite query-plan coverage (all measured)

Every plan below was obtained with `EXPLAIN QUERY PLAN` against `sqlite_schema.sql`, using the SQL copied verbatim from `src/storage.rs`. Nothing in this table is inferred from the presence of an index.

| Query (caller) | Plan (verbatim) | Conclusion |
| --- | --- | --- |
| `get_ews_item_by_uid` (`:3362`) | `SEARCH item_map USING INDEX idx_item_map_uid (owner=? AND uid=?)` + `USE TEMP B-TREE FOR ORDER BY` | Index-ranged. The sort runs over only the rows sharing that UID (normally one or two) because `LIMIT 1` applies after it, so it is immaterial. This refines the checkpoint's note: the `TEMP B-TREE` is not over the owner's whole row set. |
| `item_map` by `(owner, server_id)` — outer of the delta (`:1778`) | `SEARCH item_map USING INDEX sqlite_autoindex_item_map_1 (owner=? AND server_id=?)` | Optimal. |
| `get_item_owner` (`:3383`) | `SCAN item_map USING COVERING INDEX sqlite_autoindex_item_map_1` | **Full index scan per call** — no `owner` predicate and no `server_id`-only index. Finding P2-1. |
| `list_item_map_identities` (`:2268`) | `SEARCH item_map USING INDEX idx_item_map_instance (owner=?)` + `USE TEMP B-TREE FOR ORDER BY` | Owner-ranged, then the whole owner row set is sorted for an `ORDER BY id` the caller discards. Finding P2-2. |
| `list_jmap_id_aliases` (`:2283`) | `SEARCH jmap_id_alias USING INDEX sqlite_autoindex_jmap_id_alias_1 (owner=?)` + `USE TEMP B-TREE FOR ORDER BY` | Same redundant sort. Finding P2-2. |
| `journal_delta` upsert subquery (`:1778-1785`) | outer `SEARCH` on the unique index; `LIST SUBQUERY` → `SEARCH change_journal USING INDEX idx_change_journal_op (owner=? AND op=? AND id>?)` | Optimal — the `op`-leading index is the right one here and beats the owner index. |
| `journal_delta` delete (`:1804`) | `SEARCH change_journal USING INDEX idx_change_journal_op (owner=? AND op=? AND id>?)` | Optimal. |
| `list_journal_after` (`:1822`) | `SEARCH change_journal USING INDEX idx_change_journal_owner (owner=? AND id>?)` | Optimal; this is the query the checkpoint described (it has no `op` predicate, so the owner index is correct). |
| `prune_change_journal` (`:2119`) | `SEARCH change_journal USING INDEX idx_change_journal_owner (owner=? AND id<?)` | Optimal. |
| `get_latest_change_seq` (`:1764`) | `SEARCH change_journal` | SQLite's rowid `MAX` optimization: O(1), not a scan. |
| EWS keyset prime (`:1850`) | `SEARCH item_map USING INDEX sqlite_autoindex_item_map_1 (owner=? AND server_id>?)` | Optimal. |
| `folder_map` by owner | `SEARCH folder_map USING INDEX idx_folder_map_owner (owner=?)` | Optimal. |
| `deleted_item_tombstone` by owner | `SEARCH deleted_item_tombstone USING INDEX idx_deleted_owner_time (owner=?)` | Optimal. |
| `task_map` all for owner | `SEARCH task_map USING INDEX sqlite_autoindex_task_map_1 (owner=?)` | Optimal; `ORDER BY server_id` is free because the unique index is `(owner, server_id)`. |
| `get_email_keyword` (`:2768`) | `SEARCH email_keyword_baseline USING INDEX sqlite_autoindex_email_keyword_baseline_1 (owner=? AND collection_id=? AND email_id=?)` | Optimal plan; the cost is the call count, not the plan. Finding P1-2. |

## Ranked recommendations

> **Protocol-compliance revisions.** Items marked ↻ were rewritten after the `exchange_protocols/` review; **P2-2 is withdrawn**. Use the rewritten form in “Protocol-compliance audit of the recommendations” at the end of this document — the text in this section records only what was originally proposed.

Impact is the expected effect under the stated client workload (New Outlook for Windows, Outlook Android) with a Stalwart backend. "Proven" means the cost follows from the code and API semantics as written; "needs profiling" means the cost is real but its share of wall clock is not established.

### P0 — per-request backend client construction (connection pool and cache destruction)

**P0-1. One `JmapClient` per MAPI `Execute`.**
`src/mapi/handler.rs:718-725` builds `JmapClient::new(&state.cfg.jmap_base)` on every `Execute` RPC. `JmapClient::with_session_auth` (`src/jmap.rs:1609-1633`) constructs a fresh `reqwest::Client` (its own connection pool and TLS session cache), a fresh `session_cache: DashMap` (`:1623`) and a fresh Moka `session_auth` cache (`:1624-1630`). Consequences per `Execute`:
1. no connection reuse — a new TCP+TLS handshake to Stalwart;
2. `get_session` (`src/jmap.rs:2172-2196`) always misses its per-instance cache, so every `Execute` issues an extra `GET {base}/session` discovery request;
3. the credential-probe cache (AUDIT.md §18) never warms on this path.

The in-code comment ("the session cache inside it caches the JMAP session per-username for 5 minutes, so the per-call cost is just the cache lookup") is only true within one client instance, which here lives for exactly one request. `Execute` is the RPC that carries every MAPI operation (property reads, table reads, writes), so on that surface it is the hottest path, and this multiplies the cheapest, most frequent operation in the product.

Change: put `Arc<JmapClient>` on `MapiState`, wired from the same client `AppState` already builds (`AppState::new` builds it before the verifier for exactly this sharing reason), and use `&state.jmap` in `handle_execute`. `reqwest::Client` is documented to be cheap to clone and intended for reuse. Effect: removes one TLS handshake plus one HTTP round trip per `Execute`. Risk: low — identical code path, fewer moving parts. Confidence: proven.

**P0-2. One `CaldavClient` per request, at 21 call sites.**
`CaldavClient::new` (`src/caldav.rs:116-122`) builds a new `reqwest::Client` each call. Production call sites: `src/sync.rs:325` and `:2345` (inside `perform_sync` — i.e. every calendar Sync), `src/tasks.rs:1283, 2153`, `src/eas.rs:4210, 5211`, `src/eas_folders.rs:555, 1127, 1400, 2033`, `src/ews.rs:1759, 2250, 4656, 4719, 6574, 8682, 10255, 13363, 13705, 13941, 13985`, `src/main.rs:1406`. A calendar Sync that ends up served entirely from JMAP still pays for a CalDAV client construction.

Change: build one `Arc<CaldavClient>` in `AppState` (mirroring `state.jmap`), keep `new_from_base` for tests and for the per-user-home cases. Effect: removes a handshake per calendar/task/notes sync and per EWS calendar operation. Risk: low. Confidence: proven.

### P1 — N+1 backend round trips

**P1-1. EWS `SyncFolderItems` fetches each changed message individually. ↻**
`src/ews.rs:5663`, `:5705`, `:5770`, `:5836` each call `jmap.get_email(...)` inside the per-id loops over `changes.created` / `changes.destroyed`. A 100-message delta therefore costs 100 serial HTTP requests. A batch accessor already exists (`get_emails`, used at `src/ews.rs:11589`, `src/eas.rs:3823`, `:8891`).

Change: collect the ids for each phase, call `get_emails` in chunks bounded by Stalwart's `maxObjectsInGet` (500, back-references exempt), key the result by `email.id`, then render in the ORIGINAL id order (C1). Keep the existing fail-closed behavior: a batch failure must still fail the whole call (`ErrorServerBusy`), and a missing id must still be treated as `undelivered_ids`. Effect: O(1) round trips instead of O(n). Risk: medium — the per-id "message left the folder" (`email_lives_in_mailbox`) and seed/`rendered_ids` bookkeeping must be preserved exactly; this is the kind of change that needs the existing delta tests plus a new "batch partially absent" test. Confidence: proven cost, straightforward fix.

**P1-2. The EAS email delta reads the keyword baseline one row per message. ↻**
`src/eas.rs:8687` calls `storage.get_email_keyword(username, state_collection_id, &id)` inside the per-entry loop of the bounded flag scan (`KEYWORD_SCAN_MAX_PAGES = 8`, `:8274`). Each page is one `Email/query` plus one SQLite round trip per message.

Change: one `SELECT email_id, keywords_json FROM email_keyword_baseline WHERE owner=?1 AND collection_id=?2 AND email_id IN (...)` per page (`sqlx::QueryBuilder::push_tuples`). Preserve the current fail-closed rule: a page-level read error must still hold the scan position (`outcome.deferred = true`) rather than be treated as "no row". Effect: page-size × fewer SQLite round trips per delta. Risk: low. Confidence: proven.

**P1-3. `CopyFolder` re-lists every mailbox once per folder copied. ↻**
`src/ews_folder_ops.rs:1086` calls `ctx.jmap.query_mailboxes(...)` at every recursion level of `copy_folder_recursive`; `src/ews_folder_ops.rs:159` calls `get_mailbox_ids_for_role` (one `Mailbox/query`) per distinguished source folder. Copying a subtree of N folders costs N full mailbox listings.

Change: fetch the mailbox list once in the handler, pass `&[JmapMailbox]` through `FolderCopyCtx`, and resolve distinguished ids from that list. Effect: N listings → 1. Risk: low. Confidence: proven.

**P1-4. Contacts photo downloads are serial. ↻**
Retained from the checkpoint (`src/contacts.rs:1337-1358`): one awaited download per contact photo. Change: bounded concurrency with order preservation only — never defer (the PHOTO property is hashed into the contact `ChangeKey`; see C4). Effect: latency divided by K. Risk: medium (ordering/identity must not change). Confidence: proven cost.

### P1 — repeated parsing of the same request document

**P1-5. `extract_first_tag_text` re-parses the whole document on every call. ↻**
`src/eas.rs:743` and `src/ews.rs:594` build a `quick_xml::Reader` from the start of the document and scan until the tag closes. Call counts: **68 sites in `src/eas.rs`, 55 in `src/ews.rs`**. For a `SendMail` carrying `MIMEData` (admitted up to 40 MiB) each extraction is a separate prefix scan; the total work is O(k·n) in document size.

Change: parse once per handler into a `HashMap<&str, Vec<String>>` (borrowed local names, borrowed text where possible) or a typed request struct, and read fields from it. Cheapest interim step: have the helpers share one `Reader` via a cursor and require callers to request tags in document order. Effect: large on the SendMail/SmartForward paths, moderate on Sync. Risk: medium — entity/whitespace semantics are pinned by `extract_first_tag_text_keeps_spaces_around_entities`; a rewrite must keep the "text not trimmed per event, only the finished value" rule. Confidence: proven cost.

**P1-6. `extract_all_tag_blocks` is an O(n·m) byte-window scan with per-call allocations. ↻**
`src/eas.rs:709-735` builds `open`/`close` `Vec<u8>` per call and searches with `bytes[pos..].windows(len).position(...)`. It is also not nesting-aware.

Change: use `memchr::memmem::find_iter` (the `memchr` crate is already a declared dependency and currently unused, see P3-5). Keep the same matching semantics and replace only the search with `memchr::memmem` — do NOT reuse `extract_all_tag_blocks_checked` (`src/ews.rs:651`), which returns the full block rather than the inner text (C6). Effect: removes a quadratic term on large bodies. Risk: low. Confidence: proven.

**P1-7. `let payload = body.to_vec();` copies every EAS request body.**
`src/eas.rs:10485`, in `handle_eas_request` before dispatch — so the copy happens on **every** EAS request, not just the large ones. `body` is `Bytes` (already a cheap refcounted handle) and `Wbxml::decode` takes `&[u8]` (`src/wbxml.rs:1424`). The copy is up to `MAX_BODY_BYTES` = 40 MiB (`src/main.rs:38`).

Change: pass `&body` (or `body.as_ref()`), keeping the owned `Bytes` alive for the request. Effect: removes a full-body memcpy from every EAS request. Risk: none. Confidence: proven.

### P1 — allocation-heavy response construction

**P1-8. 280 `push_str(&format!(...))` sites allocate a temporary `String` per append.**
Distribution: `src/ews.rs` 82, `src/sync.rs` 64, `src/eas.rs` 36, `src/meeting/message.rs` 23, `src/tasks.rs` 21, `src/email.rs` 13, `src/contacts.rs` 13, `src/timezone.rs` 9, `src/oof.rs` 5, `src/wbxml.rs` 4, plus a few others.

Change: `use std::fmt::Write;` and `write!(out, "...", ..)` / `writeln!`. In render loops (a 500-item Sync response, ~10 fields per item) this removes thousands of short-lived allocations per response, and it is mechanical. Note `write!` returns `Result`; use `let _ = write!(...)` on `String` (which cannot fail) or `expect`. Effect: measurable allocator pressure reduction on every Sync/Find/GetItem response. Risk: low. Confidence: proven cost, mechanical fix.

**P1-9. The WBXML encoder allocates a `HashMap`, a `Vec<String>` and `String`s per element. ↻**
`src/wbxml.rs:1650-1652` calls `encode_namespace_attributes` for every `Event::Start` and pushes the result onto `prefix_ns_stack`. `encode_namespace_attributes` (`:1860-1895`) starts with `HashMap::new()`, `Vec::new()`, and `attr.key.as_ref().to_string()` per attribute. In EAS documents only the root element declares namespaces, so virtually all of these allocations are empty maps and empty vecs — one pair per element, i.e. thousands per Sync response.

Change: (a) return a `PrefixBindings` that is `Option<Box<[(Box<str>, Option<u8>)]>>` (or `SmallVec<[_; 2]>`) and represent "no attributes" as `None` without allocating; (b) take a fast path when `e.attributes().len() == 0`; (c) keep attribute keys as `&str` borrowed from the event buffer instead of `to_string()`. Effect: removes the dominant per-element allocation in the encoder. Risk: low-medium — namespace resolution semantics (declared-unknown = hard error, canonical prefix fallback) are pinned by `encode_rejects_declared_unknown_namespace_bindings` and the §3 byte-exact test. Confidence: proven cost.

**P1-10. The WBXML decoder stores an owned `String` per open element and formats one per close.**
`src/wbxml.rs:1452` `xml_stack: Vec<String>` and `:1480` `output.push_str(&format!("</{tag}>"))` (also `:1501` for entity references). `TAG_TO_NAME` values are `&'static str` (`src/wbxml.rs:14`), so the stack can hold `&'static str`.

Change: `Vec<&'static str>` for the stack (with an owned fallback only if a future token source needs it), `output.push_str("</"); output.push_str(tag); output.push('>')` instead of `format!`, and `String::with_capacity(bytes.len() * 2)` for `output` (WBXML is usually expanded ~2–4×). Also `:1429` `String::from_utf8(bytes.to_vec())` can be `std::str::from_utf8(bytes)?.to_owned()`. Effect: two allocations per element removed on the hottest codec path. Risk: low. Confidence: proven.

### P2 — SQLite

**P2-1. `get_item_owner` is a full index scan.**
`src/storage.rs:3383-3390`: `SELECT owner FROM item_map WHERE server_id = ?1`. Measured plan: `SCAN item_map USING COVERING INDEX sqlite_autoindex_item_map_1`. Called from `src/ews.rs:4480` (GetItem/calendar item), `src/eas.rs:4131` (attachment fetch), `src/eas.rs:4461` (calendar item fetch) — once per item read, O(total items) each.

Change: `CREATE INDEX idx_item_map_server ON item_map(server_id)` (the unique index is `(owner, server_id)`, so `server_id` alone cannot be seeked). Write cost is one extra index per `item_map` insert. Effect: O(log n) instead of O(n) per item read. Risk: low. Confidence: proven (plan measured).

**P2-2. Two owner-scoped identity queries sort their whole result set for an ordering the caller discards. ↻**
`src/storage.rs:2268` and `:2283` both end in `ORDER BY id ASC`; measured plans show `USE TEMP B-TREE FOR ORDER BY`. Both callers (`src/sync.rs:130-157`) insert into `HashMap`s where order is irrelevant.

Change: **withdrawn** — the clause is load-bearing for ServerId stability (C9). If the sort ever measures, add a plain `item_map(owner)` / `jmap_id_alias(owner)` index (which supplies rowid order) and keep the clause.

**P2-3. `CalendarIdentity::load` materializes the whole owner item map per calendar sync. ↻**
`src/sync.rs:1892` calls `CalendarIdentity::load` once per `perform_sync`; the loader (`src/sync.rs:121-160`) runs two full owner scans and builds three maps with `String` clones per row (`server_id.clone()`, `resource_identity(...).into_owned()`). For an owner with N events and C calendars, one device sync of all C calendars costs C·O(N) rows and allocations.

Change, in increasing ambition: (a) `resource_identity` already returns a `Cow` — avoid `into_owned()` when the href is already path-only by keying on a `Box<str>` built once; (b) cache the loaded identity for the duration of one sync request when several collections are synced together; (c) add a collection discriminator to `item_map` so the map can be scoped (this is the structural fix, and it also removes the cross-collection `item_map` sharing that AUDIT.md §10 documents as the reason scoped deletes cannot tombstone). Effect: proportional to N. Risk: high for (c) — it interacts with the documented shared-row semantics; (a) and (b) are low risk. Confidence: proven cost; benefit needs N in production.

**P2-4. Positive finding — do not change.** The journal delta uses `idx_change_journal_op`, the keyword baseline uses its unique `(owner, collection_id, email_id)` index, `folder_map` and `deleted_item_tombstone` use their owner indexes, and the sync-completion writes are already batched into one transaction (`Storage::commit_calendar_sync`). No further index is justified by the measured plans.

### P2 — per-request middleware overhead

**P2-5.** `src/metrics.rs:528-529` allocates `req.uri().path().to_string()` and then `normalize_path` (which builds a `Vec<String>` of segments plus a joined `String`) for every request; `record_request` (`:156-168`) performs three `with_label_values` lookups and one `status.as_u16().to_string()`. `src/rate_limit.rs:101` allocates the path string only to compare it against a static list (`is_unmetered_path`), and `src/validation.rs:44` percent-decodes every path.

Change: `is_unmetered_path(req.uri().path())` on the borrowed `&str`; make `normalize_path` write into a reusable `String` (or map the known route set through a `phf`/`OnceLock<HashMap<&'static str, &'static str>>`, since the router's route table is static); skip percent-decoding when the path contains no `%`. Effect: removes 3–5 short allocations per request from the fixed overhead. Risk: low — keep `normalize_path`'s id-collapsing behavior identical. Confidence: proven cost, small absolute size.

**P2-6. `TZ_PARAMS_CACHE` is an unbounded process-wide cache keyed on a client-supplied string. ↻**
`src/timezone.rs:987-1004`: `static TZ_PARAMS_CACHE: LazyLock<Mutex<HashMap<String, Option<TzParams>>>>` is keyed by the **raw input string** and has no capacity limit and no eviction (the only accesses are `get` at `:991` and `insert` at `:1000`). The doc comment claims it is "bounded by the finite set of IANA ids a tenant's calendar events reference", but the key is whatever the caller passes, and the production caller is the item render path `src/ews.rs:1485` passing `item.timezone` — the stored TZID of a calendar event, which an authenticated client supplies when it creates the event. A client that creates events with distinct bogus `TZID`s (each resolving to `None`, which is also cached) grows this map without bound for the whole process, across all users. Secondary cost: a single global `Mutex` taken twice per lookup, on a per-event render path, serializing timezone conversion across all worker threads.

Change: bound the cache (Moka with capacity) and stop memoizing `None`; keep the raw key (C11). Replacing the `Mutex<HashMap>` with `moka::sync::Cache` also removes the double lock acquisition. Effect: bounded memory under hostile input, less lock contention on calendar render. Risk: low — the memo is a pure function of its key. Confidence: proven by inspection.

### P3 — micro-optimizations and build hygiene

**P3-1. `sort_by_key(|(k, _)| k.to_string())` in `src/jscard.rs:367, 387, 428, 640`. ↻** `slice::sort_by_key` recomputes the key on every comparison, so this allocates O(n log n) `String`s. Use a STABLE `entries.sort_by(|a, b| a.0.cmp(b.0))` — see C12; the emitted property order is hashed into the contact `ChangeKey` and must not change.

**P3-2. `normalize_email` (`src/util.rs:120-134`)** lowercases twice (`trimmed.to_lowercase()` and `stripped.nfc().collect::<String>().to_lowercase()`), allocating three `String`s. Add an ASCII fast path (`if trimmed.is_ascii() { return trimmed.to_ascii_lowercase() }`) and return `Cow<'_, str>`. Called on every attendee/recipient comparison in calendar and recipient resolution.

**P3-3. `JmapClient::mail_properties()` (`src/jmap.rs:1561-1590`)** returns a freshly built `Vec<&'static str>` on each of its three call sites (`:2383`, `:2840`, `:2913`), which is then serialized to a `serde_json::Value`. Make it `const MAIL_PROPERTIES: &[&str]` and serialize a `&[&str]`.

**P3-4. `JmapClient::get_session` (`src/jmap.rs:2172-2182`)** returns `Ok(entry.value().1.clone())` — a full `JmapSession` clone (including `accounts`/`primary_accounts` maps) on every JMAP method call. Cache and return `Arc<JmapSession>`.

**P3-5. Seventeen declared-but-unused direct dependencies.** Verified by scanning every `src/**/*.rs`, `benches/*.rs` and `tests/*.rs` for each name: `axum-extra`, `base64ct`, `tower`, `futures-core`, `compact_str`, `flate2`, `pin-project-lite`, `http-body-util`, `arc-swap`, `memchr`, `itoa`, `ryu`, `smartstring`, `bstr`, `rustls-pki-types`, `lzxd`, `compressed-rtf`. `compressed-rtf` appears only in comments describing unimplemented RTF compression (`src/mapi/store.rs:553-601`) and `lzxd` has no mention anywhere in `src/`, `benches/` or `tests/`. `memchr` should not be dropped — P1-6 uses it. Effect: fewer direct-dependency edges to resolve, compile and keep version-pinned (crates still pulled in transitively, such as `compact_str` via another dependency, remain in the graph); no runtime change.

**P3-6. `harvest_and_store` runs on every outbound send. ↻** `src/email.rs:3253` calls `crate::smime::harvest_and_store`, which parses the whole MIME with `mail-parser` and every certificate with `cms`/`x509-cert` (`src/smime.rs:271-305`) for each `SendMail`/`SmartReply`/`SmartForward`, including large attachments. Gate on a cheap, CR/LF-unfolded, case-insensitive scan for `pkcs7` / `multipart/signed` before the full parse — a superset check, never a strict one (C13).

**P3-7. Non-UTF-8-breaking but tooling-hostile control byte.** `src/recorded_client_contract.rs:852` contains a literal `NUL` inside a `//` comment (after `cn=user`). Rust accepts it (U+0000 is valid UTF-8), but it makes the file "binary" to `grep`/`rg` and can confuse editors and diff tooling. Replace with a normal character.

## Rust 1.99.1-specific opportunities

These are the edition-2024 / 1.99.1 features that apply directly to the findings above.

| Feature | Where it applies | Note |
| --- | --- | --- |
| `std::fmt::Write` (`write!`/`writeln!` into `String`) | P1-8 (280 sites), P1-9/10 | Removes the temporary `String` per append. `String`'s `Write` impl is infallible in practice; `let _ =` or `.expect()` is idiomatic. |
| `slice::sort_by` (stable) | P3-1 / C12 | `sort_by_key` recomputes and allocates a key per comparison; `sort_by` compares borrowed fields and keeps the byte-identical property order the contact `ChangeKey` depends on. |
| `Vec::extract_if` (1.87) | any "partition then collect" in the delta paths | In-place move-out filtering without a second allocation. |
| `Cow<'_, str>` return types | P3-2, `xml_escape` family (`src/util.rs:38-56`) | Borrow when nothing needs escaping (already done for XML; extend to `normalize_email`). |
| `LazyLock` / `const` items | P3-3, `src/timezone.rs:267` | Static tables instead of per-call `vec!`/`format!`. |
| `str::from_utf8` + `to_owned` instead of `String::from_utf8(bytes.to_vec())` | `src/wbxml.rs:1429` | One copy instead of two. |
| `#[expect(lint)]` (1.81) instead of `#[allow(lint)]` | the `#[allow(clippy::too_many_arguments)]` sites in `email.rs`/`jmap.rs`/`eas.rs` | Unfulfilled `expect` is a warning, so the escape hatch cannot silently outlive its need. |
| `impl Future` in traits (RPITIT) | `src/traits.rs` already uses it; `#[async_trait]` remains only in `src/directory.rs`, `src/ews.rs`, `src/notifications.rs` | Those traits are used as `dyn`, which RPITIT cannot serve, so no change is recommended there. |
| `Option::is_none_or` / `is_some_and` | already used (`src/jmap.rs:3848` etc., `src/eas.rs`, `src/caldav.rs`, `src/calendar.rs`) | No action; noted so reviewers do not "downgrade" them. |
| `[profile.release] panic = "abort"` | `Cargo.toml` | Would shrink code and drop unwind tables, but any `catch_unwind` (test harnesses, `spawn_blocking` join semantics) must be checked first. Not recommended without that check. |

Rust 1.99.1 gives no new allocator, async-runtime, or `sqlx`/`reqwest` behavior that changes the findings; the wins above are all source-level.

## Benchmark numbers actually measured

`cargo bench --bench hot_paths -- --warm-up-time 0.5 --measurement-time 2 --noplot`, release profile (`opt-level = 3`, `lto = "thin"`, `codegen-units = 1`), criterion 0.8.2, 100 samples per benchmark, on the container this audit ran in. These are real measurements, not estimates; they are still not a production throughput claim (one machine, short measurement window, 14% outliers on `encode_64_collections`).

| Benchmark | Time (median) | Throughput |
| --- | --- | --- |
| `wbxml_codec/encode_64_collections` | 46.40 µs [45.67, 47.28] | 45.79 MiB/s |
| `wbxml_codec/decode_64_collections` | 49.46 µs [48.79, 50.24] | 42.96 MiB/s |
| `wbxml_codec/round_trip_64_collections` | 93.79 µs [93.26, 94.35] | 22.66 MiB/s |
| `jscalendar_event_to_ics` | 12.41 µs [12.26, 12.65] | — |
| `sync_delta_application_data/render_16_emails` | 89.89 µs [88.69, 91.18] | 178.0 Kelem/s |
| `sync_delta_application_data/render_64_emails` | 384.12 µs [380.27, 388.87] | 166.6 Kelem/s |
| `sync_delta_application_data/render_256_emails` | 1.4905 ms [1.4717, 1.5132] | 171.8 Kelem/s |
| `auth_cold_concurrent_probe_burst_32` | 49.69 ms [46.04, 53.36] | — |

Reading these numbers against the findings:

- **`wbxml_codec` (P1-9, P1-10).** The benchmark document is 64 `<Collection>` blocks, roughly 2 KiB of XML and ~260 elements, so ~46 µs is on the order of 150–180 ns per element. A WBXML encoder that emits a code-page byte, a tag token and (for text) a length-prefixed string has no business costing that much per element; the per-element `HashMap::new()` + `Vec::new()` in `encode_namespace_attributes` (`src/wbxml.rs:1863-1865`) and the per-close `format!` (`:1480`) are the obvious candidates. This is a hypothesis that a profiler should confirm before any claim of a specific win, but the absolute cost makes the encoder worth attention, and the fix is contained.
- **`sync_delta_application_data` (P1-8).** 256 emails render in 1.49 ms, i.e. ~5.8 µs per email of pure string building; the path is dominated by `push_str(&format!(...))` (64 sites in `src/sync.rs`). Linear scaling across 16/64/256 confirms no super-linear term, so this is constant-factor allocator work — exactly what `write!` removes.
- **`auth_cold_concurrent_probe_burst_32`.** ~50 ms for a 32-way cold burst against a stub backend; this is dominated by the deliberate serialization the §18 funnel performs (one probe per credential), so it is a design cost, not a defect.

## Rejected candidates

- **Replacing every `String::new()` with `String::with_capacity`.** The checkpoint's caution stands: growth-doubling is already amortized, and wrong capacity estimates waste memory. Only P1-10 (WBXML `output`) has a known expansion ratio worth hard-coding.
- **A blanket `sort_unstable` migration.** Most sorts here are over user-visible ordered lists (folders, reminders) where stability is part of the wire contract. P3-1 is the only candidate, and even there the STABLE form is required (C12).
- **Disabling `debug-assertions` in the test profile.** `calcard`'s exporter asserts in debug builds, and the JSCalendar participant-preservation test relies on that class of check; disabling them would silently change test semantics for a small compile-time win.
- **Fat LTO / `codegen-units = 1` changes.** `[profile.release]` already uses `lto = "thin"` with `codegen-units = 1`. Full LTO would trade a large build-time increase for a runtime gain not evidenced by any measurement here.
- **Pooling or caching `JmapClient` per user inside the client itself.** Unnecessary: the shared-client fix (P0-1/P0-2) achieves the same reuse with the caches that already exist.
- **Indexing every `item_map` column.** Only `server_id` (P2-1) has a measured full scan; the other plans are already index-ranged, and extra indexes cost write amplification on the sync-completion transaction.
- **`spawn_blocking` for `derive_display_name` / small string work.** Already correctly handled in `src/main.rs` (documented inline); the directory path is the only blocking call and is already offloaded.
- **`Box::leak` at `src/jmap.rs:11982-11983`.** Confirmed test-only (inside `#[cfg(test)] mod tests`); not a production leak.
- **`resp.text().await.unwrap_or_default()` in JMAP error paths** (`src/jmap.rs:1750, 1799, 1919, 2130, 2137`). These run only on non-2xx responses and are bounded by the response body of an error; no change recommended without a size measurement.

## Memory-growth classification (final)

- **Permanent production memory growth: none found.** Every candidate is request-scoped or a bounded cache.
- **Bounded caches:** `AuthVerifier` (Moka, capacity + TTL), `JmapClient::session_auth` (Moka, capacity + TTL), `JmapClient::session_cache` (DashMap, per-username, TTL-checked, but see P3-4 — it is keyed by username and never evicted except by explicit invalidation; it is bounded by the number of distinct usernames that authenticate against one client instance), `nspi::GalCache` (TTL), `PushMonitorRegistry`.
- **Process-lifetime statics, bounded:** `LazyLock` tables in `src/email.rs:866-868`, `src/timezone.rs:267` (`TZI_CANDIDATES`), the `phf` maps in `src/wbxml.rs`, `version::current()`. Intentional and fixed-size.
- **Process-lifetime static, UNBOUNDED:** `TZ_PARAMS_CACHE` (`src/timezone.rs:987`) — see P2-6. This is the one cache in the tree whose key space is controlled by a client.
- **Per-request peaks:** the P1-8/P1-9/P2-3 allocations raise peak RSS during a request and are freed at the end; they are not leaks.
- **Latent risk if P0 is fixed by cloning instead of sharing:** cloning `MapiState`/`Config` per request would deep-copy the configuration; wire `Arc`s, not `Clone`s.

## Module-by-module coverage record

| Module | Verdict |
| --- | --- |
| `src/main.rs` | Router/middleware stack reviewed. Autodiscover display-name resolution correctly avoids `spawn_blocking` for pure work and short-circuits directory lookup on principal mismatch. Middleware allocations: P2-5. |
| `src/metrics.rs` | P2-5 (`normalize_path`, per-request label lookups). Otherwise O(1) per request. |
| `src/rate_limit.rs` | P2-5 (path `String` for a static comparison). Governor key construction (`format!("ip:{ip}")`) is one allocation per request — acceptable. |
| `src/validation.rs` | P2-5 (unconditional percent-decode). Sync-key/item-id length caps are O(len). |
| `src/auth.rs` | Four-layer funnel (breaker, in-flight dedup, rate cap, shared client cache) is sound; no hot-path allocation worth changing. |
| `src/jmap.rs` | P0-1 (client churn), P3-3 (`mail_properties`), P3-4 (`get_session` clone). Batch accessors already exist and should be used by P1-1. JSON-clone-before-deserialize remains as noted in the checkpoint. |
| `src/eas.rs` | P1-2 (keyword baseline N+1), P1-5/P1-6 (tag extraction), P1-7 (`body.to_vec`), P1-8, P1-9/10 (codec), P0-2 (CalDAV client). |
| `src/ews.rs` | P1-1 (per-message `get_email`), P1-5 (55 extraction sites), P1-8 (82 `format!` appends), P0-2 (11 CalDAV sites). |
| `src/ews_folder_ops.rs` | P1-3 (mailbox re-listing per recursion level, per-folder role query). |
| `src/ews_folders.rs`, `src/ews_search.rs`, `src/ews_update.rs` | Request-shaped work, no N+1 found; `ews_update.rs` uses `nfc` (P3-2). |
| `src/sync.rs` | P2-2/P2-3 (identity load + redundant sorts), P0-2, P1-8 (64 appends). Delta/tombstone logic reviewed for the §10/§16 guarantees; unchanged. |
| `src/storage.rs` | P2-1, P2-2. All other measured plans are optimal; sync completion is transactional. |
| `src/caldav.rs`, `src/carddav.rs` | P0-2 (`new` builds a client). Response parsing is streaming/slice-based. |
| `src/contacts.rs` | P1-4 (serial photo downloads), plus the checkpoint's owner-wide index building. |
| `src/tasks.rs` | P0-2, P1-8 (21 appends). Reconcile is per-item by necessity (one PUT per object). |
| `src/mapi/handler.rs` | P0-1 (per-`Execute` client). ROP dispatch itself is slice-based; property tables are small linear scans over ≤ tens of entries. |
| `src/mapi/{store,data,rops,encoding,restrict,logon,session,nspi,rules,converters,fxics,namedprops,tnef,transport}.rs` | No N+1 found. `tnef`/`fxics` are bounded binary decoders. `nspi` uses the shared GAL cache. |
| `src/wbxml.rs` | P1-9, P1-10. Fail-closed decode/encode semantics untouched by these changes. |
| `src/jscalendar.rs`, `src/jscard.rs`, `src/vcard.rs`, `src/ical_parser.rs` | P3-1 (key allocation). Parsers are single-pass; folding is O(n). |
| `src/calendar.rs`, `src/timezone.rs` | `zone_transitions` sampling is the documented single source; `TZI_CANDIDATES` is fixed-size. **P2-6** (`TZ_PARAMS_CACHE` unbounded + global mutex), P1-8 (9 appends), P3-2. |
| `src/meeting/*` | P1-8 (23 appends in `message.rs`). RSVP pipeline is I/O-bound on SMTP/CalDAV, not CPU. |
| `src/oof.rs`, `src/oab.rs`, `src/room.rs`, `src/ecp.rs`, `src/smime.rs`, `src/delegate_ews.rs` | P3-6 (`harvest_and_store` on every send). OAB payload generation is already offloaded to `spawn_blocking`. |
| `src/smtp.rs` | `AsyncSmtpTransport` is built once and pooled by `lettre`; no per-send client construction. |
| `src/oidc.rs`, `src/jmap_push.rs` | One `reqwest::Client` per component instance (not per request); JWKS cache is TTL-bounded. |
| `src/directory.rs`, `src/notifications.rs` | Own blocking runtime / `async_trait` boxes — correct for their `dyn` usage. |
| `src/models.rs` | `AppState` already shares the JMAP client; extend the same pattern to CalDAV (P0-2). |
| `src/config.rs`, `src/logging.rs`, `src/version.rs`, `src/traits.rs`, `src/error.rs`, `src/util.rs` | Startup-only or O(len); `util.rs` P3-2. |
| `benches/hot_paths.rs` | Covers WBXML codec, email render (`sync_delta_application_data/render_{16,64,256}`), auth, JSCalendar. Those existing benchmarks are the measurement vehicle for P1-8/P1-9/P1-10; no new benchmark is required to validate them. |

## Suggested sequencing

1. P0-1, P0-2, P1-7, P2-2, P2-6, P3-1, P3-3, P3-5 — mechanical, low risk, no behavior change (P2-6 is the only memory-safety item and should not wait).
2. P1-1, P1-2, P1-3, P1-8, P1-9, P1-10 — larger but well covered by the existing test suite; each needs one new test pinning the batch/encoding behavior.
3. P1-5, P1-6, P2-3 — restructure request parsing / identity loading; do these behind the existing round-trip and delta tests.
4. P3-6, P2-5, P3-4 — opportunistic.
---

# Protocol-compliance audit of the recommendations

Every recommendation in the section above was re-checked against the 129 Microsoft protocol
documents in `exchange_protocols/` (release v20250520) and against the code path it would touch.
Nothing here was implemented; this section replaces the earlier wording wherever they disagree.

Verdicts:

- **COMPLIANT** — the change cannot alter client-visible bytes, ordering or identity.
- **REWRITE** — as written the change would alter protocol-visible behaviour. The rewritten form below
  is the one to implement; the original wording must not be used.
- **WITHDRAWN** — the saving does not justify the protocol risk.

## Verdict table

| Item | Verdict | Governing clause | Why |
| --- | --- | --- | --- |
| P0-1 shared `JmapClient` | COMPLIANT | [MS-OXCMAPIHTTP] (session context is gateway-side) | Backend-internal; the MAPI wire is untouched. Auth stays per request. |
| P0-2 shared `CaldavClient` | COMPLIANT, one constraint | — (CalDAV is not an MS protocol) | All 21 call sites pass `&state.cfg`, so the base is identical. Must share the 30 s `new` client, not the 10 s `new_from_base`. |
| P1-1 batch `get_emails` in SyncFolderItems | **REWRITE** (C1) | [MS-OXWSSYNC] §3.1.1, §3.1.4.2.3.6, §3.1.4.2.4.1 | `MaxChangesReturned` MUST be 1..=512 and the truncation prefix must be stable; ids must be keyed by id, never position. |
| P1-2 batch keyword baseline | **REWRITE** (C2) | [MS-ASCMD] §3.1.5 (Sync: collections processed "in the order received", `WindowSize`/`MoreAvailable`) | Emission order and the deferred-scan rule are protocol-visible through `MoreAvailable`. |
| P1-3 reuse mailbox list in CopyFolder | **REWRITE** (C3) | [MS-OXWSFOLD] §3.1.4.1 | Safe only because children are filtered by SOURCE parent id; the all-or-nothing rollback must stay. |
| P1-4 parallel photo downloads | **REWRITE** (C4) | [MS-OXWSCONT], [MS-OXWSSYNC] (delta contract) | The PHOTO property is part of the vCard whose hash IS the contact `ChangeKey`; "defer" would change every ChangeKey. |
| P1-5 parse request once | **REWRITE** (C5) | [MS-ASCMD] request schemas | The helper resets its accumulator on a nested same-name Start; a naive map returns the outer content instead. |
| P1-6 replace `extract_all_tag_blocks` | **REWRITE** (C6) | [MS-ASCMD] request shapes | The suggested replacement returns the FULL block, not the inner text, and is nesting-aware. The original suggestion is wrong. |
| P1-7 drop `body.to_vec()` | COMPLIANT | — | `payload` is read-only; byte-identical decode input. |
| P1-8 `write!` instead of `format!` | COMPLIANT | [MS-ASWBXML] §2.1.2.1 (token/code-page table) | Escaping happens in `xml_escape` before formatting; `write!` produces identical bytes. |
| P1-9 WBXML encode allocations | **REWRITE** (C7) | [MS-ASWBXML] §2.1.2.1, §2.1.3 | The binding stack is depth-aligned; duplicate/unknown-namespace hard errors must survive. |
| P1-10 WBXML decode allocations | COMPLIANT, constraints (C8) | [MS-ASWBXML] §2.1.2.1, §3 | `&'static str` is valid (all names come from `TAG_TO_NAME`); the root-page rule must be kept. |
| P2-1 `item_map(server_id)` index | COMPLIANT | — | Index only; result set unchanged. |
| P2-2 drop `ORDER BY id` | **WITHDRAWN** (C9) | [MS-ASCMD] ServerId durability | First-wins/last-wins identity resolution becomes order-dependent, giving client-visible ServerId churn. |
| P2-3 scope `CalendarIdentity` | **REWRITE** (C10) | [MS-ASCMD] ServerId; AUDIT.md §10 | (a)/(b) are safe; the collection discriminator must not re-key `item_map.server_id`. |
| P2-5 middleware allocations | COMPLIANT | — | Metrics/rate-limit keys only; `normalize_path` collapsing must stay identical. |
| P2-6 bound `TZ_PARAMS_CACHE` | **REWRITE** (C11) | [MS-ASDTYPE] §2.7 (TimeZone), [MS-OXWSGTZ] §2.2.4.12 | Bound the map and drop `None` memoisation, but keep the raw key and the exact outputs. |
| P3-1 `sort_by_key(..to_string())` | **REWRITE** (C12) | [MS-OXWSCONT], [MS-OXWSSYNC] | Property order is hashed into the contact `ChangeKey`; the sort must be stable and lexicographic. |
| P3-2 `normalize_email` fast path | COMPLIANT | [MS-OXOCAL] attendee identity | ASCII lowercase equals Unicode lowercase for ASCII input; NFC is a no-op there. |
| P3-3 `const` mail properties | COMPLIANT | RFC 8621 §4.1 (property names) | Same names, same array shape. |
| P3-4 `Arc<JmapSession>` | COMPLIANT | — | Cache handle only. |
| P3-5 drop unused deps | COMPLIANT | — | Keep `memchr` (used by C6). |
| P3-6 gate S/MIME harvest | **REWRITE** (C13) | [MS-OXOSMIME] | The gate must be a conservative superset check; a false negative loses a GAL certificate. |
| P3-7 NUL byte in a comment | COMPLIANT | — | No wire effect. |

## Rewritten changes

### C1 — replaces P1-1 (EWS `SyncFolderItems` batching)

Batch **per phase**, chunked at 500 ids (Stalwart's `maxObjectsInGet`; `MaxChangesReturned` is capped
at 512 by [MS-OXWSSYNC] §3.1.4.2.4.1, so one delta needs at most two chunks):

1. Collect the phase's ids (`changes.created` for the Create/Update arm, the destroyed walk for the
   Delete arm).
2. `jmap.get_emails(account_id, &chunk, Some(json!(JmapClient::mail_properties())), user, pass, true)`
   — pass exactly what `get_email` passes today (`jmap.rs:2909-2918` shows `get_email` is already a
   one-element `get_emails` with that property list), so the rendered `<t:Message>` is byte-identical.
3. Index the response **by `email.id`**, never by position.
4. Iterate the ORIGINAL id order and apply the existing per-id logic unchanged:
   - id absent from the response → the current `Ok(None)` branch (skip; not an error),
   - `!email_lives_in_mailbox` → `undelivered_ids`,
   - otherwise render, `rendered_ids.insert`, stage the flag-baseline seed.
5. A chunk-level `Err` fails the whole call closed exactly as a per-id `Err` does today
   (`ErrorServerBusy`, `SyncState` unchanged) — never a partial response.

Invariants that must not change:

- the **set and order** of `<t:Create>`/`<t:Update>`/`<t:Delete>` entries, because truncation to
  `MaxChangesReturned` slices a prefix of that order and a moving prefix makes the client loop;
- `IncludesLastItemInRange`, `SyncState`, and the 1..=512 cap ([MS-OXWSSYNC] §3.1.4.2.3.6,
  §3.1.4.2.4.1);
- each item's `ChangeKey` (for messages it is `server_id.clone()`, content-independent — `ews.rs:5675`);
- the flag-baseline seeds staged and committed atomically with the window (PR #2046 invariant).

### C2 — replaces P1-2 (EAS keyword-baseline batching)

One query per scan page:

```
SELECT email_id, keywords_json FROM email_keyword_baseline
WHERE owner = ?1 AND collection_id = ?2 AND email_id IN (...)
```

built with `sqlx::QueryBuilder::push_tuples`, chunked at 500 ids to stay well under SQLite's variable
limit. Then iterate the page's ids in their **original order** and look each one up in the returned
map, so the emitted `<Change>` sequence and the overflow-queue order are unchanged.

Keep: a page-level read error must still set `KeywordScanOutcome.deferred` (hold the rotating
position) and must never be treated as "no row". Keep the collection-level accounting exactly as is —
[MS-ASCMD] requires collections to be "processed by the server in the order received" and defines
`WindowSize`/`MoreAvailable` per collection, so a batching change that shortened a page would change
`MoreAvailable` and desynchronise the client.

### C3 — replaces P1-3 (`CopyFolder` mailbox-list reuse)

Hoist `query_mailboxes` into `handle_copy_folder` and pass `&[JmapMailbox]` through `FolderCopyCtx`.

This is safe for a specific reason worth recording: the recursion reads the list only for (a) the
**source** folder's display name and (b) the children filter
`parent_id == Some(src_mailbox_id)`. The copy creates mailboxes under the **destination**, so a
newly created id can never satisfy a filter on a source id — the cached list cannot go stale inside
one `CopyFolder`, and recursion is still bounded by `MAX_DEPTH`.

Also hoist the per-source `get_mailbox_ids_for_role` call (`ews_folder_ops.rs:160`) to one
`Mailbox/query` for the whole operation.

Keep: one `FolderId` per requested folder **in request order** in the `CopyFolderResponse`, and the
all-or-nothing `rollback_copied_subtree` on any failure — that rollback is what makes a client retry
safe under [MS-OXWSFOLD] §3.1.4.1.

### C4 — replaces P1-4 (contact photos)

**Parallelise only. Do not defer.**

The photo is pushed into `v.properties` **before** `v.to_string()` (`contacts.rs:1341-1358`), and
with Stalwart exposing no `@etag` the EWS contact `ChangeKey` is `sha256(vcard)`
(`contacts::contact_change_key` → `contact_hash`). Deferring the photo to render time would change
that hash for every contact and make the server report the entire address book as modified on the
next `SyncFolderItems` — the opposite of the intended saving, and a delta-contract violation.

```rust
futures_util::stream::iter(cards.into_iter().enumerate())
    .map(|(i, card)| async move { (i, render_card_with_photo(card).await) })
    .buffer_unordered(8)
    .collect::<Vec<_>>()
    .await
```

then sort by the captured index before returning, so the returned `Vec<CarddavContact>` order is the
input order (callers build full snapshots and derive ChangeKeys positionally in the EAS diff).

Failure semantics unchanged: a failed blob download logs a warning and omits `PHOTO`, exactly as
today. (That is already hash-visible; "improving" it here would change ChangeKeys for unrelated
reasons.)

### C5 — replaces P1-5 (single-pass request parsing)

A single-pass rewrite MUST reproduce this state machine (`eas.rs:743-771`) exactly:

- match on the **local name** (prefix-agnostic);
- the FIRST `Start` of that local name begins the value; a later `Start` of the SAME local name
  (nesting) **clears the accumulator** — today's code re-enters `inside` and calls `value.clear()`;
- collect `Text` + `CData` + `GeneralRef` (resolved through `resolve_xml_reference`) verbatim, with
  **no per-event trim** (that is what preserves `R &amp; D` → `R & D`);
- the FIRST `End` of that local name returns `trim_owned(value)`;
- a parse `Err` yields `None` for every tag, which is what `validate_payload` relies on to reject a
  malformed command.

A map-of-first-occurrence built by "append until the matching End" changes the nested case: it would
return the OUTER content where the current code returns the inner one. Either replicate the reset,
or prove per call site that same-name nesting is unreachable.

### C6 — replaces P1-6 (`extract_all_tag_blocks`)

**The original suggestion must not be followed.** `ews.rs:651 extract_all_tag_blocks_checked` returns
the **full raw block including `<Tag>`/`</Tag>`** and is nesting-aware; `eas.rs:709
extract_all_tag_blocks` returns the **inner text only**, is non-nesting-aware (the first `</Tag>`
closes), and is lossy-UTF-8. Swapping them changes both the returned bytes and the nesting semantics
at every call site (`ews.rs:10544` uses the ews variant; `eas.rs:1173, 3088, 3139, 3189` use the eas
variant).

Rewritten: keep the `eas.rs` semantics exactly and change only the search mechanism:

- `memchr::memmem::find(&bytes[pos..], open)` / `find(&bytes[start..], close)` instead of
  `bytes[..].windows(len).position(..)` — removes the O(n·m) scan;
- hoist the `open`/`close` construction out of the per-call path (`memmem::Finder::new` behind a
  `OnceLock` per tag, or build once and pass a `&[u8]` pair);
- keep `String::from_utf8_lossy(..).into_owned()` and the first-close rule.

A nesting-aware rewrite would change what those five call sites receive; that is a separate,
deliberate behaviour change and is out of scope for a performance change.

### C7 — replaces P1-9 (WBXML encoder allocations)

Keep all of the following (each is pinned today by `encode_rejects_non_namespace_attributes`,
`encode_rejects_declared_unknown_namespace_bindings` and the §3 byte-exact test):

- a binding set is pushed for **every** `Start`/`Empty`, empty or not, because `prefix_ns_stack` is
  depth-aligned with `ns_stack` and `resolve_prefix_binding` walks it in reverse;
- duplicate attribute names remain a hard error, compared with the same key representation the
  current `to_string()` comparison uses;
- a non-namespace attribute remains a hard error — [MS-ASWBXML] §2.1.3 defines no attribute encoding;
- `xmlns` with an unknown URI is an immediate hard error; `xmlns:p` with an unknown URI stays `None`
  and errors only when `p` is **used**;
- the canonical-prefix fallback (`AirSyncBase:` → its own page) and the implicit-root fallback are
  unchanged;
- the emitted token stream is byte-identical.

Only the representation changes: `Option<SmallVec<[(Box<str>, Option<u8>); 2]>>` (or a `&'static`
slice when the prefix is one of the canonical ones) with a zero-attribute fast path that allocates
nothing.

### C8 — constraints for P1-10 (WBXML decoder allocations)

`Vec<&'static str>` is valid: every name comes from `TAG_TO_NAME`
(`phf::Map<[u8; 2], &'static str>`, `wbxml.rs:14`) and the root-page rule takes a
`rsplit(':').next()` slice of that `'static` str. Keep:

- the root-page-unqualified rule (tags on the root page expand unqualified, `SWITCH_PAGE`-reached tags
  keep `Prefix:`);
- `format!("</{tag}>")` → `push_str("</"); push_str(tag); push('>')` — identical bytes;
- `ENTITY` → `&#{n};` — identical bytes;
- `String::from_utf8(bytes.to_vec())` → `std::str::from_utf8(bytes)?.to_owned()` in the raw-XML
  passthrough branch only (`wbxml.rs:1429`);
- `with_capacity` must account for the `<?xml …?>\n` prefix already in `output`.

### C9 — P2-2 is WITHDRAWN

`ORDER BY id ASC` is load-bearing, not incidental. `CalendarIdentity::load` resolves the canonical
ServerId with first-wins `by_uid.entry(..).or_insert(..)` and first-wins
`by_resource.entry(..).or_insert(..)`, and then **overwrites** `by_resource` from the alias list
(last-wins). Dropping the clause makes the winner depend on the index SQLite happens to scan, so an
owner holding two `item_map` rows that share a UID or an href could be given a **different ServerId
between two requests** — a client-visible identity change that EAS reports as Delete+Add
([MS-ASCMD]: the client "MUST store the ServerId … and MUST be able to locate [the object] given a
ServerId").

The sort runs once per calendar Sync over one owner's rows, where per-row rendering dominates, so
there is nothing to win. If it ever measures: a plain `item_map(owner)` index already yields rowid
order for a rowid table, so add that instead of dropping the clause.

### C10 — replaces P2-3 (scoping `CalendarIdentity`)

(a) and (b) are compliant: they change allocation and reuse only, not the resolved ServerId.

(c) is rewritten to state the hard constraint. A per-collection discriminator must NOT:

- change `item_map.server_id` — that text is the EAS `ServerId` ([MS-ASCMD]) and the body of the EWS
  `ItemId` ([MS-OXWSITEMID]);
- change which row wins the UID/href join (see C9);
- change the documented cross-collection behaviour of AUDIT.md §10.

It is a schema change with client-visible identity risk, not a performance tweak, and it should be
justified by measurement before it is attempted.

### C11 — replaces P2-6 (`TZ_PARAMS_CACHE`)

Bound the map, but keep the key and the outputs:

- keep the **raw input string** as the key (the code documents that distinct spellings get their own
  entry "rather than aliasing surprises"); do not introduce canonicalisation as part of a perf fix;
- bound it (moka, capacity ≈ 4096; entries are pure, so no TTL is needed);
- do NOT memoise `None` — that is precisely the attacker-chosen key space, and a miss is cheap;
- `moka::sync::Cache` also removes the double lock acquisition.

Outputs must stay identical: the same `(bias, win_name, dst_name, std_rule, dst_rule, 0, dst_bias)`
feeds the EAS `TimeZone` TZI blob ([MS-ASDTYPE] §2.7) and the EWS `StartTimeZone`/`EndTimeZone`
Id+Name pair ([MS-OXWSGTZ] §2.2.4.12). The computation is deterministic, so caching cannot change
them — but that is the invariant to test, not to assume.

### C12 — replaces P3-1 (`jscard` sort)

Use a **stable** sort with the **same** key:

```rust
entries.sort_by(|a, b| a.0.cmp(b.0));
```

`sort_unstable_by` would be safe only by accident (JSON object keys are unique, so there are no
ties) and is one refactor away from becoming a bug.

MUST NOT change the comparison to numeric or insertion order. The emitted vCard property order is
part of the vCard bytes, and with Stalwart exposing no `@etag` the EWS contact `ChangeKey` is
`sha256(vcard)` (`contacts::contact_change_key` → `contact_hash`). Any reordering changes every
contact's `ChangeKey` and makes the server report the whole address book as modified — a
[MS-OXWSCONT]/[MS-OXWSSYNC] delta-contract violation. The current lexicographic order on the
JSCalendar map key (`"1"`, `"10"`, `"2"`, …) must be preserved verbatim.

### C13 — replaces P3-6 (S/MIME harvest gate)

The gate must be a conservative **superset** check:

```rust
let looks_smime = contains_ci_unfolded(raw_mime, b"pkcs7")
    || contains_ci_unfolded(raw_mime, b"multipart/signed");
if looks_smime {
    crate::smime::harvest_and_store(&state.storage, raw_mime, &owned).await;
}
```

Unfold CR/LF/space/tab before matching, so a folded `Content-Type: multipart/\r\n signed` is still
found, and match `pkcs7` (which covers `application/pkcs7-mime`, `application/x-pkcs7-mime` and
`application/pkcs7-signature`). Never skip when any token appears.

The harvest is gateway-local: [MS-OXOSMIME] governs the structure of S/MIME messages, not this GAL
certificate cache, and `harvest_and_store` already cannot affect the sent message. So a false
positive costs one parse, while a false negative silently loses a certificate — the superset rule
removes the only real risk.

## Items confirmed compliant — invariants to keep while implementing

- **P0-1 / P0-2** — share the client, keep the timeout of the constructor in use (`new` = 30 s), and
  keep auth per request (never cache credentials on the client). `state.jmap_client` is already an
  `Arc<JmapClient>`, so P0-1 is a `MapiState` field, not a new abstraction.
- **P1-7** — `payload` is read-only at all three uses (`payload.first()` sniff, `wbxml.decode`,
  `validate_payload`), so `&body` is byte-identical.
- **P1-8** — escaping is performed by `xml_escape`/`xml_escape_text` *before* formatting; `write!`
  cannot change the bytes.
- **P1-10** — see C8.
- **P2-1** — an added index cannot change a result set; keep the `commit_calendar_sync` transaction
  atomic (the extra index write is inside it).
- **P2-5** — `normalize_path`'s id collapsing produces the *metric label*; keep it identical or every
  dashboard series splits.
- **P3-2** — `to_ascii_lowercase()` is exactly `to_lowercase()` for ASCII input, and NFC is a no-op
  there; the non-ASCII path must keep Unicode lowercasing + NFC, because attendee matching compares
  the normalised address.
- **P3-3** — keep the property list element-for-element; RFC 8621 silently drops unknown names.
- **P3-5** — keep `memchr`; C6 depends on it.
- **P3-7** — replace the NUL with a printable character; no protocol surface.
- **Rust-feature table** — `Vec::extract_if` is an in-place **stable** filter, so it may be used on
  the delta id lists (relative order is preserved); it must not be used to filter or reorder
  already-rendered XML fragments. `#[expect(lint)]`, `Cow` returns and `LazyLock` tables have no
  wire effect.

## Net effect on the ranking

The rewrites leave the P0/P1 ordering unchanged (the two client-construction items are the largest
and are protocol-neutral). Three items changed materially:

- **P2-2 is withdrawn** — it was the only "free" SQLite win and it is not free.
- **P1-4 keeps its cost model but loses the "defer" option** — the parallel form is the only
  compliant saving.
- **P1-6's replacement target changed** — the allocation win stands, the "reuse the checked helper"
  shortcut does not.

P3-1 moves up in importance: it is the only item where a careless implementation silently rewrites
every contact's `ChangeKey`.

---

# P0 delivered — shared backend clients (implementation record)

Scope implemented: **P0-1** and **P0-2** only, in their rewritten (↻) form from
`# Protocol-compliance audit of the recommendations`. Nothing else in this document was
touched; every P1/P2/P3 item remains open.

## P0-1 — one `JmapClient` for MAPI `Execute`

`MapiState` gained `jmap: Option<Arc<JmapClient>>`, wired by `AppState::new` from the SAME
`Arc` it already builds before the auth verifier (the ungated client, so the condition stays
exactly the old one: "JMAP is available when `jmap_base` is set", independent of
`email_enabled`). `handle_execute` now reads `state.jmap.as_deref()` instead of calling
`JmapClient::new` per RPC.

The constructors (`MapiState::new`, `MapiState::with_subscription_manager`) still build a
client from `cfg` when `jmap_base` is set, so every existing unit-test fixture keeps its exact
behaviour; production overrides it through `with_jmap`.

Constraint from the audit kept: auth stays per request. The shared client carries no
credentials — every JMAP call still passes `username`/`password` explicitly.

## P0-2 — one `CaldavClient` for the whole process

`AppState` gained `caldav_client: Option<Arc<CaldavClient>>`, built once in `AppState::new`
with `CaldavClient::new(&cfg)` — the 30 s constructor, as the audit requires (`new_from_base`
and its 10 s timeout stay for tests and per-user-home callers). All 26 production
construction sites were replaced with `state.caldav()` — the 22 the audit listed, plus the
three in the §13 RSVP pipeline (`meeting/rsvp.rs`, written after the audit) and the MAPI
calendar-contents read:

| file | sites |
| --- | --- |
| `src/sync.rs` | `apply_client_sync_mutations`, `perform_sync` (CalDAV fallback) |
| `src/tasks.rs` | `TaskBackendStore::for_user`, `backend_inventory_changed` |
| `src/eas.rs` | `ItemOperations` Fetch, `merged_freebusy_for_mailbox` |
| `src/eas_folders.rs` | create / delete / prune-reconcile / rename |
| `src/ews.rs` | free-busy fallback, `load_current_calendar_items_caldav`, GetItem ×2, SyncFolderItems, `load_stored_calendar_item`, CopyItem, CreateItem, UpdateItem, DeleteItem, `event_has_attendees` |
| `src/meeting/rsvp.rs` | `read_calendar_row_ics`, `write_existing_copy`, `create_attendee_copy` |
| `src/mapi/handler.rs` | `fetch_calendar_rows` (via `MapiState.caldav`) |
| `src/main.rs` | `verify_caldav_health` |

`state.caldav()` returns `Result<&Arc<CaldavClient>, &str>`: the `Err` carries the startup
construction failure so each call site still logs the original cause instead of a generic
"unavailable". The client is built even when `caldav_base` is empty, because that is what the
previous per-request construction did — call sites that must distinguish "CalDAV not
configured" keep testing `cfg.caldav_base`, unchanged.

`TaskBackendStore` now holds `Arc<CaldavClient>` instead of owning a private client.

## Evidence (executed, `rustc 1.99.1` / `cargo 1.99.1`)

- `models::tests::execute_reuses_one_jmap_client_across_rpcs` (in `src/mapi/handler.rs`):
  two `Execute` RPCs, each resolving a mailbox over JMAP, against a stub that counts
  `GET /session`. Asserts **2** JMAP POSTs and **1** session fetch. Reverting `handle_execute`
  to the old per-RPC construction makes the same test fail with `session_hits = 2`, i.e. the
  "extra `GET /session` per `Execute`" the audit describes is reproduced and then removed.
- `models::tests::shared_caldav_client_reuses_one_tcp_connection`: a raw TCP stub counts
  accepted sockets. Two `check_credentials` calls through `state.caldav()` open **1**
  connection; two throwaway `CaldavClient::new` clients (the pre-fix shape) add **2** more.
  The control arm proves the counter observes sockets, not requests.
- `models::tests::caldav_client_is_shared_across_the_process`,
  `mapi_state_shares_the_process_wide_backend_clients`,
  `caldav_client_exists_without_a_configured_base`: `Arc::ptr_eq` on the instances handed to
  EAS/EWS/MAPI, plus the "no `caldav_base` still yields a client" behaviour.
- Gate: 1512 lib + 36 + 3 + 10 + 6 + 5 + 10 + 15 + 11 + 1 + 8 + 1 = 1618 tests green,
  `cargo clippy --all-targets` 0 warnings, `rustfmt --edition 2024` clean on every touched
  file, release build warning-free.

## Deliberately NOT changed

- `CarddavClient::new` in `mapi/handler.rs::fetch_contact_rows` still builds a client per
  contents-table read. It is the same defect class as P0-2 but is NOT listed in P0, and the
  JMAP-first contacts path usually short-circuits before it. Recorded here as the natural
  companion of P0-2 if a follow-up is scheduled.
- `JmapClient::new` in `src/directory.rs`, `src/oof.rs` and `AuthVerifier::new`'s fallback are
  per-call by design (admin-credential probes with their own lifecycle); P0-1 named the MAPI
  `Execute` path specifically.
- `src/main.rs`'s startup capability probe keeps its one-shot client: it runs once at boot.

---

# Follow-up: the three "Deliberately NOT changed" items (delivered)

This section supersedes the three bullets immediately above that describe
`fetch_contact_rows`, `directory.rs`/`oof.rs` and `AuthVerifier::new`'s fallback.
Two of them were real and are fixed here; the third was a misreading on my part
and is corrected.

## 1. MAPI contacts table: one shared `CarddavClient` (was: one client per read)

`mapi/handler.rs::fetch_contact_rows` built `CarddavClient::new(cfg)` on every
contacts contents-table read, so each read opened a fresh TCP+TLS connection to
Stalwart and dropped the pool afterwards. `MapiState` now carries
`carddav: Option<Arc<CarddavClient>>` (field, `with_carddav` builder and
`default_carddav_client` for fixtures — mirroring the CalDAV field), `RopContext`
carries `carddav`, and `AppState::new` wires `state.carddav_client`: the same
`Arc` the EWS contact paths use.

The `None` arms keep the old contract exactly: an explicitly configured
`carddav_base` still builds one on demand, otherwise the `/dav/card` base is
derived from `caldav_base`. Production never reaches them (`Config::load` derives
`carddav_base` from `caldav_base`, and `default_carddav_client` covers
hand-built configs).

Evidence — `mapi::handler::tests::fetch_contact_rows_uses_the_shared_carddav_client`:
the shared client points at a counting stub while `cfg.carddav_base` points at a
dead port, so only the injected client can reach the stub. Two reads accept
**1** socket; the control arm (no injected client) adds **2** more, one per
on-demand construction.

## 2. `AuthVerifier::new` no longer builds a client per probe

`verify_backend`'s `None` arm constructed `JmapClient::new(&self.jmap_base)` on
every probe for a verifier that had not been handed the application client (unit
tests, MAPI harnesses): a connection pool and a credential cache discarded per
probe, and the client-level cache could not honour the verifier's knobs.

`from_parts` now builds exactly ONE client per verifier, via
`JmapClient::with_session_auth` with the verifier's own positive/negative TTLs and
capacity, so `GATEWAY_AUTH_CACHE_NEGATIVE_TTL_SECS=0` disables negative caching on
BOTH layers. The `None` arm of `verify_backend` now answers a retryable
`BackendError` (reachable only if the client could not be built at all).

Sharing the client exposed a real staleness hole that the per-probe construction
had been hiding: `probe_auth` inserts its outcome into the client cache BEFORE
`verify_outcome`'s generation check runs, so a superseded probe could leave a
fresh positive that shadows the newer login — the failure §17's ticket prevents
one layer up. Fixed with `JmapClient::invalidate_session_auth(username, password)`
(per-credential) called from the new `AuthVerifier::discard_superseded_result` on
both superseded paths (valid and invalid). The same hole existed in production,
where the client is already shared with the verifier.

Evidence — `auth::tests::standalone_verifier_reuses_one_jmap_connection_across_probes`:
two cold probes for different users accept **1** socket; the control (two
throwaway clients, the pre-fix shape) adds **2**. Restoring the old per-probe
construction makes the test fail with `2 != 1`.
`auth::tests::slow_older_verification_cannot_repopulate_cache_after_newer_login`
now also pins the client-layer purge (it failed before the fix).

## 3. Correction: `directory.rs` / `oof.rs` are startup singletons

The bullet above claims their `JmapClient::new` calls are "per-call by design".
That is wrong. `JmapDirectory::create` and `JmapOofManager::create` are each
called ONCE from `AppState::new`, the client is stored in the manager, and the
per-call `self.jmap_client.clone()` is a shallow clone (`#[derive(Clone)]` on
`JmapClient` shares the `reqwest::Client`, the session cache and the probe
cache). There is no per-request construction and therefore nothing to fix.
Documented on both struct fields so a later "optimisation" cannot reintroduce one.

## 4. Test-harness flake: `concurrent_same_credential_probes_collapse`

The probe counter was incremented AFTER `write_all`, so under CPU pressure the
acceptor thread could be descheduled between the write and the increment while
the client (which already held the response) returned `Verified` and the test read
0 — a load-dependent `hits >= 1` failure. The counter is now taken when the
request is READ, i.e. before any client can observe success; each connection is
served on its own thread; and `read_request` parses `Content-Length` instead of
waiting for a buffer that ends with `\r\n\r\n` (which never happens for a
body-carrying request and used to block until the 30 s read timeout). The test
also asserts `hits < 5`, the dedup property its name claims. 20/20 isolated runs
and 24/24 under 8-way parallel load, all green.

## Gate

1620 tests green (1514 lib + 36 + 3 + 10 + 6 + 5 + 10 + 15 + 11 + 1 + 8 + 1),
`cargo clippy --all-targets` 0 warnings, `rustfmt --edition 2024` clean on every
touched file (`auth.rs`, `jmap.rs`, `models.rs`, `mapi/handler.rs`,
`directory.rs`, `oof.rs`, `tests/transport_fault_injection.rs`), release build
warning-free.
