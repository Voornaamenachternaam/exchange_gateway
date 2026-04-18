// src/meeting/scheduling.rs

use crate::calendar::CalendarItem;
use crate::ical_parser;
use chrono::{DateTime, Utc};
use std::collections::HashMap;

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
 if let Some(ref subject) = item.subject {
  lines.push(format!("SUMMARY:{}", escape_ical_text(subject)));
 }
 if let Some(ref location) = item.location {
  lines.push(format!("LOCATION:{}", escape_ical_text(location)));
 }
 if let Some(ref start) = item.start {
  lines.push(format!("DTSTART:{}", format_ical_datetime(*start)));
 }
 if let Some(ref end) = item.end {
  lines.push(format!("DTEND:{}", format_ical_datetime(*end)));
 }
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
 let parsed = ical_parser::parse_ical(ical)?;
 let event = parsed.events.first()?;
 let mut response = ItipResponse {
  uid: event.uid.clone(),
  sequence: event.sequence.unwrap_or(0),
  attendee_email: String::new(),
  attendee_status: AttendeeStatus::NeedsAction,
 };
 for attendee in &event.attendees {
  if attendee.email != event.organizer_email {
   response.attendee_email = attendee.email.clone();
   response.attendee_status = match attendee.partstat.as_deref() {
    Some("ACCEPTED") => AttendeeStatus::Accepted,
    Some("DECLINED") => AttendeeStatus::Declined,
    Some("TENTATIVE") => AttendeeStatus::Tentative,
    Some("DELEGATED") => AttendeeStatus::Delegated,
    _ => AttendeeStatus::NeedsAction,
   };
   break;
  }
 }
 Some(response)
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
 if let Some(ref subject) = item.subject {
  lines.push(format!("SUMMARY:{}", escape_ical_text(subject)));
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
