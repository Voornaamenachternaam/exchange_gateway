use anyhow::{Result, anyhow};
use chrono::{NaiveDate, NaiveDateTime, TimeZone, Utc};
use quick_xml::Reader;
use quick_xml::events::Event;
use std::borrow::Cow;
use uuid::Uuid;

#[derive(Clone, Debug)]
pub struct CalendarItem {
    pub uid: String,
    pub subject: String,
    pub description: String,
    pub location: String,
    pub start: chrono::DateTime<Utc>,
    pub end: chrono::DateTime<Utc>,
    pub all_day: bool,
    pub rrule: Option<String>,
    pub organizer_name: Option<String>,
    pub organizer_email: Option<String>,
    pub attendees: Vec<Attendee>,
    pub response_requested: Option<bool>,
    pub meeting_status: Option<u8>,
    pub response_type: Option<u8>,
}

#[derive(Clone, Debug)]
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
    pub rrule: Option<String>,
    pub organizer_name: Option<String>,
    pub organizer_email: Option<String>,
    pub attendees: Option<Vec<Attendee>>,
    pub response_requested: Option<bool>,
    pub meeting_status: Option<u8>,
    pub response_type: Option<u8>,
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
    uid: Option<String>,
    recurrence: EasRecurrence,
    organizer_name: Option<String>,
    organizer_email: Option<String>,
    attendees: Vec<Attendee>,
    current_attendee: Option<Attendee>,
    response_requested: Option<bool>,
    meeting_status: Option<u8>,
    response_type: Option<u8>,
}

#[derive(Default)]
struct EasRecurrence {
    kind: Option<u8>,
    interval: Option<u32>,
    day_of_week: Option<String>,
    day_of_month: Option<u32>,
    week_of_month: Option<u32>,
    month_of_year: Option<u32>,
    until: Option<String>,
    occurrences: Option<u32>,
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
            rrule: self.recurrence.to_rrule(),
            organizer_name: self.organizer_name,
            organizer_email: self.organizer_email,
            attendees: self.attendees,
            response_requested: self.response_requested,
            meeting_status: self.meeting_status,
            response_type: self.response_type,
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
            rrule: self.recurrence.to_rrule(),
            organizer_name: self.organizer_name,
            organizer_email: self.organizer_email,
            attendees: if self.attendees.is_empty() {
                None
            } else {
                Some(self.attendees)
            },
            response_requested: self.response_requested,
            meeting_status: self.meeting_status,
            response_type: self.response_type,
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
                    parts.push(format!("BYDAY={}{}", week, byday[0]));
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
        if let Some(until) = &self.until
            && let Some(dt) = parse_datetime(until)
        {
            parts.push(format!("UNTIL={}", dt.format("%Y%m%dT%H%M%SZ")));
        }
        if let Some(count) = self.occurrences {
            parts.push(format!("COUNT={count}"));
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

pub fn parse_ics_event(ics: &str) -> Option<CalendarItem> {
    let props = parse_ics_content(ics);
    let mut subject = String::new();
    let mut description = String::new();
    let mut location = String::new();
    let mut uid = String::new();
    let mut start = None;
    let mut end = None;
    let mut all_day = false;
    let mut rrule = None;
    let mut organizer_name = None;
    let mut organizer_email = None;
    let mut attendees = Vec::new();
    let mut response_requested = None;
    let mut meeting_status = None;
    let mut response_type = None;

    for (key, value) in props {
        if key.starts_with("SUMMARY") {
            subject = value;
        } else if key.starts_with("DESCRIPTION") {
            description = value.replace("\\n", "\n");
        } else if key.starts_with("LOCATION") {
            location = value;
        } else if key.starts_with("UID") {
            uid = value;
        } else if key.starts_with("DTSTART") {
            start = parse_datetime(&value);
            if !value.contains('T') {
                all_day = true;
            }
        } else if key.starts_with("DTEND") {
            end = parse_datetime(&value);
        } else if key.starts_with("RRULE") {
            rrule = Some(value);
        } else if key.starts_with("ORGANIZER") {
            let (cn, email) = parse_ical_actor_line(&key, &value);
            organizer_name = cn;
            organizer_email = email;
        } else if key.starts_with("ATTENDEE") {
            let (name, email) = parse_ical_actor_line(&key, &value);
            let partstat = parse_ical_param(&key, "PARTSTAT");
            attendees.push(Attendee {
                name,
                email: email.unwrap_or_default(),
                attendee_type: parse_ical_param(&key, "ROLE").map(|role| match role.as_str() {
                    "REQ-PARTICIPANT" => 1,
                    "OPT-PARTICIPANT" => 2,
                    "NON-PARTICIPANT" => 3,
                    _ => 1,
                }),
                attendee_status: partstat.as_deref().map(partstat_to_status),
                partstat,
            });
        } else if key.starts_with("X-MS-RESPONSE-REQUESTED") {
            response_requested = Some(value == "TRUE");
        } else if key.starts_with("X-MS-MEETING-STATUS") {
            meeting_status = value.parse().ok();
        } else if key.starts_with("X-MS-RESPONSE-TYPE") {
            response_type = value.parse().ok();
        }
    }

    Some(CalendarItem {
        uid: if uid.is_empty() {
            Uuid::new_v4().to_string()
        } else {
            uid
        },
        subject,
        description,
        location,
        start: start?,
        end: end?,
        all_day,
        rrule,
        organizer_name,
        organizer_email,
        attendees,
        response_requested,
        meeting_status,
        response_type,
    })
}

pub fn render_ics(item: &CalendarItem) -> String {
    let dtstamp = Utc::now().format("%Y%m%dT%H%M%SZ").to_string();
    let uid = if item.uid.is_empty() {
        Uuid::new_v4().to_string()
    } else {
        item.uid.clone()
    };

    let (dtstart, dtend) = if item.all_day {
        (
            item.start.format("%Y%m%d").to_string(),
            item.end.format("%Y%m%d").to_string(),
        )
    } else {
        (
            item.start.format("%Y%m%dT%H%M%SZ").to_string(),
            item.end.format("%Y%m%dT%H%M%SZ").to_string(),
        )
    };

    let mut lines = vec![
        "BEGIN:VCALENDAR".to_string(),
        "VERSION:2.0".to_string(),
        "PRODID:-//exchange_gateway//EN".to_string(),
        "BEGIN:VEVENT".to_string(),
        format!("UID:{uid}"),
        format!("DTSTAMP:{dtstamp}"),
        format!("SUMMARY:{}", escape_ical_text(&item.subject)),
        format!("DTSTART:{dtstart}"),
        format!("DTEND:{dtend}"),
    ];
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
        lines.push(line);
    }
    if let Some(v) = item.response_requested {
        lines.push(format!(
            "X-MS-RESPONSE-REQUESTED:{}",
            if v { "TRUE" } else { "FALSE" }
        ));
    }
    if let Some(v) = item.meeting_status {
        lines.push(format!("X-MS-MEETING-STATUS:{v}"));
    }
    if let Some(v) = item.response_type {
        lines.push(format!("X-MS-RESPONSE-TYPE:{v}"));
    }
    lines.push("END:VEVENT".to_string());
    lines.push("END:VCALENDAR".to_string());
    format!("{}\r\n", lines.join("\r\n"))
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
    let name = parse_ical_param(key, "CN");
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
                        Some(b"Subject") => current.subject = Some(value),
                        Some(b"Location") => current.location = Some(value),
                        Some(b"StartTime") => current.start = parse_datetime(&value),
                        Some(b"EndTime") => current.end = parse_datetime(&value),
                        Some(b"AllDayEvent") => current.all_day = Some(value == "1"),
                        Some(b"UID") => current.uid = Some(value),
                        Some(b"OrganizerName") => current.organizer_name = Some(value),
                        Some(b"OrganizerEmail") => current.organizer_email = Some(value),
                        Some(b"ResponseRequested") => {
                            current.response_requested = Some(value == "1")
                        }
                        Some(b"MeetingStatus") => current.meeting_status = value.parse().ok(),
                        Some(b"ResponseType") => current.response_type = value.parse().ok(),
                        Some(b"Name") if stack.iter().any(|v| v.as_slice() == b"Attendee") => {
                            let attendee = current.current_attendee.get_or_insert(Attendee {
                                name: None,
                                email: String::new(),
                                attendee_type: None,
                                attendee_status: None,
                                partstat: None,
                            });
                            attendee.name = Some(value);
                        }
                        Some(b"Email") if stack.iter().any(|v| v.as_slice() == b"Attendee") => {
                            let attendee = current.current_attendee.get_or_insert(Attendee {
                                name: None,
                                email: String::new(),
                                attendee_type: None,
                                attendee_status: None,
                                partstat: None,
                            });
                            attendee.email = value;
                        }
                        Some(b"AttendeeType")
                            if stack.iter().any(|v| v.as_slice() == b"Attendee") =>
                        {
                            let attendee = current.current_attendee.get_or_insert(Attendee {
                                name: None,
                                email: String::new(),
                                attendee_type: None,
                                attendee_status: None,
                                partstat: None,
                            });
                            attendee.attendee_type = value.parse().ok();
                        }
                        Some(b"AttendeeStatus")
                            if stack.iter().any(|v| v.as_slice() == b"Attendee") =>
                        {
                            let attendee = current.current_attendee.get_or_insert(Attendee {
                                name: None,
                                email: String::new(),
                                attendee_type: None,
                                attendee_status: None,
                                partstat: None,
                            });
                            let status = value.parse().ok();
                            attendee.attendee_status = status;
                            attendee.partstat = status.map(status_to_partstat);
                        }
                        Some(b"Data") if stack.iter().any(|v| v.as_slice() == b"Body") => {
                            current.description = Some(value)
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
                    current.attendees.push(attendee);
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
    let uid = extract_ews_field(xml, b"UID").unwrap_or_else(|| Uuid::new_v4().to_string());
    let description = extract_ews_field(xml, b"Body")
        .or_else(|| extract_ews_field(xml, b"TextBody"))
        .unwrap_or_default();
    let location = extract_ews_field(xml, b"Location").unwrap_or_default();
    let all_day = extract_ews_field(xml, b"IsAllDayEvent")
        .map(|v| v.eq_ignore_ascii_case("true"))
        .unwrap_or(false);
    let organizer_name = extract_ews_field(xml, b"OrganizerName");
    let organizer_email = extract_ews_field(xml, b"OrganizerEmail");
    Ok(CalendarItem {
        uid,
        subject,
        description,
        location,
        start,
        end,
        all_day,
        rrule: None,
        organizer_name,
        organizer_email,
        attendees: Vec::new(),
        response_requested: None,
        meeting_status: None,
        response_type: None,
    })
}

#[cfg(test)]
mod tests {
    use super::{parse_eas_sync_mutations, parse_ics_event, render_ics};

    #[test]
    fn parses_eas_add_mutation() {
        let xml = r#"<Sync xmlns="AirSync:" xmlns:Calendar="Calendar:"><Collections><Collection><Commands><Add><ClientId>abc</ClientId><ApplicationData><Calendar:Subject>Meeting</Calendar:Subject><Calendar:StartTime>2026-03-21T10:00:00Z</Calendar:StartTime><Calendar:EndTime>2026-03-21T11:00:00Z</Calendar:EndTime></ApplicationData></Add></Commands></Collection></Collections></Sync>"#;
        let items = parse_eas_sync_mutations(xml).unwrap();
        assert_eq!(items.len(), 1);
    }

    #[test]
    fn renders_and_parses_ics() {
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
            rrule: Some("FREQ=DAILY".to_string()),
            organizer_name: Some("Organizer".to_string()),
            organizer_email: Some("organizer@example.com".to_string()),
            attendees: vec![super::Attendee {
                name: Some("Alice".to_string()),
                email: "alice@example.com".to_string(),
                attendee_type: Some(1),
                attendee_status: Some(3),
                partstat: Some("ACCEPTED".to_string()),
            }],
            response_requested: Some(true),
            meeting_status: Some(3),
            response_type: Some(3),
        };
        let ics = render_ics(&item);
        let parsed = parse_ics_event(&ics).unwrap();
        assert_eq!(parsed.uid, "uid-1");
        assert_eq!(parsed.subject, "Subject");
    }
}
