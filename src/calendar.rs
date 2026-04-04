use anyhow::Result;
use chrono::{DateTime, Utc};

#[derive(Debug, Clone, Default)]
pub struct CalendarItem {
    pub uid: String,
    pub subject: String,
    pub description: String,
    pub location: String,
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
    pub all_day: bool,
    pub dtstamp: Option<DateTime<Utc>>,
    pub timezone: Option<String>,
    pub timezone_blob: Option<String>,
    pub rrule: Option<String>,
    pub exdates: Vec<DateTime<Utc>>,
    pub organizer_name: Option<String>,
    pub organizer_email: Option<String>,
    pub attendees: Vec<Attendee>,
    pub categories: Vec<String>,
    pub busy_status: Option<u8>,
    pub sensitivity: Option<u8>,
    pub reminder: Option<i32>,
    pub response_requested: Option<bool>,
    pub disallow_new_time_proposal: Option<bool>,
    pub appointment_reply_time: Option<DateTime<Utc>>,
    pub meeting_status: Option<u8>,
    pub response_type: Option<u8>,
    pub online_meeting_conf_link: Option<String>,
    pub online_meeting_external_link: Option<String>,
    pub client_uid: Option<String>,
    pub exceptions: Vec<CalendarException>,
}

#[derive(Debug, Clone, Default)]
pub struct Attendee {
    pub name: Option<String>,
    pub email: String,
    pub attendee_type: Option<u8>,
    pub attendee_status: Option<u8>,
    pub partstat: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct CalendarException {
    pub exception_start: DateTime<Utc>,
    pub deleted: bool,
    pub subject: Option<String>,
    pub start: Option<DateTime<Utc>>,
    pub end: Option<DateTime<Utc>>,
    pub all_day: Option<bool>,
    pub location: Option<String>,
    pub busy_status: Option<u8>,
    pub sensitivity: Option<u8>,
    pub reminder: Option<i32>,
    pub appointment_reply_time: Option<DateTime<Utc>>,
    pub meeting_status: Option<u8>,
    pub response_type: Option<u8>,
    pub categories: Option<Vec<String>>,
    pub attendees: Option<Vec<Attendee>>,
    pub description: Option<String>,
}

#[derive(Debug, Clone)]
pub enum EasSyncMutation {
    Add {
        client_id: Option<String>,
        item: CalendarItem,
    },
    Change {
        server_id: String,
        patch: CalendarItemPatch,
    },
    Delete {
        server_id: String,
    },
}

#[derive(Debug, Clone, Default)]
pub struct CalendarItemPatch {
    pub uid: Option<String>,
    pub subject: Option<String>,
    pub description: Option<String>,
    pub location: Option<String>,
    pub start: Option<DateTime<Utc>>,
    pub end: Option<DateTime<Utc>>,
    pub all_day: Option<bool>,
    pub dtstamp: Option<DateTime<Utc>>,
    pub timezone: Option<String>,
    pub timezone_blob: Option<String>,
    pub rrule: Option<String>,
    pub exdates: Vec<DateTime<Utc>>,
    pub organizer_name: Option<String>,
    pub organizer_email: Option<String>,
    pub attendees: Vec<Attendee>,
    pub categories: Vec<String>,
    pub busy_status: Option<u8>,
    pub sensitivity: Option<u8>,
    pub reminder: Option<i32>,
    pub response_requested: Option<bool>,
    pub disallow_new_time_proposal: Option<bool>,
    pub appointment_reply_time: Option<DateTime<Utc>>,
    pub meeting_status: Option<u8>,
    pub response_type: Option<u8>,
    pub online_meeting_conf_link: Option<String>,
    pub online_meeting_external_link: Option<String>,
    pub client_uid: Option<String>,
    pub exceptions: Vec<CalendarException>,
}

pub fn parse_ics_event(ics: &str) -> Option<CalendarItem> {
    let mut item = CalendarItem::default();
    let mut in_event = false;
    let mut current_attendee: Option<Attendee> = None;

    for line in ics.lines() {
        let line = line.trim();

        if line.starts_with("BEGIN:VEVENT") {
            in_event = true;
        } else if line.starts_with("END:VEVENT") {
            if let Some(attendee) = current_attendee.take() {
                item.attendees.push(attendee);
            }
            in_event = false;
        } else if in_event {
            if let Some((key, value)) = line.split_once(':') {
                let key = key.split(';').next().unwrap_or(key);
                match key {
                    "UID" => item.uid = value.to_string(),
                    "SUMMARY" => item.subject = value.to_string(),
                    "DESCRIPTION" => item.description = value.to_string(),
                    "LOCATION" => item.location = value.to_string(),
                    "DTSTART" => {
                        if let Some(dt) = parse_ics_datetime(value) {
                            item.start = dt;
                            item.all_day = !value.contains('T');
                        }
                    }
                    "DTEND" => {
                        if let Some(dt) = parse_ics_datetime(value) {
                            item.end = dt;
                        }
                    }
                    "DTSTAMP" => {
                        item.dtstamp = parse_ics_datetime(value);
                    }
                    "RRULE" => item.rrule = Some(value.to_string()),
                    "EXDATE" => {
                        for ex in value.split(',') {
                            if let Some(dt) = parse_ics_datetime(ex) {
                                item.exdates.push(dt);
                            }
                        }
                    }
                    "CATEGORIES" => {
                        item.categories = value.split(',').map(|s| s.to_string()).collect();
                    }
                    "BEGIN" => {
                        if value == "VALARM" {
                            // Handle alarm
                        }
                    }
                    "END" => {
                        if value == "VALARM" {
                            // End alarm
                        }
                    }
                    _ => {}
                }
            } else if line.starts_with("ATTENDEE") {
                if let Some(attendee) = current_attendee.take() {
                    item.attendees.push(attendee);
                }
                current_attendee = Some(parse_attendee(line));
            } else if line.starts_with("ORGANIZER") {
                let (name, email) = parse_organizer(line);
                item.organizer_name = name;
                item.organizer_email = email;
            }
        }
    }

    if item.uid.is_empty() {
        item.uid = uuid::Uuid::new_v4().to_string();
    }

    if item.end <= item.start {
        item.end = item.start + chrono::Duration::hours(1);
    }

    Some(item)
}

fn parse_ics_datetime(value: &str) -> Option<DateTime<Utc>> {
    if value.ends_with('Z') {
        chrono::NaiveDateTime::parse_from_str(value, "%Y%m%dT%H%M%SZ")
            .ok()
            .map(|dt| DateTime::from_naive_utc_and_offset(dt, Utc))
    } else if value.contains('T') {
        chrono::NaiveDateTime::parse_from_str(value, "%Y%m%dT%H%M%S")
            .ok()
            .map(|dt| DateTime::from_naive_utc_and_offset(dt, Utc))
    } else {
        chrono::NaiveDate::parse_from_str(value, "%Y%m%d")
            .ok()
            .and_then(|d| d.and_hms_opt(0, 0, 0))
            .map(|dt| DateTime::from_naive_utc_and_offset(dt, Utc))
    }
}

fn parse_attendee(line: &str) -> Attendee {
    let mut attendee = Attendee::default();

    if let Some(email_start) = line.find("mailto:") {
        attendee.email = line[email_start + 7..].to_string();
    }

    if let Some(cn_start) = line.find("CN=") {
        let cn_end = line[cn_start + 3..].find(';').unwrap_or(line.len() - cn_start - 3);
        attendee.name = Some(line[cn_start + 3..cn_start + 3 + cn_end].to_string());
    }

    if line.contains("PARTSTAT=ACCEPTED") {
        attendee.attendee_status = Some(3);
        attendee.partstat = Some("ACCEPTED".to_string());
    } else if line.contains("PARTSTAT=DECLINED") {
        attendee.attendee_status = Some(4);
        attendee.partstat = Some("DECLINED".to_string());
    } else if line.contains("PARTSTAT=TENTATIVE") {
        attendee.attendee_status = Some(2);
        attendee.partstat = Some("TENTATIVE".to_string());
    } else {
        attendee.attendee_status = Some(5);
        attendee.partstat = Some("NEEDS-ACTION".to_string());
    }

    if line.contains("ROLE=REQ-PARTICIPANT") {
        attendee.attendee_type = Some(1);
    } else if line.contains("ROLE=OPT-PARTICIPANT") {
        attendee.attendee_type = Some(2);
    } else if line.contains("ROLE=NON-PARTICIPANT") {
        attendee.attendee_type = Some(3);
    }

    attendee
}

fn parse_organizer(line: &str) -> (Option<String>, Option<String>) {
    let mut name = None;
    let mut email = None;

    if let Some(email_start) = line.find("mailto:") {
        email = Some(line[email_start + 7..].to_string());
    }

    if let Some(cn_start) = line.find("CN=") {
        let cn_end = line[cn_start + 3..].find(';').unwrap_or(line.len() - cn_start - 3);
        name = Some(line[cn_start + 3..cn_start + 3 + cn_end].to_string());
    }

    (name, email)
}

pub fn render_ics(item: &CalendarItem) -> String {
    let mut ics = String::new();

    ics.push_str("BEGIN:VCALENDAR\r\n");
    ics.push_str("VERSION:2.0\r\n");
    ics.push_str("PRODID:-//Exchange Gateway//EN\r\n");
    ics.push_str("BEGIN:VEVENT\r\n");

    ics.push_str(&format!("UID:{}\r\n", item.uid));
    ics.push_str(&format!("SUMMARY:{}\r\n", ics_escape(&item.subject)));

    if !item.description.is_empty() {
        ics.push_str(&format!("DESCRIPTION:{}\r\n", ics_escape(&item.description)));
    }

    if !item.location.is_empty() {
        ics.push_str(&format!("LOCATION:{}\r\n", ics_escape(&item.location)));
    }

    if item.all_day {
        ics.push_str(&format!("DTSTART;VALUE=DATE:{}\r\n", item.start.format("%Y%m%d")));
        ics.push_str(&format!("DTEND;VALUE=DATE:{}\r\n", item.end.format("%Y%m%d")));
    } else {
        ics.push_str(&format!("DTSTART:{}\r\n", item.start.format("%Y%m%dT%H%M%SZ")));
        ics.push_str(&format!("DTEND:{}\r\n", item.end.format("%Y%m%dT%H%M%SZ")));
    }

    if let Some(dtstamp) = item.dtstamp {
        ics.push_str(&format!("DTSTAMP:{}\r\n", dtstamp.format("%Y%m%dT%H%M%SZ")));
    } else {
        ics.push_str(&format!("DTSTAMP:{}\r\n", Utc::now().format("%Y%m%dT%H%M%SZ")));
    }

    if let Some(ref rrule) = item.rrule {
        ics.push_str(&format!("RRULE:{}\r\n", rrule));
    }

    for exdate in &item.exdates {
        ics.push_str(&format!("EXDATE:{}\r\n", exdate.format("%Y%m%dT%H%M%SZ")));
    }

    if let Some(ref org_email) = item.organizer_email {
        let org_name = item.organizer_name.as_deref().unwrap_or(org_email);
        ics.push_str(&format!(
            "ORGANIZER;CN={}:mailto:{}\r\n",
            ics_escape(org_name),
            org_email
        ));
    }

    for attendee in &item.attendees {
        let name = attendee.name.as_deref().unwrap_or(&attendee.email);
        let partstat = attendee.partstat.as_deref().unwrap_or("NEEDS-ACTION");
        ics.push_str(&format!(
            "ATTENDEE;CN={};PARTSTAT={}:mailto:{}\r\n",
            ics_escape(name),
            partstat,
            attendee.email
        ));
    }

    if !item.categories.is_empty() {
        ics.push_str(&format!("CATEGORIES:{}\r\n", item.categories.join(",")));
    }

    ics.push_str("END:VEVENT\r\n");
    ics.push_str("END:VCALENDAR\r\n");

    ics
}

fn ics_escape(s: &str) -> String {
    s.replace("\\", "\\\\")
        .replace(";", "\\;")
        .replace(",", "\\,")
        .replace("\n", "\\n")
        .replace("\r", "")
}

pub fn parse_eas_sync_mutations(xml: &str) -> Result<Vec<EasSyncMutation>> {
    let mut mutations = Vec::new();

    if let Some(commands_start) = xml.find("<Commands>") {
        if let Some(commands_end) = xml[commands_start..].find("</Commands>") {
            let commands = &xml[commands_start..commands_start + commands_end + 11];

            for add in extract_elements(commands, "Add") {
                let client_id = extract_tag(&add, "ClientId");
                let app_data = extract_element(&add, "ApplicationData");
                if let Some(item) = parse_eas_app_data(&app_data) {
                    mutations.push(EasSyncMutation::Add { client_id, item });
                }
            }

            for change in extract_elements(commands, "Change") {
                if let Some(server_id) = extract_tag(&change, "ServerId") {
                    let app_data = extract_element(&change, "ApplicationData");
                    let patch = parse_eas_patch(&app_data);
                    mutations.push(EasSyncMutation::Change { server_id, patch });
                }
            }

            for delete in extract_elements(commands, "Delete") {
                if let Some(server_id) = extract_tag(&delete, "ServerId") {
                    mutations.push(EasSyncMutation::Delete { server_id });
                }
            }
        }
    }

    Ok(mutations)
}

fn parse_eas_app_data(xml: &str) -> Option<CalendarItem> {
    let mut item = CalendarItem::default();

    item.subject = extract_tag(xml, "Subject").unwrap_or_default();
    item.location = extract_tag(xml, "Location").unwrap_or_default();

    if let Some(start) = extract_tag(xml, "StartTime") {
        item.start = parse_eas_datetime(&start).unwrap_or_else(|| Utc::now());
    }

    if let Some(end) = extract_tag(xml, "EndTime") {
        item.end = parse_eas_datetime(&end).unwrap_or_else(|| Utc::now() + chrono::Duration::hours(1));
    }

    item.all_day = extract_tag(xml, "AllDayEvent")
        .map(|v| v == "1" || v.to_lowercase() == "true")
        .unwrap_or(false);

    if let Some(desc) = extract_tag(xml, "Data") {
        item.description = desc;
    }

    item.uid = extract_tag(xml, "UID").unwrap_or_else(|| uuid::Uuid::new_v4().to_string());

    if let Some(busy) = extract_tag(xml, "BusyStatus") {
        item.busy_status = busy.parse().ok();
    }

    if let Some(sens) = extract_tag(xml, "Sensitivity") {
        item.sensitivity = sens.parse().ok();
    }

    if let Some(rem) = extract_tag(xml, "Reminder") {
        item.reminder = rem.parse().ok();
    }

    Some(item)
}

fn parse_eas_patch(xml: &str) -> CalendarItemPatch {
    let mut patch = CalendarItemPatch::default();

    if let Some(v) = extract_tag(xml, "Subject") {
        patch.subject = Some(v);
    }
    if let Some(v) = extract_tag(xml, "Location") {
        patch.location = Some(v);
    }
    if let Some(v) = extract_tag(xml, "StartTime") {
        patch.start = parse_eas_datetime(&v);
    }
    if let Some(v) = extract_tag(xml, "EndTime") {
        patch.end = parse_eas_datetime(&v);
    }
    if let Some(v) = extract_tag(xml, "AllDayEvent") {
        patch.all_day = Some(v == "1" || v.to_lowercase() == "true");
    }
    if let Some(v) = extract_tag(xml, "Data") {
        patch.description = Some(v);
    }
    if let Some(v) = extract_tag(xml, "BusyStatus") {
        patch.busy_status = v.parse().ok();
    }
    if let Some(v) = extract_tag(xml, "Sensitivity") {
        patch.sensitivity = v.parse().ok();
    }
    if let Some(v) = extract_tag(xml, "Reminder") {
        patch.reminder = v.parse().ok();
    }

    patch
}

fn parse_eas_datetime(value: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|dt| dt.with_timezone(&Utc))
        .or_else(|| {
            chrono::NaiveDateTime::parse_from_str(value, "%Y-%m-%dT%H:%M:%SZ")
                .ok()
                .map(|dt| DateTime::from_naive_utc_and_offset(dt, Utc))
        })
}

fn extract_tag(xml: &str, tag: &str) -> Option<String> {
    let open = format!("<{}>", tag);
    let close = format!("</{}>", tag);

    let start = xml.find(&open)? + open.len();
    let end = xml.find(&close)?;

    Some(xml[start..end].to_string())
}

fn extract_element(xml: &str, tag: &str) -> String {
    let open = format!("<{}>", tag);
    let close = format!("</{}>", tag);

    if let Some(start) = xml.find(&open) {
        if let Some(end) = xml[start..].find(&close) {
            return xml[start..start + end + close.len()].to_string();
        }
    }
    String::new()
}

fn extract_elements(xml: &str, tag: &str) -> Vec<String> {
    let mut results = Vec::new();
    let open = format!("<{}>", tag);
    let close = format!("</{}>", tag);

    let mut pos = 0;
    while let Some(start) = xml[pos..].find(&open) {
        let start = pos + start;
        if let Some(end) = xml[start..].find(&close) {
            let end = start + end + close.len();
            results.push(xml[start..end].to_string());
            pos = end;
        } else {
            break;
        }
    }

    results
}
