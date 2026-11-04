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

/// Convert an iCalendar (ICS) document into the JSCalendar `Event` object that
/// Stalwart's `CalendarEvent/set` expects as its write payload.
///
/// Stalwart v1.0.x **silently discards** a raw `iCalendar` string supplied to
/// `CalendarEvent/set`: the create is accepted, the event is stored, and every
/// content property (title/start/duration/…) is dropped. The write therefore
/// has to be expressed as structured JSCalendar properties, which is exactly
/// what `calcard` — the library Stalwart itself uses — produces from the same
/// ICS the gateway renders. This runs the ICS → JSCalendar half of the
/// round-trip that [`jscalendar_event_to_ics`] reverses, so the payload the
/// gateway writes is the same shape Stalwart reads back.
///
/// Returns `None` when the ICS cannot be parsed or yields no event entry.
pub fn ics_to_jscalendar_event(ics: &str) -> Option<Value> {
    let parsed = calcard::icalendar::ICalendar::parse(ics).ok()?;
    let group = parsed.into_jscalendar::<String, String>();
    let json = serde_json::to_value(group.into_inner()).ok()?;
    let entry = json.get("entries")?.as_array()?.first()?.clone();
    entry.is_object().then_some(entry)
}

/// JMAP `CalendarEvent` properties the gateway must never send in a write
/// payload: they are server-managed or immutable. `method`, `isOrigin` and
/// `baseEventId` are rejected outright by Stalwart with
/// `invalidProperties`/`This property is immutable`, while `id`,
/// `calendarIds`, `@etag`, `created`, `updated`, `sequence` and `isDraft`
/// are assigned by the server (sending a stale `sequence`/`updated` would
/// fight the server's own bookkeeping).
const SERVER_MANAGED_WRITE_PROPERTIES: &[&str] = &[
    "id",
    "baseEventId",
    "calendarIds",
    "isDraft",
    "isOrigin",
    "method",
    "created",
    "updated",
    "sequence",
    "@etag",
];

/// Optional `CalendarEvent` properties that a *replacement* update must clear
/// when the new event no longer carries them. Stalwart's `CalendarEvent/set`
/// update is a **partial patch** (verified live against v1.0.3): a property
/// absent from the patch keeps its previous value. The gateway renders a
/// complete event from ICS, so any property the fresh event does not set must
/// be explicitly nulled or stale content (a removed description, a dropped
/// attendee roster, a cancelled RRULE) would survive the update.
const CLEARABLE_ON_UPDATE_PROPERTIES: &[&str] = &[
    "description",
    "locations",
    "keywords",
    "status",
    "privacy",
    "freeBusyStatus",
    "priority",
    "recurrenceRule",
    "recurrenceOverrides",
    "participants",
    "organizerCalendarAddress",
    "alerts",
    "links",
    "virtualLocations",
    "color",
    "showWithoutTime",
    "timeZone",
];

/// Build the JSCalendar payload for a `CalendarEvent/set` create or update
/// from the `Event` object produced by [`ics_to_jscalendar_event`].
///
/// For an update (`for_update = true`) every [`CLEARABLE_ON_UPDATE_PROPERTIES`]
/// entry the new event does not set is inserted as an explicit `null`, turning
/// Stalwart's partial patch into a faithful replacement of the event's content.
pub fn jscalendar_write_payload(entry: &Value, for_update: bool) -> Value {
    let mut obj = entry.as_object().cloned().unwrap_or_default();
    for key in SERVER_MANAGED_WRITE_PROPERTIES {
        obj.remove(*key);
    }
    obj.entry("@type")
        .or_insert_with(|| Value::String("Event".to_string()));
    if for_update {
        for key in CLEARABLE_ON_UPDATE_PROPERTIES {
            obj.entry((*key).to_string()).or_insert(Value::Null);
        }
    }
    Value::Object(obj)
}

/// Verify that a `CalendarEvent/get` read-back carries the content the gateway
/// just wrote. Used to fail the write path closed: if the backend accepted the
/// `CalendarEvent/set` but did not apply the payload, the caller must surface
/// an error so create/update traffic falls back to CalDAV instead of silently
/// losing the event.
///
/// The title is the content marker (the gateway's ICS always renders one);
/// when the event has no title the `start` presence is checked instead. The
/// comparison is deliberately tolerant of benign whitespace normalisation —
/// the failure being detected is the backend storing an *empty* event, not a
/// cosmetic difference.
pub fn verify_write_round_trip(read_back: &Value, entry: &Value) -> Result<(), String> {
    let expected_title = entry
        .get("title")
        .and_then(|v| v.as_str())
        .filter(|t| !t.trim().is_empty());
    if let Some(expected) = expected_title {
        let got = read_back
            .get("title")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        if got.trim().is_empty() {
            return Err(format!(
                "CalendarEvent/set payload was not applied (expected title {:?}, read back an empty event); \
                 the backend discarded the event content",
                expected
            ));
        }
        if got.trim() != expected.trim() {
            return Err(format!(
                "CalendarEvent/set payload was not applied (expected title {:?}, read back {:?})",
                expected, got
            ));
        }
    } else if entry.get("start").is_some() && read_back.get("start").is_none() {
        return Err(
            "CalendarEvent/set payload was not applied (start missing on read-back); \
             the backend discarded the event content"
                .to_string(),
        );
    }
    Ok(())
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

    #[test]
    fn ics_to_jscalendar_round_trips_event_content() {
        // The write half: ICS → JSCalendar Event object. This is what
        // `set_calendar_event` posts to Stalwart instead of a raw ICS string
        // (a raw `iCalendar` payload is silently discarded by Stalwart).
        let ics = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//exchange_gateway//EN\r\n\
                   BEGIN:VEVENT\r\nUID:rt-1\r\nDTSTAMP:20260301T000000Z\r\n\
                   DTSTART;TZID=Europe/Berlin:20260302T090000\r\n\
                   DTEND;TZID=Europe/Berlin:20260302T100000\r\nSUMMARY:Round Trip\r\n\
                   DESCRIPTION:Body text\r\nLOCATION:Room A\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
        let entry = ics_to_jscalendar_event(ics).expect("converted");
        assert_eq!(
            entry.get("title").and_then(|v| v.as_str()),
            Some("Round Trip")
        );
        assert_eq!(
            entry.get("description").and_then(|v| v.as_str()),
            Some("Body text")
        );
        assert!(
            entry.get("start").is_some(),
            "start must be present: {entry}"
        );
        // Round-tripping back to ICS via the read half must preserve the data.
        let back = jscalendar_event_to_ics(&entry).expect("back to ics");
        assert!(back.contains("SUMMARY:Round Trip"), "{back}");
        assert!(
            back.contains("DTSTART;TZID=Europe/Berlin:20260302T090000"),
            "{back}"
        );
    }

    #[test]
    fn write_payload_strips_server_managed_properties() {
        // The converted entry carries calcard bookkeeping (`method`) and, on
        // an update, server-owned fields; none may be sent to Stalwart.
        let entry = json!({
            "@type": "Event",
            "uid": "u",
            "title": "T",
            "start": "2026-03-02T09:00:00",
            "timeZone": "Etc/UTC",
            "duration": "PT1H",
            "method": "request",
            "id": "server-id",
            "calendarIds": { "c": true },
            "created": "2026-01-01T00:00:00Z",
            "updated": "2026-01-01T00:00:00Z",
            "sequence": 4,
            "@etag": "abc",
            "isDraft": false,
            "isOrigin": true,
            "baseEventId": "base"
        });
        let create = jscalendar_write_payload(&entry, false);
        for key in [
            "method",
            "id",
            "calendarIds",
            "created",
            "updated",
            "sequence",
            "@etag",
            "isDraft",
            "isOrigin",
            "baseEventId",
        ] {
            assert!(create.get(key).is_none(), "create payload leaked {key}");
        }
        assert_eq!(create.get("title").and_then(|v| v.as_str()), Some("T"));
        assert_eq!(create.get("@type").and_then(|v| v.as_str()), Some("Event"));
    }

    #[test]
    fn update_payload_nulls_absent_optional_properties() {
        // Stalwart's CalendarEvent/set update is a partial patch: any optional
        // property the fresh event does not carry must be explicitly nulled so
        // stale content (a removed description, dropped attendees, a cancelled
        // RRULE) does not survive the update.
        let entry = json!({
            "@type": "Event",
            "uid": "u",
            "title": "Only title",
            "start": "2026-03-02T09:00:00",
            "timeZone": "Etc/UTC",
            "duration": "PT1H"
        });
        let update = jscalendar_write_payload(&entry, true);
        for key in [
            "description",
            "locations",
            "recurrenceRule",
            "participants",
            "status",
            "privacy",
        ] {
            assert_eq!(
                update.get(key),
                Some(&Value::Null),
                "update payload must null absent {key}"
            );
        }
        // A property the event DOES carry is not overwritten with null.
        assert_eq!(
            update.get("title").and_then(|v| v.as_str()),
            Some("Only title")
        );
        // Create payloads must NOT null anything (no prior state to clear).
        let create = jscalendar_write_payload(&entry, false);
        assert!(create.get("description").is_none());
    }

    #[test]
    fn verify_write_round_trip_detects_discarded_payload() {
        let entry = json!({ "title": "Kept", "start": "2026-03-02T09:00:00" });
        // Content applied → Ok.
        assert!(
            verify_write_round_trip(
                &json!({ "title": "Kept", "start": "2026-03-02T09:00:00" }),
                &entry
            )
            .is_ok()
        );
        // Backend stored an empty event → Err naming the discard.
        let err = verify_write_round_trip(&json!({ "id": "x" }), &entry).unwrap_err();
        assert!(err.contains("not applied"), "{err}");
        // Whitespace normalisation is tolerated (benign).
        assert!(verify_write_round_trip(&json!({ "title": "  Kept  " }), &entry).is_ok());
    }

    #[test]
    fn verify_write_round_trip_without_title_checks_start() {
        let entry = json!({ "start": "2026-03-02T09:00:00" });
        assert!(
            verify_write_round_trip(&json!({ "start": "2026-03-02T09:00:00" }), &entry).is_ok()
        );
        assert!(verify_write_round_trip(&json!({ "id": "x" }), &entry).is_err());
    }
}
