// src/meeting/scheduling.rs

use crate::calendar::CalendarItem;
use crate::ical_parser;
use crate::meeting::attendee::{AttendeeRole, AttendeeStatus};
use crate::util::normalize_email;
use chrono::{DateTime, Utc};
use icalendar::{Calendar, Component, Event, EventLike, EventStatus, Property};

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

pub fn build_itip_request(ctx: &SchedulingContext, item: &CalendarItem) -> String {
    let mut calendar = Calendar::new();
    calendar.append_property(Property::new("PRODID", "-//Exchange Gateway//EN"));
    calendar.append_property(Property::new("METHOD", "REQUEST"));

    let mut event = Event::new();
    event.append_property(Property::new("SEQUENCE", ctx.sequence.to_string()));
    event.uid(&ctx.uid);
    event.timestamp(Utc::now());
    event.ends(item.end);
    event.starts(item.start);
    if !item.subject.is_empty() {
        event.summary(&item.subject);
    }
    if !item.location.is_empty() {
        event.location(&item.location);
    }

    let mut org_prop = Property::new("ORGANIZER", format!("mailto:{}", ctx.organizer_email));
    if let Some(name) = &ctx.organizer_name {
        org_prop.add_parameter("CN", name);
    }
    event.append_property(org_prop.done());

    for attendee in &ctx.attendees {
        let ical_role = match attendee.role {
            AttendeeRole::Required => icalendar::Role::ReqParticipant,
            AttendeeRole::Optional => icalendar::Role::OptParticipant,
            AttendeeRole::Resource => icalendar::Role::NonParticipant,
        };
        let ical_partstat = match attendee.status {
            AttendeeStatus::Accepted => icalendar::PartStat::Accepted,
            AttendeeStatus::Declined => icalendar::PartStat::Declined,
            AttendeeStatus::Tentative => icalendar::PartStat::Tentative,
            AttendeeStatus::NotResponded | AttendeeStatus::NeedsAction => {
                icalendar::PartStat::NeedsAction
            }
        };
        let cal_attendee = icalendar::Attendee::new(format!("mailto:{}", attendee.email))
            .cn(attendee
                .name
                .as_deref()
                .unwrap_or(&attendee.email)
                .to_string())
            .role(ical_role)
            .partstat(ical_partstat);
        event.attendee(cal_attendee);
    }

    calendar.push(event.done());
    calendar.to_string()
}

pub fn format_ical_datetime(dt: DateTime<Utc>) -> String {
    dt.format("%Y%m%dT%H%M%SZ").to_string()
}

pub fn parse_itip_response(ical: &str) -> Option<ItipResponse> {
    let parsed = ical_parser::parse_all_vevents(ical).ok()?;
    let event_props = parsed.first()?;

    let mut uid: Option<String> = None;
    let mut sequence: u32 = 0;
    let mut organizer_email: Option<String> = None;
    let mut responding_attendee_email: Option<String> = None;
    let mut responding_partstat: Option<String> = None;

    for (key, value) in event_props {
        let key_upper = key.to_uppercase();
        if key_upper.starts_with("UID") {
            uid = Some(value.trim().to_string());
        } else if key_upper.starts_with("SEQUENCE") {
            sequence = value.trim().parse::<u32>().unwrap_or(0);
        } else if key_upper.starts_with("ORGANIZER") {
            let email = normalize_email(value);
            organizer_email = Some(email);
        }
    }

    let uid = uid?;

    for (key, value) in event_props {
        if key.starts_with("ATTENDEE") {
            let email = normalize_email(value);

            if let Some(ref org_email) = organizer_email
                && email == *org_email
            {
                continue;
            }

            let partstat = key
                .split(';')
                .find_map(|p| {
                    let (k, v) = p.split_once('=')?;
                    if k.eq_ignore_ascii_case("PARTSTAT") {
                        Some(
                            v.trim_matches(|c: char| c == '"' || c.is_whitespace())
                                .to_uppercase(),
                        )
                    } else {
                        None
                    }
                })
                .unwrap_or_else(|| "NEEDS-ACTION".to_string());

            responding_attendee_email = Some(email);
            responding_partstat = Some(partstat);
            break;
        }
    }

    let attendee_email = responding_attendee_email?;

    let attendee_status =
        AttendeeStatus::from_partstat(responding_partstat.as_deref().unwrap_or("NEEDS-ACTION"));

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
    let mut calendar = Calendar::new();
    calendar.append_property(Property::new("PRODID", "-//Exchange Gateway//EN"));
    calendar.append_property(Property::new("METHOD", "CANCEL"));

    let mut event = Event::new();
    event.append_property(Property::new("SEQUENCE", ctx.sequence.to_string()));
    event.uid(&ctx.uid);
    event.timestamp(Utc::now());
    event.status(EventStatus::Cancelled);
    if !item.subject.is_empty() {
        event.summary(&item.subject);
    }

    calendar.push(event.done());
    calendar.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_ical_datetime() {
        let dt = DateTime::parse_from_rfc3339("2024-01-15T10:30:00Z")
            .unwrap()
            .with_timezone(&Utc);
        assert_eq!(format_ical_datetime(dt), "20240115T103000Z");
    }
}
