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
    /// RFC 5545 RECURRENCE-ID: the original UTC start of the specific
    /// recurrence instance this message applies to. `None` targets the whole
    /// series. Required by RFC 5546 §3.6.2/§3.6.7 whenever a response or
    /// counter applies to a single instance of a recurring meeting.
    pub recurrence_id: Option<DateTime<Utc>>,
    /// Email address of the responding attendee (iTIP REPLY ATTENDEE).
    /// Only set for `MeetingMessageType::Response`.
    pub responder_email: Option<String>,
    /// Display name of the responding attendee (iTIP REPLY ATTENDEE CN).
    /// Only set for `MeetingMessageType::Response`.
    pub responder_name: Option<String>,
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
    pub recurrence_id: Option<DateTime<Utc>>,
    pub responder_email: String,
    pub responder_name: Option<String>,
}

/// Parameters for constructing a `METHOD:REPLY` meeting response message.
pub struct ResponseParams<'a> {
    pub uid: &'a str,
    pub organizer_email: &'a str,
    pub subject: &'a str,
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
    pub status: AttendeeStatus,
    pub sequence: u32,
    pub responder_email: &'a str,
    pub responder_name: Option<&'a str>,
    pub recurrence_id: Option<DateTime<Utc>>,
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
            recurrence_id: None,
            responder_email: None,
            responder_name: None,
        }
    }

    pub fn new_update(item: &CalendarItem, sequence: u32) -> Self {
        let mut msg = Self::new_request(item);
        msg.message_type = MeetingMessageType::Update;
        msg.sequence = sequence;
        msg
    }

    pub fn new_response(params: &ResponseParams) -> Self {
        Self {
            message_type: MeetingMessageType::Response,
            uid: params.uid.to_string(),
            sequence: params.sequence,
            organizer_email: params.organizer_email.to_string(),
            organizer_name: None,
            subject: params.subject.to_string(),
            description: None,
            location: None,
            start: params.start,
            end: params.end,
            timezone: None,
            attendees: Vec::new(),
            dtstamp: Utc::now(),
            response_status: Some(params.status),
            proposed_start: None,
            proposed_end: None,
            recurrence_id: params.recurrence_id,
            responder_email: Some(params.responder_email.to_string()),
            responder_name: params.responder_name.map(|n| n.to_string()),
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
            recurrence_id: None,
            responder_email: None,
            responder_name: None,
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
            response_status: None,
            proposed_start: Some(params.proposed_start),
            proposed_end: Some(params.proposed_end),
            recurrence_id: params.recurrence_id,
            responder_email: Some(params.responder_email.clone()),
            responder_name: params.responder_name.clone(),
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
        use icalendar::{Calendar, Component, Event, EventLike, EventStatus, Property};

        let mut calendar = Calendar::empty();
        calendar.append_property(Property::new("VERSION", "2.0"));
        calendar.append_property(Property::new("PRODID", &self.ical_product_id));
        calendar.append_property(Property::new("METHOD", msg.message_type.to_ical_method()));

        let mut event = Event::new();
        event.uid(&msg.uid);
        event.timestamp(msg.dtstamp);
        event.sequence(msg.sequence);

        // RFC 5546 §3.6.2 (REPLY) / §3.6.7 (COUNTER): a message scoped to one
        // instance of a recurring meeting MUST carry RECURRENCE-ID naming the
        // original start of that instance. Emitted in UTC to match the UTC
        // DTSTART/DTEND of the reply/counter VEVENT.
        if let Some(recurrence_id) = msg.recurrence_id {
            event.append_property(Property::new(
                "RECURRENCE-ID",
                format!("{}Z", recurrence_id.format("%Y%m%dT%H%M%S")),
            ));
        }

        let is_counter_with_props = msg.message_type == MeetingMessageType::Counter
            && msg.proposed_start.is_some()
            && msg.proposed_end.is_some();

        if is_counter_with_props {
            if let (Some(start), Some(end)) = (msg.proposed_start, msg.proposed_end) {
                event.append_property(
                    Property::new("DTSTART", format!("{}Z", start.format("%Y%m%dT%H%M%S")))
                        .add_parameter(
                            "X-MS-OLK-ORIGINAL",
                            &format!("{}Z", msg.start.format("%Y%m%dT%H%M%S")),
                        )
                        .done(),
                );
                event.append_property(
                    Property::new("DTEND", format!("{}Z", end.format("%Y%m%dT%H%M%S")))
                        .add_parameter(
                            "X-MS-OLK-ORIGINAL",
                            &format!("{}Z", msg.end.format("%Y%m%dT%H%M%S")),
                        )
                        .done(),
                );
            }
        } else {
            event.ends(msg.end);
            event.starts(msg.start);
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
            let mut org_prop =
                Property::new("ORGANIZER", format!("mailto:{}", msg.organizer_email));
            org_prop.add_parameter(
                "CN",
                msg.organizer_name
                    .as_deref()
                    .unwrap_or(&msg.organizer_email),
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
                    .cn(attendee
                        .name
                        .as_deref()
                        .unwrap_or(&attendee.email)
                        .to_string())
                    .role(ical_role)
                    .partstat(icalendar::PartStat::NeedsAction);
                event.attendee(cal_attendee);
            }
        } else if msg.message_type == MeetingMessageType::Response {
            let mut org_prop =
                Property::new("ORGANIZER", format!("mailto:{}", msg.organizer_email));
            org_prop.add_parameter(
                "CN",
                msg.organizer_name
                    .as_deref()
                    .unwrap_or(&msg.organizer_email),
            );
            event.append_property(org_prop.done());

            let partstat = match msg.response_status {
                Some(AttendeeStatus::Accepted) => icalendar::PartStat::Accepted,
                Some(AttendeeStatus::Declined) => icalendar::PartStat::Declined,
                Some(AttendeeStatus::Tentative) => icalendar::PartStat::Tentative,
                _ => icalendar::PartStat::NeedsAction,
            };
            // iTIP REPLY: the ATTENDEE is the responder (local user), not the
            // organizer. The ORGANIZER above is the meeting organizer that the
            // reply is being sent back to (per RFC 5546 §3.2.3).
            let responder_addr = msg
                .responder_email
                .as_deref()
                .unwrap_or(&msg.organizer_email);
            let mut cal_attendee =
                icalendar::Attendee::new(format!("mailto:{}", responder_addr)).partstat(partstat);
            if let Some(ref name) = msg.responder_name {
                cal_attendee = cal_attendee.cn(name.clone());
            }
            event.attendee(cal_attendee);
        } else if msg.message_type == MeetingMessageType::Counter {
            // RFC 5546 §3.6.7: a COUNTER carries the ORGANIZER of the
            // original REQUEST plus the ATTENDEE proposing the new time.
            let mut org_prop =
                Property::new("ORGANIZER", format!("mailto:{}", msg.organizer_email));
            org_prop.add_parameter(
                "CN",
                msg.organizer_name
                    .as_deref()
                    .unwrap_or(&msg.organizer_email),
            );
            event.append_property(org_prop.done());

            let responder_addr = msg
                .responder_email
                .clone()
                .unwrap_or_else(|| msg.organizer_email.clone());
            let mut cal_attendee = icalendar::Attendee::new(format!("mailto:{}", responder_addr))
                .partstat(icalendar::PartStat::NeedsAction);
            if let Some(ref name) = msg.responder_name {
                cal_attendee = cal_attendee.cn(name.clone());
            }
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

    /// Render the EAS `MeetingResponse` command response ([MS-ASCMD]
    /// §2.2.2 / §6.26): one `<Result>` per request, in request order.
    ///
    /// Each result carries the `<RequestId>` echo — only when the request
    /// addressed the item by RequestId ([MS-ASCMD] §2.2.3.151: "present in
    /// responses only if it was present in the corresponding request"; a
    /// LongId-addressed request yields no id echo at all — §6.26 declares no
    /// LongId child for Result) — followed by the required `<Status>`
    /// (§2.2.3.177.9), then `<CalendarId>` (§2.2.3.18, present only when the
    /// response was accepted or tentative so the client can resolve the
    /// attendee's calendar copy), then the `<InstanceId>` echo for
    /// instance-scoped requests (§2.2.3.92.1), matching the §6.26 child
    /// order RequestId?, Status, CalendarId?, InstanceId?.
    pub fn generate_eas_meeting_response(&self, results: &[MeetingResponseResult]) -> String {
        let mut xml = String::with_capacity(256 + 160 * results.len());
        xml.push_str("<?xml version=\"1.0\" encoding=\"utf-8\"?>\n");
        xml.push_str("<MeetingResponse xmlns=\"MeetingResponse:\">\n");
        for result in results {
            xml.push_str("<Result>");
            if let Some(request_id) = &result.request_id {
                xml.push_str(&format!(
                    "<RequestId>{}</RequestId>",
                    xml_escape(request_id)
                ));
            }
            xml.push_str(&format!("<Status>{}</Status>", result.status));
            if let Some(calendar_id) = &result.calendar_id {
                xml.push_str(&format!(
                    "<CalendarId>{}</CalendarId>",
                    xml_escape(calendar_id)
                ));
            }
            if let Some(instance_id) = &result.instance_id {
                xml.push_str(&format!(
                    "<InstanceId>{}</InstanceId>",
                    xml_escape(instance_id)
                ));
            }
            xml.push_str("</Result>\n");
        }
        xml.push_str("</MeetingResponse>");
        xml
    }
}

/// One `<Result>` block of an EAS `MeetingResponse` command response
/// ([MS-ASCMD] §2.2.3.106 / §2.2.3.177.9).
#[derive(Clone, Debug)]
pub struct MeetingResponseResult {
    /// `<RequestId>` echo — `Some` only when the request addressed the item
    /// by RequestId ([MS-ASCMD] §2.2.3.151: absent from the response when the
    /// request used search:LongId, since §6.26 declares no LongId child of
    /// Result).
    pub request_id: Option<String>,
    /// [MS-ASCMD] §2.2.3.177.9 / §2.2.2 status for this request.
    pub status: u16,
    /// `<CalendarId>` of the attendee's calendar copy. Present only for
    /// accepted/tentative responses ([MS-ASCMD] §2.2.3.18).
    pub calendar_id: Option<String>,
    /// `<InstanceId>` echo for instance-scoped requests ([MS-ASCMD]
    /// §2.2.3.92.1), in the request's wire format.
    pub instance_id: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Duration, TimeZone};

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
                schedule_agent: None,
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
        let msg = MeetingMessage::new_response(&ResponseParams {
            uid: "test-uid",
            organizer_email: "organizer@example.com",
            subject: "Test Meeting",
            start: Utc::now(),
            end: Utc::now() + Duration::hours(1),
            status: AttendeeStatus::Accepted,
            sequence: 1,
            responder_email: "attendee@example.com",
            responder_name: Some("Attendee Name"),
            recurrence_id: None,
        });

        let ics = generator.generate_ical(&msg);

        assert!(ics.contains("METHOD:REPLY"));
        assert!(ics.contains("PARTSTAT=ACCEPTED"));
        // iTIP REPLY: ATTENDEE must be the responder, not the organizer.
        assert!(ics.contains("mailto:attendee@example.com"), "{ics}");
        // RFC 5546 §3.6.2: the REPLY echoes the triggering REQUEST's SEQUENCE.
        assert!(ics.contains("SEQUENCE:1"), "{ics}");
        // A whole-series reply must not carry RECURRENCE-ID.
        assert!(!ics.contains("RECURRENCE-ID"), "{ics}");
    }

    #[test]
    fn test_generate_ical_response_for_recurrence_instance() {
        let generator = MeetingMessageGenerator::new();
        let recurrence_id = Utc.with_ymd_and_hms(2026, 7, 10, 9, 0, 0).unwrap();
        let msg = MeetingMessage::new_response(&ResponseParams {
            uid: "test-uid",
            organizer_email: "organizer@example.com",
            subject: "Test Meeting",
            start: Utc.with_ymd_and_hms(2026, 7, 10, 10, 0, 0).unwrap(),
            end: Utc.with_ymd_and_hms(2026, 7, 10, 11, 0, 0).unwrap(),
            status: AttendeeStatus::Declined,
            sequence: 3,
            responder_email: "attendee@example.com",
            responder_name: None,
            recurrence_id: Some(recurrence_id),
        });

        let ics = generator.generate_ical(&msg);

        assert!(ics.contains("METHOD:REPLY"));
        // RFC 5546 §3.6.2: an instance-scoped REPLY carries the instance's
        // original start as RECURRENCE-ID, in UTC to match the UTC DTSTART.
        assert!(ics.contains("RECURRENCE-ID:20260710T090000Z"), "{ics}");
        assert!(ics.contains("PARTSTAT=DECLINED"), "{ics}");
        assert!(ics.contains("SEQUENCE:3"), "{ics}");
    }

    #[test]
    fn test_generate_ical_counter_carries_organizer_and_responder() {
        let generator = MeetingMessageGenerator::new();
        let msg = MeetingMessage::new_counter(&CounterParams {
            uid: "test-uid".to_string(),
            organizer_email: "organizer@example.com".to_string(),
            subject: "Test Meeting".to_string(),
            original_start: Utc.with_ymd_and_hms(2026, 7, 10, 10, 0, 0).unwrap(),
            original_end: Utc.with_ymd_and_hms(2026, 7, 10, 11, 0, 0).unwrap(),
            proposed_start: Utc.with_ymd_and_hms(2026, 7, 10, 12, 0, 0).unwrap(),
            proposed_end: Utc.with_ymd_and_hms(2026, 7, 10, 13, 0, 0).unwrap(),
            sequence: 2,
            recurrence_id: None,
            responder_email: "attendee@example.com".to_string(),
            responder_name: None,
        });

        let ics = generator.generate_ical(&msg);

        // RFC 5546 §3.6.7: COUNTER carries METHOD:COUNTER, the original
        // organizer, and the attendee proposing the new time.
        assert!(ics.contains("METHOD:COUNTER"), "{ics}");
        assert!(ics.contains("mailto:organizer@example.com"), "{ics}");
        assert!(ics.contains("mailto:attendee@example.com"), "{ics}");
        // The proposed time is the counter's DTSTART/DTEND.
        assert!(
            ics.contains("DTSTART;X-MS-OLK-ORIGINAL=20260710T100000Z:20260710T120000Z") || {
                let flat = ics.replace("\r\n ", "").replace("\r\n", "\n");
                flat.contains("DTSTART;X-MS-OLK-ORIGINAL=20260710T100000Z:20260710T120000Z")
            },
            "{ics}"
        );
        assert!(ics.contains("SEQUENCE:2"), "{ics}");
    }

    #[test]
    fn test_generate_ical_counter_for_recurrence_instance() {
        let generator = MeetingMessageGenerator::new();
        let msg = MeetingMessage::new_counter(&CounterParams {
            uid: "test-uid".to_string(),
            organizer_email: "organizer@example.com".to_string(),
            subject: "Test Meeting".to_string(),
            original_start: Utc.with_ymd_and_hms(2026, 7, 10, 10, 0, 0).unwrap(),
            original_end: Utc.with_ymd_and_hms(2026, 7, 10, 11, 0, 0).unwrap(),
            proposed_start: Utc.with_ymd_and_hms(2026, 7, 10, 12, 0, 0).unwrap(),
            proposed_end: Utc.with_ymd_and_hms(2026, 7, 10, 13, 0, 0).unwrap(),
            sequence: 2,
            recurrence_id: Some(Utc.with_ymd_and_hms(2026, 7, 10, 10, 0, 0).unwrap()),
            responder_email: "attendee@example.com".to_string(),
            responder_name: None,
        });

        let ics = generator.generate_ical(&msg);
        assert!(ics.contains("RECURRENCE-ID:20260710T100000Z"), "{ics}");
    }

    #[test]
    fn test_generate_eas_meeting_response_shape() {
        let generator = MeetingMessageGenerator::new();
        let xml = generator.generate_eas_meeting_response(&[
            MeetingResponseResult {
                request_id: Some("em-1:42".to_string()),
                status: 1,
                calendar_id: Some("cal-abc".to_string()),
                instance_id: None,
            },
            MeetingResponseResult {
                // [MS-ASCMD] §2.2.3.151 / §6.26: a LongId-addressed request
                // produces a Result with no id echo at all.
                request_id: None,
                status: 2,
                calendar_id: None,
                instance_id: None,
            },
            MeetingResponseResult {
                request_id: Some("em-1:43".to_string()),
                status: 1,
                calendar_id: Some("cal-abc".to_string()),
                instance_id: Some("2026-07-10T09:00:00.000Z".to_string()),
            },
        ]);

        assert!(
            xml.contains("<MeetingResponse xmlns=\"MeetingResponse:\""),
            "{xml}"
        );
        assert!(!xml.contains("xmlns:Search"), "{xml}");
        assert_eq!(xml.matches("<Result>").count(), 3, "{xml}");
        // [MS-ASCMD] §6.26 child order: RequestId?, Status, CalendarId?, InstanceId?.
        let first = xml
            .split("<Result>")
            .nth(1)
            .unwrap()
            .split("</Result>")
            .next()
            .unwrap();
        assert!(
            first.starts_with(
                "<RequestId>em-1:42</RequestId><Status>1</Status><CalendarId>cal-abc</CalendarId>"
            ),
            "{first}"
        );
        let second = xml
            .split("<Result>")
            .nth(2)
            .unwrap()
            .split("</Result>")
            .next()
            .unwrap();
        assert!(second.starts_with("<Status>2</Status>"), "{second}");
        assert!(!second.contains("<RequestId>"), "{second}");
        assert!(!second.contains("<CalendarId>"), "{second}");
        let third = xml
            .split("<Result>")
            .nth(3)
            .unwrap()
            .split("</Result>")
            .next()
            .unwrap();
        assert!(
            third.ends_with("<InstanceId>2026-07-10T09:00:00.000Z</InstanceId>"),
            "{third}"
        );
    }

    /// [MS-ASWBXML] §2.1.2.1.9 code page 8: the MeetingResponse response
    /// must encode with a code-page-8 root (`MeetingResponse` = 0x07) and
    /// every emitted tag must have a token on that page. A response template
    /// that fails to encode turns every client RSVP into an HTTP 500.
    #[test]
    fn test_eas_meeting_response_encodes_as_wbxml_page8() {
        let generator = MeetingMessageGenerator::new();
        let xml = generator.generate_eas_meeting_response(&[
            MeetingResponseResult {
                request_id: Some("em-1:42".to_string()),
                status: 1,
                calendar_id: Some("cal-abc".to_string()),
                instance_id: None,
            },
            MeetingResponseResult {
                request_id: None,
                status: 2,
                calendar_id: None,
                instance_id: None,
            },
            MeetingResponseResult {
                request_id: Some("em-1:43".to_string()),
                status: 146,
                calendar_id: None,
                instance_id: Some("2026-07-10T09:00:00.000Z".to_string()),
            },
        ]);

        let bytes = crate::wbxml::Wbxml::new()
            .encode(&xml)
            .expect("MeetingResponse payload must WBXML-encode");
        // The 4-byte WBXML header (version 3, publicid 1, charset 106=utf-8,
        // empty string table) is followed by SWITCH_PAGE (0x00) to code page
        // 8, then the root tag: `MeetingResponse` = 0x07 with the content bit
        // set (0x40) because it has children.
        assert_eq!(bytes[4], 0x00, "bytes: {bytes:?}");
        assert_eq!(bytes[5], 0x08, "bytes: {bytes:?}");
        assert_eq!(bytes[6], 0x47, "bytes: {bytes:?}");

        // Decoding back must reproduce the same document shape: the
        // round-trip proves every tag carries a page-8 token and every text
        // value survives.
        let decoded = crate::wbxml::Wbxml::new()
            .decode(&bytes)
            .expect("MeetingResponse WBXML must decode");
        assert!(decoded.contains("<MeetingResponse"), "{decoded}");
        assert_eq!(decoded.matches("<Result>").count(), 3, "{decoded}");
        assert!(
            decoded.contains("<RequestId>em-1:42</RequestId>"),
            "{decoded}"
        );
        assert!(decoded.contains("<Status>1</Status>"), "{decoded}");
        assert!(
            decoded.contains("<CalendarId>cal-abc</CalendarId>"),
            "{decoded}"
        );
        assert!(decoded.contains("<Status>146</Status>"), "{decoded}");
        assert!(
            decoded.contains("<InstanceId>2026-07-10T09:00:00.000Z</InstanceId>"),
            "{decoded}"
        );
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
        assert_eq!(crate::util::escape_ical_text("a,b;c\\d"), "a\\,b\\;c\\\\d");
        assert_eq!(
            crate::util::escape_ical_text("line1\nline2"),
            "line1\\nline2"
        );
    }

    #[test]
    fn test_message_type_conversion() {
        assert_eq!(MeetingMessageType::Request.to_ical_method(), "REQUEST");
        assert_eq!(MeetingMessageType::Response.to_ical_method(), "REPLY");
        assert_eq!(MeetingMessageType::Cancellation.to_ical_method(), "CANCEL");
    }
}
