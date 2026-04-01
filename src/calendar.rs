// src/calendar.rs
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
        patch: CalendarPatch,
    },
    Delete {
        server_id: String,
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
    client_uid: Option<String>,
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
}

impl EasBuilder {
    fn into_item(self) -> Result<CalendarItem> {
        let start = self.start.ok_or_else(|| anyhow!("missing StartTime"))?;
        let end = self.end.ok_or_else(|| anyhow!("missing EndTime"))?;
        Ok(CalendarItem {
            uid: self.uid.unwrap_or_else(|| Uuid::new_v4().to_string()),
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
            rrule: self.recurrence.to_rrule(),
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
    fn to_rrule(&self) -> Option<String> {
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

fn escape_ical_text(input: &str) -> String {
    input
        .replace('\\', "\\\\")
        .replace(';', "\\;")
        .replace(',', "\\,")
        .replace('\n', "\\n")
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
                Some(next) => {
                    out.push(next);
                }
                None => break,
            }
        } else {
            out.push(ch);
        }
    }
    out
}

pub fn parse_ics_content(ics: &str) -> Vec<(String, String)> {
    let mut properties = Vec::new();
    let unfolded = ics.replace("\r\n ", "").replace("\r\n\t", "");
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

fn split_ical_blocks(ics: &str) -> Vec<Vec<String>> {
    let unfolded = ics.replace("\r\n ", "").replace("\r\n\t", "");
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
    let unfolded = ics.replace("\r\n ", "").replace("\r\n\t", "");
    let mut lines = Vec::new();
    let mut in_vtimezone = false;
    for line in unfolded.lines() {
        match line.trim() {
            "BEGIN:VTIMEZONE" => {
                in_vtimezone = true;
                lines.push("BEGIN:VTIMEZONE".to_string());
            }
            "END:VTIMEZONE" if in_vtimezone => {
                lines.push("END:VTIMEZONE".to_string());
                break;
            }
            _ if in_vtimezone => lines.push(line.to_string()),
            _ => {}
        }
    }
    (!lines.is_empty()).then(|| lines.join("\r\n"))
}

fn parse_categories_value(value: &str) -> Vec<String> {
    value
        .split(',')
        .map(unescape_ical_text)
        .filter(|v| !v.is_empty())
        .collect()
}

fn parse_tzid_from_key(key: &str) -> Option<String> {
    parse_ical_param(key, "TZID").map(|v| v.trim_matches('"').to_string())
}

fn parse_datetime_with_tzid(val: &str, tzid: Option<&str>) -> Option<chrono::DateTime<Utc>> {
    if let Some(tzid) = tzid {
        if !val.ends_with('Z') && val.contains('T') {
            if let Ok(local) = NaiveDateTime::parse_from_str(val, "%Y%m%dT%H%M%S") {
                if let Ok(tz) = tzid.parse::<Tz>() {
                    if let Some(dt) = tz.from_local_datetime(&local).single() {
                        return Some(dt.with_timezone(&Utc));
                    }
                    if let Some(dt) = tz.from_local_datetime(&local).earliest() {
                        return Some(dt.with_timezone(&Utc));
                    }
                }
            }
            if let Ok(local) = NaiveDateTime::parse_from_str(val, "%Y-%m-%dT%H:%M:%S") {
                if let Ok(tz) = tzid.parse::<Tz>() {
                    if let Some(dt) = tz.from_local_datetime(&local).single() {
                        return Some(dt.with_timezone(&Utc));
                    }
                    if let Some(dt) = tz.from_local_datetime(&local).earliest() {
                        return Some(dt.with_timezone(&Utc));
                    }
                }
            }
        }
    }
    parse_datetime(val)
}

fn format_ical_datetime_with_timezone(
    dt: &chrono::DateTime<Utc>,
    all_day: bool,
    timezone: Option<&str>,
) -> (Option<String>, String) {
    if all_day {
        return (None, dt.format("%Y%m%d").to_string());
    }
    if let Some(tzid) = timezone {
        if let Ok(tz) = tzid.parse::<Tz>() {
            return (
                Some(tzid.to_string()),
                dt.with_timezone(&tz).format("%Y%m%dT%H%M%S").to_string(),
            );
        }
    }
    (None, dt.format("%Y%m%dT%H%M%SZ").to_string())
}

fn format_ical_datetime(dt: &chrono::DateTime<Utc>, all_day: bool) -> String {
    if all_day {
        dt.format("%Y%m%d").to_string()
    } else {
        dt.format("%Y%m%dT%H%M%SZ").to_string()
    }
}

fn parse_duration_minutes(trigger: &str) -> Option<i32> {
    let negative = trigger.starts_with('-');
    let value = trigger
        .trim_start_matches('-')
        .trim_start_matches('P')
        .trim_start_matches('T');
    if let Some(raw) = value.strip_suffix('M') {
        let mins = raw.parse::<i32>().ok()?;
        return Some(if negative { mins } else { -mins });
    }
    if let Some(raw) = value.strip_suffix('H') {
        let hours = raw.parse::<i32>().ok()?;
        return Some(if negative { hours * 60 } else { -(hours * 60) });
    }
    None
}

fn render_valarm(minutes_before_start: i32) -> Vec<String> {
    let abs = minutes_before_start.abs();
    let trigger = if abs % 60 == 0 {
        format!("-PT{}H", abs / 60)
    } else {
        format!("-PT{}M", abs)
    };
    vec![
        "BEGIN:VALARM".to_string(),
        "ACTION:DISPLAY".to_string(),
        "DESCRIPTION:Reminder".to_string(),
        format!("TRIGGER:{trigger}"),
        "END:VALARM".to_string(),
    ]
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
                fields.description = Some(unescape_ical_text(value));
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
    let dtstamp = item
        .dtstamp
        .unwrap_or_else(Utc::now)
        .format("%Y%m%dT%H%M%SZ")
        .to_string();
    let uid = if item.uid.is_empty() {
        Uuid::new_v4().to_string()
    } else {
        item.uid.clone()
    };

    let mut lines = vec![
        "BEGIN:VCALENDAR".to_string(),
        "VERSION:2.0".to_string(),
        "PRODID:-//exchange_gateway//EN".to_string(),
    ];
    if let Some(blob) = &item.timezone_blob {
        for line in blob.lines() {
            let trimmed = line.trim_end();
            if !trimmed.is_empty() {
                lines.push(trimmed.to_string());
            }
        }
    }
    let (dtstart_tzid, dtstart_value) =
        format_ical_datetime_with_timezone(&item.start, item.all_day, item.timezone.as_deref());
    let (dtend_tzid, dtend_value) =
        format_ical_datetime_with_timezone(&item.end, item.all_day, item.timezone.as_deref());
    let dtstart_line = if let Some(tzid) = dtstart_tzid {
        format!("DTSTART;TZID={}:{}", escape_ical_text(&tzid), dtstart_value)
    } else {
        format!("DTSTART:{}", dtstart_value)
    };
    let dtend_line = if let Some(tzid) = dtend_tzid {
        format!("DTEND;TZID={}:{}", escape_ical_text(&tzid), dtend_value)
    } else {
        format!("DTEND:{}", dtend_value)
    };
    lines.extend([
        "BEGIN:VEVENT".to_string(),
        format!("UID:{uid}"),
        format!("DTSTAMP:{dtstamp}"),
        format!("SUMMARY:{}", escape_ical_text(&item.subject)),
        dtstart_line,
        dtend_line,
    ]);
    if !item.location.is_empty() {
        lines.push(format!("LOCATION:{}", escape_ical_text(&item.location)));
    }
    if !item.description.is_empty() {
        lines.push(format!(
            "DESCRIPTION:{}",
            escape_ical_text(&item.description)
        ));
    }
    if let Some(rrule) = &item.rrule
        && !rrule.is_empty()
    {
        lines.push(format!("RRULE:{rrule}"));
    }
    let deleted_exdates: Vec<_> = item
        .exceptions
        .iter()
        .filter(|v| v.deleted)
        .map(|v| {
            format_ical_datetime_with_timezone(
                &v.exception_start,
                item.all_day,
                item.timezone.as_deref(),
            )
            .1
        })
        .collect();
    if !item.exdates.is_empty() || !deleted_exdates.is_empty() {
        let mut exdates: Vec<String> = item
            .exdates
            .iter()
            .map(|v| {
                format_ical_datetime_with_timezone(v, item.all_day, item.timezone.as_deref()).1
            })
            .collect();
        exdates.extend(deleted_exdates);
        exdates.sort();
        exdates.dedup();
        if !item.all_day {
            if let Some(tzid) = &item.timezone
                && tzid.parse::<Tz>().is_ok()
            {
                lines.push(format!(
                    "EXDATE;TZID={}:{}",
                    escape_ical_text(tzid),
                    exdates.join(",")
                ));
            } else {
                lines.push(format!("EXDATE:{}", exdates.join(",")));
            }
        } else {
            lines.push(format!("EXDATE:{}", exdates.join(",")));
        }
    }
    if let Some(email) = &item.organizer_email {
        let mut line = String::from("ORGANIZER");
        if let Some(name) = &item.organizer_name {
            line.push_str(&format!(";CN={}", escape_ical_text(name)));
        }
        line.push(':');
        line.push_str(&normalize_mailto(email));
        lines.push(line);
    }
    for attendee in &item.attendees {
        if attendee.email.is_empty() {
            continue;
        }
        lines.push(render_attendee_line(attendee));
    }
    if !item.categories.is_empty() {
        lines.push(format!(
            "CATEGORIES:{}",
            item.categories
                .iter()
                .map(|v| escape_ical_text(v))
                .collect::<Vec<_>>()
                .join(",")
        ));
    }
    if let Some(busy) = item.busy_status {
        lines.push(format!("X-MICROSOFT-CDO-BUSYSTATUS:{busy}"));
        lines.push(format!(
            "TRANSP:{}",
            if busy == 0 { "TRANSPARENT" } else { "OPAQUE" }
        ));
    }
    if let Some(sensitivity) = item.sensitivity {
        lines.push(format!("CLASS:{}", sensitivity_to_class(sensitivity)));
    }
    if let Some(reminder) = item.reminder {
        lines.extend(render_valarm(reminder));
    }
    if let Some(v) = item.response_requested {
        lines.push(format!(
            "X-MS-RESPONSE-REQUESTED:{}",
            if v { "TRUE" } else { "FALSE" }
        ));
    }
    if let Some(v) = item.disallow_new_time_proposal {
        lines.push(format!(
            "X-MS-DISALLOW-COUNTER:{}",
            if v { "TRUE" } else { "FALSE" }
        ));
    }
    if let Some(v) = item.appointment_reply_time {
        lines.push(format!(
            "X-MS-APPOINTMENT-REPLY-TIME:{}",
            v.format("%Y%m%dT%H%M%SZ")
        ));
    }
    if let Some(v) = item.meeting_status {
        lines.push(format!("X-MS-MEETING-STATUS:{v}"));
    }
    if let Some(v) = item.response_type {
        lines.push(format!("X-MS-RESPONSE-TYPE:{v}"));
    }
    if let Some(v) = &item.online_meeting_conf_link {
        lines.push(format!("X-MS-OLK-CONFLINK:{}", escape_ical_text(v)));
    }
    if let Some(v) = &item.online_meeting_external_link {
        lines.push(format!("X-MS-OLK-EXTERNALLINK:{}", escape_ical_text(v)));
    }
    if let Some(v) = &item.client_uid {
        lines.push(format!("X-MS-CLIENT-UID:{}", escape_ical_text(v)));
    }
    if !item.all_day {
        if let Some(v) = &item.timezone {
            lines.push(format!("X-EAS-TIMEZONE:{}", escape_ical_text(v)));
        }
    }
    lines.push("END:VEVENT".to_string());

    for exception in item.exceptions.iter().filter(|v| !v.deleted) {
        let base_duration = item.end - item.start;
        let effective_all_day = exception.all_day.unwrap_or(item.all_day);
        let effective_start = exception.start.unwrap_or(exception.exception_start);
        let effective_end = exception
            .end
            .unwrap_or_else(|| effective_start + base_duration);
        lines.push("BEGIN:VEVENT".to_string());
        lines.push(format!("UID:{uid}"));
        lines.push(format!("DTSTAMP:{dtstamp}"));
        let (recurrence_tzid, recurrence_value) = format_ical_datetime_with_timezone(
            &exception.exception_start,
            effective_all_day,
            item.timezone.as_deref(),
        );
        let (exception_start_tzid, exception_start_value) = format_ical_datetime_with_timezone(
            &effective_start,
            effective_all_day,
            item.timezone.as_deref(),
        );
        let (exception_end_tzid, exception_end_value) = format_ical_datetime_with_timezone(
            &effective_end,
            effective_all_day,
            item.timezone.as_deref(),
        );
        lines.push(if let Some(tzid) = recurrence_tzid {
            format!(
                "RECURRENCE-ID;TZID={}:{}",
                escape_ical_text(&tzid),
                recurrence_value
            )
        } else {
            format!("RECURRENCE-ID:{}", recurrence_value)
        });
        lines.push(if let Some(tzid) = exception_start_tzid {
            format!(
                "DTSTART;TZID={}:{}",
                escape_ical_text(&tzid),
                exception_start_value
            )
        } else {
            format!("DTSTART:{}", exception_start_value)
        });
        lines.push(if let Some(tzid) = exception_end_tzid {
            format!(
                "DTEND;TZID={}:{}",
                escape_ical_text(&tzid),
                exception_end_value
            )
        } else {
            format!("DTEND:{}", exception_end_value)
        });
        lines.push(format!(
            "SUMMARY:{}",
            escape_ical_text(exception.subject.as_deref().unwrap_or(&item.subject))
        ));
        if let Some(location) = exception.location.as_deref().or(Some(&item.location))
            && !location.is_empty()
        {
            lines.push(format!("LOCATION:{}", escape_ical_text(location)));
        }
        if let Some(description) = exception.description.as_deref().or(Some(&item.description))
            && !description.is_empty()
        {
            lines.push(format!("DESCRIPTION:{}", escape_ical_text(description)));
        }
        if let Some(attendees) = &exception.attendees {
            for attendee in attendees {
                lines.push(render_attendee_line(attendee));
            }
        }
        if let Some(categories) = &exception.categories
            && !categories.is_empty()
        {
            lines.push(format!(
                "CATEGORIES:{}",
                categories
                    .iter()
                    .map(|v| escape_ical_text(v))
                    .collect::<Vec<_>>()
                    .join(",")
            ));
        }
        if let Some(busy) = exception.busy_status {
            lines.push(format!("X-MICROSOFT-CDO-BUSYSTATUS:{busy}"));
        }
        if let Some(sensitivity) = exception.sensitivity {
            lines.push(format!("CLASS:{}", sensitivity_to_class(sensitivity)));
        }
        if let Some(reminder) = exception.reminder {
            lines.extend(render_valarm(reminder));
        }
        if let Some(v) = exception.appointment_reply_time {
            lines.push(format!(
                "X-MS-APPOINTMENT-REPLY-TIME:{}",
                v.format("%Y%m%dT%H%M%SZ")
            ));
        }
        if let Some(v) = exception.meeting_status {
            lines.push(format!("X-MS-MEETING-STATUS:{v}"));
        }
        if let Some(v) = exception.response_type {
            lines.push(format!("X-MS-RESPONSE-TYPE:{v}"));
        }
        lines.push("END:VEVENT".to_string());
    }

    lines.push("END:VCALENDAR".to_string());
    format!("{}\r\n", lines.join("\r\n"))
}

fn render_attendee_line(attendee: &Attendee) -> String {
    let mut line = String::from("ATTENDEE");
    if let Some(name) = &attendee.name {
        line.push_str(&format!(";CN={}", escape_ical_text(name)));
    }
    if let Some(kind) = attendee.attendee_type {
        let role = match kind {
            2 => "OPT-PARTICIPANT",
            3 => "NON-PARTICIPANT",
            _ => "REQ-PARTICIPANT",
        };
        line.push_str(&format!(";ROLE={role}"));
    }
    if let Some(partstat) = &attendee.partstat {
        line.push_str(&format!(";PARTSTAT={partstat}"));
    }
    line.push(':');
    line.push_str(&normalize_mailto(&attendee.email));
    line
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
    let email = value
        .strip_prefix("mailto:")
        .or_else(|| value.strip_prefix("MAILTO:"))
        .unwrap_or(value)
        .to_string();
    (name, Some(email))
}

fn normalize_mailto(email: &str) -> String {
    if email.to_ascii_lowercase().starts_with("mailto:") {
        email.to_string()
    } else {
        format!("mailto:{email}")
    }
}

fn partstat_to_status(value: &str) -> u8 {
    match value {
        "ACCEPTED" => 3,
        "DECLINED" => 4,
        "TENTATIVE" => 2,
        _ => 5,
    }
}

fn status_to_partstat(value: u8) -> String {
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

fn sensitivity_to_class(value: u8) -> &'static str {
    match value {
        2 => "PRIVATE",
        3 => "CONFIDENTIAL",
        _ => "PUBLIC",
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

fn weekday_code_to_eas(value: &str) -> Option<u32> {
    match value {
        "SU" => Some(1),
        "MO" => Some(2),
        "TU" => Some(3),
        "WE" => Some(4),
        "TH" => Some(5),
        "FR" => Some(6),
        "SA" => Some(7),
        _ => None,
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
                }
                stack.push(name);
            }
            Ok(Event::Empty(e)) => {
                let name = e.name().local_name().as_ref().to_vec();
                stack.push(name);
                stack.pop();
            }
            Ok(Event::Text(t)) => {
                if let Some(kind) = current_kind {
                    let value = match t.decode() {
                        Ok(v) => v.into_owned(),
                        Err(_) => String::new(),
                    };
                    match stack.last().map(|v| v.as_slice()) {
                        Some(b"ClientId") => current.client_id = Some(value),
                        Some(b"ServerId") => current.server_id = Some(value),
                        Some(b"Subject") => {
                            if let Some(exception) = current.current_exception.as_mut() {
                                exception.subject = Some(value);
                            } else {
                                current.subject = Some(value);
                            }
                        }
                        Some(b"Location") => {
                            if let Some(exception) = current.current_exception.as_mut() {
                                exception.location = Some(value);
                            } else {
                                current.location = Some(value);
                            }
                        }
                        Some(b"Timezone") => current.timezone = Some(value),
                        Some(b"DtStamp") => current.dtstamp = parse_datetime(&value),
                        Some(b"StartTime") => {
                            if let Some(exception) = current.current_exception.as_mut() {
                                exception.start = parse_datetime(&value);
                            } else {
                                current.start = parse_datetime(&value);
                            }
                        }
                        Some(b"EndTime") => {
                            if let Some(exception) = current.current_exception.as_mut() {
                                exception.end = parse_datetime(&value);
                            } else {
                                current.end = parse_datetime(&value);
                            }
                        }
                        Some(b"AllDayEvent") => {
                            if let Some(exception) = current.current_exception.as_mut() {
                                exception.all_day = Some(value == "1");
                            } else {
                                current.all_day = Some(value == "1");
                            }
                        }
                        Some(b"UID") => current.uid = Some(value),
                        Some(b"OrganizerName") => current.organizer_name = Some(value),
                        Some(b"OrganizerEmail") => current.organizer_email = Some(value),
                        Some(b"BusyStatus") => {
                            if let Some(exception) = current.current_exception.as_mut() {
                                exception.busy_status = value.parse().ok();
                            } else {
                                current.busy_status = value.parse().ok();
                            }
                        }
                        Some(b"Sensitivity") => {
                            if let Some(exception) = current.current_exception.as_mut() {
                                exception.sensitivity = value.parse().ok();
                            } else {
                                current.sensitivity = value.parse().ok();
                            }
                        }
                        Some(b"Reminder") => {
                            if let Some(exception) = current.current_exception.as_mut() {
                                exception.reminder = value.parse().ok();
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
                            if let Some(exception) = current.current_exception.as_mut() {
                                exception.appointment_reply_time = parse_datetime(&value);
                            } else {
                                current.appointment_reply_time = parse_datetime(&value)
                            }
                        }
                        Some(b"MeetingStatus") => {
                            if let Some(exception) = current.current_exception.as_mut() {
                                exception.meeting_status = value.parse().ok();
                            } else {
                                current.meeting_status = value.parse().ok()
                            }
                        }
                        Some(b"ResponseType") => {
                            if let Some(exception) = current.current_exception.as_mut() {
                                exception.response_type = value.parse().ok();
                            } else {
                                current.response_type = value.parse().ok()
                            }
                        }
                        Some(b"OnlineMeetingConfLink") => {
                            current.online_meeting_conf_link = Some(value)
                        }
                        Some(b"OnlineMeetingExternalLink") => {
                            current.online_meeting_external_link = Some(value)
                        }
                        Some(b"ClientUid") => current.client_uid = Some(value),
                        Some(b"Category") => {
                            if let Some(exception) = current.current_exception.as_mut() {
                                let categories = exception.categories.get_or_insert_with(Vec::new);
                                categories.push(value);
                            } else {
                                current.categories.push(value);
                            }
                        }
                        Some(b"Name") if stack.iter().any(|v| v.as_slice() == b"Attendee") => {
                            let attendee = current
                                .current_attendee
                                .get_or_insert_with(Attendee::default);
                            attendee.name = Some(value);
                        }
                        Some(b"Email") if stack.iter().any(|v| v.as_slice() == b"Attendee") => {
                            let attendee = current
                                .current_attendee
                                .get_or_insert_with(Attendee::default);
                            attendee.email = value;
                        }
                        Some(b"AttendeeType")
                            if stack.iter().any(|v| v.as_slice() == b"Attendee") =>
                        {
                            let attendee = current
                                .current_attendee
                                .get_or_insert_with(Attendee::default);
                            attendee.attendee_type = value.parse().ok();
                        }
                        Some(b"AttendeeStatus")
                            if stack.iter().any(|v| v.as_slice() == b"Attendee") =>
                        {
                            let attendee = current
                                .current_attendee
                                .get_or_insert_with(Attendee::default);
                            let status = value.parse().ok();
                            attendee.attendee_status = status;
                            attendee.partstat = status.map(status_to_partstat);
                        }
                        Some(b"Deleted") if stack.iter().any(|v| v.as_slice() == b"Exception") => {
                            if let Some(exception) = current.current_exception.as_mut() {
                                exception.deleted = value == "1";
                            }
                        }
                        Some(b"ExceptionStartTime")
                            if stack.iter().any(|v| v.as_slice() == b"Exception") =>
                        {
                            if let Some(exception) = current.current_exception.as_mut() {
                                exception.exception_start = parse_datetime(&value)
                                    .ok_or_else(|| anyhow!("invalid ExceptionStartTime"))?;
                            }
                        }
                        Some(b"Data") if stack.iter().any(|v| v.as_slice() == b"Body") => {
                            if let Some(exception) = current.current_exception.as_mut() {
                                exception.description = Some(value);
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
                        _ => {
                            let _ = kind;
                        }
                    }
                }
            }
            Ok(Event::End(e)) => {
                let name = e.name().local_name().as_ref().to_vec();
                if name.as_slice() == b"Attendee"
                    && let Some(attendee) = current.current_attendee.take()
                    && !attendee.email.is_empty()
                {
                    if let Some(exception) = current.current_exception.as_mut() {
                        let attendees = exception.attendees.get_or_insert_with(Vec::new);
                        attendees.push(attendee);
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
                        Some(EasOpKind::Add) => out.push(EasSyncMutation::Add {
                            client_id: current.client_id.clone(),
                            item: current.into_item()?,
                        }),
                        Some(EasOpKind::Change) => out.push(EasSyncMutation::Change {
                            server_id: current.server_id.clone().unwrap_or_default(),
                            patch: current.into_patch(),
                        }),
                        Some(EasOpKind::Delete) => out.push(EasSyncMutation::Delete {
                            server_id: current.server_id.clone().unwrap_or_default(),
                        }),
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

fn parse_ews_bool(xml: &str, tag: &[u8]) -> Option<bool> {
    extract_ews_field(xml, tag).map(|v| v.eq_ignore_ascii_case("true"))
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
                let mut byday = Vec::new();
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
                for (bit, code) in mapping {
                    if value & bit != 0 {
                        byday.push(code.to_string());
                    }
                }
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
                    let value = match t.decode() {
                        Ok(v) => v.into_owned(),
                        Err(_) => String::new(),
                    };
                    match stack.last().map(|v| v.as_slice()) {
                        Some(b"Name") => attendee.name = Some(value),
                        Some(b"EmailAddress") => attendee.email = value,
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
    let response_requested = parse_ews_bool(xml, b"ResponseRequested");
    let disallow_new_time_proposal = parse_ews_bool(xml, b"DisallowNewTimeProposal");
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

#[cfg(test)]
mod tests {
use super::{
    parse_datetime, parse_eas_sync_mutations, parse_ews_attendees, parse_ews_recurrence,
    parse_ics_event, render_ics,
};

    #[test]
    fn parses_eas_add_mutation() {
        let xml = r#"<Sync xmlns="AirSync:" xmlns:Calendar="Calendar:" xmlns:AirSyncBase="AirSyncBase:"><Collections><Collection><Commands><Add><ClientId>abc</ClientId><ApplicationData><Calendar:Subject>Meeting</Calendar:Subject><Calendar:StartTime>2026-03-21T10:00:00Z</Calendar:StartTime><Calendar:EndTime>2026-03-21T11:00:00Z</Calendar:EndTime><Calendar:Categories><Calendar:Category>Blue</Calendar:Category></Calendar:Categories><Calendar:Exceptions><Calendar:Exception><Calendar:ExceptionStartTime>2026-03-22T10:00:00Z</Calendar:ExceptionStartTime><Calendar:Deleted>1</Calendar:Deleted></Calendar:Exception></Calendar:Exceptions></ApplicationData></Add></Commands></Collection></Collections></Sync>"#;
        let items = parse_eas_sync_mutations(xml).unwrap();
        assert_eq!(items.len(), 1);
        match &items[0] {
            super::EasSyncMutation::Add { item, .. } => {
                assert_eq!(item.categories, vec!["Blue".to_string()]);
                assert_eq!(item.exceptions.len(), 1);
                assert!(item.exceptions[0].deleted);
            }
            _ => panic!("expected add"),
        }
    }

    #[test]
    fn renders_and_parses_ics_with_exceptions() {
        let item = super::CalendarItem {
            uid: "uid-1".to_string(),
            subject: "Subject".to_string(),
            description: "Body".to_string(),
            location: "Room".to_string(),
            start: chrono::DateTime::parse_from_rfc3339("2026-03-21T10:00:00Z")
                .unwrap()
                .with_timezone(&chrono::Utc),
            end: chrono::DateTime::parse_from_rfc3339("2026-03-21T11:00:00Z")
                .unwrap()
                .with_timezone(&chrono::Utc),
            all_day: false,
            dtstamp: Some(
                chrono::DateTime::parse_from_rfc3339("2026-03-20T10:00:00Z")
                    .unwrap()
                    .with_timezone(&chrono::Utc),
            ),
            timezone: Some("AAA=".to_string()),
            timezone_blob: Some("BEGIN:VTIMEZONE\r\nTZID:AAA=\r\nEND:VTIMEZONE".to_string()),
            rrule: Some("FREQ=WEEKLY;BYDAY=MO,WE;COUNT=4".to_string()),
            exdates: vec![
                chrono::DateTime::parse_from_rfc3339("2026-03-28T10:00:00Z")
                    .unwrap()
                    .with_timezone(&chrono::Utc),
            ],
            organizer_name: Some("Organizer".to_string()),
            organizer_email: Some("organizer@example.com".to_string()),
            attendees: vec![super::Attendee {
                name: Some("Alice".to_string()),
                email: "alice@example.com".to_string(),
                attendee_type: Some(1),
                attendee_status: Some(3),
                partstat: Some("ACCEPTED".to_string()),
            }],
            categories: vec!["Blue".to_string(), "Green".to_string()],
            busy_status: Some(2),
            sensitivity: Some(2),
            reminder: Some(15),
            response_requested: Some(true),
            disallow_new_time_proposal: Some(true),
            appointment_reply_time: Some(
                chrono::DateTime::parse_from_rfc3339("2026-03-19T10:00:00Z")
                    .unwrap()
                    .with_timezone(&chrono::Utc),
            ),
            meeting_status: Some(3),
            response_type: Some(3),
            online_meeting_conf_link: Some("https://conf.example.test/1".to_string()),
            online_meeting_external_link: Some("https://join.example.test/1".to_string()),
            client_uid: Some("client-uid-1".to_string()),
            exceptions: vec![
                super::CalendarException {
                    deleted: true,
                    exception_start: chrono::DateTime::parse_from_rfc3339("2026-03-30T10:00:00Z")
                        .unwrap()
                        .with_timezone(&chrono::Utc),
                    ..Default::default()
                },
                super::CalendarException {
                    deleted: false,
                    exception_start: chrono::DateTime::parse_from_rfc3339("2026-04-01T10:00:00Z")
                        .unwrap()
                        .with_timezone(&chrono::Utc),
                    subject: Some("Shifted subject".to_string()),
                    start: Some(
                        chrono::DateTime::parse_from_rfc3339("2026-04-01T12:00:00Z")
                            .unwrap()
                            .with_timezone(&chrono::Utc),
                    ),
                    end: Some(
                        chrono::DateTime::parse_from_rfc3339("2026-04-01T13:00:00Z")
                            .unwrap()
                            .with_timezone(&chrono::Utc),
                    ),
                    appointment_reply_time: Some(
                        chrono::DateTime::parse_from_rfc3339("2026-03-25T08:00:00Z")
                            .unwrap()
                            .with_timezone(&chrono::Utc),
                    ),
                    meeting_status: Some(3),
                    response_type: Some(2),
                    ..Default::default()
                },
            ],
        };
        let ics = render_ics(&item);
        let parsed = parse_ics_event(&ics).unwrap();
        assert_eq!(parsed.uid, "uid-1");
        assert_eq!(parsed.subject, "Subject");
        assert_eq!(parsed.categories.len(), 2);
        assert_eq!(parsed.exceptions.len(), 2);
        assert!(parsed.exceptions.iter().any(|v| v.deleted));
        assert!(
            parsed
                .exceptions
                .iter()
                .any(|v| v.subject.as_deref() == Some("Shifted subject"))
        );
        assert!(parsed.exceptions.iter().any(|v| v.response_type == Some(2)));
    }

    #[test]
    fn prefers_count_over_until_when_building_rrule() {
        let recurrence = EasRecurrence {
            kind: Some(1),
            interval: Some(2),
            day_of_week: Some("10".to_string()),
            until: Some("2026-04-01T00:00:00Z".to_string()),
            occurrences: Some(5),
            ..Default::default()
        };
        assert_eq!(
            recurrence.to_rrule().as_deref(),
            Some("FREQ=WEEKLY;INTERVAL=2;BYDAY=MO,WE;COUNT=5")
        );
    }

    #[test]
    fn preserves_exception_reply_status_metadata_from_ics() {
        let ics = "BEGIN:VCALENDAR
VERSION:2.0
BEGIN:VEVENT
UID:test
DTSTAMP:20260101T000000Z
DTSTART:20260322T090000Z
DTEND:20260322T100000Z
SUMMARY:Series
RRULE:FREQ=WEEKLY;COUNT=2
END:VEVENT
BEGIN:VEVENT
UID:test
DTSTAMP:20260101T000000Z
RECURRENCE-ID:20260329T090000Z
DTSTART:20260329T100000Z
DTEND:20260329T110000Z
SUMMARY:Moved
X-MS-APPOINTMENT-REPLY-TIME:20260320T120000Z
X-MS-MEETING-STATUS:3
X-MS-RESPONSE-TYPE:2
END:VEVENT
END:VCALENDAR
";
        let parsed = parse_ics_event(ics).expect("parsed item");
        let exception = parsed
            .exceptions
            .iter()
            .find(|v| !v.deleted)
            .expect("exception");
        assert_eq!(
            exception.appointment_reply_time,
            parse_datetime("20260320T120000Z")
        );
        assert_eq!(exception.meeting_status, Some(3));
        assert_eq!(exception.response_type, Some(2));
    }

    #[test]
    fn parses_ews_recurrence_and_attendees() {
        let xml = r#"
<CalendarItem>
  <Recurrence>
    <WeeklyRecurrence>
      <Interval>2</Interval>
      <DaysOfWeek>Monday Wednesday</DaysOfWeek>
    </WeeklyRecurrence>
    <NumberedRecurrence>
      <NumberOfOccurrences>5</NumberOfOccurrences>
    </NumberedRecurrence>
  </Recurrence>
  <RequiredAttendees>
    <Attendee>
      <Mailbox>
        <Name>Alice</Name>
        <EmailAddress>alice@example.com</EmailAddress>
      </Mailbox>
      <ResponseType>Accept</ResponseType>
    </Attendee>
  </RequiredAttendees>
  <OptionalAttendees>
    <Attendee>
      <Mailbox>
        <Name>Bob</Name>
        <EmailAddress>bob@example.com</EmailAddress>
      </Mailbox>
      <ResponseType>Tentative</ResponseType>
    </Attendee>
  </OptionalAttendees>
</CalendarItem>
"#;
        assert_eq!(
            parse_ews_recurrence(xml).as_deref(),
            Some("FREQ=WEEKLY;BYDAY=MO,WE;INTERVAL=2;COUNT=5")
        );
        let attendees = parse_ews_attendees(xml);
        assert_eq!(attendees.len(), 2);
        assert_eq!(attendees[0].attendee_type, Some(1));
        assert_eq!(attendees[1].attendee_type, Some(2));
    }
    #[test]
    fn preserves_ianna_tzid_local_times() {
        let ics = "BEGIN:VCALENDAR
VERSION:2.0
BEGIN:VEVENT
UID:test
DTSTAMP:20260101T000000Z
DTSTART;TZID=Europe/Stockholm:20260329T090000
DTEND;TZID=Europe/Stockholm:20260329T100000
SUMMARY:TZ Test
END:VEVENT
END:VCALENDAR
";
        let item = parse_ics_event(ics).unwrap();
        assert_eq!(item.timezone.as_deref(), Some("Europe/Stockholm"));
        let rendered = render_ics(&item);
        assert!(rendered.contains("DTSTART;TZID=Europe/Stockholm:20260329T090000"));
        assert!(rendered.contains("DTEND;TZID=Europe/Stockholm:20260329T100000"));
    }
}
