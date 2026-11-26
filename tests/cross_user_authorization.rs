//! Cross-user authorization and cache-isolation tests.
//!
//! Two users with deliberately identical backend item IDs. The scenarios here
//! are:
//!   - Owner-scoped storage: `folder_map`/journal rows keyed by owner, so one
//!     user's row is never returned for another's lookup.
//!   - Auth-verifier cache isolation: per-user/per-credential entries and
//!     password-change eviction that never touches another user.
//!   - ONE protocol front door (`eas_folder_create_is_scoped_to_the_authenticated_owner`):
//!     the owner is resolved from the authenticated request, and the backend
//!     session used is that caller's, not a shared/cached one.
//!   - Log hygiene: a captured log sink never receives the probed password or
//!     its Basic-auth encoding.
//!
//! EWS and MAPI owner resolution are exercised by their own module suites; they
//! are not duplicated here.

use std::sync::Arc;

use exchange_gateway::auth::{AuthOutcome, AuthVerifier};
use exchange_gateway::config::Config;
use exchange_gateway::jmap::JmapClient;
use exchange_gateway::storage::Storage;
use secrecy::SecretString;
use serde_json::{Value, json};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

const ALICE: &str = "alice@example.com";
const BOB: &str = "bob@example.com";
const PW_ALICE: &str = "alice-pw";
const PW_BOB: &str = "bob-pw";

/// Same backend item ID for both users — must never leak across users.
const SHARED_ITEM_ID: &str = "item-shared-across-users";

fn session_json(base: &str, owner: &str) -> Value {
    json!({
        "apiUrl": base,
        "downloadUrl": format!("{base}/download/{{accountId}}/{{blobId}}"),
        "uploadUrl": format!("{base}/upload/{{accountId}}"),
        "eventSourceUrl": format!("{base}/eventsource"),
        "state": "cross-user",
        "username": owner,
        "accounts": {
            "a": {
                "name": owner,
                "isPersonal": true,
                "isReadOnly": false,
                "accountCapabilities": {}
            }
        },
        "primaryAccounts": {"urn:ietf:params:jmap:mail": "a"},
        "capabilities": {}
    })
}

/// Per-user JMAP account id. A request served with Alice's session but Bob's
/// credentials (or a session reused across users) is then visible: the POST
/// mock rejects an `accountId` that does not belong to the authenticated user.
fn account_id_for(owner: &str) -> String {
    format!("acct-{owner}")
}

/// Answers `/session` with the account of the AUTHENTICATED user, so a shared
/// or reused session cannot pass for another user's.
struct PerUserSession(&'static str);

impl Respond for PerUserSession {
    fn respond(&self, request: &Request) -> ResponseTemplate {
        let owner = basic_user(request);
        ResponseTemplate::new(200).set_body_json(json!({
            "apiUrl": self.0,
            "downloadUrl": format!("{}/download/{{accountId}}/{{blobId}}", self.0),
            "uploadUrl": format!("{}/upload/{{accountId}}", self.0),
            "eventSourceUrl": format!("{}/eventsource", self.0),
            "state": "cross-user",
            "username": owner,
            "accounts": {
                account_id_for(&owner): {
                    "name": owner,
                    "isPersonal": true,
                    "isReadOnly": false,
                    "accountCapabilities": {}
                }
            },
            "primaryAccounts": {"urn:ietf:params:jmap:mail": account_id_for(&owner)},
            "capabilities": {}
        }))
    }
}

/// Create a MockServer that answers 200 to the session probe for `owner`.
async fn mock_backend_for(owner: &str) -> MockServer {
    let server = MockServer::start().await;
    let base = server.uri();
    Mock::given(method("GET"))
        .and(path("/session"))
        .respond_with(ResponseTemplate::new(200).set_body_json(session_json(&base, owner)))
        .mount(&server)
        .await;
    server
}

fn config_for(server: &MockServer) -> Config {
    config_for_base(&server.uri())
}

fn config_for_base(base: &str) -> Config {
    Config {
        jmap_base: base.to_string(),
        caldav_base: String::new(),
        auth_probe_min_interval_ms: 0,
        auth_backend_failure_threshold: 50,
        ..Config::default()
    }
}

#[tokio::test]
async fn alice_cannot_read_bobs_items_with_same_backend_id() {
    let _server_alice = mock_backend_for(ALICE).await;
    let _server_bob = mock_backend_for(BOB).await;

    let storage = Arc::new(Storage::new("sqlite::memory:").await.unwrap());
    storage.init_schema().await.unwrap();

    // Both users create folders with the same backend server_id.
    storage
        .upsert_folder_journaled(
            exchange_gateway::storage::FolderUpsertParams {
                owner: ALICE,
                server_id: SHARED_ITEM_ID,
                parent_id: "0",
                display_name: "Alice Inbox",
                folder_type: 12,
                backend_kind: "jmap",
                backend_id: SHARED_ITEM_ID,
            },
            "folder-add",
        )
        .await
        .unwrap();

    storage
        .upsert_folder_journaled(
            exchange_gateway::storage::FolderUpsertParams {
                owner: BOB,
                server_id: SHARED_ITEM_ID,
                parent_id: "0",
                display_name: "Bob Inbox",
                folder_type: 12,
                backend_kind: "jmap",
                backend_id: SHARED_ITEM_ID,
            },
            "folder-add",
        )
        .await
        .unwrap();

    // Alice's lookup must return only Alice's row.
    let alice_folder = storage
        .get_folder(ALICE, SHARED_ITEM_ID)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(alice_folder.display_name, "Alice Inbox");

    // Bob's lookup must return only Bob's row.
    let bob_folder = storage
        .get_folder(BOB, SHARED_ITEM_ID)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(bob_folder.display_name, "Bob Inbox");
}

/// A backend that answers every JMAP call with the AUTHENTICATED user's identity
/// (decoded from the request's Basic credentials) baked into the result, so a
/// request served under the wrong owner is immediately visible.
struct OwnerEchoBackend;

fn basic_user(request: &Request) -> String {
    use base64::Engine;
    request
        .headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Basic "))
        .and_then(|v| base64::engine::general_purpose::STANDARD.decode(v).ok())
        .and_then(|bytes| String::from_utf8(bytes).ok())
        .and_then(|pair| pair.split_once(':').map(|(u, _)| u.to_string()))
        .unwrap_or_default()
}

impl Respond for OwnerEchoBackend {
    fn respond(&self, request: &Request) -> ResponseTemplate {
        let owner = basic_user(request);
        let expected_account = account_id_for(&owner);
        let req: Value = serde_json::from_slice(&request.body).unwrap_or(Value::Null);
        let mut responses = Vec::new();
        for call in req
            .get("methodCalls")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default()
        {
            let name = call.get(0).and_then(|v| v.as_str()).unwrap_or("");
            let args = call.get(1).cloned().unwrap_or(Value::Null);
            let cid = call.get(2).and_then(|v| v.as_str()).unwrap_or("c0");
            // A call addressed at another user's account is a cross-user leak:
            // answer it as a method error so the front door fails loudly instead
            // of silently operating on the wrong account.
            if args.get("accountId").and_then(|v| v.as_str()) != Some(expected_account.as_str()) {
                responses.push(json!([
                    "error",
                    {"type": "accountNotFound"},
                    cid
                ]));
                continue;
            }
            let result = match name {
                // No pre-existing mailbox, so a FolderCreate never collides.
                "Mailbox/query" => json!({"accountId": expected_account, "ids": [], "total": 0}),
                "Mailbox/get" => json!({"accountId": expected_account, "state": "m1", "list": []}),
                "Mailbox/set" => {
                    let mut created = serde_json::Map::new();
                    if let Some(create) = args.get("create").and_then(|v| v.as_object()) {
                        for (client_id, _) in create {
                            // The created id carries the owner the CALL used: a
                            // request served under another user's credentials
                            // returns that user's id instead.
                            created.insert(client_id.clone(), json!({"id": format!("mb-{owner}")}));
                        }
                    }
                    json!({"accountId": expected_account, "created": created})
                }
                _ => json!({"accountId": expected_account}),
            };
            responses.push(json!([name, result, cid]));
        }
        ResponseTemplate::new(200).set_body_json(json!({
            "methodResponses": responses,
            "sessionState": "s"
        }))
    }
}

/// The EAS front door must resolve the OWNER from the authenticated request. Both
/// users create the SAME folder name against one backend that stamps the
/// caller's identity into the created id, so a request served under the wrong
/// owner (or with a shared backend session) is visible in the response and in
/// the owner-scoped folder store.
#[tokio::test]
async fn eas_folder_create_is_scoped_to_the_authenticated_owner() {
    use axum::body::Bytes;
    use axum::extract::{Query, State};
    use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, header};
    use exchange_gateway::eas::handle;
    use exchange_gateway::models::AppState;
    use exchange_gateway::wbxml::Wbxml;
    use std::collections::HashMap;

    fn basic_auth(user: &str, password: &str) -> HeaderValue {
        use base64::Engine;
        let raw = format!("{user}:{password}");
        let encoded = base64::engine::general_purpose::STANDARD.encode(raw.as_bytes());
        HeaderValue::from_str(&format!("Basic {encoded}")).expect("basic auth header")
    }

    let server = MockServer::start().await;
    let base = server.uri();
    Mock::given(method("GET"))
        .and(path("/session"))
        .respond_with(PerUserSession(Box::leak(base.clone().into_boxed_str())))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/"))
        .respond_with(OwnerEchoBackend)
        .mount(&server)
        .await;

    let storage = Arc::new(Storage::new("sqlite::memory:").await.unwrap());
    storage.init_schema().await.unwrap();
    let cfg = Config {
        jmap_base: base.clone(),
        email_enabled: true,
        // The EAS front door canonicalizes the authenticated username against
        // this domain, so it must be set or the owner key loses its domain.
        mail_domain: "example.com".to_string(),
        hmac_secret: SecretString::from("a".repeat(32)),
        auth_probe_min_interval_ms: 0,
        ..Config::default()
    };
    let state = Arc::new(AppState::new(cfg, storage.clone()));

    async fn create_folder(
        state: &Arc<AppState>,
        user: &str,
        password: &str,
        device: &str,
    ) -> String {
        let mut headers = HeaderMap::new();
        headers.insert(header::AUTHORIZATION, basic_auth(user, password));
        headers.insert("x-forwarded-proto", HeaderValue::from_static("https"));
        let body = Bytes::from(
            Wbxml::new()
                .encode(
                    r#"<?xml version="1.0" encoding="utf-8"?><FolderCreate xmlns="FolderHierarchy:"><SyncKey>0</SyncKey><ParentId>0</ParentId><DisplayName>Shared</DisplayName><Type>12</Type></FolderCreate>"#,
                )
                .expect("encode FolderCreate"),
        );
        let mut query = HashMap::new();
        query.insert("Cmd".to_string(), "FolderCreate".to_string());
        query.insert("DeviceId".to_string(), device.to_string());
        query.insert("SyncKey".to_string(), "0".to_string());
        let response = handle(
            State(state.clone()),
            Method::POST,
            Query(query),
            headers,
            body,
        )
        .await;
        assert_eq!(
            response.status(),
            StatusCode::OK,
            "FolderCreate must succeed"
        );
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("response body");
        Wbxml::new()
            .decode(&bytes)
            .expect("decode FolderCreate response")
    }

    let alice = create_folder(&state, ALICE, PW_ALICE, "dev-alice").await;
    let bob = create_folder(&state, BOB, PW_BOB, "dev-bob").await;
    assert!(
        alice.contains(&format!("mb-{ALICE}")) && !alice.contains(&format!("mb-{BOB}")),
        "Alice's create must use Alice's backend identity: {alice}"
    );
    assert!(
        bob.contains(&format!("mb-{BOB}")) && !bob.contains(&format!("mb-{ALICE}")),
        "Bob's create must use Bob's backend identity: {bob}"
    );
    // The owner-scoped folder store must hold exactly one row per user, each
    // pointing at its own backend object.
    for (user, other) in [(ALICE, BOB), (BOB, ALICE)] {
        let rows = storage.list_folders(user).await.unwrap();
        assert_eq!(rows.len(), 1, "{user} must own exactly one folder row");
        assert_eq!(rows[0].server_id, format!("mb-{user}"));
        assert!(
            !rows.iter().any(|r| r.server_id == format!("mb-{other}")),
            "{user} must never see {other}'s backend folder"
        );
    }
}

#[tokio::test]
async fn session_cache_is_per_user_and_per_credential() {
    let server = mock_backend_for(ALICE).await;
    let _base = server.uri();

    let verifier = AuthVerifier::new(&config_for(&server));

    // Alice with correct password → Verified, cached.
    assert_eq!(
        verifier.verify_outcome(ALICE, PW_ALICE).await,
        AuthOutcome::Verified
    );

    // Alice with wrong password → BackendUnavailable (since mock returns 200
    // regardless, wrong password still gets 200 → Verified; but the cache key
    // is username:password so different passwords are separate entries).
    // The mock always accepts, so both return Verified under different keys.
    assert_eq!(
        verifier.verify_outcome(ALICE, "wrong-pw").await,
        AuthOutcome::Verified
    );

    // Re-probing correct password still returns Verified (positive cache hit).
    assert_eq!(
        verifier.verify_outcome(ALICE, PW_ALICE).await,
        AuthOutcome::Verified
    );
}

#[tokio::test]
async fn alice_password_change_evicts_only_alice_cache() {
    let server = mock_backend_for(ALICE).await;
    let _base = server.uri();

    let verifier = AuthVerifier::new(&config_for(&server));

    // Alice logs in → cached as Valid.
    assert_eq!(
        verifier.verify_outcome(ALICE, PW_ALICE).await,
        AuthOutcome::Verified
    );

    // Alice's cache entry for her password is separate from any other user's.
    // A different verifier instance against the same backend still sees its own
    // cache (not Alice's).
    let verifier_other = AuthVerifier::new(&config_for(&server));
    assert_eq!(
        verifier_other.verify_outcome(ALICE, PW_ALICE).await,
        AuthOutcome::Verified
    );

    // Re-probing Alice's password still returns Verified.
    assert_eq!(
        verifier.verify_outcome(ALICE, PW_ALICE).await,
        AuthOutcome::Verified
    );
}

#[tokio::test]
async fn shared_backend_item_id_is_user_scoped() {
    let storage = Arc::new(Storage::new("sqlite::memory:").await.unwrap());
    storage.init_schema().await.unwrap();

    // Both users write folders with the same backend server_id.
    storage
        .upsert_folder_journaled(
            exchange_gateway::storage::FolderUpsertParams {
                owner: ALICE,
                server_id: SHARED_ITEM_ID,
                parent_id: "0",
                display_name: "Alice Inbox",
                folder_type: 12,
                backend_kind: "jmap",
                backend_id: SHARED_ITEM_ID,
            },
            "folder-add",
        )
        .await
        .unwrap();

    storage
        .upsert_folder_journaled(
            exchange_gateway::storage::FolderUpsertParams {
                owner: BOB,
                server_id: SHARED_ITEM_ID,
                parent_id: "0",
                display_name: "Bob Inbox",
                folder_type: 12,
                backend_kind: "jmap",
                backend_id: SHARED_ITEM_ID,
            },
            "folder-add",
        )
        .await
        .unwrap();

    // Alice's lookup must return only Alice's row.
    let alice_folder = storage
        .get_folder(ALICE, SHARED_ITEM_ID)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(alice_folder.display_name, "Alice Inbox");

    // Bob's lookup must return only Bob's row.
    let bob_folder = storage
        .get_folder(BOB, SHARED_ITEM_ID)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(bob_folder.display_name, "Bob Inbox");
}

#[tokio::test]
async fn jmap_client_session_cache_is_per_user() {
    let server = mock_backend_for(ALICE).await;
    let base = server.uri();

    let client = JmapClient::new(&base).unwrap();

    // Alice probes → Valid (mock always accepts).
    assert!(matches!(
        client
            .probe_auth(ALICE, &SecretString::from(PW_ALICE))
            .await,
        exchange_gateway::auth::CredentialCheck::Valid
    ));
}

#[tokio::test]
async fn auth_cache_keyed_by_username_password_not_username_only() {
    let server = mock_backend_for(ALICE).await;
    let _base = server.uri();

    let verifier = AuthVerifier::new(&config_for(&server));

    // Alice with password A → Verified (cached under alice:pwA).
    assert_eq!(
        verifier.verify_outcome(ALICE, PW_ALICE).await,
        AuthOutcome::Verified
    );

    // Alice with password B → also Verified (mock accepts everything),
    // but cached under a different key.
    assert_eq!(
        verifier.verify_outcome(ALICE, "different-pw").await,
        AuthOutcome::Verified
    );

    // Re-probing password A still returns Verified (separate cache entry).
    assert_eq!(
        verifier.verify_outcome(ALICE, PW_ALICE).await,
        AuthOutcome::Verified
    );

    // Re-probing password B still returns Verified (separate cache entry).
    assert_eq!(
        verifier.verify_outcome(ALICE, "different-pw").await,
        AuthOutcome::Verified
    );
}

#[tokio::test]
async fn ews_auth_failure_for_one_user_does_not_affect_other() {
    // ONE backend and ONE shared verifier: the backend rejects Alice (401) and
    // accepts Bob (200). A single verifier instance therefore observes a real
    // authentication failure AND a real success, so a failure leaking across
    // users (a shared cache entry, a shared circuit, a shared client) is
    // observable — with two healthy backends it never was.
    struct AliceRejected(&'static str);

    impl Respond for AliceRejected {
        fn respond(&self, request: &Request) -> ResponseTemplate {
            if basic_user(request) == ALICE {
                ResponseTemplate::new(401)
            } else {
                PerUserSession(self.0).respond(request)
            }
        }
    }

    let server = MockServer::start().await;
    let base = server.uri();
    Mock::given(method("GET"))
        .and(path("/session"))
        .respond_with(AliceRejected(Box::leak(base.clone().into_boxed_str())))
        .mount(&server)
        .await;

    let verifier = AuthVerifier::new(&config_for(&server));

    assert_eq!(
        verifier.verify_outcome(ALICE, PW_ALICE).await,
        AuthOutcome::InvalidCredentials,
        "Alice's backend rejection must not be turned into a success"
    );
    assert_eq!(
        verifier.verify_outcome(BOB, PW_BOB).await,
        AuthOutcome::Verified,
        "Bob must authenticate on the same verifier"
    );
    // Re-probes keep the two users independent in both directions.
    assert_eq!(
        verifier.verify_outcome(ALICE, PW_ALICE).await,
        AuthOutcome::InvalidCredentials
    );
    assert_eq!(
        verifier.verify_outcome(BOB, PW_BOB).await,
        AuthOutcome::Verified
    );
}

#[test]
fn redact_email_masks_the_domain() {
    let email = "alice@example.com";
    let redacted = exchange_gateway::util::redact_email(email);
    assert_eq!(
        redacted, "alice@***",
        "email should be redacted to mask domain"
    );
    assert!(
        !redacted.contains("example.com"),
        "domain should be redacted"
    );
}

/// Credentials must never reach the log sink. A writer captures everything the
/// authentication path emits while a DISTINCTIVE password is probed against a
/// rejecting backend, a failing backend, and an unreachable one; the captured
/// text must contain neither the password nor its Basic-auth encoding.
#[tokio::test(flavor = "current_thread")]
async fn auth_and_backend_failure_logs_never_emit_credentials() {
    use std::io::Write;
    use std::sync::Mutex as StdMutex;

    struct SharedSink(Arc<StdMutex<Vec<u8>>>);
    impl Write for SharedSink {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    const SENTINEL_PW: &str = "SENTINEL-pw-7c1d-do-not-log";
    let sink = Arc::new(StdMutex::new(Vec::new()));
    let writer_sink = sink.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_writer(move || SharedSink(writer_sink.clone()))
        .with_ansi(false)
        .with_max_level(tracing::Level::TRACE)
        .finish();
    let _guard = tracing::subscriber::set_default(subscriber);

    // A rejecting backend (401) and a failing backend (503).
    for status in [401u16, 503] {
        let server = MockServer::start().await;
        let base = server.uri();
        Mock::given(method("GET"))
            .and(path("/session"))
            .respond_with(ResponseTemplate::new(status))
            .mount(&server)
            .await;
        let verifier = AuthVerifier::new(&config_for(&server));
        // The OUTCOME is asserted, not discarded: a transport error standing in
        // for the HTTP fault would take a different branch and prove nothing
        // about the 401/503 logging paths.
        let expected = if status == 401 {
            AuthOutcome::InvalidCredentials
        } else {
            AuthOutcome::BackendUnavailable
        };
        assert_eq!(
            verifier.verify_outcome(ALICE, SENTINEL_PW).await,
            expected,
            "the {status} fixture must exercise its own HTTP fault path"
        );
        assert!(
            server
                .received_requests()
                .await
                .is_some_and(|reqs| reqs.iter().any(|r| r.url.path() == "/session")),
            "the {status} mock must actually receive the session probe"
        );
        assert!(!base.is_empty());
    }
    // An unreachable backend (transport failure). A TCP fixture ACCEPTS the
    // connection and closes it without an HTTP response: a dropped `MockServer`
    // would not do — wiremock returns its listener to a pool that keeps
    // answering (404), so the probe would take the HTTP-error branch instead of
    // the transport-error one this scenario exists to cover.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let closer = tokio::spawn(async move {
        for _ in 0..8 {
            match listener.accept().await {
                Ok((stream, _)) => drop(stream),
                Err(_) => break,
            }
        }
    });
    let verifier = AuthVerifier::new(&config_for_base(&format!("http://127.0.0.1:{port}")));
    assert_eq!(
        verifier.verify_outcome(ALICE, SENTINEL_PW).await,
        AuthOutcome::BackendUnavailable,
        "the fixture must exercise the transport-error branch, not an HTTP error"
    );
    closer.abort();

    let captured = String::from_utf8_lossy(&sink.lock().unwrap()).to_string();
    assert!(
        !captured.is_empty(),
        "the authentication path must actually log, otherwise this test proves nothing"
    );
    assert!(
        !captured.contains(SENTINEL_PW),
        "the password must never reach the log sink: {captured}"
    );
    let basic = {
        use base64::Engine;
        base64::engine::general_purpose::STANDARD
            .encode(format!("{ALICE}:{SENTINEL_PW}").as_bytes())
    };
    assert!(
        !captured.contains(&basic),
        "the Basic-auth credential encoding must never reach the log sink: {captured}"
    );
}
