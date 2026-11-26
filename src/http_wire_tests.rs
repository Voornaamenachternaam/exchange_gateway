use std::sync::Arc;
use std::time::Duration;

use exchange_gateway::Config;
use exchange_gateway::models::AppState;
use exchange_gateway::notifications::NotificationEvent;
use exchange_gateway::storage::{Storage, TaskFields};
use exchange_gateway::wbxml::Wbxml;
use reqwest::{StatusCode, header};
use roxmltree::Document;
use secrecy::SecretString;
use serde_json::{Value, json};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use super::build_router;

const OWNER: &str = "u@example.com";
const PASSWORD: &str = "wire-test-password";

struct WireGateway {
    base: String,
    device_id: String,
    client: reqwest::Client,
    backend: MockServer,
    state: Arc<AppState>,
    server_task: tokio::task::JoinHandle<()>,
}

impl Drop for WireGateway {
    fn drop(&mut self) {
        self.server_task.abort();
    }
}

impl WireGateway {
    async fn spawn() -> Self {
        let backend = MockServer::start().await;
        let backend_base = backend.uri();
        Mock::given(method("GET"))
            .and(path("/session"))
            .respond_with(ResponseTemplate::new(200).set_body_json(session_json(&backend_base)))
            .mount(&backend)
            .await;

        let storage = Arc::new(Storage::new("sqlite::memory:").await.unwrap());
        storage.init_schema().await.unwrap();
        let state = Arc::new(AppState::new(
            Config {
                jmap_base: backend_base,
                caldav_base: String::new(),
                email_enabled: true,
                mail_domain: "example.com".to_string(),
                mail_host: "mail.example.com".to_string(),
                gateway_host: "gateway.example.com".to_string(),
                hmac_secret: SecretString::from("a".repeat(32)),
                max_ping_heartbeat_secs: Some(5),
                mapi_enabled: false,
                ..Config::default()
            },
            storage,
        ));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let app = build_router(state.clone());
        let server_task = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .build()
            .unwrap();
        Self {
            base: format!("http://{address}"),
            device_id: format!("wire-{}", uuid::Uuid::new_v4().simple()),
            client,
            backend,
            state,
            server_task,
        }
    }

    async fn eas(&self, command: &str, xml: &str) -> EasWireResponse {
        post_eas(&self.client, &self.base, &self.device_id, command, xml).await
    }

    async fn assert_backend_used(&self) {
        let requests = self
            .backend
            .received_requests()
            .await
            .expect("wiremock request journal");
        assert!(
            requests
                .iter()
                .any(|request| request.url.path() == "/session"),
            "the real HTTP requests must authenticate through the backend"
        );
    }
}

struct EasWireResponse {
    status: StatusCode,
    content_type: String,
    nosniff: bool,
    xml: String,
}

fn session_json(base: &str) -> Value {
    json!({
        "apiUrl": base,
        "downloadUrl": format!("{base}/download/{{accountId}}/{{blobId}}"),
        "uploadUrl": format!("{base}/upload/{{accountId}}"),
        "eventSourceUrl": format!("{base}/eventsource"),
        "state": "wire-tests",
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

/// Post RAW XML (not WBXML) — the form a client uses when it does not encode,
/// which is where the entry-point namespace validation runs on the document as
/// written.
async fn post_eas_xml(
    client: &reqwest::Client,
    base: &str,
    device_id: &str,
    command: &str,
    xml: &str,
) -> EasWireResponse {
    let response = client
        .post(format!(
            "{base}/Microsoft-Server-ActiveSync?Cmd={command}&User={OWNER}&DeviceId={device_id}&DeviceType=Outlook"
        ))
        .basic_auth(OWNER, Some(PASSWORD))
        .header(header::CONTENT_TYPE, "text/xml")
        .header("MS-ASProtocolVersion", "16.1")
        .body(xml.to_string())
        .send()
        .await
        .expect("gateway HTTP request must complete");
    let status = response.status();
    let content_type = response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_string();
    let nosniff = response
        .headers()
        .get("x-content-type-options")
        .and_then(|value| value.to_str().ok())
        == Some("nosniff");
    let body = response.text().await.unwrap_or_default();
    EasWireResponse {
        status,
        content_type,
        nosniff,
        xml: body,
    }
}

async fn post_eas(
    client: &reqwest::Client,
    base: &str,
    device_id: &str,
    command: &str,
    xml: &str,
) -> EasWireResponse {
    let codec = Wbxml::new();
    let body = codec.encode(xml).expect("request fixture must encode");
    let response = client
        .post(format!(
            "{base}/Microsoft-Server-ActiveSync?Cmd={command}&User={OWNER}&DeviceId={device_id}&DeviceType=Outlook"
        ))
        .basic_auth(OWNER, Some(PASSWORD))
        .header(header::CONTENT_TYPE, "application/vnd.ms-sync.wbxml")
        .header("MS-ASProtocolVersion", "16.1")
        .body(body)
        .send()
        .await
        .expect("gateway HTTP request must complete");
    let status = response.status();
    let content_type = response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_string();
    let nosniff = response
        .headers()
        .get("x-content-type-options")
        .and_then(|value| value.to_str().ok())
        == Some("nosniff");
    let bytes = response.bytes().await.unwrap();
    let xml = if bytes.is_empty() {
        String::new()
    } else {
        codec
            .decode(&bytes)
            .expect("WBXML response must decode at the HTTP boundary")
    };
    EasWireResponse {
        status,
        content_type,
        nosniff,
        xml,
    }
}

fn tag_text(xml: &str, name: &str) -> Option<String> {
    Document::parse(xml).ok()?.descendants().find_map(|node| {
        (node.is_element() && node.tag_name().name() == name)
            .then(|| node.text().unwrap_or_default().to_string())
    })
}

fn tasks_sync(sync_key: &str) -> String {
    format!(
        "<Sync xmlns=\"AirSync:\"><Collections><Collection><Class>Tasks</Class><SyncKey>{sync_key}</SyncKey><CollectionId>7</CollectionId><GetChanges>1</GetChanges><WindowSize>25</WindowSize></Collection></Collections></Sync>"
    )
}

fn ping_request() -> &'static str {
    "<Ping xmlns=\"Ping:\"><HeartbeatInterval>60</HeartbeatInterval><Folders><Folder><Id>7</Id><Class>Tasks</Class></Folder></Folders></Ping>"
}

fn mixed_ping_request() -> &'static str {
    "<Ping xmlns=\"Ping:\"><HeartbeatInterval>60</HeartbeatInterval><Folders><Folder><Id>7</Id><Class>Tasks</Class></Folder><Folder><Id>2</Id><Class>Email</Class></Folder></Folders></Ping>"
}

#[tokio::test]
async fn eas_wbxml_sync_runs_prime_delta_invalid_key_and_clean_reprime_over_http() {
    let gateway = WireGateway::spawn().await;

    let prime = gateway.eas("Sync", &tasks_sync("0")).await;
    assert_eq!(prime.status, StatusCode::OK);
    assert_eq!(prime.content_type, "application/vnd.ms-sync.wbxml");
    assert!(prime.nosniff, "router middleware must add nosniff");
    assert_eq!(tag_text(&prime.xml, "Status").as_deref(), Some("1"));
    assert_eq!(tag_text(&prime.xml, "CollectionId").as_deref(), Some("7"));
    let prime_key = tag_text(&prime.xml, "SyncKey").expect("prime key");
    assert_ne!(prime_key, "0");

    let delta = gateway.eas("Sync", &tasks_sync(&prime_key)).await;
    assert_eq!(delta.status, StatusCode::OK);
    assert_eq!(tag_text(&delta.xml, "Status").as_deref(), Some("1"));
    let delta_key = tag_text(&delta.xml, "SyncKey").expect("delta key");
    assert_ne!(delta_key, prime_key, "successful deltas rotate the key");

    let rejected = gateway.eas("Sync", &tasks_sync("never-issued")).await;
    assert_eq!(rejected.status, StatusCode::OK);
    assert_eq!(tag_text(&rejected.xml, "Status").as_deref(), Some("3"));
    assert_eq!(tag_text(&rejected.xml, "SyncKey").as_deref(), Some("0"));

    let reprime = gateway.eas("Sync", &tasks_sync("0")).await;
    assert_eq!(reprime.status, StatusCode::OK);
    assert_eq!(tag_text(&reprime.xml, "Status").as_deref(), Some("1"));
    // A re-prime must hand back a USABLE key: an absent or empty SyncKey leaves
    // the client unable to resume, which the old `assert_ne!(.., Some("0"))`
    // accepted because `None != Some("0")`.
    let reprime_key = tag_text(&reprime.xml, "SyncKey");
    assert!(
        matches!(reprime_key.as_deref(), Some(key) if !key.is_empty() && key != "0"),
        "re-prime must return a usable SyncKey, got {reprime_key:?}"
    );
    gateway.assert_backend_used().await;
}

#[tokio::test]
async fn eas_ping_times_out_cleanly_and_wakes_for_a_real_journaled_change() {
    let gateway = WireGateway::spawn().await;
    let prime = gateway.eas("Sync", &tasks_sync("0")).await;
    assert_eq!(tag_text(&prime.xml, "Status").as_deref(), Some("1"));

    let quiet = gateway.eas("Ping", ping_request()).await;
    assert_eq!(quiet.status, StatusCode::OK);
    assert_eq!(quiet.content_type, "application/vnd.ms-sync.wbxml");
    assert_eq!(tag_text(&quiet.xml, "Status").as_deref(), Some("1"));

    let client = gateway.client.clone();
    let base = gateway.base.clone();
    let device_id = gateway.device_id.clone();
    let waiting = tokio::spawn(async move {
        post_eas(&client, &base, &device_id, "Ping", mixed_ping_request()).await
    });
    // Wait for the OBSERVABLE subscription signal: the Ping registers a raw
    // receiver on the notification feed before it starts waiting, so a nonzero
    // count proves the wake cannot be missed. (A fixed sleep only hoped the
    // Ping had reached its wait loop.)
    let subscribed = tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if gateway.state.subscription_manager.receiver_count() > 0 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await;
    assert!(
        subscribed.is_ok(),
        "the waiting Ping must register a notification subscription"
    );
    gateway
        .state
        .storage
        .upsert_task(
            OWNER,
            "task-wire-wake",
            &TaskFields {
                subject: Some("Wake the waiting Ping"),
                ..TaskFields::default()
            },
        )
        .await
        .unwrap();
    gateway
        .state
        .subscription_manager
        .publish(NotificationEvent::ItemCreated {
            owner: OWNER.to_string(),
            folder_id: "7".to_string(),
            item_id: "task-wire-wake".to_string(),
            change_key: "1".to_string(),
        });
    // The deadline is strictly BELOW the 5s heartbeat (with ample slack after
    // the 100 ms mutation), so a Status 2 can only have come from the
    // subscription wake, never from the heartbeat expiring.
    let woken = tokio::time::timeout(Duration::from_secs(3), waiting)
        .await
        .expect("Ping must wake before its heartbeat")
        .unwrap();
    assert_eq!(woken.status, StatusCode::OK);
    assert_eq!(tag_text(&woken.xml, "Status").as_deref(), Some("2"));
    assert_eq!(tag_text(&woken.xml, "Folder").as_deref(), Some("7"));
}

#[tokio::test]
async fn ews_syncfolderitems_returns_a_real_soap_operation_error_over_http() {
    let gateway = WireGateway::spawn().await;
    let request = r#"<?xml version="1.0" encoding="utf-8"?>
<s:Envelope xmlns:s="http://schemas.xmlsoap.org/soap/envelope/" xmlns:m="http://schemas.microsoft.com/exchange/services/2006/messages" xmlns:t="http://schemas.microsoft.com/exchange/services/2006/types">
  <s:Body><m:SyncFolderItems><m:ItemShape><t:BaseShape>IdOnly</t:BaseShape></m:ItemShape><m:SyncFolderId><t:DistinguishedFolderId Id="calendar"/></m:SyncFolderId><m:SyncState>not-a-gateway-cursor</m:SyncState><m:MaxChangesReturned>10</m:MaxChangesReturned></m:SyncFolderItems></s:Body>
</s:Envelope>"#;
    let response = gateway
        .client
        .post(format!("{}/EWS/Exchange.asmx", gateway.base))
        .basic_auth(OWNER, Some(PASSWORD))
        .header(header::CONTENT_TYPE, "text/xml; charset=utf-8")
        .body(request)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok()),
        Some("text/xml; charset=utf-8")
    );
    let body = response.text().await.unwrap();
    let document = Document::parse(&body).expect("valid SOAP XML");
    assert!(document.descendants().any(|node| {
        node.is_element() && node.tag_name().name() == "SyncFolderItemsResponseMessage"
    }));
    assert!(body.contains("ResponseClass=\"Error\""));
    assert!(body.contains("<m:ResponseCode>ErrorInvalidSyncStateData</m:ResponseCode>"));
    gateway.assert_backend_used().await;
}

#[tokio::test]
async fn autodiscover_xml_completes_the_full_http_request_response_cycle() {
    let gateway = WireGateway::spawn().await;
    let request = r#"<?xml version="1.0" encoding="utf-8"?>
<Autodiscover xmlns="http://schemas.microsoft.com/exchange/autodiscover/outlook/requestschema/2006"><Request><EMailAddress>u@example.com</EMailAddress><AcceptableResponseSchema>http://schemas.microsoft.com/exchange/autodiscover/outlook/responseschema/2006a</AcceptableResponseSchema></Request></Autodiscover>"#;
    let response = gateway
        .client
        .post(format!("{}/autodiscover/autodiscover.xml", gateway.base))
        .header(header::CONTENT_TYPE, "application/xml")
        .body(request)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert!(response.headers().contains_key(header::CONTENT_TYPE));
    let body = response.text().await.unwrap();
    Document::parse(&body).expect("Autodiscover must return well-formed XML");
    assert!(body.contains("u@example.com"));
    assert!(body.contains("gateway.example.com"));
    assert!(body.contains("/EWS/Exchange.asmx"));
}

#[tokio::test]
async fn eas_composemail_namespaced_sendmail_reaches_submission_over_http() {
    // [MS-ASCMD] §2.2.1.14: the SendMail request element lives on the shared
    // ComposeMail code page, so a conformant client sends
    // `<ComposeMail:SendMail xmlns:ComposeMail="ComposeMail:">`. The entry-point
    // namespace check must accept it (previously it answered HTTP 400 "Invalid
    // namespace" before the parser ran).
    let gw = WireGateway::spawn().await;
    let mime = "From: u@example.com\r\nTo: v@example.com\r\nSubject: hi\r\n\r\nbody\r\n";
    let xml = format!(
        "<ComposeMail:SendMail xmlns:ComposeMail=\"ComposeMail:\"><ComposeMail:ClientId>wire-cid</ComposeMail:ClientId><ComposeMail:SaveInSentItems>1</ComposeMail:SaveInSentItems><ComposeMail:MIMEData>{}</ComposeMail:MIMEData></ComposeMail:SendMail>",
        mime.replace('<', "&lt;").replace('>', "&gt;")
    );
    let response = post_eas_xml(&gw.client, &gw.base, &gw.device_id, "SendMail", &xml).await;
    assert_eq!(
        response.status,
        StatusCode::OK,
        "a ComposeMail-namespaced SendMail must reach mail submission, not be rejected: {}",
        response.xml
    );
    assert!(
        !response.xml.contains("Invalid namespace"),
        "the entry point must accept the ComposeMail namespace: {}",
        response.xml
    );
    assert!(
        response.xml.contains("<Status") || response.xml.contains(":Status"),
        "the response must be a ComposeMail Status document: {}",
        response.xml
    );
}
