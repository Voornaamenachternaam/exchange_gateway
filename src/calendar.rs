// src/calendar.rs
use crate::ical_parser;
use crate::util::nfc;
use anyhow::{Result, anyhow};
use chrono::{NaiveDate, NaiveDateTime, TimeZone, Utc};
use chrono_tz::Tz;
use quick_xml::Reader;
use quick_xml::events::Event;
use std::borrow::Cow;
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
    },
    Change {
        server_id: String,
        instance_id: Option<chrono::DateTime<Utc>>,
        patch: CalendarPatch,
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
            0 => "DAILY",
            1 => "WEEKLY",
            2 | 3 => "MONTHLY",
            5 | 6 => "YEARLY",
            _ => return None,
        };

        let mut parts = vec![format!("FREQ={freq}")];
        if let Some(interval) = self.interval
            && interval > 1
        {
            parts.push(format!("INTERVAL={interval}"));
        }
        if let Some(mask) = &self.day_of_week {
            let mut byday = Vec::new();
            let value = mask.parse::<u32>().unwrap_or(0);
            let mapping = [
                (1, "SU"),
                (2, "MO"),
                (4, "TU"),
                (8, "WE"),
                (16, "TH"),
                (32, "FR"),
                (64, "SA"),
            ];
            for (bit, code) in mapping {
                if value & bit != 0 {
                    byday.push(code.to_string());
                }
            }
            if !byday.is_empty() {
                if let Some(week) = self.week_of_month
                    && (kind == 3 || kind == 6)
                    && week > 0
                {
                    let ordinal = match week {
                        5 => -1,
                        n => n as i32,
                    };
                    parts.push(format!("BYDAY={}{}", ordinal, byday[0]));
                } else {
                    parts.push(format!("BYDAY={}", byday.join(",")));
                }
            }
        }
        if let Some(day) = self.day_of_month
            && matches!(kind, 2 | 5)
        {
            parts.push(format!("BYMONTHDAY={day}"));
        }
        if let Some(month) = self.month_of_year
            && matches!(kind, 5 | 6)
        {
            parts.push(format!("BYMONTH={month}"));
        }
        if let Some(count) = self.occurrences {
            parts.push(format!("COUNT={count}"));
        } else if let Some(until) = &self.until
            && let Some(dt) = parse_datetime(until)
        {
            parts.push(format!("UNTIL={}", dt.format("%Y%m%dT%H%M%SZ")));
        }
        if let Some(first_day) = self.first_day_of_week {
            parts.push(format!("WKST={}", weekday_code_from_eas(first_day)));
        }
        Some(parts.join(";"))
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

#[must_use]
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

#[must_use]
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

#[must_use]
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

#[must_use]
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

#[must_use]
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

    // Embed timezone blob if present — parse via icalendar so sub-components
    // (STANDARD/DAYLIGHT) and property escaping are handled correctly.
    if let Some(blob) = &item.timezone_blob {
        let wrapped = format!("BEGIN:VCALENDAR\r\n{blob}\r\nEND:VCALENDAR\r\n");
        if let Ok(parsed) = Calendar::from_str(&icalendar::parser::unfold(&wrapped)) {
            for component in parsed.iter() {
                if matches!(component, CalendarComponent::Other(_)) {
                    calendar.push(component.clone());
                }
            }
        }
    }

    // Build the main event using the icalendar crate (handles escaping + folding)
    let mut event = Event::new();
    event.uid(&uid);
    event.timestamp(dtstamp);
    event.summary(&item.subject);

    if item.all_day {
        // all_day(date) sets both DTSTART and DTEND to the same date, which
        // collapses multi-day events. Instead, set DTSTART as a DATE value
        // and append DTEND separately so multi-day ranges are preserved.
        event.starts(item.start.naive_utc().date());
        let end_date = item.end.naive_utc().date();
        if end_date != item.start.naive_utc().date() {
            event.append_property(Property::new("DTEND", end_date.format("%Y%m%d").to_string()));
        }
    } else if let Some(tzid) = &item.timezone
    && let Ok(tz) = tzid.parse::<Tz>()
    {
        // Convert UTC instant to local wall time for the WithTimezone variant,
        // which interprets the naive datetime as local time in the given tz.
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

    // EXDATE
    // When TZID is present, EXDATE values must be local time (no 'Z' suffix).
    // Per RFC 5545 §3.3.5, the 'Z' suffix means UTC, which contradicts TZID.
    let has_tzid = item.timezone.as_ref().is_some_and(|t| t.parse::<Tz>().is_ok());
    let mut all_exdates: Vec<String> = item
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
        .collect();
    let deleted_exdates: Vec<String> = item
        .exceptions
        .iter()
        .filter(|v| v.deleted)
        .map(|v| {
            if item.all_day {
                v.exception_start.format("%Y%m%d").to_string()
            } else if has_tzid {
                v.exception_start.format("%Y%m%dT%H%M%S").to_string()
            } else {
                v.exception_start.format("%Y%m%dT%H%M%SZ").to_string()
            }
        })
        .collect();
    all_exdates.extend(deleted_exdates);
    all_exdates.sort();
    all_exdates.dedup();
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
            event.append_property(Property::new("EXDATE", &exdate_str));
        }
    }

    // ORGANIZER
    if let Some(email) = &item.organizer_email {
        let mut org_prop = Property::new("ORGANIZER", normalize_mailto(email));
        if let Some(name) = &item.organizer_name {
            org_prop.add_parameter("CN", name);
        }
        event.append_property(org_prop.done());
    }

    // ATTENDEES — use the icalendar crate's Attendee builder
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

    // CATEGORIES
    if !item.categories.is_empty() {
        for category in &item.categories {
            event.add_property("CATEGORIES", category);
        }
    }

    // Microsoft-specific and custom X- properties
    if let Some(busy) = item.busy_status {
        event.append_property(Property::new("X-MICROSOFT-CDO-BUSYSTATUS", busy.to_string()));
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
        // Reminder is "minutes before start" (always positive), so the
        // iCal TRIGGER duration must be negative (before the event).
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

    // Exception instances (non-deleted)
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
        ex_event.summary(
            exception
                .subject
                .as_deref()
                .unwrap_or(&item.subject),
        );

        if effective_all_day {
            ex_event.append_property(Property::new(
                "RECURRENCE-ID",
                exception.exception_start.format("%Y%m%d").to_string(),
            ));
            // all_day(date) sets both DTSTART and DTEND to the same date, which
            // collapses multi-day events. Set DTSTART as a DATE value and append
            // DTEND separately so multi-day ranges are preserved.
            ex_event.starts(effective_start.naive_utc().date());
            let end_date = effective_end.naive_utc().date();
            if end_date != effective_start.naive_utc().date() {
                ex_event.append_property(Property::new("DTEND", end_date.format("%Y%m%d").to_string()));
            }
            "RECURRENCE-ID",
            exception.exception_start.format("%Y%m%d").to_string(),
        ));
        // all_day(date) sets both DTSTART and DTEND to the same date, which
        // collapses multi-day events. Set DTSTART as a DATE value and append
        // DTEND separately so multi-day ranges are preserved.
        ex_event.starts(effective_start.naive_utc().date());
        let end_date = effective_end.naive_utc().date();
        if end_date != effective_start.naive_utc().date() {
            ex_event.append_property(Property::new("DTEND", end_date.format("%Y%m%d").to_string()));
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
        // Convert UTC instant to local wall time for the WithTimezone variant
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
            ex_event.append_property(Property::new("CATEGORIES", categories.join(",")));
        }
        if let Some(busy) = exception.busy_status {
            ex_event.append_property(Property::new("X-MICROSOFT-CDO-BUSYSTATUS", busy.to_string()));
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

    // The icalendar crate's Display impl handles RFC 5545 line folding and CRLF
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

fn weekday_code_from_eas(value: u32) -> &'static str {
    match value {
        1 => "SU",
        2 => "MO",
        3 => "TU",
        4 => "WE",
        5 => "TH",
        6 => "FR",
        7 => "SA",
        _ => "MO",
    }
}

pub fn parse_eas_sync_mutations(xml: &str) -> Result<Vec<EasSyncMutation>> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);
    let mut buf = Vec::new();
    let mut stack: Vec<Vec<u8>> = Vec::new();
    let mut current_kind: Option<EasOpKind> = None;
    let mut current = EasBuilder::default();
    let mut out = Vec::new();

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) => {
                let name = e.name().local_name().as_ref().to_vec();
                if matches!(name.as_slice(), b"Add" | b"Change" | b"Delete") {
                    current_kind = Some(match name.as_slice() {
                        b"Add" => EasOpKind::Add,
                        b"Change" => EasOpKind::Change,
                        _ => EasOpKind::Delete,
                    });
                    current = EasBuilder::default();
                } else if name.as_slice() == b"Exception" {
                    current.current_exception = Some(CalendarException::default());
                } else if name.as_slice() == b"Recurrence" {
                    current.recurrence.is_empty = false;
                }
                stack.push(name);
            }
            Ok(Event::Empty(e)) => {
                let name = e.name().local_name().as_ref().to_vec();
                if name.as_slice() == b"Recurrence" {
                    current.recurrence.is_empty = true;
                }
                stack.push(name);
                stack.pop();
            }
            Ok(Event::Text(t)) => {
                if let Some(_kind) = current_kind {
                    let value = match t.decode() {
                        Ok(v) => v.into_owned(),
                        Err(_) => String::new(),
                    };
                    match stack.last().map(|v| v.as_slice()) {
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
                        _ => {}
                    }
                }
            }
            Ok(Event::End(e)) => {
                let name = e.name().local_name().as_ref().to_vec();
                if name.as_slice() == b"Attendee"
                    && let Some(attendee) = current.current_attendee.take()
                    && !attendee.email.is_empty()
                {
                    if let Some(ex) = current.current_exception.as_mut() {
                        ex.attendees.get_or_insert_with(Vec::new).push(attendee);
                    } else {
                        current.attendees.push(attendee);
                    }
                }
                if name.as_slice() == b"Exception"
                    && let Some(exception) = current.current_exception.take()
                {
                    current.exceptions.push(exception);
                }
                if matches!(name.as_slice(), b"Add" | b"Change" | b"Delete") {
                    match current_kind.take() {
                        Some(EasOpKind::Add) => {
                            let client_id = current.client_id.clone();
                            let builder = std::mem::take(&mut current);
                            out.push(EasSyncMutation::Add {
                                client_id,
                                item: builder.into_item()?,
                            });
                        }
                        Some(EasOpKind::Change) => {
                            let server_id = current.server_id.clone().unwrap_or_default();
                            let instance_id = current.instance_id;
                            let builder = std::mem::take(&mut current);
                            out.push(EasSyncMutation::Change {
                                server_id,
                                instance_id,
                                patch: builder.into_patch(),
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

pub fn extract_ews_field(xml: &str, tag: &[u8]) -> Option<String> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);
    let mut buf = Vec::new();
    let mut inside = false;
    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) if e.name().local_name().as_ref() == tag => inside = true,
            Ok(Event::Text(t)) if inside => {
                let decoded: std::result::Result<Cow<'_, str>, _> =
                    t.decode().map_err(|e| anyhow!(e));
                return decoded.ok().map(|v| v.into_owned());
            }
            Ok(Event::End(e)) if e.name().local_name().as_ref() == tag => inside = false,
            Ok(Event::Eof) | Err(_) => return None,
            _ => {}
        }
        buf.clear();
    }
}

pub fn extract_ews_fields(xml: &str, tag: &[u8]) -> Vec<String> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);
    let mut buf = Vec::new();
    let mut inside = false;
    let mut out = Vec::new();
    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) if e.name().local_name().as_ref() == tag => inside = true,
            Ok(Event::Text(t)) if inside => {
                if let Ok(v) = t.decode() {
                    out.push(v.into_owned());
                }
            }
            Ok(Event::End(e)) if e.name().local_name().as_ref() == tag => inside = false,
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
        buf.clear();
    }
    out
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
    let interval = extract_ews_field(xml, b"Interval")
        .and_then(|v| v.parse::<u32>().ok())
        .unwrap_or(1);
    let mut parts = Vec::new();
    if xml.contains("DailyRecurrence") {
        parts.push("FREQ=DAILY".to_string());
    } else if xml.contains("WeeklyRecurrence") {
        parts.push("FREQ=WEEKLY".to_string());
        if let Some(days) = extract_ews_field(xml, b"DaysOfWeek") {
            let (mask, _) = parse_ews_days_mask(&days);
            if mask != "0" {
                let value = mask.parse::<u32>().ok()?;
                let mapping = [
                    (1, "SU"),
                    (2, "MO"),
                    (4, "TU"),
                    (8, "WE"),
                    (16, "TH"),
                    (32, "FR"),
                    (64, "SA"),
                ];
                let byday: Vec<&str> = mapping
                    .iter()
                    .filter_map(
                        |(bit, code)| {
                            if value & bit != 0 { Some(*code) } else { None }
                        },
                    )
                    .collect();
                if !byday.is_empty() {
                    parts.push(format!("BYDAY={}", byday.join(",")));
                }
            }
        }
    } else if xml.contains("AbsoluteMonthlyRecurrence") {
        parts.push("FREQ=MONTHLY".to_string());
        if let Some(day) = extract_ews_field(xml, b"DayOfMonth") {
            parts.push(format!("BYMONTHDAY={day}"));
        }
    } else if xml.contains("RelativeMonthlyRecurrence") {
        parts.push("FREQ=MONTHLY".to_string());
        if let Some(days) = extract_ews_field(xml, b"DaysOfWeek") {
            let (mask, ordinal) = parse_ews_days_mask(&days);
            let value = mask.parse::<u32>().ok()?;
            let mapping = [
                (1, "SU"),
                (2, "MO"),
                (4, "TU"),
                (8, "WE"),
                (16, "TH"),
                (32, "FR"),
                (64, "SA"),
            ];
            let code = mapping
                .iter()
                .find_map(|(bit, code)| (value & bit != 0).then_some(*code))?;
            let ord = ordinal.unwrap_or_else(|| {
                extract_ews_field(xml, b"DayOfWeekIndex")
                    .as_deref()
                    .and_then(|v| match v {
                        "First" => Some(1),
                        "Second" => Some(2),
                        "Third" => Some(3),
                        "Fourth" => Some(4),
                        "Last" => Some(-1),
                        _ => None,
                    })
                    .unwrap_or(1)
            });
            parts.push(format!("BYDAY={}{}", ord, code));
        }
    } else if xml.contains("AbsoluteYearlyRecurrence") {
        parts.push("FREQ=YEARLY".to_string());
        if let Some(month) = extract_ews_field(xml, b"Month").and_then(|v| parse_ews_month(&v)) {
            parts.push(format!("BYMONTH={month}"));
        }
        if let Some(day) = extract_ews_field(xml, b"DayOfMonth") {
            parts.push(format!("BYMONTHDAY={day}"));
        }
    } else if xml.contains("RelativeYearlyRecurrence") {
        parts.push("FREQ=YEARLY".to_string());
        if let Some(month) = extract_ews_field(xml, b"Month").and_then(|v| parse_ews_month(&v)) {
            parts.push(format!("BYMONTH={month}"));
        }
        if let Some(days) = extract_ews_field(xml, b"DaysOfWeek") {
            let (mask, ordinal) = parse_ews_days_mask(&days);
            let value = mask.parse::<u32>().ok()?;
            let mapping = [
                (1, "SU"),
                (2, "MO"),
                (4, "TU"),
                (8, "WE"),
                (16, "TH"),
                (32, "FR"),
                (64, "SA"),
            ];
            let code = mapping
                .iter()
                .find_map(|(bit, code)| (value & bit != 0).then_some(*code))?;
            parts.push(format!("BYDAY={}{}", ordinal.unwrap_or(1), code));
        }
    } else {
        return None;
    }

    if interval > 1 {
        parts.push(format!("INTERVAL={interval}"));
    }
    if let Some(count) = extract_ews_field(xml, b"NumberOfOccurrences") {
        parts.push(format!("COUNT={count}"));
    } else if let Some(until) = extract_ews_field(xml, b"EndDate").and_then(|v| parse_datetime(&v))
    {
        parts.push(format!("UNTIL={}", until.format("%Y%m%dT%H%M%SZ")));
    }
    Some(parts.join(";"))
}

pub fn parse_ews_attendees(xml: &str) -> Vec<Attendee> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);
    let mut buf = Vec::new();
    let mut stack: Vec<Vec<u8>> = Vec::new();
    let mut attendees = Vec::new();
    let mut current: Option<Attendee> = None;
    let mut attendee_type = None;
    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) => {
                let name = e.name().local_name().as_ref().to_vec();
                if name.as_slice() == b"RequiredAttendees" {
                    attendee_type = Some(1u8);
                } else if name.as_slice() == b"OptionalAttendees" {
                    attendee_type = Some(2u8);
                } else if name.as_slice() == b"Attendee" {
                    current = Some(Attendee {
                        attendee_type,
                        ..Default::default()
                    });
                }
                stack.push(name);
            }
            Ok(Event::Text(t)) => {
                if let Some(attendee) = current.as_mut() {
                    let value = t.decode().ok().map(|v| v.into_owned()).unwrap_or_default();
                    match stack.last().map(|v| v.as_slice()) {
                        Some(b"Name") => attendee.name = Some(value),
                        Some(b"EmailAddress") => attendee.email = nfc(&value),
                        Some(b"ResponseType") => {
                            let (status, partstat) = match value.as_str() {
                                "Accept" => (Some(3), Some("ACCEPTED".to_string())),
                                "Tentative" => (Some(2), Some("TENTATIVE".to_string())),
                                "Decline" => (Some(4), Some("DECLINED".to_string())),
                                _ => (Some(5), Some("NEEDS-ACTION".to_string())),
                            };
                            attendee.attendee_status = status;
                            attendee.partstat = partstat;
                        }
                        _ => {}
                    }
                }
            }
            Ok(Event::End(e)) => {
                let name = e.name().local_name().as_ref().to_vec();
                if name.as_slice() == b"Attendee"
                    && let Some(attendee) = current.take()
                    && !attendee.email.is_empty()
                {
                    attendees.push(attendee);
                } else if matches!(name.as_slice(), b"RequiredAttendees" | b"OptionalAttendees") {
                    attendee_type = None;
                }
                stack.pop();
            }
            Ok(Event::Eof) => break,
            Err(_) => break,
            _ => {}
        }
        buf.clear();
    }
    attendees
}

pub fn parse_ews_calendar_item(xml: &str) -> Result<CalendarItem> {
    let subject = extract_ews_field(xml, b"Subject").unwrap_or_else(|| "(no subject)".to_string());
    let start = extract_ews_field(xml, b"Start")
        .or_else(|| extract_ews_field(xml, b"StartTime"))
        .and_then(|v| parse_datetime(&v))
        .ok_or_else(|| anyhow!("missing Start/StartTime"))?;
    let end = extract_ews_field(xml, b"End")
        .or_else(|| extract_ews_field(xml, b"EndTime"))
        .and_then(|v| parse_datetime(&v))
        .ok_or_else(|| anyhow!("missing End/EndTime"))?;
    let uid = extract_ews_field(xml, b"UID")
        .or_else(|| extract_ews_field(xml, b"ClientUid"))
        .unwrap_or_else(|| Uuid::new_v4().to_string());
    let description = extract_ews_field(xml, b"Body")
        .or_else(|| extract_ews_field(xml, b"TextBody"))
        .unwrap_or_default();
    let location = extract_ews_field(xml, b"Location").unwrap_or_default();
    let all_day = extract_ews_field(xml, b"IsAllDayEvent")
        .map(|v| v.eq_ignore_ascii_case("true"))
        .unwrap_or(false);
    let organizer_name = extract_ews_field(xml, b"OrganizerName");
    let organizer_email = extract_ews_field(xml, b"OrganizerEmail");
    let categories = extract_ews_fields(xml, b"String");
    let attendees = parse_ews_attendees(xml);
    let reminder =
        extract_ews_field(xml, b"ReminderMinutesBeforeStart").and_then(|v| v.parse().ok());
    let busy_status =
        extract_ews_field(xml, b"LegacyFreeBusyStatus").and_then(|v| match v.as_str() {
            "Free" => Some(0),
            "Tentative" => Some(1),
            "Busy" => Some(2),
            "OOF" => Some(3),
            _ => None,
        });
    let sensitivity = extract_ews_field(xml, b"Sensitivity").and_then(|v| match v.as_str() {
        "Normal" => Some(0),
        "Personal" => Some(1),
        "Private" => Some(2),
        "Confidential" => Some(3),
        _ => None,
    });
    let response_requested =
        extract_ews_field(xml, b"ResponseRequested").map(|v| v.eq_ignore_ascii_case("true"));
    let disallow_new_time_proposal =
        extract_ews_field(xml, b"DisallowNewTimeProposal").map(|v| v.eq_ignore_ascii_case("true"));
    let online_meeting_conf_link = extract_ews_field(xml, b"OnlineMeetingConfLink");
    let online_meeting_external_link = extract_ews_field(xml, b"OnlineMeetingExternalLink");
    let client_uid = extract_ews_field(xml, b"ClientUid");
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
        timezone: extract_ews_field(xml, b"StartTimeZone"),
        timezone_blob: extract_ews_field(xml, b"MeetingTimeZone"),
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
