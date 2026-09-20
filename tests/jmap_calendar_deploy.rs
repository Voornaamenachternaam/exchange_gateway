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
    // Verified live on stalwartlabs/stalwart:v0.16.22: an `iCalendar`
    // payload in CalendarEvent/set is silently discarded. When that is the
    // case the deploy check MUST flag it (issue + not healthy) — this is
    // exactly the incompatibility the verification exists to catch, since
    // `JmapClient::set_calendar_event` writes events via that property.
    if !report.ics_round_trip_ok {
        assert!(
            report.issues.iter().any(|i| i.contains("iCalendar")),
            "iCalendar round-trip failure not flagged in issues: {:?}",
            report.issues
        );
        assert!(
            !report.is_healthy(),
            "deploy check must refuse healthy status when the gateway's ICS write path is broken"
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
    let create = json!({
        "accountId": account,
        "create": {
            "ev1": {
                "calendarIds": { calendar_id: true },
                "uid": "jmap-cal-deploy-check-001",
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
    assert!(
        set_args.get("notCreated").is_none(),
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
        Value::String("jmap-cal-deploy-check-001".to_string()),
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
