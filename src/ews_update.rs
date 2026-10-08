// src/ews_update.rs
use crate::calendar::{
    CalendarItem, EwsRequestZone, extract_ews_field, extract_ews_fields,
    extract_ews_timezone_field, normalize_timezone_to_iana, parse_datetime_in_zone,
    parse_ews_attendees, parse_ews_recurrence,
};
use crate::util::{nfc, xml_escape, xml_escape_text};
use quick_xml::Reader;
use quick_xml::XmlVersion;
use quick_xml::events::{BytesEnd, BytesStart, Event};

/// Why an `UpdateItem` change set could not be applied.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EwsUpdateError {
    /// The request supplied a timezone id that resolves to no known zone,
    /// and the change set carries a naive (offset-less) `Start`/`End`
    /// value: no honest instant exists for it, so the update fails closed
    /// instead of silently drifting the meeting by the zone's offset.
    TimezoneUnresolved,
}

/// Parse a `calendar:start`/`calendar:end` `Value` against the request's
/// tri-state zone. `Absent` reads naive values as UTC; `Resolved` localizes
/// them in the zone; `Unresolved` rejects a NAIVE value outright
/// (`Err(EwsUpdateError::TimezoneUnresolved)`) while explicit-offset
/// values stay readable everywhere.
pub(crate) fn parse_request_zone_datetime(
    val: &str,
    zone: EwsRequestZone,
) -> Result<Option<chrono::DateTime<chrono::Utc>>, EwsUpdateError> {
    match zone {
        EwsRequestZone::Absent => Ok(parse_datetime_in_zone(val, None)),
        EwsRequestZone::Resolved(tz) => Ok(parse_datetime_in_zone(val, Some(tz))),
        EwsRequestZone::Unresolved => {
            if crate::calendar::datetime_is_naive(val) && val.contains('T') {
                return Err(EwsUpdateError::TimezoneUnresolved);
            }
            Ok(parse_datetime_in_zone(val, None))
        }
    }
}

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

fn local_name_bytes(name: &str) -> String {
    name.rsplit(':').next().unwrap_or(name).to_string()
}

fn push_start_tag(out: &mut String, e: &BytesStart<'_>) {
    out.push('<');
    out.push_str(e.name().as_ref());
    for attr in e.attributes().flatten() {
        out.push(' ');
        out.push_str(attr.key.as_ref());
        out.push_str("=\"");
        match attr.normalized_value(XmlVersion::Implicit1_0) {
            Ok(value) => out.push_str(&xml_escape(&value)),
            Err(_) => out.push_str(&xml_escape(&attr.value)),
        }
        out.push('"');
    }
    out.push('>');
}

fn push_empty_tag(out: &mut String, e: &BytesStart<'_>) {
    out.push('<');
    out.push_str(e.name().as_ref());
    for attr in e.attributes().flatten() {
        out.push(' ');
        out.push_str(attr.key.as_ref());
        out.push_str("=\"");
        match attr.normalized_value(XmlVersion::Implicit1_0) {
            Ok(value) => out.push_str(&xml_escape(&value)),
            Err(_) => out.push_str(&xml_escape(&attr.value)),
        }
        out.push('"');
    }
    out.push_str("/>");
}

fn push_end_tag(out: &mut String, e: &BytesEnd<'_>) {
    out.push_str("</");
    out.push_str(e.name().as_ref());
    out.push('>');
}

fn first_ews_field(payload: &str, candidates: &[&[u8]]) -> Option<String> {
    candidates
        .iter()
        .find_map(|name| extract_ews_field(payload, name))
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

    /// Extract the FieldURI from a FieldURI or IndexedFieldURI element.
    /// For FieldURI: returns the "FieldURI" attribute value.
    /// For IndexedFieldURI: returns "FieldURI:FieldIndex" format.
    /// For ExtendedFieldURI: returns "PropertyTag:PropertyId" or "DistinguishedPropertySetId:PropertyId".
    fn extract_field_uri_from_element(
        e: &quick_xml::events::BytesStart<'_>,
        local: &str,
    ) -> Option<String> {
        match local {
            "FieldURI" => {
                // <t:FieldURI FieldURI="calendar:Start" />
                e.attributes().flatten().find_map(|attr| {
                    if attr.key.local_name().as_ref() == "FieldURI" {
                        attr.normalized_value(XmlVersion::Implicit1_0)
                            .ok()
                            .map(|v| v.to_string())
                    } else {
                        None
                    }
                })
            }
            "IndexedFieldURI" => {
                // <t:IndexedFieldURI FieldURI="contacts:EmailAddress" FieldIndex="EmailAddress1" />
                let mut field_uri = None;
                let mut field_index = None;
                for attr in e.attributes().flatten() {
                    let key = attr.key.local_name();
                    if key.as_ref() == "FieldURI" {
                        if let Ok(v) = attr.normalized_value(XmlVersion::Implicit1_0) {
                            field_uri = Some(v.to_string());
                        }
                    } else if key.as_ref() == "FieldIndex"
                        && let Ok(v) = attr.normalized_value(XmlVersion::Implicit1_0)
                    {
                        field_index = Some(v.to_string());
                    }
                }
                match (field_uri, field_index) {
                    (Some(uri), Some(idx)) => Some(format!("{}:{}", uri, idx)),
                    (Some(uri), None) => Some(uri),
                    _ => None,
                }
            }
            "ExtendedFieldURI" => {
                // <t:ExtendedFieldURI PropertyTag="0x001A" PropertyType="String" />
                // or <t:ExtendedFieldURI DistinguishedPropertySetId="Appointment" PropertyId="AppointmentCounter" PropertyType="Integer" />
                let mut tag = None;
                let mut prop_id = None;
                let mut dist_prop_set = None;
                for attr in e.attributes().flatten() {
                    let key = attr.key.local_name();
                    if key.as_ref() == "PropertyTag" {
                        if let Ok(v) = attr.normalized_value(XmlVersion::Implicit1_0) {
                            tag = Some(v.to_string());
                        }
                    } else if key.as_ref() == "PropertyId" {
                        if let Ok(v) = attr.normalized_value(XmlVersion::Implicit1_0) {
                            prop_id = Some(v.to_string());
                        }
                    } else if key.as_ref() == "DistinguishedPropertySetId"
                        && let Ok(v) = attr.normalized_value(XmlVersion::Implicit1_0)
                    {
                        dist_prop_set = Some(v.to_string());
                    }
                }
                // Build a canonical key for the ExtendedFieldURI
                if let Some(t) = tag {
                    Some(format!("extended:{}", t))
                } else if let Some(p) = prop_id {
                    let prefix = dist_prop_set.as_deref().unwrap_or("unknown");
                    Some(format!("extended:{}:{}", prefix, p))
                } else {
                    None
                }
            }
            _ => None,
        }
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
                    State::InVerb {
                        field_uri,
                        payload_xml,
                        collecting_payload,
                        ..
                    } => {
                        if field_uri.is_none()
                            && matches!(
                                local.as_str(),
                                "FieldURI" | "IndexedFieldURI" | "ExtendedFieldURI"
                            )
                        {
                            *field_uri = extract_field_uri_from_element(e, &local);
                        } else if field_uri.is_some() {
                            *collecting_payload = true;
                            push_start_tag(payload_xml, e);
                        }
                    }
                }
            }
            Ok(Event::Empty(ref e)) => {
                let local = local_name_bytes(e.name().as_ref());
                match &mut state {
                    State::Root => {}
                    State::InVerb {
                        field_uri,
                        payload_xml,
                        collecting_payload,
                        ..
                    } => {
                        if field_uri.is_none()
                            && matches!(
                                local.as_str(),
                                "FieldURI" | "IndexedFieldURI" | "ExtendedFieldURI"
                            )
                        {
                            *field_uri = extract_field_uri_from_element(e, &local);
                        } else if field_uri.is_some() {
                            *collecting_payload = true;
                            push_empty_tag(payload_xml, e);
                        }
                    }
                }
            }
            Ok(Event::End(ref e)) => {
                let local = local_name_bytes(e.name().as_ref());
                match &mut state {
                    State::Root => {}
                    State::InVerb {
                        verb,
                        field_uri,
                        payload_xml,
                        collecting_payload,
                    } => {
                        if matches!(
                            local.as_str(),
                            "SetItemField" | "AppendToItemField" | "DeleteItemField"
                        ) {
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
                if let State::InVerb {
                    collecting_payload: true,
                    payload_xml,
                    ..
                } = &mut state
                {
                    payload_xml.push_str(&xml_escape_text(t.as_ref()));
                }
            }
            Ok(Event::CData(t)) => {
                if let State::InVerb {
                    collecting_payload: true,
                    payload_xml,
                    ..
                } = &mut state
                {
                    payload_xml.push_str(&xml_escape_text(t.as_ref()));
                }
            }
            Ok(Event::GeneralRef(r)) => {
                if let State::InVerb {
                    collecting_payload: true,
                    payload_xml,
                    ..
                } = &mut state
                {
                    // Reconstruct the entity reference verbatim rather than
                    // re-escaping it, which would double-escape the text.
                    payload_xml.push('&');
                    payload_xml.push_str(r.as_ref());
                    payload_xml.push(';');
                }
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
        buf.clear();
    }

    results
}

/// Apply parsed `SetItemField`/`AppendToItemField`/`DeleteItemField` changes
/// to a calendar item. `zone` is the request's resolved timezone
/// ([`crate::calendar::ews_update_request_zone`]): naive `calendar:start`/
/// `calendar:end` values are localized in it, while values with an explicit
/// `Z`/offset keep their own instant regardless. A zone the request SUPPLIED
/// but that does not resolve makes a naive `Start`/`End` value
/// uninterpretable — the call fails with
/// [`EwsUpdateError::TimezoneUnresolved`] (no field is applied) rather than
/// silently reading the value as UTC.
pub fn apply_field_changes(
    item: &mut CalendarItem,
    changes: &[EwsFieldChange],
    zone: EwsRequestZone,
) -> Result<(), EwsUpdateError> {
    for change in changes {
        let uri = change.field_uri.to_ascii_lowercase();
        let payload = &change.payload_xml;
        let verb = change.verb;

        match uri.as_str() {
            "item:subject" => match verb {
                ChangeVerb::Delete => item.subject.clear(),
                _ => {
                    if let Some(v) =
                        first_ews_field(payload, &[b"Subject".as_ref(), b"Value".as_ref()])
                    {
                        item.subject = v;
                    }
                }
            },
            "item:body" => match verb {
                ChangeVerb::Delete => item.description.clear(),
                _ => {
                    if let Some(v) = first_ews_field(
                        payload,
                        &[b"Body".as_ref(), b"TextBody".as_ref(), b"Value".as_ref()],
                    ) {
                        item.description = v;
                    }
                }
            },
            "item:reminderisset" => match verb {
                ChangeVerb::Delete => item.reminder = None,
                _ => {
                    if let Some(v) = first_ews_field(payload, &[b"ReminderIsSet".as_ref()])
                        && v.eq_ignore_ascii_case("false")
                    {
                        item.reminder = None;
                    }
                }
            },
            "item:reminderminutesbeforestart" => match verb {
                ChangeVerb::Delete => item.reminder = None,
                _ => {
                    if let Some(v) = first_ews_i32(
                        payload,
                        &[b"ReminderMinutesBeforeStart".as_ref(), b"Value".as_ref()],
                    ) {
                        item.reminder = Some(v);
                    }
                }
            },
            "item:sensitivity" => match verb {
                ChangeVerb::Delete => item.sensitivity = None,
                _ => {
                    if let Some(v) =
                        first_ews_field(payload, &[b"Sensitivity".as_ref(), b"Value".as_ref()])
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
            "calendar:start" => match verb {
                ChangeVerb::Delete => {}
                _ => {
                    if let Some(s) =
                        first_ews_field(payload, &[b"Start".as_ref(), b"Value".as_ref()])
                    {
                        match parse_request_zone_datetime(&s, zone) {
                            Ok(Some(v)) => item.start = v,
                            Ok(None) => {}
                            Err(e) => return Err(e),
                        }
                    }
                }
            },
            "calendar:end" => match verb {
                ChangeVerb::Delete => {}
                _ => {
                    if let Some(s) = first_ews_field(payload, &[b"End".as_ref(), b"Value".as_ref()])
                    {
                        match parse_request_zone_datetime(&s, zone) {
                            Ok(Some(v)) => item.end = v,
                            Ok(None) => {}
                            Err(e) => return Err(e),
                        }
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
                    if let Some(v) =
                        first_ews_field(payload, &[b"Location".as_ref(), b"Value".as_ref()])
                    {
                        item.location = v;
                    }
                }
            },
            "calendar:legacyfreebusystatus" => match verb {
                ChangeVerb::Delete => item.busy_status = None,
                _ => {
                    if let Some(v) = first_ews_field(
                        payload,
                        &[b"LegacyFreeBusyStatus".as_ref(), b"Value".as_ref()],
                    ) {
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
                let is_optional = uri.contains("optional");
                match verb {
                    ChangeVerb::Delete => {
                        item.attendees.retain(|a| {
                            if is_optional {
                                a.attendee_type != Some(2)
                            } else {
                                a.attendee_type == Some(2)
                            }
                        });
                    }
                    ChangeVerb::Set => {
                        item.attendees.retain(|a| {
                            if is_optional {
                                a.attendee_type != Some(2)
                            } else {
                                a.attendee_type == Some(2)
                            }
                        });
                        item.attendees.extend(attendees);
                    }
                    ChangeVerb::Append => {
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
                        item.organizer_email = Some(nfc(&v));
                    }
                    if let Some(v) = extract_ews_field(payload, b"Name") {
                        item.organizer_name = Some(v);
                    }
                }
            },
            "calendar:isresponserequested" | "calendar:responserequested" => match verb {
                ChangeVerb::Delete => item.response_requested = None,
                _ => {
                    if let Some(v) = first_ews_field(
                        payload,
                        &[
                            b"IsResponseRequested".as_ref(),
                            b"ResponseRequested".as_ref(),
                        ],
                    ) {
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
                    // Both Outlook wire shapes: the `t:StartTimeZoneId` element
                    // (zone id as element TEXT, [MS-OXWSMTGS] §2.2.2.41) and
                    // the `t:StartTimeZone` element (zone id in the `Id`
                    // ATTRIBUTE, [MS-OXWSCDATA] §2.2.4.18). A text-only
                    // local-name lookup finds NEITHER, silently dropping the
                    // event's timezone change.
                    if let Some(raw) = extract_ews_field(payload, b"StartTimeZoneId")
                        .or_else(|| extract_ews_timezone_field(payload, b"StartTimeZone"))
                    {
                        item.timezone = Some(normalize_timezone_to_iana(&raw));
                    }
                }
            },
            "calendar:endtimezone" | "calendar:endtimezoneid" => {
                if verb != ChangeVerb::Delete
                    && item.timezone.is_none()
                    && let Some(raw) = extract_ews_field(payload, b"EndTimeZoneId")
                        .or_else(|| extract_ews_timezone_field(payload, b"EndTimeZone"))
                {
                    item.timezone = Some(normalize_timezone_to_iana(&raw));
                }
            }
            "calendar:meetingtimezone" => match verb {
                ChangeVerb::Delete => item.timezone_blob = None,
                _ => {
                    // `t:MeetingTimeZone` carries the id in its `TimeZoneName`
                    // attribute ([MS-OXWSCDATA] §2.2.4.11).
                    if let Some(raw) = extract_ews_timezone_field(payload, b"MeetingTimeZone") {
                        item.timezone_blob = Some(raw.clone());
                        item.timezone = Some(normalize_timezone_to_iana(&raw));
                    }
                }
            },
            "calendar:uid" => {}
            "calendar:appointmentreplytime" => match verb {
                ChangeVerb::Delete => item.appointment_reply_time = None,
                _ => {
                    if let Some(v) = first_ews_field(
                        payload,
                        &[b"AppointmentReplyTime".as_ref(), b"Value".as_ref()],
                    )
                    .and_then(|s| crate::calendar::parse_datetime(&s))
                    {
                        item.appointment_reply_time = Some(v);
                    }
                }
            },
            "calendar:onlinemeetingconflink" => match verb {
                ChangeVerb::Delete => item.online_meeting_conf_link = None,
                _ => {
                    item.online_meeting_conf_link = first_ews_field(
                        payload,
                        &[b"OnlineMeetingConfLink".as_ref(), b"Value".as_ref()],
                    );
                }
            },
            "calendar:onlinemeetingexternallink" => match verb {
                ChangeVerb::Delete => item.online_meeting_external_link = None,
                _ => {
                    item.online_meeting_external_link = first_ews_field(
                        payload,
                        &[b"OnlineMeetingExternalLink".as_ref(), b"Value".as_ref()],
                    );
                }
            },
            // IndexedFieldURI patterns: e.g. "contacts:EmailAddress:EmailAddress1"
            // These are used by OneCalendar and other EWS clients for contact-like fields
            // embedded in calendar items. We handle the common ones gracefully.
            uri if uri.starts_with("contacts:") || uri.starts_with("message:") => {
                // Silently ignore contact/message field changes in calendar items
                // (not applicable to CalDAV VEVENT)
                tracing::debug!(field_uri = %change.field_uri, "Ignoring non-calendar IndexedFieldURI change");
            }
            uri if uri.starts_with("extended:") => {
                // ExtendedFieldURI: used for MAPI extended properties.
                // Common ones like reminder offset, appointment color, etc.
                tracing::debug!(field_uri = %change.field_uri, "Ignoring ExtendedFieldURI change (not mappable to CalDAV)");
            }
            _ => {
                tracing::debug!(field_uri = %change.field_uri, "Unrecognized FieldURI in UpdateItem; ignoring");
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base_item() -> CalendarItem {
        CalendarItem {
            uid: "update-tz@example.com".to_string(),
            subject: "Before update".to_string(),
            start: crate::calendar::parse_datetime("2025-01-15T12:00:00Z").unwrap(),
            end: crate::calendar::parse_datetime("2025-01-15T13:00:00Z").unwrap(),
            ..Default::default()
        }
    }

    /// `calendar:start`/`calendar:end` values are offset-less xs:dateTime when
    /// Outlook writes them with a request-wide `TimeZoneContext`; the update
    /// must localize them in the request's zone, not read them as UTC (the
    /// §14 offset-drift failure).
    #[test]
    fn apply_field_changes_localizes_naive_start_end_in_request_zone() {
        let body = concat!(
            r#"<m:UpdateItem xmlns:m="http://schemas.microsoft.com/exchange/services/2006/messages""#,
            r#" xmlns:t="http://schemas.microsoft.com/exchange/services/2006/types">"#,
            r#"<m:ItemChanges><t:ItemChange><t:Updates>"#,
            r#"<t:SetItemField><t:FieldURI FieldURI="calendar:Start"/>"#,
            r#"<t:CalendarItem><t:Start>2025-01-15T09:00:00</t:Start></t:CalendarItem>"#,
            r#"</t:SetItemField>"#,
            r#"<t:SetItemField><t:FieldURI FieldURI="calendar:End"/>"#,
            r#"<t:CalendarItem><t:End>2025-01-15T10:00:00</t:End></t:CalendarItem>"#,
            r#"</t:SetItemField>"#,
            r#"</t:Updates></t:ItemChange></m:ItemChanges></m:UpdateItem>"#
        );

        let changes = parse_item_changes(body);
        assert_eq!(changes.len(), 2, "both SetItemField changes parse");

        // 09:00/10:00 wall clock in Berlin (CET, +01:00 in January).
        let zone = EwsRequestZone::Resolved("Europe/Berlin".parse::<chrono_tz::Tz>().unwrap());
        let mut item = base_item();
        apply_field_changes(&mut item, &changes, zone).unwrap();
        assert_eq!(
            item.start,
            crate::calendar::parse_datetime("2025-01-15T08:00:00Z").unwrap(),
            "naive Start must localize in the request zone"
        );
        assert_eq!(
            item.end,
            crate::calendar::parse_datetime("2025-01-15T09:00:00Z").unwrap()
        );

        // No request zone at all: a naive value reads as UTC.
        let mut item = base_item();
        apply_field_changes(&mut item, &changes, EwsRequestZone::Absent).unwrap();
        assert_eq!(
            item.start,
            crate::calendar::parse_datetime("2025-01-15T09:00:00Z").unwrap()
        );
        assert_eq!(
            item.end,
            crate::calendar::parse_datetime("2025-01-15T10:00:00Z").unwrap()
        );

        // An explicit-`Z` value keeps its own instant regardless of the zone.
        let body_z = body
            .replace("09:00:00<", "09:00:00Z<")
            .replace("10:00:00<", "10:00:00Z<");
        let changes_z = parse_item_changes(&body_z);
        let mut item = base_item();
        apply_field_changes(&mut item, &changes_z, zone).unwrap();
        assert_eq!(
            item.start,
            crate::calendar::parse_datetime("2025-01-15T09:00:00Z").unwrap()
        );
    }

    /// A zone the request SUPPLIED but that resolves to nothing leaves a naive
    /// `Start`/`End` with no honest instant: the change set must fail closed
    /// (and leave the item untouched) instead of silently reading UTC.
    #[test]
    fn apply_field_changes_fails_closed_on_supplied_but_unresolved_zone() {
        let body = concat!(
            r#"<m:UpdateItem xmlns:m="http://schemas.microsoft.com/exchange/services/2006/messages""#,
            r#" xmlns:t="http://schemas.microsoft.com/exchange/services/2006/types">"#,
            r#"<m:ItemChanges><t:ItemChange><t:Updates>"#,
            r#"<t:SetItemField><t:FieldURI FieldURI="calendar:Start"/>"#,
            r#"<t:CalendarItem><t:Start>2025-01-15T09:00:00</t:Start></t:CalendarItem>"#,
            r#"</t:SetItemField>"#,
            r#"</t:Updates></t:ItemChange></m:ItemChanges></m:UpdateItem>"#
        );

        assert_eq!(
            crate::calendar::ews_update_request_zone(body),
            EwsRequestZone::Absent,
            "no zone in the request at all"
        );

        // Unresolvable zone id supplied via the SOAP TimeZoneContext header.
        let body_ctx = concat!(
            r#"<m:UpdateItem xmlns:m="http://schemas.microsoft.com/exchange/services/2006/messages""#,
            r#" xmlns:t="http://schemas.microsoft.com/exchange/services/2006/types">"#,
            r#"<m:TimeZoneContext><t:TimeZoneDefinition Id="Mars/Standard Time"/></m:TimeZoneContext>"#,
            r#"<m:ItemChanges><t:ItemChange><t:Updates>"#,
            r#"<t:SetItemField><t:FieldURI FieldURI="calendar:Start"/>"#,
            r#"<t:CalendarItem><t:Start>2025-01-15T09:00:00</t:Start></t:CalendarItem>"#,
            r#"</t:SetItemField>"#,
            r#"</t:Updates></t:ItemChange></m:ItemChanges></m:UpdateItem>"#
        );
        assert_eq!(
            crate::calendar::ews_update_request_zone(body_ctx),
            EwsRequestZone::Unresolved,
            "supplied-but-unknown zone id must surface as Unresolved"
        );

        let changes = parse_item_changes(body_ctx);
        assert_eq!(changes.len(), 1);
        let mut item = base_item();
        assert_eq!(
            apply_field_changes(&mut item, &changes, EwsRequestZone::Unresolved),
            Err(EwsUpdateError::TimezoneUnresolved),
            "a naive Start against a supplied-but-unresolved zone must fail closed"
        );
        assert_eq!(
            item.start,
            crate::calendar::parse_datetime("2025-01-15T12:00:00Z").unwrap(),
            "the rejected change must not have been applied"
        );

        // An explicit-offset value stays readable even with an unresolved zone.
        let body_z = body_ctx.replace("09:00:00<", "09:00:00Z<");
        let changes_z = parse_item_changes(&body_z);
        let mut item = base_item();
        apply_field_changes(&mut item, &changes_z, EwsRequestZone::Unresolved).unwrap();
        assert_eq!(
            item.start,
            crate::calendar::parse_datetime("2025-01-15T09:00:00Z").unwrap()
        );
    }

    /// Outlook sends timezone changes as `SetItemField`s in BOTH shapes: the
    /// `t:StartTimeZoneId` element (id as text) and the `t:StartTimeZone`
    /// element (id in the `Id` attribute). Both must land on `item.timezone`
    /// (IANA-normalized, the same form `parse_ews_calendar_item` stores).
    #[test]
    fn apply_field_changes_sets_timezone_from_both_starttimezone_shapes() {
        let body_id_element = concat!(
            r#"<m:UpdateItem xmlns:m="http://schemas.microsoft.com/exchange/services/2006/messages""#,
            r#" xmlns:t="http://schemas.microsoft.com/exchange/services/2006/types">"#,
            r#"<m:ItemChanges><t:ItemChange><t:Updates>"#,
            r#"<t:SetItemField><t:FieldURI FieldURI="calendar:StartTimeZoneId"/>"#,
            r#"<t:CalendarItem><t:StartTimeZoneId>W. Europe Standard Time</t:StartTimeZoneId></t:CalendarItem>"#,
            r#"</t:SetItemField>"#,
            r#"</t:Updates></t:ItemChange></m:ItemChanges></m:UpdateItem>"#
        );
        let changes = parse_item_changes(body_id_element);
        assert_eq!(changes.len(), 1);
        let mut item = base_item();
        apply_field_changes(&mut item, &changes, EwsRequestZone::Absent).unwrap();
        assert_eq!(
            item.timezone.as_deref(),
            Some("Europe/Berlin"),
            "StartTimeZoneId element text must set the normalized IANA zone"
        );

        let body_id_attribute = concat!(
            r#"<m:UpdateItem xmlns:m="http://schemas.microsoft.com/exchange/services/2006/messages""#,
            r#" xmlns:t="http://schemas.microsoft.com/exchange/services/2006/types">"#,
            r#"<m:ItemChanges><t:ItemChange><t:Updates>"#,
            r#"<t:SetItemField><t:FieldURI FieldURI="calendar:StartTimeZone"/>"#,
            r#"<t:CalendarItem><t:StartTimeZone Id="Pacific Standard Time" Name="(UTC-08:00) Pacific Time (US &amp; Canada)"/></t:CalendarItem>"#,
            r#"</t:SetItemField>"#,
            r#"</t:Updates></t:ItemChange></m:ItemChanges></m:UpdateItem>"#
        );
        let changes = parse_item_changes(body_id_attribute);
        assert_eq!(changes.len(), 1);
        let mut item = base_item();
        apply_field_changes(&mut item, &changes, EwsRequestZone::Absent).unwrap();
        assert_eq!(
            item.timezone.as_deref(),
            Some("America/Los_Angeles"),
            "StartTimeZone Id attribute must set the normalized IANA zone"
        );
    }

    /// The legacy `t:MeetingTimeZone` shape carries the id in its
    /// `TimeZoneName` attribute; it must reach BOTH the stored raw name
    /// (`timezone_blob`, the legacy blob field) and the normalized
    /// `timezone` — the same split `parse_ews_calendar_item` produces.
    #[test]
    fn apply_field_changes_sets_timezone_from_meetingtimezone_attribute() {
        let body = concat!(
            r#"<m:UpdateItem xmlns:m="http://schemas.microsoft.com/exchange/services/2006/messages""#,
            r#" xmlns:t="http://schemas.microsoft.com/exchange/services/2006/types">"#,
            r#"<m:ItemChanges><t:ItemChange><t:Updates>"#,
            r#"<t:SetItemField><t:FieldURI FieldURI="calendar:MeetingTimeZone"/>"#,
            r#"<t:CalendarItem><t:MeetingTimeZone TimeZoneName="W. Europe Standard Time"/></t:CalendarItem>"#,
            r#"</t:SetItemField>"#,
            r#"</t:Updates></t:ItemChange></m:ItemChanges></m:UpdateItem>"#
        );
        let changes = parse_item_changes(body);
        assert_eq!(changes.len(), 1);
        let mut item = base_item();
        apply_field_changes(&mut item, &changes, EwsRequestZone::Absent).unwrap();
        assert_eq!(
            item.timezone_blob.as_deref(),
            Some("W. Europe Standard Time"),
            "MeetingTimeZone keeps the raw Windows name for the legacy blob"
        );
        assert_eq!(
            item.timezone.as_deref(),
            Some("Europe/Berlin"),
            "MeetingTimeZone also sets the normalized IANA zone"
        );
    }
}
