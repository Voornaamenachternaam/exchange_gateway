# Exchange Gateway — Contributor Guide

## Build & Test
```bash
cargo check          # Compile (Rust 1.95.0, edition 2024)
cargo test           # 68 unit + integration + snapshot tests
cargo clippy         # Lint (2 known dead_code warnings: is_stub_action, folded_line)
```

## Architecture
- **Protocol adapters**: `ews.rs` (EWS SOAP), `eas.rs` (EAS WBXML), `caldav.rs` (CalDAV)
- **Calendar/iCal**: `calendar.rs`, `ical_parser.rs`, `meeting/` (scheduling, message, attendee, state)
- **WBXML codec**: `wbxml.rs` (EAS binary XML, compile-time `phf::Map` lookup tables)
- **Storage**: `storage.rs` (SQLite via sqlx), `permission/` (delegate permissions)
- **Sync**: `sync.rs` (EAS sync protocol), `ews_folders.rs`, `ews_update.rs`
- **Utilities**: `util.rs` (XML escape via quick_xml, NFC normalize, email normalize, path sanitize)
- **Timezone**: `timezone.rs` (IANA ↔ Windows mapping via windows-timezones)

## Key Dependencies
| Dep | Version | Purpose |
|-----|---------|---------|
| quick-xml | 0.39.2 | XML parsing + `escape::escape()`/`partial_escape()` for XML encoding |
| phf | 0.12.1 | Compile-time perfect hash maps for WBXML tag lookup (zero startup cost) |
| icalendar | 0.17.10 | RFC 5545 iCal parser/builder; `parser::unfold()` for line unfolding |
| unicode-normalization | 0.1.25 | NFC normalization for email/string comparison |
| rrule | 0.14.0 | Recurrence rule parsing (shared with icalendar) |
| windows-timezones | 0.5.1 | IANA ↔ Windows timezone ID mapping |
| nom | 8.0.0 | iCal property line parser, email parsing |
| chrono / chrono-tz | latest | DateTime with timezone support |
| axum | 0.8.x | HTTP framework |
| sqlx | 0.8.x | Async SQLite |

## Dependency Decisions
- **`icu_normalizer`**: REJECTED. 50+ crate tree for zero functional gain over `unicode-normalization`.
- **`calcard`**: REMOVED. Dead dependency (never imported in any .rs file).
- **`structured-email-address`**: REJECTED. v0.0.5 / 80 downloads — too immature for production.
- **`axum-auth`**: REJECTED. Only ~50 lines of custom basic auth; EAS needs zeroize/SecretString.
- **`icalendar` full integration**: Phase 2. Builder/parser can replace render_ics/generate_ical/parse_ics, but requires careful CalendarItem ↔ Event adapter mapping.

## Code Patterns
- XML escaping: use `crate::util::xml_escape()` / `xml_escape_text()` — delegates to `quick_xml::escape`
- iCal unfolding: use `crate::ical_parser::unfold_ical_content()` — delegates to `icalendar::parser::unfold`
- Email normalization: use `crate::util::normalize_email()` — strips mailto:, NFC, lowercases
- WBXML: static `TAG_TO_NAME`/`NAME_TO_TAG` via `phf::Map` (compile-time, no LazyLock)
- iCal text escaping: 3 duplicate `escape_ical_text` impls (calendar.rs, meeting/message.rs, meeting/scheduling.rs) — Phase 2 candidate for icalendar consolidation

## Warnings
- `is_stub_action` in ews.rs: dead code, safe to ignore
- `folded_line` in meeting/message.rs: dead code, safe to ignore