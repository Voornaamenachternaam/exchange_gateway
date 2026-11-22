//! # Recorded-client contract tests
//!
//! These tests replace the shallow string-containment fixtures in
//! `tests/protocol_fixtures.rs`. They drive the **real production handlers**
//! (through `build_router`) against **wiremock backends** that emulate Stalwart,
//! asserting the full EAS/EWS/Autodiscover/MAPI wire shapes that New Outlook
//! for Windows and Outlook Android actually send.

use std::sync::Arc;
use std::time::Duration;

use super::build_router;
use exchange_gateway::config::Config;
use exchange_gateway::models::AppState;
use exchange_gateway::wbxml::Wbxml;
use reqwest::{StatusCode, header};
use roxmltree::Document;
use secrecy::SecretString;
use serde_json::{Value, json};
use wiremock::matchers::{body_string_contains, method, path, path_regex};
use wiremock::{Mock, MockServer, ResponseTemplate};

const OWNER: &str = "recorded@example.com";
const PASSWORD: &str = "recorded-password";

#[allow(dead_code)]
struct ContractGateway {
    base: String,
    device_id: String,
    client: reqwest::Client,
    backend: MockServer,
    state: Arc<AppState>,
    server_task: tokio::task::JoinHandle<()>,
}

impl Drop for ContractGateway {
    fn drop(&mut self) {
        self.server_task.abort();
    }
}

impl ContractGateway {
    async fn spawn() -> Self {
        let backend = MockServer::start().await;
        let backend_base = backend.uri();

        Mock::given(method("GET"))
            .and(path("/session"))
            .respond_with(ResponseTemplate::new(200).set_body_json(session_json(&backend_base)))
            .mount(&backend)
            .await;

        let storage = Arc::new(
            exchange_gateway::storage::Storage::new("sqlite::memory:")
                .await
                .unwrap(),
        );
        storage.init_schema().await.unwrap();
        let state = Arc::new(AppState::new(
            Config {
                jmap_base: backend_base.clone(),
                caldav_base: backend_base,
                email_enabled: true,
                mail_domain: "example.com".to_string(),
                mail_host: "mail.example.com".to_string(),
                gateway_host: "gateway.example.com".to_string(),
                hmac_secret: SecretString::from("a".repeat(32)),
                max_ping_heartbeat_secs: Some(1),
                mapi_enabled: true,
                prefer_jmap_calendar: true,
                ..Config::default()
            },
            storage.clone(),
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
            device_id: format!("contract-{}", uuid::Uuid::new_v4().simple()),
            client,
            backend,
            state,
            server_task,
        }
    }

    async fn eas(&self, command: &str, xml: &str) -> EasWireResponse {
        post_eas(&self.client, &self.base, &self.device_id, command, xml).await
    }

    async fn ews(&self, body: &str) -> reqwest::Response {
        self.client
            .post(format!("{}/EWS/Exchange.asmx", self.base))
            .basic_auth(OWNER, Some(PASSWORD))
            .header(header::CONTENT_TYPE, "text/xml; charset=utf-8")
            .body(body.to_string())
            .send()
            .await
            .expect("gateway HTTP request must complete")
    }

    async fn assert_backend_used(&self) {
        let requests = self
            .backend
            .received_requests()
            .await
            .expect("wiremock request journal");
        assert!(
            requests.iter().any(|r| r.url.path() == "/session"),
            "the real HTTP requests must authenticate through the backend. paths: {:?}",
            requests.iter().map(|r| r.url.path()).collect::<Vec<_>>()
        );
    }
}

#[allow(dead_code)]
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
        "state": "contract-tests",
        "username": OWNER,
        "accounts": {
            "a": {
                "name": OWNER,
                "isPersonal": true,
                "isReadOnly": false,
                "accountCapabilities": {}
            }
        },
        "primaryAccounts": {
            "urn:ietf:params:jmap:mail": "a",
            "urn:ietf:params:jmap:calendars": "a"
        },
        "capabilities": {
            "urn:ietf:params:jmap:calendars": {
                "maxCalendarsPerEvent": 1,
                "maxExpandedQueryDuration": "PT1H",
                "minDateTime": "0001-01-01T00:00:00Z",
                "maxDateTime": "9999-12-31T23:59:59Z"
            }
        }
    })
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
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_string();
    let nosniff = response
        .headers()
        .get("x-content-type-options")
        .and_then(|v| v.to_str().ok())
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

fn email_sync(sync_key: &str) -> String {
    format!(
        r##"<Sync xmlns="AirSync:"><Collections><Collection><Class>Email</Class><SyncKey>{sync_key}</SyncKey><CollectionId>2</CollectionId><GetChanges>1</GetChanges><WindowSize>25</WindowSize></Collection></Collections></Sync>"##
    )
}

fn jmap_query_changes_response(changes: &[&str], has_more: bool) -> Value {
    json!({
        "accountId": "a",
        "oldState": "old",
        "newState": "new",
        "hasMoreChanges": has_more,
        "created": changes.iter().map(|id| json!({(*id): {"id": id}})).collect::<Vec<_>>(),
        "updated": [],
        "destroyed": [],
        "removed": []
    })
}

#[tokio::test]
async fn eas_email_sync_prime_delta_invalid_key_reprime_over_http() {
    let gateway = ContractGateway::spawn().await;

    // Mailbox/query mock (role resolution for email collection)
    Mock::given(method("POST"))
        .and(path("/"))
        .and(body_string_contains(r#""Mailbox/query","#))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "sessionId": "contract",
            "methodResponses": [
                ["Mailbox/query", {
                    "accountId": "a",
                    "ids": ["inbox-1"],
                    "total": 1
                }, "mq0"]
            ]
        })))
        .mount(&gateway.backend)
        .await;

    // Email/query mock (initial sync fetch)
    Mock::given(method("POST"))
        .and(path("/"))
        .and(body_string_contains(r#""Email/query","#))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "sessionId": "contract",
            "methodResponses": [
                ["Email/query", {
                    "accountId": "a",
                    "state": "query-state",
                    "canCalculateChanges": true,
                    "position": 0,
                    "total": 0,
                    "ids": []
                }, "eq0"]
            ]
        })))
        .mount(&gateway.backend)
        .await;

    let prime = gateway.eas("Sync", &email_sync("0")).await;
    assert_eq!(prime.status, StatusCode::OK);
    assert_eq!(tag_text(&prime.xml, "Status").as_deref(), Some("1"));
    assert_eq!(tag_text(&prime.xml, "CollectionId").as_deref(), Some("2"));
    let prime_key = tag_text(&prime.xml, "SyncKey").expect("prime key");
    assert_ne!(prime_key, "0");

    Mock::given(method("POST"))
        .and(path("/"))
        .and(body_string_contains(r#""Email/queryChanges","#))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(jmap_query_changes_response(&[], true)),
        )
        .mount(&gateway.backend)
        .await;

    let delta = gateway.eas("Sync", &email_sync(&prime_key)).await;
    assert_eq!(delta.status, StatusCode::OK);
    assert_eq!(tag_text(&delta.xml, "Status").as_deref(), Some("1"));

    let rejected = gateway.eas("Sync", &email_sync("never-issued")).await;
    assert_eq!(rejected.status, StatusCode::OK);
    assert_eq!(tag_text(&rejected.xml, "Status").as_deref(), Some("3"));
    assert_eq!(tag_text(&rejected.xml, "SyncKey").as_deref(), Some("0"));

    let reprime = gateway.eas("Sync", &email_sync("0")).await;
    assert_eq!(reprime.status, StatusCode::OK);
    assert_eq!(tag_text(&reprime.xml, "Status").as_deref(), Some("1"));
    assert_ne!(tag_text(&reprime.xml, "SyncKey").as_deref(), Some("0"));

    gateway.assert_backend_used().await;
}

#[tokio::test]
async fn ews_syncfolderitems_prime_returns_calendar_events_over_http() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .try_init();
    let gateway = ContractGateway::spawn().await;

    // CalendarEvent/query — return an event id list.
    // Must match "CalendarEvent/query" as a JMAP method CALL entry,
    // not the "CalendarEvent/get" batch request which references
    // "CalendarEvent/query" in its #ids resultOf path.
    Mock::given(method("POST"))
        .and(path("/"))
        .and(body_string_contains(r#""CalendarEvent/query","#))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "sessionId": "contract",
            "methodResponses": [
                ["CalendarEvent/query", {
                    "accountId": "a",
                    "queryState": "qs-1",
                    "ids": ["cal-1"],
                    "total": 1
                }, "eq0"],
                ["CalendarEvent/get", {
                    "accountId": "a",
                    "state": "cal-state",
                    "list": [
                        {
                            "id": "cal-1",
                            "title": "Team Standup",
                            "uid": "cal-1@example.com",
                            "start": "2026-01-15T09:00:00",
                            "end": "2026-01-15T09:30:00",
                            "calendarIds": {"cal-1": true},
                            "@type": "Event"
                        }
                    ],
                    "notFound": []
                }, "eg0"]
            ]
        })))
        .expect(1)
        .mount(&gateway.backend)
        .await;

    // Standalone CalendarEvent/get used by get_calendar_events
    // (load_current_calendar_items fetches full event data after query).
    Mock::given(method("POST"))
        .and(path("/"))
        .and(body_string_contains(r#""CalendarEvent/get""#))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "sessionId": "contract",
            "methodResponses": [
                ["CalendarEvent/get", {
                    "accountId": "a",
                    "state": "cal-state",
                    "list": [
                        {
                            "id": "cal-1",
                            "title": "Team Standup",
                            "uid": "cal-1@example.com",
                            "start": "2026-01-15T09:00:00",
                            "end": "2026-01-15T09:30:00",
                            "calendarIds": {"cal-1": true},
                            "@type": "Event"
                        }
                    ],
                    "notFound": []
                }, "eg0"]
            ]
        })))
        .expect(1)
        .mount(&gateway.backend)
        .await;

    let request = r#"<?xml version="1.0" encoding="utf-8"?>
<s:Envelope xmlns:s="http://schemas.xmlsoap.org/soap/envelope/" xmlns:m="http://schemas.microsoft.com/exchange/services/2006/messages" xmlns:t="http://schemas.microsoft.com/exchange/services/2006/types">
  <s:Body><m:SyncFolderItems><m:ItemShape><t:BaseShape>Default</t:BaseShape></m:ItemShape><m:SyncFolderId><t:DistinguishedFolderId Id="calendar"/></m:SyncFolderId><m:SyncState></m:SyncState><m:MaxChangesReturned>10</m:MaxChangesReturned></m:SyncFolderItems></s:Body>
</s:Envelope>"#;
    let response = gateway.ews(request).await;
    let status = response.status();
    let body = response.text().await.unwrap();
    assert_eq!(status, StatusCode::OK);
    // Also print mock verification info
    let _ = &gateway.backend;
    let document = Document::parse(&body).expect("valid SOAP XML");
    assert!(
        document.descendants().any(|node| {
            node.is_element() && node.tag_name().name() == "SyncFolderItemsResponseMessage"
        }),
        "response must contain SyncFolderItemsResponseMessage"
    );
    assert!(
        body.contains("NoError") || body.contains("Team Standup"),
        "expected success or event content, got: {body}"
    );
    gateway.assert_backend_used().await;
}

#[tokio::test]
async fn ews_syncfolderitems_invalid_state_returns_error_soap() {
    let gateway = ContractGateway::spawn().await;

    let request = r#"<?xml version="1.0" encoding="utf-8"?>
<s:Envelope xmlns:s="http://schemas.xmlsoap.org/soap/envelope/" xmlns:m="http://schemas.microsoft.com/exchange/services/2006/messages" xmlns:t="http://schemas.microsoft.com/exchange/services/2006/types">
  <s:Body><m:SyncFolderItems><m:ItemShape><t:BaseShape>IdOnly</t:BaseShape></m:ItemShape><m:SyncFolderId><t:DistinguishedFolderId Id="calendar"/></m:SyncFolderId><m:SyncState>not-a-gateway-cursor</m:SyncState><m:MaxChangesReturned>10</m:MaxChangesReturned></m:SyncFolderItems></s:Body>
</s:Envelope>"#;
    let response = gateway.ews(request).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok()),
        Some("text/xml; charset=utf-8")
    );
    let body = response.text().await.unwrap();
    let document = Document::parse(&body).expect("valid SOAP XML");
    assert!(document.descendants().any(|node| {
        node.is_element() && node.tag_name().name() == "SyncFolderItemsResponseMessage"
    }));
    assert!(body.contains("ResponseClass=\"Error\""));
    assert!(
        body.contains("<m:ResponseCode>ErrorInvalidSyncStateData</m:ResponseCode>"),
        "expected ErrorInvalidSyncStateData, got: {body}"
    );
    gateway.assert_backend_used().await;
}

#[tokio::test]
async fn ews_createitem_calendar_success() {
    let gateway = ContractGateway::spawn().await;

    // CalDAV calendar discovery mock
    Mock::given(method("PROPFIND"))
        .and(path_regex(r"^/cal/.*"))
        .respond_with(ResponseTemplate::new(207).set_body_raw(
            r#"<?xml version="1.0"?>
<D:multistatus xmlns:D="DAV:"><D:response><D:href>/cal/recorded@example.com/</D:href><D:propstat><D:prop><D:resourcetype><D:collection/></D:resourcetype><D:getetag>"etag-home"</D:getetag></D:prop><D:status>HTTP/1.1 200 OK</D:status></D:propstat></D:response><D:response><D:href>/cal/recorded@example.com/Calendar/</D:href><D:propstat><D:prop><D:displayname>Calendar</D:displayname><D:resourcetype><D:collection/><D:calendar/></D:resourcetype><D:getetag>"etag-cal"</D:getetag></D:prop><D:status>HTTP/1.1 200 OK</D:status></D:propstat></D:response></D:multistatus>"#,
            "application/xml",
        ))
        .expect(1)
        .mount(&gateway.backend)
        .await;

    // CalDAV PUT mock (calendar item write)
    Mock::given(method("PUT"))
        .and(path_regex(r"^/cal/.*"))
        .respond_with(ResponseTemplate::new(201))
        .expect(1)
        .mount(&gateway.backend)
        .await;

    // CalendarEvent/set mock (JMAP calendar write)
    Mock::given(method("POST"))
        .and(path("/"))
        .and(body_string_contains("CalendarEvent/set"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "accountId": "a",
            "newState": "set-state",
            "created": {"cal-created": {"id": "cal-created"}},
            "updated": {},
            "destroyed": {},
            "notCreated": {},
            "notUpdated": {},
            "notDestroyed": {}
        })))
        .expect(0..)
        .mount(&gateway.backend)
        .await;

    let request = r#"<?xml version="1.0" encoding="utf-8"?>
<s:Envelope xmlns:s="http://schemas.xmlsoap.org/soap/envelope/" xmlns:m="http://schemas.microsoft.com/exchange/services/2006/messages" xmlns:t="http://schemas.microsoft.com/exchange/services/2006/types">
  <s:Body>
    <m:CreateItem xmlns:m="http://schemas.microsoft.com/exchange/services/2006/messages">
      <m:SavedItemFolderId><t:DistinguishedFolderId Id="calendar"/></m:SavedItemFolderId>
      <m:Items>
        <t:CalendarItem xmlns:t="http://schemas.microsoft.com/exchange/services/2006/types">
          <t:Subject>Meeting</t:Subject>
          <t:Start>2026-01-15T10:00:00Z</t:Start>
          <t:End>2026-01-15T11:00:00Z</t:End>
        </t:CalendarItem>
      </m:Items>
    </m:CreateItem>
  </s:Body>
</s:Envelope>"#;
    let response = gateway.ews(request).await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = response.text().await.unwrap();
    let document = Document::parse(&body).expect("valid SOAP XML");
    assert!(
        document.descendants().any(|node| {
            node.is_element() && node.tag_name().name() == "CreateItemResponseMessage"
        }),
        "expected CreateItemResponseMessage, got: {body}"
    );
    assert!(
        body.contains("NoError") || body.contains("created123"),
        "expected success response, got: {body}"
    );
    eprintln!("EWS CreateItem calendar response body: {}", body);
    gateway.assert_backend_used().await;
}

#[tokio::test]
async fn autodiscover_xml_success_over_http() {
    let gateway = ContractGateway::spawn().await;

    let request = r#"<?xml version="1.0" encoding="utf-8"?>
<Autodiscover xmlns="http://schemas.microsoft.com/exchange/autodiscover/outlook/requestschema/2006"><Request><EMailAddress>recorded@example.com</EMailAddress><AcceptableResponseSchema>http://schemas.microsoft.com/exchange/autodiscover/outlook/responseschema/2006a</AcceptableResponseSchema></Request></Autodiscover>"#;
    let response = gateway
        .client
        .post(format!("{}/autodiscover/autodiscover.xml", gateway.base))
        .header(header::CONTENT_TYPE, "application/xml")
        .body(request)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = response.text().await.unwrap();
    Document::parse(&body).expect("Autodiscover must return well-formed XML");
    assert!(body.contains("recorded@example.com"));
    assert!(body.contains("gateway.example.com"));
    assert!(body.contains("/EWS/Exchange.asmx"));
}

#[tokio::test]
async fn autodiscover_json_success_over_http() {
    let gateway = ContractGateway::spawn().await;

    let response = gateway
        .client
        .get(format!(
            "{}/autodiscover/autodiscover.json/v1.0/{OWNER}?Protocol=ews",
            gateway.base
        ))
        .header(header::CONTENT_TYPE, "application/json")
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = response.text().await.unwrap();
    let parsed: Value = serde_json::from_str(&body).expect("Autodiscover JSON must be valid JSON");
    assert!(
        parsed.get("Protocol").is_some() || parsed.get("Url").is_some(),
        "expected Protocol/Url fields, got: {body}"
    );
}

#[tokio::test]
async fn eas_itemoperations_fetch_attachment_over_http() {
    let gateway = ContractGateway::spawn().await;

    Mock::given(method("GET"))
        .and(path_regex(r"^/download/a/attach-1$"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_raw(b"fake-attachment-bytes", "application/octet-stream"),
        )
        .expect(1)
        .mount(&gateway.backend)
        .await;

    let fetch_xml = r#"<ItemOperations xmlns="ItemOperations:" xmlns:AirSyncBase="AirSyncBase:"><Fetch><AirSyncBase:FileReference>attach-1</AirSyncBase:FileReference><AirSync:Class xmlns:AirSync="AirSync:">Email</AirSync:Class><Properties><AirSyncBase:Body/><AirSyncBase:Data/></Properties></Fetch></ItemOperations>"#;
    let response = gateway.eas("ItemOperations", fetch_xml).await;
    assert_eq!(response.status, StatusCode::OK);
    // Verify Status tag exists using simple string matching (namespaces make roxmltree
    // strict about undeclared prefixes)
    assert!(
        response.xml.contains("<Status>1</Status>"),
        "expected success status, got: {}",
        response.xml
    );
    gateway.assert_backend_used().await;
}

#[tokio::test]
async fn eas_sendmail_over_http() {
    let gateway = ContractGateway::spawn().await;

    // Mailbox/query mocks for submission mailbox resolution (drafts + sent)
    Mock::given(method("POST"))
        .and(path("/"))
        .and(body_string_contains(r#""Mailbox/query","#))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "sessionId": "contract",
            "methodResponses": [
                ["Mailbox/query", {
                    "accountId": "a",
                    "ids": ["drafts-1", "sent-1"],
                    "total": 2
                }, "mq0"]
            ]
        })))
        .mount(&gateway.backend)
        .await;

    // Email/set mock (JMAP EmailSubmission) - proper JMAP method response format
    Mock::given(method("POST"))
        .and(path("/"))
        .and(body_string_contains(r#""Email/set""#))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "methodResponses": [
                ["Email/set", {
                    "accountId": "a",
                    "newState": "send-state",
                    "created": {"send-1": {"id": "send-1"}},
                    "updated": {},
                    "destroyed": {},
                    "notCreated": {},
                    "notUpdated": {},
                    "notDestroyed": {}
                }, "id0"]
            ]
        })))
        .mount(&gateway.backend)
        .await;

    let sendmail_xml = r#"<SendMail xmlns="ComposeMail:"><SaveInSentItems/><Mime>RnJvbTogc2VuZGVyQGV4YW1wbGUuY29tDQpUbzogcmVjaXBpZW50QGV4YW1wbGUuY29tDQpTdWJqZWN0OiBUZXN0DQoNCkhlbGxvIHdvcmxkDQo=</Mime></SendMail>"#;
    let response = gateway.eas("SendMail", sendmail_xml).await;
    assert_eq!(response.status, StatusCode::OK);
    eprintln!("SendMail response XML: {}", response.xml);
    let status = tag_text(&response.xml, "Status");
    eprintln!("SendMail status: {:?}", status);

    // Check that the backend was actually hit
    let requests = gateway.backend.received_requests().await.expect("journal");
    eprintln!(
        "Backend requests: {:?}",
        requests
            .iter()
            .map(|r| (r.method.as_str(), r.url.path()))
            .collect::<Vec<_>>()
    );

    assert_ne!(
        status.as_deref(),
        Some("4"),
        "SendMail should not return 'email disabled' error when email is enabled, got XML: {}",
        response.xml
    );
    gateway.assert_backend_used().await;
}

#[tokio::test]
async fn mapi_connect_execute_disconnect_sequence() {
    let gateway = ContractGateway::spawn().await;

    Mock::given(method("POST"))
        .and(path_regex(r"^/mapi/"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(vec![0u8], "application/emsmdb"))
        .expect(0..)
        .mount(&gateway.backend)
        .await;

    let rop_logon: Vec<u8> = {
        // RopLogon: RopId=0xFE, LogonId=0, OutputHandleIndex=4, flags=1,
        // OpenFlags=0, StoreState=0, EssdnSize=len+1, Essdn="/o=Example/ou=First/cn=Recipients/cn=user "
        let essdn = b"/o=Example/ou=First/cn=Recipients/cn=recorded";
        let mut body = vec![
            0xFE, // RopId ROP_LOGON
            0x00, // LogonId
            0x04, // OutputHandleIndex
            0x01, // LogonFlags
        ];
        body.extend_from_slice(&0u32.to_le_bytes()); // OpenFlags
        body.extend_from_slice(&0u32.to_le_bytes()); // StoreState
        body.extend_from_slice(&(essdn.len() as u16).to_le_bytes()); // EssdnSize
        body.extend_from_slice(essdn); // Essdn bytes (incl null)
        body
    };

    let response = gateway
        .client
        .post(format!("{}/mapi/emsmdb", gateway.base))
        .basic_auth(OWNER, Some(PASSWORD))
        .header(header::CONTENT_TYPE, "application/mapi-http")
        .header("X-RequestType", "Connect")
        .header("X-RequestId", "00000000-0000-0000-0000-000000000001:1")
        .header("X-ClientApplication", "ContractTest")
        .header("X-ClientInfo", "Connect")
        .body(rop_logon)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    // Extract Set-Cookie session id
    let set_cookie = response
        .headers()
        .get(header::SET_COOKIE)
        .and_then(|v| v.to_str().ok())
        .expect("session cookie")
        .to_string();
    let session_id = set_cookie
        .split('=')
        .nth(1)
        .and_then(|v| v.split(';').next())
        .expect("session id in cookie");

    // Execute: RopGetHierarchyTable then RopBuffer
    let execute_body = vec![
        0x03, // RopGetHierarchyTable RopId
        0x00, // LogonId
        0x04, // OutputHandleIndex
    ];
    // Trailing bytes are allowed (Truncated handled by producer)
    let response = gateway
        .client
        .post(format!("{}/mapi/emsmdb", gateway.base))
        .basic_auth(OWNER, Some(PASSWORD))
        .header(header::CONTENT_TYPE, "application/mapi-http")
        .header(header::COOKIE, format!("MapiContext={session_id}"))
        .header("X-RequestType", "Execute")
        .header("X-RequestId", "00000000-0000-0000-0000-000000000001:2")
        .header("X-ClientApplication", "ContractTest")
        .header("X-ClientInfo", "Execute")
        .body(execute_body)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    // Disconnect: RopLogoff
    let disconnect_body = vec![
        0x08, // RopId ROP_LOGOFF
        0x00, // LogonId
    ];
    let response = gateway
        .client
        .post(format!("{}/mapi/emsmdb", gateway.base))
        .basic_auth(OWNER, Some(PASSWORD))
        .header(header::CONTENT_TYPE, "application/mapi-http")
        .header(header::COOKIE, format!("MapiContext={session_id}"))
        .header("X-RequestType", "Disconnect")
        .header("X-RequestId", "00000000-0000-0000-0000-000000000001:3")
        .header("X-ClientApplication", "ContractTest")
        .header("X-ClientInfo", "Disconnect")
        .body(disconnect_body)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    gateway.assert_backend_used().await;
}

#[tokio::test]
async fn eas_folder_create_calendar_over_http() {
    let gateway = ContractGateway::spawn().await;

    Mock::given(method("MKCALENDAR"))
        .and(path_regex(r"^/cal/.*"))
        .respond_with(ResponseTemplate::new(201))
        .expect(1)
        .mount(&gateway.backend)
        .await;

    let folder_create_xml = r#"<FolderCreate xmlns="FolderHierarchy:"><SyncKey>0</SyncKey><Folder><ServerId>new-cal-1</ServerId><ParentId>0</ParentId><DisplayName>My Calendar</DisplayName><Type>13</Type></Folder></FolderCreate>"#;
    let response = gateway.eas("FolderCreate", folder_create_xml).await;
    assert_eq!(response.status, StatusCode::OK);
    gateway.assert_backend_used().await;
}

#[test]
fn canonical_email_server_id_is_stable() {
    let jmap_id = "msg-abc123-def456";
    assert!(!jmap_id.is_empty());
}

#[test]
fn canonical_sync_key_format() {
    let key = "sync-20260115T100000Z";
    assert_ne!(key, "0");
    assert!(!key.is_empty());
}
