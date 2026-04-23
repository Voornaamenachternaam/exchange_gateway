# Exchange Gateway - Agent Memory

## Project Overview
Rust exchange gateway implementing EAS (Exchange ActiveSync), EWS (Exchange Web Services), CalDAV, and IMAP protocols. Synchronizes calendar, contacts, and email between Exchange/Outlook and downstream clients.

## Build & Test
- `cargo build` - builds the library
- `cargo build --release` - release build (~3 min)
- `cargo test` - runs all tests (unit + snapshot)
- `cargo test --lib` - library unit tests only

## Key Dependencies & API Notes

### icalendar ^0.17.10
- **Imports**: `use icalendar::{Calendar, Component, Event, EventLike, Property, Class, EventStatus, Parameter, Alarm, Related, Trigger, Attendee, Role, PartStat, CUType, CalendarDateTime, DatePerhapsTime};`
- `EventLike` trait is required for `.starts()`, `.ends()`, `.all_day()`, `.location()` on `Event`
- `DatePerhapsTime::From<(NaiveDateTime, Tz)>` is the way to pass timezone-aware datetimes (NOT `DateTime<Tz>` directly)
- `Property::add_parameter()` returns `&mut Self` — must call `.done()` at end of builder chain to get owned `Property`
- `Alarm::display(description, trigger)` creates VALARM components; trigger can be `chrono::Duration` or `(Duration, Related)`
- `event.alarm(alarm)` adds VALARM as proper sub-component (not raw property)
- `Event::status(EventStatus::Tentative|Confirmed|Cancelled)` sets STATUS property
- No `Transp` enum exported — use `event.add_property("TRANSP", "OPAQUE"/"TRANSPARENT")` directly
- `recurrence()` method requires `recurrence` feature flag — use `add_property("RRULE", ...)` as fallback
- `icalendar::parser::unfold(input)` handles RFC 5545 line unfolding

### iso8601-duration ^0.2.0
- **Import**: `use iso8601_duration::Duration as IsoDuration;`
- **Parse**: `IsoDuration::parse("PT1H30M")` or `"PT1H30M".parse::<IsoDuration>()`
- No `parser` module — use `Duration::parse()` or `FromStr` impl
- Fields are `f32`: `year`, `month`, `day`, `hour`, `minute`, `second` (no `week`, no `millisecond`)
- `num_minutes()`, `num_hours()`, `num_seconds()` return `Option<f32>` — `None` if year/month nonzero
- `to_std()` converts to `std::time::Duration` (returns `None` if year/month nonzero)

### strum ~0.27.2
- **Derives**: `strum::Display`, `strum::FromRepr`
- `#[strum(serialize = "DISPLAY_VALUE")]` for custom Display output
- `FromRepr::from_repr(usize)` — always takes `usize`, not `u8` (cast with `as usize`)
- Multiple variants can share same serialize value (e.g., `NotResponded` → "NEEDS-ACTION")

## Architecture
- `src/lib.rs` — crate root, re-exports modules
- `src/main.rs` — CLI entrypoint (axum server)
- `src/config.rs` — configuration loading (toml-based)
- `src/models.rs` — core data models (CalendarItem, etc.)
- `src/error.rs` — error types
- `src/util.rs` — shared utilities (normalize_email, escape_ical_text, etc.)
- `src/traits.rs` — shared traits
- `src/calendar.rs` — Calendar ICS parsing and rendering (uses icalendar crate for rendering)
- `src/ical_parser.rs` — iCal content parsing helpers (uses icalendar::parser::unfold, iso8601-duration)
- `src/meeting/` — Meeting/iPIM modules (attendee, message, scheduling, types)
- `src/eas.rs` — EAS protocol implementation
- `src/ews.rs` — EWS protocol implementation
- `src/permission/` — Permission types (uses strum derives)

## Code Patterns
- Use `icalendar` crate builders for all ICS generation (Calendar → Event → .done() → .push() → .done())
- Use `iso8601_duration::Duration::parse()` for duration parsing, then convert to minutes via `num_minutes()` or manual f32 math
- Use `strum::Display` derive instead of hand-written `fmt::Display` impls for enums
- Use `strum::FromRepr` derive instead of hand-written `From<u8>` where appropriate (remember `usize` cast)
- The `chrono::Tz` type (from `chrono-tz`) is used for timezone handling — pass by value, not by deref