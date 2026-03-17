# GAP_ANALYSIS.md

## Evaluation baseline

- Source-of-truth protocol corpus: `Binder1.txt`.
- Evaluated code: Rust gateway (`src/*.rs`), Cloudflare worker (`worker/index.js`), D1 schema (`d1_schema.sql`), and existing tests (`tests/*`, unit tests in `src/*`).
- Target profile: Outlook for Windows 11 + Outlook for Android 15 calendar interoperability against Stalwart Mailserver v0.15.5 with basic auth.

## What is now fully covered for the stated use-case

1. **Exchange calendar profile flow coverage (EAS + EWS + autodiscover)**
   - EAS and EWS endpoints are implemented in Rust.
   - Worker now serves XML/SOAP/JSON autodiscover, and can forward `/EWS/*` and `/Microsoft-Server-ActiveSync*` to the Rust origin.
   - This closes the previous profile orchestration gap for this exact calendar-focused deployment profile.

2. **Edge controls in free Cloudflare footprint**
   - Worker now includes edge request limiting for EWS/EAS with KV-backed counters.
   - Secret-based gateway API authorization and idempotency handling are in place.
   - This closes the previous missing edge-rate-control gap for this deployment shape.

## Remaining gaps (up-to-date)

1. **Full Exchange parity beyond calendar-focused profile is still open**
   - Binder1 includes broader protocol families and branch permutations not required by this use-case.
   - Non-calendar Outlook workflows and deeper property/behavior permutations are not fully implemented.

2. **Exhaustive conformance evidence across all Binder1 negative permutations is still open**
   - Existing tests validate many operation shapes and selected negative paths.
   - A full machine-generated MUST/SHOULD matrix for every Binder1 branch is not yet present.

3. **Long-run production evidence remains open**
   - Multi-week soak, failure-injection, and controlled migration/rollback artifacts are not yet stored as repeatable CI evidence in this repository.

## Definition of Done for your specific use-case

A release is done when all conditions below are true for your environment:

1. Outlook Windows 11 and Outlook Android 15 can configure accounts natively (no client plugins) using autodiscover and basic auth.
2. Calendar operations (create/update/delete/sync/meeting response in the implemented profile) converge correctly across both clients.
3. Worker + D1 + tunnel + Rust gateway are configured exactly as documented and validated by endpoint checks.
4. Sync/provision and mapping state remain consistent across process restarts and transient retries.
5. TLS termination and request-shaping controls are active at Cloudflare edge and verified in production.
6. Traceability from Binder1 requirement families to code paths and tests remains current.

## Dependency recommendation

No additional runtime dependencies are required to satisfy this use-case now. Keep the current dependency set and prioritize protocol test-depth and operational evidence over adding crates.

