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
