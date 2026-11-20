use std::panic::{AssertUnwindSafe, catch_unwind};

use arbitrary::{Arbitrary, Unstructured};
use chrono::{DateTime, Utc};
use exchange_gateway::auth::{AuthOutcome, AuthVerifier};
use exchange_gateway::calendar::{CalendarItem, parse_ics_event, render_ics};
use exchange_gateway::config::Config;
use exchange_gateway::eas_sync_options::{
    CollectionLevelControls, EasBodyPreference, EasSyncCollectionOptions, parse_collection_options,
};
use exchange_gateway::ical_parser::parse_all_vevents;
use exchange_gateway::wbxml::Wbxml;
use proptest::prelude::*;

fn xml_text() -> impl Strategy<Value = String> {
    prop::sample::select(vec![
        String::new(),
        "plain ASCII".to_string(),
        "<>&\"'".to_string(),
        "élan 東京 🙂".to_string(),
        "0123456789abcdef".to_string(),
    ])
}

fn identifier() -> impl Strategy<Value = String> {
    "[a-z][a-z0-9-]{0,23}"
}

fn ical_text() -> impl Strategy<Value = String> {
    prop::collection::vec(
        prop::sample::select(vec!['a', 'Z', '0', ' ', ',', ';', '\\', '\n', 'é', '🙂']),
        0..40,
    )
    .prop_map(|chars| chars.into_iter().collect())
}

fn wbxml_document() -> impl Strategy<Value = String> {
    (
        prop::collection::vec((xml_text(), xml_text()), 0..5),
        any::<bool>(),
    )
        .prop_map(|(collections, declare_namespace)| {
            let collections = collections
                .into_iter()
                .map(|(key, id)| {
                    format!(
                        "<Collection><Class>Email</Class><SyncKey>{}</SyncKey><CollectionId>{}</CollectionId></Collection>",
                        quick_xml::escape::escape(&key),
                        quick_xml::escape::escape(&id),
                    )
                })
                .collect::<String>();
            let namespace = if declare_namespace {
                " xmlns=\"AirSync:\""
            } else {
                ""
            };
            format!("<Sync{namespace}><Collections>{collections}</Collections></Sync>")
        })
}

fn assert_wbxml_round_trip(xml: &str) -> Result<(), TestCaseError> {
    fn codec_result<T>(result: anyhow::Result<T>) -> Result<T, TestCaseError> {
        result.map_err(|error| TestCaseError::fail(error.to_string()))
    }

    let codec = Wbxml::new();
    let first = codec_result(codec.encode(xml))?;
    let decoded = codec_result(codec.decode(&first))?;
    let second = codec_result(codec.encode(&decoded))?;
    prop_assert_eq!(
        &second,
        &first,
        "encode → decode → encode changed the wire bytes\nsource: {:?}\ndecoded: {:?}",
        xml,
        decoded
    );
    let decoded_again = codec_result(codec.decode(&second))?;
    prop_assert_eq!(decoded_again, decoded);
    Ok(())
}

#[derive(Arbitrary, Debug)]
struct AdversarialInput {
    wbxml: Vec<u8>,
    ical: String,
    options: String,
}

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 64,
        max_shrink_iters: 1024,
        failure_persistence: None,
        ..ProptestConfig::default()
    })]

    #[test]
    fn generated_wbxml_documents_round_trip_byte_exactly(xml in wbxml_document()) {
        assert_wbxml_round_trip(&xml)?;
    }

    #[test]
    fn arbitrary_wbxml_bytes_are_fail_closed_and_never_panic(bytes in prop::collection::vec(any::<u8>(), 0..768)) {
        let codec = Wbxml::new();
        let decoded = catch_unwind(AssertUnwindSafe(|| codec.decode(&bytes)));
        prop_assert!(decoded.is_ok(), "WBXML decoder panicked");
        if let Ok(xml) = decoded.unwrap() {
            // Any successful decode that the encoder accepts must have one
            // canonical re-encoding. Plain-XML pass-through is intentionally
            // excluded: malformed plain XML is rejected by the encoder.
            if !bytes.first().is_some_and(|b| *b == b'<')
                && let Ok(encoded) = codec.encode(&xml)
            {
                let decoded_again = codec.decode(&encoded)
                    .map_err(|error| TestCaseError::fail(error.to_string()))?;
                let encoded_again = codec.encode(&decoded_again)
                    .map_err(|error| TestCaseError::fail(error.to_string()))?;
                prop_assert_eq!(encoded_again, encoded);
            }
        }
    }

    #[test]
    fn generated_calendar_events_preserve_instants_across_render_and_parse(
        uid in identifier(),
        subject in ical_text(),
        description in ical_text(),
        location in ical_text(),
        start_secs in 1_600_000_000i64..1_900_000_000,
        duration_secs in 60i64..604_800,
        timezone in prop::option::of(prop::sample::select(vec![
            "UTC",
            "Europe/Berlin",
            "America/New_York",
            "Pacific/Apia",
        ])),
    ) {
        let start = DateTime::<Utc>::from_timestamp(start_secs, 0).unwrap();
        let end = DateTime::<Utc>::from_timestamp(start_secs + duration_secs, 0).unwrap();
        let item = CalendarItem {
            uid,
            subject,
            description,
            location,
            start,
            end,
            dtstamp: Some(start),
            timezone: timezone.map(str::to_string),
            ..CalendarItem::default()
        };
        let first = render_ics(&item);
        let parsed = parse_ics_event(&first).ok_or_else(|| {
            TestCaseError::fail(format!("generated event did not parse:\n{first}"))
        })?;
        prop_assert_eq!(parsed.start.timestamp(), start.timestamp());
        prop_assert_eq!(parsed.end.timestamp(), end.timestamp());
        prop_assert_eq!(parsed.uid.as_str(), item.uid.as_str());
        prop_assert_eq!(parsed.subject.as_str(), item.subject.as_str());
        prop_assert_eq!(parsed.description.as_str(), item.description.as_str());
        prop_assert_eq!(parsed.location.as_str(), item.location.as_str());

        let second = render_ics(&parsed);
        let reparsed = parse_ics_event(&second).ok_or_else(|| {
            TestCaseError::fail(format!("re-rendered event did not parse:\n{second}"))
        })?;
        prop_assert_eq!(reparsed.start.timestamp(), start.timestamp());
        prop_assert_eq!(reparsed.end.timestamp(), end.timestamp());
    }

    #[test]
    fn malformed_icalendar_text_never_panics(input in any::<String>()) {
        let parsed = catch_unwind(AssertUnwindSafe(|| {
            let _ = parse_ics_event(&input);
            let _ = parse_all_vevents(&input);
        }));
        prop_assert!(parsed.is_ok(), "iCalendar parser panicked");
    }

    #[test]
    fn generated_eas_option_matrices_parse_exactly_and_remain_sticky(
        filter_type in 0u8..=7,
        body_preferences in prop::collection::vec(
            (1u8..=2, prop::option::of(any::<u32>()), prop::option::of(any::<bool>()), prop::option::of(any::<u8>())),
            0..3,
        ),
        mime_support in prop::option::of(0u8..=2),
        mime_truncation in prop::option::of(0u8..=8),
        max_items in prop::option::of(any::<u32>()),
        conflict in prop::option::of(0u8..=1),
        rights_management_support in any::<bool>(),
        conversation_mode in any::<bool>(),
        deletes_as_moves in any::<bool>(),
    ) {
        let body_xml = body_preferences
            .iter()
            .map(|(body_type, truncation, all_or_none, preview)| {
                format!(
                    "<AirSyncBase:BodyPreference><AirSyncBase:Type>{body_type}</AirSyncBase:Type>{}{}{}</AirSyncBase:BodyPreference>",
                    truncation.map(|v| format!("<AirSyncBase:TruncationSize>{v}</AirSyncBase:TruncationSize>")).unwrap_or_default(),
                    all_or_none.map(|v| format!("<AirSyncBase:AllOrNone>{}</AirSyncBase:AllOrNone>", u8::from(v))).unwrap_or_default(),
                    preview.map(|v| format!("<AirSyncBase:Preview>{v}</AirSyncBase:Preview>")).unwrap_or_default(),
                )
            })
            .collect::<String>();
        let xml = format!(
            "<Collection><ConversationMode>{}</ConversationMode><DeletesAsMoves>{}</DeletesAsMoves><Options><FilterType>{filter_type}</FilterType>{body_xml}{}{}{}{}{}{}</Options></Collection>",
            u8::from(conversation_mode),
            u8::from(deletes_as_moves),
            mime_support.map(|v| format!("<MIMESupport>{v}</MIMESupport>")).unwrap_or_default(),
            mime_truncation.map(|v| format!("<MIMETruncation>{v}</MIMETruncation>")).unwrap_or_default(),
            max_items.map(|v| format!("<MaxItems>{v}</MaxItems>")).unwrap_or_default(),
            conflict.map(|v| format!("<Conflict>{v}</Conflict>")).unwrap_or_default(),
            if rights_management_support { "<RightsManagementSupport>1</RightsManagementSupport>" } else { "" },
            "",
        );
        let parsed = parse_collection_options(&xml);
        let expected_body = body_preferences
            .iter()
            .map(|(body_type, truncation_size, all_or_none, preview)| EasBodyPreference {
                body_type: *body_type,
                truncation_size: *truncation_size,
                all_or_none: *all_or_none,
                preview: *preview,
            })
            .collect::<Vec<_>>();
        prop_assert_eq!(parsed.options.filter_type, Some(filter_type));
        prop_assert_eq!(&parsed.options.body_preferences, &expected_body);
        prop_assert_eq!(parsed.options.mime_support, mime_support);
        prop_assert_eq!(parsed.options.mime_truncation, mime_truncation);
        prop_assert_eq!(parsed.options.max_items, max_items);
        prop_assert_eq!(parsed.options.conflict, conflict);
        prop_assert_eq!(parsed.options.rights_management_support, rights_management_support);
        prop_assert!(parsed.options.explicitly_set);
        prop_assert_eq!(parsed.controls, CollectionLevelControls {
            conversation_mode: Some(conversation_mode),
            deletes_as_moves: Some(deletes_as_moves),
        });

        let serialized = serde_json::to_string(&parsed.options).unwrap();
        let restored: EasSyncCollectionOptions = serde_json::from_str(&serialized).unwrap();
        prop_assert_eq!(&restored.body_preferences, &parsed.options.body_preferences);
        prop_assert_eq!(restored.filter_type, parsed.options.filter_type);
        prop_assert!(!restored.explicitly_set, "request-local presence must not be persisted");
        prop_assert_eq!(
            EasSyncCollectionOptions::resolve_sticky(Some(restored), Some(parsed.options.clone())),
            Some(parsed.options.clone())
        );
    }

    #[test]
    fn malformed_eas_option_xml_never_panics(input in any::<String>()) {
        let xml = format!("<Collection><Options>{input}</Options></Collection>");
        let parsed = catch_unwind(AssertUnwindSafe(|| parse_collection_options(&xml)));
        prop_assert!(parsed.is_ok(), "EAS options parser panicked");
        prop_assert!(parsed.unwrap().options.explicitly_set);
    }
}

#[test]
fn arbitrary_derived_raw_inputs_exercise_all_three_parsers_without_panics() {
    let mut seed = 0x6a09_e667_f3bc_c909u64;
    for _ in 0..256 {
        let mut entropy = [0u8; 512];
        for byte in &mut entropy {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            *byte = seed as u8;
        }
        let mut unstructured = Unstructured::new(&entropy);
        let Ok(input) = AdversarialInput::arbitrary(&mut unstructured) else {
            continue;
        };
        let result = catch_unwind(AssertUnwindSafe(|| {
            let codec = Wbxml::new();
            let _ = codec.decode(&input.wbxml);
            let _ = parse_ics_event(&input.ical);
            let _ = parse_all_vevents(&input.ical);
            let _ = parse_collection_options(&input.options);
        }));
        assert!(result.is_ok(), "derived adversarial input caused a panic");
    }
}

#[test]
fn arbitrary_credentials_have_only_the_documented_unconfigured_outcomes() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .unwrap();
    let verifier = AuthVerifier::new(&Config {
        jmap_base: String::new(),
        caldav_base: String::new(),
        auth_probe_min_interval_ms: 0,
        ..Config::default()
    });
    proptest!(
        ProptestConfig {
            failure_persistence: None,
            ..ProptestConfig::default()
        },
        |(username in any::<String>(), password in any::<String>())| {
            let outcome = runtime.block_on(verifier.verify_outcome(&username, &password));
            let expected = if username.is_empty() || password.is_empty() {
                AuthOutcome::InvalidCredentials
            } else {
                AuthOutcome::BackendUnavailable
            };
            prop_assert_eq!(outcome, expected);
        }
    );
}
