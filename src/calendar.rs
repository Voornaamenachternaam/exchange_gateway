// src/calendar.rs
use crate::attachment::ParsedEasAttachmentAdd;
use crate::ical_parser;
use crate::util::nfc;
use anyhow::{Result, anyhow};
use chrono::{NaiveDate, NaiveDateTime, TimeZone, Utc};
use chrono_tz::Tz;
use derive_more::Debug;
use itertools::Itertools;
use phf::phf_map;
use quick_xml::Reader;
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

fn parse_datetime_with_tzid(val: &str, tzid: Option<&str>) -> Option<chrono::DateTime<Utc>> {
    if let Some(tzid) = tzid
        && !val.ends_with('Z')
        && val.contains('T')
    {
        if let Ok(local) = NaiveDateTime::parse_from_str(val, "%Y%m%dT%H%M%S")
            && let Ok(tz) = tzid.parse::<Tz>()
        {
            if let Some(dt) = tz.from_local_datetime(&local).single() {
                return Some(dt.with_timezone(&Utc));
            }
            if let Some(dt) = tz.from_local_datetime(&local).earliest() {
                return Some(dt.with_timezone(&Utc));
            }
        }
        if let Ok(local) = NaiveDateTime::parse_from_str(val, "%Y-%m-%dT%H:%M:%S")
            && let Ok(tz) = tzid.parse::<Tz>()
        {
            if let Some(dt) = tz.from_local_datetime(&local).single() {
                return Some(dt.with_timezone(&Utc));
            }
            if let Some(dt) = tz.from_local_datetime(&local).earliest() {
                return Some(dt.with_timezone(&Utc));
            }
        }
    }
    parse_datetime(val)
}

fn parse_duration_minutes(trigger: &str) -> Option<i32> {
    ical_parser::parse_ical_duration_minutes(trigger).ok()
}

fn parse_event_lines(lines: &[String]) -> CalendarEventFields {
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
                fields.start = parse_datetime_with_tzid(value, tzid.as_deref());
                if fields.timezone.is_none() {
                    fields.timezone = tzid;
                }
                if !value.contains('T') {
                    fields.all_day = Some(true);
                }
            }
            k if k.starts_with("DTEND") => {
                let tzid = parse_tzid_from_key(k);
                fields.end = parse_datetime_with_tzid(value, tzid.as_deref());
                if fields.timezone.is_none() {
                    fields.timezone = tzid;
                }
            }
            k if k.starts_with("RECURRENCE-ID") => {
                let tzid = parse_tzid_from_key(k);
                fields.recurrence_id = parse_datetime_with_tzid(value, tzid.as_deref());
                if fields.timezone.is_none() {
                    fields.timezone = tzid;
                }
                if !value.contains('T') {
                    fields.all_day = Some(true);
                }
            }
            k if k.starts_with("RRULE") => fields.rrule = Some(value.to_string()),
            k if k.starts_with("EXDATE") => {
                for ex in value.split(',') {
                    if let Some(dt) =
                        parse_datetime_with_tzid(ex, parse_tzid_from_key(k).as_deref())
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

    for block in split_ical_blocks(ics) {
        let fields = parse_event_lines(&block);
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

    if let Some(blob) = &item.timezone_blob {
        let wrapped = format!("BEGIN:VCALENDAR\r\n{blob}\r\nEND:VCALENDAR\r\n");
        if let Ok(parsed) = Calendar::from_str(&icalendar::parser::unfold(&wrapped)) {
            for component in parsed.iter() {
                if let CalendarComponent::Other(other) = component {
                    if other.component_kind() == "VTIMEZONE" {
                        calendar.push(component.clone());
                    }
                }
            }
        }
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
        event.ends((item.end.with_timezone(&tz).naive_local(), tz));
        event.starts((item.start.with_timezone(&tz).naive_local(), tz));
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

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) => {
                let local = e.name().local_name();
                let name: &[u8] = local.as_ref();
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
            }
            Ok(Event::Empty(e)) => {
                let local = e.name().local_name();
                let name: &[u8] = local.as_ref();
                let tag: SmallVec<[u8; 16]> = SmallVec::from_slice(name);
                if name == b"Recurrence" {
                    current.recurrence.is_empty = true;
                }
                stack.push(tag);
                stack.pop();
            }
            Ok(Event::Text(t)) => {
                if let Some(_kind) = current_kind {
                    let value = match t.decode() {
                        Ok(v) => v.into_owned(),
                        Err(_) => String::new(),
                    };
                    let last_tag = stack.last().map(|v| v.as_slice());
                    match last_tag {
                        Some(b"ClientId") => current.client_id = Some(value),
                        Some(b"ServerId")
                            if !stack.iter().any(|v| v.as_slice() == b"Exception") =>
                        {
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
                        Some(b"DisplayName")
                            if stack.iter().any(|v| v.as_slice() == b"Location") =>
                        {
                            if let Some(ex) = current.current_exception.as_mut() {
                                ex.location = Some(value);
                            } else {
                                current.location = Some(value);
                            }
                        }
                        Some(b"Timezone") => current.timezone = Some(value),
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
                        Some(b"ResponseRequested") => {
                            current.response_requested = Some(value == "1")
                        }
                        Some(b"DisallowNewTimeProposal") => {
                            current.disallow_new_time_proposal = Some(value == "1")
                        }
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
                        Some(b"OnlineMeetingConfLink") => {
                            current.online_meeting_conf_link = Some(value)
                        }
                        Some(b"OnlineMeetingExternalLink") => {
                            current.online_meeting_external_link = Some(value)
                        }
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
                        Some(b"AttendeeType")
                            if stack.iter().any(|v| v.as_slice() == b"Attendee") =>
                        {
                            current
                                .current_attendee
                                .get_or_insert_with(Attendee::default)
                                .attendee_type = value.parse().ok();
                        }
                        Some(b"AttendeeStatus")
                            if stack.iter().any(|v| v.as_slice() == b"Attendee") =>
                        {
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
                        Some(b"ExceptionStartTime")
                            if stack.iter().any(|v| v.as_slice() == b"Exception") =>
                        {
                            if let Some(ex) = current.current_exception.as_mut() {
                                ex.exception_start = parse_datetime(&value)
                                    .ok_or_else(|| anyhow!("invalid ExceptionStartTime"))?;
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
                        Some(b"Interval")
                            if stack.iter().any(|v| v.as_slice() == b"Recurrence") =>
                        {
                            current.recurrence.interval = value.parse().ok()
                        }
                        Some(b"DayOfWeek")
                            if stack.iter().any(|v| v.as_slice() == b"Recurrence") =>
                        {
                            current.recurrence.day_of_week = Some(value)
                        }
                        Some(b"DayOfMonth")
                            if stack.iter().any(|v| v.as_slice() == b"Recurrence") =>
                        {
                            current.recurrence.day_of_month = value.parse().ok()
                        }
                        Some(b"WeekOfMonth")
                            if stack.iter().any(|v| v.as_slice() == b"Recurrence") =>
                        {
                            current.recurrence.week_of_month = value.parse().ok()
                        }
                        Some(b"MonthOfYear")
                            if stack.iter().any(|v| v.as_slice() == b"Recurrence") =>
                        {
                            current.recurrence.month_of_year = value.parse().ok()
                        }
                        Some(b"Until") if stack.iter().any(|v| v.as_slice() == b"Recurrence") => {
                            current.recurrence.until = Some(value)
                        }
                        Some(b"Occurrences")
                            if stack.iter().any(|v| v.as_slice() == b"Recurrence") =>
                        {
                            current.recurrence.occurrences = value.parse().ok()
                        }
                        Some(b"FirstDayOfWeek")
                            if stack.iter().any(|v| v.as_slice() == b"Recurrence") =>
                        {
                            current.recurrence.first_day_of_week = value.parse().ok()
                        }
                        Some(b"CalendarType")
                            if stack.iter().any(|v| v.as_slice() == b"Recurrence") =>
                        {
                            current.recurrence.calendar_type = value.parse().ok()
                        }
                        Some(b"DisplayName")
                            if current.in_att_display_name && current.current_att.is_some() =>
                        {
                            if let Some(att) = current.current_att.as_mut() {
                                att.display_name = value;
                            }
                        }
                        Some(b"Method")
                            if current.in_att_method && current.current_att.is_some() =>
                        {
                            if let Some(att) = current.current_att.as_mut() {
                                att.method = value.parse().unwrap_or(1);
                            }
                        }
                        Some(b"EstimatedDataSize")
                            if current.in_att_estimated_data_size
                                && current.current_att.is_some() =>
                        {
                            if let Some(att) = current.current_att.as_mut() {
                                att.estimated_data_size = value.parse().unwrap_or(0);
                            }
                        }
                        Some(b"ContentType")
                            if current.in_att_content_type && current.current_att.is_some() =>
                        {
                            if let Some(att) = current.current_att.as_mut() {
                                att.content_type = value;
                            }
                        }
                        Some(b"ContentId")
                            if current.in_att_content_id && current.current_att.is_some() =>
                        {
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
                        Some(b"IsInline")
                            if current.in_att_is_inline && current.current_att.is_some() =>
                        {
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
                }
            }
            Ok(Event::End(e)) => {
                let local = e.name().local_name();
                let name: &[u8] = local.as_ref();
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

fn extract_ews_field_doc(doc: &Document, tag: &[u8]) -> Option<String> {
    let tag_str = std::str::from_utf8(tag).ok()?;
    doc.descendants()
        .filter(|n| n.is_element() && n.tag_name().name() == tag_str)
        .find_map(|n| n.text().map(|s| s.to_string()))
}

fn extract_ews_fields_doc(doc: &Document, tag: &[u8]) -> Vec<String> {
    let tag_str = std::str::from_utf8(tag).ok().unwrap_or_default();
    doc.descendants()
        .filter(|n| n.is_element() && n.tag_name().name() == tag_str)
        .filter_map(|n| n.text().map(|s| s.to_string()))
        .collect()
}
pub fn extract_ews_field(xml: &str, tag: &[u8]) -> Option<String> {
    let doc = Document::parse(xml).ok()?;
    extract_ews_field_doc(&doc, tag)
}

pub fn extract_ews_fields(xml: &str, tag: &[u8]) -> Vec<String> {
    let doc = match Document::parse(xml) {
        Ok(d) => d,
        Err(_) => return Vec::new(),
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

pub fn parse_ews_calendar_item(xml: &str) -> Result<CalendarItem> {
    let doc = Document::parse(xml).map_err(|e| anyhow!("failed to parse EWS XML: {e}"))?;

    let subject =
        extract_ews_field_doc(&doc, b"Subject").unwrap_or_else(|| "(no subject)".to_string());
    let start = extract_ews_field_doc(&doc, b"Start")
        .or_else(|| extract_ews_field_doc(&doc, b"StartTime"))
        .and_then(|v| parse_datetime(&v))
        .ok_or_else(|| anyhow!("missing Start/StartTime"))?;
    let end = extract_ews_field_doc(&doc, b"End")
        .or_else(|| extract_ews_field_doc(&doc, b"EndTime"))
        .and_then(|v| parse_datetime(&v))
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
        timezone: extract_ews_field_doc(&doc, b"StartTimeZone"),
        timezone_blob: extract_ews_field_doc(&doc, b"MeetingTimeZone"),
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
