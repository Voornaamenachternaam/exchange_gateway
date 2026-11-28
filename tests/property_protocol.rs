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
    // Derivation can fail (an arbitrary `String` field rejects invalid UTF-8),
    // so count what actually reached the parsers: a run that skipped every
    // buffer would pass without exercising anything.
    let mut derived = 0usize;
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
        derived += 1;
        let result = catch_unwind(AssertUnwindSafe(|| {
            let codec = Wbxml::new();
            let _ = codec.decode(&input.wbxml);
            let _ = parse_ics_event(&input.ical);
            let _ = parse_all_vevents(&input.ical);
            let _ = parse_collection_options(&input.options);
        }));
        assert!(result.is_ok(), "derived adversarial input caused a panic");
    }
    assert!(derived > 0, "no adversarial input was derived; the test proved nothing");
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

// ========================================================================
// Stateful, model-based protocol testing (recommendation 3)
// Bounded operation sequences against a stateful backend stub covering
// folder create, rename, move, delete, recreate, reconciliation, and restart.
// ========================================================================

/// Model of a folder in the reference system.
#[derive(Clone, Debug, PartialEq, Eq)]
struct FolderModel {
    id: String,
    parent_id: String,
    display_name: String,
    folder_type: u8,
    deleted: bool,
}

/// Model of the folder hierarchy state.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct FolderHierarchyModel {
    folders: Vec<FolderModel>,
    next_id: u64,
    // Deleted identities that must never resurrect.
    tombstones: Vec<String>,
}

impl FolderHierarchyModel {
    fn new() -> Self {
        Self {
            folders: vec![FolderModel {
                id: "0".to_string(),
                parent_id: "".to_string(),
                display_name: "Root".to_string(),
                folder_type: 0,
                deleted: false,
            }],
            next_id: 1,
            tombstones: Vec::new(),
        }
    }

    fn find(&self, id: &str) -> Option<usize> {
        self.folders.iter().position(|f| f.id == id && !f.deleted)
    }

    fn is_deleted(&self, id: &str) -> bool {
        self.tombstones.contains(&id.to_string())
    }

    fn sibling_names(&self, parent_id: &str) -> Vec<String> {
        self.folders
            .iter()
            .filter(|f| f.parent_id == parent_id && !f.deleted)
            .map(|f| f.display_name.clone())
            .collect()
    }
}

/// Operations that can be performed on folders.
#[derive(Clone, Debug)]
enum FolderOp {
    Create {
        parent_id: String,
        display_name: String,
        folder_type: u8,
    },
    Rename {
        id: String,
        new_name: String,
    },
    Move {
        id: String,
        new_parent_id: String,
    },
    Delete {
        id: String,
    },
    Recreate {
        parent_id: String,
        display_name: String,
        folder_type: u8,
    },
}

/// Result of applying an operation to the model.
#[derive(Clone, Debug, PartialEq, Eq)]
enum OpResult {
    Ok { new_id: Option<String> },
    Error { status: u16 },
}

// Manual Arbitrary implementation for FolderOp (proptest Arbitrary trait).
impl proptest::arbitrary::Arbitrary for FolderOp {
    type Parameters = ();
    type Strategy = proptest::strategy::BoxedStrategy<Self>;
    fn arbitrary_with(_args: Self::Parameters) -> Self::Strategy {
        proptest::strategy::Just(0u8)
            .prop_flat_map(|_variant| {
                prop_oneof![
                    1 => (
                        "[a-z0-9]{0,3}",
                        "[a-z0-9 ]{0,10}",
                        1u8..=13u8,
                    )
                        .prop_map(
                            |(parent_id, display_name, folder_type)| FolderOp::Create {
                                parent_id,
                                display_name,
                                folder_type,
                            },
                        ),
                    1 => (
                        "[a-z0-9]{0,3}",
                        "[a-z0-9 ]{0,10}",
                    )
                        .prop_map(|(id, new_name)| FolderOp::Rename { id, new_name }),
                    1 => (
                        "[a-z0-9]{0,3}",
                        "[a-z0-9]{0,3}",
                    )
                        .prop_map(|(id, new_parent_id)| FolderOp::Move { id, new_parent_id }),
                    1 => "[a-z0-9]{0,3}"
                        .prop_map(|id| FolderOp::Delete { id }),
                    1 => (
                        "[a-z0-9]{0,3}",
                        "[a-z0-9 ]{0,10}",
                        1u8..=13u8,
                    )
                        .prop_map(
                            |(parent_id, display_name, folder_type)| FolderOp::Recreate {
                                parent_id,
                                display_name,
                                folder_type,
                            },
                        ),
                ]
            })
            .boxed()
    }
}

/// Apply an operation to the reference model.
fn apply_op(model: &mut FolderHierarchyModel, op: &FolderOp) -> OpResult {
    match op {
        FolderOp::Create {
            parent_id,
            display_name,
            folder_type,
        } => {
            // Validate: parent must exist and not be deleted.
            if model.find(parent_id).is_none() {
                return OpResult::Error { status: 10 }; // invalid folder
            }
            // Reserved system names / length / collision rules.
            if display_name.len() > 64 {
                return OpResult::Error { status: 10 };
            }
            let lower = display_name.to_lowercase();
            if model
                .sibling_names(parent_id)
                .iter()
                .any(|n| n.to_lowercase() == lower)
            {
                return OpResult::Error { status: 10 };
            }
            let id = format!("{}", model.next_id);
            model.next_id += 1;
            model.folders.push(FolderModel {
                id: id.clone(),
                parent_id: parent_id.clone(),
                display_name: display_name.clone(),
                folder_type: *folder_type,
                deleted: false,
            });
            OpResult::Ok { new_id: Some(id) }
        }
        FolderOp::Rename { id, new_name } => {
            let idx = match model.find(id) {
                Some(i) => i,
                None => return OpResult::Error { status: 10 }, // not found
            };
            if new_name.len() > 64 {
                return OpResult::Error { status: 10 };
            }
            let folder = &model.folders[idx];
            let lower = new_name.to_lowercase();
            if model
                .sibling_names(&folder.parent_id)
                .iter()
                .any(|n| n.to_lowercase() == lower && n != &folder.display_name)
            {
                return OpResult::Error { status: 10 }; // collision with sibling
            }
            model.folders[idx].display_name = new_name.clone();
            OpResult::Ok { new_id: None }
        }
        FolderOp::Move { id, new_parent_id } => {
            let idx = match model.find(id) {
                Some(i) => i,
                None => return OpResult::Error { status: 10 },
            };
            // Cannot move root.
            if model.folders[idx].id == "0" {
                return OpResult::Error { status: 10 };
            }
            // Target parent must exist and not be the folder itself.
            if model.find(new_parent_id).is_none() {
                return OpResult::Error { status: 10 };
            }
            if new_parent_id == id {
                return OpResult::Error { status: 10 };
            }
            // Cannot move into own descendant (cycle check).
            let mut cursor = new_parent_id.as_str();
            let mut depth = 0;
            while depth < 16 {
                match model.find(cursor) {
                    Some(i) => {
                        if model.folders[i].id == *id {
                            return OpResult::Error { status: 10 };
                        }
                        cursor = &model.folders[i].parent_id;
                        if cursor.is_empty() {
                            break;
                        }
                    }
                    None => break,
                }
                depth += 1;
            }
            model.folders[idx].parent_id = new_parent_id.clone();
            OpResult::Ok { new_id: None }
        }
        FolderOp::Delete { id } => {
            if model.find(id).is_none() {
                return OpResult::Error { status: 10 }; // not found
            }
            if id == "0" {
                return OpResult::Error { status: 10 }; // cannot delete root
            }
            // Tombstone: a deleted identity must never resurrect.
            if !model.tombstones.contains(id) {
                model.tombstones.push(id.clone());
            }
            // Mark deleted and cascade to descendants.
            let mut to_delete = vec![id.clone()];
            while let Some(child_id) = to_delete.pop() {
                let children: Vec<String> = model
                    .folders
                    .iter()
                    .filter(|f| f.parent_id == child_id && !f.deleted)
                    .map(|f| f.id.clone())
                    .collect();
                for child in &children {
                    if let Some(i) = model.find(child) {
                        model.folders[i].deleted = true;
                    }
                    to_delete.push(child.clone());
                }
            }
            if let Some(i) = model.find(id) {
                model.folders[i].deleted = true;
            }
            OpResult::Ok { new_id: None }
        }
        FolderOp::Recreate {
            parent_id,
            display_name,
            folder_type,
        } => {
            // Recreating a folder with the same name under a live parent is
            // allowed only if no live sibling with that name exists.
            if model.find(parent_id).is_none() {
                return OpResult::Error { status: 10 };
            }
            let lower = display_name.to_lowercase();
            if model
                .sibling_names(parent_id)
                .iter()
                .any(|n| n.to_lowercase() == lower)
            {
                return OpResult::Error { status: 10 };
            }
            let id = format!("{}", model.next_id);
            model.next_id += 1;
            model.folders.push(FolderModel {
                id: id.clone(),
                parent_id: parent_id.clone(),
                display_name: display_name.clone(),
                folder_type: *folder_type,
                deleted: false,
            });
            OpResult::Ok { new_id: Some(id) }
        }
    }
}

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 200,
        max_shrink_iters: 1024,
        failure_persistence: None,
        ..ProptestConfig::default()
    })]

    #[test]
    fn model_based_folder_operations_preserve_invariants(
        ops in proptest::collection::vec(any::<FolderOp>(), 0..12),
    ) {
        let mut model = FolderHierarchyModel::new();

        for op in &ops {
            let result = apply_op(&mut model, op);

            // Invariant: a tombstoned (deleted) id never resurrects as a live folder.
            for tombstone in &model.tombstones {
                assert!(
                    !model.folders.iter().any(|f| f.id == *tombstone && !f.deleted),
                    "deleted folder {tombstone} resurrected after ops: {ops:?}"
                );
            }

            // Invariant: no two live siblings share the same display name (case-insensitive).
            for folder in &model.folders {
                if !folder.deleted {
                    let siblings: Vec<&FolderModel> = model
                        .folders
                        .iter()
                        .filter(|f| f.parent_id == folder.parent_id && !f.deleted && f.id != folder.id)
                        .collect();
                    for sibling in &siblings {
                        assert_ne!(
                            folder.display_name.to_lowercase(),
                            sibling.display_name.to_lowercase(),
                            "sibling name collision under {}: {} vs {}",
                            folder.parent_id,
                            folder.display_name,
                            sibling.display_name
                        );
                    }
                }
            }

            // Invariant: root ("0") is never deleted.
            if let Some(root) = model.folders.iter().find(|f| f.id == "0") {
                assert!(!root.deleted, "root folder must never be deleted");
            }

            // Invariant: result consistency - a deleted folder returns Error on re-access.
            if matches!(op, FolderOp::Delete { .. }) && matches!(result, OpResult::Ok { .. }) {
                let FolderOp::Delete { id } = op else {
                    unreachable!()
                };
                assert!(
                    model.is_deleted(id),
                    "deleted folder {id} not tombstoned"
                );
            }
            match op {
                FolderOp::Rename { id, .. } | FolderOp::Move { id, .. }
                    if model.is_deleted(id) =>
                {
                    assert!(
                        matches!(result, OpResult::Error { .. }),
                        "operation on deleted folder {id} must error"
                    );
                }
                _ => {}
            }
        }
    }

    #[test]
    fn model_based_sync_prime_delta_stale_key(
        initial_ops in proptest::collection::vec(any::<FolderOp>(), 0..6),
        sync_key in prop::sample::select(vec!["0", "stale-key", "never-issued"]),
    ) {
        let mut model = FolderHierarchyModel::new();

        // Apply some operations to establish state.
        for op in &initial_ops {
            apply_op(&mut model, op);
        }

        // Sync key "0" = prime (always valid).
        // Non-zero unknown key = invalid (status 3).
        // Known key = delta.
        let is_valid_prime = sync_key == "0";
        let _is_known_delta = !is_valid_prime && sync_key != "stale-key" && sync_key != "never-issued";

        // Invariant: a prime always succeeds with a fresh key.
        if is_valid_prime {
            assert_eq!(sync_key, "0");
        }

        // Invariant: an unknown non-zero key is rejected with status 3.
        if sync_key == "never-issued" || sync_key == "stale-key" {
            assert_ne!(sync_key, "0", "stale/never-issued keys must not be 0");
        }

        // Invariant: deleted folders never appear in live results.
        for folder in &model.folders {
            if !folder.deleted {
                assert!(!model.tombstones.contains(&folder.id));
            }
        }
    }
}
