# GAP_ANALYSIS.md

## Protocol source of truth

All protocol conclusions below are scoped to `Binder1.txt` only.

## Definition of Done status (specific use-case)

1. **Native account setup in Outlook Windows 11 + Android 15 via autodiscover/basic auth**
   - **Implemented:** YES

2. **Calendar create/update/delete/sync/meeting-response convergence for implemented profile**
   - **Implemented:** YES

3. **Worker + D1 + tunnel + gateway deployment profile documented and endpoint-verifiable**
   - **Implemented:** YES

4. **Sync/provision/item mapping consistency across retries/restarts**
   - **Implemented:** YES

5. **TLS termination + request-shaping controls active and verified in production runtime**
   - **Implemented:** NO (requires live deployment evidence, not repository-only evidence)

6. **Binder1 family traceability to code/tests remains current**
   - **Implemented:** YES

### DoD implementation summary

- **Implemented:** items **1, 2, 3, 4, 6** (5 of 6)
- **Not fully implemented in-repo:** item **5**

## Up-to-date remaining gaps

1. Production runtime evidence bundle for item 5 is not yet present as repeatable artifacts in this repository.
2. Full non-calendar Exchange parity from Binder1 remains intentionally out of scope for this calendar-focused deployment.

## xsd-parser dependency recommendation

### Recommendation

Do **not** integrate `xsd-parser` into the runtime path for this use-case.

### Why

1. Your use-case is calendar-focused EAS/EWS interoperability, not full Exchange family parity.
2. `xsd-parser` does not by itself provide a complete production-grade runtime conformance gate for all Binder1 branches and Outlook behavior permutations.
3. It would increase dependency and maintenance surface without closing the highest-priority open gap (live production verification evidence for DoD item 5).
4. Current gateway validation + operation-scoped checks are aligned to the implemented profile, and improvement priority should remain test depth and deployment evidence.

### Decision applied in repository

- Removed optional `xsd-parser` dependency and the unused `xsd-validation` feature from `Cargo.toml`.

