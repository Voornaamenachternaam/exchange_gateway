// src/jscalendar.rs
//
// Reconstruct iCalendar from the JSCalendar object Stalwart returns in the
// JMAP `iCalendar` property.
//
// Stalwart's `CalendarEvent/get` does NOT return an ICS string in
// `iCalendar`. It returns the JSCalendar *representation of the ICS
// component* — `{"convertedProperties":…,"name":"vevent","properties":[…]}` —
// with every standard JSCalendar property (`title`, `start`, `duration`,
// `recurrenceRule`, …) as a *sibling* of it in the same event object. The
// gateway renders EWS/EAS items from ICS, so the whole event object is
// reassembled and converted with `calcard` — the exact library Stalwart uses
// to produce it — which reproduces the source component (DTSTART/DTEND,
// RRULE/EXDATE/RECURRENCE-ID overrides, VALARM, ATTENDEE/ORGANIZER, and the
// original X-properties carried in the `iCalendar` fragment) without loss.
//
// Converted live against Stalwart's own calcard producer for the full event
// shape matrix: simple/attendee/organizer events, RRULE with COUNT/UNTIL/BYDAY,
// EXDATE, moved RECURRENCE-ID overrides, all-day (single and multi-day),
// DURATION-only, CATEGORIES/CLASS/STATUS/TRANSP, VALARM, TZID and custom
// VTIMEZONE zones, X-MS-* properties, cancellation and sequence/priority.

use serde_json::{Map, Value, json};

/// JMAP-only `CalendarEvent` properties. They are part of the JMAP object but
/// have no iCalendar counterpart, so `calcard` would otherwise emit them as
/// non-standard `JSPROP;JSPTR=…` lines.
const JMAP_ONLY_PROPERTIES: &[&str] = &[
    "id",
    "baseEventId",
    "calendarIds",
    "isDraft",
    "isOrigin",
    "mayInviteSelf",
    "mayInviteOthers",
    "hideAttendees",
    "useDefaultAlerts",
    "utcStart",
    "utcEnd",
    // Requested from Stalwart and retained on `JmapCalendarEvent::etag` for
    // concurrency tracking; it has no iCalendar counterpart.
    "@etag",
];

/// Convert a JSCalendar event (a whole `CalendarEvent/get` object, or the
/// `iCalendar` property value) into an iCalendar document.
///
/// Accepts a literal ICS string (returned verbatim) or a JSCalendar object.
/// Returns `None` when the value is neither, when it is empty, or when the
/// conversion does not produce a component.
pub fn jscalendar_event_to_ics(event: &Value) -> Option<String> {
    if let Some(ics) = event.as_str() {
        return (!ics.is_empty()).then(|| ics.to_string());
    }
    let obj = event.as_object()?;
    let entry = sanitize_event(obj);
    // Reject an object that carries no event data at all (only the `@type`
    // we defaulted, optionally `version`): it would convert to an empty
    // VEVENT, which is never useful to a caller.
    if entry.keys().all(|k| k == "@type" || k == "version") {
        return None;
    }
    let wrapped = json!({ "@type": "Group", "entries": [Value::Object(entry)] }).to_string();
    let parsed = calcard::jscalendar::JSCalendar::<String, String>::parse(&wrapped).ok()?;
    let ics = parsed.into_icalendar()?;
    let rendered = ics.to_string();
    rendered.contains("BEGIN:VEVENT").then_some(rendered)
}

/// Prepare a JSCalendar event object for `calcard`'s exporter.
fn sanitize_event(obj: &Map<String, Value>) -> Map<String, Value> {
    let mut out = obj.clone();
    for key in JMAP_ONLY_PROPERTIES {
        out.remove(*key);
    }
    // `@type` is required by the JSCalendar parser; Stalwart always sets it,
    // but default it so a bare fragment still converts.
    out.entry("@type")
        .or_insert_with(|| Value::String("Event".to_string()));
    // `calcard`'s exporter asserts (in debug builds) that every participant
    // becomes either an ATTENDEE/ORGANIZER property or a PARTICIPANT
    // component. A participant with a `calendarAddress` becomes an ATTENDEE;
    // one without has neither, which trips the assertion (verified against
    // calcard 0.3.14, export/convert.rs:671) and would panic the request.
    // RFC 8984 §4.4.6 permits an addressless participant (e.g. a resource
    // booked by name), so rather than dropping it the participant is pinned to
    // a PARTICIPANT component by supplying the converted-component fragment
    // `calcard` keys on (`{"name":"participant"}`); calcard then preserves the
    // participant's name and remaining properties in the ICS instead of
    // panicking.
    if let Some(Value::Object(participants)) = out.get_mut("participants") {
        for participant in participants.values_mut() {
            let Some(obj) = participant.as_object_mut() else {
                continue;
            };
            let addressless = !obj.get("calendarAddress").is_some_and(|v| !v.is_null());
            if addressless && !obj.contains_key("iCalendar") {
                obj.insert("iCalendar".to_string(), json!({ "name": "participant" }));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ical_parser;
    use serde_json::json;

    fn to_ics(v: Value) -> String {
        jscalendar_event_to_ics(&v).expect("conversion")
    }

    #[test]
    fn converts_string_ics_verbatim() {
        let ics = "BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nUID:u\r\nEND:VEVENT\r\nEND:VCALENDAR";
        assert_eq!(jscalendar_event_to_ics(&json!(ics)).as_deref(), Some(ics));
        assert!(jscalendar_event_to_ics(&json!("")).is_none());
    }

    #[test]
    fn reconstructs_full_event_from_jscalendar_object() {
        // The exact shape Stalwart returns: standard properties plus the
        // `iCalendar` fragment that carries the converted-property mapping.
        let event = json!({
            "@type": "Event",
            "id": "evt1",
            "uid": "u-1",
            "title": "Team Sync",
            "start": "2026-01-05T09:00:00",
            "timeZone": "Europe/Berlin",
            "duration": "PT1H",
            "calendarIds": { "c": true },
            "isDraft": false,
            "utcStart": "2026-01-05T08:00:00Z",
            "iCalendar": {
                "convertedProperties": { "duration": { "name": "dtend" } },
                "name": "vevent"
            }
        });
        let ics = to_ics(event);
        assert!(ics.contains("BEGIN:VEVENT"));
        assert!(ics.contains("UID:u-1"));
        assert!(ics.contains("SUMMARY:Team Sync"));
        assert!(ics.contains("DTSTART;TZID=Europe/Berlin:20260105T090000"));
        assert!(ics.contains("DTEND;TZID=Europe/Berlin:20260105T100000"));
        // JMAP-only properties must not leak as non-standard lines.
        assert!(!ics.contains("JSPROP"));
        assert!(!ics.contains("calendarIds"));
    }

    #[test]
    fn preserves_recurrence_rule_exdates_and_moved_override() {
        // Mirrors Stalwart's real output: the moved occurrence carries a
        // `locations` map (not a bare `location` string), and calcard folds
        // long lines at 75 octets.
        let event = json!({
            "@type": "Event",
            "uid": "rec-1",
            "title": "Weekly",
            "start": "2026-01-05T09:00:00",
            "timeZone": "Etc/UTC",
            "duration": "PT1H",
            "recurrenceRule": { "frequency": "weekly", "count": 5 },
            "recurrenceOverrides": {
                "2026-01-12T09:00:00": {
                    "start": "2026-01-12T11:00:00",
                    "title": "Moved",
                    "locations": {
                        "l1": { "@type": "Location", "name": "NewRoom" }
                    }
                }
            },
            "iCalendar": { "name": "vevent" }
        });
        let ics = to_ics(event);
        assert!(ics.contains("RRULE:FREQ=WEEKLY;COUNT=5"), "{ics}");
        assert!(ics.contains("RECURRENCE-ID:20260112T090000Z"), "{ics}");
        assert!(ics.contains("SUMMARY:Moved"), "{ics}");
        // Unfold before matching: calcard wraps at 75 octets.
        let unfolded = ical_parser::unfold_ical_content(&ics);
        assert!(unfolded.contains("LOCATION;JSID=l1:NewRoom"), "{ics}");
    }

    #[test]
    fn preserves_all_day_dates() {
        let event = json!({
            "@type": "Event",
            "uid": "ad-1",
            "title": "Holiday",
            "start": "2026-05-01T00:00:00",
            "timeZone": "Etc/UTC",
            "duration": "P1D",
            "showWithoutTime": true,
            "iCalendar": {
                "convertedProperties": {
                    "duration": { "name": "dtend", "parameters": { "value": "DATE" } },
                    "start": { "parameters": { "value": "DATE" } }
                },
                "name": "vevent"
            }
        });
        let ics = to_ics(event);
        assert!(ics.contains("DTSTART;VALUE=DATE:20260501"), "{ics}");
        assert!(ics.contains("DTEND;VALUE=DATE:20260502"), "{ics}");
    }

    #[test]
    fn preserves_valarm_and_x_properties_from_fragment() {
        let event = json!({
            "@type": "Event",
            "uid": "x-1",
            "title": "X",
            "start": "2026-01-05T09:00:00",
            "timeZone": "Etc/UTC",
            "duration": "PT1H",
            "alerts": {
                "a1": { "@type": "Alert", "action": "display", "trigger": { "@type": "OffsetTrigger", "offset": "-PT15M" } }
            },
            "iCalendar": {
                "convertedProperties": { "duration": { "name": "dtend" } },
                "name": "vevent",
                "properties": [["x-ms-meeting-status", {}, "unknown", "1"]]
            }
        });
        let ics = to_ics(event);
        assert!(ics.contains("BEGIN:VALARM"), "{ics}");
        assert!(ics.contains("X-MS-MEETING-STATUS:1"), "{ics}");
    }

    #[test]
    fn preserves_addressless_participant_without_panicking() {
        // A participant with no `calendarAddress` trips calcard's exporter
        // assertion in debug builds; it must be preserved as a PARTICIPANT
        // component, not dropped and not panic.
        let event = json!({
            "@type": "Event",
            "uid": "p-1",
            "title": "P",
            "start": "2026-01-05T09:00:00",
            "timeZone": "Etc/UTC",
            "duration": "PT1H",
            "iCalendar": { "convertedProperties": { "duration": { "name": "dtend" } }, "name": "vevent" },
            "participants": {
                "ghost": { "@type": "Participant", "name": "No Address" },
                "real": {
                    "@type": "Participant",
                    "calendarAddress": "mailto:alice@example.com",
                    "name": "Alice",
                    "roles": { "required": true },
                    "participationStatus": "accepted"
                }
            }
        });
        let ics = to_ics(event);
        let unfolded = ical_parser::unfold_ical_content(&ics);
        // The addressless participant's details survive as a component.
        assert!(unfolded.contains("BEGIN:PARTICIPANT"), "{ics}");
        assert!(unfolded.contains("SUMMARY:No Address"), "{ics}");
        // The addressed participant still becomes an ATTENDEE.
        assert!(unfolded.contains("mailto:alice@example.com"), "{ics}");
        // The nested PARTICIPANT block must not leak into the event's own
        // fields: its SUMMARY must not overwrite the event subject.
        let item = crate::calendar::parse_ics_event(&ics).expect("parsed event");
        assert_eq!(item.subject, "P", "participant SUMMARY leaked into subject");
        assert_eq!(item.attendees.len(), 1);
    }

    #[test]
    fn strips_etag_from_reconstructed_ics() {
        // `@etag` is requested from Stalwart and kept on the event for
        // concurrency tracking; it must never leak into the ICS as a
        // `JSPROP;JSPTR=@etag` line.
        let event = json!({
            "@type": "Event",
            "uid": "e-1",
            "title": "E",
            "start": "2026-01-05T09:00:00",
            "timeZone": "Etc/UTC",
            "duration": "PT1H",
            "@etag": "W/\"abc123\"",
            "iCalendar": { "name": "vevent" }
        });
        let ics = to_ics(event);
        assert!(!ics.contains("@etag"), "{ics}");
        assert!(!ics.contains("JSPROP"), "{ics}");
        assert!(!ics.contains("abc123"), "{ics}");
    }

    #[test]
    fn rejects_non_event_values() {
        assert!(jscalendar_event_to_ics(&json!(42)).is_none());
        assert!(jscalendar_event_to_ics(&json!({})).is_none());
        assert!(jscalendar_event_to_ics(&json!(null)).is_none());
    }

    #[test]
    fn reconstructed_ics_parses_through_the_consumer_chain() {
        // End-to-end: a Stalwart-shaped CalendarEvent/get object must survive
        // `to_ics` → `parse_ics_event` (the exact path sync.rs/eas.rs/ews.rs
        // use) and yield the event data, not an empty item.
        let event = json!({
            "@type": "Event",
            "id": "e1",
            "uid": "chain-1",
            "title": "Budget Review",
            "description": "Q1 numbers",
            "start": "2026-03-02T14:00:00",
            "timeZone": "Europe/Berlin",
            "duration": "PT45M",
            "locations": { "l1": { "@type": "Location", "name": "Room 4" } },
            "recurrenceRule": { "frequency": "weekly", "count": 3 },
            "iCalendar": {
                "convertedProperties": { "duration": { "name": "dtend" } },
                "name": "vevent"
            }
        });
        let ics = jscalendar_event_to_ics(&event).expect("ics");
        let item = crate::calendar::parse_ics_event(&ics).expect("parsed event");
        assert_eq!(item.uid, "chain-1");
        assert_eq!(item.subject, "Budget Review");
        assert_eq!(item.description, "Q1 numbers");
        assert_eq!(item.location, "Room 4");
        assert!(item.rrule.is_some(), "RRULE must survive: {ics}");
        // 14:00 Europe/Berlin (CET, UTC+1) on 2026-03-02 == 13:00Z.
        assert_eq!(item.start.timestamp(), 1_772_456_400);
        assert_eq!(item.end.timestamp(), 1_772_456_400 + 45 * 60);
    }
}
