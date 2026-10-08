// src/calendar.rs
use crate::attachment::ParsedEasAttachmentAdd;
use crate::ical_parser;
use crate::util::nfc;
use crate::util::resolve_xml_reference;
use anyhow::{Result, anyhow};
use chrono::{NaiveDate, NaiveDateTime, Offset, TimeZone, Utc};
use chrono_tz::Tz;
use derive_more::Debug;
use itertools::Itertools;
use phf::phf_map;
use quick_xml::Reader;
use quick_xml::XmlVersion;
use quick_xml::events::Event;
use roxmltree::Document;
use rrule::{Frequency, NWeekday, RRule, Tz as RruleTz, Weekday};
use smallvec::SmallVec;
use uuid::Uuid;

#[derive(Clone, Debug, Default)]
pub struct CalendarItem {
    pub uid: String,
    pub subject: String,
    pub description: String,
    pub location: String,
    pub start: chrono::DateTime<Utc>,
    pub end: chrono::DateTime<Utc>,
    pub all_day: bool,
    pub dtstamp: Option<chrono::DateTime<Utc>>,
    pub timezone: Option<String>,
    pub timezone_blob: Option<String>,
    pub rrule: Option<String>,
    pub exdates: Vec<chrono::DateTime<Utc>>,
    pub organizer_name: Option<String>,
    pub organizer_email: Option<String>,
    pub attendees: Vec<Attendee>,
    pub categories: Vec<String>,
    pub busy_status: Option<u8>,
    pub sensitivity: Option<u8>,
    pub reminder: Option<i32>,
    pub response_requested: Option<bool>,
    pub disallow_new_time_proposal: Option<bool>,
    pub appointment_reply_time: Option<chrono::DateTime<Utc>>,
    pub meeting_status: Option<u8>,
    pub response_type: Option<u8>,
    pub online_meeting_conf_link: Option<String>,
    pub online_meeting_external_link: Option<String>,
    pub client_uid: Option<String>,
    pub exceptions: Vec<CalendarException>,
}

#[derive(Clone, Debug, Default)]
pub struct CalendarException {
    pub deleted: bool,
    pub exception_start: chrono::DateTime<Utc>,
    pub subject: Option<String>,
    pub description: Option<String>,
    pub location: Option<String>,
    pub start: Option<chrono::DateTime<Utc>>,
    pub end: Option<chrono::DateTime<Utc>>,
    pub all_day: Option<bool>,
    pub busy_status: Option<u8>,
    pub sensitivity: Option<u8>,
    pub reminder: Option<i32>,
    pub appointment_reply_time: Option<chrono::DateTime<Utc>>,
    pub meeting_status: Option<u8>,
    pub response_type: Option<u8>,
    pub attendees: Option<Vec<Attendee>>,
    pub categories: Option<Vec<String>>,
}

#[derive(Clone, Debug, Default)]
pub struct Attendee {
    pub name: Option<String>,
    pub email: String,
    pub attendee_type: Option<u8>,
    pub attendee_status: Option<u8>,
    pub partstat: Option<String>,
    /// RFC 6638 §7.1 `SCHEDULE-AGENT` value for this attendee. When
    /// `Some("CLIENT")`, the server MUST NOT auto-deliver scheduling messages
    /// for this attendee (the client claims responsibility). Used to honour
    /// EWS `SendToNone` without stripping attendee data the user intends to
    /// invite later. `None` lets the server auto-schedule normally.
    pub schedule_agent: Option<String>,
}

#[derive(Clone, Debug, Default)]
pub struct CalendarPatch {
    pub uid: Option<String>,
    pub subject: Option<String>,
    pub description: Option<String>,
    pub location: Option<String>,
    pub start: Option<chrono::DateTime<Utc>>,
    pub end: Option<chrono::DateTime<Utc>>,
    pub all_day: Option<bool>,
    pub dtstamp: Option<chrono::DateTime<Utc>>,
    pub timezone: Option<String>,
    pub timezone_blob: Option<String>,
    pub rrule: Option<String>,
    pub exdates: Option<Vec<chrono::DateTime<Utc>>>,
    pub organizer_name: Option<String>,
    pub organizer_email: Option<String>,
    pub attendees: Option<Vec<Attendee>>,
    pub categories: Option<Vec<String>>,
    pub busy_status: Option<u8>,
    pub sensitivity: Option<u8>,
    pub reminder: Option<i32>,
    pub response_requested: Option<bool>,
    pub disallow_new_time_proposal: Option<bool>,
    pub appointment_reply_time: Option<chrono::DateTime<Utc>>,
    pub meeting_status: Option<u8>,
    pub response_type: Option<u8>,
    pub online_meeting_conf_link: Option<String>,
    pub online_meeting_external_link: Option<String>,
    pub client_uid: Option<String>,
    pub exceptions: Option<Vec<CalendarException>>,
}

#[derive(Clone, Debug)]
pub enum EasSyncMutation {
    Add {
        client_id: Option<String>,
        item: CalendarItem,
        attachment_adds: Vec<ParsedEasAttachmentAdd>,
    },
    Change {
        server_id: String,
        instance_id: Option<chrono::DateTime<Utc>>,
        patch: CalendarPatch,
        attachment_adds: Vec<ParsedEasAttachmentAdd>,
        attachment_deletes: Vec<String>,
    },
    Delete {
        server_id: String,
        instance_id: Option<chrono::DateTime<Utc>>,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EasOpKind {
    Add,
    Change,
    Delete,
}

#[derive(Default)]
struct EasBuilder {
    client_id: Option<String>,
    server_id: Option<String>,
    instance_id: Option<chrono::DateTime<Utc>>,
    subject: Option<String>,
    description: Option<String>,
    location: Option<String>,
    start: Option<chrono::DateTime<Utc>>,
    end: Option<chrono::DateTime<Utc>>,
    all_day: Option<bool>,
    dtstamp: Option<chrono::DateTime<Utc>>,
    timezone: Option<String>,
    timezone_blob: Option<String>,
    uid: Option<String>,
    client_uid: Option<String>,
    recurrence: EasRecurrence,
    exdates: Vec<chrono::DateTime<Utc>>,
    organizer_name: Option<String>,
    organizer_email: Option<String>,
    attendees: Vec<Attendee>,
    current_attendee: Option<Attendee>,
    categories: Vec<String>,
    busy_status: Option<u8>,
    sensitivity: Option<u8>,
    reminder: Option<i32>,
    response_requested: Option<bool>,
    disallow_new_time_proposal: Option<bool>,
    appointment_reply_time: Option<chrono::DateTime<Utc>>,
    meeting_status: Option<u8>,
    response_type: Option<u8>,
    online_meeting_conf_link: Option<String>,
    online_meeting_external_link: Option<String>,
    exceptions: Vec<CalendarException>,
    current_exception: Option<CalendarException>,
    in_attachments: bool,
    in_attachment: bool,
    in_att_display_name: bool,
    in_att_method: bool,
    in_att_estimated_data_size: bool,
    in_att_content_type: bool,
    in_att_content_id: bool,
    in_att_content_location: bool,
    in_att_is_inline: bool,
    in_att_data: bool,
    in_att_file_reference: bool,
    current_att: Option<ParsedEasAttachmentAdd>,
    attachment_adds: Vec<ParsedEasAttachmentAdd>,
    attachment_deletes: Vec<String>,
}

#[derive(Default)]
pub(crate) struct EasRecurrence {
    kind: Option<u8>,
    interval: Option<u32>,
    day_of_week: Option<String>,
    day_of_month: Option<u32>,
    week_of_month: Option<u32>,
    month_of_year: Option<u32>,
    until: Option<String>,
    occurrences: Option<u32>,
    first_day_of_week: Option<u32>,
    calendar_type: Option<u8>,
    is_empty: bool,
}

static DAY_BITS: [(u32, &str); 7] = [
    (1, "SU"),
    (2, "MO"),
    (4, "TU"),
    (8, "WE"),
    (16, "TH"),
    (32, "FR"),
    (64, "SA"),
];

static WEEKDAY_CODES: phf::Map<u32, &'static str> = phf_map! {
    1u32 => "SU",
    2u32 => "MO",
    3u32 => "TU",
    4u32 => "WE",
    5u32 => "TH",
    6u32 => "FR",
    7u32 => "SA",
};

fn mask_to_byday(value: u32) -> Vec<&'static str> {
    DAY_BITS
        .iter()
        .filter_map(|&(bit, code)| if value & bit != 0 { Some(code) } else { None })
        .collect()
}

fn weekday_code_from_eas(value: u32) -> &'static str {
    WEEKDAY_CODES.get(&value).copied().unwrap_or("MO")
}

fn day_code_to_weekday(code: &str) -> Option<Weekday> {
    match code {
        "SU" => Some(Weekday::Sun),
        "MO" => Some(Weekday::Mon),
        "TU" => Some(Weekday::Tue),
        "WE" => Some(Weekday::Wed),
        "TH" => Some(Weekday::Thu),
        "FR" => Some(Weekday::Fri),
        "SA" => Some(Weekday::Sat),
        _ => None,
    }
}

fn month_num_to_chrono(m: u32) -> Option<chrono::Month> {
    match m {
        1 => Some(chrono::Month::January),
        2 => Some(chrono::Month::February),
        3 => Some(chrono::Month::March),
        4 => Some(chrono::Month::April),
        5 => Some(chrono::Month::May),
        6 => Some(chrono::Month::June),
        7 => Some(chrono::Month::July),
        8 => Some(chrono::Month::August),
        9 => Some(chrono::Month::September),
        10 => Some(chrono::Month::October),
        11 => Some(chrono::Month::November),
        12 => Some(chrono::Month::December),
        _ => None,
    }
}

impl EasBuilder {
    fn into_item(self) -> Result<CalendarItem> {
        let start = self.start.ok_or_else(|| anyhow!("missing StartTime"))?;
        let end = self.end.ok_or_else(|| anyhow!("missing EndTime"))?;
        let uid = self
            .client_uid
            .clone()
            .or(self.uid)
            .unwrap_or_else(|| Uuid::new_v4().to_string());
        Ok(CalendarItem {
            uid,
            subject: self.subject.unwrap_or_else(|| "(no subject)".to_string()),
            description: self.description.unwrap_or_default(),
            location: self.location.unwrap_or_default(),
            start,
            end,
            all_day: self.all_day.unwrap_or(false),
            dtstamp: self.dtstamp,
            timezone: self.timezone,
            timezone_blob: self.timezone_blob,
            rrule: self.recurrence.to_rrule(),
            exdates: self.exdates,
            organizer_name: self.organizer_name,
            organizer_email: self.organizer_email,
            attendees: self.attendees,
            categories: self.categories,
            busy_status: self.busy_status,
            sensitivity: self.sensitivity,
            reminder: self.reminder,
            response_requested: self.response_requested,
            disallow_new_time_proposal: self.disallow_new_time_proposal,
            appointment_reply_time: self.appointment_reply_time,
            meeting_status: self.meeting_status,
            response_type: self.response_type,
            online_meeting_conf_link: self.online_meeting_conf_link,
            online_meeting_external_link: self.online_meeting_external_link,
            client_uid: self.client_uid,
            exceptions: self.exceptions,
        })
    }

    fn into_patch(self) -> CalendarPatch {
        let rrule = if self.recurrence.is_empty {
            Some(String::new())
        } else {
            self.recurrence.to_rrule()
        };
        CalendarPatch {
            uid: self.uid,
            subject: self.subject,
            description: self.description,
            location: self.location,
            start: self.start,
            end: self.end,
            all_day: self.all_day,
            dtstamp: self.dtstamp,
            timezone: self.timezone,
            timezone_blob: self.timezone_blob,
            rrule,
            exdates: (!self.exdates.is_empty()).then_some(self.exdates),
            organizer_name: self.organizer_name,
            organizer_email: self.organizer_email,
            attendees: (!self.attendees.is_empty()).then_some(self.attendees),
            categories: (!self.categories.is_empty()).then_some(self.categories),
            busy_status: self.busy_status,
            sensitivity: self.sensitivity,
            reminder: self.reminder,
            response_requested: self.response_requested,
            disallow_new_time_proposal: self.disallow_new_time_proposal,
            appointment_reply_time: self.appointment_reply_time,
            meeting_status: self.meeting_status,
            response_type: self.response_type,
            online_meeting_conf_link: self.online_meeting_conf_link,
            online_meeting_external_link: self.online_meeting_external_link,
            client_uid: self.client_uid,
            exceptions: (!self.exceptions.is_empty()).then_some(self.exceptions),
        }
    }
}

impl EasRecurrence {
    pub fn to_rrule(&self) -> Option<String> {
        if self.is_empty {
            return None;
        }
        let kind = self.kind?;
        let freq = match kind {
            0 => Frequency::Daily,
            1 => Frequency::Weekly,
            2 | 3 => Frequency::Monthly,
            5 | 6 => Frequency::Yearly,
            _ => return None,
        };

        let mut rule = RRule::new(freq);

        if let Some(interval) = self.interval
            && interval > 1
        {
            rule = rule.interval(interval as u16);
        }

        if let Some(mask) = &self.day_of_week {
            let value = mask.parse::<u32>().unwrap_or(0);
            let byday = mask_to_byday(value);
            if !byday.is_empty() {
                let nweekdays: Vec<NWeekday> = if let Some(week) = self.week_of_month
                    && (kind == 3 || kind == 6)
                    && week > 0
                {
                    let ordinal = match week {
                        5 => -1i16,
                        n => n as i16,
                    };
                    byday
                        .iter()
                        .take(1)
                        .filter_map(|&code| {
                            day_code_to_weekday(code).map(|wd| NWeekday::Nth(ordinal, wd))
                        })
                        .collect()
                } else {
                    byday
                        .into_iter()
                        .filter_map(|code| day_code_to_weekday(code).map(NWeekday::Every))
                        .collect()
                };
                if !nweekdays.is_empty() {
                    rule = rule.by_weekday(nweekdays);
                }
            }
        }

        if let Some(day) = self.day_of_month
            && matches!(kind, 2 | 5)
        {
            rule = rule.by_month_day(vec![day as i8]);
        }

        if let Some(month) = self.month_of_year
            && matches!(kind, 5 | 6)
            && let Some(m) = month_num_to_chrono(month)
        {
            rule = rule.by_month(&[m]);
        }

        if let Some(count) = self.occurrences {
            rule = rule.count(count);
        } else if let Some(until) = &self.until
            && let Some(dt) = parse_datetime(until)
        {
            let until_dt: chrono::DateTime<RruleTz> = dt.with_timezone(&RruleTz::UTC);
            rule = rule.until(until_dt);
        }

        if let Some(first_day) = self.first_day_of_week {
            let wkst_code = weekday_code_from_eas(first_day);
            if let Some(wd) = day_code_to_weekday(wkst_code) {
                rule = rule.week_start(wd);
            }
        }

        Some(rule.to_string())
    }
}

/// True when the value is an offset-less (`xs:dateTime` naive) date-time: it
/// carries neither a `Z` suffix nor a `±hh:mm` suffix, so the caller must
/// localise it in the request's timezone rather than reading it as UTC.
pub(crate) fn datetime_is_naive(val: &str) -> bool {
    let val = val.trim();
    !val.ends_with('Z') && !val.ends_with('z') && !naive_has_offset(val)
}

/// True when the value carries an explicit `±hh:mm`/`±hhmm` UTC offset.
fn naive_has_offset(val: &str) -> bool {
    let Some(pos) = val.rfind(['+', '-']) else {
        return false;
    };
    // Only a sign in the TIME part (after the 10th char) is an offset; a sign
    // in a date part would be malformed input anyway.
    pos >= 10
        && val[pos + 1..]
            .trim_start_matches(|c: char| c.is_ascii_digit())
            .starts_with(':')
}

pub fn parse_datetime(val: &str) -> Option<chrono::DateTime<Utc>> {
    let val = val.trim();
    if val.ends_with('Z') {
        NaiveDateTime::parse_from_str(val, "%Y%m%dT%H%M%SZ")
            .map(|dt| Utc.from_utc_datetime(&dt))
            .ok()
            .or_else(|| {
                chrono::DateTime::parse_from_rfc3339(val)
                    .ok()
                    .map(|dt| dt.with_timezone(&Utc))
            })
    } else if val.contains('T') {
        NaiveDateTime::parse_from_str(val, "%Y%m%dT%H%M%S")
            .map(|dt| Utc.from_utc_datetime(&dt))
            .ok()
            .or_else(|| {
                NaiveDateTime::parse_from_str(val, "%Y-%m-%dT%H:%M:%S")
                    .map(|dt| Utc.from_utc_datetime(&dt))
                    .ok()
            })
            .or_else(|| {
                chrono::DateTime::parse_from_rfc3339(val)
                    .ok()
                    .map(|dt| dt.with_timezone(&Utc))
            })
    } else {
        NaiveDate::parse_from_str(val, "%Y%m%d")
            .ok()
            .and_then(|d| d.and_hms_opt(0, 0, 0))
            .map(|dt| Utc.from_utc_datetime(&dt))
            .or_else(|| {
                NaiveDate::parse_from_str(val, "%Y-%m-%d")
                    .ok()
                    .and_then(|d| d.and_hms_opt(0, 0, 0))
                    .map(|dt| Utc.from_utc_datetime(&dt))
            })
    }
}

/// Parse an EWS `xs:dateTime` in the request's timezone when the value is
/// naive (no `Z`, no offset). Outlook's EWS stack serialises item start/end
/// without an offset and sets the `t:TimeZoneContext` SOAP header (or
/// `t:StartTimeZoneId`) instead; reading such values as UTC shifts every
/// event by the zone's UTC offset — the §14 offset-drift failure. Values that
/// already carry an offset (or no `zone`) parse exactly as `parse_datetime`.
pub(crate) fn parse_datetime_in_zone(val: &str, zone: Option<Tz>) -> Option<chrono::DateTime<Utc>> {
    let Some(zone) = zone else {
        return parse_datetime(val);
    };
    let val = val.trim();
    if !datetime_is_naive(val) || !val.contains('T') {
        return parse_datetime(val);
    }
    let naive = NaiveDateTime::parse_from_str(val, "%Y%m%dT%H%M%S")
        .or_else(|_| NaiveDateTime::parse_from_str(val, "%Y-%m-%dT%H:%M:%S"))
        .ok()?;
    localize_gap_tolerant(naive, zone)
}

fn unescape_ical_text(input: &str) -> String {
    let mut out = String::new();
    let mut chars = input.chars();
    while let Some(ch) = chars.next() {
        if ch == '\\' {
            match chars.next() {
                Some('n') | Some('N') => out.push('\n'),
                Some('\\') => out.push('\\'),
                Some(';') => out.push(';'),
                Some(',') => out.push(','),
                Some(next) => out.push(next),
                None => break,
            }
        } else {
            out.push(ch);
        }
    }
    out
}

pub fn parse_ics_content(ics: &str) -> Vec<(String, String)> {
    match ical_parser::parse_property_lines(&ical_parser::unfold_ical_content(ics)) {
        Ok(properties) => properties,
        Err(_) => {
            let unfolded = ical_parser::unfold_ical_content(ics);
            let mut properties = Vec::new();
            for line in unfolded.lines() {
                if line.is_empty() {
                    continue;
                }
                if let Some(colon_idx) = line.find(':') {
                    let key = line[..colon_idx].to_string();
                    let value = line[colon_idx + 1..].to_string();
                    properties.push((key, value));
                }
            }
            properties
        }
    }
}

fn split_ical_blocks(ics: &str) -> Vec<Vec<String>> {
    let unfolded = ical_parser::unfold_ical_content(ics);
    let mut blocks = Vec::new();
    let mut current = Vec::new();
    let mut in_vevent = false;
    for line in unfolded.lines() {
        match line.trim() {
            "BEGIN:VEVENT" => {
                in_vevent = true;
                current = vec!["BEGIN:VEVENT".to_string()];
            }
            "END:VEVENT" if in_vevent => {
                current.push("END:VEVENT".to_string());
                blocks.push(current.clone());
                current.clear();
                in_vevent = false;
            }
            _ if in_vevent => current.push(line.to_string()),
            _ => {}
        }
    }
    blocks
}

fn extract_vtimezone_block(ics: &str) -> Option<String> {
    ical_parser::parse_vtimezone_block(ics).ok().flatten()
}

fn parse_categories_value(value: &str) -> Vec<String> {
    value
        .split(',')
        .map(unescape_ical_text)
        .filter(|v| !v.is_empty())
        .collect()
}

fn parse_tzid_from_key(key: &str) -> Option<String> {
    parse_ical_param(key, "TZID")
}

/// Resolve an iCalendar `TZID` parameter value to a zone the gateway can
/// interpret `DTSTART`/`DTEND`/`RECURRENCE-ID`/`EXDATE` values against, in
/// this order:
///
/// 1. the id is already an IANA id (the canonical case for Stalwart and this
///    gateway's own `render_ics` emission);
/// 2. the id is a Windows registry id or display name (`"W. Europe Standard
///    Time"`, `"(UTC+01:00) Amsterdam, Berlin, ..."`) — Exchange-authored
///    iCalendar carries these;
/// 3. the authoritative `VTIMEZONE` component in the same file identifies the
///    zone structurally by its transition rules, so even a fully custom
///    `TZID` string (e.g. `"(GMT-08.00) Pacific Time (US & Canada)/Tijuana"`,
///    the form [MS-ASCMD] documents) resolves to the right IANA zone;
/// 4. only with no `VTIMEZONE` left to consult, a `(GMT±hh:mm)`-style offset
///    id — a lossy fixed-offset degradation that must never preempt the
///    DST-preserving structural match (a `"(GMT-08.00) Pacific ..."` TZID
///    that collapses to `Etc/GMT+8` silently shifts every summer event by an
///    hour).
///
/// Without this, an unresolvable `TZID` falls back to naive-UTC parsing of the
/// datetime and every such event drifts by its UTC offset — the exact
/// "round-trips Stalwart CalDAV without offset drift" failure §14 calls out.
fn resolve_ical_tzid(tzid: &str, vtimezone: Option<&str>) -> Option<String> {
    let trimmed = tzid.trim().trim_matches('"');
    if trimmed.is_empty() {
        return None;
    }
    if trimmed.parse::<Tz>().is_ok() {
        return Some(trimmed.to_string());
    }
    if let Some(iana) = crate::timezone::windows_named_timezone_to_iana(trimmed)
        && iana.parse::<Tz>().is_ok()
    {
        return Some(iana);
    }
    if let Some(block) = vtimezone
        && let Some(iana) = crate::timezone::match_vtimezone_to_iana(block)
    {
        return Some(iana);
    }
    crate::timezone::utc_offset_name_to_iana(trimmed)
}

/// Map a wall-clock datetime in `tz` to its UTC instant, gap-tolerant: an
/// ambiguous (fold) time takes the earlier mapping; a non-existent (gap)
/// time maps with the offset in force just before the transition — the
/// policy RFC 5545/RFC 7265 recommend and the same instant Outlook produces.
/// chrono's `LocalResult` yields `None` for gaps and `Ambiguous` for folds.
///
/// The pre-transition offset is recovered by probing backward in 12-hour
/// steps. A single fixed probe is not enough: date-line transitions skip a
/// whole civil day (Pacific/Apia dropped 2011-12-30 when it moved from
/// UTC-10 to UTC+14), so a naive time more than 12 hours into a >12h gap
/// still lands inside the gap at the first probe. 48 hours of probing covers
/// every known skipped-day transition; each probe must itself resolve
/// unambiguously so the recovered offset really is the pre-transition one.
fn localize_gap_tolerant(naive: NaiveDateTime, tz: Tz) -> Option<chrono::DateTime<Utc>> {
    match naive.and_local_timezone(tz) {
        chrono::LocalResult::Single(dt) => Some(dt.with_timezone(&Utc)),
        chrono::LocalResult::Ambiguous(earliest, _) => Some(earliest.with_timezone(&Utc)),
        chrono::LocalResult::None => {
            let offset = (1..=4i64)
                .map(|step| naive - chrono::Duration::hours(12 * step))
                .find_map(|probe| {
                    probe
                        .and_local_timezone(tz)
                        .single()
                        .map(|dt| dt.offset().fix())
                })?;
            let utc = naive - chrono::Duration::seconds(offset.local_minus_utc() as i64);
            Some(chrono::DateTime::<Utc>::from_naive_utc_and_offset(utc, Utc))
        }
    }
}

/// Parse an iCalendar date-time against a timezone id. A naive value (no `Z`,
/// no offset) is localized in `tzid` — gap-tolerant, fold-takes-earliest —
/// via `localize_gap_tolerant`. Values with an explicit `Z`/offset and DATE
/// values carry their own meaning and parse as `parse_datetime` does.
///
/// A `tzid` that fails to resolve to a chrono-tz zone leaves a naive value
/// with NO honest instant: returning `None` (rather than a silent UTC guess
/// that drifts by the zone's offset) is the fail-closed choice.
fn parse_datetime_with_tzid(val: &str, tzid: Option<&str>) -> Option<chrono::DateTime<Utc>> {
    let Some(tzid) = tzid else {
        return parse_datetime(val);
    };
    if !val.contains('T') || !datetime_is_naive(val) {
        return parse_datetime(val);
    }
    let naive = NaiveDateTime::parse_from_str(val, "%Y%m%dT%H%M%S")
        .or_else(|_| NaiveDateTime::parse_from_str(val, "%Y-%m-%dT%H:%M:%S"))
        .ok()?;
    localize_gap_tolerant(naive, tzid.parse::<Tz>().ok()?)
}

/// Resolve a single date-time property's own `TZID` parameter through the
/// per-file memoized resolver. Each property is resolved independently —
/// RFC 5545 §3.8.2.2/§3.8.5.3 allow a different `TZID` per property (e.g. a
/// `DTSTART;TZID=A` paired with a `DTEND;TZID=B` flight-style event), so the
/// zone for one property must never be reused for another.
fn resolve_property_zone(
    tzid: Option<String>,
    tz_resolver: &mut dyn FnMut(&str) -> Option<String>,
) -> Option<String> {
    tzid.as_deref().and_then(tz_resolver)
}

/// Parse an iCalendar date-time value against its property's resolved zone.
///
/// `zone` is the IANA id the property's `TZID` resolved to; `supplied` records
/// whether the property carried a `TZID` parameter at all. When the property
/// DID carry a `TZID` that failed to resolve (`supplied && zone.is_none()`),
/// a naive date-time is uninterpretable — the honest answer is no instant,
/// not a silent UTC guess that shifts the event by the zone's offset (the
/// §14 offset-drift failure). DATE values and explicit-offset/UTC values
/// stay readable in every case: a DATE is zone-agnostic and an explicit
/// offset carries its own instant.
fn parse_datetime_with_tzid_failing_unresolved(
    val: &str,
    supplied: bool,
    zone: Option<String>,
) -> Option<chrono::DateTime<Utc>> {
    if supplied && zone.is_none() && datetime_is_naive(val) && val.contains('T') {
        return None;
    }
    parse_datetime_with_tzid(val, zone.as_deref())
}

fn parse_duration_minutes(trigger: &str) -> Option<i32> {
    ical_parser::parse_ical_duration_minutes(trigger).ok()
}

fn parse_event_lines(
    lines: &[String],
    tz_resolver: &mut dyn FnMut(&str) -> Option<String>,
) -> CalendarEventFields {
    let mut fields = CalendarEventFields::default();
    let mut in_valarm = false;

    for line in lines {
        if line == "BEGIN:VALARM" {
            in_valarm = true;
            continue;
        }
        if line == "END:VALARM" {
            in_valarm = false;
            continue;
        }
        if matches!(line.as_str(), "BEGIN:VEVENT" | "END:VEVENT") {
            continue;
        }

        let Some((key, value)) = line.split_once(':') else {
            continue;
        };

        if in_valarm {
            if key.starts_with("TRIGGER") {
                fields.reminder = parse_duration_minutes(value);
            }
            continue;
        }

        match key {
            k if k.starts_with("SUMMARY") => fields.subject = Some(unescape_ical_text(value)),
            k if k.starts_with("DESCRIPTION") => {
                fields.description = Some(unescape_ical_text(value))
            }
            k if k.starts_with("LOCATION") => fields.location = Some(unescape_ical_text(value)),
            k if k.starts_with("UID") => fields.uid = Some(value.to_string()),
            k if k.starts_with("DTSTAMP") => fields.dtstamp = parse_datetime(value),
            k if k.starts_with("DTSTART") => {
                let tzid = parse_tzid_from_key(k);
                let supplied = tzid.is_some();
                let zone = resolve_property_zone(tzid.clone(), tz_resolver);
                fields.start = parse_datetime_with_tzid_failing_unresolved(value, supplied, zone);
                if fields.timezone.is_none() {
                    fields.timezone = tzid;
                }
                if !value.contains('T') {
                    fields.all_day = Some(true);
                }
            }
            k if k.starts_with("DTEND") => {
                let tzid = parse_tzid_from_key(k);
                let supplied = tzid.is_some();
                let zone = resolve_property_zone(tzid.clone(), tz_resolver);
                fields.end = parse_datetime_with_tzid_failing_unresolved(value, supplied, zone);
                if fields.timezone.is_none() {
                    fields.timezone = tzid;
                }
            }
            k if k.starts_with("RECURRENCE-ID") => {
                let tzid = parse_tzid_from_key(k);
                let supplied = tzid.is_some();
                let zone = resolve_property_zone(tzid.clone(), tz_resolver);
                fields.recurrence_id =
                    parse_datetime_with_tzid_failing_unresolved(value, supplied, zone);
                if fields.timezone.is_none() {
                    fields.timezone = tzid;
                }
                if !value.contains('T') {
                    fields.all_day = Some(true);
                }
            }
            k if k.starts_with("RRULE") => fields.rrule = Some(value.to_string()),
            k if k.starts_with("EXDATE") => {
                let tzid = parse_tzid_from_key(k);
                let supplied = tzid.is_some();
                let zone = resolve_property_zone(tzid, tz_resolver);
                for ex in value.split(',') {
                    if let Some(dt) =
                        parse_datetime_with_tzid_failing_unresolved(ex, supplied, zone.clone())
                    {
                        fields.exdates.push(dt);
                    }
                }
            }
            k if k.starts_with("ORGANIZER") => {
                let (cn, email) = parse_ical_actor_line(k, value);
                fields.organizer_name = cn;
                fields.organizer_email = email;
            }
            k if k.starts_with("ATTENDEE") => {
                let (name, email) = parse_ical_actor_line(k, value);
                let partstat = parse_ical_param(k, "PARTSTAT");
                fields.attendees.push(Attendee {
                    name,
                    email: email.unwrap_or_default(),
                    attendee_type: parse_ical_param(k, "ROLE").map(|role| match role.as_str() {
                        "REQ-PARTICIPANT" => 1,
                        "OPT-PARTICIPANT" => 2,
                        "NON-PARTICIPANT" => 3,
                        _ => 1,
                    }),
                    attendee_status: partstat.as_deref().map(partstat_to_status),
                    partstat,
                    schedule_agent: parse_ical_param(k, "SCHEDULE-AGENT"),
                });
            }
            k if k.starts_with("CATEGORIES") => {
                fields.categories.extend(parse_categories_value(value));
            }
            "CLASS" => fields.sensitivity = class_to_sensitivity(value),
            "STATUS" if value.eq_ignore_ascii_case("CANCELLED") => fields.deleted = true,
            "TRANSP" => {
                fields.busy_status = Some(if value.eq_ignore_ascii_case("TRANSPARENT") {
                    0
                } else {
                    2
                });
            }
            "X-MICROSOFT-CDO-BUSYSTATUS" => fields.busy_status = value.parse().ok(),
            "X-MICROSOFT-CDO-ALLDAYEVENT" => fields.all_day = Some(value == "TRUE"),
            "X-MICROSOFT-CDO-REPLYTIME" | "X-MS-APPOINTMENT-REPLY-TIME" => {
                fields.appointment_reply_time = parse_datetime(value)
            }
            "X-MS-OLK-CONFLINK" => fields.online_meeting_conf_link = Some(value.to_string()),
            "X-MS-OLK-EXTERNALLINK" => {
                fields.online_meeting_external_link = Some(value.to_string())
            }
            "X-MS-RESPONSE-REQUESTED" => fields.response_requested = Some(value == "TRUE"),
            "X-MS-DISALLOW-COUNTER" => fields.disallow_new_time_proposal = Some(value == "TRUE"),
            "X-MS-MEETING-STATUS" => fields.meeting_status = value.parse().ok(),
            "X-MS-RESPONSE-TYPE" => fields.response_type = value.parse().ok(),
            "X-MS-CLIENT-UID" => fields.client_uid = Some(value.to_string()),
            "X-EAS-TIMEZONE" => fields.timezone = Some(value.to_string()),
            _ => {}
        }
    }

    fields
}

#[derive(Default)]
struct CalendarEventFields {
    subject: Option<String>,
    description: Option<String>,
    location: Option<String>,
    uid: Option<String>,
    start: Option<chrono::DateTime<Utc>>,
    end: Option<chrono::DateTime<Utc>>,
    all_day: Option<bool>,
    dtstamp: Option<chrono::DateTime<Utc>>,
    recurrence_id: Option<chrono::DateTime<Utc>>,
    rrule: Option<String>,
    exdates: Vec<chrono::DateTime<Utc>>,
    organizer_name: Option<String>,
    organizer_email: Option<String>,
    attendees: Vec<Attendee>,
    categories: Vec<String>,
    busy_status: Option<u8>,
    sensitivity: Option<u8>,
    reminder: Option<i32>,
    response_requested: Option<bool>,
    disallow_new_time_proposal: Option<bool>,
    appointment_reply_time: Option<chrono::DateTime<Utc>>,
    meeting_status: Option<u8>,
    response_type: Option<u8>,
    online_meeting_conf_link: Option<String>,
    online_meeting_external_link: Option<String>,
    client_uid: Option<String>,
    timezone: Option<String>,
    deleted: bool,
}

pub fn parse_ics_event(ics: &str) -> Option<CalendarItem> {
    let timezone_blob = extract_vtimezone_block(ics);
    let mut master: Option<CalendarItem> = None;
    let mut derived_deleted = Vec::new();
    let mut pending_exceptions = Vec::new();

    // TZID resolution memo for this file: a raw `TZID` parameter spelling maps
    // to the IANA id the gateway interprets date-times against (see
    // `resolve_ical_tzid`). Memoized because a recurring event repeats the
    // same `TZID` on DTSTART, DTEND, EXDATE and every RECURRENCE-ID.
    let mut tzid_memo: std::collections::HashMap<String, Option<String>> =
        std::collections::HashMap::new();
    let mut resolve = |tzid: &str| -> Option<String> {
        if let Some(hit) = tzid_memo.get(tzid) {
            return hit.clone();
        }
        let resolved = resolve_ical_tzid(tzid, timezone_blob.as_deref());
        tzid_memo.insert(tzid.to_string(), resolved.clone());
        resolved
    };

    for block in split_ical_blocks(ics) {
        let mut fields = parse_event_lines(&block, &mut resolve);
        if let Some(recurrence_id) = fields.recurrence_id {
            let exception = CalendarException {
                deleted: fields.deleted,
                exception_start: recurrence_id,
                subject: fields.subject,
                description: fields.description,
                location: fields.location,
                start: fields.start,
                end: fields.end,
                all_day: fields.all_day,
                busy_status: fields.busy_status,
                sensitivity: fields.sensitivity,
                reminder: fields.reminder,
                appointment_reply_time: fields.appointment_reply_time,
                meeting_status: fields.meeting_status,
                response_type: fields.response_type,
                attendees: (!fields.attendees.is_empty()).then_some(fields.attendees),
                categories: (!fields.categories.is_empty()).then_some(fields.categories),
            };
            if let Some(item) = &mut master {
                item.exceptions.push(exception);
            } else {
                pending_exceptions.push(exception);
            }
            continue;
        }

        let uid = fields.uid.unwrap_or_else(|| Uuid::new_v4().to_string());
        // Store the RESOLVED IANA id, not the raw TZID spelling, so every
        // downstream consumer (render_ics DTSTART;TZID, EAS `Calendar:Timezone`
        // blob synthesis, recurrence expansion) interprets the event in the
        // zone the `VTIMEZONE`/TZID actually denotes. The authoritative
        // `VTIMEZONE` block itself is preserved verbatim in `timezone_blob`
        // for byte-faithful re-emission to CalDAV.
        if let Some(raw) = fields.timezone.as_deref()
            && let Some(resolved) = resolve(raw)
        {
            fields.timezone = Some(resolved);
        }
        let mut item = CalendarItem {
            uid,
            subject: fields.subject.unwrap_or_default(),
            description: fields.description.unwrap_or_default(),
            location: fields.location.unwrap_or_default(),
            start: fields.start?,
            end: fields.end?,
            all_day: fields.all_day.unwrap_or(false),
            dtstamp: fields.dtstamp,
            timezone: fields.timezone,
            timezone_blob: timezone_blob.clone(),
            rrule: fields.rrule,
            exdates: fields.exdates.clone(),
            organizer_name: fields.organizer_name,
            organizer_email: fields.organizer_email,
            attendees: fields.attendees,
            categories: fields.categories,
            busy_status: fields.busy_status,
            sensitivity: fields.sensitivity,
            reminder: fields.reminder,
            response_requested: fields.response_requested,
            disallow_new_time_proposal: fields.disallow_new_time_proposal,
            appointment_reply_time: fields.appointment_reply_time,
            meeting_status: fields.meeting_status,
            response_type: fields.response_type,
            online_meeting_conf_link: fields.online_meeting_conf_link,
            online_meeting_external_link: fields.online_meeting_external_link,
            client_uid: fields.client_uid,
            exceptions: Vec::new(),
        };
        derived_deleted.append(
            &mut item
                .exdates
                .iter()
                .copied()
                .map(|dt| CalendarException {
                    deleted: true,
                    exception_start: dt,
                    ..Default::default()
                })
                .collect(),
        );
        item.exceptions.append(&mut pending_exceptions);
        master = Some(item);
    }

    let mut item = master?;
    for deleted in derived_deleted {
        if !item
            .exceptions
            .iter()
            .any(|existing| existing.exception_start == deleted.exception_start)
        {
            item.exceptions.push(deleted);
        }
    }
    item.exceptions.sort_by_key(|v| v.exception_start);
    Some(item)
}

/// Ensure a `CalendarItem` carries an ORGANIZER email when it represents a
/// scheduled meeting (has attendees) but no organizer was supplied by the
/// client. The ORGANIZER property is mandatory in RFC 5545/iTIP REQUESTs and
/// is required by Stalwart's CalDAV scheduler (RFC 6638) to route invitations
/// to attendees — without it Stalwart will reject or silently drop the
/// scheduling iTIP and Outlook attendees never receive the invite.
///
/// `owner_email` is the authenticated user's primary SMTP (their calendar is
/// being written to, so they are the meeting organizer). It is left empty for
/// attendee-less personal appointments, where ORGANIZER is optional and
/// Stalwart will not attempt scheduling.
pub fn ensure_organizer_for_scheduling(item: &mut CalendarItem, owner_email: Option<&str>) {
    if item
        .organizer_email
        .as_deref()
        .is_some_and(|e| !e.trim().is_empty())
    {
        return;
    }
    if item.attendees.is_empty() {
        return;
    }
    if let Some(addr) = owner_email
        && !addr.trim().is_empty()
    {
        item.organizer_email = Some(addr.trim().to_string());
    }
}

/// The EWS `CalendarItemCreateOrUpdateOperationType` controlling whether meeting
/// invitations/cancellations are sent. Mirrors [MS-OXWSICAL] §3.1.4.2.2.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScheduleDisposition {
    /// No scheduling operations are performed.
    SendToNone,
    /// /Calendar: Save the updated copy to the organizer's calendar as well as
    /// /Calendar: sending the invite (CreateItem) or rescheduling (UpdateItem).
    SendToAllAndSaveCopy,
    /// Send the invite/cancellation but do not save a copy in the organizer's
    /// calendar. Not used by Outlook for normal meeting creation; treated as
    /// "send" for scheduling purposes.
    SendOnlyToAll,
    /// UpdateItem-only: only message changed attendees, save a copy.
    SendToChangedOnly,
    SendToAllAndSaveCopyInclDeleted,
    SendToChangedAndSaveCopy,
}

impl ScheduleDisposition {
    /// Parse the `SendMeetingInvitationsOrCancellations` /
    /// `SendMeetingInvitations` / `SendMeetingInvitationsOrCancellations`
    /// attribute value (CreateItem/UpdateItem) and `SendMeetingCancellations`
    /// (DeleteItem). Unknown / empty → `None`.
    pub fn parse(value: Option<&str>) -> Option<Self> {
        match value.map(str::trim)?.trim() {
            "SendToNone" => Some(Self::SendToNone),
            "SendOnlyToAll" => Some(Self::SendOnlyToAll),
            "SendToAllAndSaveCopy" => Some(Self::SendToAllAndSaveCopy),
            "SendOnlyToChanged" | "SendToChangedOnly" => Some(Self::SendToChangedOnly),
            "SendToAllAndSaveCopyInclDeleted" => Some(Self::SendToAllAndSaveCopyInclDeleted),
            "SendToChangedAndSaveCopy" => Some(Self::SendToChangedAndSaveCopy),
            // Outlook sometimes sends "SendToNone" variants or a numeric form.
            _ => None,
        }
    }

    /// Whether the client wants attendees notified (i.e. a real scheduling
    /// operation must run server-side). `SendToNone` and `Unknown` never do.
    pub fn wants_scheduling(self) -> bool {
        !matches!(self, Self::SendToNone)
    }
}

/// Honour an EWS `SendToNone` disposition without destroying the attendee list.
///
/// Stalwart's CalDAV scheduler auto-delivers invitations for any event that
/// carries ATTENDEE properties (RFC 6638 §3.2): there is no per-PUT "do not
/// schedule" flag outside the data. The standards-correct way to suppress
/// scheduling while keeping the attendee roster intact is RFC 6638 §7.1
/// `SCHEDULE-AGENT=CLIENT`, which signals "the client is responsible for
/// delivery; the server must NOT auto-schedule". We annotate every attendee
/// with that parameter rather than deleting them, so Outlook can still show
/// the planned invitee list and re-send later.
pub fn mark_scheduling_client_side(item: &mut CalendarItem) {
    for attendee in &mut item.attendees {
        attendee.schedule_agent = Some("CLIENT".to_string());
    }
}

/// True when the item carries attendees and the disposition asks the server to
/// schedule (send invites/cancellations). This is the criterion that selects
/// the CalDAV backend — JMAP Calendar `iCalendar`-blob writes do not trigger
/// Stalwart's scheduler, so only CalDAV PUT delivers the iTIP to attendees.
pub fn scheduling_needed(item: &CalendarItem, disposition: ScheduleDisposition) -> bool {
    disposition.wants_scheduling() && !item.attendees.is_empty()
}

pub fn render_ics(item: &CalendarItem) -> String {
    use icalendar::{Calendar, CalendarComponent, Class, Component, Event, EventLike, Property};
    use std::str::FromStr;

    let dtstamp = item.dtstamp.unwrap_or_else(Utc::now);
    let uid = if item.uid.is_empty() {
        Uuid::new_v4().to_string()
    } else {
        item.uid.clone()
    };

    let mut calendar = Calendar::new();
    calendar.append_property(Property::new("PRODID", "-//exchange_gateway//EN"));

    // Push every `VTIMEZONE` component found in `source` (wrapped in a throwaway
    // VCALENDAR so the `icalendar` parser accepts a raw VTIMEZONE) onto `calendar`,
    // returning whether any was pushed. The authoritative stored blob is parsed
    // first; its real VTIMEZONE(s) are re-emitted byte-for-byte. Synthesis runs
    // only when that yields no VTIMEZONE — including the case where the stored
    // blob fails to parse (e.g. a bare Windows time-zone name captured from an
    // inbound EWS `MeetingTimeZone` has no name-value separator and parses to
    // `Err`). The fallback synthesises the canonical VTIMEZONE from the item's
    // IANA id so the emitted iCalendar is RFC-5545-valid and round-trips
    // byte-for-byte with what CalDAV expects (authoritative TZID/RRULE UNTIL
    // boundaries).
    fn push_vtimezones(calendar: &mut Calendar, source: &str) -> bool {
        let wrapped = format!("BEGIN:VCALENDAR\r\n{source}\r\nEND:VCALENDAR\r\n");
        let Ok(parsed) = Calendar::from_str(&icalendar::parser::unfold(&wrapped)) else {
            return false;
        };
        let mut emitted = false;
        for component in parsed.iter() {
            if let CalendarComponent::Other(other) = component
                && other.component_kind() == "VTIMEZONE"
            {
                calendar.push(component.clone());
                emitted = true;
            }
        }
        emitted
    }

    let mut emitted_vtimezone = match &item.timezone_blob {
        Some(blob) => push_vtimezones(&mut calendar, blob),
        // No stored blob at all: synthesise the VTIMEZONE from the IANA id so a
        // gateway-originated event edits round-trip with a real zone definition.
        None => item
            .timezone
            .as_deref()
            .and_then(crate::timezone::render_vtimezone_block)
            .map(|synth| push_vtimezones(&mut calendar, &synth))
            .unwrap_or(false),
    };
    if !emitted_vtimezone
        && let Some(tzid) = &item.timezone
        && let Some(synth) = crate::timezone::render_vtimezone_block(tzid)
    {
        emitted_vtimezone = push_vtimezones(&mut calendar, &synth);
    }

    let mut event = Event::new();
    event.uid(&uid);
    event.timestamp(dtstamp);
    event.summary(&item.subject);

    if item.all_day {
        event.starts(item.start.naive_utc().date());
        let end_date = item.end.naive_utc().date();
        if end_date != item.start.naive_utc().date() {
            event.append_property(
                Property::new("DTEND", end_date.format("%Y%m%d").to_string())
                    .add_parameter("VALUE", "DATE")
                    .done(),
            );
        }
    } else if let Some(tzid) = &item.timezone
        && let Ok(tz) = tzid.parse::<Tz>()
    {
        if emitted_vtimezone {
            // A matching VTIMEZONE was produced (the authoritative stored blob
            // was re-emitted, or a canonical one was synthesised). Emit the
            // start/end as local wall-clock with a TZID referencing it.
            event.ends((item.end.with_timezone(&tz).naive_local(), tz));
            event.starts((item.start.with_timezone(&tz).naive_local(), tz));
        } else {
            // The IANA id parses as a Tz here, but neither the authoritative
            // blob nor the synthesised fallback produced a VTIMEZONE. Emit the
            // absolute UTC instant (with `Z`) so no orphan `DTSTART;TZID=...`
            // references a missing VTIMEZONE, keeping the iCalendar
            // RFC-5545-valid.
            event.ends(item.end);
            event.starts(item.start);
        }
    } else {
        event.ends(item.end);
        event.starts(item.start);
    }

    if !item.location.is_empty() {
        event.location(&item.location);
    }
    if !item.description.is_empty() {
        event.description(&item.description);
    }

    if let Some(rrule) = &item.rrule
        && !rrule.is_empty()
    {
        event.add_property("RRULE", rrule);
    }

    let has_tzid = item
        .timezone
        .as_ref()
        .is_some_and(|t| t.parse::<Tz>().is_ok());
    let all_exdates: Vec<String> = item
        .exdates
        .iter()
        .map(|v| {
            if item.all_day {
                v.format("%Y%m%d").to_string()
            } else if has_tzid {
                v.format("%Y%m%dT%H%M%S").to_string()
            } else {
                v.format("%Y%m%dT%H%M%SZ").to_string()
            }
        })
        .chain(item.exceptions.iter().filter(|v| v.deleted).map(|v| {
            if item.all_day {
                v.exception_start.format("%Y%m%d").to_string()
            } else if has_tzid {
                v.exception_start.format("%Y%m%dT%H%M%S").to_string()
            } else {
                v.exception_start.format("%Y%m%dT%H%M%SZ").to_string()
            }
        }))
        .sorted()
        .dedup()
        .collect();

    if !all_exdates.is_empty() {
        let exdate_str = all_exdates.join(",");
        if !item.all_day {
            if let Some(tzid) = &item.timezone
                && tzid.parse::<Tz>().is_ok()
            {
                event.append_property(
                    Property::new("EXDATE", &exdate_str)
                        .add_parameter("TZID", tzid)
                        .done(),
                );
            } else {
                event.append_property(Property::new("EXDATE", &exdate_str));
            }
        } else {
            event.append_property(
                Property::new("EXDATE", &exdate_str)
                    .add_parameter("VALUE", "DATE")
                    .done(),
            );
        }
    }

    if let Some(email) = &item.organizer_email {
        let mut org_prop = Property::new("ORGANIZER", normalize_mailto(email));
        if let Some(name) = &item.organizer_name {
            org_prop.add_parameter("CN", name);
        }
        event.append_property(org_prop.done());
    }

    for attendee in &item.attendees {
        if attendee.email.is_empty() {
            continue;
        }
        let cal_addr = normalize_mailto(&attendee.email);

        // RFC 6638 §7.1 SCHEDULE-AGENT is not modelled by the icalendar builder,
        // so emit the ATTENDEE property manually when set (e.g. "CLIENT" to keep
        // the server from auto-scheduling this attendee).
        if let Some(agent) = &attendee.schedule_agent
            && !agent.is_empty()
        {
            let mut prop = Property::new("ATTENDEE", cal_addr.as_str());
            prop.add_parameter("SCHEDULE-AGENT", agent);
            if let Some(name) = &attendee.name {
                prop.add_parameter("CN", name);
            }
            if let Some(kind) = attendee.attendee_type {
                let role = match kind {
                    2 => "OPT-PARTICIPANT",
                    3 => "NON-PARTICIPANT",
                    _ => "REQ-PARTICIPANT",
                };
                prop.add_parameter("ROLE", role);
            }
            if let Some(partstat) = &attendee.partstat {
                let ps = match partstat.to_uppercase().as_str() {
                    "ACCEPTED" => "ACCEPTED",
                    "DECLINED" => "DECLINED",
                    "TENTATIVE" => "TENTATIVE",
                    "DELEGATED" => "DELEGATED",
                    _ => "NEEDS-ACTION",
                };
                prop.add_parameter("PARTSTAT", ps);
            }
            event.append_property(prop.done());
            continue;
        }

        let mut cal_attendee = icalendar::Attendee::new(cal_addr);
        if let Some(name) = &attendee.name {
            cal_attendee = cal_attendee.cn(name.clone());
        }
        if let Some(kind) = attendee.attendee_type {
            let role = match kind {
                2 => icalendar::Role::OptParticipant,
                3 => icalendar::Role::NonParticipant,
                _ => icalendar::Role::ReqParticipant,
            };
            cal_attendee = cal_attendee.role(role);
        }
        if let Some(partstat) = &attendee.partstat {
            let ps = match partstat.to_uppercase().as_str() {
                "ACCEPTED" => icalendar::PartStat::Accepted,
                "DECLINED" => icalendar::PartStat::Declined,
                "TENTATIVE" => icalendar::PartStat::Tentative,
                "DELEGATED" => icalendar::PartStat::Delegated,
                _ => icalendar::PartStat::NeedsAction,
            };
            cal_attendee = cal_attendee.partstat(ps);
        }
        event.attendee(cal_attendee);
    }

    if !item.categories.is_empty() {
        for category in &item.categories {
            event.add_property("CATEGORIES", category);
        }
    }

    if let Some(busy) = item.busy_status {
        event.append_property(Property::new(
            "X-MICROSOFT-CDO-BUSYSTATUS",
            busy.to_string(),
        ));
        event.add_property("TRANSP", if busy == 0 { "TRANSPARENT" } else { "OPAQUE" });
    }
    if let Some(sensitivity) = item.sensitivity {
        event.class(match sensitivity {
            2 => Class::Private,
            3 => Class::Confidential,
            _ => Class::Public,
        });
    }
    if let Some(reminder) = item.reminder {
        let abs = reminder.abs();
        let trigger = if abs % 60 == 0 {
            -chrono::Duration::hours(abs as i64 / 60)
        } else {
            -chrono::Duration::minutes(abs as i64)
        };
        event.alarm(icalendar::Alarm::display("Reminder", trigger));
    }
    if let Some(v) = item.response_requested {
        event.append_property(Property::new(
            "X-MS-RESPONSE-REQUESTED",
            if v { "TRUE" } else { "FALSE" },
        ));
    }
    if let Some(v) = item.disallow_new_time_proposal {
        event.append_property(Property::new(
            "X-MS-DISALLOW-COUNTER",
            if v { "TRUE" } else { "FALSE" },
        ));
    }
    if let Some(v) = item.appointment_reply_time {
        event.append_property(Property::new(
            "X-MS-APPOINTMENT-REPLY-TIME",
            v.format("%Y%m%dT%H%M%SZ").to_string(),
        ));
    }
    if let Some(v) = item.meeting_status {
        event.append_property(Property::new("X-MS-MEETING-STATUS", v.to_string()));
    }
    if let Some(v) = item.response_type {
        event.append_property(Property::new("X-MS-RESPONSE-TYPE", v.to_string()));
    }
    if let Some(v) = &item.online_meeting_conf_link {
        event.append_property(Property::new("X-MS-OLK-CONFLINK", v));
    }
    if let Some(v) = &item.online_meeting_external_link {
        event.append_property(Property::new("X-MS-OLK-EXTERNALLINK", v));
    }
    if let Some(v) = &item.client_uid {
        event.append_property(Property::new("X-MS-CLIENT-UID", v));
    }
    if !item.all_day
        && let Some(v) = &item.timezone
    {
        event.append_property(Property::new("X-EAS-TIMEZONE", v));
    }

    calendar.push(event.done());

    for exception in item.exceptions.iter().filter(|v| !v.deleted) {
        let base_duration = item.end - item.start;
        let effective_all_day = exception.all_day.unwrap_or(item.all_day);
        let effective_start = exception.start.unwrap_or(exception.exception_start);
        let effective_end = exception
            .end
            .unwrap_or_else(|| effective_start + base_duration);

        let mut ex_event = Event::new();
        ex_event.uid(&uid);
        ex_event.timestamp(dtstamp);
        ex_event.summary(exception.subject.as_deref().unwrap_or(&item.subject));

        if effective_all_day {
            ex_event.append_property(
                Property::new(
                    "RECURRENCE-ID",
                    exception.exception_start.format("%Y%m%d").to_string(),
                )
                .add_parameter("VALUE", "DATE")
                .done(),
            );
            ex_event.starts(effective_start.naive_utc().date());
            let end_date = effective_end.naive_utc().date();
            if end_date != effective_start.naive_utc().date() {
                ex_event.append_property(
                    Property::new("DTEND", end_date.format("%Y%m%d").to_string())
                        .add_parameter("VALUE", "DATE")
                        .done(),
                );
            }
        } else if let Some(tzid) = &item.timezone
            && let Ok(tz) = tzid.parse::<Tz>()
        {
            ex_event.append_property(
                Property::new(
                    "RECURRENCE-ID",
                    exception
                        .exception_start
                        .with_timezone(&tz)
                        .format("%Y%m%dT%H%M%S")
                        .to_string(),
                )
                .add_parameter("TZID", tzid)
                .done(),
            );
            ex_event.starts((effective_start.with_timezone(&tz).naive_local(), tz));
            ex_event.ends((effective_end.with_timezone(&tz).naive_local(), tz));
        } else {
            ex_event.append_property(Property::new(
                "RECURRENCE-ID",
                exception
                    .exception_start
                    .format("%Y%m%dT%H%M%SZ")
                    .to_string(),
            ));
            ex_event.starts(effective_start);
            ex_event.ends(effective_end);
        }

        if let Some(location) = exception.location.as_deref().or(Some(&item.location))
            && !location.is_empty()
        {
            ex_event.location(location);
        }
        if let Some(description) = exception.description.as_deref().or(Some(&item.description))
            && !description.is_empty()
        {
            ex_event.description(description);
        }
        if let Some(attendees) = &exception.attendees {
            for attendee in attendees {
                if attendee.email.is_empty() {
                    continue;
                }
                let mut cal_attendee = icalendar::Attendee::new(normalize_mailto(&attendee.email));
                if let Some(name) = &attendee.name {
                    cal_attendee = cal_attendee.cn(name.clone());
                }
                if let Some(kind) = attendee.attendee_type {
                    let role = match kind {
                        2 => icalendar::Role::OptParticipant,
                        3 => icalendar::Role::NonParticipant,
                        _ => icalendar::Role::ReqParticipant,
                    };
                    cal_attendee = cal_attendee.role(role);
                }
                if let Some(partstat) = &attendee.partstat {
                    let ps = match partstat.to_uppercase().as_str() {
                        "ACCEPTED" => icalendar::PartStat::Accepted,
                        "DECLINED" => icalendar::PartStat::Declined,
                        "TENTATIVE" => icalendar::PartStat::Tentative,
                        "DELEGATED" => icalendar::PartStat::Delegated,
                        _ => icalendar::PartStat::NeedsAction,
                    };
                    cal_attendee = cal_attendee.partstat(ps);
                }
                ex_event.attendee(cal_attendee);
            }
        }
        if let Some(categories) = &exception.categories
            && !categories.is_empty()
        {
            for category in categories {
                ex_event.add_property("CATEGORIES", category);
            }
        }
        if let Some(busy) = exception.busy_status {
            ex_event.append_property(Property::new(
                "X-MICROSOFT-CDO-BUSYSTATUS",
                busy.to_string(),
            ));
        }
        if let Some(sensitivity) = exception.sensitivity {
            ex_event.class(match sensitivity {
                2 => Class::Private,
                3 => Class::Confidential,
                _ => Class::Public,
            });
        }
        if let Some(reminder) = exception.reminder {
            let abs = reminder.abs();
            let trigger = if abs % 60 == 0 {
                -chrono::Duration::hours(abs as i64 / 60)
            } else {
                -chrono::Duration::minutes(abs as i64)
            };
            ex_event.alarm(icalendar::Alarm::display("Reminder", trigger));
        }
        if let Some(v) = exception.appointment_reply_time {
            ex_event.append_property(Property::new(
                "X-MS-APPOINTMENT-REPLY-TIME",
                v.format("%Y%m%dT%H%M%SZ").to_string(),
            ));
        }
        if let Some(v) = exception.meeting_status {
            ex_event.append_property(Property::new("X-MS-MEETING-STATUS", v.to_string()));
        }
        if let Some(v) = exception.response_type {
            ex_event.append_property(Property::new("X-MS-RESPONSE-TYPE", v.to_string()));
        }

        calendar.push(ex_event.done());
    }

    calendar.to_string()
}

fn parse_ical_param(key: &str, name: &str) -> Option<String> {
    for part in key.split(';').skip(1) {
        let (k, v) = part.split_once('=')?;
        if k.eq_ignore_ascii_case(name) {
            return Some(v.to_string());
        }
    }
    None
}

fn parse_ical_actor_line(key: &str, value: &str) -> (Option<String>, Option<String>) {
    let name = parse_ical_param(key, "CN").map(|v| unescape_ical_text(&v));
    let raw_email = value
        .strip_prefix("mailto:")
        .or_else(|| value.strip_prefix("MAILTO:"))
        .unwrap_or(value);
    let email = nfc(raw_email);
    (name, Some(email))
}

fn normalize_mailto(email: &str) -> String {
    let nfc_email = nfc(email);
    if nfc_email.to_ascii_lowercase().starts_with("mailto:") {
        nfc_email
    } else {
        format!("mailto:{nfc_email}")
    }
}

pub fn partstat_to_status(value: &str) -> u8 {
    match value {
        "ACCEPTED" => 3,
        "DECLINED" => 4,
        "TENTATIVE" => 2,
        _ => 5,
    }
}

pub fn status_to_partstat(value: u8) -> String {
    match value {
        3 => "ACCEPTED".to_string(),
        4 => "DECLINED".to_string(),
        2 => "TENTATIVE".to_string(),
        _ => "NEEDS-ACTION".to_string(),
    }
}

fn class_to_sensitivity(value: &str) -> Option<u8> {
    match value.to_ascii_uppercase().as_str() {
        "PRIVATE" => Some(2),
        "CONFIDENTIAL" => Some(3),
        "PUBLIC" => Some(0),
        _ => None,
    }
}

pub fn parse_eas_sync_mutations(xml: &str) -> Result<Vec<EasSyncMutation>> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);
    let mut buf = Vec::new();
    let mut stack: Vec<SmallVec<[u8; 16]>> = Vec::new();
    let mut current_kind: Option<EasOpKind> = None;
    let mut current = EasBuilder::default();
    let mut out = Vec::new();
    // Accumulated text of the current leaf element, with entity references
    // resolved. Flushed into the current field when the leaf ends.
    let mut leaf_text = String::new();

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) => {
                let local = e.name().local_name();
                let name: &[u8] = local.as_ref().as_bytes();
                let tag: SmallVec<[u8; 16]> = SmallVec::from_slice(name);
                if matches!(name, b"Add" | b"Change" | b"Delete") {
                    current_kind = Some(match name {
                        b"Add" => EasOpKind::Add,
                        b"Change" => EasOpKind::Change,
                        _ => EasOpKind::Delete,
                    });
                    current = EasBuilder::default();
                } else if name == b"Exception" {
                    current.current_exception = Some(CalendarException::default());
                } else if name == b"Recurrence" {
                    current.recurrence.is_empty = false;
                } else if name == b"Attachments" {
                    current.in_attachments = true;
                } else if name == b"Attachment" && current.in_attachments {
                    current.in_attachment = true;
                    current.current_att = Some(ParsedEasAttachmentAdd {
                        display_name: String::new(),
                        method: 1,
                        estimated_data_size: 0,
                        content_type: String::new(),
                        content_id: None,
                        content_location: None,
                        is_inline: false,
                        content_base64: String::new(),
                    });
                } else if current.in_attachment {
                    match name {
                        b"DisplayName" => current.in_att_display_name = true,
                        b"Method" => current.in_att_method = true,
                        b"EstimatedDataSize" => current.in_att_estimated_data_size = true,
                        b"ContentType" => current.in_att_content_type = true,
                        b"ContentId" => current.in_att_content_id = true,
                        b"ContentLocation" => current.in_att_content_location = true,
                        b"IsInline" => current.in_att_is_inline = true,
                        b"Data" => current.in_att_data = true,
                        b"FileReference" => current.in_att_file_reference = true,
                        _ => {}
                    }
                }
                stack.push(tag);
                leaf_text.clear();
            }
            Ok(Event::Empty(e)) => {
                let local = e.name().local_name();
                let name: &[u8] = local.as_ref().as_bytes();
                let tag: SmallVec<[u8; 16]> = SmallVec::from_slice(name);
                if name == b"Recurrence" {
                    current.recurrence.is_empty = true;
                }
                stack.push(tag);
                stack.pop();
                leaf_text.clear();
            }
            Ok(Event::Text(t)) => {
                if current_kind.is_some() {
                    leaf_text.push_str(t.as_ref());
                }
            }
            Ok(Event::GeneralRef(r)) => {
                if current_kind.is_some() {
                    leaf_text.push_str(&resolve_xml_reference(r.as_ref()));
                }
            }
            Ok(Event::End(e)) => {
                let local = e.name().local_name();
                let name: &[u8] = local.as_ref().as_bytes();
                if name == b"Attendee"
                    && let Some(attendee) = current.current_attendee.take()
                    && !attendee.email.is_empty()
                {
                    if let Some(ex) = current.current_exception.as_mut() {
                        ex.attendees.get_or_insert_with(Vec::new).push(attendee);
                    } else {
                        current.attendees.push(attendee);
                    }
                }
                if name == b"Exception"
                    && let Some(exception) = current.current_exception.take()
                {
                    current.exceptions.push(exception);
                }
                if name == b"Attachments" {
                    current.in_attachments = false;
                } else if name == b"Attachment" && current.in_attachment {
                    current.in_attachment = false;
                    if let Some(att) = current.current_att.take()
                        && (!att.content_base64.is_empty() || !att.display_name.is_empty())
                    {
                        current.attachment_adds.push(att);
                    }
                    current.in_att_display_name = false;
                    current.in_att_method = false;
                    current.in_att_estimated_data_size = false;
                    current.in_att_content_type = false;
                    current.in_att_content_id = false;
                    current.in_att_content_location = false;
                    current.in_att_is_inline = false;
                    current.in_att_data = false;
                    current.in_att_file_reference = false;
                }
                if matches!(name, b"Add" | b"Change" | b"Delete") {
                    match current_kind.take() {
                        Some(EasOpKind::Add) => {
                            let client_id = current.client_id.clone();
                            let attachment_adds = std::mem::take(&mut current.attachment_adds);
                            let builder = std::mem::take(&mut current);
                            out.push(EasSyncMutation::Add {
                                client_id,
                                item: builder.into_item()?,
                                attachment_adds,
                            });
                        }
                        Some(EasOpKind::Change) => {
                            let server_id = current.server_id.clone().unwrap_or_default();
                            let instance_id = current.instance_id;
                            let attachment_adds = std::mem::take(&mut current.attachment_adds);
                            let attachment_deletes =
                                std::mem::take(&mut current.attachment_deletes);
                            let builder = std::mem::take(&mut current);
                            out.push(EasSyncMutation::Change {
                                server_id,
                                instance_id,
                                patch: builder.into_patch(),
                                attachment_adds,
                                attachment_deletes,
                            });
                        }
                        Some(EasOpKind::Delete) => {
                            let server_id = current.server_id.clone().unwrap_or_default();
                            let instance_id = current.instance_id;
                            let _builder = std::mem::take(&mut current);
                            out.push(EasSyncMutation::Delete {
                                server_id,
                                instance_id,
                            });
                        }
                        None => {}
                    }
                }
                // Flush accumulated leaf text (entity references already
                // resolved) into the current field before unwinding the stack.
                if current_kind.is_some() && !leaf_text.is_empty() {
                    assign_eas_field(&mut current, &stack, std::mem::take(&mut leaf_text))?;
                }
                leaf_text.clear();
                stack.pop();
            }
            Ok(Event::Eof) => break,
            Err(e) => return Err(anyhow!("failed parsing EAS Sync body: {e}")),
            _ => {}
        }
        buf.clear();
    }

    Ok(out)
}

/// Assign accumulated leaf text to the appropriate `EasBuilder` field based
/// on the leaf element name (`stack.last()`) and its ancestor context.
fn assign_eas_field(
    current: &mut EasBuilder,
    stack: &[SmallVec<[u8; 16]>],
    value: String,
) -> Result<()> {
    let last_tag = stack.last().map(|v| v.as_slice());
    match last_tag {
        Some(b"ClientId") => current.client_id = Some(value),
        Some(b"ServerId") if !stack.iter().any(|v| v.as_slice() == b"Exception") => {
            current.server_id = Some(value);
        }
        Some(b"InstanceId") => {
            current.instance_id = parse_datetime(&value);
        }
        Some(b"Subject") => {
            if let Some(ex) = current.current_exception.as_mut() {
                ex.subject = Some(value);
            } else {
                current.subject = Some(value);
            }
        }
        Some(b"Location") => {
            if let Some(ex) = current.current_exception.as_mut() {
                ex.location = Some(value);
            } else {
                current.location = Some(value);
            }
        }
        Some(b"DisplayName") if stack.iter().any(|v| v.as_slice() == b"Location") => {
            if let Some(ex) = current.current_exception.as_mut() {
                ex.location = Some(value);
            } else {
                current.location = Some(value);
            }
        }
        Some(b"Timezone") => {
            // EAS Timezone is a base64 Windows timezone blob (MS-ASSETTINGS).
            // Convert to an IANA id so render_ics emits DTSTART;TZID=<iana>.
            let tz = crate::timezone::eas_timezone_blob_to_iana(&value).unwrap_or(value);
            current.timezone = Some(tz);
        }
        Some(b"DtStamp") => current.dtstamp = parse_datetime(&value),
        Some(b"StartTime") => {
            if let Some(ex) = current.current_exception.as_mut() {
                ex.start = parse_datetime(&value);
            } else {
                current.start = parse_datetime(&value);
            }
        }
        Some(b"EndTime") => {
            if let Some(ex) = current.current_exception.as_mut() {
                ex.end = parse_datetime(&value);
            } else {
                current.end = parse_datetime(&value);
            }
        }
        Some(b"AllDayEvent") => {
            if let Some(ex) = current.current_exception.as_mut() {
                ex.all_day = Some(value == "1");
            } else {
                current.all_day = Some(value == "1");
            }
        }
        Some(b"UID") => current.uid = Some(value),
        Some(b"ClientUid") => current.client_uid = Some(value),
        Some(b"OrganizerName") => current.organizer_name = Some(value),
        Some(b"OrganizerEmail") => current.organizer_email = Some(nfc(&value)),
        Some(b"BusyStatus") => {
            if let Some(ex) = current.current_exception.as_mut() {
                ex.busy_status = value.parse().ok();
            } else {
                current.busy_status = value.parse().ok();
            }
        }
        Some(b"Sensitivity") => {
            if let Some(ex) = current.current_exception.as_mut() {
                ex.sensitivity = value.parse().ok();
            } else {
                current.sensitivity = value.parse().ok();
            }
        }
        Some(b"Reminder") => {
            if let Some(ex) = current.current_exception.as_mut() {
                ex.reminder = value.parse().ok();
            } else {
                current.reminder = value.parse().ok();
            }
        }
        Some(b"ResponseRequested") => current.response_requested = Some(value == "1"),
        Some(b"DisallowNewTimeProposal") => current.disallow_new_time_proposal = Some(value == "1"),
        Some(b"AppointmentReplyTime") => {
            if let Some(ex) = current.current_exception.as_mut() {
                ex.appointment_reply_time = parse_datetime(&value);
            } else {
                current.appointment_reply_time = parse_datetime(&value);
            }
        }
        Some(b"MeetingStatus") => {
            if let Some(ex) = current.current_exception.as_mut() {
                ex.meeting_status = value.parse().ok();
            } else {
                current.meeting_status = value.parse().ok();
            }
        }
        Some(b"ResponseType") => {
            if let Some(ex) = current.current_exception.as_mut() {
                ex.response_type = value.parse().ok();
            } else {
                current.response_type = value.parse().ok();
            }
        }
        Some(b"OnlineMeetingConfLink") => current.online_meeting_conf_link = Some(value),
        Some(b"OnlineMeetingExternalLink") => current.online_meeting_external_link = Some(value),
        Some(b"Category") => {
            if let Some(ex) = current.current_exception.as_mut() {
                ex.categories.get_or_insert_with(Vec::new).push(value);
            } else {
                current.categories.push(value);
            }
        }
        Some(b"Name") if stack.iter().any(|v| v.as_slice() == b"Attendee") => {
            current
                .current_attendee
                .get_or_insert_with(Attendee::default)
                .name = Some(value);
        }
        Some(b"Email") if stack.iter().any(|v| v.as_slice() == b"Attendee") => {
            current
                .current_attendee
                .get_or_insert_with(Attendee::default)
                .email = value;
        }
        Some(b"AttendeeType") if stack.iter().any(|v| v.as_slice() == b"Attendee") => {
            current
                .current_attendee
                .get_or_insert_with(Attendee::default)
                .attendee_type = value.parse().ok();
        }
        Some(b"AttendeeStatus") if stack.iter().any(|v| v.as_slice() == b"Attendee") => {
            let attendee = current
                .current_attendee
                .get_or_insert_with(Attendee::default);
            let status: Option<u8> = value.parse().ok();
            attendee.attendee_status = status;
            attendee.partstat = status.map(status_to_partstat);
        }
        Some(b"Deleted") if stack.iter().any(|v| v.as_slice() == b"Exception") => {
            if let Some(ex) = current.current_exception.as_mut() {
                ex.deleted = value == "1";
            }
        }
        Some(b"ExceptionStartTime") if stack.iter().any(|v| v.as_slice() == b"Exception") => {
            if let Some(ex) = current.current_exception.as_mut() {
                ex.exception_start =
                    parse_datetime(&value).ok_or_else(|| anyhow!("invalid ExceptionStartTime"))?;
            }
        }
        Some(b"Data") if stack.iter().any(|v| v.as_slice() == b"Body") => {
            if let Some(ex) = current.current_exception.as_mut() {
                ex.description = Some(value);
            } else {
                current.description = Some(value);
            }
        }
        Some(b"Type") if stack.iter().any(|v| v.as_slice() == b"Recurrence") => {
            current.recurrence.kind = value.parse().ok()
        }
        Some(b"Interval") if stack.iter().any(|v| v.as_slice() == b"Recurrence") => {
            current.recurrence.interval = value.parse().ok()
        }
        Some(b"DayOfWeek") if stack.iter().any(|v| v.as_slice() == b"Recurrence") => {
            current.recurrence.day_of_week = Some(value)
        }
        Some(b"DayOfMonth") if stack.iter().any(|v| v.as_slice() == b"Recurrence") => {
            current.recurrence.day_of_month = value.parse().ok()
        }
        Some(b"WeekOfMonth") if stack.iter().any(|v| v.as_slice() == b"Recurrence") => {
            current.recurrence.week_of_month = value.parse().ok()
        }
        Some(b"MonthOfYear") if stack.iter().any(|v| v.as_slice() == b"Recurrence") => {
            current.recurrence.month_of_year = value.parse().ok()
        }
        Some(b"Until") if stack.iter().any(|v| v.as_slice() == b"Recurrence") => {
            current.recurrence.until = Some(value)
        }
        Some(b"Occurrences") if stack.iter().any(|v| v.as_slice() == b"Recurrence") => {
            current.recurrence.occurrences = value.parse().ok()
        }
        Some(b"FirstDayOfWeek") if stack.iter().any(|v| v.as_slice() == b"Recurrence") => {
            current.recurrence.first_day_of_week = value.parse().ok()
        }
        Some(b"CalendarType") if stack.iter().any(|v| v.as_slice() == b"Recurrence") => {
            current.recurrence.calendar_type = value.parse().ok()
        }
        Some(b"DisplayName") if current.in_att_display_name && current.current_att.is_some() => {
            if let Some(att) = current.current_att.as_mut() {
                att.display_name = value;
            }
        }
        Some(b"Method") if current.in_att_method && current.current_att.is_some() => {
            if let Some(att) = current.current_att.as_mut() {
                att.method = value.parse().unwrap_or(1);
            }
        }
        Some(b"EstimatedDataSize")
            if current.in_att_estimated_data_size && current.current_att.is_some() =>
        {
            if let Some(att) = current.current_att.as_mut() {
                att.estimated_data_size = value.parse().unwrap_or(0);
            }
        }
        Some(b"ContentType") if current.in_att_content_type && current.current_att.is_some() => {
            if let Some(att) = current.current_att.as_mut() {
                att.content_type = value;
            }
        }
        Some(b"ContentId") if current.in_att_content_id && current.current_att.is_some() => {
            if let Some(att) = current.current_att.as_mut() {
                att.content_id = Some(value);
            }
        }
        Some(b"ContentLocation")
            if current.in_att_content_location && current.current_att.is_some() =>
        {
            if let Some(att) = current.current_att.as_mut() {
                att.content_location = Some(value);
            }
        }
        Some(b"IsInline") if current.in_att_is_inline && current.current_att.is_some() => {
            if let Some(att) = current.current_att.as_mut() {
                att.is_inline = value == "1" || value.eq_ignore_ascii_case("true");
            }
        }
        Some(b"Data") if current.in_att_data && current.current_att.is_some() => {
            if let Some(att) = current.current_att.as_mut() {
                att.content_base64 = value;
            }
        }
        Some(b"FileReference") if current.in_att_file_reference => {
            current.attachment_deletes.push(value);
        }
        _ => {}
    }
    Ok(())
}

fn extract_ews_field_doc(doc: &Document, tag: &[u8]) -> Option<String> {
    let tag_str = std::str::from_utf8(tag).ok()?;
    doc.descendants()
        .filter(|n| n.is_element() && n.tag_name().name() == tag_str)
        .find_map(|n| n.text().map(|s| s.to_string()))
}

/// Extract the request-wide timezone from the SOAP `t:TimeZoneContext` header
/// ([MS-OXWSCORE] §2.2.1.12): a `t:TimeZoneDefinition` child whose `Id`
/// attribute (or element text, for the gateway's own legacy emits) carries the
/// Windows timezone id Outlook serialises naive `xs:dateTime` values
/// against. Searched document-wide because the header rides in the SOAP
/// envelope, outside the operation element the per-item zone readers see.
fn extract_ews_timezone_context(doc: &Document) -> Option<String> {
    doc.descendants()
        .filter(|n| n.is_element() && n.tag_name().name() == "TimeZoneContext")
        .flat_map(|ctx| ctx.descendants())
        .find(|n| n.is_element() && n.tag_name().name() == "TimeZoneDefinition")
        .and_then(|def| {
            def.attribute("Id")
                .map(|s| s.to_string())
                .or_else(|| def.text().map(|s| s.to_string()))
                .filter(|s| !s.trim().is_empty())
        })
}

/// Extract a timezone identifier from a `t:StartTimeZone`/`t:EndTimeZone`/
/// `t:MeetingTimeZone` element. The canonical EWS wire form carries the id in
/// an attribute — `Id` (StartTimeZone/EndTimeZone) or `TimeZoneName`
/// (MeetingTimeZone, per the legacy `SerializableTimeZone`) — but older
/// server fixtures also emit a bare Windows name as element text or as a
/// `<t:Value>` child. Read text first (back-compat with the gateway's own
/// legacy emit and the `<Value>` child of a `TimeZoneDefinition`), then fall
/// back to the attribute so a real Outlook `GetItem`/`CreateItem` echo
/// (`<t:StartTimeZone Id="Pacific Standard Time"/>`) round-trips.
///
/// The newer `t:StartTimeZoneId`/`t:EndTimeZoneId` elements ([MS-OXWSMTGS]
/// §2.2.2.41/§2.2.2.13) carry the timezone id as plain element TEXT with no
/// attributes — `extract_ews_field_doc` handles those directly, which is why
/// `parse_ews_calendar_item` reads them as ordinary text fields rather than
/// through this helper.
fn extract_ews_timezone_field_doc(doc: &Document, tag: &[u8]) -> Option<String> {
    let tag_str = std::str::from_utf8(tag).ok()?;
    let tz_attr = if tag_str == "MeetingTimeZone" {
        "TimeZoneName"
    } else {
        "Id"
    };
    doc.descendants()
        .filter(|n| n.is_element() && n.tag_name().name() == tag_str)
        .find_map(|n| {
            // Non-empty element text (bare Windows name, or an inline
            // <t:Value> child) takes precedence.
            if let Some(t) = n.text().filter(|s| !s.trim().is_empty()) {
                return Some(t.to_string());
            }
            for child in n.children().filter(|c| c.is_element()) {
                if let Some(t) = child.text().filter(|s| !s.trim().is_empty()) {
                    return Some(t.to_string());
                }
            }
            // Canonical attribute form: <t:StartTimeZone Id="..."/> or
            // <t:MeetingTimeZone TimeZoneName="..."/>.
            n.attribute(tz_attr).map(|s| s.to_string())
        })
}

pub(crate) fn extract_ews_timezone_field(xml: &str, tag: &[u8]) -> Option<String> {
    match Document::parse(xml) {
        Ok(doc) => extract_ews_timezone_field_doc(&doc, tag),
        // `SetItemField` payload fragments carry prefixes whose `xmlns`
        // declarations live on the SOAP envelope ancestor, so the strict
        // parser rejects them outright; scan leniently instead (same
        // precedence: element text / `Value` child / `Id` attribute).
        Err(_) => extract_ews_timezone_field_lenient(xml, tag),
    }
}

/// Lenient [`extract_ews_timezone_field`] for namespace-undeclared
/// fragments, with the strict walk's precedence: the first matching
/// element's direct text (bare id), then an inline `Value` child's text,
/// then the `Id`/`TimeZoneName` attribute. Attribute values unescape the
/// standard entity set.
fn extract_ews_timezone_field_lenient(xml: &str, tag: &[u8]) -> Option<String> {
    let tag_str = std::str::from_utf8(tag).ok()?;
    let attr_name = if tag == b"MeetingTimeZone" {
        "TimeZoneName"
    } else {
        "Id"
    };
    if let Some(t) = extract_ews_field_lenient(xml, tag).filter(|t| !t.trim().is_empty()) {
        return Some(t);
    }
    if let Some(t) = extract_ews_field_lenient(xml, b"Value").filter(|t| !t.trim().is_empty()) {
        return Some(t);
    }
    let mut reader = Reader::from_str(xml);
    let mut buf = Vec::new();
    loop {
        match reader.read_event_into(&mut buf) {
            // `<t:StartTimeZone Id="..."/>` is self-closing on the wire:
            // quick-xml reports it as Empty, not Start.
            Ok(event @ (Event::Start(_) | Event::Empty(_))) => {
                let e = match event {
                    Event::Start(e) | Event::Empty(e) => e,
                    _ => unreachable!(),
                };
                if local_name_str(e.name().as_ref()) == tag_str {
                    for attr in e.attributes().flatten() {
                        if attr.key.local_name().as_ref() == attr_name
                            && let Ok(v) = attr.normalized_value(XmlVersion::Implicit1_0)
                        {
                            return Some(v.to_string());
                        }
                    }
                }
            }
            Ok(Event::Eof) => return None,
            Ok(_) => {}
            Err(_) => return None,
        }
    }
}

/// The timezone an EWS `UpdateItem` request resolved to — a TRI-state,
/// because the two failure halves need different answers: `Absent` (the
/// request carries no zone at all) leaves a naive value readable as UTC —
/// the only defensible reading — while `Unresolved` (the request SUPPLIED
/// a zone id that maps to no zone the gateway knows) leaves a naive value
/// with no honest instant and must fail closed instead of silently
/// drifting by the zone's offset (the §14 offset-drift failure).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EwsRequestZone {
    /// No zone anywhere in the request: naive values read as UTC.
    Absent,
    /// The supplied zone id resolved to a chrono-tz zone.
    Resolved(Tz),
    /// A zone id was supplied but does not resolve.
    Unresolved,
}

/// Resolve the timezone an EWS `UpdateItem` request intends its naive
/// (offset-less) `Start`/`End` values to be read against, applying the SAME
/// precedence `parse_ews_calendar_item` uses for items: item-level zone
/// elements first (`t:StartTimeZone`/`t:StartTimeZoneId`/`t:MeetingTimeZone`/
/// `t:EndTimeZone`/`t:EndTimeZoneId` — in an update these arrive inside the
/// `SetItemField` payloads), then the request-wide SOAP `t:TimeZoneContext`
/// header ([MS-OXWSCORE] §2.2.1.12) Outlook sends alongside naive values.
/// `EwsRequestZone::Absent` when the request carries no zone at all — a naive
/// value then reads as UTC, which remains the only defensible reading — and
/// `EwsRequestZone::Unresolved` when a zone WAS supplied but maps to no known
/// zone, so a naive value fails closed instead of drifting. Values that carry
/// an explicit `Z`/offset are unaffected by the resolved zone.
pub(crate) fn ews_update_request_zone(body: &str) -> EwsRequestZone {
    let raw = extract_ews_timezone_field(body, b"StartTimeZone")
        .or_else(|| extract_ews_field(body, b"StartTimeZoneId"))
        .or_else(|| extract_ews_timezone_field(body, b"MeetingTimeZone"))
        .or_else(|| extract_ews_timezone_field(body, b"EndTimeZone"))
        .or_else(|| extract_ews_field(body, b"EndTimeZoneId"))
        .or_else(|| {
            let doc = Document::parse(body).ok()?;
            extract_ews_timezone_context(&doc)
        });
    match raw {
        None => EwsRequestZone::Absent,
        Some(raw) => match normalize_timezone_to_iana(&raw).parse::<Tz>() {
            Ok(tz) => EwsRequestZone::Resolved(tz),
            Err(_) => EwsRequestZone::Unresolved,
        },
    }
}

fn extract_ews_fields_doc(doc: &Document, tag: &[u8]) -> Vec<String> {
    let tag_str = std::str::from_utf8(tag).ok().unwrap_or_default();
    doc.descendants()
        .filter(|n| n.is_element() && n.tag_name().name() == tag_str)
        .filter_map(|n| n.text().map(|s| s.to_string()))
        .collect()
}
/// Prefix-agnostic fallback extractor for XML fragments whose namespace
/// prefixes are declared on ancestors OUTSIDE the fragment — e.g. the
/// `SetItemField` payloads `parse_item_changes` collects: they carry `t:`
/// names but the `xmlns:t` declaration lives on the SOAP envelope.
/// `roxmltree::Document::parse` rejects undeclared prefixes, so the strict
/// path returns `None` for exactly the fragments UpdateItem field
/// application lives on. The scan matches LOCAL names (the same
/// prefix-agnostic convention as the EAS request readers) and returns the
/// first matching element's direct text.
fn extract_ews_field_lenient(xml: &str, tag: &[u8]) -> Option<String> {
    let mut reader = Reader::from_str(xml);
    // No trimming: the strict path (`roxmltree::Node::text`) returns raw
    // text, and trimming would also eat the spaces around entity refs
    // ("Lunch & learn" would collapse to "Lunch&learn").
    reader.config_mut().trim_text(false);
    let mut buf = Vec::new();
    let mut depth = 0usize;
    let mut out: Option<String> = None;
    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) => {
                let local = local_name_str(e.name().as_ref());
                if depth == 0 {
                    if local.as_bytes() == tag {
                        depth = 1;
                    }
                } else {
                    depth += 1;
                }
            }
            Ok(Event::Text(t)) => {
                if depth == 1 {
                    out.get_or_insert_with(String::new).push_str(t.as_ref());
                }
            }
            Ok(Event::GeneralRef(r)) => {
                if depth == 1 {
                    out.get_or_insert_with(String::new)
                        .push_str(&resolve_xml_reference(r.as_ref()));
                }
            }
            Ok(Event::End(_)) => {
                if depth > 0 {
                    depth -= 1;
                    if depth == 0 && out.is_some() {
                        break;
                    }
                }
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
        buf.clear();
    }
    out
}

/// Plural form of [`extract_ews_field_lenient`]: every matching element's
/// direct text, in document order.
fn extract_ews_fields_lenient(xml: &str, tag: &[u8]) -> Vec<String> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(false);
    let mut buf = Vec::new();
    let mut depth = 0usize;
    let mut out = Vec::new();
    let mut current: Option<usize> = None;
    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) => {
                let local = local_name_str(e.name().as_ref());
                if depth == 0 {
                    if local.as_bytes() == tag {
                        depth = 1;
                        out.push(String::new());
                        current = Some(out.len() - 1);
                    }
                } else {
                    depth += 1;
                }
            }
            Ok(Event::Text(t)) => {
                if depth == 1
                    && let Some(i) = current
                {
                    out[i].push_str(t.as_ref());
                }
            }
            Ok(Event::GeneralRef(r)) => {
                if depth == 1
                    && let Some(i) = current
                {
                    out[i].push_str(&resolve_xml_reference(r.as_ref()));
                }
            }
            Ok(Event::End(_)) => {
                if depth > 0 {
                    depth -= 1;
                    if depth == 0 {
                        current = None;
                    }
                }
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
        buf.clear();
    }
    out
}

fn local_name_str(name: &str) -> String {
    name.rsplit(':').next().unwrap_or(name).to_string()
}

pub fn extract_ews_field(xml: &str, tag: &[u8]) -> Option<String> {
    match Document::parse(xml) {
        Ok(doc) => extract_ews_field_doc(&doc, tag),
        Err(_) => extract_ews_field_lenient(xml, tag),
    }
}

pub fn extract_ews_fields(xml: &str, tag: &[u8]) -> Vec<String> {
    let doc = match Document::parse(xml) {
        Ok(d) => d,
        Err(_) => return extract_ews_fields_lenient(xml, tag),
    };
    extract_ews_fields_doc(&doc, tag)
}

fn parse_ews_month(value: &str) -> Option<u32> {
    match value {
        "January" => Some(1),
        "February" => Some(2),
        "March" => Some(3),
        "April" => Some(4),
        "May" => Some(5),
        "June" => Some(6),
        "July" => Some(7),
        "August" => Some(8),
        "September" => Some(9),
        "October" => Some(10),
        "November" => Some(11),
        "December" => Some(12),
        _ => None,
    }
}

fn parse_ews_days_mask(value: &str) -> (String, Option<i32>) {
    let mut mask = 0u8;
    let mut ordinal = None;
    for token in value.split_whitespace() {
        match token {
            "Sunday" => mask |= 1,
            "Monday" => mask |= 2,
            "Tuesday" => mask |= 4,
            "Wednesday" => mask |= 8,
            "Thursday" => mask |= 16,
            "Friday" => mask |= 32,
            "Saturday" => mask |= 64,
            "First" => ordinal = Some(1),
            "Second" => ordinal = Some(2),
            "Third" => ordinal = Some(3),
            "Fourth" => ordinal = Some(4),
            "Last" => ordinal = Some(-1),
            _ => {}
        }
    }
    (mask.to_string(), ordinal)
}

pub fn parse_ews_recurrence(xml: &str) -> Option<String> {
    if !xml.contains("Recurrence") {
        return None;
    }

    let doc = Document::parse(xml).ok()?;

    let interval = extract_ews_field_doc(&doc, b"Interval")
        .and_then(|v| v.parse::<u32>().ok())
        .unwrap_or(1);

    let freq = if xml.contains("DailyRecurrence") {
        Frequency::Daily
    } else if xml.contains("WeeklyRecurrence") {
        Frequency::Weekly
    } else if xml.contains("AbsoluteMonthlyRecurrence") || xml.contains("RelativeMonthlyRecurrence")
    {
        Frequency::Monthly
    } else if xml.contains("AbsoluteYearlyRecurrence") || xml.contains("RelativeYearlyRecurrence") {
        Frequency::Yearly
    } else {
        return None;
    };

    let mut rule = RRule::new(freq);

    if interval > 1 {
        rule = rule.interval(interval as u16);
    }

    if let Some(days) = extract_ews_field_doc(&doc, b"DaysOfWeek") {
        let (mask_str, ordinal_opt) = parse_ews_days_mask(&days);
        if let Ok(value) = mask_str.parse::<u32>() {
            let byday = mask_to_byday(value);
            if !byday.is_empty() {
                let nweekdays: Vec<NWeekday> = if let Some(ordinal) = ordinal_opt {
                    let ord = match ordinal {
                        -1 => -1i16,
                        n => n as i16,
                    };
                    byday
                        .into_iter()
                        .filter_map(|code| {
                            day_code_to_weekday(code).map(|wd| NWeekday::Nth(ord, wd))
                        })
                        .collect()
                } else if xml.contains("RelativeMonthlyRecurrence")
                    || xml.contains("RelativeYearlyRecurrence")
                {
                    let ord = extract_ews_field_doc(&doc, b"DayOfWeekIndex")
                        .and_then(|v| match v.as_str() {
                            "First" => Some(1i16),
                            "Second" => Some(2),
                            "Third" => Some(3),
                            "Fourth" => Some(4),
                            "Last" => Some(-1),
                            _ => None,
                        })
                        .unwrap_or(1);
                    byday
                        .into_iter()
                        .filter_map(|code| {
                            day_code_to_weekday(code).map(|wd| NWeekday::Nth(ord, wd))
                        })
                        .collect()
                } else {
                    byday
                        .into_iter()
                        .filter_map(|code| day_code_to_weekday(code).map(NWeekday::Every))
                        .collect()
                };
                if !nweekdays.is_empty() {
                    rule = rule.by_weekday(nweekdays);
                }
            }
        }
    }

    if let Some(day_str) = extract_ews_field_doc(&doc, b"DayOfMonth")
        && let Ok(d) = day_str.parse::<i8>()
    {
        rule = rule.by_month_day(vec![d]);
    }

    if let Some(month_str) = extract_ews_field_doc(&doc, b"Month")
        && let Some(m) = parse_ews_month(&month_str)
        && let Some(mo) = month_num_to_chrono(m)
    {
        rule = rule.by_month(&[mo]);
    }

    if let Some(count_str) = extract_ews_field_doc(&doc, b"NumberOfOccurrences") {
        if let Ok(c) = count_str.parse::<u32>() {
            rule = rule.count(c);
        }
    } else if let Some(until) =
        extract_ews_field_doc(&doc, b"EndDate").and_then(|v| parse_datetime(&v))
    {
        let until_dt: chrono::DateTime<RruleTz> = until.with_timezone(&RruleTz::UTC);
        rule = rule.until(until_dt);
    }

    Some(rule.to_string())
}

pub fn parse_ews_attendees(xml: &str) -> Vec<Attendee> {
    let doc = match Document::parse(xml) {
        Ok(d) => d,
        Err(_) => return Vec::new(),
    };

    let mut attendees = Vec::new();

    for attendee_node in doc
        .descendants()
        .filter(|n| n.is_element() && n.tag_name().name() == "Attendee")
    {
        let mut attendee = Attendee::default();

        for child in attendee_node.descendants() {
            if !child.is_element() {
                continue;
            }
            let name = child.tag_name().name();
            let text = child
                .descendants()
                .filter(|c| c.is_text())
                .filter_map(|c| c.text())
                .next()
                .map(|s| s.to_string());

            match name {
                "Name" => attendee.name = text,
                "EmailAddress" => {
                    if let Some(t) = text {
                        attendee.email = nfc(&t);
                    }
                }
                "ResponseType" => {
                    if let Some(t) = text {
                        let (status, partstat) = match t.as_str() {
                            "Accept" => (Some(3), Some("ACCEPTED".to_string())),
                            "Tentative" => (Some(2), Some("TENTATIVE".to_string())),
                            "Decline" => (Some(4), Some("DECLINED".to_string())),
                            _ => (Some(5), Some("NEEDS-ACTION".to_string())),
                        };
                        attendee.attendee_status = status;
                        attendee.partstat = partstat;
                    }
                }
                _ => {}
            }
        }

        if !attendee.email.is_empty() {
            attendees.push(attendee);
        }
    }

    attendees
}

/// Normalise a timezone identifier carried in EWS XML (`StartTimeZone`/
/// `MeetingTimeZone`/`EndTimeZone`) to an IANA timezone id.
///
/// Outlook sends Windows timezone names (e.g. "Pacific Standard Time"); the
/// icalendar crate and Stalwart require IANA ids (e.g. "America/Los_Angeles").
/// Values that are already IANA ids round-trip unchanged. Unrecognised values
/// are returned as-is so callers can surface the original (render_ics falls
/// back to UTC for unparseable ids rather than dropping the timezone hint).
pub fn normalize_timezone_to_iana(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return raw.to_string();
    }
    if trimmed.parse::<chrono_tz::Tz>().is_ok() {
        return trimmed.to_string();
    }
    if let Some(iana) = crate::timezone::windows_timezone_name_to_iana(trimmed)
        && iana.parse::<chrono_tz::Tz>().is_ok()
    {
        return iana;
    }
    raw.to_string()
}

pub fn parse_ews_calendar_item(xml: &str) -> Result<CalendarItem> {
    let doc = Document::parse(xml).map_err(|e| anyhow!("failed to parse EWS XML: {e}"))?;

    // The request's timezone: `t:StartTimeZone`/`t:StartTimeZoneId`/
    // `t:MeetingTimeZone`/`t:EndTimeZone`/`t:EndTimeZoneId` on the item, or
    // the SOAP `t:TimeZoneContext` header ([MS-OXWSCORE] §2.2.1.12) Outlook
    // sends alongside naive (offset-less) `t:Start`/`t:End` values. The
    // header is the LAST fallback: element-level zones override the
    // request-wide context.
    let timezone_raw = extract_ews_timezone_field_doc(&doc, b"StartTimeZone")
        .or_else(|| extract_ews_field_doc(&doc, b"StartTimeZoneId"))
        .or_else(|| extract_ews_timezone_field_doc(&doc, b"MeetingTimeZone"))
        .or_else(|| extract_ews_timezone_field_doc(&doc, b"EndTimeZone"))
        .or_else(|| extract_ews_field_doc(&doc, b"EndTimeZoneId"))
        .or_else(|| extract_ews_timezone_context(&doc));
    let timezone = timezone_raw.as_deref().map(normalize_timezone_to_iana);
    let tz: Option<Tz> = timezone.as_deref().and_then(|id| id.parse().ok());

    // A timezone the request SUPPLIED but that failed to resolve leaves naive
    // (offset-less) Start/End values with no honest instant — error instead of
    // silently reading them as UTC, which shifts the event by the zone's
    // offset (the §14 offset-drift failure). Values that carry their own
    // `Z`/offset are unaffected, and so is a request that supplied no zone
    // at all (UTC stays the only defensible reading there).
    let zone_supplied_unresolved = timezone_raw.is_some() && tz.is_none();
    let parse_start_end = |v: String| -> Option<chrono::DateTime<Utc>> {
        if zone_supplied_unresolved && datetime_is_naive(&v) && v.contains('T') {
            return None;
        }
        parse_datetime_in_zone(&v, tz)
    };

    let subject =
        extract_ews_field_doc(&doc, b"Subject").unwrap_or_else(|| "(no subject)".to_string());
    let start = extract_ews_field_doc(&doc, b"Start")
        .or_else(|| extract_ews_field_doc(&doc, b"StartTime"))
        .and_then(parse_start_end)
        .ok_or_else(|| anyhow!("missing Start/StartTime"))?;
    let end = extract_ews_field_doc(&doc, b"End")
        .or_else(|| extract_ews_field_doc(&doc, b"EndTime"))
        .and_then(parse_start_end)
        .ok_or_else(|| anyhow!("missing End/EndTime"))?;
    let uid = extract_ews_field_doc(&doc, b"UID")
        .or_else(|| extract_ews_field_doc(&doc, b"ClientUid"))
        .unwrap_or_else(|| Uuid::new_v4().to_string());
    let description = extract_ews_field_doc(&doc, b"Body")
        .or_else(|| extract_ews_field_doc(&doc, b"TextBody"))
        .unwrap_or_default();
    let location = extract_ews_field_doc(&doc, b"Location").unwrap_or_default();
    let all_day = extract_ews_field_doc(&doc, b"IsAllDayEvent")
        .map(|v| v.eq_ignore_ascii_case("true"))
        .unwrap_or(false);
    let organizer_name = extract_ews_field_doc(&doc, b"OrganizerName");
    let organizer_email = extract_ews_field_doc(&doc, b"OrganizerEmail");
    let categories = extract_ews_fields_doc(&doc, b"String");
    let attendees = parse_ews_attendees(xml);
    let reminder =
        extract_ews_field_doc(&doc, b"ReminderMinutesBeforeStart").and_then(|v| v.parse().ok());
    let busy_status =
        extract_ews_field_doc(&doc, b"LegacyFreeBusyStatus").and_then(|v| match v.as_str() {
            "Free" => Some(0),
            "Tentative" => Some(1),
            "Busy" => Some(2),
            "OOF" => Some(3),
            _ => None,
        });
    let sensitivity = extract_ews_field_doc(&doc, b"Sensitivity").and_then(|v| match v.as_str() {
        "Normal" => Some(0),
        "Personal" => Some(1),
        "Private" => Some(2),
        "Confidential" => Some(3),
        _ => None,
    });
    let response_requested =
        extract_ews_field_doc(&doc, b"ResponseRequested").map(|v| v.eq_ignore_ascii_case("true"));
    let disallow_new_time_proposal = extract_ews_field_doc(&doc, b"DisallowNewTimeProposal")
        .map(|v| v.eq_ignore_ascii_case("true"));
    let online_meeting_conf_link = extract_ews_field_doc(&doc, b"OnlineMeetingConfLink");
    let online_meeting_external_link = extract_ews_field_doc(&doc, b"OnlineMeetingExternalLink");
    let client_uid = extract_ews_field_doc(&doc, b"ClientUid");
    let rrule = parse_ews_recurrence(xml);

    Ok(CalendarItem {
        uid,
        subject,
        description,
        location,
        start,
        end,
        all_day,
        dtstamp: Some(Utc::now()),
        timezone,
        timezone_blob: extract_ews_timezone_field_doc(&doc, b"MeetingTimeZone"),
        rrule,
        exdates: Vec::new(),
        organizer_name,
        organizer_email,
        attendees,
        categories,
        busy_status,
        sensitivity,
        reminder,
        response_requested,
        disallow_new_time_proposal,
        appointment_reply_time: None,
        meeting_status: None,
        response_type: None,
        online_meeting_conf_link,
        online_meeting_external_link,
        client_uid,
        exceptions: Vec::new(),
    })
}

#[cfg(test)]
mod scheduling_tests {
    use super::*;
    use chrono::Utc;

    #[test]
    fn resolve_ical_tzid_iana_id_passes_through() {
        assert_eq!(
            resolve_ical_tzid("Europe/Berlin", None).as_deref(),
            Some("Europe/Berlin")
        );
    }

    #[test]
    fn resolve_ical_tzid_windows_registry_name_resolves() {
        // Exchange-authored iCalendar TZIDs are Windows registry ids.
        assert_eq!(
            resolve_ical_tzid("W. Europe Standard Time", None).as_deref(),
            Some("Europe/Berlin")
        );
        assert_eq!(
            resolve_ical_tzid("Pacific Standard Time", None).as_deref(),
            Some("America/Los_Angeles")
        );
    }

    #[test]
    fn resolve_ical_tzid_windows_display_name_resolves() {
        // The "(UTC+01:00) Amsterdam, Berlin, Bern, Rome, Stockholm, Vienna"
        // display-name TZID form Exchange 2007-era iCalendar carries.
        assert_eq!(
            resolve_ical_tzid(
                "(UTC+01:00) Amsterdam, Berlin, Bern, Rome, Stockholm, Vienna",
                None
            )
            .as_deref(),
            Some("Europe/Berlin")
        );
        assert_eq!(
            resolve_ical_tzid("(UTC-08:00) Pacific Time (US & Canada)", None).as_deref(),
            Some("America/Los_Angeles")
        );
    }

    #[test]
    fn resolve_ical_tzid_custom_tzid_resolves_via_vtimezone_structure() {
        // The [MS-ASCMD]-documented custom TZID with an authoritative
        // VTIMEZONE: only the component's rules identify the zone.
        let vtimezone = "BEGIN:VTIMEZONE\r\n\
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
            resolve_ical_tzid(
                "(GMT-08.00) Pacific Time (US & Canada)/Tijuana",
                Some(vtimezone)
            )
            .as_deref(),
            Some("America/Los_Angeles")
        );
    }

    #[test]
    fn parse_ics_resolves_windows_tzid_and_keeps_local_time() {
        // §14 wire proof: an Exchange-authored VEVENT whose TZID is a Windows
        // registry id and whose DTSTART is wall-clock must land on the exact
        // UTC instant — 09:00 Berlin summer time == 07:00 UTC (CEST +02:00),
        // not 09:00 UTC (the old naive fallback).
        let ics = "BEGIN:VCALENDAR\r\n\
             PRODID:-//Exchange//EN\r\n\
             BEGIN:VEVENT\r\n\
             UID:tzid-test@example.com\r\n\
             DTSTART;TZID=W. Europe Standard Time:20250701T090000\r\n\
             DTEND;TZID=W. Europe Standard Time:20250701T100000\r\n\
             SUMMARY:TZID resolution test\r\n\
             END:VEVENT\r\n\
             END:VCALENDAR\r\n";
        let ev = parse_ics_event(ics).expect("parses");
        assert_eq!(ev.start, parse_datetime("20250701T070000Z").unwrap());
        assert_eq!(ev.end, parse_datetime("20250701T080000Z").unwrap());
        assert_eq!(
            ev.timezone.as_deref(),
            Some("Europe/Berlin"),
            "the item must carry the RESOLVED IANA id, not the raw TZID"
        );
    }

    #[test]
    fn parse_ics_resolves_custom_tzid_via_vtimezone() {
        // Same instant-keeping guarantee for a fully custom TZID that only
        // its VTIMEZONE block identifies.
        let ics = "BEGIN:VCALENDAR\r\n\
             PRODID:-//Exchange//EN\r\n\
             BEGIN:VTIMEZONE\r\n\
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
             END:VTIMEZONE\r\n\
             BEGIN:VEVENT\r\n\
             UID:tzid-vtimezone-test@example.com\r\n\
             DTSTART;TZID=(GMT-08.00) Pacific Time (US & Canada)/Tijuana:20250701T090000\r\n\
             DTEND;TZID=(GMT-08.00) Pacific Time (US & Canada)/Tijuana:20250701T100000\r\n\
             SUMMARY:VTIMEZONE resolution test\r\n\
             END:VEVENT\r\n\
             END:VCALENDAR\r\n";
        let ev = parse_ics_event(ics).expect("parses");
        assert_eq!(ev.start, parse_datetime("20250701T160000Z").unwrap());
        assert_eq!(ev.end, parse_datetime("20250701T170000Z").unwrap());
    }

    #[test]
    fn parse_ics_event_localizes_each_property_in_its_own_tzid() {
        // RFC 5545 §3.8.2.2/§3.8.5.3: every date-time property carries its
        // own TZID — a `DTSTART;TZID=A` may pair with `DTEND;TZID=B`. The
        // parser must resolve EACH property's zone independently, never
        // reuse the first property's zone for the rest (the latch bug this
        // regression pins: DTEND used to inherit DTSTART's zone).
        let ics = "BEGIN:VCALENDAR\r\n\
             PRODID:-//Exchange//EN\r\n\
             BEGIN:VEVENT\r\n\
             UID:per-property-tzid@example.com\r\n\
             DTSTART;TZID=America/Los_Angeles:20250701T090000\r\n\
             DTEND;TZID=Europe/Berlin:20250701T180000\r\n\
             SUMMARY:Per-property TZID\r\n\
             END:VEVENT\r\n\
             END:VCALENDAR\r\n";
        let ev = parse_ics_event(ics).expect("parses");
        assert_eq!(
            ev.start,
            parse_datetime("20250701T160000Z").unwrap(),
            "DTSTART must localize in America/Los_Angeles"
        );
        assert_eq!(
            ev.end,
            parse_datetime("20250701T160000Z").unwrap(),
            "DTEND must localize in Europe/Berlin (18:00 CEST), not LA"
        );
    }

    #[test]
    fn parse_ics_event_unknown_tzid_naive_value_has_no_instant() {
        // A TZID the resolver cannot identify leaves a NAIVE value with no
        // honest instant: fail closed (no event) rather than silently reading
        // it as UTC, which would drift the meeting by the unknown zone's
        // offset. A UTC-qualified value still parses — it needs no zone.
        let ics = "BEGIN:VCALENDAR\r\n\
             PRODID:-//Exchange//EN\r\n\
             BEGIN:VEVENT\r\n\
             UID:unknown-tzid@example.com\r\n\
             DTSTART;TZID=Custom/Unknown_Zone:20250701T090000\r\n\
             DTEND;TZID=Custom/Unknown_Zone:20250701T100000\r\n\
             SUMMARY:Unresolvable zone\r\n\
             END:VEVENT\r\n\
             END:VCALENDAR\r\n";
        assert!(
            parse_ics_event(ics).is_none(),
            "naive value with a supplied-but-unresolved TZID must fail closed"
        );

        let ics_utc = ics.replace("20250701T090000", "20250701T090000Z");
        let ics_utc = ics_utc.replace("20250701T100000", "20250701T100000Z");
        let ev = parse_ics_event(&ics_utc).expect("explicit Z needs no resolvable zone");
        assert_eq!(ev.start, parse_datetime("20250701T090000Z").unwrap());
    }

    #[test]
    fn parse_ews_calendar_item_localises_naive_datetime_via_starttimezone() {
        // Outlook's EWS CreateItem/UpdateItem carries offset-less
        // xs:dateTime values plus the Windows zone on the item
        // ([MS-OXWSMTGS] `StartTimeZone`). Without localisation every such
        // event drifts by the zone offset; with it, 09:00 Pacific Daylight
        // Time is exactly 16:00 UTC.
        let xml = r#"<t:CalendarItem xmlns:t="http://schemas.microsoft.com/exchange/services/2006/types">
  <t:Subject>Naive Pacific event</t:Subject>
  <t:Start>2025-07-01T09:00:00</t:Start>
  <t:End>2025-07-01T10:00:00</t:End>
  <t:StartTimeZone Id="Pacific Standard Time" Name="(UTC-08:00) Pacific Time (US &amp; Canada)"/>
</t:CalendarItem>"#;
        let item = parse_ews_calendar_item(xml).expect("parses");
        assert_eq!(
            item.start,
            parse_datetime("2025-07-01T16:00:00Z").unwrap_or_else(Utc::now)
        );
        assert_eq!(
            item.timezone.as_deref(),
            Some("America/Los_Angeles"),
            "item timezone must be the RESOLVED IANA id"
        );
    }

    #[test]
    fn extract_ews_field_reads_fragments_with_ancestor_declared_prefixes() {
        // `parse_item_changes` collects `SetItemField` payload fragments whose
        // `t:` prefixes are declared on the SOAP envelope, NOT inside the
        // fragment. `roxmltree::Document::parse` rejects undeclared prefixes,
        // so the strict walk returned nothing for exactly these fragments —
        // and every UpdateItem field change was silently dropped. The
        // prefix-agnostic fallback must read them, including entity-encoded
        // text, while declared documents keep the exact doc-walk behavior.
        let fragment = r#"<t:CalendarItem><t:Subject>Lunch &amp; learn</t:Subject><t:Start>2025-01-15T09:00:00</t:Start></t:CalendarItem>"#;
        assert_eq!(
            extract_ews_field(fragment, b"Start"),
            Some("2025-01-15T09:00:00".to_string()),
            "prefixed fragment with ancestor-declared namespace must extract"
        );
        assert_eq!(
            extract_ews_field(fragment, b"Subject"),
            Some("Lunch & learn".to_string()),
            "entity references must resolve to text"
        );
        assert_eq!(extract_ews_field(fragment, b"Missing"), None);

        // A nested same-name child IS the first document-order match, so the
        // lenient path must agree with the strict walk (which also finds it).
        let nested = r#"<t:CalendarItem><t:Body><t:Start>x</t:Start></t:Body><t:Start>real</t:Start></t:CalendarItem>"#;
        let declared_nested = Document::parse(
            r#"<r xmlns:t="urn:x"><t:CalendarItem><t:Body><t:Start>x</t:Start></t:Body><t:Start>real</t:Start></t:CalendarItem></r>"#,
        )
        .unwrap();
        assert_eq!(
            extract_ews_field(nested, b"Start"),
            extract_ews_field_doc(&declared_nested, b"Start"),
            "lenient fragment extraction must agree with the strict doc walk"
        );
        assert_eq!(extract_ews_field(nested, b"Start"), Some("x".to_string()));

        let declared = r#"<t:CalendarItem xmlns:t="http://schemas.microsoft.com/exchange/services/2006/types"><t:Categories><t:String>Work</t:String><t:String>Focus</t:String></t:Categories></t:CalendarItem>"#;
        assert_eq!(
            extract_ews_fields(declared, b"String"),
            vec!["Work".to_string(), "Focus".to_string()]
        );
        assert_eq!(
            extract_ews_fields(fragment, b"String"),
            Vec::<String>::new(),
            "no matches yields an empty vec, not an error"
        );
    }

    #[test]
    fn parse_ews_calendar_item_supplied_but_unresolved_zone_rejects_naive_values() {
        // A zone the request SUPPLIED but that fails to resolve leaves a
        // naive (offset-less) Start/End with no honest instant: error rather
        // than silently reading them as UTC, which drifts the meeting by the
        // zone's offset (§14 offset-drift failure). Explicit-`Z` values and
        // requests with NO zone at all keep parsing.
        let xml = r#"<t:CalendarItem xmlns:t="http://schemas.microsoft.com/exchange/services/2006/types">
  <t:Subject>Bogus zone</t:Subject>
  <t:Start>2025-07-01T09:00:00</t:Start>
  <t:End>2025-07-01T10:00:00</t:End>
  <t:StartTimeZone Id="Bogus/No_Such_Zone" Name="(UTC) Nowhere"/>
</t:CalendarItem>"#;
        let err = parse_ews_calendar_item(xml).expect_err("must fail closed");
        assert!(
            err.to_string().contains("Start/StartTime"),
            "error must point at the uninterpretable value: {err}"
        );

        let utc_xml = r#"<t:CalendarItem xmlns:t="http://schemas.microsoft.com/exchange/services/2006/types">
  <t:Subject>Bogus zone, explicit Z</t:Subject>
  <t:Start>2025-07-01T09:00:00Z</t:Start>
  <t:End>2025-07-01T10:00:00Z</t:End>
  <t:StartTimeZone Id="Bogus/No_Such_Zone" Name="(UTC) Nowhere"/>
</t:CalendarItem>"#;
        let item = parse_ews_calendar_item(utc_xml).expect("explicit Z carries its own instant");
        assert_eq!(item.start, parse_datetime("2025-07-01T09:00:00Z").unwrap());

        let no_zone_xml = r#"<t:CalendarItem xmlns:t="http://schemas.microsoft.com/exchange/services/2006/types">
  <t:Subject>No zone at all</t:Subject>
  <t:Start>2025-07-01T09:00:00</t:Start>
  <t:End>2025-07-01T10:00:00</t:End>
</t:CalendarItem>"#;
        let item = parse_ews_calendar_item(no_zone_xml).expect("no zone supplied → UTC");
        assert_eq!(item.start, parse_datetime("2025-07-01T09:00:00Z").unwrap());
    }

    #[test]
    fn localize_gap_tolerant_maps_gap_with_pre_transition_offset() {
        // A normal 1h spring-forward gap: Europe/Berlin 2025-03-30 02:30
        // does not exist; the pre-transition offset (+01:00) maps it to
        // 01:30 UTC (RFC 5545/7265 gap policy).
        let naive = NaiveDateTime::parse_from_str("2025-03-30T02:30:00", "%Y-%m-%dT%H:%M:%S")
            .expect("naive");
        let dt = localize_gap_tolerant(naive, "Europe/Berlin".parse().expect("tz"))
            .expect("gap is tolerated");
        assert_eq!(dt, parse_datetime("2025-03-30T01:30:00Z").unwrap());

        // An ambiguous fall-back time takes the earlier mapping.
        let fold = NaiveDateTime::parse_from_str("2025-10-26T02:30:00", "%Y-%m-%dT%H:%M:%S")
            .expect("naive");
        let dt = localize_gap_tolerant(fold, "Europe/Berlin".parse().expect("tz"))
            .expect("fold is tolerated");
        assert_eq!(dt, parse_datetime("2025-10-26T00:30:00Z").unwrap());
    }

    #[test]
    fn localize_gap_tolerant_survives_date_line_day_skip() {
        // Pacific/Apia skipped all of 2011-12-30 when it jumped UTC-10 →
        // UTC+14: a 24h gap, so a single 12h backward probe still lands
        // inside the gap. The ladder must keep probing until it leaves the
        // transition window and recover the pre-transition offset (-10:00):
        // naive 12:00 wall clock → 22:00 UTC that day.
        let naive = NaiveDateTime::parse_from_str("2011-12-30T12:00:00", "%Y-%m-%dT%H:%M:%S")
            .expect("naive");
        let dt = localize_gap_tolerant(naive, "Pacific/Apia".parse().expect("tz"))
            .expect(">12h gap must resolve via the probe ladder");
        assert_eq!(dt, parse_datetime("2011-12-30T22:00:00Z").unwrap());
    }

    #[test]
    fn ews_update_request_zone_resolves_header_and_item_zones() {
        // UpdateItem requests carry the zone either as the SOAP
        // `t:TimeZoneContext` header ([MS-OXWSCORE] §2.2.1.12) or as zone
        // elements inside the SetItemField payloads; element-level zones
        // win over the request-wide header, mirroring
        // `parse_ews_calendar_item`.
        let header_only = r#"<soap:Envelope xmlns:soap="http://schemas.xmlsoap.org/soap/envelope/"
  xmlns:t="http://schemas.microsoft.com/exchange/services/2006/types"
  xmlns:m="http://schemas.microsoft.com/exchange/services/2006/messages">
  <soap:Header>
    <t:TimeZoneContext><t:TimeZoneDefinition Id="Pacific Standard Time"/></t:TimeZoneContext>
  </soap:Header>
  <soap:Body>
    <m:UpdateItem><m:ItemChanges><t:ItemChange/></m:ItemChanges></m:UpdateItem>
  </soap:Body>
</soap:Envelope>"#;
        let tz = match ews_update_request_zone(header_only) {
            EwsRequestZone::Resolved(tz) => tz,
            other => panic!("header zone must resolve, got {other:?}"),
        };
        assert_eq!(tz, "America/Los_Angeles".parse::<Tz>().unwrap());

        let item_level = r#"<m:UpdateItem xmlns:m="http://schemas.microsoft.com/exchange/services/2006/messages"
  xmlns:t="http://schemas.microsoft.com/exchange/services/2006/types">
  <m:ItemChanges>
    <t:ItemChange>
      <t:Updates>
        <t:SetItemField><t:FieldURI FieldURI="calendar:StartTimeZone"/>
          <t:CalendarItem><t:StartTimeZone Id="W. Europe Standard Time"/></t:CalendarItem>
        </t:SetItemField>
      </t:Updates>
    </t:ItemChange>
  </m:ItemChanges>
</m:UpdateItem>"#;
        let tz = match ews_update_request_zone(item_level) {
            EwsRequestZone::Resolved(tz) => tz,
            other => panic!("payload zone must resolve, got {other:?}"),
        };
        assert_eq!(tz, "Europe/Berlin".parse::<Tz>().unwrap());

        let bare = r#"<m:UpdateItem xmlns:m="http://schemas.microsoft.com/exchange/services/2006/messages"></m:UpdateItem>"#;
        assert_eq!(
            ews_update_request_zone(bare),
            EwsRequestZone::Absent,
            "no zone supplied → Absent (naive values read as UTC)"
        );

        // A zone the request SUPPLIED but that maps to no known zone must
        // surface as `Unresolved` — the caller then fails a naive Start/End
        // closed instead of silently reading UTC.
        let unknown = r#"<soap:Envelope xmlns:soap="http://schemas.xmlsoap.org/soap/envelope/"
  xmlns:t="http://schemas.microsoft.com/exchange/services/2006/types">
  <soap:Header>
    <t:TimeZoneContext><t:TimeZoneDefinition Id="Mars/Standard Time"/></t:TimeZoneContext>
  </soap:Header>
  <soap:Body>
    <m:UpdateItem xmlns:m="http://schemas.microsoft.com/exchange/services/2006/messages"/>
  </soap:Body>
</soap:Envelope>"#;
        assert_eq!(
            ews_update_request_zone(unknown),
            EwsRequestZone::Unresolved,
            "supplied-but-unknown zone id → Unresolved"
        );
    }

    #[test]
    fn parse_ews_calendar_item_localises_naive_datetime_via_time_zone_context() {
        // The SOAP `TimeZoneContext` header ([MS-OXWSCORE] §2.2.1.12) rides
        // in the envelope; the item itself carries only naive datetimes.
        let xml = r#"<s:Envelope xmlns:s="http://schemas.xmlsoap.org/soap/envelope/">
  <s:Header>
    <t:TimeZoneContext xmlns:t="http://schemas.microsoft.com/exchange/services/2006/types">
      <t:TimeZoneDefinition Id="W. Europe Standard Time" Name="(UTC+01:00) Amsterdam, Berlin, Bern, Rome, Stockholm, Vienna"/>
    </t:TimeZoneContext>
  </s:Header>
  <s:Body>
    <m:CreateItem xmlns:m="http://schemas.microsoft.com/exchange/services/2006/messages">
      <m:Items>
        <t:CalendarItem xmlns:t="http://schemas.microsoft.com/exchange/services/2006/types">
          <t:Subject>Naive Berlin event</t:Subject>
          <t:Start>2025-07-01T09:00:00</t:Start>
          <t:End>2025-07-01T10:00:00</t:End>
        </t:CalendarItem>
      </m:Items>
    </m:CreateItem>
  </s:Body>
</s:Envelope>"#;
        let item = parse_ews_calendar_item(xml).expect("parses");
        // 09:00 CEST (UTC+2) == 07:00 UTC.
        assert_eq!(item.start, parse_datetime("2025-07-01T07:00:00Z").unwrap());
        assert_eq!(item.end, parse_datetime("2025-07-01T08:00:00Z").unwrap());
        assert_eq!(item.timezone.as_deref(), Some("Europe/Berlin"));
    }

    #[test]
    fn parse_ews_calendar_item_starttimezoneid_element_form() {
        // The `t:StartTimeZoneId`/`t:EndTimeZoneId` element form carries the
        // zone id as element text ([MS-OXWSMTGS] §2.2.2.41).
        let xml = r#"<t:CalendarItem xmlns:t="http://schemas.microsoft.com/exchange/services/2006/types">
  <t:Subject>Element-form zone</t:Subject>
  <t:Start>2025-01-15T09:00:00</t:Start>
  <t:End>2025-01-15T10:00:00</t:End>
  <t:StartTimeZoneId>Eastern Standard Time</t:StartTimeZoneId>
</t:CalendarItem>"#;
        let item = parse_ews_calendar_item(xml).expect("parses");
        // 09:00 EST (UTC-5) == 14:00 UTC — the January instant proves the
        // STANDARD offset applies, not a DST one.
        assert_eq!(item.start, parse_datetime("2025-01-15T14:00:00Z").unwrap());
        assert_eq!(item.timezone.as_deref(), Some("America/New_York"));
    }

    #[test]
    fn parse_ews_calendar_item_explicit_utc_value_ignores_zone() {
        // A value that already carries `Z` (or an explicit offset) must be
        // read as-is; the request zone never shifts an explicit instant.
        let xml = r#"<t:CalendarItem xmlns:t="http://schemas.microsoft.com/exchange/services/2006/types">
  <t:Subject>Explicit UTC</t:Subject>
  <t:Start>2025-07-01T09:00:00Z</t:Start>
  <t:End>2025-07-01T10:00:00Z</t:End>
  <t:StartTimeZoneId>Pacific Standard Time</t:StartTimeZoneId>
</t:CalendarItem>"#;
        let item = parse_ews_calendar_item(xml).expect("parses");
        assert_eq!(item.start, parse_datetime("2025-07-01T09:00:00Z").unwrap());
    }

    #[test]
    fn parse_ews_calendar_item_element_zone_overrides_header_context() {
        // Element-level zones beat the request-wide TimeZoneContext header.
        let xml = r#"<s:Envelope xmlns:s="http://schemas.xmlsoap.org/soap/envelope/">
  <s:Header>
    <t:TimeZoneContext xmlns:t="http://schemas.microsoft.com/exchange/services/2006/types">
      <t:TimeZoneDefinition Id="W. Europe Standard Time"/>
    </t:TimeZoneContext>
  </s:Header>
  <s:Body>
    <m:CreateItem xmlns:m="http://schemas.microsoft.com/exchange/services/2006/messages">
      <m:Items>
        <t:CalendarItem xmlns:t="http://schemas.microsoft.com/exchange/services/2006/types">
          <t:Subject>Zone override</t:Subject>
          <t:Start>2025-07-01T09:00:00</t:Start>
          <t:End>2025-07-01T10:00:00</t:End>
          <t:StartTimeZoneId>Pacific Standard Time</t:StartTimeZoneId>
        </t:CalendarItem>
      </m:Items>
    </m:CreateItem>
  </s:Body>
</s:Envelope>"#;
        let item = parse_ews_calendar_item(xml).expect("parses");
        // 09:00 PDT (UTC-7) == 16:00 UTC — Pacific wins over Berlin context.
        assert_eq!(item.start, parse_datetime("2025-07-01T16:00:00Z").unwrap());
        assert_eq!(item.timezone.as_deref(), Some("America/Los_Angeles"));
    }

    #[test]
    fn parse_ews_calendar_item_dst_gap_instant_is_earliest_mapping() {
        // 02:30 on the US spring-forward gap does not exist; the parser must
        // take the earliest valid mapping (03:30 EDT => 07:30 UTC), the same
        // gap-tolerant policy the iCalendar path uses — never fail the item.
        let xml = r#"<t:CalendarItem xmlns:t="http://schemas.microsoft.com/exchange/services/2006/types">
  <t:Subject>Gap event</t:Subject>
  <t:Start>2025-03-09T02:30:00</t:Start>
  <t:End>2025-03-09T03:30:00</t:End>
  <t:StartTimeZoneId>Eastern Standard Time</t:StartTimeZoneId>
</t:CalendarItem>"#;
        let item = parse_ews_calendar_item(xml).expect("parses");
        assert_eq!(item.start, parse_datetime("2025-03-09T07:30:00Z").unwrap());
    }

    fn sample_event_with_attendee() -> CalendarItem {
        CalendarItem {
            uid: "test-uid".to_string(),
            subject: "Quarterly Review".to_string(),
            start: Utc::now(),
            end: Utc::now() + chrono::Duration::hours(1),
            organizer_email: Some("boss@example.com".to_string()),
            organizer_name: Some("The Boss".to_string()),
            attendees: vec![Attendee {
                email: "alice@example.com".to_string(),
                name: Some("Alice".to_string()),
                attendee_type: Some(1),
                attendee_status: Some(0),
                partstat: Some("NEEDS-ACTION".to_string()),
                schedule_agent: None,
            }],
            ..Default::default()
        }
    }

    #[test]
    fn render_ics_emits_schedule_agent_client() {
        let mut item = sample_event_with_attendee();
        mark_scheduling_client_side(&mut item);
        let ics = render_ics(&item);
        // RFC 5545 line-folds long property lines (75 chars) so the literal
        // "SCHEDULE-AGENT=CLIENT" string may be split across a fold. Unfold
        // (remove CRLF + leading whitespace on continuation lines) before
        // asserting on the logical content.
        let unfolded: String = ics
            .lines()
            .map(|l| l.trim_start().to_string())
            .collect::<Vec<_>>()
            .join("");
        assert!(
            unfolded.contains("SCHEDULE-AGENT=CLIENT:mailto:alice@example.com"),
            "expected ATTENDEE with SCHEDULE-AGENT=CLIENT parameter, got:\n{ics}\nunfolded:\n{unfolded}"
        );
        assert!(
            ics.contains("ATTENDEE"),
            "attendee should be preserved, got:\n{ics}"
        );
    }

    #[test]
    fn render_ics_omits_schedule_agent_when_none() {
        let item = sample_event_with_attendee();
        let ics = render_ics(&item);
        assert!(
            !ics.contains("SCHEDULE-AGENT"),
            "no SCHEDULE-AGENT should be emitted when schedule_agent is None, got:\n{ics}"
        );
    }

    #[test]
    fn mark_scheduling_client_side_preserves_attendee_count() {
        let mut item = sample_event_with_attendee();
        let before = item.attendees.len();
        mark_scheduling_client_side(&mut item);
        let after = item.attendees.len();
        assert_eq!(before, after, "attendee count must not change");
        assert_eq!(item.attendees[0].schedule_agent.as_deref(), Some("CLIENT"));
    }

    #[test]
    fn schedule_disposition_wants_scheduling() {
        assert!(!ScheduleDisposition::SendToNone.wants_scheduling());
        assert!(ScheduleDisposition::SendOnlyToAll.wants_scheduling());
        assert!(ScheduleDisposition::SendToAllAndSaveCopy.wants_scheduling());
        assert!(ScheduleDisposition::SendToChangedOnly.wants_scheduling());
        assert!(ScheduleDisposition::SendToAllAndSaveCopyInclDeleted.wants_scheduling());
        assert!(ScheduleDisposition::SendToChangedAndSaveCopy.wants_scheduling());
    }

    #[test]
    fn scheduling_needed_requires_attendees_and_disposition() {
        let item = sample_event_with_attendee();
        let mut empty = item.clone();
        empty.attendees = vec![];
        assert!(!scheduling_needed(
            &empty,
            ScheduleDisposition::SendToAllAndSaveCopy
        ));
        assert!(scheduling_needed(
            &item,
            ScheduleDisposition::SendToAllAndSaveCopy
        ));
        assert!(!scheduling_needed(&item, ScheduleDisposition::SendToNone));
    }

    #[test]
    fn schedule_disposition_parse_known_values() {
        assert_eq!(
            ScheduleDisposition::parse(Some("SendToNone")),
            Some(ScheduleDisposition::SendToNone)
        );
        assert_eq!(
            ScheduleDisposition::parse(Some("SendToAllAndSaveCopy")),
            Some(ScheduleDisposition::SendToAllAndSaveCopy)
        );
        assert_eq!(
            ScheduleDisposition::parse(Some("  SendOnlyToAll  ")),
            Some(ScheduleDisposition::SendOnlyToAll)
        );
        assert_eq!(ScheduleDisposition::parse(Some("nonsense")), None);
        assert_eq!(ScheduleDisposition::parse(None), None);
    }

    #[test]
    fn ensure_organizer_for_scheduling_adds_when_attendees_present() {
        let mut item = sample_event_with_attendee();
        item.organizer_email = None;
        item.organizer_name = None;
        ensure_organizer_for_scheduling(&mut item, Some("boss@example.com"));
        assert_eq!(item.organizer_email.as_deref(), Some("boss@example.com"));
    }

    #[test]
    fn ensure_organizer_is_noop_without_attendees() {
        let mut item = sample_event_with_attendee();
        item.attendees = vec![];
        item.organizer_email = None;
        ensure_organizer_for_scheduling(&mut item, Some("boss@example.com"));
        assert!(
            item.organizer_email.is_none(),
            "no organizer should be synthesised when there are no attendees"
        );
    }

    /// DST-crossover recurrence round-trip (audit gap #1). A weekly recurring
    /// series anchored in `America/New_York` at 09:00 local, whose window
    /// straddles the 2025 spring-forward (March 9), must round-trip through
    /// `render_ics` and back through `parse_ics_event` with the original UTC
    /// start instant preserved byte-for-byte AND a synthesised VTIMEZONE whose
    /// STANDARD/DAYLIGHT offsets (-05:00 / -04:00) and TZID match the
    /// authoritative IANA zone (so recurrence exception wall-clock times land
    /// on the correct side of the DST boundary on both Outlook clients).
    #[test]
    fn render_ics_round_trips_dst_crossover_recurrence_with_synthesized_vtimezone() {
        use chrono::{Duration, TimeZone};
        // 09:00 EST = 14:00 UTC on 2025-03-02 (Sunday), well before the
        // March 9 spring-forward. UNTIL is a month later (April 6, after the
        // transition) so the series spans the crossover.
        let start = chrono::Utc.from_utc_datetime(
            &chrono::NaiveDateTime::parse_from_str("2025-03-02T14:00:00", "%Y-%m-%dT%H:%M:%S")
                .unwrap(),
        );
        let item = CalendarItem {
            uid: "dst-crossover-001".to_string(),
            subject: "Weekly Standup".to_string(),
            start,
            end: start + Duration::hours(1),
            all_day: false,
            dtstamp: Some(start),
            timezone: Some("America/New_York".to_string()),
            // No authoritative VTIMEZONE blob (EWS/gateway origin): the gateway
            // must SYNTHESISE one from the IANA id so the edited event round-trips.
            timezone_blob: None,
            rrule: Some("FREQ=WEEKLY;UNTIL=20250406T150000Z".to_string()),
            ..Default::default()
        };

        let ics = render_ics(&item);
        // 1) A VTIMEZONE was synthesised and carries the canonical IANA TZID.
        assert!(
            ics.contains("BEGIN:VTIMEZONE"),
            "synthesised VTIMEZONE missing:\n{ics}"
        );
        assert!(
            ics.contains("TZID:America/New_York"),
            "non-canonical/missing TZID:\n{ics}"
        );
        // 2) The STANDARD/DAYLIGHT offsets are EDT (+1h) -> EST (UTC-5/-4).
        assert!(ics.contains("TZOFFSETTO:-0500"), "missing STD offset -0500");
        assert!(ics.contains("TZOFFSETTO:-0400"), "missing DST offset -0400");
        assert!(
            ics.contains("TZOFFSETFROM:-0500") && ics.contains("TZOFFSETFROM:-0400"),
            "missing TZOFFSETFROM boundaries"
        );
        // 3) The recurrence RRULE was preserved verbatim (authoritative UNTIL).
        assert!(ics.contains("RRULE:FREQ=WEEKLY;UNTIL=20250406T150000Z"));
        // 4) DTSTART carries the IANA TZID (not a bare UTC instant), so the
        //    recurrence engine honours the zone.
        assert!(ics.contains("DTSTART;TZID=America/New_York:20250302T090000"));

        // 5) Round-trip through the parser: the master event must survive with
        //    the same UTC start instant + preserved RRULE + canonical TZID.
        let parsed = parse_ics_event(&ics).expect("round-trip parse");
        assert_eq!(parsed.uid, "dst-crossover-001");
        assert_eq!(
            parsed.start, start,
            "UTC start instant drifted across round-trip"
        );
        assert_eq!(parsed.end, start + Duration::hours(1));
        assert_eq!(parsed.timezone.as_deref(), Some("America/New_York"));
        assert_eq!(
            parsed.rrule.as_deref(),
            Some("FREQ=WEEKLY;UNTIL=20250406T150000Z")
        );
        assert!(
            parsed
                .timezone_blob
                .is_some_and(|b| b.contains("VTIMEZONE")),
            "parser did not capture the round-tripped VTIMEZONE block"
        );
    }

    /// RFC 5545 §3.6.5: a `DTSTART` inside a `STANDARD`/`DAYLIGHT` subcomponent
    /// of a `VTIMEZONE` MUST be a local (date-time) value — a UTC-suffixed
    /// `DTSTART` (with trailing `Z`) is explicitly forbidden here, and the EWS
    /// rendering of `CalendarItem.Start/End` `StartTimeZone`/`EndTimeZone`
    /// relied on the synthesised blob to be a valid `VTIMEZONE` (CJK/`Z`
    /// suffixes would make stricter iCalendar parsers reject the whole
    /// calendar). Regression guard for the C13 fix.
    #[test]
    fn render_ics_synthesized_vtimezone_dtstart_is_local_no_trailing_z() {
        use chrono::TimeZone;
        let start = chrono::Utc.with_ymd_and_hms(2025, 3, 2, 14, 0, 0).unwrap();
        let item = CalendarItem {
            uid: "dtstart-no-z-001".to_string(),
            subject: "Weekly Standup".to_string(),
            start,
            end: start + chrono::Duration::hours(1),
            all_day: false,
            dtstamp: Some(start),
            timezone: Some("America/New_York".to_string()),
            timezone_blob: None,
            rrule: Some("FREQ=WEEKLY;UNTIL=20250406T150000Z".to_string()),
            ..Default::default()
        };

        let ics = render_ics(&item);
        // isolate the VTIMEZONE block and audit every DTSTART inside it.
        let tz_start = ics.find("BEGIN:VTIMEZONE").expect("VTIMEZONE present");
        let tz_end = ics[tz_start..]
            .find("END:VTIMEZONE")
            .expect("END:VTIMEZONE present")
            + tz_start
            + "END:VTIMEZONE".len();
        let vtz = &ics[tz_start..tz_end];
        let mut saw_dtstart = false;
        for line in vtz.lines() {
            if let Some(rest) = line.strip_prefix("DTSTART:") {
                saw_dtstart = true;
                assert!(
                    !rest.ends_with('Z'),
                    "VTIMEZONE DTSTART must be local (no Z): {line}"
                );
            }
        }
        assert!(saw_dtstart, "VTIMEZONE carried no DTSTART:\n{vtz}");
        // The master-event DTSTART (outside the VTIMEZONE) is local+TZID and is
        // itself not UTC-suffixed; ensure the audit didn't mis-flag it by
        // confirming at least one TZID-bearing DTSTART exists outside the block.
        assert!(ics.contains("DTSTART;TZID=America/New_York:20250302T090000"));
    }

    /// When the authoritative `timezone_blob` is a bare Windows time-zone name
    /// (captured from an inbound EWS `MeetingTimeZone` that failed to normalise),
    /// it has no `BEGIN:VTIMEZONE` framing and the `icalendar` parser yields
    /// `Err` (a property-line with no `:`name-value separator). The gateway must
    /// then synthesise the canonical VTIMEZONE from the IANA id — and when even
    /// that fails (e.g. the IANA id is unresolvable), it must NOT emit an orphan
    /// `DTSTART;TZID=...` referencing a missing VTIMEZONE (RFC 5545 invariant).
    /// Regression guard for the C2 + C11 restructure of `render_ics`.
    #[test]
    fn render_ics_synthesizes_vtimezone_when_blob_is_unparseable_windows_name() {
        use chrono::TimeZone;
        let start = chrono::Utc.with_ymd_and_hms(2025, 3, 2, 14, 0, 0).unwrap();
        let item = CalendarItem {
            uid: "malformed-blob-001".to_string(),
            subject: "Weekly Standup".to_string(),
            start,
            end: start + chrono::Duration::hours(1),
            all_day: false,
            dtstamp: Some(start),
            timezone: Some("America/New_York".to_string()),
            // A bare Windows name (no name-value separator) fails to parse as a
            // VTIMEZONE — exercises the `Err` branch CodeRabbit flagged (C2).
            timezone_blob: Some("Eastern Standard Time".to_string()),
            rrule: Some("FREQ=WEEKLY;UNTIL=20250406T150000Z".to_string()),
            ..Default::default()
        };

        let ics = render_ics(&item);
        // The fallback synthesis must yield a real VTIMEZONE for the IANA id...
        assert!(
            ics.contains("BEGIN:VTIMEZONE"),
            "synthesised VTIMEZONE missing for malformed blob:\n{ics}"
        );
        assert!(
            ics.contains("TZID:America/New_York"),
            "canonical TZID missing:\n{ics}"
        );
        // ...and the master DTSTART must carry the matching TZID (never an orphan
        // — the VTIMEZONE was synthesised).
        assert!(
            ics.contains("DTSTART;TZID=America/New_York:20250302T090000"),
            "master DTSTART missing/Orphaned:\n{ics}"
        );
        // Round-trips through the parser cleanly.
        let parsed = parse_ics_event(&ics).expect("round-trip parse");
        assert_eq!(parsed.start, start);
    }

    /// A CalDAV-origin event whose authoritative `timezone_blob` is a real
    /// iCalendar VTIMEZONE block must be re-emitted byte-for-byte — the
    /// synthesised-VTIMEZONE fallback must NOT override the authoritative blob
    /// (audit gap #1: "honoured byte-for-byte and re-emitted on edit").
    #[test]
    fn render_ics_preserves_authoritative_caldav_vtimezone_byte_for_byte() {
        let authoritative = "\
BEGIN:VTIMEZONE\r
TZID:America/New_York\r
BEGIN:DAYLIGHT\r
TZOFFSETFROM:-0500\r
TZOFFSETTO:-0400\r
DTSTART:19700308T020000\r
RRULE:FREQ=YEARLY;BYDAY=2SU;BYMONTH=3\r
END:DAYLIGHT\r
BEGIN:STANDARD\r
TZOFFSETFROM:-0400\r
TZOFFSETTO:-0500\r
DTSTART:19701101T020000\r
RRULE:FREQ=YEARLY;BYDAY=1SU;BYMONTH=11\r
END:STANDARD\r
END:VTIMEZONE";
        let item = CalendarItem {
            uid: "caldav-origin-001".to_string(),
            subject: "Authoritative".to_string(),
            start: chrono::Utc.with_ymd_and_hms(2025, 3, 2, 14, 0, 0).unwrap(),
            end: chrono::Utc.with_ymd_and_hms(2025, 3, 2, 15, 0, 0).unwrap(),
            all_day: false,
            dtstamp: Some(chrono::Utc.with_ymd_and_hms(2025, 1, 1, 0, 0, 0).unwrap()),
            timezone: Some("America/New_York".to_string()),
            timezone_blob: Some(authoritative.to_string()),
            ..Default::default()
        };

        let ics = render_ics(&item);
        // The authoritative RRULE (2SU / 1SU) MUST be present verbatim — the
        // synthesised fallback would instead emit the chronos-derived nth-week
        // rule, breaking the round-trip contract.
        assert!(
            ics.contains("BYDAY=2SU;BYMONTH=3"),
            "authoritative daylight RRULE lost:\n{ics}"
        );
        assert!(
            ics.contains("BYDAY=1SU;BYMONTH=11"),
            "authoritative standard RRULE lost:\n{ics}"
        );
        assert!(ics.contains("DTSTART:19700308T020000"));
    }

    /// Extract the text of the first `<...:Tag>` (or `<Tag>`) element from an
    /// EAS `Calendar` fragment. `map_rrule_to_recurrence_xml` emits a bare
    /// `<Calendar:Recurrence>` fragment *without* declaring the `Calendar`
    /// namespace prefix, so a strict XML parser (roxmltree) rejects it; this
    /// helper instead matches the local element name directly on the raw text,
    /// which is sufficient for round-trip field assertions.
    fn eas_field(xml: &str, tag: &str) -> Option<String> {
        let start = seek_tag(xml, 0, tag)?;
        let close = xml[start..].find('>')? + start;
        let value_end = xml[close + 1..].find('<')? + close + 1;
        let raw = xml[close + 1..value_end].trim();
        (!raw.is_empty()).then(|| raw.to_string())
    }

    /// Locate the `>`-terminated local-name match for `tag` starting at `from`,
    /// tolerating an optional namespace prefix (`Calendar:`).
    fn seek_tag(xml: &str, mut from: usize, tag: &str) -> Option<usize> {
        while let Some(rel) = xml[from..].find('<') {
            let abs = from + rel;
            if xml.get(abs + 1..)?.starts_with('/') {
                from = abs + 1;
                continue;
            }
            let name_start = abs + 1;
            let name_end = xml[name_start..].find(['>', ' ', '\t', '\n'])? + name_start;
            let raw_name = &xml[name_start..name_end];
            let local = raw_name.rsplit(':').next().unwrap_or(raw_name);
            if local == tag {
                return Some(abs);
            }
            from = name_end + 1;
        }
        None
    }

    /// RRULE → EAS `<Calendar:Recurrence>` → RRULE round-trip (audit gap #5).
    ///
    /// `map_rrule_to_recurrence_xml` (sync.rs) renders the gateway's canonical
    /// RRULE into the EAS recurrence fields, and `EasRecurrence::to_rrule`
    /// renders the other direction. A recurrence whose `FREQ`/`INTERVAL`/`BYDAY`
    /// survive both hops unchanged is exactly what Outlook Android needs to
    /// display the series identically to Outlook for Windows (which reads the
    /// EWS rendering). This guards the hand-rolled `RRULE <-> EAS` translation
    /// against silent drift (ordinal `n`→`WeekOfMonth=5` for "Last", mask
    /// bit→`DayOfWeek`, and the kind-normalisation freq 2↔3 / 5↔6).
    #[test]
    fn rrule_round_trips_through_eas_recurrence_xml() {
        // (rrule, expected_freq_str, expected_type, expected_week_of_month)
        let cases: Vec<(&str, &str, u8, Option<u32>)> = vec![
            ("FREQ=DAILY;INTERVAL=2", "DAILY", 0, None),
            ("FREQ=WEEKLY;BYDAY=MO,WE,FR", "WEEKLY", 1, None),
            // Relative monthly (2nd Monday) → type 3, week-of-month 2.
            ("FREQ=MONTHLY;BYDAY=2MO", "MONTHLY", 3, Some(2)),
            // Relative yearly (last Friday) → type 6, week-of-month 5 (Last).
            ("FREQ=YEARLY;BYDAY=-1FR;BYMONTH=3", "YEARLY", 6, Some(5)),
            // Absolute monthly (day 15) → type 2, no week-of-month.
            ("FREQ=MONTHLY;BYMONTHDAY=15", "MONTHLY", 2, None),
        ];

        for (input, expected_freq, expected_type, expected_week) in cases {
            let xml = crate::sync::map_rrule_to_recurrence_xml(input)
                .unwrap_or_else(|| panic!("no EAS recurrence XML for {input}"));

            let kind: u8 = eas_field(&xml, "Type")
                .and_then(|v| v.parse().ok())
                .unwrap_or_else(|| panic!("missing Type for {input}"));
            assert_eq!(kind, expected_type, "EAS Type mismatch for {input}");

            let interval: u32 = eas_field(&xml, "Interval")
                .and_then(|v| v.parse().ok())
                .unwrap_or(1);

            // Reconstruct `EasRecurrence` from the rendered fields and render it
            // back to an RRULE, then confirm FREQ/INTERVAL survived verbatim.
            let recon = EasRecurrence {
                kind: Some(kind),
                interval: Some(interval),
                day_of_week: eas_field(&xml, "DayOfWeek"),
                day_of_month: eas_field(&xml, "DayOfMonth").and_then(|v| v.parse().ok()),
                week_of_month: eas_field(&xml, "WeekOfMonth").and_then(|v| v.parse().ok()),
                month_of_year: eas_field(&xml, "MonthOfYear").and_then(|v| v.parse().ok()),
                first_day_of_week: eas_field(&xml, "FirstDayOfWeek").and_then(|v| v.parse().ok()),
                until: None,
                occurrences: eas_field(&xml, "Occurrences").and_then(|v| v.parse().ok()),
                calendar_type: None,
                is_empty: false,
            };
            let round_tripped = recon
                .to_rrule()
                .unwrap_or_else(|| panic!("to_rrule failed for {input}"));
            let rrule_has = |token: &str| round_tripped.contains(token);

            assert!(
                rrule_has(&format!("FREQ={expected_freq}")),
                "FREQ drift for {input}: round-tripped {round_tripped}"
            );
            if interval > 1 {
                assert!(
                    rrule_has(&format!("INTERVAL={interval}")),
                    "INTERVAL drift for {input}: round-tripped {round_tripped}"
                );
            }
            // Assert the day/week/month/ordinal pattern too, not just FREQ, so a
            // silent drift in BYDAY/BYMONTHDAY/BYMONTH cannot pass unnoticed.
            match input {
                "FREQ=WEEKLY;BYDAY=MO,WE,FR" => assert!(
                    rrule_has("BYDAY=MO,WE,FR"),
                    "weekly BYDAY drift for {input}: {round_tripped}"
                ),
                "FREQ=MONTHLY;BYDAY=2MO" => assert!(
                    rrule_has("BYDAY=2MO"),
                    "relative-monthly BYDAY drift for {input}: {round_tripped}"
                ),
                "FREQ=YEARLY;BYDAY=-1FR;BYMONTH=3" => {
                    assert!(
                        rrule_has("BYDAY=-1FR"),
                        "yearly BYDAY drift for {input}: {round_tripped}"
                    );
                    assert!(
                        rrule_has("BYMONTH=3"),
                        "yearly BYMONTH drift for {input}: {round_tripped}"
                    );
                }
                "FREQ=MONTHLY;BYMONTHDAY=15" => assert!(
                    rrule_has("BYMONTHDAY=15"),
                    "absolute-monthly BYMONTHDAY drift for {input}: {round_tripped}"
                ),
                _ => {}
            }
            if let Some(w) = expected_week {
                assert_eq!(
                    eas_field(&xml, "WeekOfMonth").and_then(|v| v.parse::<u32>().ok()),
                    Some(w),
                    "WeekOfMonth mismatch for {input}"
                );
            }
        }
    }

    /// RRULE → EWS `<t:Recurrence>` → RRULE round-trip (audit gap #5).
    ///
    /// `render_ews_recurrence_xml` (ews.rs) and `parse_ews_recurrence` (here)
    /// are the two halves of the Outlook-for-Windows recurrence path. A series
    /// whose `FREQ`/`INTERVAL`/`BYDAY`/`COUNT` survive this hop unchanged is the
    /// fidelity contract New Outlook relies on when it re-reads a series it just
    /// created. UNTIL is excluded from the strict set because the two renderers
    /// normalise the UTC instant to a date differently (byte drift only); the
    /// recurrence *pattern* is what must not drift.
    #[test]
    fn rrule_round_trips_through_ews_recurrence_xml() {
        use chrono::TimeZone;
        let start = chrono::Utc.with_ymd_and_hms(2025, 3, 2, 14, 0, 0).unwrap();

        // (rrule, expected FREQ prefix of the round-tripped RRULE)
        let cases: Vec<(&str, &str)> = vec![
            ("FREQ=DAILY;INTERVAL=3", "FREQ=DAILY"),
            ("FREQ=WEEKLY;INTERVAL=1;BYDAY=TU,TH", "FREQ=WEEKLY"),
            ("FREQ=MONTHLY;INTERVAL=1;BYDAY=2MO", "FREQ=MONTHLY"),
            ("FREQ=YEARLY;INTERVAL=1;BYDAY=-1FR;BYMONTH=3", "FREQ=YEARLY"),
            (
                "FREQ=MONTHLY;INTERVAL=1;BYMONTHDAY=15;COUNT=10",
                "FREQ=MONTHLY",
            ),
        ];

        for (input, expected_freq) in cases {
            let xml = crate::ews::render_ews_recurrence_xml(input, start);
            assert!(
                xml.contains("<t:Recurrence>"),
                "no EWS recurrence for {input}"
            );
            // The renderer emits a bare `<t:Recurrence>` fragment (the enclosing
            // EWS response declares `xmlns:t`); wrap it so the strict XML parser
            // sees a well-formed document with the `t` namespace bound.
            const T_NS: &str = "http://schemas.microsoft.com/exchange/services/2006/types";
            let wrapped = format!(r#"<root xmlns:t="{T_NS}">{xml}</root>"#);
            let round_tripped = parse_ews_recurrence(&wrapped)
                .unwrap_or_else(|| panic!("parse_ews_recurrence failed for {input}"));
            let has = |token: &str| round_tripped.contains(token);

            assert!(
                round_tripped.starts_with(expected_freq) || round_tripped.contains(expected_freq),
                "FREQ drift for {input}: round-tripped {round_tripped}"
            );
            // Assert the full pattern (interval, weekday/ordinal, month/day,
            // count), not just FREQ, so a drift in any field cannot pass.
            match input {
                "FREQ=DAILY;INTERVAL=3" => assert!(
                    has("INTERVAL=3"),
                    "DAILY INTERVAL drift for {input}: {round_tripped}"
                ),
                "FREQ=WEEKLY;INTERVAL=1;BYDAY=TU,TH" => assert!(
                    has("BYDAY=TU,TH"),
                    "WEEKLY BYDAY drift for {input}: {round_tripped}"
                ),
                "FREQ=MONTHLY;INTERVAL=1;BYDAY=2MO" => assert!(
                    has("BYDAY=2MO"),
                    "MONTHLY ordinal drift for {input}: {round_tripped}"
                ),
                "FREQ=YEARLY;INTERVAL=1;BYDAY=-1FR;BYMONTH=3" => {
                    assert!(
                        has("BYDAY=-1FR"),
                        "YEARLY ordinal drift for {input}: {round_tripped}"
                    );
                    assert!(
                        has("BYMONTH=3"),
                        "YEARLY BYMONTH drift for {input}: {round_tripped}"
                    );
                }
                "FREQ=MONTHLY;INTERVAL=1;BYMONTHDAY=15;COUNT=10" => {
                    assert!(
                        has("BYMONTHDAY=15"),
                        "MONTHLY BYMONTHDAY drift for {input}: {round_tripped}"
                    );
                    assert!(
                        has("COUNT=10"),
                        "MONTHLY COUNT drift for {input}: {round_tripped}"
                    );
                }
                _ => {}
            }
        }
    }
}
