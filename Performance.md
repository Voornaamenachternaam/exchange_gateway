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

**P1-1. EWS `SyncFolderItems` fetches each changed message individually.**
`src/ews.rs:5663`, `:5705`, `:5770`, `:5836` each call `jmap.get_email(...)` inside the per-id loops over `changes.created` / `changes.destroyed`. A 100-message delta therefore costs 100 serial HTTP requests. A batch accessor already exists (`get_emails`, used at `src/ews.rs:11589`, `src/eas.rs:3823`, `:8891`).

Change: collect the ids for each phase, call `get_emails` in chunks bounded by Stalwart's `maxObjectsInGet` (500, back-references exempt), build a `HashMap<&str, JmapEmail>`, then render. Keep the existing fail-closed behavior: a batch failure must still fail the whole call (`ErrorServerBusy`), and a missing id must still be treated as `undelivered_ids`. Effect: O(1) round trips instead of O(n). Risk: medium — the per-id "message left the folder" (`email_lives_in_mailbox`) and seed/`rendered_ids` bookkeeping must be preserved exactly; this is the kind of change that needs the existing delta tests plus a new "batch partially absent" test. Confidence: proven cost, straightforward fix.

**P1-2. The EAS email delta reads the keyword baseline one row per message.**
`src/eas.rs:8687` calls `storage.get_email_keyword(username, state_collection_id, &id)` inside the per-entry loop of the bounded flag scan (`KEYWORD_SCAN_MAX_PAGES = 8`, `:8274`). Each page is one `Email/query` plus one SQLite round trip per message.

Change: one `SELECT email_id, keywords_json FROM email_keyword_baseline WHERE owner=?1 AND collection_id=?2 AND email_id IN (...)` per page (`sqlx::QueryBuilder::push_tuples`). Preserve the current fail-closed rule: a page-level read error must still hold the scan position (`outcome.deferred = true`) rather than be treated as "no row". Effect: page-size × fewer SQLite round trips per delta. Risk: low. Confidence: proven.

**P1-3. `CopyFolder` re-lists every mailbox once per folder copied.**
`src/ews_folder_ops.rs:1086` calls `ctx.jmap.query_mailboxes(...)` at every recursion level of `copy_folder_recursive`; `src/ews_folder_ops.rs:159` calls `get_mailbox_ids_for_role` (one `Mailbox/query`) per distinguished source folder. Copying a subtree of N folders costs N full mailbox listings.

Change: fetch the mailbox list once in the handler, pass `&[JmapMailbox]` through `FolderCopyCtx`, and resolve distinguished ids from that list. Effect: N listings → 1. Risk: low. Confidence: proven.

**P1-4. Contacts photo downloads are serial.**
Retained from the checkpoint (`src/contacts.rs:1337-1358`): one awaited download per contact photo. Change: bounded concurrency (`futures_util::stream::iter(...).buffer_unordered(K)`, K≈8) or defer to rendered contacts. Effect: latency divided by K. Risk: medium (ordering/identity must not change). Confidence: proven cost.

### P1 — repeated parsing of the same request document

**P1-5. `extract_first_tag_text` re-parses the whole document on every call.**
`src/eas.rs:743` and `src/ews.rs:594` build a `quick_xml::Reader` from the start of the document and scan until the tag closes. Call counts: **68 sites in `src/eas.rs`, 55 in `src/ews.rs`**. For a `SendMail` carrying `MIMEData` (admitted up to 40 MiB) each extraction is a separate prefix scan; the total work is O(k·n) in document size.

Change: parse once per handler into a `HashMap<&str, Vec<String>>` (borrowed local names, borrowed text where possible) or a typed request struct, and read fields from it. Cheapest interim step: have the helpers share one `Reader` via a cursor and require callers to request tags in document order. Effect: large on the SendMail/SmartForward paths, moderate on Sync. Risk: medium — entity/whitespace semantics are pinned by `extract_first_tag_text_keeps_spaces_around_entities`; a rewrite must keep the "text not trimmed per event, only the finished value" rule. Confidence: proven cost.

**P1-6. `extract_all_tag_blocks` is an O(n·m) byte-window scan with per-call allocations.**
`src/eas.rs:709-735` builds `open`/`close` `Vec<u8>` per call and searches with `bytes[pos..].windows(len).position(...)`. It is also not nesting-aware.

Change: use `memchr::memmem::find_iter` (the `memchr` crate is already a declared dependency and currently unused, see P3-5) or reuse `extract_all_tag_blocks_checked` (`src/ews.rs:651`), which is nesting-aware and slice-based. Effect: removes a quadratic term on large bodies. Risk: low. Confidence: proven.

**P1-7. `let payload = body.to_vec();` copies every EAS request body.**
`src/eas.rs:10485`, in `handle_eas_request` before dispatch — so the copy happens on **every** EAS request, not just the large ones. `body` is `Bytes` (already a cheap refcounted handle) and `Wbxml::decode` takes `&[u8]` (`src/wbxml.rs:1424`). The copy is up to `MAX_BODY_BYTES` = 40 MiB (`src/main.rs:38`).

Change: pass `&body` (or `body.as_ref()`), keeping the owned `Bytes` alive for the request. Effect: removes a full-body memcpy from every EAS request. Risk: none. Confidence: proven.

### P1 — allocation-heavy response construction

**P1-8. 280 `push_str(&format!(...))` sites allocate a temporary `String` per append.**
Distribution: `src/ews.rs` 82, `src/sync.rs` 64, `src/eas.rs` 36, `src/meeting/message.rs` 23, `src/tasks.rs` 21, `src/email.rs` 13, `src/contacts.rs` 13, `src/timezone.rs` 9, `src/oof.rs` 5, `src/wbxml.rs` 4, plus a few others.

Change: `use std::fmt::Write;` and `write!(out, "...", ..)` / `writeln!`. In render loops (a 500-item Sync response, ~10 fields per item) this removes thousands of short-lived allocations per response, and it is mechanical. Note `write!` returns `Result`; use `let _ = write!(...)` on `String` (which cannot fail) or `expect`. Effect: measurable allocator pressure reduction on every Sync/Find/GetItem response. Risk: low. Confidence: proven cost, mechanical fix.

**P1-9. The WBXML encoder allocates a `HashMap`, a `Vec<String>` and `String`s per element.**
`src/wbxml.rs:1650-1652` calls `encode_namespace_attributes` for every `Event::Start` and pushes the result onto `prefix_ns_stack`. `encode_namespace_attributes` (`:1860-1895`) starts with `HashMap::new()`, `Vec::new()`, and `attr.key.as_ref().to_string()` per attribute. In EAS documents only the root element declares namespaces, so virtually all of these allocations are empty maps and empty vecs — one pair per element, i.e. thousands per Sync response.

Change: (a) return a `PrefixBindings` that is `Option<Box<[(Box<str>, Option<u8>)]>>` (or `SmallVec<[_; 2]>`) and represent "no attributes" as `None` without allocating; (b) take a fast path when `e.attributes().len() == 0`; (c) keep attribute keys as `&str` borrowed from the event buffer instead of `to_string()`. Effect: removes the dominant per-element allocation in the encoder. Risk: low-medium — namespace resolution semantics (declared-unknown = hard error, canonical prefix fallback) are pinned by `encode_rejects_declared_unknown_namespace_bindings` and the §3 byte-exact test. Confidence: proven cost.

**P1-10. The WBXML decoder stores an owned `String` per open element and formats one per close.**
`src/wbxml.rs:1452` `xml_stack: Vec<String>` and `:1480` `output.push_str(&format!("</{tag}>"))` (also `:1501` for entity references). `TAG_TO_NAME` values are `&'static str` (`src/wbxml.rs:14`), so the stack can hold `&'static str`.

Change: `Vec<&'static str>` for the stack (with an owned fallback only if a future token source needs it), `output.push_str("</"); output.push_str(tag); output.push('>')` instead of `format!`, and `String::with_capacity(bytes.len() * 2)` for `output` (WBXML is usually expanded ~2–4×). Also `:1429` `String::from_utf8(bytes.to_vec())` can be `std::str::from_utf8(bytes)?.to_owned()`. Effect: two allocations per element removed on the hottest codec path. Risk: low. Confidence: proven.

### P2 — SQLite

**P2-1. `get_item_owner` is a full index scan.**
`src/storage.rs:3383-3390`: `SELECT owner FROM item_map WHERE server_id = ?1`. Measured plan: `SCAN item_map USING COVERING INDEX sqlite_autoindex_item_map_1`. Called from `src/ews.rs:4480` (GetItem/calendar item), `src/eas.rs:4131` (attachment fetch), `src/eas.rs:4461` (calendar item fetch) — once per item read, O(total items) each.

Change: `CREATE INDEX idx_item_map_server ON item_map(server_id)` (the unique index is `(owner, server_id)`, so `server_id` alone cannot be seeked). Write cost is one extra index per `item_map` insert. Effect: O(log n) instead of O(n) per item read. Risk: low. Confidence: proven (plan measured).

**P2-2. Two owner-scoped identity queries sort their whole result set for an ordering the caller discards.**
`src/storage.rs:2268` and `:2283` both end in `ORDER BY id ASC`; measured plans show `USE TEMP B-TREE FOR ORDER BY`. Both callers (`src/sync.rs:130-157`) insert into `HashMap`s where order is irrelevant.

Change: drop `ORDER BY id` (or add `(owner, id)`-leading indexes if a deterministic order is ever needed). Effect: removes an O(n log n) sort per calendar Sync. Risk: none. Confidence: proven (plan measured).

**P2-3. `CalendarIdentity::load` materializes the whole owner item map per calendar sync.**
`src/sync.rs:1892` calls `CalendarIdentity::load` once per `perform_sync`; the loader (`src/sync.rs:121-160`) runs two full owner scans and builds three maps with `String` clones per row (`server_id.clone()`, `resource_identity(...).into_owned()`). For an owner with N events and C calendars, one device sync of all C calendars costs C·O(N) rows and allocations.

Change, in increasing ambition: (a) `resource_identity` already returns a `Cow` — avoid `into_owned()` when the href is already path-only by keying on a `Box<str>` built once; (b) cache the loaded identity for the duration of one sync request when several collections are synced together; (c) add a collection discriminator to `item_map` so the map can be scoped (this is the structural fix, and it also removes the cross-collection `item_map` sharing that AUDIT.md §10 documents as the reason scoped deletes cannot tombstone). Effect: proportional to N. Risk: high for (c) — it interacts with the documented shared-row semantics; (a) and (b) are low risk. Confidence: proven cost; benefit needs N in production.

**P2-4. Positive finding — do not change.** The journal delta uses `idx_change_journal_op`, the keyword baseline uses its unique `(owner, collection_id, email_id)` index, `folder_map` and `deleted_item_tombstone` use their owner indexes, and the sync-completion writes are already batched into one transaction (`Storage::commit_calendar_sync`). No further index is justified by the measured plans.

### P2 — per-request middleware overhead

**P2-5.** `src/metrics.rs:528-529` allocates `req.uri().path().to_string()` and then `normalize_path` (which builds a `Vec<String>` of segments plus a joined `String`) for every request; `record_request` (`:156-168`) performs three `with_label_values` lookups and one `status.as_u16().to_string()`. `src/rate_limit.rs:101` allocates the path string only to compare it against a static list (`is_unmetered_path`), and `src/validation.rs:44` percent-decodes every path.

Change: `is_unmetered_path(req.uri().path())` on the borrowed `&str`; make `normalize_path` write into a reusable `String` (or map the known route set through a `phf`/`OnceLock<HashMap<&'static str, &'static str>>`, since the router's route table is static); skip percent-decoding when the path contains no `%`. Effect: removes 3–5 short allocations per request from the fixed overhead. Risk: low — keep `normalize_path`'s id-collapsing behavior identical. Confidence: proven cost, small absolute size.

**P2-6. `TZ_PARAMS_CACHE` is an unbounded process-wide cache keyed on a client-supplied string.**
`src/timezone.rs:987-1004`: `static TZ_PARAMS_CACHE: LazyLock<Mutex<HashMap<String, Option<TzParams>>>>` is keyed by the **raw input string** and has no capacity limit and no eviction (the only accesses are `get` at `:991` and `insert` at `:1000`). The doc comment claims it is "bounded by the finite set of IANA ids a tenant's calendar events reference", but the key is whatever the caller passes, and the production caller is the item render path `src/ews.rs:1485` passing `item.timezone` — the stored TZID of a calendar event, which an authenticated client supplies when it creates the event. A client that creates events with distinct bogus `TZID`s (each resolving to `None`, which is also cached) grows this map without bound for the whole process, across all users. Secondary cost: a single global `Mutex` taken twice per lookup, on a per-event render path, serializing timezone conversion across all worker threads.

Change: bound the cache (Moka with capacity, or an `LruCache`), and key it on the canonical IANA id after resolution — `None` results should not be memoized at all, since the failure is cheap to recompute and is exactly the attacker-chosen key space. Replacing the `Mutex<HashMap>` with `moka::sync::Cache` also removes the double lock acquisition. Effect: bounded memory under hostile input, less lock contention on calendar render. Risk: low — the memo is a pure function of its key. Confidence: proven by inspection.

### P3 — micro-optimizations and build hygiene

**P3-1. `sort_by_key(|(k, _)| k.to_string())` in `src/jscard.rs:367, 387, 428, 640`.** `slice::sort_by_key` recomputes the key on every comparison, so this allocates O(n log n) `String`s. Use `sort_unstable_by(|a, b| a.0.cmp(&b.0))` (order of equal keys is irrelevant for vCard property emission that is re-sorted by the consumer).

**P3-2. `normalize_email` (`src/util.rs:120-134`)** lowercases twice (`trimmed.to_lowercase()` and `stripped.nfc().collect::<String>().to_lowercase()`), allocating three `String`s. Add an ASCII fast path (`if trimmed.is_ascii() { return trimmed.to_ascii_lowercase() }`) and return `Cow<'_, str>`. Called on every attendee/recipient comparison in calendar and recipient resolution.

**P3-3. `JmapClient::mail_properties()` (`src/jmap.rs:1561-1590`)** returns a freshly built `Vec<&'static str>` on each of its three call sites (`:2383`, `:2840`, `:2913`), which is then serialized to a `serde_json::Value`. Make it `const MAIL_PROPERTIES: &[&str]` and serialize a `&[&str]`.

**P3-4. `JmapClient::get_session` (`src/jmap.rs:2172-2182`)** returns `Ok(entry.value().1.clone())` — a full `JmapSession` clone (including `accounts`/`primary_accounts` maps) on every JMAP method call. Cache and return `Arc<JmapSession>`.

**P3-5. Seventeen declared-but-unused direct dependencies.** Verified by scanning every `src/**/*.rs`, `benches/*.rs` and `tests/*.rs` for each name: `axum-extra`, `base64ct`, `tower`, `futures-core`, `compact_str`, `flate2`, `pin-project-lite`, `http-body-util`, `arc-swap`, `memchr`, `itoa`, `ryu`, `smartstring`, `bstr`, `rustls-pki-types`, `lzxd`, `compressed-rtf`. `compressed-rtf` appears only in comments describing unimplemented RTF compression (`src/mapi/store.rs:553-601`) and `lzxd` has no mention anywhere in `src/`, `benches/` or `tests/`. `memchr` should not be dropped — P1-6 uses it. Effect: fewer direct-dependency edges to resolve, compile and keep version-pinned (crates still pulled in transitively, such as `compact_str` via another dependency, remain in the graph); no runtime change.

**P3-6. `harvest_and_store` runs on every outbound send.** `src/email.rs:3253` calls `crate::smime::harvest_and_store`, which parses the whole MIME with `mail-parser` and every certificate with `cms`/`x509-cert` (`src/smime.rs:271-305`) for each `SendMail`/`SmartReply`/`SmartForward`, including large attachments. Gate on a cheap scan for `application/pkcs7-mime` / `multipart/signed` before the full parse.

**P3-7. Non-UTF-8-breaking but tooling-hostile control byte.** `src/recorded_client_contract.rs:852` contains a literal `NUL` inside a `//` comment (after `cn=user`). Rust accepts it (U+0000 is valid UTF-8), but it makes the file "binary" to `grep`/`rg` and can confuse editors and diff tooling. Replace with a normal character.

## Rust 1.99.1-specific opportunities

These are the edition-2024 / 1.99.1 features that apply directly to the findings above.

| Feature | Where it applies | Note |
| --- | --- | --- |
| `std::fmt::Write` (`write!`/`writeln!` into `String`) | P1-8 (280 sites), P1-9/10 | Removes the temporary `String` per append. `String`'s `Write` impl is infallible in practice; `let _ =` or `.expect()` is idiomatic. |
| `slice::sort_unstable_by` | P3-1 | `sort_by_key` recomputes keys per comparison; `sort_unstable_by` compares borrowed fields directly. |
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
- **A blanket `sort_unstable` migration.** Most sorts here are over user-visible ordered lists (folders, reminders) where stability is part of the wire contract. Only P3-1 is justified because the key order is re-derived by the consumer.
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
