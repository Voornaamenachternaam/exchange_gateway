# Exchange Gateway — Contributor Guide

## Build & Test
```bash
cargo check          # Compile (Rust 1.95.0, edition 2024)
cargo test           # 70 unit + integration + snapshot tests
cargo clippy         # Lint (0 warnings — clean)
cargo build --release # Production build
```

## Architecture
- **Protocol adapters**: `ews.rs` (EWS SOAP), `eas.rs` (EAS WBXML), `caldav.rs` (CalDAV)
- **Calendar/iCal**: `calendar.rs`, `ical_parser.rs`, `meeting/` (scheduling, message, attendee, state)
- **WBXML codec**: `wbxml.rs` (EAS binary XML, compile-time `phf::Map` lookup tables)
- **Storage**: `storage.rs` (SQLite via sqlx), `permission/` (delegate permissions)
- **Sync**: `sync.rs` (EAS sync protocol), `ews_folders.rs`, `ews_update.rs`
- **Utilities**: `util.rs` (XML escape, iCal text escape, NFC normalize, email normalize+validate, path sanitize, UTF-8 safe truncation)
- **Timezone**: `timezone.rs` (IANA ↔ Windows mapping via `windows-timezones` crate, `chrono::Offset` for bias computation)

## Key Dependencies
| Dep | Version | Purpose |
|-----|---------|---------|
| quick-xml | 0.39.2 | XML parsing + `escape::escape()`/`partial_escape()` for XML encoding |
| phf | 0.12.1 | Compile-time perfect hash maps for WBXML tag lookup (zero startup cost) |
| icalendar | 0.17.10 | RFC 5545 iCal parser/builder; `parser::unfold()` for line unfolding |
| unicode-normalization | 0.1.25 | NFC normalization for email/string comparison |
| rrule | 0.14.0 | Recurrence rule parsing (shared with icalendar) |
| windows-timezones | 0.5.1 | IANA to Windows timezone mapping (`WindowsTimezone` enum, `TryFrom<Tz>`, `FromStr`, `IntoEnumIterator`) |
| email_address | 0.2.9 | RFC-compliant email validation in `normalize_email()` |
| nom | 8.0.0 | iCal property line parser, email parsing |
| chrono / chrono-tz | latest | DateTime with timezone support; `Offset::fix()` for UTC offset extraction |
| axum | 0.8.x | HTTP framework |
| sqlx | 0.8.x | Async SQLite |
| secrecy | 0.10 | SecretString for config secrets (worker_secret, hmac_secret) |
| zeroize | 1.8 | Zeroizing config file reads to clear memory |

## Dependency Decisions
- **`icu_normalizer`**: REJECTED. 50+ crate tree for zero functional gain over `unicode-normalization`.
- **`calcard`**: REMOVED. Dead dependency (never imported in any .rs file).
- **`structured-email-address`**: REJECTED. v0.0.5 / 80 downloads — too immature for production.
- **`axum-auth`**: REJECTED. Only ~50 lines of custom basic auth; EAS needs zeroize/SecretString.
- **`icalendar` full integration**: Phase 2. Builder/parser can replace render_ics/generate_ical/parse_ics, but requires careful CalendarItem ↔ Event adapter mapping.
- **`sanitize-filename`**: REMOVED. Different semantics (replaces special chars with `-`/`_`, truncates to 255 chars); our `sanitize_path_segment` allows `.` and uses `_` consistently.
- **`validator`**: REMOVED. Not used directly; Config validation is custom logic (SecretString fields, placeholder detection, URL scheme checks) that validator derives cannot express.
- **`icalendar::escape_text`**: Private API (`pub(crate)`), not usable. Our `escape_ical_text` adds `\r` stripping (Windows line endings).

## Code Patterns
- XML escaping: use `crate::util::xml_escape()` / `xml_escape_text()` — delegates to `quick_xml::escape`
- iCal text escaping: use `crate::util::escape_ical_text()` — char-by-char, handles `\r` stripping
- iCal unfolding: use `crate::ical_parser::unfold_ical_content()` — delegates to `icalendar::parser::unfold`
- Email normalization: use `crate::util::normalize_email()` — strips mailto:, NFC, lowercases + `email_address` RFC validation
- UTF-8 safe truncation: use `crate::util::truncate_string()` — char_indices boundary-safe with "..."
- WBXML: static `TAG_TO_NAME`/`NAME_TO_TAG` via `phf::Map` (compile-time, no LazyLock)
- Timezone IANA to Windows: use `WindowsTimezone::try_from(Tz)` — from `windows-timezones` crate
- Timezone Windows to IANA: use `WindowsTimezone::from_str().tzdb_id()` — from `windows-timezones` crate
- UTC offset extraction: use `chrono::Offset::fix().local_minus_utc()` on `TzOffset`

## Security
- **HTTPS enforcement**: `forwarded_https_enforced()` checks `x-forwarded-proto` header in EWS/EAS
- **HSTS**: `Strict-Transport-Security: max-age=63072000; includeSubDomains` on all responses
- **Cache-Control**: `private, no-store` + `Pragma: no-cache` on all EAS responses
- **Secret handling**: Config secrets use `SecretString` with `Zeroizing` file reads; `skip_serializing`
- **Placeholder detection**: Config validation rejects `REPLACE_*` prefixed secrets at startup
- **Error messages**: All client-facing error responses use generic messages; internal details logged via `tracing::error!`
- **No unsafe code**: Zero `unsafe` blocks in the codebase
- **No panics in production**: No `todo!`, `unimplemented!`, or `panic!` in non-test code

## Warnings
- `is_stub_action` in ews.rs: suppressed with `#[allow(dead_code)]` (compile-time stub detection, may be used later)
- `folded_line` in meeting/message.rs: suppressed with `#[allow(dead_code)]` (convenience wrapper for fold_ical_line)

## Type Aliases
- `PropertyLine` / `VeventProps` / `NomError` in `ical_parser.rs`: reduce clippy type_complexity
- `DeviceInfo` in `eas.rs`: parsed device information tuple
- `TzParams` in `timezone.rs`: tuple of `(i32, String, String, (u8,u8,u8,u8), (u8,u8,u8,u8), i32, i32)`

## FromStr Implementations
- `DistinguishedFolder::from_str` → `impl FromStr` (ews_folders.rs)
- `PermissionLevel::from_str` → `impl FromStr` (permission/types.rs)
- `DelegatePermission::from_str` → `impl FromStr` (permission/types.rs)
- `WindowsTimezone::from_str` → from `windows-timezones` crate (used in timezone.rs)
