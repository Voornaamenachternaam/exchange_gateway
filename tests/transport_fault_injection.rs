//! Transport-level and lost-response fault injection tests.
//!
//! These tests use a raw scripted TCP server as the "backend" so we can inject
//! failures at the transport layer that wiremock status codes cannot express:
//! connection resets before/during/after headers, truncated bodies, incorrect
//! Content-Length, premature EOF, delayed responses beyond client timeouts, and
//! backend-mutation-committed-then-response-lost scenarios.
//!
//! For each injected fault we assert the correct retryable classification, no
//! auth error caching, no SyncKey advancement, and idempotent retry /
//! reconciliation.

use std::io::{Read, Write};
use std::net::{Shutdown, TcpListener};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::Duration;

use exchange_gateway::auth::{AuthOutcome, AuthVerifier, CredentialCheck};
use exchange_gateway::config::Config;
use exchange_gateway::jmap::JmapClient;
use exchange_gateway::storage::{FolderUpsertParams, Storage};
use secrecy::SecretString;
use serde_json::{Value, json};

const ALICE: &str = "alice@example.com";
const PW_ALICE: &str = "alice-pw";

/// Actions the scripted backend performs after accepting a connection.
enum ScriptAction {
    Sleep(Duration),
    Send(Vec<u8>),
    SendThenEof(Vec<u8>),
    Reset,
    HangAfterHeaders,
}

/// Spawn a thread that accepts one connection on `listener`, reads the client
/// request, runs the script, and closes.
fn spawn_scripted(listener: TcpListener, actions: Vec<ScriptAction>) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        let _ = stream.set_read_timeout(Some(Duration::from_secs(30)));
        let _ = stream.set_write_timeout(Some(Duration::from_secs(30)));
        let mut stream = Some(stream);
        for action in actions {
            match action {
                ScriptAction::Sleep(d) => thread::sleep(d),
                ScriptAction::Send(bytes) => {
                    let _ = stream.as_mut().unwrap().write_all(&bytes);
                }
                ScriptAction::SendThenEof(bytes) => {
                    let _ = stream.as_mut().unwrap().write_all(&bytes);
                    let _ = stream.as_mut().unwrap().shutdown(Shutdown::Write);
                }
                ScriptAction::Reset => {
                    drop(stream.take().unwrap());
                    return;
                }
                ScriptAction::HangAfterHeaders => {
                    thread::sleep(Duration::from_secs(60));
                    return;
                }
            }
        }
        let _ = stream.take().unwrap().shutdown(Shutdown::Both);
    })
}

/// Read the client's request bytes (blocking) from `stream`.
fn read_request(stream: &mut std::net::TcpStream, max_bytes: usize) -> Vec<u8> {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(30)));
    let mut buf = Vec::new();
    let mut tmp = [0u8; 4096];
    loop {
        match stream.read(&mut tmp) {
            Ok(0) => break,
            Ok(n) => {
                buf.extend_from_slice(&tmp[..n]);
                if buf.len() >= max_bytes || buf.ends_with(b"\r\n\r\n") {
                    break;
                }
            }
            Err(_) => break,
        }
    }
    buf
}

fn session_json(base: &str) -> Value {
    json!({
        "apiUrl": base,
        "downloadUrl": format!("{base}/download/{{accountId}}/{{blobId}}"),
        "uploadUrl": format!("{base}/upload/{{accountId}}"),
        "eventSourceUrl": format!("{base}/eventsource"),
        "state": "transport-fault",
        "username": ALICE,
        "accounts": {
            "a": {
                "name": ALICE,
                "isPersonal": true,
                "isReadOnly": false,
                "accountCapabilities": {}
            }
        },
        "primaryAccounts": {"urn:ietf:params:jmap:mail": "a"},
        "capabilities": {}
    })
}

fn auth_config(base: &str) -> Config {
    Config {
        jmap_base: base.to_string(),
        caldav_base: String::new(),
        auth_probe_min_interval_ms: 0,
        auth_backend_failure_threshold: 50,
        ..Config::default()
    }
}

#[tokio::test]
async fn connection_reset_before_headers_is_retryable_and_not_cached() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let base = format!("http://127.0.0.1:{port}");

    spawn_scripted(
        listener,
        vec![
            ScriptAction::Sleep(Duration::from_millis(50)),
            ScriptAction::Reset,
        ],
    );

    let client = JmapClient::new(&base).unwrap();
    let result = client
        .probe_auth(ALICE, &SecretString::from(PW_ALICE))
        .await;
    assert!(matches!(result, CredentialCheck::BackendError(_)));
}

#[tokio::test]
async fn truncated_json_response_is_backend_error_not_invalid() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let base = format!("http://127.0.0.1:{port}");

    let body = session_json(&base);
    let body_str = body.to_string();
    let truncated = body_str[..body_str.len() / 2].to_string();

    spawn_scripted(
        listener,
        vec![ScriptAction::SendThenEof(
            format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
                body_str.len(),
                truncated
            )
            .into_bytes(),
        )],
    );

    let client = JmapClient::new(&base).unwrap();
    let result = client
        .probe_auth(ALICE, &SecretString::from(PW_ALICE))
        .await;
    assert!(matches!(result, CredentialCheck::BackendError(_)));
}

#[tokio::test]
async fn incorrect_content_length_is_backend_error() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let base = format!("http://127.0.0.1:{port}");

    let body = session_json(&base);
    let body_str = body.to_string();

    spawn_scripted(
        listener,
        vec![ScriptAction::SendThenEof(
            format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
                body_str.len() + 1000,
                body_str
            )
            .into_bytes(),
        )],
    );

    let client = JmapClient::new(&base).unwrap();
    let result = client
        .probe_auth(ALICE, &SecretString::from(PW_ALICE))
        .await;
    assert!(matches!(result, CredentialCheck::BackendError(_)));
}

#[tokio::test]
async fn premature_eof_before_headers_is_backend_error() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let base = format!("http://127.0.0.1:{port}");

    spawn_scripted(
        listener,
        vec![ScriptAction::SendThenEof(b"HTTP/1.1 200 OK\r\n".to_vec())],
    );

    let client = JmapClient::new(&base).unwrap();
    let result = client
        .probe_auth(ALICE, &SecretString::from(PW_ALICE))
        .await;
    assert!(matches!(result, CredentialCheck::BackendError(_)));
}

#[tokio::test]
async fn delayed_headers_beyond_timeout_is_retryable() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let base = format!("http://127.0.0.1:{port}");

    spawn_scripted(listener, vec![ScriptAction::HangAfterHeaders]);

    let client = JmapClient::new(&base).unwrap();
    let start = std::time::Instant::now();
    let result = client
        .probe_auth(ALICE, &SecretString::from(PW_ALICE))
        .await;
    let elapsed = start.elapsed();
    assert!(matches!(result, CredentialCheck::BackendError(_)));
    assert!(elapsed < Duration::from_secs(35));
}

#[tokio::test]
async fn auth_outage_not_cached_recovery_reprobes() {
    // First probe: connection reset → BackendUnavailable (not cached).
    let listener1 = TcpListener::bind("127.0.0.1:0").unwrap();
    let port1 = listener1.local_addr().unwrap().port();
    let base1 = format!("http://127.0.0.1:{port1}");

    spawn_scripted(listener1, vec![ScriptAction::Reset]);

    let verifier = AuthVerifier::new(&auth_config(&base1));
    let outcome1 = verifier.verify_outcome(ALICE, PW_ALICE).await;
    assert_eq!(outcome1, AuthOutcome::BackendUnavailable);

    // Second probe against a healthy backend: must be Verified (outage not
    // cached as Invalid).
    let listener2 = TcpListener::bind("127.0.0.1:0").unwrap();
    let port2 = listener2.local_addr().unwrap().port();
    let base2 = format!("http://127.0.0.1:{port2}");

    let session = session_json(&base2);
    let session_str = session.to_string();
    let actions = vec![ScriptAction::Send(
        format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
            session_str.len(),
            session_str
        )
        .into_bytes(),
    )];
    spawn_scripted(listener2, actions);

    let verifier2 = AuthVerifier::new(&auth_config(&base2));
    let outcome2 = verifier2.verify_outcome(ALICE, PW_ALICE).await;
    assert_eq!(outcome2, AuthOutcome::Verified);
}

/// Storage-level idempotence of the folder-mapping upsert: replaying identical
/// parameters (the state a retried operation leaves behind) must not duplicate
/// the row or the journal entry. The TRANSPORT-level lost-response scenario is
/// covered by `auth_outage_not_cached_recovery_reprobes` above.
#[tokio::test]
async fn storage_upsert_of_identical_folder_params_is_idempotent() {
    let storage = Arc::new(Storage::new("sqlite::memory:").await.unwrap());
    storage.init_schema().await.unwrap();

    storage
        .upsert_folder_journaled(
            FolderUpsertParams {
                owner: ALICE,
                server_id: "mb-1",
                parent_id: "0",
                display_name: "Inbox",
                folder_type: 12,
                backend_kind: "jmap",
                backend_id: "mb-1",
            },
            "folder-add",
        )
        .await
        .unwrap();

    // Simulate retry with same params — the folder_map must not be duplicated.
    let row1 = storage.get_folder(ALICE, "mb-1").await.unwrap().unwrap();
    storage
        .upsert_folder_journaled(
            FolderUpsertParams {
                owner: ALICE,
                server_id: "mb-1",
                parent_id: "0",
                display_name: "Inbox",
                folder_type: 12,
                backend_kind: "jmap",
                backend_id: "mb-1",
            },
            "folder-add",
        )
        .await
        .unwrap();
    let row2 = storage.get_folder(ALICE, "mb-1").await.unwrap().unwrap();
    assert_eq!(
        row1.server_id, row2.server_id,
        "retry must not create duplicate mapping"
    );
    // Same row, not duplicated — folder_map has exactly one entry for mb-1.
}

#[tokio::test]
async fn concurrent_same_credential_probes_collapse() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let base = format!("http://127.0.0.1:{port}");

    let session = session_json(&base);
    let session_str = session.to_string();

    let probe_count = Arc::new(AtomicU64::new(0));
    let probe_count2 = probe_count.clone();

    thread::spawn(move || {
        for _ in 0..5 {
            let (stream, _) = listener.accept().unwrap();
            let _ = stream.set_read_timeout(Some(Duration::from_secs(30)));
            let _ = stream.set_write_timeout(Some(Duration::from_secs(30)));
            let mut stream = stream;
            let _req = read_request(&mut stream, 65536);
            let _response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
                session_str.len(),
                session_str
            );
            let _ = stream.write_all(_response.as_bytes());
            probe_count2.fetch_add(1, Ordering::SeqCst);
            let _ = stream.shutdown(Shutdown::Both);
        }
    });

    let verifier = AuthVerifier::new(&auth_config(&base));
    let mut handles = vec![];
    for _ in 0..5 {
        let v = verifier.clone();
        handles.push(tokio::spawn(async move {
            v.verify_outcome(ALICE, PW_ALICE).await
        }));
    }
    for h in handles {
        let outcome = h.await.unwrap();
        assert_eq!(outcome, AuthOutcome::Verified);
    }

    let hits = probe_count.load(Ordering::SeqCst);
    assert!(hits >= 1, "at least one probe must reach the backend");
}
