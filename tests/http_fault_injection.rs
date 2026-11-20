use std::sync::Arc;

use exchange_gateway::auth::{AuthOutcome, AuthVerifier, CredentialCheck};
use exchange_gateway::caldav::CaldavClient;
use exchange_gateway::config::Config;
use exchange_gateway::eas_folders::delete_folder;
use exchange_gateway::jmap::JmapClient;
use exchange_gateway::models::AppState;
use exchange_gateway::storage::{FolderUpsertParams, Storage};
use secrecy::SecretString;
use serde_json::{Value, json};
use wiremock::matchers::{body_string_contains, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const OWNER: &str = "u@example.com";

fn password() -> SecretString {
    SecretString::from("pw".to_string())
}

fn session_json(base: &str) -> Value {
    json!({
        "apiUrl": base,
        "downloadUrl": format!("{base}/download/{{accountId}}/{{blobId}}"),
        "uploadUrl": format!("{base}/upload/{{accountId}}"),
        "eventSourceUrl": format!("{base}/eventsource"),
        "state": "fault-injection",
        "username": OWNER,
        "accounts": {
            "a": {
                "name": OWNER,
                "isPersonal": true,
                "isReadOnly": false,
                "accountCapabilities": {}
            }
        },
        "primaryAccounts": {"urn:ietf:params:jmap:mail": "a"},
        "capabilities": {}
    })
}

fn auth_config(server: &MockServer) -> Config {
    Config {
        jmap_base: server.uri(),
        caldav_base: server.uri(),
        auth_probe_min_interval_ms: 0,
        auth_cache_negative_ttl_secs: 300,
        auth_backend_failure_threshold: 50,
        ..Config::default()
    }
}

#[tokio::test]
async fn jmap_backend_errors_are_not_cached_and_auth_rejections_are_distinct() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/session"))
        .respond_with(ResponseTemplate::new(503))
        .expect(2)
        .mount(&server)
        .await;
    let client = JmapClient::new(&server.uri()).unwrap();
    for _ in 0..2 {
        assert!(matches!(
            client.probe_auth(OWNER, &password()).await,
            CredentialCheck::BackendError(_)
        ));
    }
    server.reset().await;

    Mock::given(method("GET"))
        .and(path("/session"))
        .respond_with(ResponseTemplate::new(401))
        .expect(1)
        .mount(&server)
        .await;
    assert_eq!(
        client.probe_auth(OWNER, &password()).await,
        CredentialCheck::Invalid
    );
    server.reset().await;

    Mock::given(method("GET"))
        .and(path("/session"))
        .respond_with(ResponseTemplate::new(200).set_body_string("not a JMAP session"))
        .expect(2)
        .mount(&server)
        .await;
    for _ in 0..2 {
        assert!(matches!(
            client
                .probe_auth("malformed@example.com", &password())
                .await,
            CredentialCheck::BackendError(_)
        ));
    }
}

#[tokio::test]
async fn jmap_403_is_an_authoritative_rejection_at_the_client_boundary() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/session"))
        .respond_with(ResponseTemplate::new(403))
        .expect(1)
        .mount(&server)
        .await;
    let client = JmapClient::new(&server.uri()).unwrap();
    assert_eq!(
        client.check_credentials(OWNER, &password()).await,
        CredentialCheck::Invalid
    );
}

#[tokio::test]
async fn caldav_403_is_a_backend_error_and_is_never_negative_cached() {
    let server = MockServer::start().await;
    Mock::given(method("PROPFIND"))
        .and(path(format!("/cal/{OWNER}/")))
        .respond_with(ResponseTemplate::new(403))
        .expect(3)
        .mount(&server)
        .await;
    let mut config = auth_config(&server);
    config.jmap_base.clear();
    let verifier = AuthVerifier::new(&config);
    for _ in 0..2 {
        assert_eq!(
            verifier.verify_outcome(OWNER, "pw").await,
            AuthOutcome::BackendUnavailable
        );
    }

    let client = CaldavClient::new(&config).unwrap();
    assert!(matches!(
        client.check_credentials(OWNER, "pw").await,
        CredentialCheck::BackendError(_)
    ));
}

#[tokio::test]
async fn jmap_outage_short_circuits_without_caldav_amplification() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/session"))
        .respond_with(ResponseTemplate::new(503))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("PROPFIND"))
        .respond_with(ResponseTemplate::new(207))
        .expect(0)
        .mount(&server)
        .await;
    let verifier = AuthVerifier::new(&auth_config(&server));
    assert_eq!(
        verifier.verify_outcome(OWNER, "pw").await,
        AuthOutcome::BackendUnavailable
    );
}

#[tokio::test]
async fn authoritative_jmap_rejection_allows_caldav_arbitration() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/session"))
        .respond_with(ResponseTemplate::new(403))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("PROPFIND"))
        .and(path(format!("/cal/{OWNER}/")))
        .respond_with(ResponseTemplate::new(207))
        .expect(1)
        .mount(&server)
        .await;
    let verifier = AuthVerifier::new(&auth_config(&server));
    assert_eq!(
        verifier.verify_outcome(OWNER, "pw").await,
        AuthOutcome::Verified
    );
}

#[tokio::test]
async fn mailbox_set_503_mid_delete_preserves_mapping_and_prune_intent() {
    let server = MockServer::start().await;
    let base = server.uri();
    Mock::given(method("GET"))
        .and(path("/session"))
        .respond_with(ResponseTemplate::new(200).set_body_json(session_json(&base)))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/"))
        .and(body_string_contains("Mailbox/set"))
        .respond_with(ResponseTemplate::new(503))
        .expect(1)
        .mount(&server)
        .await;

    let storage = Arc::new(Storage::new("sqlite::memory:").await.unwrap());
    storage.init_schema().await.unwrap();
    storage
        .upsert_folder_journaled(
            FolderUpsertParams {
                owner: OWNER,
                server_id: "mb-fault",
                parent_id: "0",
                display_name: "Fault injection",
                folder_type: 12,
                backend_kind: "jmap",
                backend_id: "mb-fault",
            },
            "folder-add",
        )
        .await
        .unwrap();
    let state = Arc::new(AppState::new(
        Config {
            jmap_base: base,
            caldav_base: String::new(),
            email_enabled: true,
            mail_domain: "example.com".to_string(),
            hmac_secret: SecretString::from("a".repeat(32)),
            ..Config::default()
        },
        storage.clone(),
    ));

    let result = delete_folder(&state, OWNER, &password(), "mb-fault").await;
    assert_eq!(result.status, 6);
    assert!(
        storage
            .get_folder(OWNER, "mb-fault")
            .await
            .unwrap()
            .is_some(),
        "an unknown destroy outcome must not prune the mirror"
    );
    assert_eq!(
        storage.list_pending_folder_prunes(OWNER).await.unwrap(),
        vec!["mb-fault".to_string()],
        "the unknown outcome must remain replayable"
    );
}
