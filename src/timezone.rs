// src/timezone.rs
use base64::{Engine, engine::general_purpose::STANDARD as BASE64};
use chrono::{Datelike, Offset, TimeZone};
use chrono_tz::Tz;
use std::collections::HashMap;
use std::str::FromStr;
use std::sync::{LazyLock, Mutex};
use strum::IntoEnumIterator;
use windows_timezones::WindowsTimezone;

pub(crate) const TZ_BLOB_LEN: usize = 172;

pub fn decode_eas_timezone_bias(b64: &str) -> Option<i32> {
    let bytes = BASE64.decode(b64.trim()).ok()?;
    if bytes.len() < 4 {
        return None;
    }
    Some(i32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
}

fn read_wchar_name(bytes: &[u8], offset: usize) -> String {
    let end = offset + 64;
    if end > bytes.len() {
        return String::new();
    }
    let chars: Vec<u16> = bytes[offset..end]
        .as_chunks::<2>()
        .0
        .iter()
        .map(|&c| u16::from_le_bytes(c))
        .take_while(|&c| c != 0)
        .collect();
    String::from_utf16_lossy(&chars).to_string()
}

fn write_wchar_name(blob: &mut [u8], offset: usize, name: &str) {
    let mut pos = offset;
    for ch in name.encode_utf16().take(32) {
        if pos + 2 > blob.len() {
            break;
        }
        let b = ch.to_le_bytes();
        blob[pos] = b[0];
        blob[pos + 1] = b[1];
        pos += 2;
    }
}

fn find_windows_timezone(name: &str) -> Option<WindowsTimezone> {
    let n = name.trim();
    if n.is_empty() {
        return None;
    }

    if let Ok(tz) = WindowsTimezone::from_str(n) {
        return Some(tz);
    }

    for variant in WindowsTimezone::iter() {
        if variant.name().eq_ignore_ascii_case(n) {
            return Some(variant);
        }
    }

    let n_lower = n.to_ascii_lowercase();
    WindowsTimezone::iter()
        .filter(|variant| {
            let name = variant.name();
            n_lower.contains(&name.to_ascii_lowercase())
        })
        .max_by_key(|variant| variant.name().len())
}

fn windows_name_to_iana(name: &str) -> Option<String> {
    let n = name.trim();
    if n.is_empty() {
        return None;
    }

    if let Some(tz) = parse_utc_offset_name(n) {
        return Some(tz);
    }

    find_windows_timezone(n).map(|tz| tz.tzdb_id().to_string())
}

pub fn eas_timezone_blob_to_iana(b64: &str) -> Option<String> {
    let bytes = BASE64.decode(b64.trim()).ok()?;
    if bytes.len() < TZ_BLOB_LEN {
        return None;
    }
    let bias = i32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
    let std_name = read_wchar_name(&bytes, 4);
    let dst_name = read_wchar_name(&bytes, 88);

    if let Some(iana) = windows_name_to_iana(&std_name).or_else(|| windows_name_to_iana(&dst_name))
    {
        return Some(iana);
    }

    if bias == 0 {
        return Some("UTC".to_string());
    }
    let hours = bias / 60;
    Some(format!("Etc/GMT{:+}", hours))
}

pub fn eas_timezone_blob_to_tz(b64: &str) -> Option<Tz> {
    let iana = eas_timezone_blob_to_iana(b64)?;
    iana.parse().ok()
}

pub fn windows_timezone_name_to_tz(name: &str) -> Option<Tz> {
    if let Some(iana) = parse_utc_offset_name(&name.to_ascii_lowercase()) {
        return iana.parse().ok();
    }
    find_windows_timezone(name).map(Tz::from)
}

/// Convert an Outlook/Windows timezone display name (e.g. "Pacific Standard Time")
/// into its canonical IANA identifier (e.g. "America/Los_Angeles").
///
/// Outlook EWS `StartTimeZone`/`MeetingTimeZone` and EAS `TimezoneName` carry
/// Windows timezone names; Stalwart and the icalendar crate require IANA names.
/// Returns `None` for unrecognised names so callers can fall back to UTC.
pub fn windows_timezone_name_to_iana(name: &str) -> Option<String> {
    let n = name.trim();
    if n.is_empty() {
        return None;
    }
    if let Some(iana) = parse_utc_offset_name(&n.to_ascii_lowercase()) {
        return Some(iana.to_string());
    }
    find_windows_timezone(n).map(|tz| tz.tzdb_id().to_string())
}

/// Convert an IANA timezone identifier back to the Windows timezone display
/// name Outlook expects in `StartTimeZone`/`EndTimeZone`. Returns `None` for
/// unrecognised identifiers.
pub fn iana_to_windows_timezone_name(iana: &str) -> Option<String> {
    iana_to_windows_params(iana).map(|(_, win_name, _, _, _, _, _)| win_name)
}

pub fn iana_to_eas_timezone_blob(iana: &str) -> Option<String> {
    let (bias, std_name, dst_name, std_date, dst_date, std_bias, dst_bias) =
        iana_to_windows_params(iana)?;
    let mut blob = [0u8; TZ_BLOB_LEN];
    blob[0..4].copy_from_slice(&bias.to_le_bytes());
    write_wchar_name(&mut blob, 4, &std_name);
    blob[68..84].copy_from_slice(&std_date);
    blob[84..88].copy_from_slice(&std_bias.to_le_bytes());
    write_wchar_name(&mut blob, 88, &dst_name);
    blob[152..168].copy_from_slice(&dst_date);
    blob[168..172].copy_from_slice(&dst_bias.to_le_bytes());
    Some(BASE64.encode(blob))
}

fn parse_utc_offset_name(name: &str) -> Option<String> {
    let lower = name.to_ascii_lowercase();
    if !lower.contains("utc") && !lower.contains("gmt") {
        return None;
    }
    let bytes = lower.as_bytes();
    for i in 0..bytes.len().saturating_sub(2) {
        let sign = match bytes[i] {
            b'+' => "-",
            b'-' => "+",
            _ => continue,
        };
        if bytes[i + 1].is_ascii_digit() && bytes[i + 2].is_ascii_digit() {
            let hours: i32 = (bytes[i + 1] - b'0') as i32 * 10 + (bytes[i + 2] - b'0') as i32;
            if (1..=12).contains(&hours) {
                return Some(format!("Etc/GMT{}{}", sign, hours));
            }
        }
    }
    None
}

const NO_DST: [u8; 16] = [0u8; 16];

pub type TzParams = (i32, String, String, [u8; 16], [u8; 16], i32, i32);

/// Windows `TZI` SYSTEMTIME layout (16 bytes, little-endian WORDs):
/// wYear(2) wMonth(2) wDayOfWeek(2) wDay(2) wHour(2) wMinute(2) wSecond(2) wMs(2).
/// A `wYear` of 0 marks a *rule-based* transition parametrised by
/// (month, weekday, week-of-month, hour): `wDay` ∈ 1..=4 = the nth weekday,
/// 5 = the last weekday of the month; `wDayOfWeek` ∈ 0..=6 = Sunday..Saturday.
fn systemtime_rule(month: u16, weekday: u16, week: u16, hour: u16) -> [u8; 16] {
    let mut b = [0u8; 16];
    // wYear = 0 (rule-based, not a fixed year)
    b[2..4].copy_from_slice(&month.to_le_bytes());
    b[4..6].copy_from_slice(&(weekday & 0x07).to_le_bytes());
    b[6..8].copy_from_slice(&week.to_le_bytes());
    b[8..10].copy_from_slice(&hour.to_le_bytes());
    b
}

/// Project a `chrono_tz::Tz` offset (in minutes east of UTC) for a naive
/// local datetime, choosing the *earliest* disambiguation at a fold and
/// `None` in a gap (the gateway never interprets a transition instant as a
/// wall-clock time the offset is undefined for).
fn local_offset_minutes(tz: Tz, ndt: chrono::NaiveDateTime) -> Option<i32> {
    match tz.from_local_datetime(&ndt) {
        chrono::LocalResult::Single(dt) => Some(dt.offset().fix().local_minus_utc() / 60),
        chrono::LocalResult::Ambiguous(dt, _) => Some(dt.offset().fix().local_minus_utc() / 60),
        chrono::LocalResult::None => None,
    }
}

/// Locate the wall-clock hour (0..=23) at which the zone offset first leaves
/// `from` on `date`, robust to both gaps (spring-forward, where the transition
/// hour does not exist as a local time) and folds (fall-back, where it repeats).
/// The Windows `TZI` `DaylightDate`/`StandardDate` `wHour` is documented as the
/// instant the offset changes, expressed in the **outgoing** phase's wall
/// clock — i.e. the naive hour at which the offset stops being `from`. For a
/// spring-forward gap that hour is the (non-existent) gap start (e.g. 02:00
/// Eastern, 01:00 GMT); for a fall-back fold it is the first naive hour that
/// is exclusively in the new phase. The result is the literal naive boundary
/// hour in `0..=23` (a `SYSTEMTIME` `wHour` legitimately carries `0`, so zones
/// that change at midnight, e.g. America/Santiago's autumn resume, are encoded
/// correctly rather than shifted an hour late).
fn transition_hour(tz: Tz, date: chrono::NaiveDate, from: i32, _to: i32) -> Option<u16> {
    let mut prev: Option<i32> = Some(from);
    let mut hour: u16 = 0;
    while hour < 24 {
        let ndt = date.and_hms_opt(hour.into(), 0, 0)?;
        let cur = local_offset_minutes(tz, ndt);
        // A flip is detected when the offset leaves `from` — either by becoming
        // the new offset, or by dropping into a gap (None) right after an
        // `from` hour (spring-forward).
        if prev == Some(from) && cur != Some(from) {
            return Some(hour);
        }
        prev = cur;
        hour += 1;
    }
    None
}

/// Encode a transition *rule* for a specific detected boundary date. The
/// weekday and week are derived so the rule generalises to any year following
/// the same monthly pattern (e.g. "second Sunday of March at 02:00"). `from` and
/// `to` are the offsets (minutes east of UTC) bracketing the flip.
fn encode_boundary(
    tz: Tz,
    date: chrono::NaiveDate,
    month: u32,
    year: i32,
    from: i32,
    to: i32,
) -> Option<[u8; 16]> {
    let weekday = date.weekday().num_days_from_sunday() as u16;
    let dim = month_days(year, month);
    // Windows TZI wDay: 1..=4 = nth weekday of the month, 5 = last weekday.
    let week = if date.day() + 7 > dim {
        5
    } else {
        date.day().div_ceil(7).min(4) as u16
    };
    let hour = transition_hour(tz, date, from, to)?;
    Some(systemtime_rule(month as u16, weekday, week, hour))
}

/// Derive the Windows `TZI` standard & daylight transition SYSTEMTIME records
/// by sampling the zone's actual offsets across a reference year with
/// `chrono_tz`. This honours the authoritative IANA transition rules
/// byte-for-byte (e.g. southern-hemisphere DST, zones whose transition dates
/// differ from the legacy EU/US approximation, and zones with no DST),
/// replacing the old hardcoded per-region guesswork.
fn derive_tzi_transitions(tz: Tz, standard: i32, dst: i32) -> ([u8; 16], [u8; 16]) {
    const REF_YEAR: i32 = 2025;
    let (std_rule, dst_rule) = scan_full_year(tz, REF_YEAR, standard, dst);
    if std_rule == NO_DST || dst_rule == NO_DST {
        // A zone flagged as DST-observing that we failed to resolve (e.g. the
        // reference year fell entirely on one side of a DST change) degrades to
        // a zeroed TZI rather than emitting a half-populated, malformed blob.
        return (NO_DST, NO_DST);
    }
    (std_rule, dst_rule)
}

/// Walk every day of `year` at 12:00 local, record the standard->dst boundary
/// (first occurrence) and the dst->standard boundary (last occurrence), then
/// encode both as Windows `TZI` SYSTEMTIME rules. Deriving the transition dates
/// directly from `chrono_tz`'s compiled zoneinfo means the synthesised blob is
/// byte-for-byte correct for the zone's actual (possibly non-EU/US, possibly
/// southern-hemisphere) DST rules. The walk stops at the year boundary so a
/// January-1 boundary of the following year is never mis-encoded with this
/// year's month/week.
fn scan_full_year(tz: Tz, year: i32, standard: i32, dst: i32) -> ([u8; 16], [u8; 16]) {
    let mut std_rule = NO_DST;
    let mut dst_rule = NO_DST;
    let mut prev_offset: Option<i32> = None;
    let mut date = chrono::NaiveDate::from_ymd_opt(year, 1, 1).unwrap();
    while let Some(ndt) = date.and_hms_opt(12, 0, 0) {
        let off = local_offset_minutes(tz, ndt);
        if let (Some(prev), Some(cur)) = (prev_offset, off) {
            if prev == standard
                && cur == dst
                && dst_rule == NO_DST
                && let Some(rule) = encode_boundary(tz, date, date.month(), year, standard, dst)
            {
                dst_rule = rule;
            } else if prev == dst
                && cur == standard
                && let Some(rule) = encode_boundary(tz, date, date.month(), year, dst, standard)
            {
                // Keep the LAST dst->standard boundary of the year so the rule
                // matches the zone's autumn transition even when a spring one
                // was recorded first in a southern-hemisphere order.
                std_rule = rule;
            }
        }
        prev_offset = off;
        let Some(next) = date.succ_opt() else { break };
        // Stop before crossing into the next year so a New-Year boundary is
        // not sampled (and mis-encoded) under this year's month/week.
        if next.year() != year {
            break;
        }
        date = next;
    }
    (std_rule, dst_rule)
}

fn month_days(year: i32, month: u32) -> u32 {
    chrono::NaiveDate::from_ymd_opt(year, month + 1, 1)
        .or_else(|| chrono::NaiveDate::from_ymd_opt(year + 1, 1, 1))
        .map(|d| d.pred_opt().unwrap().day())
        .unwrap_or(28)
}

/// Zones that by definition never observe DST, kept as a fast path so the
/// synthesised blob carries a clean zeroed SYSTEMTIME (matching a no-DST
/// Windows TZI) without a needless year-long scan. Restricted to the
/// genuinely-fixed IANA categories — `UTC`, `Etc/*`, and `GMT` — so any zone
/// that *might* observe DST (Africa/Cairo resumed DST in 2023; many "currently
/// fixed" regional zones have a DST history under different rules) is computed
/// from its actual sampled offsets by `zone_transitions` instead of being
/// suppressed by a stale hard-coded list.
fn fixed_offset_zone(iana: &str) -> bool {
    iana == "UTC" || iana == "GMT" || iana.starts_with("Etc/")
}

/// Resolve the Windows timezone display name for an IANA id, robust to legacy
/// IANA aliases the `chrono_tz` enum still carries (e.g. `Asia/Kolkata`, the
/// canonical form of the deprecated `Asia/Calcutta` that the `windows-timezones`
/// `TryFrom<Tz>` mapping is keyed on). Falls back to iterating all Windows
/// timezones and matching the candidate whose `tzdb_id()` resolves to the
/// *same* zone as the input — comparing resolved local offsets at four
/// representative instants (one per season) rather than the raw `Tz` enum
/// discriminant, because `chrono_tz` exposes aliased ids (Asia/Kolkata vs
/// Asia/Calcutta) as *distinct* enum variants that nonetheless resolve to
/// identical offsets.
fn windows_timezone_name_for_iana(iana: &str, tz: Tz) -> Option<String> {
    if let Ok(w) = WindowsTimezone::try_from(tz) {
        return Some(w.name().to_string());
    }
    let sample_offset = |candidate: Tz, m: u32| -> Option<i32> {
        let ndt =
            chrono::NaiveDate::from_ymd_opt(2025, m, 15).and_then(|d| d.and_hms_opt(12, 0, 0))?;
        let dt = tz.from_local_datetime(&ndt).earliest()?;
        let cd = candidate.from_local_datetime(&ndt).earliest()?;
        // Equal iff both resolve and share the local-minus-UTC for this instant.
        Some((dt.offset().fix().local_minus_utc() == cd.offset().fix().local_minus_utc()) as i32)
    };
    let samples = [1u32, 4, 7, 10];
    for variant in WindowsTimezone::iter() {
        if let Ok(candidate) = variant.tzdb_id().parse::<Tz>()
            && samples
                .iter()
                .all(|&m| sample_offset(candidate, m) == Some(1))
        {
            return Some(variant.name().to_string());
        }
    }
    // Last resort: a case-insensitive name match (e.g. caller passes the
    // Windows name itself as `iana`).
    find_windows_timezone(iana).map(|w| w.name().to_string())
}

/// Resolved DST transition metadata for an IANA zone, derived by sampling the
/// zone's actual local offsets across a reference year with `chrono_tz`. This
/// is the single authoritative source for the Windows `TZI` blob AND the
/// synthesised iCalendar `VTIMEZONE` block, so the EAS/EWS rendering and the
/// CalDAV `render_ics` emission agree byte-for-byte on the same transition
/// boundaries (the audit gap: the per-event `StartTimeZone`/`EndTimeZone` was
/// built from a Windows-TZ→base64 mapping that did not preserve the
/// authoritative TZID/RRULE UNTIL boundaries CalDAV round-trips).
struct ZoneTransitions {
    standard_offset: i32,
    /// `None` when the zone observes no DST.
    dst_offset: Option<i32>,
    /// Windows `TZI` standard-resume SYSTEMTIME (or `NO_DST`).
    std_rule: [u8; 16],
    /// Windows `TZI` daylight-start SYSTEMTIME (or `NO_DST`).
    dst_rule: [u8; 16],
}

fn zone_transitions(iana: &str, tz: Tz) -> ZoneTransitions {
    let offsets: Vec<i32> = (1..=12)
        .filter_map(|month| {
            chrono::NaiveDate::from_ymd_opt(2025, month, 15)
                .and_then(|d| d.and_hms_opt(12, 0, 0))
                .and_then(|dt| dt.and_local_timezone(tz).earliest())
                .map(|dt| dt.offset().fix().local_minus_utc() / 60)
        })
        .collect();
    let Some(&standard_offset) = offsets.iter().min() else {
        return ZoneTransitions {
            standard_offset: 0,
            dst_offset: None,
            std_rule: NO_DST,
            dst_rule: NO_DST,
        };
    };
    let has_dst = offsets.iter().any(|&offset| offset != standard_offset);
    if !has_dst || fixed_offset_zone(iana) {
        return ZoneTransitions {
            standard_offset,
            dst_offset: None,
            std_rule: NO_DST,
            dst_rule: NO_DST,
        };
    }
    let dst_offset = standard_offset + 60;
    let (std_rule, dst_rule) = derive_tzi_transitions(tz, standard_offset, dst_offset);
    if std_rule == NO_DST || dst_rule == NO_DST {
        return ZoneTransitions {
            standard_offset,
            dst_offset: None,
            std_rule: NO_DST,
            dst_rule: NO_DST,
        };
    }
    ZoneTransitions {
        standard_offset,
        dst_offset: Some(dst_offset),
        std_rule,
        dst_rule,
    }
}

/// Build an RFC 5545 `VTIMEZONE` block for `iana` by sampling the zone's actual
/// offsets with `chrono_tz`, so a gateway-originated (EWS/MAPI) calendar item
/// that carries a Windows time-zone name but no authoritative CalDAV
/// `VTIMEZONE` still round-trips with a byte-for-byte-correct zone definition.
/// The `TZID` is the IANA id (canonical), matching what `render_ics` emits on
/// `DTSTART;TZID=…`; the `STANDARD`/`DAYLIGHT` subcomponents carry the
/// `TZOFFSETFROM`/`TZOFFSETTO`/`DTSTART`/`RRULE` derived from the same sampled
/// transition rules as the Windows `TZI` blob (so a client re-editing the
/// event on either transport sees identical boundaries).
pub fn render_vtimezone_block(iana: &str) -> Option<String> {
    let tz: Tz = iana.parse().ok()?;
    let zt = zone_transitions(iana, tz);
    let tzid = canonical_iana(iana);
    let mut out = String::with_capacity(256);
    out.push_str("BEGIN:VTIMEZONE\r\n");
    out.push_str(&format!("TZID:{tzid}\r\n"));

    let std_off = format_offset(zt.standard_offset);
    if let Some(dst_off) = zt.dst_offset {
        let dst_str = format_offset(dst_off);
        out.push_str("BEGIN:DAYLIGHT\r\n");
        out.push_str(&format!("TZOFFSETFROM:{std_off}\r\n"));
        out.push_str(&format!("TZOFFSETTO:{dst_str}\r\n"));
        out.push_str(&format!("DTSTART:{}\r\n", vtimezone_dtstart(zt.dst_rule)));
        out.push_str(&vtimezone_rrule(zt.dst_rule));
        out.push_str("END:DAYLIGHT\r\n");

        out.push_str("BEGIN:STANDARD\r\n");
        out.push_str(&format!("TZOFFSETFROM:{dst_str}\r\n"));
        out.push_str(&format!("TZOFFSETTO:{std_off}\r\n"));
        out.push_str(&format!("DTSTART:{}\r\n", vtimezone_dtstart(zt.std_rule)));
        out.push_str(&vtimezone_rrule(zt.std_rule));
        out.push_str("END:STANDARD\r\n");
    } else {
        // Fixed-offset zone: a single STANDARD subcomponent with no RRULE.
        out.push_str("BEGIN:STANDARD\r\n");
        out.push_str(&format!("TZOFFSETFROM:{std_off}\r\n"));
        out.push_str(&format!("TZOFFSETTO:{std_off}\r\n"));
        out.push_str("DTSTART:19700101T000000\r\n");
        out.push_str("END:STANDARD\r\n");
    }
    out.push_str("END:VTIMEZONE");
    // Defensive self-validation: reject any definition that is structurally
    // malformed (missing the mandatory VTIMEZONE framing or TZID) so the sole
    // caller (calendar.rs `render_ics`) can't receive an unusable block. The
    // caller additionally round-trips the block through the `icalendar` parser
    // and, when even that fails, falls back to a UTC `DTSTART` so no orphan
    // TZID is ever emitted (RFC 5545 invariant).
    if out.contains("BEGIN:VTIMEZONE\r\n") && out.contains("TZID:") && out.contains("END:VTIMEZONE")
    {
        Some(out)
    } else {
        None
    }
}

/// Canonicalise an IANA id for use as a `VTIMEZONE` `TZID`. `chrono_tz`
/// exposes a few aliased ids as distinct enum variants (Asia/Kolkata vs
/// Asia/Calcutta); prefer the modern canonical form.
fn canonical_iana(iana: &str) -> &str {
    match iana {
        "Asia/Calcutta" => "Asia/Kolkata",
        "US/Eastern" => "America/New_York",
        "US/Pacific" => "America/Los_Angeles",
        "US/Central" => "America/Chicago",
        "US/Mountain" => "America/Denver",
        other => other,
    }
}

/// Format a signed offset (minutes east of UTC) as an iCalendar UTC-OFFSET
/// (`+HHMM` / `-HHMM`).
fn format_offset(minutes: i32) -> String {
    let sign = if minutes < 0 { '-' } else { '+' };
    let m = minutes.abs();
    format!("{}{:02}{:02}", sign, m / 60, m % 60)
}

/// iCalendar weekday name for a Windows `TZI` wDayOfWeek (0=Sunday..6=Saturday).
fn weekday_name(wday: u16) -> &'static str {
    match wday {
        0 => "SU",
        1 => "MO",
        2 => "TU",
        3 => "WE",
        4 => "TH",
        5 => "FR",
        6 => "SA",
        _ => "SU",
    }
}

/// Resolve the `DTSTART` for a `VTIMEZONE` subcomponent from a Windows `TZI`
/// SYSTEMTIME rule. Per RFC 5545 §3.6.5 the `STANDARD`/`DAYLIGHT` `DTSTART` is
/// the transition's **local** wall-clock time (the instant the offset becomes
/// effective, expressed in that offset's own clock), anchored at the customary
/// epoch year `1970` and emitted as a naive date-time with **no** trailing `Z`
/// (UTC-suffixed values are explicitly forbidden here). The `RRULE` recurs it
/// annually, so the year is only a stable anchor for the first occurrence.
fn vtimezone_dtstart(rule: [u8; 16]) -> String {
    let month = u16::from_le_bytes([rule[2], rule[3]]) as u32;
    const EPOCH_YEAR: i32 = 1970;
    // nth weekday of the month (1..=4), or last (5).
    let day_of_week = u16::from_le_bytes([rule[4], rule[5]]);
    let week = u16::from_le_bytes([rule[6], rule[7]]);
    let hour = u16::from_le_bytes([rule[8], rule[9]]);
    let weekday = match_rule_weekday_of_month(EPOCH_YEAR, month, day_of_week, week);
    let Some(date) = chrono::NaiveDate::from_ymd_opt(EPOCH_YEAR, month, weekday) else {
        return "19700101T000000".to_string();
    };
    let Some(naive) = date.and_hms_opt(hour.into(), 0, 0) else {
        return "19700101T000000".to_string();
    };
    naive.format("%Y%m%dT%H%M%S").to_string()
}

fn match_rule_weekday_of_month(year: i32, month: u32, day_of_week: u16, week: u16) -> u32 {
    let first = chrono::NaiveDate::from_ymd_opt(year, month, 1)
        .unwrap_or_else(|| chrono::NaiveDate::from_ymd_opt(year, month.clamp(1, 12), 28).unwrap());
    let first_wd = first.weekday().num_days_from_sunday() as u16;
    let target = day_of_week % 7;
    let mut offset = (target + 7 - first_wd) % 7;
    let dim = month_days(year, month) as i32;
    if week == 5 {
        // last weekday of the month: advance by 4 weeks then clamp to the
        // month length.
        offset += 28;
        let mut day = 1 + offset as i32;
        while day > dim {
            day -= 7;
        }
        return day.max(1) as u32;
    }
    offset += (week.saturating_sub(1)) * 7;
    // Clamp to the month length so a malformed nth-week rule (e.g. the 4th
    // occurrence of a weekday that only occurs three times in February) never
    // yields an out-of-range day.
    (1 + offset as i32).min(dim).max(1) as u32
}

/// Emit the iCalendar `RRULE` for a Windows `TZI` SYSTEMTIME rule (BYDAY with
/// an ordinal week, BYMONTH, and a fixed time). An empty rule returns nothing
/// (the subcomponent relies on a single DTSTART).
fn vtimezone_rrule(rule: [u8; 16]) -> String {
    let month = u16::from_le_bytes([rule[2], rule[3]]);
    let day_of_week = u16::from_le_bytes([rule[4], rule[5]]);
    let week = u16::from_le_bytes([rule[6], rule[7]]);
    if month == 0 {
        return String::new();
    }
    let wk = if week == 5 { -1 } else { week as i32 };
    format!(
        "RRULE:FREQ=YEARLY;BYDAY={wk}{};BYMONTH={month}\r\n",
        weekday_name(day_of_week)
    )
}

/// Process-lifetime memo of `iana_to_windows_params`: an IANA id maps to a
/// deterministic `TzParams` (the `chrono_tz` zone rules are baked into the
/// binary, and the reference year is a constant), so the (potentially
/// expensive) full-year offset scan + the `WindowsTimezone::iter()` offset
/// comparison need only run once per id per process. This is the per-item
/// render path's hot loop — a calendar folder of N events triggered N year
/// scans before the cache — so the memo converts N scans into one. The map is
/// keyed by the raw input string (canonicalisation happens after the lookup)
/// so distinct spellings (e.g. `UTC` vs `Etc/UTC`) each get their own entry
/// rather than aliasing surprises; the values are small and bounded by the
/// finite set of IANA ids a tenant's calendar events reference.
static TZ_PARAMS_CACHE: LazyLock<Mutex<HashMap<String, Option<TzParams>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

pub fn iana_to_windows_params(iana: &str) -> Option<TzParams> {
    if let Some(cached) = TZ_PARAMS_CACHE
        .lock()
        .expect("TZ_PARAMS_CACHE mutex poisoned")
        .get(iana)
        .cloned()
    {
        return cached;
    }
    let computed = compute_iana_to_windows_params(iana);
    TZ_PARAMS_CACHE
        .lock()
        .expect("TZ_PARAMS_CACHE mutex poisoned")
        .insert(iana.to_string(), computed.clone());
    computed
}

fn compute_iana_to_windows_params(iana: &str) -> Option<TzParams> {
    let tz: Tz = iana.parse().ok()?;

    let win_name = match iana {
        "UTC" | "Etc/UTC" | "Etc/GMT" | "GMT" => "UTC".to_string(),
        _ => windows_timezone_name_for_iana(iana, tz)?,
    };

    let zt = zone_transitions(iana, tz);
    let bias = -zt.standard_offset;
    let std_date = zt.std_rule;
    let dst_date = zt.dst_rule;
    let dst_bias = zt
        .dst_offset
        .map(|d| -(d - zt.standard_offset))
        .unwrap_or(0);

    let dst_name = if zt.dst_offset.is_some() {
        win_name.replace("Standard", "Daylight")
    } else {
        win_name.clone()
    };

    Some((bias, win_name, dst_name, std_date, dst_date, 0, dst_bias))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Decode little-endian WORD from a SYSTEMTIME slice at `off`.
    fn word(b: &[u8], off: usize) -> u16 {
        u16::from_le_bytes([b[off], b[off + 1]])
    }

    /// `(month, weekday, week, hour)` for a Windows TZI SYSTEMTIME.
    fn parse_rule(b: &[u8; 16]) -> (u16, u16, u16, u16) {
        (word(b, 2), word(b, 4), word(b, 6), word(b, 8))
    }

    #[test]
    fn iana_to_windows_us_eastern_emits_second_sunday_march() {
        // America/New_York: DST begins 2nd Sunday of March at 02:00 EST (gap),
        // resumes 1st Sunday of November at 02:00 EDT (fold). The Windows TZI
        // wHour is the naive boundary hour in the outgoing phase = 02:00 both
        // directions (US 2007+ rules). Verifies the chrono_tz derivation matches
        // the documented Windows TZI for Eastern time.
        let (bias, _, _, std_date, dst_date, _, dst_bias) =
            iana_to_windows_params("America/New_York").expect("Eastern tz params");

        assert_eq!(bias, 300, "Eastern standard bias is UTC-5 => +300 bias");
        assert_eq!(dst_bias, -60);
        let dst = parse_rule(&dst_date);
        assert_eq!(dst.0, 3, "DST starts in March");
        assert_eq!(dst.1, 0, "DST starts on a Sunday");
        assert_eq!(dst.2, 2, "DST starts on the 2nd Sunday");
        assert_eq!(dst.3, 2, "DST starts at 02:00 (EST gap start)");
        let std = parse_rule(&std_date);
        assert_eq!(std.0, 11, "Standard resumes in November");
        assert_eq!(std.1, 0, "Standard resumes on a Sunday");
        assert_eq!(std.2, 1, "Standard resumes on the 1st Sunday");
        assert_eq!(std.3, 2, "Standard resumes at 02:00 (EDT fold boundary)");
    }

    #[test]
    fn iana_to_windows_santiago_southern_hemisphere_dst() {
        // America/Santiago observes DST Sep→Apr (southern hemisphere). The old
        // hardcoded approximation mis-encoded this as EU (Mar dst / Oct std),
        // i.e. the *reversed* hemisphere. The chrono_tz derivation must place
        // the DST-start month in September/October and the std-resume month in
        // April, with a -04 standard bias / -03 daylight (standard + 60).
        // Chile's autumn resume falls at midnight local (00:00) — a SYSTEMTIME
        // wHour of 0 — which the old `.clamp(1, 23)` erroneously shifted to 01:00;
        // this regression-asserts the boundary survives at 0 (gap #1 / C7 fix).
        let (bias, _, _, std_date, dst_date, _, dst_bias) =
            iana_to_windows_params("America/Santiago").expect("Santiago tz params");

        assert_eq!(bias, 240, "Santiago standard offset is UTC-4 => +240 bias");
        assert_eq!(dst_bias, -60);
        let dst = parse_rule(&dst_date);
        let std = parse_rule(&std_date);
        // DST begins in Sep or Oct, resumes standard in Apr.
        assert!(
            matches!(dst.0, 9 | 10),
            "Santiago DST begins Sep/Oct, got {}",
            dst.0
        );
        assert_eq!(std.0, 4, "Santiago standard resumes in April");
        // The autumn (std-resume) hour is 00:00 — verify the midnight boundary
        // survives the encoder rather than being clamped to 1.
        assert_eq!(
            std.3, 0,
            "Santiago standard resumes at 00:00 (midnight), got {}",
            std.3
        );
        // Sanity: a rule was actually produced (not the zeroed NO_DST).
        assert_ne!(dst_date, NO_DST);
        assert_ne!(std_date, NO_DST);
    }

    #[test]
    fn render_vtimezone_block_us_eastern_is_local_naive_no_z() {
        // The synthesised VTIMEZONE must use *local* wall-clock DTSTART values
        // (RFC 5545 §3.6.5: a UTC-suffixed DTSTART is forbidden inside a
        // VTIMEZONE subcomponent) anchored at the epoch year, with the STANDARD
        // and DAYLIGHT transitions emitted in the right order. This is a direct
        // regression guard for the C13 fix (no trailing `Z`, no offset math).
        let block = render_vtimezone_block("America/New_York").expect("Eastern VTIMEZONE");
        assert!(block.starts_with("BEGIN:VTIMEZONE\r\n"), "block = {block}");
        assert!(block.contains("TZID:America/New_York\r\n"));
        assert!(
            block.contains("BEGIN:DAYLIGHT\r\n") && block.contains("END:DAYLIGHT\r\n"),
            "DAYLIGHT subcomponent missing: {block}"
        );
        assert!(
            block.contains("BEGIN:STANDARD\r\n") && block.contains("END:STANDARD\r\n"),
            "STANDARD subcomponent missing: {block}"
        );
        // No DTSTART inside the VTIMEZONE may carry a UTC `Z` suffix.
        for line in block.lines() {
            if line.starts_with("DTSTART:") {
                assert!(
                    !line.ends_with('Z'),
                    "VTIMEZONE DTSTART must be local (no Z): {line}"
                );
            }
        }
        // The epoch-year anchor (1970) is the conventional first occurrence for
        // the RRULE; assert the DAYLIGHT DTSTART's month/day reflects the 2nd
        // Sunday of March 1970 (March 8, 1970 was a Sunday).
        assert!(
            block.contains("DTSTART:19700308T020000"),
            "expected 2nd-Sunday-of-March local DTSTART, block = {block}"
        );
    }

    #[test]
    fn render_vtimezone_block_kolkata_fixed_offset_no_dst() {
        // A fixed-offset (no-DST) zone synthesises a single STANDARD
        // subcomponent with no DAYLIGHT block, no RRULE, and a 1970-epoch
        // DTSTART. Guards the fixed-offset branch of render_vtimezone_block.
        let block = render_vtimezone_block("Asia/Kolkata").expect("Kolkata VTIMEZONE");
        assert!(block.contains("BEGIN:STANDARD\r\n"));
        assert!(
            !block.contains("BEGIN:DAYLIGHT\r\n"),
            "Kolkata must not carry a DAYLIGHT subcomponent: {block}"
        );
        assert!(
            !block.contains("RRULE:"),
            "Kolkata fixed-offset block must not carry an RRULE: {block}"
        );
        assert!(block.contains("DTSTART:19700101T000000"));
        assert!(block.contains("TZOFFSETFROM:+0530"));
        assert!(block.contains("TZOFFSETTO:+0530"));
        assert!(!block.contains("\r\nZ\r\n"), "no stray UTC Z: {block}");
    }

    #[test]
    fn iana_to_windows_no_dst_zone_emits_zeroed_transitions() {
        // Asia/Kolkata never observes DST; the blob must carry zeroed
        // StandardDate/DaylightDate so clients treat it as fixed-offset.
        let (bias, _, _, std_date, dst_date, _, dst_bias) =
            iana_to_windows_params("Asia/Kolkata").expect("Kolkata tz params");

        assert_eq!(bias, -330, "IST is UTC+5:30 => -330 bias");
        assert_eq!(dst_bias, 0);
        assert_eq!(std_date, NO_DST);
        assert_eq!(dst_date, NO_DST);
    }

    #[test]
    fn eas_timezone_blob_round_trips_via_decode_for_eastern() {
        // The synthesised base64 EAS Timezone blob must decode back to a bias
        // the EAS->IANA path accepts (i.e. the blob is structurally valid and
        // carries the correct bias for the zone).
        let blob = iana_to_eas_timezone_blob("America/New_York").expect("blob for Eastern");
        let bytes = BASE64.decode(blob.trim()).unwrap();
        assert_eq!(bytes.len(), TZ_BLOB_LEN);
        let bias = i32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
        assert_eq!(bias, 300);
        // Round-trip the bias via the decoder the EAS parse path uses.
        assert_eq!(decode_eas_timezone_bias(&blob), Some(300));
    }

    #[test]
    fn iana_to_windows_sydney_southern_hemisphere_dst() {
        // Australia/Sydney: DST begins 1st Sunday October at 02:00, resumes
        // standard 1st Sunday April at 03:00 (AEDT +11 / AEST +10).
        let (bias, _, _, std_date, dst_date, _, dst_bias) =
            iana_to_windows_params("Australia/Sydney").expect("Sydney tz params");

        assert_eq!(bias, -600, "AEST is UTC+10 => -600 bias");
        assert_eq!(dst_bias, -60);
        let dst = parse_rule(&dst_date);
        let std = parse_rule(&std_date);
        assert_eq!(dst.0, 10, "Sydney DST begins in October");
        assert_eq!(dst.3, 2, "Sydney DST begins at 02:00 (AEST gap start)");
        assert_eq!(std.0, 4, "Sydney standard resumes in April");
        assert_eq!(
            std.3, 3,
            "Sydney standard resumes at 03:00 (AEDT fold boundary)"
        );
        assert_ne!(dst_date, NO_DST);
        assert_ne!(std_date, NO_DST);
    }

    #[test]
    fn iana_to_windows_london_emits_last_sunday_march() {
        // Europe/London: DST begins last Sunday March 01:00 UTC (02:00 BST),
        // resumes last Sunday October at 02:00. Verifies the EU rules are still
        // produced by the chrono_tz path (not regressed by the rewrite).
        let (bias, _, _, std_date, dst_date, _, dst_bias) =
            iana_to_windows_params("Europe/London").expect("London tz params");

        assert_eq!(bias, 0, "London standard is UTC => 0 bias");
        assert_eq!(dst_bias, -60);
        let dst = parse_rule(&dst_date);
        let std = parse_rule(&std_date);
        assert_eq!(dst.0, 3, "London DST begins in March");
        assert_eq!(dst.2, 5, "last Sunday of March");
        assert_eq!(dst.3, 1, "London DST begins at 01:00 (GMT gap start)");
        assert_eq!(std.0, 10, "London standard resumes in October");
        assert_eq!(std.2, 5, "last Sunday of October");
        assert_eq!(
            std.3, 2,
            "London standard resumes at 02:00 (BST fold boundary)"
        );
    }
}
