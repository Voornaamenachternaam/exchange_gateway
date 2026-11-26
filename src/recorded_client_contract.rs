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

    async fn eas_multipart(&self, command: &str, xml: &str) -> EasRawResponse {
        post_eas_multipart(&self.client, &self.base, &self.device_id, command, xml).await
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

/// Response of an EAS request whose body is not WBXML (multipart delivery).
struct EasRawResponse {
    status: StatusCode,
    content_type: String,
    body: Vec<u8>,
}

/// POST an EAS command asking for multipart delivery
/// ([MS-ASHTTP] §2.2.1.1.2.5 `MS-ASAcceptMultiPart: T`).
async fn post_eas_multipart(
    client: &reqwest::Client,
    base: &str,
    device_id: &str,
    command: &str,
    xml: &str,
) -> EasRawResponse {
    let codec = Wbxml::new();
    let body = codec.encode(xml).expect("request fixture must encode");
    let response = client
        .post(format!(
            "{base}/Microsoft-Server-ActiveSync?Cmd={command}&User={OWNER}&DeviceId={device_id}&DeviceType=Outlook"
        ))
        .basic_auth(OWNER, Some(PASSWORD))
        .header(header::CONTENT_TYPE, "application/vnd.ms-sync.wbxml")
        .header("MS-ASProtocolVersion", "16.1")
        .header("MS-ASAcceptMultiPart", "T")
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
    EasRawResponse {
        status,
        content_type,
        body: response.bytes().await.unwrap().to_vec(),
    }
}

/// Split a `application/vnd.ms-sync.multipart` body into its parts
/// ([MS-ASCMD] §2.2.1.10.1.1): u32 part count, one `{offset, length}` pair per
/// part, then the raw part bytes.
fn parse_multipart_parts(body: &[u8]) -> Vec<Vec<u8>> {
    let count = u32::from_le_bytes(body[0..4].try_into().unwrap()) as usize;
    let mut parts = Vec::with_capacity(count);
    for index in 0..count {
        let meta = 4 + index * 8;
        let offset = u32::from_le_bytes(body[meta..meta + 4].try_into().unwrap()) as usize;
        let length = u32::from_le_bytes(body[meta + 4..meta + 8].try_into().unwrap()) as usize;
        parts.push(body[offset..offset + length].to_vec());
    }
    parts
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

/// Text of the FIRST element whose LOCAL name is `name`.
///
/// A WBXML-decoded response keeps the canonical `Prefix:` form (e.g.
/// `<Email:Subject>`) but declares no namespace bindings, which roxmltree
/// rejects outright — so the scan is prefix-agnostic and namespace-free.
fn tag_text(xml: &str, name: &str) -> Option<String> {
    use quick_xml::events::Event;
    let mut reader = quick_xml::Reader::from_str(xml);
    reader.config_mut().trim_text(true);
    let mut buf = Vec::new();
    let mut inside = false;
    let mut value = String::new();
    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(start))
                if start.name().local_name().as_ref().as_bytes() == name.as_bytes() =>
            {
                inside = true;
                value.clear();
            }
            Ok(Event::Text(text)) if inside => value.push_str(text.as_ref()),
            Ok(Event::CData(text)) if inside => value.push_str(text.as_ref()),
            Ok(Event::GeneralRef(reference)) if inside => match reference.as_ref() {
                "lt" => value.push('<'),
                "gt" => value.push('>'),
                "amp" => value.push('&'),
                "quot" => value.push('"'),
                "apos" => value.push('\''),
                _ => {}
            },
            Ok(Event::End(end))
                if end.name().local_name().as_ref().as_bytes() == name.as_bytes() =>
            {
                return Some(value);
            }
            Ok(Event::Eof) | Err(_) => return None,
            _ => {}
        }
        buf.clear();
    }
}

fn email_sync(sync_key: &str) -> String {
    format!(
        r##"<Sync xmlns="AirSync:"><Collections><Collection><Class>Email</Class><SyncKey>{sync_key}</SyncKey><CollectionId>2</CollectionId><GetChanges>1</GetChanges><WindowSize>25</WindowSize></Collection></Collections></Sync>"##
    )
}

/// RFC 8620 §5.6 `Email/queryChanges` result: `added` is a list of
/// `{id, index}` objects, `removed` a bare id list, and the gateway advances its
/// stored token from `newQueryState` (a missing one forces a re-prime).
fn jmap_query_changes_response(added: &[String], removed: &[String], updated: &[String]) -> Value {
    json!({
        "sessionId": "contract",
        "methodResponses": [["Email/queryChanges", {
            "accountId": "a",
            "oldQueryState": "query-state",
            "newQueryState": "query-state-2",
            "total": 0,
            "added": added
                .iter()
                .enumerate()
                .map(|(index, id)| json!({ "id": id, "index": index }))
                .collect::<Vec<_>>(),
            "removed": removed,
            "updated": updated
        }, "qc0"]]
    })
}

/// One contract calendar event as `CalendarEvent/get` returns it.
/// The fixture start instant: DISTINCTIVE on purpose, so a test can prove a
/// partial update preserved the untouched time rather than defaulting it.
const CONTRACT_EVENT_START: &str = "2026-01-15T09:00:00";
/// The fixture location name, preserved across a subject-only update.
const CONTRACT_EVENT_LOCATION: &str = "Room 4B";

fn contract_calendar_event(id: &str, subject: &str) -> Value {
    json!({
        "id": id,
        "uid": format!("{id}@example.com"),
        "title": subject,
        "start": CONTRACT_EVENT_START,
        "timeZone": "Etc/UTC",
        "duration": "PT30M",
        "locations": {"loc-1": {"@type": "Location", "name": CONTRACT_EVENT_LOCATION}},
        "calendarIds": {"cal-1": true},
        "@type": "Event"
    })
}

/// Matcher: the request body contains `needle` and does NOT contain `absent`.
/// Used to separate a standalone `CalendarEvent/get` (the update's base read)
/// from the batch that also carries the `CalendarEvent/set`.
struct BodyContainsAndNot {
    needle: &'static str,
    absent: &'static str,
}

impl wiremock::Match for BodyContainsAndNot {
    fn matches(&self, request: &wiremock::Request) -> bool {
        let body = String::from_utf8_lossy(&request.body);
        body.contains(self.needle) && !body.contains(self.absent)
    }
}

fn contract_email(seen: bool) -> Value {
    contract_email_with_id("msg-1", seen)
}

fn contract_email_with_id(id: &str, seen: bool) -> Value {
    json!({
        "id": id,
        "threadId": "thread-1",
        "mailboxIds": {"inbox-1": true},
        "blobIds": ["blob-1"],
        "size": 123,
        "headers": [],
        "textBody": [{"partId": "1", "blobId": "blob-1", "size": 5, "type": "text/plain", "charset": "us-ascii"}],
        "htmlBody": null,
        "bodyValues": {"1": {"value": "hello", "isEncodingProblem": false, "isTruncated": false}},
        "attachments": [],
        "keywords": {"$seen": seen, "$answered": false, "$flagged": false},
        "receivedAt": "2026-01-15T10:00:00Z",
        "sentAt": "2026-01-15T10:00:00Z",
        "from": [{"name": "Sender", "email": "sender@example.com"}],
        "to": [{"name": "Recipient", "email": "recipient@example.com"}]
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

    // Email/query and its Email/get are BATCHED into one request
    // (RFC 8621 §3.6): the get back-references the query's /ids, so the
    // response must carry both method results.
    Mock::given(method("POST"))
        .and(path("/"))
        .and(body_string_contains(r#""Email/query","#))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "sessionId": "contract",
            "methodResponses": [
                ["Email/query", {
                    "accountId": "a",
                    "queryState": "query-state",
                    "canCalculateChanges": true,
                    "position": 0,
                    "total": 0,
                    "ids": []
                }, "q0"],
                ["Email/get", {
                    "accountId": "a",
                    "state": "email-state",
                    "list": [],
                    "notFound": []
                }, "g0"]
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
            ResponseTemplate::new(200).set_body_json(jmap_query_changes_response(&[], &[], &[])),
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

    // The submission batch (Email/set + EmailSubmission/set, RFC 8621 §7.5).
    // The `created` ids echo the CLIENT's creation ids (`e0`/`s0`); a send is
    // confirmed only by `EmailSubmission/set` `created.s0`.
    Mock::given(method("POST"))
        .and(path("/"))
        .and(body_string_contains(r#""Email/set""#))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "methodResponses": [
                ["Email/set", {
                    "accountId": "a",
                    "newState": "send-state",
                    "created": {"e0": {"id": "email-1"}},
                    "updated": {},
                    "destroyed": {},
                    "notCreated": {},
                    "notUpdated": {},
                    "notDestroyed": {}
                }, "ec0"],
                ["EmailSubmission/set", {
                    "accountId": "a",
                    "created": {"s0": {"id": "sub-1"}},
                    "notCreated": {}
                }, "ess0"]
            ]
        })))
        .mount(&gateway.backend)
        .await;

    let sendmail_xml = r#"<SendMail xmlns="ComposeMail:"><SaveInSentItems/><Mime>RnJvbTogc2VuZGVyQGV4YW1wbGUuY29tDQpUbzogcmVjaXBpZW50QGV4YW1wbGUuY29tDQpTdWJqZWN0OiBUZXN0DQoNCkhlbGxvIHdvcmxkDQo=</Mime></SendMail>"#;
    let response = gateway.eas("SendMail", sendmail_xml).await;
    assert_eq!(response.status, StatusCode::OK);
    let status = tag_text(&response.xml, "Status");

    assert_eq!(
        status.as_deref(),
        Some("1"),
        "a submitted message must answer the EAS SendMail success status, got XML: {}",
        response.xml
    );
    // A Status 1 alone does not prove the message was SUBMITTED: assert the
    // recorded wire traffic carried the `EmailSubmission/set` half of the
    // RFC 8621 §7.5 batch (a bare `Email/set` would only have staged a draft).
    let requests = gateway
        .backend
        .received_requests()
        .await
        .unwrap_or_default();
    assert!(
        requests
            .iter()
            .any(|r| String::from_utf8_lossy(&r.body).contains("\"EmailSubmission/set\"")),
        "SendMail must reach EmailSubmission/set, recorded bodies: {:?}",
        requests
            .iter()
            .map(|r| String::from_utf8_lossy(&r.body).to_string())
            .collect::<Vec<_>>()
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

    // Execute: a VALID `RopGetHierarchyTable` request ([MS-OXCROPS]
    // §2.2.4.13.1): RopId(0x04) · LogonId · InputHandleIndex · OutputHandleIndex
    // · TableFlags. `0x03` would be `RopOpenMessage`, and omitting
    // `InputHandleIndex`/`TableFlags` shifts every following field.
    let execute_body = vec![
        0x04, // RopId ROP_GET_HIERARCHY_TABLE
        0x00, // LogonId
        0x04, // InputHandleIndex (the logon handle installed by RopLogon)
        0x04, // OutputHandleIndex
        0x00, // TableFlags ([MS-OXCFOLD] §2.2.1.13.1; 0 = no flags)
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
    // Decode the ROP response (4-byte RopBufferSize, then the ROP buffer) and
    // assert the hierarchy table was actually OPENED (§2.2.4.13.2): RopId ·
    // OutputHandleIndex · ReturnValue(4) · RowCount(4). Relying on the HTTP
    // status alone would accept a ROP-level NotFound.
    let execute_bytes = response.bytes().await.unwrap();
    let rop = &execute_bytes[4..];
    assert_eq!(
        rop[0], 0x04,
        "the response must be a RopGetHierarchyTable ROP"
    );
    assert_eq!(rop[1], 0x04, "OutputHandleIndex must be echoed");
    let return_value = u32::from_le_bytes([rop[2], rop[3], rop[4], rop[5]]);
    assert_eq!(return_value, 0, "RopGetHierarchyTable must succeed");
    let row_count = u32::from_le_bytes([rop[6], rop[7], rop[8], rop[9]]);
    assert!(row_count > 0, "the hierarchy table must carry rows");

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

#[tokio::test]
async fn eas_email_sync_flag_change_over_http() {
    // Covers Outlook Android flag-change flow: prime → flag edit → delta
    // reports <Change> for the flagged item, not <Add>.
    let gateway = ContractGateway::spawn().await;

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

    // Metadata scan: `Email/query` + keyword-only `Email/get`, batched
    // (RFC 8621 §3.6). The prime's scan seeds the delivery baseline (message
    // unflagged); the delta's scan observes the flag edit.
    let scan_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let scan_counter = scan_calls.clone();
    Mock::given(method("POST"))
        .and(path("/"))
        .and(body_string_contains(r#""properties":["keywords"]"#))
        .respond_with(move |_req: &wiremock::Request| {
            let seen = scan_counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst) > 0;
            ResponseTemplate::new(200).set_body_json(json!({
                "sessionId": "contract",
                "methodResponses": [
                    ["Email/query", {
                        "accountId": "a",
                        "queryState": "query-state",
                        "canCalculateChanges": true,
                        "position": 0,
                        "total": 1,
                        "ids": ["msg-1"]
                    }, "q0"],
                    ["Email/get", {
                        "accountId": "a",
                        "state": "get-state",
                        "list": [{"id": "msg-1", "keywords": {"$seen": seen}}],
                        "notFound": []
                    }, "g0"]
                ]
            }))
        })
        .mount(&gateway.backend)
        .await;

    // Delivery render: the same batch with the FULL property list and body
    // values. The prime's <Add> is unflagged; the delta's <Change> carries the
    // flag edit.
    let render_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let render_counter = render_calls.clone();
    Mock::given(method("POST"))
        .and(path("/"))
        .and(body_string_contains(r#""fetchTextBodyValues":true"#))
        .respond_with(move |_req: &wiremock::Request| {
            let n = render_counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let seen = n > 0;
            ResponseTemplate::new(200).set_body_json(json!({
                "sessionId": "contract",
                "methodResponses": [
                    ["Email/query", {
                        "accountId": "a",
                        "queryState": "query-state",
                        "canCalculateChanges": true,
                        "position": 0,
                        "total": 1,
                        "ids": ["msg-1"]
                    }, "q0"],
                    ["Email/get", {
                        "accountId": "a",
                        "state": "get-state",
                        "list": [contract_email(seen)],
                        "notFound": []
                    }, "g0"]
                ]
            }))
        })
        .mount(&gateway.backend)
        .await;

    // RFC 8620 §5.6: a flag edit is NOT a membership change, so the queryChanges
    // result carries no ids; the delta detects it with its keyword baseline.
    Mock::given(method("POST"))
        .and(path("/"))
        .and(body_string_contains(r#""Email/queryChanges","#))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(jmap_query_changes_response(&[], &[], &[])),
        )
        .mount(&gateway.backend)
        .await;

    let prime = gateway.eas("Sync", &email_sync("0")).await;
    assert_eq!(prime.status, StatusCode::OK);
    assert_eq!(tag_text(&prime.xml, "Status").as_deref(), Some("1"));
    let prime_key = tag_text(&prime.xml, "SyncKey").expect("prime key");
    assert_ne!(prime_key, "0");

    // Delta with flag change: should report <Change> not <Add> for msg-1
    let delta = gateway.eas("Sync", &email_sync(&prime_key)).await;
    assert_eq!(delta.status, StatusCode::OK);
    assert_eq!(tag_text(&delta.xml, "Status").as_deref(), Some("1"));
    // The edit must arrive as a <Change> on the item the prime already
    // delivered — never re-added — and the rendered value must carry the
    // edited flag. Asserting the flag VALUE (not just the id's presence) keeps
    // a mock that silently re-served the unflagged prime response from
    // satisfying the test.
    assert!(
        delta.xml.contains("<Change>"),
        "a flag edit must be reported as <Change>, got: {}",
        delta.xml
    );
    assert!(
        !delta.xml.contains("<Add>"),
        "the delta must not re-add the held message: {}",
        delta.xml
    );
    assert_eq!(
        tag_text(&delta.xml, "Read").as_deref(),
        Some("1"),
        "the <Change> must carry the edited read flag: {}",
        delta.xml
    );

    gateway.assert_backend_used().await;
}

#[tokio::test]
async fn eas_itemoperations_multipart_fetch_over_http() {
    // Covers MS-ASAcceptMultiPart multipart ItemOperations Fetch delivery
    // ([MS-ASCMD] §2.2.1.10.1): the WBXML document is part 0 and every fetched
    // attachment is a raw binary part referenced by <Part>n</Part>.
    let gateway = ContractGateway::spawn().await;

    Mock::given(method("GET"))
        .and(path_regex(r"^/download/a/attach-1$"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_raw(b"raw-attachment-bytes-1", "application/octet-stream"),
        )
        .expect(1)
        .mount(&gateway.backend)
        .await;

    Mock::given(method("GET"))
        .and(path_regex(r"^/download/a/attach-2$"))
        .respond_with(
            ResponseTemplate::new(200).set_body_raw(b"raw-attachment-bytes-2", "image/png"),
        )
        .expect(1)
        .mount(&gateway.backend)
        .await;

    let fetch_xml = r#"<ItemOperations xmlns="ItemOperations:" xmlns:AirSync="AirSync:" xmlns:AirSyncBase="AirSyncBase:"><Fetch><AirSyncBase:FileReference>attach-1</AirSyncBase:FileReference><Properties><AirSyncBase:Data/></Properties></Fetch><Fetch><AirSyncBase:FileReference>attach-2</AirSyncBase:FileReference><Properties><AirSyncBase:Data/></Properties></Fetch></ItemOperations>"#;
    let response = gateway.eas_multipart("ItemOperations", fetch_xml).await;
    assert_eq!(response.status, StatusCode::OK);
    assert_eq!(
        response.content_type, "application/vnd.ms-sync.multipart",
        "MS-ASAcceptMultiPart must select multipart delivery"
    );

    let parts = parse_multipart_parts(&response.body);
    assert_eq!(parts.len(), 3, "expected WBXML + 2 binary parts");
    let xml = Wbxml::new()
        .decode(&parts[0])
        .expect("the first part must be the WBXML document");
    // Validate each `<Fetch>` block INDIVIDUALLY: its status, the echoed
    // FileReference and the `<Part>` index it names must all agree with the
    // bytes in `parts`. Checking the document as a whole would accept swapped
    // part references as long as the binary parts stayed in order.
    let blocks: Vec<&str> = xml
        .split("<Fetch>")
        .skip(1)
        .filter_map(|b| b.split("</Fetch>").next())
        .collect();
    assert_eq!(
        blocks.len(),
        2,
        "one response Fetch per request, got: {xml}"
    );
    let mut referenced = Vec::new();
    for block in &blocks {
        assert!(
            block.contains("<Status>1</Status>"),
            "every Fetch must succeed, got: {block}"
        );
        let file_ref = if block.contains("<AirSyncBase:FileReference>attach-1") {
            "attach-1"
        } else if block.contains("<AirSyncBase:FileReference>attach-2") {
            "attach-2"
        } else {
            panic!("Fetch block must echo its FileReference, got: {block}")
        };
        let part: usize = block
            .split("<Part>")
            .nth(1)
            .and_then(|rest| rest.split("</Part>").next())
            .and_then(|v| v.trim().parse().ok())
            .unwrap_or_else(|| panic!("Fetch block must name its part index, got: {block}"));
        let expected: &[u8] = match file_ref {
            "attach-1" => b"raw-attachment-bytes-1",
            _ => b"raw-attachment-bytes-2",
        };
        assert_eq!(
            parts.get(part).map(Vec::as_slice),
            Some(expected),
            "part {part} must carry the bytes of {file_ref}"
        );
        referenced.push(part);
    }
    // EXACT coverage, not just the count: the referenced part indexes must be
    // precisely 1..parts.len() (no gap, no duplicate) and the referenced
    // FileReferences must be exactly the requested attachment set.
    referenced.sort_unstable();
    let expected_parts: Vec<usize> = (1..parts.len()).collect();
    assert_eq!(
        referenced, expected_parts,
        "every binary part must be referenced exactly once"
    );
    let mut references: Vec<&str> = blocks
        .iter()
        .map(|block| {
            if block.contains("<AirSyncBase:FileReference>attach-1") {
                "attach-1"
            } else if block.contains("<AirSyncBase:FileReference>attach-2") {
                "attach-2"
            } else {
                panic!("Fetch block must echo its FileReference, got: {block}")
            }
        })
        .collect();
    references.sort_unstable();
    assert_eq!(
        references,
        vec!["attach-1", "attach-2"],
        "the response must cover exactly the requested attachments"
    );
    gateway.assert_backend_used().await;
}

#[tokio::test]
async fn eas_itemoperations_invalid_key_recovery_over_http() {
    // Covers invalid-key recovery: SyncKey 0 prime, then a stale SyncKey
    // returns status 3 with SyncKey reset to 0.
    let gateway = ContractGateway::spawn().await;

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

    Mock::given(method("POST"))
        .and(path("/"))
        .and(body_string_contains(r#""Email/query","#))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "sessionId": "contract",
            "methodResponses": [
                ["Email/query", {
                    "accountId": "a",
                    "queryState": "query-state",
                    "canCalculateChanges": true,
                    "position": 0,
                    "total": 0,
                    "ids": []
                }, "q0"],
                ["Email/get", {
                    "accountId": "a",
                    "state": "email-state",
                    "list": [],
                    "notFound": []
                }, "g0"]
            ]
        })))
        .mount(&gateway.backend)
        .await;

    let prime = gateway.eas("Sync", &email_sync("0")).await;
    assert_eq!(prime.status, StatusCode::OK);
    assert_eq!(tag_text(&prime.xml, "Status").as_deref(), Some("1"));
    let prime_key = tag_text(&prime.xml, "SyncKey").expect("prime key");
    assert_ne!(prime_key, "0");

    // Stale key: must get status 3 and SyncKey reset to 0
    let stale = gateway.eas("Sync", &email_sync("stale-key-12345")).await;
    assert_eq!(stale.status, StatusCode::OK);
    assert_eq!(tag_text(&stale.xml, "Status").as_deref(), Some("3"));
    assert_eq!(tag_text(&stale.xml, "SyncKey").as_deref(), Some("0"));

    // Reprime with SyncKey 0 works again
    let reprime = gateway.eas("Sync", &email_sync("0")).await;
    assert_eq!(reprime.status, StatusCode::OK);
    assert_eq!(tag_text(&reprime.xml, "Status").as_deref(), Some("1"));
    let reprime_key = tag_text(&reprime.xml, "SyncKey").expect("reprime key");
    assert_ne!(reprime_key, "0");
    assert_ne!(reprime_key, prime_key);

    gateway.assert_backend_used().await;
}

#[tokio::test]
async fn eas_email_sync_overflow_window_over_http() {
    // Covers overflow window: many changes exceeding MaxChangesReturned
    // produce deferred behavior and the client must sync again.
    let gateway = ContractGateway::spawn().await;

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

    // 30 messages: more than the client's `<WindowSize>25</WindowSize>`, so the
    // delta must window the change set and queue the remainder.
    let ids: Vec<String> = (1..=30).map(|n| format!("msg-{n}")).collect();
    let batch_ids = json!(&ids);
    let batch_list: Vec<Value> = ids
        .iter()
        .map(|id| contract_email_with_id(id, false))
        .collect();
    Mock::given(method("POST"))
        .and(path("/"))
        .and(body_string_contains(r#""Email/query","#))
        .respond_with(move |_req: &wiremock::Request| {
            ResponseTemplate::new(200).set_body_json(json!({
                "sessionId": "contract",
                "methodResponses": [
                    ["Email/query", {
                        "accountId": "a",
                        "queryState": "query-state",
                        "canCalculateChanges": true,
                        "position": 0,
                        "total": 30,
                        "ids": batch_ids
                    }, "q0"],
                    ["Email/get", {
                        "accountId": "a",
                        "state": "get-state",
                        "list": batch_list,
                        "notFound": []
                    }, "g0"]
                ]
            }))
        })
        .mount(&gateway.backend)
        .await;

    let prime = gateway.eas("Sync", &email_sync("0")).await;
    assert_eq!(prime.status, StatusCode::OK);
    assert_eq!(tag_text(&prime.xml, "Status").as_deref(), Some("1"));
    let prime_key = tag_text(&prime.xml, "SyncKey").expect("prime key");

    // Plain `Email/get` by explicit ids: the delta renders each `<Change>` from
    // one of these (no back-referenced query in the batch).
    Mock::given(method("POST"))
        .and(path("/"))
        .and(body_string_contains(r#""Email/get","#))
        .respond_with(|req: &wiremock::Request| {
            let body: Value = serde_json::from_slice(&req.body).unwrap_or(Value::Null);
            let requested: Vec<String> = body["methodCalls"]
                .as_array()
                .and_then(|calls| calls.iter().find(|call| call[0] == "Email/get"))
                .and_then(|call| call[1]["ids"].as_array())
                .map(|ids| {
                    ids.iter()
                        .filter_map(|id| id.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default();
            ResponseTemplate::new(200).set_body_json(json!({
                "sessionId": "contract",
                "methodResponses": [
                    ["Email/get", {
                        "accountId": "a",
                        "state": "get-state",
                        "list": requested
                            .iter()
                            .map(|id| contract_email_with_id(id, true))
                            .collect::<Vec<_>>(),
                        "notFound": []
                    }, "g0"]
                ]
            }))
        })
        .mount(&gateway.backend)
        .await;

    // A bulk flag edit: RFC 8620 §5.6 query changes for every message.
    let updated = ids.clone();
    Mock::given(method("POST"))
        .and(path("/"))
        .and(body_string_contains(r#""Email/queryChanges","#))
        .respond_with(move |_req: &wiremock::Request| {
            ResponseTemplate::new(200).set_body_json(jmap_query_changes_response(
                &[],
                &[],
                &updated,
            ))
        })
        .mount(&gateway.backend)
        .await;

    let delta = gateway.eas("Sync", &email_sync(&prime_key)).await;
    assert_eq!(delta.status, StatusCode::OK);
    assert_eq!(tag_text(&delta.xml, "Status").as_deref(), Some("1"));
    let delta_key = tag_text(&delta.xml, "SyncKey").expect("delta key");
    assert_ne!(delta_key, prime_key);

    // The window is bounded by WindowSize: the first 25 operations are
    // delivered and the remaining 5 stay queued for the next Sync, which must
    // report only the queued remainder (never the whole backlog again).
    let ops = |xml: &str| xml.matches("<Add>").count() + xml.matches("<Change>").count();
    assert_eq!(
        ops(&delta.xml),
        25,
        "expected a 25-item window, got: {}",
        delta.xml
    );
    assert!(delta.xml.contains("msg-1") && !delta.xml.contains("msg-30"));
    // A clipped window must tell the client to sync again ([MS-ASCMD]
    // §2.2.3.116); a silently truncated response would leave the remainder
    // undelivered with the client believing it is up to date.
    assert!(
        delta.xml.contains("<MoreAvailable"),
        "a clipped window must advertise MoreAvailable, got: {}",
        delta.xml
    );

    let drained = gateway.eas("Sync", &email_sync(&delta_key)).await;
    assert_eq!(drained.status, StatusCode::OK);
    assert_eq!(tag_text(&drained.xml, "Status").as_deref(), Some("1"));
    assert_eq!(ops(&drained.xml), 5, "{}", drained.xml);
    assert!(
        !drained.xml.contains("<MoreAvailable"),
        "the drained remainder must not advertise MoreAvailable, got: {}",
        drained.xml
    );

    // Exactly-once delivery: the two responses together carry every changed
    // item, each exactly once, so a queued remainder is neither dropped nor
    // re-delivered by the continuation. The marker includes the `<Add>`/
    // `<Change>` opener because the rendered ApplicationData repeats the
    // ServerId inside the message body.
    for id in &ids {
        let seen = ["<Add>", "<Change>"]
            .iter()
            .map(|opener| {
                let marker = format!("{opener}<ServerId>em-{id}</ServerId>");
                delta.xml.matches(&marker).count() + drained.xml.matches(&marker).count()
            })
            .sum::<usize>();
        assert_eq!(
            seen, 1,
            "{id} must be delivered exactly once across the window and its continuation"
        );
    }

    gateway.assert_backend_used().await;
}

#[tokio::test]
async fn ews_getitem_calendar_success_over_http() {
    // Covers EWS GetItem for a calendar item over real HTTP.
    let gateway = ContractGateway::spawn().await;
    gateway
        .state
        .storage
        .upsert_item_map(
            OWNER,
            "jmap://calendar/a",
            "jmap://calendar/a/cal-1",
            "cal-1",
            "cal-1@example.com",
            "ck-1",
        )
        .await
        .unwrap();

    Mock::given(method("POST"))
        .and(path("/"))
        .and(body_string_contains("CalendarEvent/get"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "sessionId": "contract",
            "methodResponses": [
                ["CalendarEvent/get", {
                    "accountId": "a",
                    "state": "cal-state",
                    "list": [contract_calendar_event("cal-1", "Team Standup")],
                    "notFound": []
                }, "eg0"]
            ]
        })))
        .expect(1)
        .mount(&gateway.backend)
        .await;

    let request = r#"<?xml version="1.0" encoding="utf-8"?>
<s:Envelope xmlns:s="http://schemas.xmlsoap.org/soap/envelope/" xmlns:m="http://schemas.microsoft.com/exchange/services/2006/messages" xmlns:t="http://schemas.microsoft.com/exchange/services/2006/types">
  <s:Body>
    <m:GetItem>
      <m:ItemShape><t:BaseShape>AllProperties</t:BaseShape></m:ItemShape>
      <m:ItemIds><t:ItemId Id="cal-1" ChangeKey="ck-1"/></m:ItemIds>
    </m:GetItem>
  </s:Body>
</s:Envelope>"#;
    let response = gateway.ews(request).await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = response.text().await.unwrap();
    assert!(body.contains("GetItemResponse"));
    assert!(body.contains("ResponseClass=\"Success\""));
    gateway.assert_backend_used().await;
}

#[tokio::test]
async fn ews_updateitem_calendar_success_over_http() {
    // Covers EWS UpdateItem for a calendar item over real HTTP.
    let gateway = ContractGateway::spawn().await;
    gateway
        .state
        .storage
        .upsert_item_map(
            OWNER,
            "jmap://calendar/a",
            "jmap://calendar/a/cal-1",
            "cal-1",
            "cal-1@example.com",
            "ck-1",
        )
        .await
        .unwrap();

    // The update batch reads the current event, writes the patched one, and
    // reads it back to verify the write: the responder echoes the written
    // object so the read-back matches it property-for-property. The written
    // payload is also CAPTURED, so the test can assert a subject-only update
    // preserved the untouched start/location instead of trusting the echo.
    // The base event the update patches MUST come from the backend: mount a
    // standalone `CalendarEvent/get` (a request that carries no set call) so
    // the untouched start/location come from the fixture instead of the
    // gateway's blank fallback template.
    Mock::given(method("POST"))
        .and(path("/"))
        .and(BodyContainsAndNot {
            needle: "CalendarEvent/get",
            absent: "CalendarEvent/set",
        })
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "sessionId": "contract",
            "methodResponses": [
                ["CalendarEvent/get", {
                    "accountId": "a",
                    "state": "cal-state",
                    "list": [contract_calendar_event("cal-1", "Team Standup")],
                    "notFound": []
                }, "eg0"]
            ]
        })))
        .mount(&gateway.backend)
        .await;

    let written_payloads: Arc<std::sync::Mutex<Vec<Value>>> =
        Arc::new(std::sync::Mutex::new(Vec::new()));
    let capture = written_payloads.clone();
    Mock::given(method("POST"))
        .and(path("/"))
        .and(body_string_contains("CalendarEvent/set"))
        .respond_with(move |req: &wiremock::Request| {
            let body: Value = serde_json::from_slice(&req.body).unwrap_or(Value::Null);
            let calls = body["methodCalls"].as_array().cloned().unwrap_or_default();
            let written = calls
                .iter()
                .find(|call| call[0] == "CalendarEvent/set")
                .and_then(|call| call[1]["update"].get("cal-1").cloned())
                .map(|mut event| {
                    event["id"] = json!("cal-1");
                    event
                })
                .unwrap_or_else(|| contract_calendar_event("cal-1", "Team Standup"));
            capture.lock().unwrap().push(written.clone());
            let responses: Vec<Value> = calls
                .iter()
                .map(|call| {
                    let tag = call[2].clone();
                    match call[0].as_str().unwrap_or_default() {
                        "CalendarEvent/get" => json!([
                            "CalendarEvent/get",
                            {
                                "accountId": "a",
                                "state": "cal-state",
                                "list": [written.clone()],
                                "notFound": []
                            },
                            tag
                        ]),
                        "CalendarEvent/set" => json!([
                            "CalendarEvent/set",
                            {
                                "accountId": "a",
                                "newState": "set-state",
                                "created": {},
                                "updated": {"cal-1": {"id": "cal-1"}},
                                "destroyed": {},
                                "notCreated": {},
                                "notUpdated": {},
                                "notDestroyed": {}
                            },
                            tag
                        ]),
                        other => json!([other, {"accountId": "a"}, tag]),
                    }
                })
                .collect();
            ResponseTemplate::new(200)
                .set_body_json(json!({"sessionId": "contract", "methodResponses": responses}))
        })
        .mount(&gateway.backend)
        .await;

    let request = r#"<?xml version="1.0" encoding="utf-8"?>
<s:Envelope xmlns:s="http://schemas.xmlsoap.org/soap/envelope/" xmlns:m="http://schemas.microsoft.com/exchange/services/2006/messages" xmlns:t="http://schemas.microsoft.com/exchange/services/2006/types">
  <s:Body>
    <m:UpdateItem ConflictResolution="AutoResolve" MessageDisposition="SaveOnly">
      <m:ItemChanges>
        <t:ItemChange>
          <t:ItemId Id="cal-1"/>
          <t:Updates>
            <t:SetItemField>
              <t:FieldURI FieldURI="item:Subject"/>
              <t:CalendarItem><t:Subject>Updated Meeting</t:Subject></t:CalendarItem>
            </t:SetItemField>
          </t:Updates>
        </t:ItemChange>
      </m:ItemChanges>
    </m:UpdateItem>
  </s:Body>
</s:Envelope>"#;
    let response = gateway.ews(request).await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = response.text().await.unwrap();
    assert!(body.contains("UpdateItemResponse"));
    assert!(body.contains("ResponseClass=\"Success\""));
    let captured = written_payloads.lock().unwrap().clone();
    let written = captured
        .first()
        .expect("the update must have issued a CalendarEvent/set");
    assert_eq!(
        written["title"].as_str(),
        Some("Updated Meeting"),
        "the subject-only update must be written"
    );
    assert_eq!(
        written["start"].as_str(),
        Some(CONTRACT_EVENT_START),
        "a subject-only update must preserve the event start"
    );
    // The location keeps its NAME but the gateway mints its own location id, so
    // assert on the name rather than the fixture's id.
    let location_names: Vec<&str> = written["locations"]
        .as_object()
        .map(|m| {
            m.values()
                .filter_map(|l| l["name"].as_str())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    assert_eq!(
        location_names,
        vec![CONTRACT_EVENT_LOCATION],
        "a subject-only update must preserve the event location (written: {written})"
    );
    assert!(
        written["description"].is_null()
            || written["description"].as_str().is_none_or(str::is_empty),
        "a subject-only update must not invent a description (written: {written})"
    );
    gateway.assert_backend_used().await;
}

#[tokio::test]
async fn ews_deleteitem_calendar_success_over_http() {
    // Covers EWS DeleteItem for a calendar item over real HTTP. The ChangeKey
    // is NOT hard-coded: it is read back from a GetItem, so the request carries
    // the version the server actually issued and `validate_item_change_key`
    // accepts it.
    let gateway = ContractGateway::spawn().await;
    gateway
        .state
        .storage
        .upsert_item_map(
            OWNER,
            "jmap://calendar/a",
            "jmap://calendar/a/cal-1",
            "cal-1",
            "cal-1@example.com",
            "ck-1",
        )
        .await
        .unwrap();

    Mock::given(method("POST"))
        .and(path("/"))
        .and(body_string_contains("CalendarEvent/get"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "sessionId": "contract",
            "methodResponses": [
                ["CalendarEvent/get", {
                    "accountId": "a",
                    "state": "cal-state",
                    "list": [contract_calendar_event("cal-1", "Team Standup")],
                    "notFound": []
                }, "eg0"]
            ]
        })))
        .mount(&gateway.backend)
        .await;

    Mock::given(method("POST"))
        .and(path("/"))
        .and(body_string_contains("CalendarEvent/set"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "sessionId": "contract",
            "methodResponses": [
                ["CalendarEvent/set", {
                    "accountId": "a",
                    "newState": "set-state",
                    "created": {},
                    "updated": {},
                    "destroyed": ["cal-1"],
                    "notCreated": {},
                    "notUpdated": {},
                    "notDestroyed": {}
                }, "es0"]
            ]
        })))
        .expect(1)
        .mount(&gateway.backend)
        .await;

    let get_item = r#"<?xml version="1.0" encoding="utf-8"?>
<s:Envelope xmlns:s="http://schemas.xmlsoap.org/soap/envelope/" xmlns:m="http://schemas.microsoft.com/exchange/services/2006/messages" xmlns:t="http://schemas.microsoft.com/exchange/services/2006/types">
  <s:Body>
    <m:GetItem>
      <m:ItemShape><t:BaseShape>AllProperties</t:BaseShape></m:ItemShape>
      <m:ItemIds><t:ItemId Id="cal-1"/></m:ItemIds>
    </m:GetItem>
  </s:Body>
</s:Envelope>"#;
    let got = gateway.ews(get_item).await;
    assert_eq!(got.status(), StatusCode::OK);
    let got_body = got.text().await.unwrap();
    let change_key = got_body
        .split("ChangeKey=\"")
        .nth(1)
        .and_then(|rest| rest.split('"').next())
        .expect("GetItem must report the item's ChangeKey")
        .to_string();
    assert_ne!(
        change_key, "ck-1",
        "the issued key is derived, not the raw etag"
    );

    let request = format!(
        r#"<?xml version="1.0" encoding="utf-8"?>
<s:Envelope xmlns:s="http://schemas.xmlsoap.org/soap/envelope/" xmlns:m="http://schemas.microsoft.com/exchange/services/2006/messages" xmlns:t="http://schemas.microsoft.com/exchange/services/2006/types">
  <s:Body>
    <m:DeleteItem DeleteType="HardDelete">
      <m:ItemIds><t:ItemId Id="cal-1" ChangeKey="{change_key}"/></m:ItemIds>
    </m:DeleteItem>
  </s:Body>
</s:Envelope>"#
    );
    let response = gateway.ews(&request).await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = response.text().await.unwrap();
    assert!(body.contains("DeleteItemResponse"));
    assert!(
        body.contains("ResponseClass=\"Success\""),
        "the server-issued ChangeKey must be accepted, got: {body}"
    );
    gateway.assert_backend_used().await;
}

#[tokio::test]
async fn ews_syncfolderitems_delta_over_http() {
    // Covers EWS SyncFolderItems delta flow: prime returns a cursor, then
    // delta reports only changes since the prime.
    let gateway = ContractGateway::spawn().await;

    Mock::given(method("POST"))
        .and(path("/"))
        .and(body_string_contains("CalendarEvent/query"))
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
        .mount(&gateway.backend)
        .await;

    // Standalone CalendarEvent/get issued by `load_current_calendar_items` for
    // the prime snapshot.
    Mock::given(method("POST"))
        .and(path("/"))
        .and(body_string_contains(r#""CalendarEvent/get""#))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "sessionId": "contract",
            "methodResponses": [
                ["CalendarEvent/get", {
                    "accountId": "a",
                    "state": "cal-state",
                    "list": [contract_calendar_event("cal-1", "Team Standup")],
                    "notFound": []
                }, "eg0"]
            ]
        })))
        .mount(&gateway.backend)
        .await;

    // Prime
    let prime_request = r#"<?xml version="1.0" encoding="utf-8"?>
<s:Envelope xmlns:s="http://schemas.xmlsoap.org/soap/envelope/" xmlns:m="http://schemas.microsoft.com/exchange/services/2006/messages" xmlns:t="http://schemas.microsoft.com/exchange/services/2006/types">
  <s:Body>
    <m:SyncFolderItems>
      <m:ItemShape><t:BaseShape>Default</t:BaseShape></m:ItemShape>
      <m:SyncFolderId><t:DistinguishedFolderId Id="calendar"/></m:SyncFolderId>
      <m:MaxChangesReturned>10</m:MaxChangesReturned>
    </m:SyncFolderItems>
  </s:Body>
</s:Envelope>"#;
    let prime = gateway.ews(prime_request).await;
    assert_eq!(prime.status(), StatusCode::OK);
    let prime_body = prime.text().await.unwrap();
    assert!(prime_body.contains("SyncState"));

    // Extract sync state from response (opaque gateway cursor)
    let sync_state = tag_text(&prime_body, "SyncState").expect("sync state");
    assert!(!sync_state.is_empty());

    // Journal a REAL change after the prime (the delta feed is the local
    // journal, not the backend's `CalendarEvent/changes`): the event is
    // re-stored with a new ETag, which is what a backend edit produces.
    gateway
        .state
        .storage
        .upsert_item_map(
            OWNER,
            "jmap://calendar/a",
            "jmap://calendar/a/cal-1",
            "cal-1",
            "cal-1@example.com",
            "ck-2",
        )
        .await
        .unwrap();

    Mock::given(method("POST"))
        .and(path("/"))
        .and(body_string_contains("CalendarEvent/changes"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "sessionId": "contract",
            "methodResponses": [
                ["CalendarEvent/changes", {
                    "accountId": "a",
                    "oldState": "old",
                    "newState": "new",
                    "hasMoreChanges": false,
                    "created": [],
                    "updated": ["cal-1"],
                    "destroyed": [],
                    "removed": []
                }, "ec0"]
            ]
        })))
        .mount(&gateway.backend)
        .await;

    // Delta
    let delta_request = format!(
        r#"<?xml version="1.0" encoding="utf-8"?>
<s:Envelope xmlns:s="http://schemas.xmlsoap.org/soap/envelope/" xmlns:m="http://schemas.microsoft.com/exchange/services/2006/messages" xmlns:t="http://schemas.microsoft.com/exchange/services/2006/types">
  <s:Body>
    <m:SyncFolderItems>
      <m:ItemShape><t:BaseShape>Default</t:BaseShape></m:ItemShape>
      <m:SyncFolderId><t:DistinguishedFolderId Id="calendar"/></m:SyncFolderId>
      <m:SyncState>{}</m:SyncState>
      <m:MaxChangesReturned>10</m:MaxChangesReturned>
    </m:SyncFolderItems>
  </s:Body>
</s:Envelope>"#,
        sync_state
    );
    let delta = gateway.ews(&delta_request).await;
    assert_eq!(delta.status(), StatusCode::OK);
    let delta_body = delta.text().await.unwrap();
    assert!(delta_body.contains("SyncFolderItemsResponse"));
    // The mocked change is an UPDATE of `cal-1`: the delta must report it as
    // such (an empty delta or an operation error would silently lose the
    // client's view of the event).
    assert!(
        delta_body.contains("NoError"),
        "the delta must succeed, got: {delta_body}"
    );
    assert!(
        delta_body.contains("<t:Update>"),
        "the changed event must be reported as an Update, got: {delta_body}"
    );
    assert!(
        delta_body.contains("Team Standup"),
        "the Update must carry the event's current data, got: {delta_body}"
    );
    gateway.assert_backend_used().await;
}

#[tokio::test]
async fn autodiscover_soap_success_over_http() {
    // Covers Autodiscover SOAP success path over real HTTP.
    let gateway = ContractGateway::spawn().await;

    let request = r#"<?xml version="1.0" encoding="utf-8"?>
<s:Envelope xmlns:s="http://www.w3.org/2003/05/soap-envelope" xmlns:a="http://schemas.microsoft.com/exchange/2010/Autodiscover">
  <s:Body>
    <a:GetUserSettingsRequestMessage>
      <a:Request>
        <a:Users><a:User><a:Mailbox>recorded@example.com</a:Mailbox></a:User></a:Users>
        <a:RequestedSettings><a:Setting>ExternalEwsUrl</a:Setting><a:Setting>InternalEwsUrl</a:Setting></a:RequestedSettings>
      </a:Request>
    </a:GetUserSettingsRequestMessage>
  </s:Body>
</s:Envelope>"#;
    let response = gateway
        .client
        .post(format!("{}/Autodiscover/Autodiscover.svc", gateway.base))
        .header(header::CONTENT_TYPE, "application/soap+xml; charset=utf-8")
        .body(request)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = response.text().await.unwrap();
    assert!(body.contains("Envelope"));
    assert!(body.contains("recorded@example.com"));
}

#[tokio::test]
async fn autodiscover_soap_error_over_http() {
    // Covers Autodiscover SOAP error path: invalid email returns error.
    let gateway = ContractGateway::spawn().await;

    let request = r#"<?xml version="1.0" encoding="utf-8"?>
<s:Envelope xmlns:s="http://www.w3.org/2003/05/soap-envelope" xmlns:a="http://schemas.microsoft.com/exchange/2010/Autodiscover">
  <s:Body>
    <a:GetUserSettingsRequestMessage>
      <a:Request>
        <a:Users><a:User><a:Mailbox>invalid@nonexistent.example</a:Mailbox></a:User></a:Users>
        <a:RequestedSettings><a:Setting>ExternalEwsUrl</a:Setting></a:RequestedSettings>
      </a:Request>
    </a:GetUserSettingsRequestMessage>
  </s:Body>
</s:Envelope>"#;
    let response = gateway
        .client
        .post(format!("{}/Autodiscover/Autodiscover.svc", gateway.base))
        .header(header::CONTENT_TYPE, "application/soap+xml; charset=utf-8")
        .body(request)
        .send()
        .await
        .unwrap();
    // Error path still returns HTTP 200 with a SOAP fault
    assert_eq!(response.status(), StatusCode::OK);
    let body = response.text().await.unwrap();
    assert!(body.contains("Fault") || body.contains("fault") || body.contains("Error"));
}

#[tokio::test]
async fn eas_sendmail_with_attachment_over_http() {
    // Covers EAS SendMail carrying a complex (multipart) MIME message over real
    // HTTP. Such a message is relayed byte-for-byte through SMTP
    // ([MS-ASCMD] §2.2.1.17); a deployment without SMTP must therefore FAIL
    // CLOSED with Status 4 instead of silently dropping the attachment or
    // pretending success.
    let gateway = ContractGateway::spawn().await;

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
        .expect(0)
        .mount(&gateway.backend)
        .await;

    // MIME with a PDF attachment part.
    let mime = "RnJvbTogc2VuZGVyQGV4YW1wbGUuY29tDQpUbzogcmVjaXBpZW50QGV4YW1wbGUuY29tDQpTdWJqZWN0OiBUZXN0IEF0dGFjaG1lbnQNCkNvbnRlbnQtVHlwZTogbXVsdGlwYXJ0L21peDsgYm91bmRhcnk9ImJvdW5kYXJ5LTEyMyINCg0KLS1ib3VuZGFyeS0xMjMNCkNvbnRlbnQtVHlwZTogdGV4dC9wbGFpbjsgY2hhcnNldD11dGYtOA0KDQpIZWxsbyBXb3JsZA0KDQotLWJvdW5kYXJ5LTEyMw0KQ29udGVudC1UeXBlOiBhcHBsaWNhdGlvbi9wZGY7IG5hbWU9InRlc3QucGRmIg0KQ29udGVudC1UQ3JhbnNmZXItRW5jb2Rpbmc6IGJhc2U2NA0KDQolUERGLTEuNA0KDQotLWJvdW5kYXJ5LTEyMy0tDQo=";
    let sendmail_xml = format!(
        r#"<SendMail xmlns="ComposeMail:"><SaveInSentItems/><Mime>{}</Mime></SendMail>"#,
        mime
    );
    let response = gateway.eas("SendMail", &sendmail_xml).await;
    assert_eq!(response.status, StatusCode::OK);
    assert_eq!(
        tag_text(&response.xml, "Status").as_deref(),
        Some("4"),
        "an attachment message without SMTP must fail closed, got: {}",
        response.xml
    );
    gateway.assert_backend_used().await;
}

#[tokio::test]
async fn eas_getitemestimate_over_http() {
    // Covers EAS GetItemEstimate for an email collection over real HTTP.
    let gateway = ContractGateway::spawn().await;

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

    Mock::given(method("POST"))
        .and(path("/"))
        .and(body_string_contains(r#""Email/query","#))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "sessionId": "contract",
            "methodResponses": [
                ["Email/query", {
                    "accountId": "a",
                    "queryState": "query-state",
                    "canCalculateChanges": true,
                    "position": 0,
                    "total": 42,
                    "ids": []
                }, "q0"],
                ["Email/get", {
                    "accountId": "a",
                    "state": "email-state",
                    "list": [],
                    "notFound": []
                }, "g0"]
            ]
        })))
        .mount(&gateway.backend)
        .await;

    // [MS-ASWBXML] §2.1.2.1.7: code page 6 has no `Class`/`SyncKey` tokens for
    // protocol 16.1 — both come from the AirSync page and must be declared.
    // Prime the collection first and use the ISSUED SyncKey: an estimate is
    // only meaningful for a window the gateway actually handed out (key 0 is
    // the un-primed state, which the gateway answers with status 4).
    let prime = gateway.eas("Sync", &email_sync("0")).await;
    assert_eq!(prime.status, StatusCode::OK);
    assert_eq!(tag_text(&prime.xml, "Status").as_deref(), Some("1"));
    let prime_key = tag_text(&prime.xml, "SyncKey").expect("prime key");
    assert_ne!(prime_key, "0");

    let estimate_xml = format!(
        r#"<GetItemEstimate xmlns="GetItemEstimate:" xmlns:AirSync="AirSync:"><Collections><Collection><AirSync:Class>Email</AirSync:Class><CollectionId>2</CollectionId><AirSync:SyncKey>{prime_key}</AirSync:SyncKey></Collection></Collections></GetItemEstimate>"#
    );
    let response = gateway.eas("GetItemEstimate", &estimate_xml).await;
    assert_eq!(response.status, StatusCode::OK);
    assert_eq!(
        tag_text(&response.xml, "Status").as_deref(),
        Some("1"),
        "an estimate for the issued SyncKey must succeed, got: {}",
        response.xml
    );
    assert!(
        tag_text(&response.xml, "Estimate").is_some(),
        "a successful GetItemEstimate must carry the Estimate count, got: {}",
        response.xml
    );

    // Invalid-key recovery is a SEPARATE concern: an unissued key answers
    // status 4 ([MS-ASCMD] §2.2.3.177.8).
    let stale = gateway
        .eas(
            "GetItemEstimate",
            r#"<GetItemEstimate xmlns="GetItemEstimate:" xmlns:AirSync="AirSync:"><Collections><Collection><AirSync:Class>Email</AirSync:Class><CollectionId>2</CollectionId><AirSync:SyncKey>never-issued</AirSync:SyncKey></Collection></Collections></GetItemEstimate>"#,
        )
        .await;
    assert_eq!(stale.status, StatusCode::OK);
    assert_eq!(
        tag_text(&stale.xml, "Status").as_deref(),
        Some("4"),
        "an unissued SyncKey must answer status 4, got: {}",
        stale.xml
    );
    gateway.assert_backend_used().await;
}

#[tokio::test]
async fn eas_meetingresponse_over_http() {
    // Covers EAS MeetingResponse over real HTTP: the attendee's calendar copy
    // is read from CalDAV, its PARTSTAT is patched, and the copy is written
    // back with If-Match. No <SendResponse> ⇒ no iTIP email.
    let gateway = ContractGateway::spawn().await;

    // The EAS calendar item the client is responding to (seeded into the item
    // map by an earlier Sync in production).
    gateway
        .state
        .storage
        .upsert_item_map(
            OWNER,
            "/dav/cal/recorded@example.com/calendar/",
            "/dav/cal/recorded@example.com/calendar/cal-item-1.ics",
            "cal-item-1",
            "cal-item-1@example.com",
            "\"etag-1\"",
        )
        .await
        .unwrap();

    let invitation = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//contract//EN\r\nBEGIN:VEVENT\r\nUID:cal-item-1@example.com\r\nSUMMARY:Team Standup\r\nDTSTART:20260115T090000Z\r\nDTEND:20260115T093000Z\r\nSEQUENCE:1\r\nORGANIZER;CN=Organizer:mailto:organizer@example.com\r\nATTENDEE;CN=Recorded;PARTSTAT=NEEDS-ACTION;RSVP=TRUE:mailto:recorded@example.com\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
    Mock::given(method("GET"))
        .and(path_regex(r"^/dav/cal/.*/cal-item-1\.ics$"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("ETag", "\"etag-1\"")
                .set_body_raw(invitation, "text/calendar"),
        )
        .expect(1)
        .mount(&gateway.backend)
        .await;

    Mock::given(method("PUT"))
        .and(path_regex(r"^/dav/cal/.*/cal-item-1\.ics$"))
        .and(body_string_contains("PARTSTAT=ACCEPTED"))
        .respond_with(ResponseTemplate::new(204).insert_header("ETag", "\"etag-2\""))
        .expect(1)
        .mount(&gateway.backend)
        .await;

    let meeting_response_xml = r#"<MeetingResponse xmlns="MeetingResponse:"><Request><RequestId>cal-item-1</RequestId><UserResponse>1</UserResponse></Request></MeetingResponse>"#;
    let response = gateway.eas("MeetingResponse", meeting_response_xml).await;
    assert_eq!(response.status, StatusCode::OK);
    assert_eq!(
        tag_text(&response.xml, "Status").as_deref(),
        Some("1"),
        "expected success, got: {}",
        response.xml
    );
    assert_eq!(
        tag_text(&response.xml, "CalendarId").as_deref(),
        Some("cal-item-1"),
        "an accepted meeting must echo its CalendarId, got: {}",
        response.xml
    );
    gateway.assert_backend_used().await;
}

// ========================================================================
// Stateful model-based protocol tests driving REAL production handlers
// through build_router against wiremock backends.
// Cover: Sync prime, continuation, delta, replayed key, stale key and the
// overflow window (see `eas_email_sync_prime_delta_invalid_key_reprime_over_http`,
// `eas_email_sync_overflow_window_over_http`, `ews_syncfolderitems_delta_over_http`).
// The SyncKey FORMAT itself is not asserted here: the issued key is validated
// by those wire tests (prime → delta must accept the key the prime returned,
// and an unissued key must answer Status 3).
// Invariants:
//   - A stale key must not resurrect deleted items.
//   - Identical retries cannot create duplicate backend objects.
//   - A failed durable write never advances the acknowledged SyncKey (covered
//     by `eas::tests` and `storage::tests`, which can inject the write fault).
// ========================================================================

#[test]
fn canonical_email_server_id_is_stable() {
    use exchange_gateway::email::{
        email_server_id_from_jmap_id, is_email_server_id, jmap_id_from_email_server_id,
    };
    let jmap_id = "msg-abc123-def456";
    let first = email_server_id_from_jmap_id(jmap_id);
    // The formatter is DETERMINISTIC (the same JMAP id always yields the same
    // EAS ServerId, so a client's stored id stays valid across calls) and
    // REVERSIBLE (every EAS operation maps the id back to the JMAP id).
    assert_eq!(first, email_server_id_from_jmap_id(jmap_id));
    assert!(is_email_server_id(&first));
    assert_eq!(jmap_id_from_email_server_id(&first), Some(jmap_id));
    assert_ne!(first, email_server_id_from_jmap_id("msg-other"));
}
