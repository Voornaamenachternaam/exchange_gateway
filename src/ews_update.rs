// src/ews_update.rs
//
// EWS UpdateItem field-level patching engine.
//
// Gaps closed:
//   Gap 4.1 / EWS schema coverage — Implements the complete per-FieldURI
//   SetItemField / AppendToItemField / DeleteItemField dispatch table required by
//   [MS-OXWSCORE] and the EWS schema in Binder1. The previous implementation
//   only extracted fields by tag name from the raw request body, ignoring which
//   FieldURI was targeted and which change verb was used.
//
//   Per Binder1 §2.2.3.4 (t:FieldURI), all calendar:* and item:* FieldURI values
//   used by Outlook are now dispatched correctly:
//     - SetItemField  → overwrite the field on the existing item
//     - AppendToItemField → append (for multi-value fields like categories/attendees)
//     - DeleteItemField   → clear the field
//
//   This file is a pure logic module imported by src/ews.rs. It does not do I/O.

use crate::calendar::{
    CalendarItem, extract_ews_field, extract_ews_fields, parse_ews_attendees, parse_ews_recurrence,
};
use quick_xml::Reader;
use quick_xml::events::Event;

/// A single parsed item-change entry from an EWS UpdateItem request body.
#[derive(Clone, Debug)]
pub struct EwsFieldChange {
    pub verb: ChangeVerb,
    pub field_uri: String,
    pub payload_xml: String,
pub fn serde_json_string(s: &str) -> String { serde_json::to_string(s).expect("Failed to serialize string to JSON") }

/// Which of the three EWS update verbs was used.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ChangeVerb {
    Set,
    Append,
    Delete,
}

/// Parse every `SetItemField`, `AppendToItemField`, and `DeleteItemField` element
/// inside an EWS `UpdateItem` request body.
///
/// Each element carries either a `FieldURI/@FieldURI` attribute (for well-known
/// paths) or an `ExtendedFieldURI` — we only handle the former, which is
/// sufficient for all Outlook calendar operations.
pub fn parse_item_changes(body: &str) -> Vec<EwsFieldChange> {
    let mut reader = Reader::from_str(body);
    reader.config_mut().trim_text(true);
    let mut buf = Vec::new();
    let mut results = Vec::new();

    #[derive(PartialEq, Clone)]
    enum State {
        Root,
        InVerb(ChangeVerb),
        CollectPayload(ChangeVerb, String, usize),
    }

    let mut state = State::Root;
    let mut payload_buf = String::new();

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(ref e)) => {
                let local = String::from_utf8_lossy(e.name().local_name().as_ref()).to_string();
                match &state {
                    State::Root => {
                        if let Some(verb) = match local.as_str() {
                            "SetItemField" => Some(ChangeVerb::Set),
                            "AppendToItemField" => Some(ChangeVerb::Append),
                            "DeleteItemField" => Some(ChangeVerb::Delete),
                            _ => None,
                        } {
                            state = State::InVerb(verb);
                        }
                    }
                    State::InVerb(verb) => {
                        if local == "FieldURI" {
                            let mut field_uri = String::new();
                            for attr in e.attributes().flatten() {
                                if attr.key.local_name().as_ref() == b"FieldURI" {
                                    if let Ok(v) = attr.decode_and_unescape_value(reader.decoder()) {
                                        field_uri = v.to_string();
                                    }
                                }
                            }
                            if !field_uri.is_empty() {
                                state = State::CollectPayload(verb.clone(), field_uri, 1);
                                payload_buf.clear();
                            }
                        }
                    }
                    State::CollectPayload(verb, uri, depth) => {
                        let mut writer = quick_xml::writer::Writer::new(Vec::new());
                        let mut depth = 1;
                        while depth > 0 {
                            match reader.read_event_into(&mut buf) {
                                Ok(Event::Start(e)) => {
                                    writer.write_event(Event::Start(e.clone())).ok();
                                    depth += 1;
                                }
                                Ok(Event::End(e)) => {
                                    depth -= 1;
                                    if depth > 0 {
                                        writer.write_event(Event::End(e.clone())).ok();
                                    }
                                }
                                Ok(Event::Text(e)) => {
                                    writer.write_event(Event::Text(e.clone())).ok();
                                }
                                _ => {}
                            }
                            buf.clear();
                        }
                        payload_buf = String::from_utf8(writer.into_inner()).unwrap_or_default();
                    }
                    _ => {}
                }
            }
            Ok(Event::End(ref e)) => {
                let local = String::from_utf8_lossy(e.name().local_name().as_ref()).to_string();
                match &state {
                    State::CollectPayload(verb, uri, depth) => {
                        if *depth == 1 && matches!(local.as_str(), "SetItemField" | "AppendToItemField" | "DeleteItemField") {
                            results.push(EwsFieldChange { verb: verb.clone(), field_uri: uri.clone(), payload_xml: payload_buf.clone() });
                            state = State::Root;
                        } else {
                            payload_buf.push_str(&format!("</{}>", local));
                            state = State::CollectPayload(verb.clone(), uri.clone(), depth - 1);
                        }
                    }
                    State::InVerb(_) => {
                        if matches!(local.as_str(), "SetItemField" | "AppendToItemField" | "DeleteItemField") {
                            state = State::Root;
                        }
                    }
                    _ => {}
                }
            }
            Ok(Event::Text(t)) => {
                if let State::CollectPayload(_, _, _) = &state {
                    if let Ok(text) = t.decode() {
                        payload_buf.push_str(&text);
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
        CollectPayload(ChangeVerb, String, usize), // (verb, field_uri, depth)
    }

    let mut state = State::Root;
    let mut payload_buf = String::new();

96:                        }
97:                        // Handle self-closing FieldURI elements.
98:                        if local == "FieldURI" {
99:                            let mut field_uri_val = String::new();
100:                            for attr in e.attributes().flatten() {
101:                                if attr.key.local_name().as_ref() == b"FieldURI" {
102:                                    if let Ok(v) = attr.decode_and_unescape_value(reader.decoder())
103:                                    {
104:                                        field_uri_val = v.to_string();
105:                                    }
106:                                }
107:                            }
108:                            if !field_uri_val.is_empty() {
109:                                state = State::CollectPayload(verb.clone(), field_uri_val, 0);
110:                                payload_buf.clear();
111:                            }
112:                        }
113:                    }
114:                    Ok(Event::Empty(ref e)) => {
115:                        let local = String::from_utf8_lossy(e.name().local_name().as_ref()).to_string();
116:                        if let State::InVerb(verb) = &state {
117:                            if local == "FieldURI" {
118:                                let mut field_uri_val = String::new();
119:                                for attr in e.attributes().flatten() {
120:                                    if attr.key.local_name().as_ref() == b"FieldURI" {
121:                                        if let Ok(v) = attr.decode_and_unescape_value(reader.decoder())
122:                                        {
123:                                            field_uri_val = v.to_string();
124:                                        }
125:                                    }
126:                                }
127:                                if !field_uri_val.is_empty() {
128:                                    state = State::CollectPayload(verb.clone(), field_uri_val, 0);
129:                                    payload_buf.clear();
130:                                }
131:                            }
132:                        }
133:                    }
                        // Also handle IndexedFieldURI and ExtendedFieldURI — we skip
                        // those as Outlook calendar operations don't use them for
                        // calendar:* fields.
                    }
                    State::CollectPayload(_, _, depth) => {
                        // Collect all XML inside the change verb after FieldURI.
                        // We serialize back to a simple tag-value string for extraction.
                        payload_buf.push('<');
                        payload_buf.push_str(&local);
                        payload_buf.push('>');
                        // depth tracks nesting so we know when we're back at the verb level.
                        let _ = depth; // depth is managed in End branch
                    }
                }
            }
            Ok(Event::Text(t)) => {
                if let State::CollectPayload(_, _, _) = &state {
                    if let Ok(text) = t.decode() {
                        payload_buf.push_str(&text);
                    }
                }
            }
            Ok(Event::End(ref e)) => {
                let local = String::from_utf8_lossy(e.name().local_name().as_ref()).to_string();
                match &state {
                    State::InVerb(_) => {
                        if matches!(
                            local.as_str(),
                            "SetItemField" | "AppendToItemField" | "DeleteItemField"
                        ) {
                            state = State::Root;
                        }
                    }
                    State::CollectPayload(verb, field_uri, _) => {
                        if matches!(
                            local.as_str(),
                            "SetItemField" | "AppendToItemField" | "DeleteItemField"
                        ) {
                            results.push(EwsFieldChange {
                                verb: verb.clone(),
                                field_uri: field_uri.clone(),
                                payload_xml: payload_buf.clone(),
                            });
                            payload_buf.clear();
                            state = State::Root;
                        } else {
                            payload_buf.push_str("</");
                            payload_buf.push_str(&local);
                            payload_buf.push('>');
                        }
                    }
                    State::Root => {}
                }
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
        buf.clear();
    }

    results
}

/// Apply a slice of `EwsFieldChange` entries to a mutable `CalendarItem`.
///
/// Each entry is dispatched on `field_uri` (normalized to lower-case). The
/// full list of supported calendar:* and item:* FieldURI values is derived
/// from the Binder1 `t:UnindexedFieldURIType` enumeration.
pub fn apply_field_changes(item: &mut CalendarItem, changes: &[EwsFieldChange]) {
    for change in changes {
        let uri = change.field_uri.to_ascii_lowercase();
        let payload = &change.payload_xml;
        let verb = &change.verb;
        match uri.as_str() {
            // ── item:* ────────────────────────────────────────────────────
            "item:subject" => match verb {
                ChangeVerb::Delete => item.subject.clear(),
                _ => {
                    if let Some(v) = extract_ews_field(payload, b"Subject")
                        .or_else(|| extract_ews_field(payload, b"Value"))
                    {
                        item.subject = v;
                    }
                }
            },
            "item:body" => match verb {
                ChangeVerb::Delete => item.description.clear(),
                _ => {
                    if let Some(v) = extract_ews_field(payload, b"Body")
                        .or_else(|| extract_ews_field(payload, b"TextBody"))
                        .or_else(|| extract_ews_field(payload, b"Value"))
                    {
                        item.description = v;
                    }
                }
            },
            "item:reminderisset" => match verb {
                ChangeVerb::Delete => item.reminder = None,
                _ => {
                    if let Some(v) = extract_ews_field(payload, b"ReminderIsSet") {
                        if v.eq_ignore_ascii_case("false") {
                            item.reminder = None;
                        }
                    }
                }
            },
            "item:reminderminutesbeforestart" => match verb {
                ChangeVerb::Delete => item.reminder = None,
                _ => {
                    if let Some(v) = extract_ews_field(payload, b"ReminderMinutesBeforeStart")
                        .or_else(|| extract_ews_field(payload, b"Value"))
                        .and_then(|s| s.parse().ok())
                    {
                        item.reminder = Some(v);
                    }
                }
            },
            "item:sensitivity" => match verb {
                ChangeVerb::Delete => item.sensitivity = None,
                _ => {
                    if let Some(v) = extract_ews_field(payload, b"Sensitivity")
                        .or_else(|| extract_ews_field(payload, b"Value"))
                    {
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
            // ── calendar:* ────────────────────────────────────────────────
            "calendar:start" => match verb {
                ChangeVerb::Delete => {} // cannot delete Start
                _ => {
                    if let Some(v) = extract_ews_field(payload, b"Start")
                        .or_else(|| extract_ews_field(payload, b"Value"))
                        .and_then(|s| crate::calendar::parse_datetime(&s))
                    {
                        item.start = v;
                    }
                }
            },
            "calendar:end" => match verb {
                ChangeVerb::Delete => {} // cannot delete End
                _ => {
                    if let Some(v) = extract_ews_field(payload, b"End")
                        .or_else(|| extract_ews_field(payload, b"Value"))
                        .and_then(|s| crate::calendar::parse_datetime(&s))
                    {
                        item.end = v;
                    }
                }
            },
            "calendar:isalldayevent" => match verb {
                ChangeVerb::Delete => item.all_day = false,
                _ => {
                    if let Some(v) = extract_ews_field(payload, b"IsAllDayEvent") {
                        item.all_day = v.eq_ignore_ascii_case("true");
                    }
                }
            },
            "calendar:location" => match verb {
                ChangeVerb::Delete => item.location.clear(),
                _ => {
                    if let Some(v) = extract_ews_field(payload, b"Location")
                        .or_else(|| extract_ews_field(payload, b"Value"))
                    {
                        item.location = v;
                    }
                }
            },
            "calendar:legacyfreebusystatus" => match verb {
                ChangeVerb::Delete => item.busy_status = None,
                _ => {
                    if let Some(v) = extract_ews_field(payload, b"LegacyFreeBusyStatus")
                        .or_else(|| extract_ews_field(payload, b"Value"))
                    {
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
            "calendar:location" => match verb {
                ChangeVerb::Delete => item.location.clear(),
                _ => {
                    if let Some(v) = extract_ews_field(payload, b"Location")
                        .or_else(|| extract_ews_field(payload, b"Value"))
                    {
                        item.location = v;
                    }
                }
            },
            "calendar:legacyfreebusystatus" => match verb {
                ChangeVerb::Delete => item.busy_status = None,
                _ => {
                    if let Some(v) = extract_ews_field(payload, b"LegacyFreeBusyStatus")
                        .or_else(|| extract_ews_field(payload, b"Value"))
                    {
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
                _ => {
                    item.rrule = parse_ews_recurrence(payload);
                }
            },
            "calendar:requiredattendees" | "calendar:optionalattendees" => {
                let attendees = parse_ews_attendees(payload);
                match verb {
                    ChangeVerb::Delete => {
                        let is_optional = uri.contains("optional");
                        item.attendees.retain(|a| {
                            if is_optional {
                                a.attendee_type != Some(2)
                            } else {
                                a.attendee_type == Some(2)
                            }
                        });
                    }
                    ChangeVerb::Append => {
                        item.attendees.extend(attendees);
                    }
                    ChangeVerb::Set => {
                        let is_optional = uri.contains("optional");
                        item.attendees.retain(|a| {
                            if is_optional {
                                a.attendee_type != Some(2)
                            } else {
                                a.attendee_type == Some(2)
                            }
                        });
                        item.attendees.extend(attendees);
                    }
                }
            }
441:                ChangeVerb::Delete => item.rrule = None,
442:                _ => {
443:                    item.rrule = parse_ews_recurrence(payload);
444:                }
445:            },
            "calendar:requiredattendees"
            | "calendar:optionalattendees" => {
                let attendees = parse_ews_attendees(payload);
                match verb {
                    ChangeVerb::Delete => {
                        // Remove this class of attendees.
                        let is_optional = uri.contains("optional");
                        item.attendees.retain(|a| {
                            if is_optional {
                                a.attendee_type != Some(2)
                            } else {
                                a.attendee_type == Some(2)
                            }
                        });
                    }
                    ChangeVerb::Append => {
                        item.attendees.extend(attendees);
                    }
                    ChangeVerb::Set => {
                        // Replace only this class of attendees.
                        let is_optional = uri.contains("optional");
                        item.attendees.retain(|a| {
                            if is_optional {
                                a.attendee_type != Some(2)
                            } else {
                                a.attendee_type == Some(2)
                            }
                        });
                        item.attendees.extend(attendees);
                    }
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
                    if let Some(v) = extract_ews_field(payload, b"IsResponseRequested")
                        .or_else(|| extract_ews_field(payload, b"ResponseRequested"))
                    {
                        item.response_requested = Some(v.eq_ignore_ascii_case("true"));
                    }
                }
            },
            "calendar:allownewtimeproposal" => match verb {
                ChangeVerb::Delete => item.disallow_new_time_proposal = None,
                _ => {
                    if let Some(v) = extract_ews_field(payload, b"AllowNewTimeProposal") {
                        // AllowNewTimeProposal is the inverse of DisallowNewTimeProposal.
                        item.disallow_new_time_proposal =
                            Some(!v.eq_ignore_ascii_case("true"));
                    }
                }
            },
            "calendar:starttimezone" | "calendar:starttimezoneid" => match verb {
                ChangeVerb::Delete => item.timezone = None,
                _ => {
                    if let Some(v) = extract_ews_field(payload, b"StartTimeZone")
                        .or_else(|| extract_ews_field(payload, b"Value"))
                    {
                        item.timezone = Some(v);
                    }
                }
            },
            "calendar:endtimezone" | "calendar:endtimezoneid" => {
                // EndTimeZone — only update if different from StartTimeZone.
                // For simplicity we keep a single timezone field.
                if verb != &ChangeVerb::Delete {
                    if let Some(v) = extract_ews_field(payload, b"EndTimeZone")
                        .or_else(|| extract_ews_field(payload, b"Value"))
                    {
                        if item.timezone.is_none() {
                            item.timezone = Some(v);
                        }
                    }
                }
            }
            "calendar:meetingtimezone" => match verb {
                ChangeVerb::Delete => item.timezone_blob = None,
                _ => {
                    if let Some(v) = extract_ews_field(payload, b"MeetingTimeZone")
                        .or_else(|| extract_ews_field(payload, b"Value"))
                    {
                        item.timezone_blob = Some(v);
                    }
                }
            },
            "calendar:uid" => {
                // UID is immutable once set; ignore set/delete.
            }
            "calendar:appointmentreplytime" => match verb {
                ChangeVerb::Delete => item.appointment_reply_time = None,
                _ => {
                    if let Some(v) = extract_ews_field(payload, b"AppointmentReplyTime")
                        .or_else(|| extract_ews_field(payload, b"Value"))
                        .and_then(|s| crate::calendar::parse_datetime(&s))
                    {
                        item.appointment_reply_time = Some(v);
                    }
                }
            },
            "calendar:onlinemeetingconflink" => match verb {
                ChangeVerb::Delete => item.online_meeting_conf_link = None,
                _ => {
                    item.online_meeting_conf_link =
                        extract_ews_field(payload, b"OnlineMeetingConfLink")
                            .or_else(|| extract_ews_field(payload, b"Value"));
                }
            },
            "calendar:onlinemeetingexternallink" => match verb {
                ChangeVerb::Delete => item.online_meeting_external_link = None,
                _ => {
                    item.online_meeting_external_link =
                        extract_ews_field(payload, b"OnlineMeetingExternalLink")
                            .or_else(|| extract_ews_field(payload, b"Value"));
                }
            },
            // Any other FieldURI is silently ignored — forward compatibility.
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::calendar::CalendarItem;
    use chrono::Utc;

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
    fn set_is_all_day_event() {
        let body = r#"<UpdateItem><ItemChanges><ItemChange>
            <SetItemField>
                <FieldURI FieldURI="calendar:IsAllDayEvent"/>
                <CalendarItem><IsAllDayEvent>true</IsAllDayEvent></CalendarItem>
            </SetItemField>
        </ItemChange></ItemChanges></UpdateItem>"#;
        let changes = parse_item_changes(body);
        let mut item = base_item();
        apply_field_changes(&mut item, &changes);
        assert!(item.all_day);
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
