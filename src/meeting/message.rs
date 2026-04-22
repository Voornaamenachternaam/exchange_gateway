// src/meeting/message.rs
use crate::calendar::{Attendee, CalendarItem};
use crate::meeting::attendee::{AttendeeRole, AttendeeStatus};
use crate::util::{escape_ical_text, xml_escape};
use chrono::{DateTime, Utc};

#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub enum MeetingMessageType {
    #[default]
    Request,
    Update,
    Response,
    Cancellation,
    Counter,
    Forward,
}

impl MeetingMessageType {
    pub fn to_ical_method(&self) -> &'static str {
        match self {
            Self::Request => "REQUEST",
            Self::Update => "REQUEST",
            Self::Response => "REPLY",
            Self::Cancellation => "CANCEL",
            Self::Counter => "COUNTER",
            Self::Forward => "REQUEST",
        }
    }

    pub fn to_ews_message_class(&self, partstat: Option<&str>) -> &'static str {
        match self {
            Self::Request => "IPM.Schedule.Meeting.Request",
            Self::Update => "IPM.Schedule.Meeting.Request",
            Self::Response => match partstat {
                Some("ACCEPTED") => "IPM.Schedule.Meeting.Resp.Pos",
                Some("DECLINED") => "IPM.Schedule.Meeting.Resp.Neg",
                Some("TENTATIVE") => "IPM.Schedule.Meeting.Resp.Tent",
                _ => "IPM.Schedule.Meeting.Resp",
            },
            Self::Cancellation => "IPM.Schedule.Meeting.Canceled",
            Self::Counter => "IPM.Schedule.Meeting.Request",
            Self::Forward => "IPM.Schedule.Meeting.Request",
        }
    }

    pub fn to_eas_meeting_type(&self) -> u8 {
        match self {
            Self::Request => 1,
            Self::Update => 2,
            Self::Cancellation => 4,
            Self::Response => 0,
            Self::Counter => 3,
            Self::Forward => 5,
        }
    }
}

#[derive(Clone, Debug)]
pub struct MeetingMessage {
    pub message_type: MeetingMessageType,
    pub uid: String,
    pub sequence: u32,
    pub organizer_email: String,
    pub organizer_name: Option<String>,
    pub subject: String,
    pub description: Option<String>,
    pub location: Option<String>,
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
    pub timezone: Option<String>,
    pub attendees: Vec<Attendee>,
    pub dtstamp: DateTime<Utc>,
    pub response_status: Option<AttendeeStatus>,
    pub proposed_start: Option<DateTime<Utc>>,
    pub proposed_end: Option<DateTime<Utc>>,
}

impl MeetingMessage {
    pub fn new_request(item: &CalendarItem) -> Self {
        Self {
            message_type: MeetingMessageType::Request,
            uid: item.uid.clone(),
            sequence: 0,
            organizer_email: item.organizer_email.clone().unwrap_or_default(),
            organizer_name: item.organizer_name.clone(),
            subject: item.subject.clone(),
            description: Some(item.description.clone()).filter(|s| !s.is_empty()),
            location: Some(item.location.clone()).filter(|s| !s.is_empty()),
            start: item.start,
            end: item.end,
            timezone: item.timezone.clone(),
            attendees: item.attendees.clone(),
            dtstamp: Utc::now(),
            response_status: None,
            proposed_start: None,
            proposed_end: None,
        }
    }

    pub fn new_update(item: &CalendarItem, sequence: u32) -> Self {
        let mut msg = Self::new_request(item);
        msg.message_type = MeetingMessageType::Update;
        msg.sequence = sequence;
        msg
    }

    pub fn new_response(
        uid: &str,
        organizer_email: &str,
        subject: &str,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
        status: AttendeeStatus,
        sequence: u32,
    ) -> Self {
        Self {
            message_type: MeetingMessageType::Response,
            uid: uid.to_string(),
            sequence,
            organizer_email: organizer_email.to_string(),
            organizer_name: None,
            subject: subject.to_string(),
            description: None,
            location: None,
            start,
            end,
            timezone: None,
            attendees: Vec::new(),
            dtstamp: Utc::now(),
            response_status: Some(status),
            proposed_start: None,
            proposed_end: None,
        }
    }

    pub fn new_cancellation(item: &CalendarItem, sequence: u32) -> Self {
        Self {
            message_type: MeetingMessageType::Cancellation,
            uid: item.uid.clone(),
            sequence,
            organizer_email: item.organizer_email.clone().unwrap_or_default(),
            organizer_name: item.organizer_name.clone(),
            subject: item.subject.clone(),
            description: None,
            location: None,
            start: item.start,
            end: item.end,
            timezone: item.timezone.clone(),
            attendees: Vec::new(),
            dtstamp: Utc::now(),
            response_status: None,
            proposed_start: None,
            proposed_end: None,
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn new_counter(
        uid: &str,
        organizer_email: &str,
        subject: &str,
        original_start: DateTime<Utc>,
        original_end: DateTime<Utc>,
        proposed_start: DateTime<Utc>,
        proposed_end: DateTime<Utc>,
        sequence: u32,
    ) -> Self {
        Self {
            message_type: MeetingMessageType::Counter,
            uid: uid.to_string(),
            sequence,
            organizer_email: organizer_email.to_string(),
            organizer_name: None,
            subject: subject.to_string(),
            description: None,
            location: None,
            start: original_start,
            end: original_end,
            timezone: None,
            attendees: Vec::new(),
            dtstamp: Utc::now(),
            response_status: Some(AttendeeStatus::Tentative),
            proposed_start: Some(proposed_start),
            proposed_end: Some(proposed_end),
        }
    }
}

pub struct MeetingMessageGenerator {
    ical_product_id: String,
}

impl Default for MeetingMessageGenerator {
    fn default() -> Self {
        Self::new()
    }
}

impl MeetingMessageGenerator {
    pub fn new() -> Self {
        Self {
            ical_product_id: "-//Exchange Gateway//Calendar//EN".to_string(),
        }
    }

    pub fn generate_ical(&self, msg: &MeetingMessage) -> String {
        let mut ics = String::with_capacity(4096);

        ics.push_str("BEGIN:VCALENDAR\r\n");
        ics.push_str(&format!("PRODID:{}\r\n", self.ical_product_id));
        ics.push_str("VERSION:2.0\r\n");
        ics.push_str(&format!("METHOD:{}\r\n", msg.message_type.to_ical_method()));

        ics.push_str("BEGIN:VEVENT\r\n");
        ics.push_str(&format!("UID:{}\r\n", msg.uid));
        ics.push_str(&format!(
            "DTSTAMP:{}\r\n",
            msg.dtstamp.format("%Y%m%dT%H%M%SZ")
        ));
        ics.push_str(&format!(
            "DTSTART:{}\r\n",
            msg.start.format("%Y%m%dT%H%M%SZ")
        ));
        ics.push_str(&format!("DTEND:{}\r\n", msg.end.format("%Y%m%dT%H%M%SZ")));
        ics.push_str(&format!("SEQUENCE:{}\r\n", msg.sequence));

        if !msg.subject.is_empty() {
            ics.push_str(&format!("SUMMARY:{}\r\n", escape_ical_text(&msg.subject)));
        }

        if let Some(ref desc) = msg.description {
            ics.push_str(&format!("DESCRIPTION:{}\r\n", escape_ical_text(desc)));
        }

        if let Some(ref loc) = msg.location {
            ics.push_str(&format!("LOCATION:{}\r\n", escape_ical_text(loc)));
        }

        if msg.message_type == MeetingMessageType::Request
            || msg.message_type == MeetingMessageType::Update
        {
            ics.push_str(&format!(
                "ORGANIZER;CN={}:mailto:{}\r\n",
                escape_ical_text(
                    msg.organizer_name
                        .as_deref()
                        .unwrap_or(&msg.organizer_email)
                ),
                msg.organizer_email
            ));

            for attendee in &msg.attendees {
                let role = AttendeeRole::from(attendee.attendee_type.unwrap_or(1));
                let status = AttendeeStatus::NeedsAction;
                ics.push_str(&format!(
                    "ATTENDEE;CN={};ROLE={};PARTSTAT={}:mailto:{}\r\n",
                    escape_ical_text(attendee.name.as_deref().unwrap_or(&attendee.email)),
                    role.to_ical_role(),
                    status.to_partstat(),
                    attendee.email
                ));
            }
        } else if msg.message_type == MeetingMessageType::Response {
            if let Some(ref status) = msg.response_status {
                ics.push_str(&format!(
                    "ORGANIZER;CN={}:mailto:{}\r\n",
                    escape_ical_text(
                        msg.organizer_name
                            .as_deref()
                            .unwrap_or(&msg.organizer_email)
                    ),
                    msg.organizer_email
                ));
                ics.push_str(&format!(
                    "ATTENDEE;PARTSTAT={}:mailto:{}\r\n",
                    status.to_partstat(),
                    msg.organizer_email
                ));
            }
        } else if msg.message_type == MeetingMessageType::Counter
            && let (Some(start), Some(end)) = (msg.proposed_start, msg.proposed_end)
        {
            ics.push_str(&format!(
                "DTSTART;X-MS-OLK-ORIGINAL={}Z:{}Z\r\n",
                msg.start.format("%Y%m%dT%H%M%S"),
                start.format("%Y%m%dT%H%M%S")
            ));
            ics.push_str(&format!(
                "DTEND;X-MS-OLK-ORIGINAL={}Z:{}Z\r\n",
                msg.end.format("%Y%m%dT%H%M%S"),
                end.format("%Y%m%dT%H%M%S")
            ));
        }

        if msg.message_type == MeetingMessageType::Request
            || msg.message_type == MeetingMessageType::Update
        {
            ics.push_str("X-MICROSOFT-CDO-BUSYSTATUS:BUSY\r\n");
        }

        if msg.message_type == MeetingMessageType::Cancellation {
            ics.push_str("STATUS:CANCELLED\r\n");
        }

        ics.push_str("END:VEVENT\r\n");
        ics.push_str("END:VCALENDAR\r\n");

        ics
    }

    pub fn generate_ews_create_response(
        &self,
        message_id: &str,
        change_key: &str,
        item: &CalendarItem,
    ) -> String {
        let mut items_xml = String::with_capacity(4096);

        items_xml.push_str(&format!(
            "<t:CalendarItem><t:ItemId Id=\"{}\" ChangeKey=\"{}\"/>",
            xml_escape(message_id),
            xml_escape(change_key)
        ));

        items_xml.push_str(&format!(
            "<t:Subject>{}</t:Subject>",
            xml_escape(&item.subject)
        ));

        items_xml.push_str(&format!(
            "<t:Start>{}</t:Start>",
            item.start.format("%Y-%m-%dT%H:%M:%SZ")
        ));

        items_xml.push_str(&format!(
            "<t:End>{}</t:End>",
            item.end.format("%Y-%m-%dT%H:%M:%SZ")
        ));

        if !item.location.is_empty() {
            items_xml.push_str(&format!(
                "<t:Location>{}</t:Location>",
                xml_escape(&item.location)
            ));
        }

        if !item.attendees.is_empty() {
            let required: Vec<&Attendee> = item
                .attendees
                .iter()
                .filter(|a| a.attendee_type.unwrap_or(1) == 1)
                .collect();
            let optional: Vec<&Attendee> = item
                .attendees
                .iter()
                .filter(|a| a.attendee_type.unwrap_or(1) == 2)
                .collect();

            if !required.is_empty() {
                items_xml.push_str("<t:RequiredAttendees>");
                for att in required {
                    items_xml.push_str(&format!(
                        "<t:Attendee><t:Mailbox><t:EmailAddress>{}</t:EmailAddress>",
                        xml_escape(&att.email)
                    ));
                    if let Some(ref name) = att.name {
                        items_xml.push_str(&format!("<t:Name>{}</t:Name>", xml_escape(name)));
                    }
                    items_xml.push_str("</t:Mailbox></t:Attendee>");
                }
                items_xml.push_str("</t:RequiredAttendees>");
            }

            if !optional.is_empty() {
                items_xml.push_str("<t:OptionalAttendees>");
                for att in optional {
                    items_xml.push_str(&format!(
                        "<t:Attendee><t:Mailbox><t:EmailAddress>{}</t:EmailAddress>",
                        xml_escape(&att.email)
                    ));
                    if let Some(ref name) = att.name {
                        items_xml.push_str(&format!("<t:Name>{}</t:Name>", xml_escape(name)));
                    }
                    items_xml.push_str("</t:Mailbox></t:Attendee>");
                }
                items_xml.push_str("</t:OptionalAttendees>");
            }
        }

        items_xml.push_str("</t:CalendarItem>");

        items_xml
    }

    pub fn generate_eas_meeting_request(&self, item: &CalendarItem, _server_id: &str) -> String {
        let mut xml = String::with_capacity(4096);

        xml.push_str("<ApplicationData>");
        xml.push_str(&format!(
            "<Calendar:Subject xmlns:Calendar=\"Calendar:\">{}</Calendar:Subject>",
            xml_escape(&item.subject)
        ));
        xml.push_str(&format!(
            "<Calendar:Location xmlns:Calendar=\"Calendar:\">{}</Calendar:Location>",
            xml_escape(&item.location)
        ));
        xml.push_str(&format!(
            "<Calendar:StartTime xmlns:Calendar=\"Calendar:\">{}</Calendar:StartTime>",
            item.start.format("%Y-%m-%dT%H:%M:%SZ")
        ));
        xml.push_str(&format!(
            "<Calendar:EndTime xmlns:Calendar=\"Calendar:\">{}</Calendar:EndTime>",
            item.end.format("%Y-%m-%dT%H:%M:%SZ")
        ));

        if !item.attendees.is_empty() {
            xml.push_str("<Calendar:Attendees xmlns:Calendar=\"Calendar:\">");
            for att in &item.attendees {
                xml.push_str("<Calendar:Attendee>");
                xml.push_str(&format!(
                    "<Calendar:Email>{}</Calendar:Email>",
                    xml_escape(&att.email)
                ));
                if let Some(ref name) = att.name {
                    xml.push_str(&format!(
                        "<Calendar:Name>{}</Calendar:Name>",
                        xml_escape(name)
                    ));
                }
                xml.push_str(&format!(
                    "<Calendar:AttendeeType>{}</Calendar:AttendeeType>",
                    att.attendee_type.unwrap_or(1)
                ));
                xml.push_str("</Calendar:Attendee>");
            }
            xml.push_str("</Calendar:Attendees>");
        }

        xml.push_str(&format!(
            "<Calendar:MeetingStatus xmlns:Calendar=\"Calendar:\">{}</Calendar:MeetingStatus>",
            item.meeting_status.unwrap_or(1)
        ));

        if let Some(ref organizer) = item.organizer_email {
            xml.push_str("<Calendar:Organizer xmlns:Calendar=\"Calendar:\">");
            xml.push_str(&format!(
                "<Calendar:Email>{}</Calendar:Email>",
                xml_escape(organizer)
            ));
            if let Some(ref name) = item.organizer_name {
                xml.push_str(&format!(
                    "<Calendar:Name>{}</Calendar:Name>",
                    xml_escape(name)
                ));
            }
            xml.push_str("</Calendar:Organizer>");
        }

        xml.push_str("</ApplicationData>");

        xml
    }

    pub fn generate_eas_meeting_response(
        &self,
        request_id: &str,
        calendar_id: &str,
        _status: u8,
    ) -> String {
        format!(
            r#"<?xml version="1.0" encoding="utf-8"?>
<MeetingResponse xmlns="MeetingResponse:">
    <Result>
        <RequestId>{}</RequestId>
        <CalendarId>{}</CalendarId>
        <Status>1</Status>
    </Result>
</MeetingResponse>"#,
            xml_escape(request_id),
            xml_escape(calendar_id)
        )
    }
}

#[allow(dead_code)]
fn folded_line(line: &str) -> String {
    fold_ical_line(line, 75)
}

pub fn fold_ical_line(line: &str, max_len: usize) -> String {
    if line.len() <= max_len {
        return line.to_string();
    }

    let mut result = String::with_capacity(line.len() + (line.len() / max_len) * 3);
    let mut remaining = line;

    while remaining.len() > max_len {
        // Find a safe split point that doesn't break a multi-byte UTF-8 character
        let split_point = remaining
            .char_indices()
            .take_while(|(idx, _)| *idx <= max_len)
            .map(|(idx, c)| idx + c.len_utf8())
            .last()
            .unwrap_or(0);

        // Ensure we make progress and don't split at 0
        let split_point = if split_point == 0 {
            // Take at least one character to ensure progress
            remaining
                .char_indices()
                .nth(1)
                .map(|(idx, _)| idx)
                .unwrap_or(remaining.len())
        } else {
            split_point
        };

        let (chunk, rest) = remaining.split_at(split_point);
        result.push_str(chunk);
        result.push_str("\r\n ");
        remaining = rest;
    }
    result.push_str(remaining);

    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;

    fn make_test_item() -> CalendarItem {
        CalendarItem {
            uid: "test-uid-123".to_string(),
            subject: "Test Meeting".to_string(),
            description: "Test Description".to_string(),
            location: "Test Location".to_string(),
            start: Utc::now(),
            end: Utc::now() + Duration::hours(1),
            organizer_email: Some("organizer@example.com".to_string()),
            organizer_name: Some("Organizer Name".to_string()),
            attendees: vec![Attendee {
                email: "attendee1@example.com".to_string(),
                name: Some("Attendee One".to_string()),
                attendee_type: Some(1),
                attendee_status: Some(0),
                partstat: None,
            }],
            ..Default::default()
        }
    }

    #[test]
    fn test_generate_ical_request() {
        let generator = MeetingMessageGenerator::new();
        let item = make_test_item();
        let msg = MeetingMessage::new_request(&item);

        let ics = generator.generate_ical(&msg);

        assert!(ics.contains("BEGIN:VCALENDAR"));
        assert!(ics.contains("METHOD:REQUEST"));
        assert!(ics.contains("BEGIN:VEVENT"));
        assert!(ics.contains("test-uid-123"));
        assert!(ics.contains("SUMMARY:Test Meeting"));
        assert!(ics.contains("ORGANIZER"));
        assert!(ics.contains("ATTENDEE"));
        assert!(ics.contains("END:VEVENT"));
        assert!(ics.contains("END:VCALENDAR"));
    }

    #[test]
    fn test_generate_ical_response() {
        let generator = MeetingMessageGenerator::new();
        let msg = MeetingMessage::new_response(
            "test-uid",
            "organizer@example.com",
            "Test Meeting",
            Utc::now(),
            Utc::now() + Duration::hours(1),
            AttendeeStatus::Accepted,
            1,
        );

        let ics = generator.generate_ical(&msg);

        assert!(ics.contains("METHOD:REPLY"));
        assert!(ics.contains("PARTSTAT=ACCEPTED"));
    }

    #[test]
    fn test_generate_ical_cancellation() {
        let generator = MeetingMessageGenerator::new();
        let item = make_test_item();
        let msg = MeetingMessage::new_cancellation(&item, 1);

        let ics = generator.generate_ical(&msg);

        assert!(ics.contains("METHOD:CANCEL"));
        assert!(ics.contains("STATUS:CANCELLED"));
    }

    #[test]
    fn test_escape_ical_text() {
        assert_eq!(escape_ical_text("a,b;c\\d"), "a\\,b\\;c\\\\d");
        assert_eq!(escape_ical_text("line1\nline2"), "line1\\nline2");
    }

    #[test]
    fn test_message_type_conversion() {
        assert_eq!(MeetingMessageType::Request.to_ical_method(), "REQUEST");
        assert_eq!(MeetingMessageType::Response.to_ical_method(), "REPLY");
        assert_eq!(MeetingMessageType::Cancellation.to_ical_method(), "CANCEL");
    }
}
