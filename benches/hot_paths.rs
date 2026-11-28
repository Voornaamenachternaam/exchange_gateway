use std::collections::HashMap;
use std::hint::black_box;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use criterion::{Criterion, Throughput, criterion_group, criterion_main};
use exchange_gateway::auth::{AuthOutcome, AuthVerifier};
use exchange_gateway::config::Config;
use exchange_gateway::eas_sync_options::EasSyncCollectionOptions;
use exchange_gateway::email::render_jmap_email_as_eas_application_data_with_options;
use exchange_gateway::jmap::{JmapBodyPart, JmapBodyValue, JmapClient, JmapEmail};
use exchange_gateway::jscalendar::{ics_to_jscalendar_event, jscalendar_event_to_ics};
use exchange_gateway::wbxml::Wbxml;
use futures_util::future::join_all;
use serde_json::json;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// Start a mock server that never retains request history: the auth benchmark
/// reuses one server across every iteration and never reads the recording, so
/// the default per-request bookkeeping would grow the measured workload.
async fn start_unrecorded_mock_server() -> MockServer {
    MockServer::builder()
        .disable_request_recording()
        .start()
        .await
}

fn wbxml_source(collections: usize) -> String {
    let collections = (0..collections)
        .map(|n| {
            format!(
                "<Collection><Class>Email</Class><SyncKey>key-{n}</SyncKey><CollectionId>mailbox-{n}</CollectionId></Collection>"
            )
        })
        .collect::<String>();
    format!("<Sync><Collections>{collections}</Collections></Sync>")
}

fn bench_wbxml(c: &mut Criterion) {
    let codec = Wbxml::new();
    let xml = wbxml_source(64);
    let encoded = codec.encode(&xml).unwrap();
    let mut group = c.benchmark_group("wbxml_codec");
    group.throughput(Throughput::Bytes(encoded.len() as u64));
    group.bench_function("encode_64_collections", |b| {
        b.iter(|| codec.encode(black_box(&xml)).unwrap())
    });
    group.bench_function("decode_64_collections", |b| {
        b.iter(|| codec.decode(black_box(&encoded)).unwrap())
    });
    group.bench_function("round_trip_64_collections", |b| {
        b.iter(|| {
            let encoded = codec.encode(black_box(&xml)).unwrap();
            codec.decode(black_box(&encoded)).unwrap()
        })
    });
    group.finish();
}

fn bench_jscalendar(c: &mut Criterion) {
    let ics = concat!(
        "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nBEGIN:VEVENT\r\n",
        "UID:bench-event\r\nDTSTAMP:20261120T000000Z\r\n",
        "DTSTART:20261121T090000Z\r\nDTEND:20261121T100000Z\r\n",
        "SUMMARY:Criterion benchmark\r\nDESCRIPTION:JSCalendar conversion hot path\r\n",
        "LOCATION:Room A\r\nRRULE:FREQ=WEEKLY;COUNT=12\r\n",
        "ATTENDEE;CN=Alice:mailto:alice@example.com\r\n",
        "END:VEVENT\r\nEND:VCALENDAR\r\n"
    );
    let event = ics_to_jscalendar_event(ics).expect("benchmark fixture converts");
    c.bench_function("jscalendar_event_to_ics", |b| {
        b.iter(|| jscalendar_event_to_ics(black_box(&event)).expect("event converts"))
    });
}

fn benchmark_email(index: usize) -> JmapEmail {
    let body_id = "1".to_string();
    JmapEmail {
        id: Some(format!("email-{index}")),
        blob_id: Some(format!("blob-{index}")),
        thread_id: Some(format!("thread-{index}")),
        mailbox_ids: Some(HashMap::from([("mailbox-1".to_string(), true)])),
        keywords: Some(HashMap::from([("$seen".to_string(), true)])),
        received_at: Some("2026-11-20T12:00:00Z".to_string()),
        subject: Some(format!("Delta message {index}")),
        preview: Some("A representative message preview".to_string()),
        body_values: Some(HashMap::from([(
            body_id.clone(),
            JmapBodyValue {
                value: "The quick brown fox jumps over the lazy dog. ".repeat(32),
                is_encoding_problem: Some(false),
                is_truncated: Some(false),
            },
        )])),
        text_body: Some(vec![JmapBodyPart {
            part_id: body_id,
            blob_id: None,
            size: None,
            content_type: Some("text/plain".to_string()),
            charset: Some("utf-8".to_string()),
        }]),
        from: Some(
            serde_json::from_value(json!([{
                "name": "Benchmark Sender",
                "email": "sender@example.com"
            }]))
            .unwrap(),
        ),
        to: Some(
            serde_json::from_value(json!([{
                "name": "Benchmark Recipient",
                "email": "recipient@example.com"
            }]))
            .unwrap(),
        ),
        ..JmapEmail::default()
    }
}

fn bench_sync_delta_render(c: &mut Criterion) {
    let options = EasSyncCollectionOptions::default();
    let mut group = c.benchmark_group("sync_delta_application_data");
    for count in [16usize, 64, 256] {
        let emails = (0..count).map(benchmark_email).collect::<Vec<_>>();
        group.throughput(Throughput::Elements(count as u64));
        group.bench_with_input(format!("render_{count}_emails"), &emails, |b, emails| {
            b.iter(|| {
                let rendered = emails
                    .iter()
                    .enumerate()
                    .map(|(index, email)| {
                        render_jmap_email_as_eas_application_data_with_options(
                            black_box(email),
                            &format!("email-{index}"),
                            "mailbox-1",
                            black_box(&options),
                            false,
                            true,
                        )
                    })
                    .collect::<Vec<_>>();
                black_box(rendered)
            })
        });
    }
    group.finish();
}

fn bench_auth_probe_burst(c: &mut Criterion) {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let (verifier, _server) = runtime.block_on(async {
        let server = start_unrecorded_mock_server().await;
        let base = server.uri();
        Mock::given(method("GET"))
            .and(path("/session"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "apiUrl": base,
                "downloadUrl": format!("{base}/download/{{accountId}}/{{blobId}}"),
                "uploadUrl": format!("{base}/upload/{{accountId}}"),
                "eventSourceUrl": format!("{base}/eventsource"),
                "state": "benchmark",
                "username": "benchmark",
                "accounts": {},
                "primaryAccounts": {},
                "capabilities": {}
            })))
            .mount(&server)
            .await;
        let client = Arc::new(JmapClient::new(&base).unwrap());
        let config = Config {
            jmap_base: base,
            caldav_base: String::new(),
            auth_probe_min_interval_ms: 0,
            auth_cache_max_entries: 100_000,
            ..Config::default()
        };
        (
            AuthVerifier::new_from_topology(&config, Some(client)),
            server,
        )
    });
    let generation = AtomicU64::new(0);
    c.bench_function("auth_cold_concurrent_probe_burst_32", |b| {
        b.to_async(&runtime).iter(|| {
            let verifier = verifier.clone();
            let generation = generation.fetch_add(1, Ordering::Relaxed);
            async move {
                let outcomes = join_all((0..32).map(|index| {
                    let verifier = verifier.clone();
                    async move {
                        verifier
                            .verify_outcome(&format!("bench-{generation}-{index}"), "benchmark")
                            .await
                    }
                }))
                .await;
                assert!(outcomes.iter().all(|o| *o == AuthOutcome::Verified));
                black_box(outcomes)
            }
        })
    });
}

criterion_group!(
    hot_paths,
    bench_wbxml,
    bench_jscalendar,
    bench_sync_delta_render,
    bench_auth_probe_burst
);
criterion_main!(hot_paths);
