// src/ews_update.rs
use crate::calendar::{
    extract_ews_field, extract_ews_fields, parse_ews_attendees, parse_ews_recurrence,
    CalendarItem,
};
use quick_xml::events::{BytesEnd, BytesStart, Event};
use quick_xml::Reader;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EwsFieldChange {
    pub verb: ChangeVerb,
    pub field_uri: String,
    pub payload_xml: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChangeVerb {
    Set,
    Append,
    Delete,
}

fn verb_from_local_name(local: &str) -> Option<ChangeVerb> {
    match local {
        "SetItemField" => Some(ChangeVerb::Set),
        "AppendToItemField" => Some(ChangeVerb::Append),
        "DeleteItemField" => Some(ChangeVerb::Delete),
        _ => None,
    }
}

fn xml_escape_text(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            _ => out.push(ch),
        }
    }
    out
}

fn xml_escape_attr(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            _ => out.push(ch),
        }
    }
    out
}

fn local_name_bytes(name: &[u8]) -> String {
    let local = name.rsplit(|b| *b == b':').next().unwrap_or(name);
    String::from_utf8_lossy(local).into_owned()
}

fn push_start_tag(out: &mut String, e: &BytesStart<'_>, decoder: quick_xml::encoding::Decoder) {
    out.push('<');
    out.push_str(&String::from_utf8_lossy(e.name().as_ref()));
    for attr in e.attributes().flatten() {
        out.push(' ');
        out.push_str(&String::from_utf8_lossy(attr.key.as_ref()));
        out.push_str("=\"");
        match attr.decode_and_unescape_value(decoder) {
            Ok(value) => out.push_str(&xml_escape_attr(&value)),
            Err(_) => out.push_str(&xml_escape_attr(&String::from_utf8_lossy(attr.value.as_ref()))),
        }
        out.push('"');
    }
    out.push('>');
}

fn push_empty_tag(out: &mut String, e: &BytesStart<'_>, decoder: quick_xml::encoding::Decoder) {
    out.push('<');
    out.push_str(&String::from_utf8_lossy(e.name().as_ref()));
    for attr in e.attributes().flatten() {
        out.push(' ');
        out.push_str(&String::from_utf8_lossy(attr.key.as_ref()));
        out.push_str("=\"");
        match attr.decode_and_unescape_value(decoder) {
            Ok(value) => out.push_str(&xml_escape_attr(&value)),
            Err(_) => out.push_str(&xml_escape_attr(&String::from_utf8_lossy(attr.value.as_ref()))),
        }
        out.push('"');
    }
    out.push_str("/>");
}

fn push_end_tag(out: &mut String, e: &BytesEnd<'_>) {
    out.push_str("</");
    out.push_str(&String::from_utf8_lossy(e.name().as_ref()));
    out.push('>');
}

fn first_ews_field(payload: &str, candidates: &[&[u8]]) -> Option<String> {
    candidates.iter().find_map(|name| extract_ews_field(payload, name))
}

fn first_ews_i32(payload: &str, candidates: &[&[u8]]) -> Option<i32> {
    first_ews_field(payload, candidates).and_then(|v| v.parse::<i32>().ok())
}

pub fn parse_item_changes(body: &str) -> Vec<EwsFieldChange> {
    let mut reader = Reader::from_str(body);
    reader.config_mut().trim_text(true);

    let mut buf = Vec::new();
    let mut results = Vec::new();

    enum State {
        Root,
        InVerb {
            verb: ChangeVerb,
            field_uri: Option<String>,
            payload_xml: String,
            collecting_payload: bool,
        },
    }

    let mut state = State::Root;

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(ref e)) => {
                let local = local_name_bytes(e.name().as_ref());
                match &mut state {
                    State::Root => {
                        if let Some(verb) = verb_from_local_name(&local) {
                            state = State::InVerb {
                                verb,
                                field_uri: None,
                                payload_xml: String::new(),
                                collecting_payload: false,
                            };
                        }
                    }
                    State::InVerb { field_uri, payload_xml, collecting_payload, .. } => {
                        if local == "FieldURI" && field_uri.is_none() {
                            for attr in e.attributes().flatten() {
                                if attr.key.local_name().as_ref() == b"FieldURI" {
                                    if let Ok(v) = attr.decode_and_unescape_value(reader.decoder()) {
                                        *field_uri = Some(v.to_string());
                                    }
                                }
                            }
                        } else if field_uri.is_some() {
                            *collecting_payload = true;
                            push_start_tag(payload_xml, e, reader.decoder());
                        }
                    }
                }
            }
            Ok(Event::Empty(ref e)) => {
                let local = local_name_bytes(e.name().as_ref());
                match &mut state {
                    State::Root => {}
                    State::InVerb { field_uri, payload_xml, collecting_payload, .. } => {
                        if local == "FieldURI" && field_uri.is_none() {
                            for attr in e.attributes().flatten() {
                                if attr.key.local_name().as_ref() == b"FieldURI" {
                                    if let Ok(v) = attr.decode_and_unescape_value(reader.decoder()) {
                                        *field_uri = Some(v.to_string());
                                    }
                                }
                            }
                        } else if field_uri.is_some() {
                            *collecting_payload = true;
                            push_empty_tag(payload_xml, e, reader.decoder());
                        }
                    }
                }
            }
            Ok(Event::End(ref e)) => {
                let local = local_name_bytes(e.name().as_ref());
                match &mut state {
                    State::Root => {}
                    State::InVerb { verb, field_uri, payload_xml, collecting_payload } => {
                        if matches!(local.as_str(), "SetItemField" | "AppendToItemField" | "DeleteItemField") {
                            if let Some(field_uri) = field_uri.take() {
                                results.push(EwsFieldChange {
                                    verb: *verb,
                                    field_uri,
                                    payload_xml: std::mem::take(payload_xml),
                                });
                            }
                            state = State::Root;
                        } else if *collecting_payload {
                            push_end_tag(payload_xml, e);
                        }
                    }
                }
            }
            Ok(Event::Text(t)) => {
                if let State::InVerb { collecting_payload: true, payload_xml, .. } = &mut state {
                    if let Ok(text) = t.decode() {
                        payload_xml.push_str(&xml_escape_text(&text));
                    }
                }
            }
            Ok(Event::CData(t)) => {
                if let State::InVerb { collecting_payload: true, payload_xml, .. } = &mut state {
                    if let Ok(text) = t.decode() {
                        payload_xml.push_str(&xml_escape_text(&text));
                    }
                }
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
        buf.clear();
    }

    results
}

pub fn apply_field_changes(item: &mut CalendarItem, changes: &[EwsFieldChange]) {
    for change in changes {
        let uri = change.field_uri.to_ascii_lowercase();
        let payload = &change.payload_xml;
        let verb = change.verb;

        match uri.as_str() {
            "item:subject" => match verb {
                ChangeVerb::Delete => item.subject.clear(),
                _ => {
                    if let Some(v) = first_ews_field(payload, &[b"Subject".as_ref(), b"Value".as_ref()]) {
                        item.subject = v;
                    }
                }
            },
            "item:body" => match verb {
                ChangeVerb::Delete => item.description.clear(),
                _ => {
                    if let Some(v) = first_ews_field(payload, &[b"Body".as_ref(), b"TextBody".as_ref(), b"Value".as_ref()]) {
                        item.description = v;
                    }
                }
            },
            "item:reminderisset" => match verb {
                ChangeVerb::Delete => item.reminder = None,
                _ => {
                    if let Some(v) = first_ews_field(payload, &[b"ReminderIsSet".as_ref()]) {
                        if v.eq_ignore_ascii_case("false") {
                            item.reminder = None;
                        }
                    }
                }
            },
            "item:reminderminutesbeforestart" => match verb {
                ChangeVerb::Delete => item.reminder = None,
                _ => {
                    if let Some(v) = first_ews_i32(payload, &[b"ReminderMinutesBeforeStart".as_ref(), b"Value".as_ref()]) {
                        item.reminder = Some(v);
                    }
                }
            },
            "item:sensitivity" => match verb {
                ChangeVerb::Delete => item.sensitivity = None,
                _ => {
                    if let Some(v) = first_ews_field(payload, &[b"Sensitivity".as_ref(), b"Value".as_ref()]) {
                        item.sensitivity = Some(match v.as_str() {
                            "Normal" => 0,
                            "Personal" => 1,
                            "Private" => 2,
                            "Confidential" => 3,
                            _ => 0,
                        });
                    }
                }
            },
            "item:categories" => {
                let cats = extract_ews_fields(payload, b"String");
                match verb {
                    ChangeVerb::Delete => item.categories.clear(),
                    ChangeVerb::Append => item.categories.extend(cats),
                    ChangeVerb::Set => item.categories = cats,
                }
            }
            "calendar:start" => match verb {
                ChangeVerb::Delete => {}
                _ => {
                    if let Some(v) = first_ews_field(payload, &[b"Start".as_ref(), b"Value".as_ref()])
                        .and_then(|s| crate::calendar::parse_datetime(&s))
                    {
                        item.start = v;
                    }
                }
            },
            "calendar:end" => match verb {
                ChangeVerb::Delete => {}
                _ => {
                    if let Some(v) = first_ews_field(payload, &[b"End".as_ref(), b"Value".as_ref()])
                        .and_then(|s| crate::calendar::parse_datetime(&s))
                    {
                        item.end = v;
                    }
                }
            },
            "calendar:isalldayevent" => match verb {
                ChangeVerb::Delete => item.all_day = false,
                _ => {
                    if let Some(v) = first_ews_field(payload, &[b"IsAllDayEvent".as_ref()]) {
                        item.all_day = v.eq_ignore_ascii_case("true");
                    }
                }
            },
            "calendar:location" => match verb {
                ChangeVerb::Delete => item.location.clear(),
                _ => {
                    if let Some(v) = first_ews_field(payload, &[b"Location".as_ref(), b"Value".as_ref()]) {
                        item.location = v;
                    }
                }
            },
            "calendar:legacyfreebusystatus" => match verb {
                ChangeVerb::Delete => item.busy_status = None,
                _ => {
                    if let Some(v) = first_ews_field(payload, &[b"LegacyFreeBusyStatus".as_ref(), b"Value".as_ref()]) {
                        item.busy_status = Some(match v.as_str() {
                            "Free" => 0,
                            "Tentative" => 1,
                            "Busy" => 2,
                            "OOF" => 3,
                            _ => 2,
                        });
                    }
                }
            },
            "calendar:recurrence" => match verb {
                ChangeVerb::Delete => item.rrule = None,
                _ => { item.rrule = parse_ews_recurrence(payload); }
            },
            "calendar:requiredattendees" | "calendar:optionalattendees" => {
                let attendees = parse_ews_attendees(payload);
                let is_optional = uri.contains("optional");
                match verb {
                    ChangeVerb::Delete => {
                        item.attendees.retain(|a| {
                            if is_optional { a.attendee_type != Some(2) } else { a.attendee_type == Some(2) }
                        });
                    }
                    ChangeVerb::Set => {
                        item.attendees.retain(|a| {
                            if is_optional { a.attendee_type != Some(2) } else { a.attendee_type == Some(2) }
                        });
                        item.attendees.extend(attendees);
                    }
                    ChangeVerb::Append => { item.attendees.extend(attendees); }
                }
            }
            "calendar:organizer" => match verb {
                ChangeVerb::Delete => {
                    item.organizer_name = None;
                    item.organizer_email = None;
                }
                _ => {
                    if let Some(v) = extract_ews_field(payload, b"EmailAddress") {
                        item.organizer_email = Some(v);
                    }
                    if let Some(v) = extract_ews_field(payload, b"Name") {
                        item.organizer_name = Some(v);
                    }
                }
            },
            "calendar:isresponserequested" | "calendar:responserequested" => match verb {
                ChangeVerb::Delete => item.response_requested = None,
                _ => {
                    if let Some(v) = first_ews_field(payload, &[b"IsResponseRequested".as_ref(), b"ResponseRequested".as_ref()]) {
                        item.response_requested = Some(v.eq_ignore_ascii_case("true"));
                    }
                }
            },
            "calendar:allownewtimeproposal" => match verb {
                ChangeVerb::Delete => item.disallow_new_time_proposal = None,
                _ => {
                    if let Some(v) = extract_ews_field(payload, b"AllowNewTimeProposal") {
                        item.disallow_new_time_proposal = Some(!v.eq_ignore_ascii_case("true"));
                    }
                }
            },
            "calendar:starttimezone" | "calendar:starttimezoneid" => match verb {
                ChangeVerb::Delete => item.timezone = None,
                _ => {
                    if let Some(v) = first_ews_field(payload, &[b"StartTimeZone".as_ref(), b"Value".as_ref()]) {
                        item.timezone = Some(v);
                    }
                }
            },
            "calendar:endtimezone" | "calendar:endtimezoneid" => {
                if verb != ChangeVerb::Delete {
                    if let Some(v) = first_ews_field(payload, &[b"EndTimeZone".as_ref(), b"Value".as_ref()]) {
                        if item.timezone.is_none() {
                            item.timezone = Some(v);
                        }
                    }
                }
            }
            "calendar:meetingtimezone" => match verb {
                ChangeVerb::Delete => item.timezone_blob = None,
                _ => {
                    if let Some(v) = first_ews_field(payload, &[b"MeetingTimeZone".as_ref(), b"Value".as_ref()]) {
                        item.timezone_blob = Some(v);
                    }
                }
            },
            "calendar:uid" => {}
            "calendar:appointmentreplytime" => match verb {
                ChangeVerb::Delete => item.appointment_reply_time = None,
                _ => {
                    if let Some(v) = first_ews_field(payload, &[b"AppointmentReplyTime".as_ref(), b"Value".as_ref()])
                        .and_then(|s| crate::calendar::parse_datetime(&s))
                    {
                        item.appointment_reply_time = Some(v);
                    }
                }
            },
            "calendar:onlinemeetingconflink" => match verb {
                ChangeVerb::Delete => item.online_meeting_conf_link = None,
                _ => { item.online_meeting_conf_link = first_ews_field(payload, &[b"OnlineMeetingConfLink".as_ref(), b"Value".as_ref()]); }
            },
            "calendar:onlinemeetingexternallink" => match verb {
                ChangeVerb::Delete => item.online_meeting_external_link = None,
                _ => { item.online_meeting_external_link = first_ews_field(payload, &[b"OnlineMeetingExternalLink".as_ref(), b"Value".as_ref()]); }
            },
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Timelike, Utc};

    fn base_item() -> CalendarItem {
        CalendarItem {
            uid: "uid-1".to_string(),
            subject: "Original Subject".to_string(),
            start: Utc::now(),
            end: Utc::now() + chrono::Duration::hours(1),
            ..Default::default()
        }
    }

    #[test]
    fn set_subject_via_field_uri() {
        let body = r#"<UpdateItem><ItemChanges><ItemChange>
            <SetItemField>
                <FieldURI FieldURI="item:Subject"/>
                <CalendarItem><Subject>New Subject</Subject></CalendarItem>
            </SetItemField>
        </ItemChange></ItemChanges></UpdateItem>"#;
        let changes = parse_item_changes(body);
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].field_uri, "item:Subject");
        assert_eq!(changes[0].verb, ChangeVerb::Set);
        let mut item = base_item();
        apply_field_changes(&mut item, &changes);
        assert_eq!(item.subject, "New Subject");
    }

    #[test]
    fn delete_reminder_via_field_uri() {
        let body = r#"<UpdateItem><ItemChanges><ItemChange>
            <DeleteItemField>
                <FieldURI FieldURI="item:ReminderMinutesBeforeStart"/>
            </DeleteItemField>
        </ItemChange></ItemChanges></UpdateItem>"#;
        let changes = parse_item_changes(body);
        assert_eq!(changes[0].verb, ChangeVerb::Delete);
        let mut item = base_item();
        item.reminder = Some(15);
        apply_field_changes(&mut item, &changes);
        assert!(item.reminder.is_none());
    }

    #[test]
    fn append_categories_via_field_uri() {
        let body = r#"<UpdateItem><ItemChanges><ItemChange>
            <AppendToItemField>
                <FieldURI FieldURI="item:Categories"/>
                <CalendarItem><Categories><String>Blue</String></Categories></CalendarItem>
            </AppendToItemField>
        </ItemChange></ItemChanges></UpdateItem>"#;
        let changes = parse_item_changes(body);
        assert_eq!(changes[0].verb, ChangeVerb::Append);
        let mut item = base_item();
        item.categories = vec!["Red".to_string()];
        apply_field_changes(&mut item, &changes);
        assert_eq!(item.categories, vec!["Red", "Blue"]);
    }

    #[test]
    fn set_location_via_field_uri() {
        let body = r#"<UpdateItem><ItemChanges><ItemChange>
            <SetItemField>
                <FieldURI FieldURI="calendar:Location"/>
                <CalendarItem><Location>Room B</Location></CalendarItem>
            </SetItemField>
        </ItemChange></ItemChanges></UpdateItem>"#;
        let changes = parse_item_changes(body);
        let mut item = base_item();
        apply_field_changes(&mut item, &changes);
        assert_eq!(item.location, "Room B");
    }

    #[test]
    fn set_free_busy_status() {
        let body = r#"<UpdateItem><ItemChanges><ItemChange>
            <SetItemField>
                <FieldURI FieldURI="calendar:LegacyFreeBusyStatus"/>
                <CalendarItem><LegacyFreeBusyStatus>Tentative</LegacyFreeBusyStatus></CalendarItem>
            </SetItemField>
        </ItemChange></ItemChanges></UpdateItem>"#;
        let changes = parse_item_changes(body);
        let mut item = base_item();
        apply_field_changes(&mut item, &changes);
        assert_eq!(item.busy_status, Some(1));
    }

    #[test]
    fn multiple_changes_applied_in_order() {
        let body = r#"<UpdateItem><ItemChanges><ItemChange>
            <SetItemField>
                <FieldURI FieldURI="item:Subject"/>
                <CalendarItem><Subject>A</Subject></CalendarItem>
            </SetItemField>
            <SetItemField>
                <FieldURI FieldURI="calendar:Location"/>
                <CalendarItem><Location>Conference Room</Location></CalendarItem>
            </SetItemField>
            <SetItemField>
                <FieldURI FieldURI="item:ReminderMinutesBeforeStart"/>
                <CalendarItem><ReminderMinutesBeforeStart>30</ReminderMinutesBeforeStart></CalendarItem>
            </SetItemField>
        </ItemChange></ItemChanges></UpdateItem>"#;
        let changes = parse_item_changes(body);
        assert_eq!(changes.len(), 3);
        let mut item = base_item();
        apply_field_changes(&mut item, &changes);
        assert_eq!(item.subject, "A");
        assert_eq!(item.location, "Conference Room");
        assert_eq!(item.reminder, Some(30));
    }

    #[test]
    fn set_start_and_end_times() {
        let body = r#"<UpdateItem><ItemChanges><ItemChange>
            <SetItemField>
                <FieldURI FieldURI="calendar:Start"/>
                <CalendarItem><Start>2026-03-25T10:00:00Z</Start></CalendarItem>
            </SetItemField>
            <SetItemField>
                <FieldURI FieldURI="calendar:End"/>
                <CalendarItem><End>2026-03-25T11:30:00Z</End></CalendarItem>
            </SetItemField>
        </ItemChange></ItemChanges></UpdateItem>"#;
        let changes = parse_item_changes(body);
        let mut item = base_item();
        apply_field_changes(&mut item, &changes);
        assert_eq!(item.start.hour(), 10);
        assert_eq!(item.end.hour(), 11);
    }

    #[test]
    fn set_sensitivity() {
        let body = r#"<UpdateItem><ItemChanges><ItemChange>
            <SetItemField>
                <FieldURI FieldURI="item:Sensitivity"/>
                <CalendarItem><Sensitivity>Private</Sensitivity></CalendarItem>
            </SetItemField>
        </ItemChange></ItemChanges></UpdateItem>"#;
        let changes = parse_item_changes(body);
        let mut item = base_item();
        apply_field_changes(&mut item, &changes);
        assert_eq!(item.sensitivity, Some(2));
    }
}
