// src/models.rs
// Data models for Exchange Gateway
//
// Features:
// - EAS calendar event model
// - EAS attendee model
// - EAS recurrence model
// - EAS exception model
// - Conversion helpers for CalDAV <-> EAS
//
// March 2026 - Production-ready, security-hardened

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// EAS Calendar Event model
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct EasCalendarEvent {
    pub server_id: Option<String>,
    pub uid: Option<String>,
    pub subject: Option<String>,
    pub start_time: Option<String>,
    pub end_time: Option<String>,
    pub dt_stamp: Option<String>,
    pub organizer_name: Option<String>,
    pub organizer_email: Option<String>,
    pub location: Option<String>,
    pub body: Option<String>,
    pub body_type: u8,           // 1 = Plain text, 2 = HTML
    pub sensitivity: Option<u8>, // 0 = Normal, 1 = Personal, 2 = Private, 3 = Confidential
    pub busy_status: Option<u8>, // 0 = Free, 1 = Tentative, 2 = Busy, 3 = OOF, 4 = Working elsewhere
    pub all_day_event: bool,
    pub reminder: Option<u32>, // Minutes before event
    pub attendees: Vec<EasAttendee>,
    pub recurrence: Option<EasRecurrence>,
    pub exceptions: Vec<EasException>,
    pub categories: Vec<String>,
    pub importance: Option<u8>, // 0 = Low, 1 = Normal, 2 = High
    pub is_recurring: bool,
    pub meeting_status: Option<u8>, // 0 = Not a meeting, 1 = Meeting, 3 = Received, 5 = Cancelled
}

/// EAS Attendee model
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct EasAttendee {
    pub email: String,
    pub name: Option<String>,
    pub attendee_type: u8,           // 1 = Required, 2 = Optional, 3 = Resource
    pub attendee_status: Option<u8>, // 0 = Unknown, 2 = Tentative, 3 = Accept, 4 = Decline, 5 = Not responded
}

/// EAS Recurrence model
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct EasRecurrence {
    pub recurrence_type: u8,
    // 0 = Recurs daily
    // 1 = Recurs weekly
    // 2 = Recurs monthly
    // 3 = Recurs monthly on the nth day
    // 5 = Recurs yearly
    // 6 = Recurs yearly on the nth day
    pub interval: Option<u16>,
    pub until: Option<String>,
    pub occurrences: Option<u32>,
    pub day_of_week: Option<u8>, // Bitmask: 1=Sun, 2=Mon, 4=Tue, 8=Wed, 16=Thu, 32=Fri, 64=Sat
    pub day_of_month: Option<u8>,
    pub week_of_month: Option<u8>, // 1-5 (5 = last week)
    pub month_of_year: Option<u8>, // 1-12
}

/// EAS Exception model for recurring events
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct EasException {
    pub exception_start_time: String,
    pub subject: Option<String>,
    pub start_time: Option<String>,
    pub end_time: Option<String>,
    pub location: Option<String>,
    pub body: Option<String>,
    pub deleted: bool,
    pub is_exception: bool,
}

/// Parse EAS calendar request from XML
pub fn parse_eas_calendar_request(xml: &str) -> Result<EasCalendarEvent, String> {
    let mut event = EasCalendarEvent::default();
    let mut reader = quick_xml::Reader::from_str(xml);
    reader.config_mut().trim_text(true);
    let mut buf = Vec::new();
    let mut current_element: Option<String> = None;
    let mut in_attendees = false;
    let mut in_recurrence = false;
    let mut in_exceptions = false;
    let mut current_attendee: Option<EasAttendee> = None;
    let mut current_exception: Option<EasException> = None;

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(quick_xml::events::Event::Start(e)) => {
                let name = String::from_utf8_lossy(e.name().local_name().as_ref()).to_string();
                current_element = Some(name.clone());

                match name.as_str() {
                    "Attendees" => in_attendees = true,
                    "Attendee" if in_attendees => {
                        current_attendee = Some(EasAttendee::default());
                    }
                    "Recurrence" => {
                        in_recurrence = true;
                        event.recurrence = Some(EasRecurrence::default());
                    }
                    "Exceptions" => in_exceptions = true,
                    "Exception" if in_exceptions => {
                        current_exception = Some(EasException::default());
                    }
                    _ => {}
                }
            }
            Ok(quick_xml::events::Event::Text(t)) => {
                if let Some(ref elem) = current_element {
                    if let Ok(text) = t.decode() {
                        let text = text.into_owned();

                        // Handle attendee fields
                        if in_attendees && current_attendee.is_some() {
                            let attendee = current_attendee.as_mut().unwrap();
                            match elem.as_str() {
                                "Email" => attendee.email = text,
                                "Name" => attendee.name = Some(text),
                                "AttendeeType" => {
                                    attendee.attendee_type = text.parse().unwrap_or(1)
                                }
                                "AttendeeStatus" => attendee.attendee_status = text.parse().ok(),
                                _ => {}
                            }
                            continue;
                        }

                        // Handle recurrence fields
                        if in_recurrence && event.recurrence.is_some() {
                            let recurrence = event.recurrence.as_mut().unwrap();
                            match elem.as_str() {
                                "Type" => recurrence.recurrence_type = text.parse().unwrap_or(0),
                                "Interval" => recurrence.interval = text.parse().ok(),
                                "Until" => recurrence.until = Some(text),
                                "Occurrences" => recurrence.occurrences = text.parse().ok(),
                                "DayOfWeek" => recurrence.day_of_week = text.parse().ok(),
                                "DayOfMonth" => recurrence.day_of_month = text.parse().ok(),
                                "WeekOfMonth" => recurrence.week_of_month = text.parse().ok(),
                                "MonthOfYear" => recurrence.month_of_year = text.parse().ok(),
                                _ => {}
                            }
                            continue;
                        }

                        // Handle exception fields
                        if in_exceptions && current_exception.is_some() {
                            let exception = current_exception.as_mut().unwrap();
                            match elem.as_str() {
                                "ExceptionStartTime" => exception.exception_start_time = text,
                                "Subject" => exception.subject = Some(text),
                                "StartTime" => exception.start_time = Some(text),
                                "EndTime" => exception.end_time = Some(text),
                                "Location" => exception.location = Some(text),
                                "Body" => exception.body = Some(text),
                                "Deleted" => exception.deleted = text == "1",
                                "IsException" => exception.is_exception = text == "1",
                                _ => {}
                            }
                            continue;
                        }

                        // Handle main event fields
                        match elem.as_str() {
                            "ServerId" => event.server_id = Some(text),
                            "UID" => event.uid = Some(text),
                            "Subject" => event.subject = Some(text),
                            "StartTime" => event.start_time = Some(text),
                            "EndTime" => event.end_time = Some(text),
                            "DtStamp" => event.dt_stamp = Some(text),
                            "OrganizerName" => event.organizer_name = Some(text),
                            "OrganizerEmail" => event.organizer_email = Some(text),
                            "Location" => event.location = Some(text),
                            "Body" => event.body = Some(text),
                            "BodyType" => event.body_type = text.parse().unwrap_or(1),
                            "Sensitivity" => event.sensitivity = text.parse().ok(),
                            "BusyStatus" => event.busy_status = text.parse().ok(),
                            "AllDayEvent" => event.all_day_event = text == "1",
                            "Reminder" => event.reminder = text.parse().ok(),
                            "Importance" => event.importance = text.parse().ok(),
                            "MeetingStatus" => event.meeting_status = text.parse().ok(),
                            "IsRecurring" => event.is_recurring = text == "1",
                            _ => {}
                        }
                    }
                }
            }
            Ok(quick_xml::events::Event::End(e)) => {
                let name = String::from_utf8_lossy(e.name().local_name().as_ref());
                match name.as_ref() {
                    "Attendees" => in_attendees = false,
                    "Attendee" if in_attendees => {
                        if let Some(attendee) = current_attendee.take() {
                            event.attendees.push(attendee);
                        }
                    }
                    "Recurrence" => in_recurrence = false,
                    "Exceptions" => in_exceptions = false,
                    "Exception" if in_exceptions => {
                        if let Some(exception) = current_exception.take() {
                            event.exceptions.push(exception);
                        }
                    }
                    _ => {}
                }
                current_element = None;
            }
            Ok(quick_xml::events::Event::Eof) => break,
            Err(e) => return Err(format!("XML parse error: {}", e)),
            _ => {}
        }
        buf.clear();
    }

    Ok(event)
}

/// Parse attendees from EAS XML
pub fn parse_attendees_from_eas(xml: &str) -> Vec<EasAttendee> {
    let mut attendees = Vec::new();
    let mut reader = quick_xml::Reader::from_str(xml);
    reader.config_mut().trim_text(true);
    let mut buf = Vec::new();
    let mut current_element: Option<String> = None;
    let mut in_attendee = false;
    let mut current_attendee: Option<EasAttendee> = None;

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(quick_xml::events::Event::Start(e)) => {
                let name = String::from_utf8_lossy(e.name().local_name().as_ref()).to_string();
                current_element = Some(name.clone());
                if name == "Attendee" {
                    in_attendee = true;
                    current_attendee = Some(EasAttendee::default());
                }
            }
            Ok(quick_xml::events::Event::Text(t)) => {
                if in_attendee && current_attendee.is_some() {
                    if let Some(ref elem) = current_element {
                        if let Ok(text) = t.decode() {
                            let text = text.into_owned();
                            let attendee = current_attendee.as_mut().unwrap();
                            match elem.as_str() {
                                "Email" => attendee.email = text,
                                "Name" => attendee.name = Some(text),
                                "AttendeeType" => {
                                    attendee.attendee_type = text.parse().unwrap_or(1)
                                }
                                "AttendeeStatus" => attendee.attendee_status = text.parse().ok(),
                                _ => {}
                            }
                        }
                    }
                }
            }
            Ok(quick_xml::events::Event::End(e)) => {
                if e.name().local_name().as_ref() == b"Attendee" {
                    if let Some(attendee) = current_attendee.take() {
                        attendees.push(attendee);
                    }
                    in_attendee = false;
                }
                current_element = None;
            }
            Ok(quick_xml::events::Event::Eof) => break,
            Err(_) => break,
            _ => {}
        }
        buf.clear();
    }

    attendees
}

/// Parse recurrence from EAS XML
pub fn parse_recurrence_from_eas(xml: &str) -> Option<EasRecurrence> {
    let mut recurrence = EasRecurrence::default();
    let mut reader = quick_xml::Reader::from_str(xml);
    reader.config_mut().trim_text(true);
    let mut buf = Vec::new();
    let mut current_element: Option<String> = None;
    let mut in_recurrence = false;

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(quick_xml::events::Event::Start(e)) => {
                let name = String::from_utf8_lossy(e.name().local_name().as_ref()).to_string();
                current_element = Some(name.clone());
                if name == "Recurrence" {
                    in_recurrence = true;
                }
            }
            Ok(quick_xml::events::Event::Text(t)) => {
                if in_recurrence {
                    if let Some(ref elem) = current_element {
                        if let Ok(text) = t.decode() {
                            let text = text.into_owned();
                            match elem.as_str() {
                                "Type" => recurrence.recurrence_type = text.parse().unwrap_or(0),
                                "Interval" => recurrence.interval = text.parse().ok(),
                                "Until" => recurrence.until = Some(text),
                                "Occurrences" => recurrence.occurrences = text.parse().ok(),
                                "DayOfWeek" => recurrence.day_of_week = text.parse().ok(),
                                "DayOfMonth" => recurrence.day_of_month = text.parse().ok(),
                                "WeekOfMonth" => recurrence.week_of_month = text.parse().ok(),
                                "MonthOfYear" => recurrence.month_of_year = text.parse().ok(),
                                _ => {}
                            }
                        }
                    }
                }
            }
            Ok(quick_xml::events::Event::End(e)) => {
                if e.name().local_name().as_ref() == b"Recurrence" {
                    in_recurrence = false;
                }
                current_element = None;
            }
            Ok(quick_xml::events::Event::Eof) => break,
            Err(_) => break,
            _ => {}
        }
        buf.clear();
    }

    if in_recurrence || recurrence.recurrence_type > 0 {
        Some(recurrence)
    } else {
        None
    }
}

/// Build EAS calendar response XML
pub fn build_eas_calendar_response(event: &EasCalendarEvent) -> String {
    let mut xml = String::new();

    if let Some(ref server_id) = event.server_id {
        xml.push_str(&format!("<ServerId>{}</ServerId>", xml_escape(server_id)));
    }

    if let Some(ref uid) = event.uid {
        xml.push_str(&format!("<UID>{}</UID>", xml_escape(uid)));
    }

    if let Some(ref subject) = event.subject {
        xml.push_str(&format!("<Subject>{}</Subject>", xml_escape(subject)));
    }

    if let Some(ref start) = event.start_time {
        xml.push_str(&format!("<StartTime>{}</StartTime>", xml_escape(start)));
    }

    if let Some(ref end) = event.end_time {
        xml.push_str(&format!("<EndTime>{}</EndTime>", xml_escape(end)));
    }

    if let Some(ref dt_stamp) = event.dt_stamp {
        xml.push_str(&format!("<DtStamp>{}</DtStamp>", xml_escape(dt_stamp)));
    }

    if let Some(ref organizer_name) = event.organizer_name {
        xml.push_str(&format!(
            "<OrganizerName>{}</OrganizerName>",
            xml_escape(organizer_name)
        ));
    }

    if let Some(ref organizer_email) = event.organizer_email {
        xml.push_str(&format!(
            "<OrganizerEmail>{}</OrganizerEmail>",
            xml_escape(organizer_email)
        ));
    }

    if let Some(ref location) = event.location {
        xml.push_str(&format!("<Location>{}</Location>", xml_escape(location)));
    }

    // Body with type
    if let Some(ref body) = event.body {
        xml.push_str("<Body>");
        xml.push_str(&format!("<Type>{}</Type>", event.body_type));
        xml.push_str(&format!("<Data>{}</Data>", xml_escape(body)));
        xml.push_str("</Body>");
    }

    if let Some(sensitivity) = event.sensitivity {
        xml.push_str(&format!("<Sensitivity>{}</Sensitivity>", sensitivity));
    }

    if let Some(busy_status) = event.busy_status {
        xml.push_str(&format!("<BusyStatus>{}</BusyStatus>", busy_status));
    }

    if event.all_day_event {
        xml.push_str("<AllDayEvent>1</AllDayEvent>");
    }

    if let Some(reminder) = event.reminder {
        xml.push_str(&format!("<Reminder>{}</Reminder>", reminder));
    }

    // Attendees
    if !event.attendees.is_empty() {
        xml.push_str("<Attendees>");
        for attendee in &event.attendees {
            xml.push_str("<Attendee>");
            xml.push_str(&format!("<Email>{}</Email>", xml_escape(&attendee.email)));
            if let Some(ref name) = attendee.name {
                xml.push_str(&format!("<Name>{}</Name>", xml_escape(name)));
            }
            xml.push_str(&format!(
                "<AttendeeType>{}</AttendeeType>",
                attendee.attendee_type
            ));
            if let Some(status) = attendee.attendee_status {
                xml.push_str(&format!("<AttendeeStatus>{}</AttendeeStatus>", status));
            }
            xml.push_str("</Attendee>");
        }
        xml.push_str("</Attendees>");
    }

    // Recurrence
    if let Some(ref recurrence) = event.recurrence {
        xml.push_str("<Recurrence>");
        xml.push_str(&format!("<Type>{}</Type>", recurrence.recurrence_type));
        if let Some(interval) = recurrence.interval {
            xml.push_str(&format!("<Interval>{}</Interval>", interval));
        }
        if let Some(ref until) = recurrence.until {
            xml.push_str(&format!("<Until>{}</Until>", xml_escape(until)));
        }
        if let Some(occurrences) = recurrence.occurrences {
            xml.push_str(&format!("<Occurrences>{}</Occurrences>", occurrences));
        }
        if let Some(day_of_week) = recurrence.day_of_week {
            xml.push_str(&format!("<DayOfWeek>{}</DayOfWeek>", day_of_week));
        }
        if let Some(day_of_month) = recurrence.day_of_month {
            xml.push_str(&format!("<DayOfMonth>{}</DayOfMonth>", day_of_month));
        }
        if let Some(week_of_month) = recurrence.week_of_month {
            xml.push_str(&format!("<WeekOfMonth>{}</WeekOfMonth>", week_of_month));
        }
        if let Some(month_of_year) = recurrence.month_of_year {
            xml.push_str(&format!("<MonthOfYear>{}</MonthOfYear>", month_of_year));
        }
        xml.push_str("</Recurrence>");
    }

    // Exceptions
    if !event.exceptions.is_empty() {
        xml.push_str("<Exceptions>");
        for exception in &event.exceptions {
            xml.push_str("<Exception>");
            xml.push_str(&format!(
                "<ExceptionStartTime>{}</ExceptionStartTime>",
                xml_escape(&exception.exception_start_time)
            ));
            if let Some(ref subject) = exception.subject {
                xml.push_str(&format!("<Subject>{}</Subject>", xml_escape(subject)));
            }
            if let Some(ref start) = exception.start_time {
                xml.push_str(&format!("<StartTime>{}</StartTime>", xml_escape(start)));
            }
            if let Some(ref end) = exception.end_time {
                xml.push_str(&format!("<EndTime>{}</EndTime>", xml_escape(end)));
            }
            if let Some(ref location) = exception.location {
                xml.push_str(&format!("<Location>{}</Location>", xml_escape(location)));
            }
            if let Some(ref body) = exception.body {
                xml.push_str(&format!("<Body>{}</Body>", xml_escape(body)));
            }
            if exception.deleted {
                xml.push_str("<Deleted>1</Deleted>");
            }
            if exception.is_exception {
                xml.push_str("<IsException>1</IsException>");
            }
            xml.push_str("</Exception>");
        }
        xml.push_str("</Exceptions>");
    }

    // Categories
    if !event.categories.is_empty() {
        xml.push_str("<Categories>");
        for category in &event.categories {
            xml.push_str(&format!("<Category>{}</Category>", xml_escape(category)));
        }
        xml.push_str("</Categories>");
    }

    if let Some(importance) = event.importance {
        xml.push_str(&format!("<Importance>{}</Importance>", importance));
    }

    if event.is_recurring {
        xml.push_str("<IsRecurring>1</IsRecurring>");
    }

    if let Some(meeting_status) = event.meeting_status {
        xml.push_str(&format!(
            "<MeetingStatus>{}</MeetingStatus>",
            meeting_status
        ));
    }

    xml
}

/// XML escape helper
fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

/// Convert iCalendar RRULE to EAS recurrence
pub fn ical_rrule_to_eas(rrule: &str) -> Option<EasRecurrence> {
    let mut recurrence = EasRecurrence::default();

    // Parse FREQ
    if rrule.contains("FREQ=DAILY") {
        recurrence.recurrence_type = 0;
    } else if rrule.contains("FREQ=WEEKLY") {
        recurrence.recurrence_type = 1;
    } else if rrule.contains("FREQ=MONTHLY") {
        if rrule.contains("BYDAY=") {
            recurrence.recurrence_type = 3; // Monthly on nth day
        } else {
            recurrence.recurrence_type = 2; // Monthly on date
        }
    } else if rrule.contains("FREQ=YEARLY") {
        if rrule.contains("BYDAY=") {
            recurrence.recurrence_type = 6; // Yearly on nth day
        } else {
            recurrence.recurrence_type = 5; // Yearly on date
        }
    }

    // Parse INTERVAL
    if let Some(pos) = rrule.find("INTERVAL=") {
        let start = pos + 9;
        let end = rrule[start..]
            .find(|c| c == ';' || c == '\n')
            .map(|p| start + p)
            .unwrap_or(rrule.len());
        recurrence.interval = rrule[start..end].parse().ok();
    }

    // Parse COUNT
    if let Some(pos) = rrule.find("COUNT=") {
        let start = pos + 6;
        let end = rrule[start..]
            .find(|c| c == ';' || c == '\n')
            .map(|p| start + p)
            .unwrap_or(rrule.len());
        recurrence.occurrences = rrule[start..end].parse().ok();
    }

    // Parse UNTIL
    if let Some(pos) = rrule.find("UNTIL=") {
        let start = pos + 6;
        let end = rrule[start..]
            .find(|c| c == ';' || c == '\n')
            .map(|p| start + p)
            .unwrap_or(rrule.len());
        recurrence.until = Some(rrule[start..end].to_string());
    }

    // Parse BYDAY
    if let Some(pos) = rrule.find("BYDAY=") {
        let start = pos + 6;
        let end = rrule[start..]
            .find(|c| c == ';' || c == '\n')
            .map(|p| start + p)
            .unwrap_or(rrule.len());
        let byday = &rrule[start..end];

        // Convert BYDAY to DayOfWeek bitmask
        let mut day_mask: u8 = 0;
        if byday.contains("SU") {
            day_mask |= 1;
        }
        if byday.contains("MO") {
            day_mask |= 2;
        }
        if byday.contains("TU") {
            day_mask |= 4;
        }
        if byday.contains("WE") {
            day_mask |= 8;
        }
        if byday.contains("TH") {
            day_mask |= 16;
        }
        if byday.contains("FR") {
            day_mask |= 32;
        }
        if byday.contains("SA") {
            day_mask |= 64;
        }
        recurrence.day_of_week = Some(day_mask);

        // Parse week of month from BYDAY (e.g., "1MO" = first Monday)
        if let Some(week) = byday.chars().next().and_then(|c| c.to_digit(10)) {
            recurrence.week_of_month = Some(week as u8);
        }
    }

    // Parse BYMONTHDAY
    if let Some(pos) = rrule.find("BYMONTHDAY=") {
        let start = pos + 11;
        let end = rrule[start..]
            .find(|c| c == ';' || c == '\n')
            .map(|p| start + p)
            .unwrap_or(rrule.len());
        recurrence.day_of_month = rrule[start..end].parse().ok();
    }

    // Parse BYMONTH
    if let Some(pos) = rrule.find("BYMONTH=") {
        let start = pos + 8;
        let end = rrule[start..]
            .find(|c| c == ';' || c == '\n')
            .map(|p| start + p)
            .unwrap_or(rrule.len());
        recurrence.month_of_year = rrule[start..end].parse().ok();
    }

    Some(recurrence)
}

/// Convert EAS recurrence to iCalendar RRULE
pub fn eas_recurrence_to_ical(recurrence: &EasRecurrence) -> String {
    let mut rrule = String::from("RRULE:FREQ=");

    match recurrence.recurrence_type {
        0 => rrule.push_str("DAILY"),
        1 => rrule.push_str("WEEKLY"),
        2 | 3 => rrule.push_str("MONTHLY"),
        5 | 6 => rrule.push_str("YEARLY"),
        _ => rrule.push_str("DAILY"),
    }

    if let Some(interval) = recurrence.interval {
        rrule.push_str(&format!(";INTERVAL={}", interval));
    }

    if let Some(occurrences) = recurrence.occurrences {
        rrule.push_str(&format!(";COUNT={}", occurrences));
    }

    if let Some(ref until) = recurrence.until {
        rrule.push_str(&format!(";UNTIL={}", until));
    }

    if let Some(day_of_week) = recurrence.day_of_week {
        let mut days = Vec::new();
        if day_of_week & 1 != 0 {
            days.push("SU");
        }
        if day_of_week & 2 != 0 {
            days.push("MO");
        }
        if day_of_week & 4 != 0 {
            days.push("TU");
        }
        if day_of_week & 8 != 0 {
            days.push("WE");
        }
        if day_of_week & 16 != 0 {
            days.push("TH");
        }
        if day_of_week & 32 != 0 {
            days.push("FR");
        }
        if day_of_week & 64 != 0 {
            days.push("SA");
        }
        if !days.is_empty() {
            rrule.push_str(&format!(";BYDAY={}", days.join(",")));
        }
    }

    if let Some(day_of_month) = recurrence.day_of_month {
        rrule.push_str(&format!(";BYMONTHDAY={}", day_of_month));
    }

    if let Some(month_of_year) = recurrence.month_of_year {
        rrule.push_str(&format!(";BYMONTH={}", month_of_year));
    }

    rrule
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_eas_calendar_request() {
        let xml = r#"<ApplicationData>
            <Subject>Test Meeting</Subject>
            <StartTime>2026-03-22T10:00:00.000Z</StartTime>
            <EndTime>2026-03-22T11:00:00.000Z</EndTime>
            <Location>Conference Room</Location>
            <Body>Meeting description</Body>
        </ApplicationData>"#;

        let event = parse_eas_calendar_request(xml).unwrap();
        assert_eq!(event.subject, Some("Test Meeting".to_string()));
        assert_eq!(event.location, Some("Conference Room".to_string()));
    }

    #[test]
    fn test_ical_rrule_to_eas() {
        let rrule = "FREQ=WEEKLY;BYDAY=MO,WE,FR;INTERVAL=1";
        let recurrence = ical_rrule_to_eas(rrule).unwrap();
        assert_eq!(recurrence.recurrence_type, 1);
        assert_eq!(recurrence.day_of_week, Some(42)); // 2 + 8 + 32
    }

    #[test]
    fn test_eas_recurrence_to_ical() {
        let recurrence = EasRecurrence {
            recurrence_type: 1,
            interval: Some(1),
            day_of_week: Some(42),
            ..Default::default()
        };
        let rrule = eas_recurrence_to_ical(&recurrence);
        assert!(rrule.contains("FREQ=WEEKLY"));
        assert!(rrule.contains("BYDAY=MO,WE,FR"));
    }
}
