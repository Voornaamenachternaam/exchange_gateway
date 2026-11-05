// tests/jmap_calendar_deploy.rs
//
// Deploy-time verification of the JMAP Calendars capability
// (`urn:ietf:params:jmap:calendars`) against a real Stalwart v0.16.22
// backend (audit item 5). Skipped unless the environment points at a live
// server:
//
//   STALWART_JMAP_URL=http://localhost:8080/jmap \
//   STALWART_USER=admin STALWART_PASS=... \
//   cargo test --test jmap_calendar_deploy -- --nocapture
//
// The test asserts that the deploy check the gateway runs at startup
// (`JmapClient::verify_calendar_capability_deploy`) verifies clean, and then
// performs a real `CalendarEvent/set` round-trip with a recurring event plus
// a per-instance exception — the exact shape EAS MS-ASCAL <Exceptions>
// relies on — asserting the server persists and returns it faithfully.

use exchange_gateway::jmap::JmapClient;
use secrecy::SecretString;
use serde_json::{Value, json};

fn env_config() -> Option<(String, String, SecretString)> {
    match (
        std::env::var("STALWART_JMAP_URL"),
        std::env::var("STALWART_USER"),
        std::env::var("STALWART_PASS"),
    ) {
        (Ok(url), Ok(user), Ok(pass)) => Some((url, user, SecretString::from(pass))),
        _ => None,
    }
}

#[tokio::test]
async fn verify_calendar_capability_against_real_stalwart() {
    let Some((base, user, pass)) = env_config() else {
        eprintln!("STALWART_JMAP_URL/STALWART_USER/STALWART_PASS not set; skipping");
        return;
    };

    let client = JmapClient::new(&base).expect("JmapClient::new");
    let report = client
        .verify_calendar_capability_deploy(&user, &pass)
        .await
        .expect("deploy verification call failed");

    eprintln!("report: {:#?}", report);
    assert!(report.capability_present, "calendars capability missing");
    assert!(report.account_id.is_some(), "no primary calendar account");
    assert!(report.calendar_get_ok, "Calendar/get failed");
    assert!(report.calendar_event_query_ok, "CalendarEvent/query failed");
    assert!(
        report.structured_recurrence_ok,
        "recurrenceRule/recurrenceOverrides round-trip or recurrence expansion failed"
    );
    // The write probe now exercises the gateway's actual create path: an ICS
    // converted to structured JSCalendar properties (a raw `iCalendar`
    // payload is discarded by Stalwart v1.0.x, which is why the gateway
    // converts before writing). When the round-trip fails the deploy check
    // MUST flag it (issue + not healthy).
    if !report.ics_round_trip_ok {
        assert!(
            report.issues.iter().any(|i| i.starts_with("write probe:")),
            "write round-trip failure not flagged in issues: {:?}",
            report.issues
        );
        assert!(
            !report.is_healthy(),
            "deploy check must refuse healthy status when the gateway's write path is broken"
        );
    } else {
        assert!(
            report.is_healthy(),
            "deploy report has issues: {:?}",
            report.issues
        );
    }
}

#[tokio::test]
async fn recurring_event_with_exception_round_trip() {
    let Some((base, user, pass)) = env_config() else {
        eprintln!("STALWART_JMAP_URL/STALWART_USER/STALWART_PASS not set; skipping");
        return;
    };
    let client = JmapClient::new(&base).expect("JmapClient::new");
    let session = client
        .get_session(&user, &pass)
        .await
        .expect("session fetch");
    let cap = exchange_gateway::jmap::JMAP_CAL_CAPABILITY;
    let account = session
        .primary_accounts
        .get(cap)
        .cloned()
        .expect("no calendar primary account");

    let using = vec!["urn:ietf:params:jmap:core", cap];

    // Find a calendar to write into.
    let resp = client
        .api_call(
            &session.api_url,
            &using,
            vec![(
                "Calendar/get",
                json!({"accountId": account, "ids": null}),
                "g",
            )],
            &user,
            &pass,
        )
        .await
        .expect("Calendar/get");
    let calendars: Vec<Value> = serde_json::from_value(
        resp.method_responses
            .iter()
            .find(|(m, _, _)| m == "Calendar/get")
            .expect("Calendar/get response")
            .1
            .get("list")
            .cloned()
            .unwrap_or(json!([])),
    )
    .unwrap();
    let calendar_id = calendars
        .first()
        .and_then(|c| c.get("id"))
        .and_then(|v| v.as_str())
        .expect("no calendar available")
        .to_string();

    // Create a weekly recurring event with one excluded instance. Verified
    // live against v0.16.22: RFC-form `recurrenceRules` arrays are rejected
    // ("invalidProperties"); this build speaks the older-draft spelling
    // `recurrenceRule` (single object) + `recurrenceOverrides`.
    // Unique UID per run: Stalwart rejects a create whose UID collides with
    // any event still present in the account (e.g. one orphaned by an
    // earlier failed cleanup), which would make repeated runs flaky.
    let event_uid = format!("jmap-cal-deploy-check-{}", uuid::Uuid::new_v4());
    let create = json!({
        "accountId": account,
        "create": {
            "ev1": {
                "calendarIds": { calendar_id: true },
                "uid": event_uid,
                "title": "deploy-check recurring",
                "start": "2026-03-02T09:00:00",
                "timeZone": "Europe/Berlin",
                "duration": "PT1H",
                "recurrenceRule": { "frequency": "weekly" },
                "recurrenceOverrides": {
                    "2026-03-09T09:00:00": { "excluded": true }
                }
            }
        }
    });
    let resp = client
        .api_call(
            &session.api_url,
            &using,
            vec![("CalendarEvent/set", create, "s")],
            &user,
            &pass,
        )
        .await
        .expect("CalendarEvent/set create");
    let set_args = &resp
        .method_responses
        .iter()
        .find(|(m, _, _)| m == "CalendarEvent/set")
        .expect("CalendarEvent/set response")
        .1;
    // `notCreated` only indicates failure when it is non-null and non-empty;
    // servers may echo back an explicit null or empty object on success.
    assert!(
        set_args
            .get("notCreated")
            .is_none_or(|v| { v.is_null() || v.as_object().is_some_and(|o| o.is_empty()) }),
        "create rejected: {:?}",
        set_args.get("notCreated")
    );
    let event_id = set_args["created"]["ev1"]["id"]
        .as_str()
        .expect("created id")
        .to_string();

    // Read back and assert recurrence + exception survived, then destroy.
    let read_back = client
        .api_call(
            &session.api_url,
            &using,
            vec![(
                "CalendarEvent/get",
                json!({
                    "accountId": account,
                    "ids": [event_id.as_str()],
                    "properties": ["uid", "recurrenceRule", "recurrenceOverrides", "timeZone"]
                }),
                "r",
            )],
            &user,
            &pass,
        )
        .await
        .expect("CalendarEvent/get");

    // Assert uid, recurrence rule and the excluded occurrence all
    // round-tripped, then confirm the event appears in its first-occurrence
    // window (proves the server indexed the recurrence for time queries).
    let ev = &read_back
        .method_responses
        .iter()
        .find(|(m, _, _)| m == "CalendarEvent/get")
        .expect("CalendarEvent/get response")
        .1["list"][0];
    assert_eq!(
        ev["uid"],
        Value::String(event_uid.clone()),
        "uid did not round-trip: {}",
        ev
    );
    assert_eq!(
        ev["recurrenceRule"]["frequency"],
        Value::String("weekly".to_string()),
        "recurrenceRule did not round-trip: {}",
        ev
    );
    assert_eq!(
        ev["recurrenceOverrides"]["2026-03-09T09:00:00"]["excluded"],
        Value::Bool(true),
        "excluded occurrence did not round-trip: {}",
        ev
    );
    assert_eq!(
        ev["timeZone"],
        Value::String("Europe/Berlin".to_string()),
        "timeZone did not round-trip: {}",
        ev
    );

    // The event (RRULE FREQ=WEEKLY with one EXDATE instance) must expand
    // into its first occurrence window.
    let expand = client
        .api_call(
            &session.api_url,
            &using,
            vec![(
                "CalendarEvent/query",
                json!({
                    "accountId": account,
                    "filter": {
                        "after": "2026-03-02T00:00:00Z",
                        "before": "2026-03-03T00:00:00Z"
                    }
                }),
                "q0",
            )],
            &user,
            &pass,
        )
        .await
        .expect("CalendarEvent/query expansion");
    let expanded_ids = &expand
        .method_responses
        .iter()
        .find(|(m, _, _)| m == "CalendarEvent/query")
        .expect("CalendarEvent/query response")
        .1["ids"];
    assert!(
        expanded_ids
            .as_array()
            .map(|ids| ids.iter().any(|i| i.as_str() == Some(event_id.as_str())))
            .unwrap_or(false),
        "recurring event not found in its first occurrence window: {}",
        expanded_ids
    );

    let destroy_resp = client
        .api_call(
            &session.api_url,
            &using,
            vec![(
                "CalendarEvent/set",
                json!({"accountId": account, "destroy": [event_id.as_str()]}),
                "d",
            )],
            &user,
            &pass,
        )
        .await
        .expect("CalendarEvent/set destroy");
    assert!(
        destroy_resp
            .method_responses
            .iter()
            .find(|(m, _, _)| m == "CalendarEvent/set")
            .and_then(|(_, a, _)| a.get("destroyed"))
            .map(|v| !v.as_array().map(|a| a.is_empty()).unwrap_or(true))
            .unwrap_or(false),
        "destroy failed: {:?}",
        destroy_resp.method_responses
    );
}

/// End-to-end exercise of the gateway's OWN calendar write path
/// (`JmapClient::set_calendar_event`), the path fixed for AUDIT3 finding #2.
///
/// Stalwart v1.0.x silently discards a raw `iCalendar` payload posted to
/// `CalendarEvent/set`; the fix converts the rendered ICS to structured
/// JSCalendar properties and verifies the write by reading the event back.
/// This test drives create, then update (title + description + location), and
/// asserts the content actually persisted both times — the exact failure the
/// audit found (an accepted write that stored an empty event).
#[tokio::test]
async fn gateway_set_calendar_event_round_trips_content() {
    use exchange_gateway::jmap::SetCalendarEventParams;

    let Some((base, user, pass)) = env_config() else {
        eprintln!("STALWART_JMAP_URL/STALWART_USER/STALWART_PASS not set; skipping");
        return;
    };
    let client = JmapClient::new(&base).expect("JmapClient::new");
    let account = client
        .get_account_id(&user, &pass)
        .await
        .expect("account id");
    let calendars = client
        .query_calendars(&user, &pass)
        .await
        .expect("query_calendars");
    let calendar_id = calendars
        .calendars
        .first()
        .and_then(|c| c.id.clone())
        .expect("no calendar available");

    let uid = format!("gw-write-check-{}", uuid::Uuid::new_v4());
    let ics = format!(
        "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//exchange_gateway//deploy-check//EN\r\n\
         BEGIN:VEVENT\r\nUID:{uid}\r\nDTSTAMP:20260301T000000Z\r\n\
         DTSTART;TZID=Europe/Berlin:20260401T090000\r\nDTEND;TZID=Europe/Berlin:20260401T100000\r\n\
         SUMMARY:Gateway create\r\nDESCRIPTION:first body\r\nLOCATION:Room One\r\n\
         END:VEVENT\r\nEND:VCALENDAR\r\n"
    );

    // Create — must return Ok (the helper verifies the read-back internally).
    let (event_id, returned_uid, _etag) = client
        .set_calendar_event(SetCalendarEventParams {
            account_id: &account,
            calendar_id: Some(&calendar_id),
            event_id: None,
            ics: &ics,
            username: &user,
            password: &pass,
        })
        .await
        .expect("gateway create must not discard the payload");
    assert!(!event_id.is_empty(), "create returned no event id");
    assert_eq!(returned_uid, uid, "create returned the wrong uid");

    // Update — title/description/location change must persist.
    let ics2 = ics
        .replace("SUMMARY:Gateway create", "SUMMARY:Gateway updated")
        .replace("DESCRIPTION:first body", "DESCRIPTION:second body")
        .replace("LOCATION:Room One", "LOCATION:Room Two");
    let (updated_id, _, _) = client
        .set_calendar_event(SetCalendarEventParams {
            account_id: &account,
            calendar_id: None,
            event_id: Some(&event_id),
            ics: &ics2,
            username: &user,
            password: &pass,
        })
        .await
        .expect("gateway update must not discard the payload");
    assert_eq!(updated_id, event_id, "update changed the event id");

    // Read back through the gateway's own reader and confirm the content.
    let (back_ics, _, _) = client
        .get_calendar_event(&account, &event_id, &user, &pass)
        .await
        .expect("get_calendar_event");
    let item = exchange_gateway::calendar::parse_ics_event(&back_ics).expect("parse read-back");
    assert_eq!(item.subject, "Gateway updated", "subject: {back_ics}");
    assert_eq!(item.description, "second body", "description: {back_ics}");
    assert_eq!(item.location, "Room Two", "location: {back_ics}");

    // Cleanup.
    client
        .destroy_calendar_events(&account, std::slice::from_ref(&event_id), &user, &pass)
        .await
        .expect("destroy");
}

/// Stalwart's `CalendarEvent/set` update is a partial patch: a property absent
/// from the payload keeps its previous value. `jscalendar_write_payload`
/// therefore nulls every clearable property the replacement ICS omits. This
/// proves the null-patch is accepted live and that removed content actually
/// disappears — otherwise a deleted description/location/attendee would survive
/// an edit (the exact stale-data hazard of the partial-patch semantics).
#[tokio::test]
async fn gateway_update_clears_omitted_content() {
    use exchange_gateway::jmap::SetCalendarEventParams;

    let Some((base, user, pass)) = env_config() else {
        eprintln!("STALWART_JMAP_URL/STALWART_USER/STALWART_PASS not set; skipping");
        return;
    };
    let client = JmapClient::new(&base).expect("JmapClient::new");
    let account = client
        .get_account_id(&user, &pass)
        .await
        .expect("account id");
    let calendars = client
        .query_calendars(&user, &pass)
        .await
        .expect("query_calendars");
    let calendar_id = calendars
        .calendars
        .first()
        .and_then(|c| c.id.clone())
        .expect("no calendar available");

    let uid = format!("gw-clear-check-{}", uuid::Uuid::new_v4());
    let rich = format!(
        "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//exchange_gateway//deploy-check//EN\r\n\
         BEGIN:VEVENT\r\nUID:{uid}\r\nDTSTAMP:20260301T000000Z\r\n\
         DTSTART:20260401T090000Z\r\nDTEND:20260401T100000Z\r\n\
         SUMMARY:Keep me\r\nDESCRIPTION:to be removed\r\nLOCATION:Room Gone\r\n\
         ATTENDEE;CN=Alice;PARTSTAT=ACCEPTED:mailto:alice@example.com\r\n\
         CATEGORIES:Work\r\n\
         END:VEVENT\r\nEND:VCALENDAR\r\n"
    );
    let (event_id, _, _) = client
        .set_calendar_event(SetCalendarEventParams {
            account_id: &account,
            calendar_id: Some(&calendar_id),
            event_id: None,
            ics: &rich,
            username: &user,
            password: &pass,
        })
        .await
        .expect("rich create");

    // Replace with a minimal event: description/location/attendee/categories
    // are all omitted and must be cleared by the null-patch.
    let minimal = format!(
        "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//exchange_gateway//deploy-check//EN\r\n\
         BEGIN:VEVENT\r\nUID:{uid}\r\nDTSTAMP:20260301T000000Z\r\n\
         DTSTART:20260401T090000Z\r\nDTEND:20260401T100000Z\r\n\
         SUMMARY:Keep me\r\n\
         END:VEVENT\r\nEND:VCALENDAR\r\n"
    );
    client
        .set_calendar_event(SetCalendarEventParams {
            account_id: &account,
            calendar_id: None,
            event_id: Some(&event_id),
            ics: &minimal,
            username: &user,
            password: &pass,
        })
        .await
        .expect("clearing update must be accepted");

    let (back_ics, _, _) = client
        .get_calendar_event(&account, &event_id, &user, &pass)
        .await
        .expect("get_calendar_event");
    let item = exchange_gateway::calendar::parse_ics_event(&back_ics).expect("parse read-back");
    assert_eq!(item.subject, "Keep me", "{back_ics}");
    assert!(
        item.description.is_empty(),
        "removed description survived the update: {back_ics}"
    );
    assert!(
        item.location.is_empty(),
        "removed location survived the update: {back_ics}"
    );
    assert!(
        item.attendees.is_empty(),
        "removed attendee survived the update: {back_ics}"
    );

    client
        .destroy_calendar_events(&account, std::slice::from_ref(&event_id), &user, &pass)
        .await
        .expect("destroy");
}

/// The read-back verification now compares every written content field, not
/// just the title. A false positive here would break calendar writes for
/// recurring/attendee/alarm events, so this drives a full-featured event
/// through the real `set_calendar_event` (which runs the verification
/// internally) and asserts it succeeds.
#[tokio::test]
async fn gateway_set_calendar_event_verifies_rich_content() {
    use exchange_gateway::jmap::SetCalendarEventParams;

    let Some((base, user, pass)) = env_config() else {
        eprintln!("STALWART_JMAP_URL/STALWART_USER/STALWART_PASS not set; skipping");
        return;
    };
    let client = JmapClient::new(&base).expect("JmapClient::new");
    let account = client
        .get_account_id(&user, &pass)
        .await
        .expect("account id");
    let calendars = client
        .query_calendars(&user, &pass)
        .await
        .expect("query_calendars");
    let calendar_id = calendars
        .calendars
        .first()
        .and_then(|c| c.id.clone())
        .expect("no calendar available");

    let uid = format!("gw-rich-check-{}", uuid::Uuid::new_v4());
    let ics = format!(
        "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//exchange_gateway//deploy-check//EN\r\n\
         BEGIN:VEVENT\r\nUID:{uid}\r\nDTSTAMP:20260301T000000Z\r\n\
         DTSTART;TZID=Europe/Berlin:20260401T090000\r\nDTEND;TZID=Europe/Berlin:20260401T100000\r\n\
         SUMMARY:Rich event\r\nDESCRIPTION:rich body\r\nLOCATION:Room Rich\r\n\
         RRULE:FREQ=WEEKLY;COUNT=4\r\nEXDATE;TZID=Europe/Berlin:20260408T090000\r\n\
         ORGANIZER;CN=Org:mailto:org@example.com\r\n\
         ATTENDEE;CN=Alice;PARTSTAT=ACCEPTED:mailto:alice@example.com\r\n\
         CATEGORIES:Work\r\nPRIORITY:5\r\nCLASS:PUBLIC\r\nTRANSP:OPAQUE\r\nSTATUS:CONFIRMED\r\n\
         BEGIN:VALARM\r\nACTION:DISPLAY\r\nTRIGGER:-PT15M\r\nDESCRIPTION:reminder\r\nEND:VALARM\r\n\
         END:VEVENT\r\nEND:VCALENDAR\r\n"
    );

    let (event_id, _, _) = client
        .set_calendar_event(SetCalendarEventParams {
            account_id: &account,
            calendar_id: Some(&calendar_id),
            event_id: None,
            ics: &ics,
            username: &user,
            password: &pass,
        })
        .await
        .expect("rich event create must pass the full read-back verification");

    // Update the same rich event: the verification runs on the update path too
    // (which additionally nulls the clearable properties the replacement ICS
    // omits), and must not produce a false positive.
    let ics2 = ics
        .replace("SUMMARY:Rich event", "SUMMARY:Rich updated")
        .replace("LOCATION:Room Rich", "LOCATION:Room Richer");
    let (updated_id, _, _) = client
        .set_calendar_event(SetCalendarEventParams {
            account_id: &account,
            calendar_id: None,
            event_id: Some(&event_id),
            ics: &ics2,
            username: &user,
            password: &pass,
        })
        .await
        .expect("rich event update must pass the full read-back verification");
    assert_eq!(updated_id, event_id, "update changed the event id");

    // The content the verification checked must also survive the gateway's own
    // reader end-to-end.
    let (back_ics, _, _) = client
        .get_calendar_event(&account, &event_id, &user, &pass)
        .await
        .expect("get_calendar_event");
    let item = exchange_gateway::calendar::parse_ics_event(&back_ics).expect("parse read-back");
    assert_eq!(item.subject, "Rich updated", "{back_ics}");
    assert_eq!(item.description, "rich body", "{back_ics}");
    assert_eq!(item.location, "Room Richer", "{back_ics}");
    assert!(item.rrule.is_some(), "RRULE lost: {back_ics}");
    assert_eq!(item.attendees.len(), 1, "attendee lost: {back_ics}");

    client
        .destroy_calendar_events(&account, std::slice::from_ref(&event_id), &user, &pass)
        .await
        .expect("destroy");
}
