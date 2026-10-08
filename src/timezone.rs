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

    // Substring heuristic for registry names embedded in longer custom
    // strings (e.g. Outlook's "Customized Time Zone" spellings). The bare
    // fixed-offset ids ("UTC", "UTC-11", "UTC+12" …) are excluded: they are
    // so short they appear inside every "(UTC±hh:mm)" display name and would
    // hijack it to a wrong fixed zone.
    let n_lower = n.to_ascii_lowercase();
    WindowsTimezone::iter()
        .filter(|variant| {
            let name = variant.name();
            name.len() >= 10 && n_lower.contains(&name.to_ascii_lowercase())
        })
        .max_by_key(|variant| variant.name().len())
}

/// Registry-id and display-name resolution WITHOUT the `(GMT±hh:mm)`-style
/// fixed-offset fallback: a real named zone carries the DST rules, while the
/// offset-string heuristic is only correct for genuinely fixed-offset
/// inputs. Keeping the fallback out of this tier lets callers consult an
/// authoritative `VTIMEZONE` (structural, DST-preserving) before ever
/// degrading to a fixed offset.
fn windows_named_zone_to_iana(name: &str) -> Option<String> {
    let n = name.trim();
    if n.is_empty() {
        return None;
    }
    if let Some(tz) = find_windows_timezone(n) {
        return Some(canonical_iana_id(tz.tzdb_id()).to_string());
    }
    // Windows display names (the `windows-timezones` `description()` strings):
    // "(UTC-08:00) Pacific Time (US & Canada)", the form Outlook/iCalendar
    // TZIDs and EAS `TimezoneName` elements carry.
    for variant in WindowsTimezone::iter() {
        if variant.description().eq_ignore_ascii_case(n) {
            return Some(canonical_iana_id(variant.tzdb_id()).to_string());
        }
    }
    None
}

fn windows_name_to_iana(name: &str) -> Option<String> {
    if let Some(iana) = windows_named_zone_to_iana(name) {
        return Some(iana);
    }
    // A named Windows zone wins over a fixed-offset id: a real zone carries
    // the DST rules, while the `(GMT±hh:mm)`-style fallback is only correct
    // for genuinely fixed-offset inputs. Resolving the name first keeps a
    // "(UTC-08:00) Pacific Time (US & Canada)"-style name from collapsing to
    // `Etc/GMT+8` and silently dropping Pacific DST.
    parse_utc_offset_name(name)
}

/// The modern IANA spelling for the handful of legacy tzdb ids the
/// `windows-timezones` crate still emits (CLDR's `windowsZones.xml` carried
/// them for years): both spellings parse everywhere, but emitting the
/// canonical id keeps the gateway's stored/JMAP/iCalendar timezone strings
/// stable across client updates. Inputs are the crate's `'static` ids, so the
/// returned borrow is `'static` too.
fn canonical_iana_id(tzdb_id: &'static str) -> &'static str {
    match tzdb_id {
        "America/Godthab" => "America/Nuuk",
        "Europe/Kiev" => "Europe/Kyiv",
        "Asia/Calcutta" => "Asia/Kolkata",
        "Asia/Katmandu" => "Asia/Kathmandu",
        "Asia/Rangoon" => "Asia/Yangon",
        // The gateway's canonical UTC spelling (`compute_iana_to_windows_params`
        // special-cases it; EAS/EWS/ICS all emit bare "UTC").
        "Etc/UTC" => "UTC",
        other => other,
    }
}

/// A decoded Windows `SYSTEMTIME` transition record from a `TZI` blob
/// ([MS-DTYP] §2.3.13). `year == 0` marks the *rule-based* form
/// (month/weekday/week-of-month/hour); a non-zero year marks a fixed-date
/// transition, which the matcher normalises back to the rule form.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct TziSystemTime {
    pub year: u16,
    pub month: u16,
    pub day_of_week: u16,
    pub day: u16,
    pub hour: u16,
    pub minute: u16,
    pub second: u16,
    pub millisecond: u16,
}

impl TziSystemTime {
    fn from_le_bytes(b: [u8; 16]) -> Self {
        let word = |off: usize| u16::from_le_bytes([b[off], b[off + 1]]);
        Self {
            year: word(0),
            month: word(2),
            day_of_week: word(4),
            day: word(6),
            hour: word(8),
            minute: word(10),
            second: word(12),
            millisecond: word(14),
        }
    }

    pub fn is_zeroed(self) -> bool {
        self == Self::default()
    }

    /// True when this record carries an annual recurrence rule
    /// (`wYear == 0` with a month set) rather than a fixed date.
    fn is_rule(self) -> bool {
        self.year == 0 && self.month != 0
    }
}

/// The fully decoded [MS-ASDTYPE] §2.7.6 `TimeZone` structure (the base64
/// payload of an EAS `Calendar:Timezone` element): the UTC bias, the standard
/// and daylight display names, and both `SYSTEMTIME` transition records with
/// their bias deltas. `TzParams` is the *encoding* counterpart of this type.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EasTimezoneBlob {
    pub bias: i32,
    pub standard_name: String,
    pub standard_date: TziSystemTime,
    pub standard_bias: i32,
    pub daylight_name: String,
    pub daylight_date: TziSystemTime,
    pub daylight_bias: i32,
}

/// Decode a base64 EAS `Timezone` blob ([MS-ASDTYPE] §2.7.6) into its full
/// `TZI` structure. Returns `None` for anything that is not a well-formed
/// 172-byte `TimeZone` structure.
pub fn decode_eas_timezone_blob(b64: &str) -> Option<EasTimezoneBlob> {
    let bytes = BASE64.decode(b64.trim()).ok()?;
    if bytes.len() < TZ_BLOB_LEN {
        return None;
    }
    let bias = i32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
    let standard_name = read_wchar_name(&bytes, 4);
    let mut std_bytes = [0u8; 16];
    std_bytes.copy_from_slice(&bytes[68..84]);
    let standard_bias = i32::from_le_bytes([bytes[84], bytes[85], bytes[86], bytes[87]]);
    let daylight_name = read_wchar_name(&bytes, 88);
    let mut dst_bytes = [0u8; 16];
    dst_bytes.copy_from_slice(&bytes[152..168]);
    let daylight_bias = i32::from_le_bytes([bytes[168], bytes[169], bytes[170], bytes[171]]);
    Some(EasTimezoneBlob {
        bias,
        standard_name,
        standard_date: TziSystemTime::from_le_bytes(std_bytes),
        standard_bias,
        daylight_name,
        daylight_date: TziSystemTime::from_le_bytes(dst_bytes),
        daylight_bias,
    })
}

/// Normalise a `SYSTEMTIME` transition record to the rule-based form Windows
/// `TZI` comparisons use: a fixed-date record (`wYear != 0`) is converted to
/// its equivalent (month, weekday, week-of-month, hour) rule so legacy blobs —
/// some Android OEM EAS stacks emit fixed-year records — compare equal to
/// rule-form candidates. All-zero records stay all-zero (no transition).
fn normalise_tzi_rule(record: TziSystemTime) -> TziSystemTime {
    if record.is_rule() || record.is_zeroed() {
        return record;
    }
    let Some(date) =
        chrono::NaiveDate::from_ymd_opt(record.year as i32, record.month as u32, record.day as u32)
    else {
        return record;
    };
    let weekday = date.weekday().num_days_from_sunday() as u16;
    let week = if u32::from(record.day) + 7 > month_days(record.year as i32, record.month as u32) {
        5
    } else {
        record.day.div_ceil(7).min(4)
    };
    TziSystemTime {
        year: 0,
        month: record.month,
        day_of_week: weekday,
        day: week,
        hour: record.hour,
        minute: record.minute,
        second: 0,
        millisecond: 0,
    }
}

/// One Windows-zone candidate for structural `TZI` matching: the Windows
/// registry id, its IANA id, and the synthesised `TzParams` blob encoding
/// (bias, std rule, dst rule, dst bias) derived from the IANA zone's actual
/// `chrono_tz` rules.
struct TziCandidate {
    windows_name: &'static str,
    iana: &'static str,
    params: Option<TzParams>,
}

/// Process-lifetime candidate table: every `WindowsTimezone` variant paired
/// with its IANA id and synthesised `TZI` parameters. Built once (each entry
/// costs one sampled full-year offset scan) and shared by the EAS blob decode
/// and the iCalendar `VTIMEZONE` matcher.
static TZI_CANDIDATES: LazyLock<Vec<TziCandidate>> = LazyLock::new(|| {
    WindowsTimezone::iter()
        .map(|variant| TziCandidate {
            windows_name: variant.name(),
            iana: canonical_iana_id(variant.tzdb_id()),
            params: compute_iana_to_windows_params(variant.tzdb_id()),
        })
        .collect()
});

/// Resolve an IANA id for a decoded `TZI` structure by structural matching
/// against every Windows zone's synthesised parameters. `prefer` (the IANA id
/// the blob's embedded names resolved to, if any) breaks ties between
/// structurally identical zones so name and rules never disagree; remaining
/// ties (distinct Windows zones that currently share one rule set, e.g.
/// "Pacific Standard Time" / "Pacific Standard Time (Mexico)") resolve to the
/// lexicographically-first canonical id, which matches the CLDR primary-zone
/// representative in every real pair.
///
/// Match tiers, most to least specific:
/// 1. exact `(bias, std rule, dst rule, dst bias)` match;
/// 2. `prefer`'s own zone when its bias and daylight bias agree with the blob
///    (the name is the client's authoritative identity; rules may reflect a
///    different transition era, e.g. pre-2007 US rules under a modern name);
/// 3. offset-level match `(bias, dst bias)` with differing rules;
/// 4. nothing — the caller degrades to a fixed-offset zone.
fn match_tzi_to_iana(
    bias: i32,
    standard_date: TziSystemTime,
    daylight_date: TziSystemTime,
    daylight_bias: i32,
    prefer: Option<&str>,
) -> Option<&'static str> {
    let std_rule = normalise_tzi_rule(standard_date);
    let dst_rule = normalise_tzi_rule(daylight_date);
    let has_dst = !std_rule.is_zeroed() && !dst_rule.is_zeroed();

    let exact = |c: &TziCandidate| -> bool {
        let Some(p) = &c.params else {
            return false;
        };
        let (c_bias, _, _, c_std, c_dst, _, c_dst_bias) = p;
        *c_bias == bias
            && *c_dst_bias == daylight_bias
            && TziSystemTime::from_le_bytes(*c_std) == std_rule
            && TziSystemTime::from_le_bytes(*c_dst) == dst_rule
    };
    let offset_only = |c: &TziCandidate| -> bool {
        let Some(p) = &c.params else {
            return false;
        };
        let (c_bias, _, _, c_std, c_dst, _, c_dst_bias) = p;
        *c_bias == bias
            && *c_dst_bias == daylight_bias
            && (c_std.iter().all(|&b| b == 0) == std_rule.is_zeroed())
            && (c_dst.iter().all(|&b| b == 0) == dst_rule.is_zeroed())
    };

    let mut exact_hits: Vec<&TziCandidate> = Vec::new();
    let mut offset_hits: Vec<&TziCandidate> = Vec::new();
    for candidate in TZI_CANDIDATES.iter() {
        if exact(candidate) {
            exact_hits.push(candidate);
        } else if offset_only(candidate) {
            offset_hits.push(candidate);
        }
    }
    // `prefer` (the zone the blob's embedded names resolved to) wins over the
    // tie-break whenever it is among the hits, so identity and rules never
    // disagree. Remaining structural ties resolve to the zone with the
    // SHORTEST Windows name (Windows' primary zones carry short registry
    // names; the regional variants are longer, e.g. "Pacific Standard Time"
    // vs. "Pacific Standard Time (Mexico)"), lexicographic id as the final
    // deterministic order.
    let pick = |hits: &[&TziCandidate]| -> Option<&'static str> {
        hits.iter()
            .find(|c| Some(c.iana) == prefer)
            .or_else(|| hits.iter().min_by_key(|c| (c.windows_name.len(), c.iana)))
            .map(|c| c.iana)
    };
    let exact_hit = pick(&exact_hits);
    if exact_hit.is_some() {
        return exact_hit;
    }
    // Tier 2: the name-resolved zone, accepted when its offsets agree with the
    // blob (identity wins over rule-era differences).
    if let Some(prefer) = prefer
        && let Some(candidate) = TZI_CANDIDATES
            .iter()
            .find(|c| c.iana == prefer || c.windows_name == prefer)
        && let Some((c_bias, _, _, _, _, _, c_dst_bias)) = &candidate.params
        && *c_bias == bias
        && *c_dst_bias == daylight_bias
    {
        return Some(candidate.iana);
    }
    if !has_dst {
        return None;
    }
    pick(&offset_hits)
}

/// Resolve the embedded Windows timezone name of a `TZI` blob to an IANA id:
/// the registry id (`"Pacific Standard Time"`), the Windows display name
/// (`"(UTC-08:00) Pacific Time (US & Canada)"`, as carried by the
/// `windows-timezones` descriptions and seen in real client blobs), or a
/// legacy GMT-offset display name via the substring heuristic.
fn blob_name_to_iana(name: &str) -> Option<String> {
    let n = name.trim();
    if n.is_empty() {
        return None;
    }
    windows_name_to_iana(n)
}

pub fn eas_timezone_blob_to_iana(b64: &str) -> Option<String> {
    let blob = decode_eas_timezone_blob(b64)?;
    let prefer =
        blob_name_to_iana(&blob.standard_name).or_else(|| blob_name_to_iana(&blob.daylight_name));

    if let Some(iana) = match_tzi_to_iana(
        blob.bias,
        blob.standard_date,
        blob.daylight_date,
        blob.daylight_bias,
        prefer.as_deref(),
    ) {
        return Some(iana.to_string());
    }

    // A DST-observing blob whose rules matched no known zone (e.g. an exotic
    // or historic rule set) still resolves through its embedded names before
    // degrading to a fixed offset — the names carry the zone identity even
    // when the transition rules do not correspond to any current zone.
    if let Some(iana) = prefer {
        return Some(iana);
    }

    if blob.bias == 0 {
        return Some("UTC".to_string());
    }
    Some(format!("Etc/GMT{:+}", blob.bias / 60))
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
/// Resolution order: registry id → Windows display name → `(GMT±hh:mm)`-style
/// fixed-offset id, so a named zone's DST rules never lose to the offset
/// string heuristic. Returns `None` for unrecognised names so callers can
/// fall back to UTC.
pub fn windows_timezone_name_to_iana(name: &str) -> Option<String> {
    windows_name_to_iana(name)
}

/// Registry-id/display-name resolution only — for callers that consult a
/// `VTIMEZONE` (or other authoritative structure) before ever accepting the
/// lossy fixed-offset degradation.
pub fn windows_named_timezone_to_iana(name: &str) -> Option<String> {
    windows_named_zone_to_iana(name)
}

/// The `(GMT±hh:mm)`/`(UTC±hh:mm)`-style fixed-offset id LAST-resort tier,
/// for callers that have exhausted named and structural resolution.
pub fn utc_offset_name_to_iana(name: &str) -> Option<String> {
    parse_utc_offset_name(name)
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
///
/// The legacy tzdb spelling the `windows-timezones` crate is keyed on for the
/// ids [`canonical_iana_id`] modernises — the inverse mapping, used when
/// resolving an IANA id back to its Windows zone so a canonical spelling
/// still finds the crate's entry deterministically instead of falling to
/// offset-sampling ambiguity.
fn legacy_iana_id(iana: &str) -> &str {
    match iana {
        "America/Nuuk" => "America/Godthab",
        "Europe/Kyiv" => "Europe/Kiev",
        "Asia/Kolkata" => "Asia/Calcutta",
        "Asia/Kathmandu" => "Asia/Katmandu",
        "Asia/Yangon" => "Asia/Rangoon",
        "UTC" => "Etc/UTC",
        other => other,
    }
}

fn windows_variant_for_iana(iana: &str, tz: Tz) -> Option<WindowsTimezone> {
    // Deterministic id match first — the crate's own (possibly legacy) id, or
    // the legacy spelling of a canonical id (chrono_tz carries Kyiv/Kiev etc.
    // as distinct variants, so `try_from` alone would miss the canonical one).
    for variant in WindowsTimezone::iter() {
        if variant.tzdb_id() == iana || variant.tzdb_id() == legacy_iana_id(iana) {
            return Some(variant);
        }
    }
    if let Ok(w) = WindowsTimezone::try_from(tz) {
        return Some(w);
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
            return Some(variant);
        }
    }
    // Last resort: a case-insensitive name match (e.g. caller passes the
    // Windows name itself as `iana`).
    find_windows_timezone(iana)
}

fn windows_timezone_name_for_iana(iana: &str, tz: Tz) -> Option<String> {
    windows_variant_for_iana(iana, tz).map(|v| v.name().to_string())
}

/// The Windows display description for an IANA id — the human-readable
/// `(UTC±hh:mm) …` string Exchange itself serves as the `Name`
/// attribute of `t:StartTimeZone`/`t:EndTimeZone` and in
/// `GetServerTimeZones` `TimeZoneDefinition` elements, which Outlook renders
/// in the timezone picker.
pub fn iana_to_windows_display_description(iana: &str) -> Option<String> {
    let tz: Tz = iana.parse().ok()?;
    windows_variant_for_iana(iana, tz).map(|v| v.description().to_string())
}

/// The matched `(registry Id, display-description Name)` pair for an IANA id,
/// both derived from ONE `windows_variant_for_iana` result — EWS serves the
/// two as the `Id`/`Name` attributes of a `t:TimeZoneDefinitionType`
/// (`StartTimeZone`/`EndTimeZone`), and the pair must never splice a name
/// from a different variant than the id. The UTC-class zones collapse to the
/// canonical `"UTC"` registry id (the same special case the `TZI` parameter
/// path applies) while the description still comes from that one variant
/// lookup.
pub fn iana_to_windows_timezone_pair(iana: &str) -> Option<(String, String)> {
    let tz: Tz = iana.parse().ok()?;
    let variant = windows_variant_for_iana(iana, tz)?;
    let id = match iana {
        "UTC" | "Etc/UTC" | "Etc/GMT" | "GMT" => "UTC".to_string(),
        _ => variant.name().to_string(),
    };
    Some((id, variant.description().to_string()))
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
    let dst_offset = *offsets.iter().max().expect("offsets non-empty");
    // A DST delta other than the conventional 60 minutes (e.g. Lord Howe's
    // +10:30/+11:00 half-hour shift) must survive into `DaylightBias`, so the
    // daylight offset is taken from the zone's actual maximum sampled offset
    // rather than assumed to be standard+60.
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

/// Parse a `TZI` `SYSTEMTIME` transition record ([MS-DTYP] §2.3.13) from its
/// 16-byte little-endian wire form — the encoding counterpart of
/// [`TziSystemTime::from_le_bytes`], for consumers holding a raw blob slice
/// (e.g. the EWS `TimeZoneDefinition` renderer working off `TzParams`).
pub fn tzi_rule_from_blob(blob: &[u8; 16]) -> TziSystemTime {
    TziSystemTime::from_le_bytes(*blob)
}

/// Evaluate a Windows `TZI` rule ([MS-DTYP] §2.3.13 `SYSTEMTIME` semantics)
/// and return the day-of-month of the transition it denotes in `year`.
/// `wDay` 1..=4 selects the nth weekday of the month; 5 selects the last;
/// a non-zero `wYear` pins the exact date.
fn tzi_transition_date(record: TziSystemTime, year: i32) -> Option<chrono::NaiveDate> {
    if record.is_zeroed() || record.month == 0 {
        return None;
    }
    if record.year != 0 {
        return chrono::NaiveDate::from_ymd_opt(
            record.year as i32,
            record.month as u32,
            record.day as u32,
        );
    }
    let first = chrono::NaiveDate::from_ymd_opt(year, record.month as u32, 1)?;
    let first_wd = first.weekday().num_days_from_sunday() as i32;
    let target = record.day_of_week as i32 % 7;
    let mut day = 1 + (target + 7 - first_wd) % 7;
    if record.day == 5 {
        day += 28;
        let dim = month_days(year, record.month as u32) as i32;
        while day > dim {
            day -= 7;
        }
    } else {
        day += ((record.day as i32).saturating_sub(1)) * 7;
    }
    chrono::NaiveDate::from_ymd_opt(year, record.month as u32, day as u32)
}

/// Resolve the local UTC offset (minutes east) that a Windows `TZI` blob
/// prescribes for a naive wall-clock date-time, applying the standard Windows
/// transition semantics: the year's daylight window is `[DaylightDate,
/// StandardDate)`; inside it the offset is `-(bias + daylight_bias)`, outside
/// it `-(bias + standard_bias)`. This is the reference evaluator the
/// no-offset-drift proof tests compare against `chrono_tz` ground truth.
pub fn tzi_offset_minutes_at(blob: &EasTimezoneBlob, ndt: chrono::NaiveDateTime) -> i32 {
    let date = ndt.date();
    let standard_bias_delta = if blob.standard_bias != 0 {
        blob.standard_bias
    } else {
        0
    };
    let base = -(blob.bias + standard_bias_delta);

    let dst_start = tzi_transition_date(blob.daylight_date, date.year());
    let dst_end = tzi_transition_date(blob.standard_date, date.year());
    let (Some(start), Some(end)) = (dst_start, dst_end) else {
        return base;
    };
    let dst_delta = blob.daylight_bias - blob.standard_bias;

    // Build the wall-clock instants of both transitions; a DST start earlier
    // in the year than its end denotes a northern-hemisphere window
    // (Mar..Nov), the reverse order denotes a southern-hemisphere window
    // (Oct..Apr) that wraps the new year.
    let at = |d: chrono::NaiveDate, record: TziSystemTime| {
        d.and_hms_opt(record.hour as u32, record.minute as u32, 0)
    };
    let (Some(start_at), Some(end_at)) =
        (at(start, blob.daylight_date), at(end, blob.standard_date))
    else {
        return base;
    };

    let in_window = if start_at <= end_at {
        ndt >= start_at && ndt < end_at
    } else {
        ndt >= start_at || ndt < end_at
    };
    if in_window { base - dst_delta } else { base }
}

/// Match an RFC 5545 `VTIMEZONE` component to an IANA zone id by structural
/// comparison of its `STANDARD`/`DAYLIGHT` transition data against every
/// Windows zone's synthesised `TZI` parameters. This is what lets an
/// Exchange-authored iCalendar whose `TZID` is a Windows display name (e.g.
/// `"(GMT-08.00) Pacific Time (US & Canada)/Tijuana"`, the exact form
/// [MS-ASCMD] documents) round-trip through Stalwart with the correct
/// `DTSTART`/`DTEND` UTC instants instead of being read as naive UTC.
///
/// Returns `None` when the block does not carry a resolvable
/// `STANDARD`/`DAYLIGHT` transition set.
pub fn match_vtimezone_to_iana(block: &str) -> Option<String> {
    /// One in-flight subcomponent's accumulated transition data.
    #[derive(Default)]
    struct Sub {
        offset_to: Option<i32>,
        dtstart: Option<TziSystemTime>,
        rrule_month: Option<u16>,
        rrule_byday: Option<(i32, u16)>,
    }

    impl Sub {
        /// Collapse the accumulated `DTSTART`/`RRULE` into the rule-based
        /// `TziSystemTime` form the `TZI` matcher compares against. The
        /// `RRULE` (`FREQ=YEARLY;BYDAY=2SU;BYMONTH=3`) wins over the
        /// `DTSTART`-derived week/weekday; the hour always comes from the
        /// `DTSTART` (the RFC 5545 local transition wall-clock).
        fn finish(&self) -> Option<(i32, TziSystemTime)> {
            let offset_to = self.offset_to?;
            let dtstart = self.dtstart?;
            let month = self
                .rrule_month
                .or(Some(dtstart.month).filter(|&m| m != 0))?;
            let (day, day_of_week) = match self.rrule_byday {
                Some((ordinal, weekday)) => (ordinal_to_tzi_week(ordinal)?, weekday),
                None => (dtstart.day, dtstart.day_of_week),
            };
            Some((
                offset_to,
                TziSystemTime {
                    year: 0,
                    month,
                    day_of_week,
                    day,
                    hour: dtstart.hour,
                    minute: dtstart.minute,
                    second: 0,
                    millisecond: 0,
                },
            ))
        }
    }

    let unfolded = crate::ical_parser::unfold_ical_content(block);
    let mut standard: Option<(i32, TziSystemTime)> = None;
    let mut daylight: Option<(i32, TziSystemTime)> = None;
    let mut current: Option<(bool, Sub)> = None;

    for line in unfolded.lines() {
        let line = line.trim_end_matches('\r').trim();
        if line == "BEGIN:STANDARD" || line == "BEGIN:DAYLIGHT" {
            current = Some((line.ends_with("STANDARD"), Sub::default()));
            continue;
        }
        if line == "END:STANDARD" || line == "END:DAYLIGHT" {
            if let Some((is_standard, sub)) = current.take()
                && let Some(resolved) = sub.finish()
            {
                if is_standard {
                    standard = Some(resolved);
                } else {
                    daylight = Some(resolved);
                }
            }
            continue;
        }
        let Some((_, sub)) = current.as_mut() else {
            continue;
        };
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        if key.starts_with("TZOFFSETTO") {
            sub.offset_to = parse_ical_offset(value);
        } else if key.starts_with("DTSTART") {
            sub.dtstart = parse_vtimezone_dtstart_rule(value);
        } else if key.starts_with("RRULE") {
            let (month, byday) = parse_vtimezone_rrule(value);
            sub.rrule_month = month;
            sub.rrule_byday = byday;
        }
    }

    let (std_offset, std_rule) = standard?;
    // No DAYLIGHT subcomponent: the zone is fixed-offset in the block's view.
    // Resolve through the candidate table first (a Windows id may still be
    // identifiable by bias), then degrade to the matching `Etc/GMT` zone.
    let Some((dst_offset, dst_rule)) = daylight else {
        let bias = -std_offset;
        return match_tzi_to_iana(
            bias,
            TziSystemTime::default(),
            TziSystemTime::default(),
            0,
            None,
        )
        .map(|iana| iana.to_string())
        .or_else(|| Some(fixed_offset_iana(bias)));
    };
    let bias = -std_offset;
    let daylight_bias = -(dst_offset - std_offset);
    match_tzi_to_iana(bias, std_rule, dst_rule, daylight_bias, None).map(|s| s.to_string())
}

/// `Etc/GMT+H` / `Etc/GMT-H` id for a bias in Windows sign convention
/// (positive = west of UTC), or `UTC` for zero.
fn fixed_offset_iana(bias: i32) -> String {
    if bias == 0 {
        "UTC".to_string()
    } else {
        format!("Etc/GMT{:+}", bias / 60)
    }
}

fn ordinal_to_tzi_week(ordinal: i32) -> Option<u16> {
    match ordinal {
        -4..=-1 => Some(5),
        1..=4 => Some(ordinal as u16),
        _ => None,
    }
}

/// Parse an iCalendar UTC offset value (`+0530` / `-0800`) into minutes east
/// of UTC.
fn parse_ical_offset(value: &str) -> Option<i32> {
    let v = value.trim();
    let bytes = v.as_bytes();
    if bytes.len() < 5 {
        return None;
    }
    let sign = match bytes[0] {
        b'+' => 1,
        b'-' => -1,
        _ => return None,
    };
    let hours: i32 = v.get(1..3)?.parse().ok()?;
    let minutes: i32 = v.get(3..5)?.parse().ok()?;
    Some(sign * (hours * 60 + minutes))
}

/// Parse a `VTIMEZONE` subcomponent `DTSTART` (`19700308T020000`) into a
/// `TziSystemTime` carrying its month/day/weekday/hour. Weekday-of-month and
/// week ordinal are derived from the concrete date; an `RRULE` overrides both
/// when present.
fn parse_vtimezone_dtstart_rule(value: &str) -> Option<TziSystemTime> {
    let v = value.trim();
    let naive = chrono::NaiveDateTime::parse_from_str(v, "%Y%m%dT%H%M%S")
        .or_else(|_| chrono::NaiveDateTime::parse_from_str(v, "%Y-%m-%dT%H:%M:%S"))
        .ok()?;
    let date = naive.date();
    let month = date.month() as u16;
    let day = date.day() as u16;
    let weekday = date.weekday().num_days_from_sunday() as u16;
    let week = if u32::from(day) + 7 > month_days(date.year(), date.month()) {
        5
    } else {
        day.div_ceil(7).min(4)
    };
    Some(TziSystemTime {
        year: 0,
        month,
        day_of_week: weekday,
        day: week,
        hour: chrono::Timelike::hour(&naive) as u16,
        minute: chrono::Timelike::minute(&naive) as u16,
        second: 0,
        millisecond: 0,
    })
}

/// Extract `(BYMONTH, (BYDAY ordinal, weekday))` from a `VTIMEZONE` `RRULE`
/// (`FREQ=YEARLY;BYDAY=2SU;BYMONTH=3` or `...BYDAY=-1SU...`). Returns
/// `Option` per field since either may be absent.
fn parse_vtimezone_rrule(value: &str) -> (Option<u16>, Option<(i32, u16)>) {
    let mut month = None;
    let mut byday = None;
    for part in value.split(';') {
        let Some((k, v)) = part.split_once('=') else {
            continue;
        };
        match k {
            "BYMONTH" => month = v.trim().parse().ok(),
            "BYDAY" => {
                let v = v.trim();
                let weekday =
                    match v.as_bytes().last() {
                        Some(b'U') if v.ends_with("SU") => 0,
                        Some(b'O') if v.ends_with("MO") => 1,
                        Some(b'E') if v.ends_with("TU") || v.ends_with("WE") => {
                            if v.ends_with("TU") { 2 } else { 3 }
                        }
                        Some(b'H') if v.ends_with("TH") => 4,
                        Some(b'R') if v.ends_with("FR") => 5,
                        Some(b'A') if v.ends_with("SA") => 6,
                        _ => continue,
                    };
                let ordinal: i32 = v[..v.len().saturating_sub(2)].parse().unwrap_or(1);
                byday = Some((ordinal, weekday));
            }
            _ => {}
        }
    }
    (month, byday)
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

    /// The 172-byte `TimeZone` structure of [MS-ASDTYPE] §2.7.6, built from a
    /// decoded field set — the inverse of `decode_eas_timezone_blob` and the
    /// exact wire form the [MS-ASCAL] §2.2.2.44 examples carry.
    fn build_tzi_blob(
        bias: i32,
        std_name: &str,
        std_rule: TziSystemTime,
        dst_name: &str,
        dst_rule: TziSystemTime,
        dst_bias: i32,
    ) -> String {
        let mut bytes = vec![0u8; TZ_BLOB_LEN];
        bytes[0..4].copy_from_slice(&bias.to_le_bytes());
        write_wchar_name(&mut bytes, 4, std_name);
        bytes[68..84].copy_from_slice(&std_rule_bytes(std_rule));
        // StandardBias: 0 in every real Windows TZI.
        bytes[84..88].copy_from_slice(&0i32.to_le_bytes());
        write_wchar_name(&mut bytes, 88, dst_name);
        bytes[152..168].copy_from_slice(&std_rule_bytes(dst_rule));
        bytes[168..172].copy_from_slice(&dst_bias.to_le_bytes());
        BASE64.encode(bytes)
    }

    fn std_rule_bytes(rule: TziSystemTime) -> [u8; 16] {
        let mut b = [0u8; 16];
        for (off, v) in [
            (0, rule.year),
            (2, rule.month),
            (4, rule.day_of_week),
            (6, rule.day),
            (8, rule.hour),
            (10, rule.minute),
            (12, rule.second),
            (14, rule.millisecond),
        ] {
            b[off..off + 2].copy_from_slice(&v.to_le_bytes());
        }
        b
    }

    /// [MS-ASCMD]/[MS-DTYP] rule form: `wYear=0` recurrence.
    fn rule(month: u16, day_of_week: u16, day: u16, hour: u16) -> TziSystemTime {
        TziSystemTime {
            year: 0,
            month,
            day_of_week,
            day,
            hour,
            minute: 0,
            second: 0,
            millisecond: 0,
        }
    }

    /// The Windows `TZI` rules for a zone, as an assertion-friendly tuple.
    fn expected_tzi(iana: &str) -> (i32, TziSystemTime, TziSystemTime, i32) {
        let (bias, _, _, std, dst, _, dst_bias) =
            iana_to_windows_params(iana).unwrap_or_else(|| panic!("params for {iana}"));
        (
            bias,
            TziSystemTime::from_le_bytes(std),
            TziSystemTime::from_le_bytes(dst),
            dst_bias,
        )
    }

    #[test]
    fn eas_timezone_blob_decodes_full_structure() {
        // The [MS-ASDTYPE] §2.7.6 decode must surface every field, not just the
        // bias: names, both transition records and both bias deltas.
        let blob = build_tzi_blob(
            480,
            "Pacific Standard Time",
            rule(11, 0, 1, 2),
            "Pacific Daylight Time",
            rule(3, 0, 2, 2),
            -60,
        );
        let decoded = decode_eas_timezone_blob(&blob).expect("decodable 172-byte TZI");
        assert_eq!(decoded.bias, 480);
        assert_eq!(decoded.standard_name, "Pacific Standard Time");
        assert_eq!(decoded.daylight_name, "Pacific Daylight Time");
        assert_eq!(decoded.standard_bias, 0);
        assert_eq!(decoded.daylight_bias, -60);
        assert_eq!(decoded.standard_date, rule(11, 0, 1, 2));
        assert_eq!(decoded.daylight_date, rule(3, 0, 2, 2));
    }

    #[test]
    fn eas_blob_with_registry_names_decodes_to_matching_iana_zone() {
        // A registry-name blob (the form New Outlook / Outlook Android emit
        // for their Windows zones) must resolve to the zone whose DST rules
        // the blob actually carries.
        let blob = build_tzi_blob(
            480,
            "Pacific Standard Time",
            rule(11, 0, 1, 2),
            "Pacific Daylight Time",
            rule(3, 0, 2, 2),
            -60,
        );
        assert_eq!(
            eas_timezone_blob_to_iana(&blob).as_deref(),
            Some("America/Los_Angeles")
        );
    }

    #[test]
    fn eas_blob_with_display_names_decodes_by_dst_rules_not_fixed_offset() {
        // [MS-ASDTYPE] §3.6.6's example `TimeZone` blob carries Windows
        // DISPLAY names ("(GMT-08:00) Pacific Time (US & C" in the legacy
        // spelling) rather than registry ids. The name is unresolvable, so
        // the old decode degraded to `Etc/GMT+8` — a fixed UTC-8 zone that
        // silently drops Pacific DST and shifts every summer event by an
        // hour. The structural matcher must instead read the blob's own
        // (bias, DST rules, DST bias) and resolve America/Los_Angeles.
        let blob = build_tzi_blob(
            480,
            "(GMT-08:00) Pacific Time (US & C",
            rule(11, 0, 1, 2),
            "(GMT-08:00) Pacific Time (US & C",
            rule(3, 0, 2, 2),
            -60,
        );
        assert_eq!(
            eas_timezone_blob_to_iana(&blob).as_deref(),
            Some("America/Los_Angeles"),
            "display-name blob must resolve by its DST rules, not collapse to Etc/GMT+8"
        );
    }

    #[test]
    fn eas_blob_with_display_names_europe_resolves_dst_zone() {
        // The classic Berlin/Amsterdam breakage: a "(GMT+01:00) Amsterdam,
        // Berlin, Bern, Rome, Stockholm, Vienna"-style blob must resolve to
        // a REAL EU DST zone — NOT to fixed Etc/GMT-1, which silently drops
        // DST and shifts every summer event by an hour. The Windows registry
        // has several structurally identical +01:00/+02:00 EU zones
        // (W. Europe / Romance / Central Europe …), so without a resolvable
        // name hint the exact spelling is an equivalence-class pick; the
        // guaranteed properties are (1) not a fixed Etc zone, and (2) rules
        // byte-identical to the blob's EU rules (last Sunday March 02:00 /
        // last Sunday October 03:00, ±60 DST delta).
        let blob = build_tzi_blob(
            -60,
            "(GMT+01:00) Amsterdam, Berlin, Bern, Rome, Stockholm, Vienna",
            rule(10, 0, 5, 3),
            "(GMT+02:00) Amsterdam, Berlin, Bern, Rome, Stockholm, Vienna",
            rule(3, 0, 5, 2),
            -60,
        );
        let iana = eas_timezone_blob_to_iana(&blob).expect("must resolve");
        assert!(
            !fixed_offset_zone(&iana),
            "display-name blob collapsed to fixed-offset zone {iana} (DST silently dropped)"
        );
        let (bias, std_rule, dst_rule, dst_bias) = expected_tzi(&iana);
        assert_eq!(bias, -60, "resolved zone must be the +01:00 EU cluster");
        assert_eq!(dst_bias, -60, "resolved zone must observe one-hour DST");
        assert_eq!(dst_rule.month, 3, "EU DST begins in March");
        assert_eq!(dst_rule.day, 5, "last Sunday of March");
        assert_eq!(dst_rule.hour, 2, "02:00 CET outgoing");
        assert_eq!(std_rule.month, 10, "EU standard resumes in October");
        assert_eq!(std_rule.day, 5, "last Sunday of October");
        assert_eq!(std_rule.hour, 3, "03:00 CEST outgoing");
    }

    #[test]
    fn eas_blob_fixed_date_transitions_normalise_to_rule_form() {
        // Some Android OEM EAS stacks encode the transitions as fixed-date
        // SYSTEMTIMEs (`wYear != 0`). The matcher normalises them to the
        // equivalent rule form so such blobs still resolve.
        let fixed = TziSystemTime {
            year: 2025,
            month: 3,
            day_of_week: 0,
            day: 9,
            hour: 2,
            ..TziSystemTime::default()
        };
        let blob = build_tzi_blob(
            480,
            "Pacific Standard Time",
            rule(11, 0, 1, 2),
            "Pacific Daylight Time",
            fixed,
            -60,
        );
        assert_eq!(
            eas_timezone_blob_to_iana(&blob).as_deref(),
            Some("America/Los_Angeles")
        );
    }

    #[test]
    fn eas_blob_no_dst_zeroed_blob_decodes_fixed_offset() {
        // An all-zero transition pair (no DST) with a non-zero bias resolves
        // to the matching fixed-offset zone, and a zero bias to UTC. Bias is
        // minutes WEST of UTC, so India's +05:30 is -330.
        let blob = build_tzi_blob(
            -330,
            "India Standard Time",
            TziSystemTime::default(),
            "India Standard Time",
            TziSystemTime::default(),
            0,
        );
        assert_eq!(
            eas_timezone_blob_to_iana(&blob).as_deref(),
            Some("Asia/Kolkata")
        );
        let utc_blob = build_tzi_blob(
            0,
            "UTC",
            TziSystemTime::default(),
            "UTC",
            TziSystemTime::default(),
            0,
        );
        assert_eq!(eas_timezone_blob_to_iana(&utc_blob).as_deref(), Some("UTC"));
    }

    #[test]
    fn half_hour_dst_zone_encodes_dst_bias() {
        // Australia/Lord_Howe shifts only +30 minutes for DST
        // (+10:30/+11:00). The blob must carry DaylightBias=-30 — the old
        // `standard+60` assumption produced -60 and moved every Lord Howe
        // summer event by half an hour.
        let (bias, _, _, std_date, dst_date, _, dst_bias) =
            iana_to_windows_params("Australia/Lord_Howe").expect("Lord Howe params");
        assert_eq!(bias, -630, "LHST is UTC+10:30 => -630 bias");
        assert_eq!(dst_bias, -30, "Lord Howe DST delta is +30 minutes");
        assert_ne!(std_date, NO_DST);
        assert_ne!(dst_date, NO_DST);
    }

    #[test]
    fn every_windows_timezone_blob_round_trips_to_same_iana() {
        // The §14 proof test: EVERY Windows timezone id the clients can emit
        // maps to an IANA zone, the zone synthesizes a 172-byte TZI blob,
        // and decoding that blob resolves back to the SAME IANA zone —
        // names and DST rules agreeing end to end.
        use strum::IntoEnumIterator;
        use windows_timezones::WindowsTimezone;

        let mut checked = 0;
        let mut failures = Vec::new();
        for variant in WindowsTimezone::iter() {
            let iana = canonical_iana_id(variant.tzdb_id());
            let Some(blob) = iana_to_eas_timezone_blob(iana) else {
                failures.push(format!("{iana}: no blob"));
                continue;
            };
            let decoded = match eas_timezone_blob_to_iana(&blob) {
                Some(z) => z,
                None => {
                    failures.push(format!("{iana}: decode returned None"));
                    continue;
                }
            };
            if decoded != iana {
                failures.push(format!("{iana}: decoded as {decoded}"));
                continue;
            }
            checked += 1;
        }
        assert!(
            failures.is_empty(),
            "{} of {} zones failed round-trip:\n{}",
            failures.len(),
            WindowsTimezone::iter().count(),
            failures.join("\n")
        );
        assert_eq!(checked, WindowsTimezone::iter().count());
    }

    #[test]
    fn tzi_blob_offsets_match_chrono_tz_ground_truth() {
        // The no-offset-drift proof: for a representative spread of zones —
        // northern/southern DST, half-hour DST delta, half-hour standard
        // offset, no-DST — the offset the Windows TZI blob prescribes must
        // equal the offset chrono_tz (the tzdb ground truth) reports, at
        // wall-clock samples across BOTH the standard and the daylight
        // phases of the reference year.
        let zones = [
            "America/Los_Angeles",
            "America/New_York",
            "Europe/Berlin",
            "Europe/Amsterdam",
            "Europe/London",
            "Australia/Sydney",
            "America/Santiago",
            "Australia/Lord_Howe",
            "Asia/Kolkata",
            "Asia/Tokyo",
            "Pacific/Chatham",
            "America/Sao_Paulo",
        ];
        let mut failures = Vec::new();
        for iana in zones {
            let tz: Tz = iana.parse().expect("parseable IANA id");
            let Some(b64) = iana_to_eas_timezone_blob(iana) else {
                failures.push(format!("{iana}: no blob"));
                continue;
            };
            let Some(blob) = decode_eas_timezone_blob(&b64) else {
                failures.push(format!("{iana}: blob did not decode"));
                continue;
            };
            // Sample the 1st and 15th of every month at 09:00 and 21:00 —
            // 48 samples spanning both daily halves and both DST phases.
            for (month, day) in (1..=12u32).flat_map(|m| [(m, 1u32), (m, 15u32)]) {
                for hour in [9u32, 21u32] {
                    let ndt = chrono::NaiveDate::from_ymd_opt(2025, month, day)
                        .and_then(|d| d.and_hms_opt(hour, 0, 0))
                        .expect("valid sample date");
                    let tzi_minutes = tzi_offset_minutes_at(&blob, ndt);
                    let chrono_minutes = ndt
                        .and_local_timezone(tz)
                        .earliest()
                        .map(|dt| dt.offset().fix().local_minus_utc() / 60);
                    if chrono_minutes != Some(tzi_minutes) {
                        failures.push(format!(
                            "{iana} {month:02}-{day:02}T{hour:02}:00: TZI says {tzi_minutes}, chrono_tz says {chrono_minutes:?}"
                        ));
                    }
                }
            }
        }
        assert!(
            failures.is_empty(),
            "offset drift detected:\n{}",
            failures.join("\n")
        );
    }

    #[test]
    fn vtimezone_structural_match_resolves_exchange_display_tzid() {
        // The [MS-ASCMD]-documented Exchange iCalendar TZID form ("(GMT-08.00)
        // Pacific Time (US & Canada)/Tijuana") with its authoritative
        // VTIMEZONE: the block's STANDARD/DAYLIGHT rules identify
        // America/Los_Angeles even though the TZID string is unparseable.
        let block = "BEGIN:VTIMEZONE\r\n\
             TZID:(GMT-08.00) Pacific Time (US & Canada)/Tijuana\r\n\
             BEGIN:STANDARD\r\n\
             DTSTART:16010101T020000\r\n\
             TZOFFSETFROM:-0700\r\n\
             TZOFFSETTO:-0800\r\n\
             RRULE:FREQ=YEARLY;BYDAY=1SU;BYMONTH=11\r\n\
             END:STANDARD\r\n\
             BEGIN:DAYLIGHT\r\n\
             DTSTART:16010101T020000\r\n\
             TZOFFSETFROM:-0800\r\n\
             TZOFFSETTO:-0700\r\n\
             RRULE:FREQ=YEARLY;BYDAY=2SU;BYMONTH=3\r\n\
             END:DAYLIGHT\r\n\
             END:VTIMEZONE\r\n";
        assert_eq!(
            match_vtimezone_to_iana(block).as_deref(),
            Some("America/Los_Angeles")
        );
    }

    #[test]
    fn vtimezone_structural_match_resolves_eu_zone() {
        // EU form: last-Sunday March / last-Sunday October with the
        // 02:00/03:00 outgoing-phase wall clocks, +01:00/+02:00 offsets.
        let block = "BEGIN:VTIMEZONE\r\n\
             TZID:Custom-W-Europe\r\n\
             BEGIN:STANDARD\r\n\
             DTSTART:19701025T030000\r\n\
             TZOFFSETFROM:+0200\r\n\
             TZOFFSETTO:+0100\r\n\
             RRULE:FREQ=YEARLY;BYDAY=-1SU;BYMONTH=10\r\n\
             END:STANDARD\r\n\
             BEGIN:DAYLIGHT\r\n\
             DTSTART:19700329T020000\r\n\
             TZOFFSETFROM:+0100\r\n\
             TZOFFSETTO:+0200\r\n\
             RRULE:FREQ=YEARLY;BYDAY=-1SU;BYMONTH=3\r\n\
             END:DAYLIGHT\r\n\
             END:VTIMEZONE\r\n";
        let matched = match_vtimezone_to_iana(block).expect("resolvable");
        // The structurally identical W. Europe / Central Europe zones — the
        // matcher must land on a zone whose rules ARE the EU rules (verified
        // via its own synthesized params), not any +01:00 zone.
        let (_, std_rule, dst_rule, dst_bias) = expected_tzi(&matched);
        assert_eq!(std_rule.month, 10);
        assert_eq!(std_rule.day, 5, "last Sunday of October");
        assert_eq!(std_rule.hour, 3, "03:00 CEST outgoing");
        assert_eq!(dst_rule.month, 3);
        assert_eq!(dst_rule.day, 5, "last Sunday of March");
        assert_eq!(dst_rule.hour, 2, "02:00 CET outgoing");
        assert_eq!(dst_bias, -60);
    }

    #[test]
    fn vtimezone_structural_match_fixed_offset_block() {
        // A VTIMEZONE with only a STANDARD subcomponent (+05:30) resolves to
        // the matching fixed-offset zone.
        let block = "BEGIN:VTIMEZONE\r\n\
             TZID:Custom-Kolkata\r\n\
             BEGIN:STANDARD\r\n\
             DTSTART:19700101T000000\r\n\
             TZOFFSETFROM:+0530\r\n\
             TZOFFSETTO:+0530\r\n\
             END:STANDARD\r\n\
             END:VTIMEZONE\r\n";
        assert_eq!(
            match_vtimezone_to_iana(block).as_deref(),
            Some("Asia/Kolkata")
        );
    }

    #[test]
    fn tzi_offset_evaluator_us_eastern_window() {
        // Reference evaluator spot-check: US Eastern 2025 DST window is
        // [Mar 9 02:00, Nov 2 02:00). Outside it UTC-5, inside it UTC-4.
        let blob = build_tzi_blob(
            300,
            "Eastern Standard Time",
            rule(11, 0, 1, 2),
            "Eastern Daylight Time",
            rule(3, 0, 2, 2),
            -60,
        );
        let at = |s: &str| chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S").unwrap();
        let blob = decode_eas_timezone_blob(&blob).expect("decodable");
        // Offsets are minutes EAST of UTC (chrono's `local_minus_utc`
        // convention): Eastern standard is -300, Eastern daylight is -240.
        assert_eq!(
            tzi_offset_minutes_at(&blob, at("2025-03-08T09:00:00")),
            -300
        );
        assert_eq!(
            tzi_offset_minutes_at(&blob, at("2025-03-09T03:00:00")),
            -240
        );
        assert_eq!(
            tzi_offset_minutes_at(&blob, at("2025-07-04T12:00:00")),
            -240
        );
        assert_eq!(
            tzi_offset_minutes_at(&blob, at("2025-11-01T12:00:00")),
            -240
        );
        assert_eq!(
            tzi_offset_minutes_at(&blob, at("2025-11-02T03:00:00")),
            -300
        );
    }

    #[test]
    fn tzi_offset_evaluator_southern_hemisphere_wrapping_window() {
        // Southern-hemisphere DST wraps the new year: Sydney is in DST for
        // Jan–Apr and Oct–Dec. The evaluator's window logic must keep both
        // legs at +11:00.
        let blob = build_tzi_blob(
            -600,
            "AUS Eastern Standard Time",
            rule(4, 0, 1, 3),
            "AUS Eastern Daylight Time",
            rule(10, 0, 1, 2),
            -60,
        );
        let at = |s: &str| chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S").unwrap();
        let blob = decode_eas_timezone_blob(&blob).expect("decodable");
        // East-positive: AEST is +600, AEDT is +660, and the wrap keeps both
        // summer legs (January, December) at +660.
        assert_eq!(tzi_offset_minutes_at(&blob, at("2025-01-15T09:00:00")), 660);
        assert_eq!(tzi_offset_minutes_at(&blob, at("2025-06-15T09:00:00")), 600);
        assert_eq!(tzi_offset_minutes_at(&blob, at("2025-12-15T09:00:00")), 660);
    }

    #[test]
    fn iana_to_windows_timezone_pair_is_a_matched_variant_pair() {
        // EWS renders `t:StartTimeZone Id="…" Name="…"` — the registry id and
        // the display description must come from the SAME Windows zone, never
        // two independently-resolved lookups. Cross-check the pair against
        // the crate's own variant for the same zone, and the UTC-class
        // collapse onto the canonical "UTC" registry id.
        let (id, name) =
            iana_to_windows_timezone_pair("America/Los_Angeles").expect("LA pair resolves");
        assert_eq!(id, "Pacific Standard Time");
        assert_eq!(
            name,
            iana_to_windows_display_description("America/Los_Angeles")
                .expect("description resolves"),
            "pair Name must equal the variant's display description"
        );
        assert_ne!(id, name, "id and Name are distinct attribute roles");

        for utc_class in ["UTC", "Etc/UTC", "Etc/GMT", "GMT"] {
            let (id, _) = iana_to_windows_timezone_pair(utc_class)
                .unwrap_or_else(|| panic!("{utc_class}: pair must resolve"));
            assert_eq!(
                id, "UTC",
                "{utc_class}: UTC-class zones share one registry id"
            );
        }

        assert_eq!(
            iana_to_windows_timezone_pair("Not/A_Real_Zone"),
            None,
            "unresolvable zone yields no pair"
        );
    }
}
