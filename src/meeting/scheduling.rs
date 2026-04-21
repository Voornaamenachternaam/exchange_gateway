// src/meeting/scheduling.rs

use crate::calendar::CalendarItem;
use crate::ical_parser;
use crate::util::normalize_email;
use chrono::{DateTime, Utc};

pub struct SchedulingContext {
    pub organizer_email: String,
    pub organizer_name: Option<String>,
    pub attendees: Vec<AttendeeInfo>,
    pub sequence: u32,
    pub uid: String,
}

pub struct AttendeeInfo {
    pub email: String,
    pub name: Option<String>,
    pub role: AttendeeRole,
    pub status: AttendeeStatus,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AttendeeRole {
    Chair = 0,
    Required = 1,
    Optional = 2,
    NonParticipant = 3,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AttendeeStatus {
    NeedsAction = 0,
    Accepted = 1,
    Declined = 2,
    Tentative = 3,
    Delegated = 4,
    Completed = 5,
    InProcess = 6,
}

pub fn build_itip_request(ctx: &SchedulingContext, item: &CalendarItem) -> String {
    let mut lines: Vec<String> = Vec::new();
    lines.push("BEGIN:VCALENDAR".to_string());
    lines.push("VERSION:2.0".to_string());
    lines.push("PRODID:-//Exchange Gateway//EN".to_string());
    lines.push("METHOD:REQUEST".to_string());
    lines.push(format!("SEQUENCE:{}", ctx.sequence));
    lines.push(build_vevent(ctx, item));
    lines.push("END:VCALENDAR".to_string());
    lines.join("\r\n")
}

fn build_vevent(ctx: &SchedulingContext, item: &CalendarItem) -> String {
    let mut lines: Vec<String> = Vec::new();
    lines.push("BEGIN:VEVENT".to_string());
    lines.push(format!("UID:{}", ctx.uid));
    lines.push(format!("DTSTAMP:{}", format_ical_datetime(Utc::now())));
    if !item.subject.is_empty() {
        lines.push(format!("SUMMARY:{}", escape_ical_text(&item.subject)));
    }
    if !item.location.is_empty() {
        lines.push(format!("LOCATION:{}", escape_ical_text(&item.location)));
    }
    lines.push(format!("DTSTART:{}", format_ical_datetime(item.start)));
    lines.push(format!("DTEND:{}", format_ical_datetime(item.end)));
    lines.push(format!("ORGANIZER;CN={}:mailto:{}", 
        escape_ical_param(ctx.organizer_name.as_deref().unwrap_or("")),
        ctx.organizer_email
    ));
    for attendee in &ctx.attendees {
        let role = match attendee.role {
            AttendeeRole::Chair => "CHAIR",
            AttendeeRole::Required => "REQ-PARTICIPANT",
            AttendeeRole::Optional => "OPT-PARTICIPANT",
            AttendeeRole::NonParticipant => "NON-PARTICIPANT",
        };
        let status = match attendee.status {
            AttendeeStatus::NeedsAction => "NEEDS-ACTION",
            AttendeeStatus::Accepted => "ACCEPTED",
            AttendeeStatus::Declined => "DECLINED",
            AttendeeStatus::Tentative => "TENTATIVE",
            AttendeeStatus::Delegated => "DELEGATED",
            AttendeeStatus::Completed => "COMPLETED",
            AttendeeStatus::InProcess => "IN-PROCESS",
        };
        lines.push(format!(
            "ATTENDEE;CN={};ROLE={};PARTSTAT={}:mailto:{}",
            attendee.name.as_deref().unwrap_or(""),
            role,
            status,
            attendee.email
        ));
    }
    lines.push("END:VEVENT".to_string());
    lines.join("\r\n")
}

pub fn format_ical_datetime(dt: DateTime<Utc>) -> String {
    dt.format("%Y%m%dT%H%M%SZ").to_string()
}

pub fn escape_ical_text(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace(';', "\\;")
        .replace(',', "\\,")
        .replace('\n', "\\n")
}

/// Escape and quote a parameter value for iCalendar property parameters.
/// Per RFC 5545, parameter values containing special characters must be quoted.
pub fn escape_ical_param(s: &str) -> String {
    let escaped = s.replace('\\', "\\\\")
        .replace(';', "\\;")
        .replace(':', "\\:")
        .replace(',', "\\,")
        .replace('"', "\\'");
    // Quote if contains special chars or spaces
    if escaped.chars().any(|c| c == ';' || c == ':' || c == ',' || c == ' ' || c == '"' || c == '\\' || c == '\n') {
        format!("\"{}\"", escaped)
    } else {
        escaped
    }
}

pub fn parse_itip_response(ical: &str) -> Option<ItipResponse> {
    let parsed = ical_parser::parse_all_vevents(ical).ok()?;
    let event_props = parsed.first()?;

    let mut uid: Option<String> = None;
    let mut sequence: u32 = 0;
    let mut organizer_email: Option<String> = None;
    let mut responding_attendee_email: Option<String> = None;
    let mut responding_partstat: Option<String> = None;

    // First pass: extract UID, SEQUENCE, and ORGANIZER
    for (key, value) in event_props {
        if key.starts_with("UID") {
            uid = Some(value.trim().to_string());
        } else if key.starts_with("SEQUENCE") {
            sequence = value.trim().parse::<u32>().unwrap_or(0);
        } else if key.starts_with("ORGANIZER") {
            // Extract email from mailto: format, NFC-normalize for storage
            let email = normalize_email(value);
            organizer_email = Some(email);
        }
    }

    // UID is required
    let uid = uid?;

    // Second pass: find the first ATTENDEE that is not the organizer
    for (key, value) in event_props {
        if key.starts_with("ATTENDEE") {
            // Extract email from mailto: format, NFC-normalize for storage
            let email = normalize_email(value);

            // Skip if this attendee is the organizer
            if let Some(ref org_email) = organizer_email {
                if email == *org_email {
                    continue;
                }
            }

            // Extract partstat from the key
            let partstat = key.split(';')
                .find_map(|p| {
                    let (k, v) = p.split_once('=')?;
                    if k.eq_ignore_ascii_case("PARTSTAT") {
                        Some(v.trim_matches(|c: char| c == '"' || c.is_whitespace()).to_uppercase())
                    } else {
                        None
                    }
                })
                .unwrap_or_else(|| "NEEDS-ACTION".to_string());

            responding_attendee_email = Some(email);
            responding_partstat = Some(partstat);
            break; // Take the first non-organizer attendee
        }
    }

    // Attendee email is required
    let attendee_email = responding_attendee_email?;

    let attendee_status = match responding_partstat.as_deref() {
        Some("ACCEPTED") => AttendeeStatus::Accepted,
        Some("DECLINED") => AttendeeStatus::Declined,
        Some("TENTATIVE") => AttendeeStatus::Tentative,
        Some("DELEGATED") => AttendeeStatus::Delegated,
        _ => AttendeeStatus::NeedsAction,
    };

    Some(ItipResponse {
        uid,
        sequence,
        attendee_email,
        attendee_status,
    })
}
pub struct ItipResponse {
    pub uid: String,
    pub sequence: u32,
    pub attendee_email: String,
    pub attendee_status: AttendeeStatus,
}

pub fn build_cancel_request(ctx: &SchedulingContext, item: &CalendarItem) -> String {
    let mut lines: Vec<String> = Vec::new();
    lines.push("BEGIN:VCALENDAR".to_string());
    lines.push("VERSION:2.0".to_string());
    lines.push("PRODID:-//Exchange Gateway//EN".to_string());
    lines.push("METHOD:CANCEL".to_string());
    lines.push(format!("SEQUENCE:{}", ctx.sequence));
    lines.push(build_cancel_vevent(ctx, item));
    lines.push("END:VCALENDAR".to_string());
    lines.join("\r\n")
}

fn build_cancel_vevent(ctx: &SchedulingContext, item: &CalendarItem) -> String {
    let mut lines: Vec<String> = Vec::new();
    lines.push("BEGIN:VEVENT".to_string());
    lines.push(format!("UID:{}", ctx.uid));
    lines.push(format!("DTSTAMP:{}", format_ical_datetime(Utc::now())));
    lines.push(format!("STATUS:CANCELLED"));
    if !item.subject.is_empty() {
        lines.push(format!("SUMMARY:{}", escape_ical_text(&item.subject)));
    }
    lines.push("END:VEVENT".to_string());
    lines.join("\r\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_ical_datetime() {
        let dt = DateTime::parse_from_rfc3339("2024-01-15T10:30:00Z").unwrap().with_timezone(&Utc);
        assert_eq!(format_ical_datetime(dt), "20240115T103000Z");
    }

    #[test]
    fn test_escape_ical_text() {
        assert_eq!(escape_ical_text("hello;world"), "hello\\;world");
        assert_eq!(escape_ical_text("a,b\\c"), "a\\,b\\\\c");
    }
}
