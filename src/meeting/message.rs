// src/meeting/message.rs
use crate::calendar::{Attendee, CalendarItem};
use crate::meeting::attendee::{AttendeeRole, AttendeeStatus};
use crate::util::xml_escape;
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

pub struct CounterParams {
    pub uid: String,
    pub organizer_email: String,
    pub subject: String,
    pub original_start: DateTime<Utc>,
    pub original_end: DateTime<Utc>,
    pub proposed_start: DateTime<Utc>,
    pub proposed_end: DateTime<Utc>,
    pub sequence: u32,
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

    pub fn new_counter(params: &CounterParams) -> Self {
        Self {
            message_type: MeetingMessageType::Counter,
            uid: params.uid.clone(),
            sequence: params.sequence,
            organizer_email: params.organizer_email.clone(),
            organizer_name: None,
            subject: params.subject.clone(),
            description: None,
            location: None,
            start: params.original_start,
            end: params.original_end,
            timezone: None,
            attendees: Vec::new(),
            dtstamp: Utc::now(),
            response_status: Some(AttendeeStatus::Tentative),
            proposed_start: Some(params.proposed_start),
            proposed_end: Some(params.proposed_end),
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

    /// Generate an iCal representation using the `icalendar` crate,
    /// which handles RFC 5545 escaping and line folding automatically.
    pub fn generate_ical(&self, msg: &MeetingMessage) -> String {
        use icalendar::{Calendar, Component, Event, EventLike, EventStatus, Property};

        let mut calendar = Calendar::new();
        calendar.append_property(Property::new("PRODID", &self.ical_product_id));
        calendar.append_property(Property::new("METHOD", msg.message_type.to_ical_method()));

        let mut event = Event::new();
        event.uid(&msg.uid);
        event.timestamp(msg.dtstamp);
        event.sequence(msg.sequence);

        // For Counter messages with proposed times, the DTSTART/DTEND carry
        // the proposed times with X-MS-OLK-ORIGINAL parameters for the originals.
        // Do NOT call event.starts()/event.ends() for Counter, as they would
        // emit duplicate DTSTART/DTEND properties alongside append_property.
        let is_counter_with_props = msg.message_type == MeetingMessageType::Counter
            && msg.proposed_start.is_some()
            && msg.proposed_end.is_some();

        if is_counter_with_props {
            if let (Some(start), Some(end)) = (msg.proposed_start, msg.proposed_end) {
                event.append_property(Property::new(
                    "DTSTART",
                    format!("{}Z", start.format("%Y%m%dT%H%M%S")),
                ).add_parameter("X-MS-OLK-ORIGINAL", &format!("{}Z", msg.start.format("%Y%m%dT%H%M%S"))).done());
                event.append_property(Property::new(
                    "DTEND",
                    format!("{}Z", end.format("%Y%m%dT%H%M%S")),
                ).add_parameter("X-MS-OLK-ORIGINAL", &format!("{}Z", msg.end.format("%Y%m%dT%H%M%S"))).done());
            }
        } else {
            event.starts(msg.start);
            event.ends(msg.end);
        }

        if !msg.subject.is_empty() {
            event.summary(&msg.subject);
        }
        if let Some(ref desc) = msg.description {
            event.description(desc);
        }
        if let Some(ref loc) = msg.location {
            event.location(loc);
        }

        if msg.message_type == MeetingMessageType::Request
            || msg.message_type == MeetingMessageType::Update
        {
            let mut org_prop = Property::new(
                "ORGANIZER",
                format!("mailto:{}", msg.organizer_email),
            );
            org_prop.add_parameter(
                "CN",
                msg.organizer_name.as_deref().unwrap_or(&msg.organizer_email),
            );
            event.append_property(org_prop.done());

            for attendee in &msg.attendees {
                let role = AttendeeRole::from(attendee.attendee_type.unwrap_or(1));
                let ical_role = match role {
                    AttendeeRole::Optional => icalendar::Role::OptParticipant,
                    AttendeeRole::Resource => icalendar::Role::NonParticipant,
                    _ => icalendar::Role::ReqParticipant,
                };
                let cal_attendee = icalendar::Attendee::new(format!("mailto:{}", attendee.email))
                    .cn(attendee.name.as_deref().unwrap_or(&attendee.email).to_string())
                    .role(ical_role)
                    .partstat(icalendar::PartStat::NeedsAction);
                event.attendee(cal_attendee);
            }
        } else if msg.message_type == MeetingMessageType::Response
            && let Some(ref status) = msg.response_status {
                let mut org_prop = Property::new(
                    "ORGANIZER",
                    format!("mailto:{}", msg.organizer_email),
                );
                org_prop.add_parameter(
                    "CN",
                    msg.organizer_name.as_deref().unwrap_or(&msg.organizer_email),
                );
                event.append_property(org_prop.done());

                let partstat = match status {
                    AttendeeStatus::Accepted => icalendar::PartStat::Accepted,
                    AttendeeStatus::Declined => icalendar::PartStat::Declined,
                    AttendeeStatus::Tentative => icalendar::PartStat::Tentative,
                    _ => icalendar::PartStat::NeedsAction,
                };
                let cal_attendee = icalendar::Attendee::new(format!("mailto:{}", msg.organizer_email))
                    .partstat(partstat);
                event.attendee(cal_attendee);
            }

        if msg.message_type == MeetingMessageType::Request
            || msg.message_type == MeetingMessageType::Update
        {
            event.append_property(Property::new("X-MICROSOFT-CDO-BUSYSTATUS", "BUSY"));
        }

        if msg.message_type == MeetingMessageType::Cancellation {
            event.status(EventStatus::Cancelled);
        }

        calendar.push(event.done());
        calendar.to_string()
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
        // The icalendar crate now handles escaping internally
        // but we can verify the util function still works correctly for backward compat
        assert_eq!(crate::util::escape_ical_text("a,b;c\\d"), "a\\,b\\;c\\\\d");
        assert_eq!(crate::util::escape_ical_text("line1\nline2"), "line1\\nline2");
    }

    #[test]
    fn test_message_type_conversion() {
        assert_eq!(MeetingMessageType::Request.to_ical_method(), "REQUEST");
        assert_eq!(MeetingMessageType::Response.to_ical_method(), "REPLY");
        assert_eq!(MeetingMessageType::Cancellation.to_ical_method(), "CANCEL");
    }
}
