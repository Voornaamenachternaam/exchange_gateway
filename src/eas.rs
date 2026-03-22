use crate::models::AppState;
use crate::sync;
use crate::wbxml::Wbxml;
use axum::extract::{Query, State};
use axum::http::HeaderMap;
use axum::{
    body::Bytes,
    http::{Method, StatusCode, header},
    response::{IntoResponse, Response},
};
use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use lazy_static::lazy_static;
use quick_xml::Reader;
use quick_xml::events::Event;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use uuid::Uuid;

lazy_static! {
    static ref DEVICE_WINDOW: Mutex<HashMap<String, Vec<Instant>>> = Mutex::new(HashMap::new());
}

const MAX_REQUESTS_PER_WINDOW: usize = 60;
const WINDOW: Duration = Duration::from_secs(60);
const RETRY_AFTER_SECONDS: u64 = 30;

#[derive(Clone, Debug)]
struct EasRequest {
    command: String,
    sync_key: Option<String>,
    class: Option<String>,
    collection_id: Option<String>,
    device_id: Option<String>,
    policy_key: Option<String>,
}

#[derive(Clone, Copy)]
struct CommandGrammar {
    namespace: &'static str,
    required_tags: &'static [&'static str],
}

fn command_grammar(command: &str) -> Option<CommandGrammar> {
    let cmd = command.to_ascii_lowercase();
    match cmd.as_str() {
        "sync" => Some(CommandGrammar {
            namespace: "AirSync:",
            required_tags: &["Collections", "Collection", "CollectionId", "SyncKey"],
        }),
        "foldersync" => Some(CommandGrammar {
            namespace: "FolderHierarchy:",
            required_tags: &["SyncKey"],
        }),
        "provision" => Some(CommandGrammar {
            namespace: "Provision:",
            required_tags: &["Policies", "Policy"],
        }),
        "settings" => Some(CommandGrammar {
            namespace: "Settings:",
            required_tags: &["UserInformation"],
        }),
        "ping" => Some(CommandGrammar {
            namespace: "Ping:",
            required_tags: &["HeartbeatInterval", "Folders"],
        }),
        "itemoperations" => Some(CommandGrammar {
            namespace: "ItemOperations:",
            required_tags: &["Fetch"],
        }),
        "search" => Some(CommandGrammar {
            namespace: "Search:",
            required_tags: &["Store"],
        }),
        "meetingresponse" => Some(CommandGrammar {
            namespace: "MeetingResponse:",
            required_tags: &["RequestId", "UserResponse"],
        }),
        "resolverecipients" => Some(CommandGrammar {
            namespace: "ResolveRecipients:",
            required_tags: &["To"],
        }),
        "sendmail" | "smartreply" | "smartforward" => Some(CommandGrammar {
            namespace: "ComposeMail:",
            required_tags: &[],
        }),
        "validatecert" => Some(CommandGrammar {
            namespace: "ValidateCert:",
            required_tags: &["Certificates"],
        }),
        "getitemestimate" => Some(CommandGrammar {
            namespace: "GetItemEstimate:",
            required_tags: &["Collections", "Collection", "SyncKey", "CollectionId"],
        }),
        "moveitems" => Some(CommandGrammar {
            namespace: "Move:",
            required_tags: &["Move"],
        }),
        _ => None,
    }
}

fn parse_basic_auth(headers: &HeaderMap) -> Option<(String, String)> {
    if let Some(v) = headers.get(header::AUTHORIZATION)
        && let Ok(s) = v.to_str()
    {
        let s = s.trim();
        if s.to_ascii_lowercase().starts_with("basic ") {
            let b64 = &s[6..].trim();
            let mut out = Vec::new();
            if BASE64.decode_vec(b64.as_bytes(), &mut out).is_ok()
                && let Ok(creds) = String::from_utf8(out)
                && let Some(idx) = creds.find(':')
            {
                return Some((creds[..idx].to_string(), creds[idx + 1..].to_string()));
            }
        }
    }
    None
}

fn extract_root_command(xml: &str) -> Option<String> {
    if xml.trim().is_empty() {
        return None;
    }
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);
    let mut buf = Vec::new();
    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) | Ok(Event::Empty(e)) => {
                let name = String::from_utf8_lossy(e.name().local_name().as_ref()).to_string();
                return Some(name);
            }
            Ok(Event::Eof) | Err(_) => return None,
            _ => {}
        }
        buf.clear();
    }
}

fn extract_first_tag_text(xml: &str, tag: &[u8]) -> Option<String> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);
    let mut buf = Vec::new();
    let mut inside = false;
    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) if e.name().local_name().as_ref() == tag => inside = true,
            Ok(Event::Text(t)) if inside => return t.decode().ok().map(|v| v.into_owned()),
            Ok(Event::End(e)) if e.name().local_name().as_ref() == tag => inside = false,
            Ok(Event::Eof) | Err(_) => return None,
            _ => {}
        }
        buf.clear();
    }
}

fn command_from_query(query: &HashMap<String, String>) -> Option<String> {
    query
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("Cmd"))
        .map(|(_, v)| v.clone())
}

fn value_from_query(query: &HashMap<String, String>, key: &str) -> Option<String> {
    query
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case(key))
        .map(|(_, v)| v.clone())
}

fn validate_payload(command: &str, xml: &str) -> Result<(), &'static str> {
    let cmd = command.to_ascii_lowercase();

    if xml.is_empty() {
        return if matches!(cmd.as_str(), "sendmail" | "smartreply" | "smartforward") {
            Ok(())
        } else {
            Err("Empty request body")
        };
    }

    if let Some(grammar) = command_grammar(cmd.as_str()) {
        if !xml.contains(grammar.namespace) {
            return Err("Request missing expected command namespace");
        }

        for tag in grammar.required_tags {
            if !xml.contains(&format!("<{}", tag)) {
                return Err("Request missing required command element");
            }
        }
    }

    if cmd == "sync" {
        let class = extract_first_tag_text(xml, b"Class").unwrap_or_else(|| "Calendar".to_string());
        let supported = [
            "Calendar",
            "Contacts",
            "Email",
            "Notes",
            "Tasks",
            "DocumentLibrary",
            "SMS",
            "RightsManagement",
        ];
        if !supported.iter().any(|c| c.eq_ignore_ascii_case(&class)) {
            return Err("Unsupported Sync class");
        }
    }

    Ok(())
}

fn make_request_id() -> String {
    Uuid::new_v4().to_string()
}

fn inject_common_headers(resp: &mut Response, request_id: &str) {
    let headers = resp.headers_mut();
    headers.insert("MS-Server-ActiveSync", "16.1".parse().unwrap());
    headers.insert("X-MS-ProtocolVersion", "16.1".parse().unwrap());
    headers.insert("Cache-Control", "private, no-store".parse().unwrap());
    headers.insert("Pragma", "no-cache".parse().unwrap());
    headers.insert("X-Request-Id", request_id.parse().unwrap());
}

fn unauth_response(request_id: &str) -> Response {
    let mut r = (
        StatusCode::UNAUTHORIZED,
        [(
            header::WWW_AUTHENTICATE.as_str(),
            "Basic realm=\"Microsoft-Server-ActiveSync\"",
        )],
        "Unauthorized",
    )
        .into_response();
    inject_common_headers(&mut r, request_id);
    r
}

fn options_response(request_id: &str) -> Response {
    let mut r = (
        StatusCode::OK,
        [
            ("Allow", "OPTIONS,POST"),
            ("MS-ASProtocolVersions", "12.0,12.1,14.0,14.1,16.0,16.1"),
            (
                "MS-ASProtocolCommands",
                "Sync,FolderSync,Provision,MeetingResponse",
            ),
        ],
        "",
    )
        .into_response();
    inject_common_headers(&mut r, request_id);
    r
}

fn throttled_response(request_id: &str) -> Response {
    let mut r = (
        StatusCode::SERVICE_UNAVAILABLE,
        [(
            "Retry-After",
            Box::leak(RETRY_AFTER_SECONDS.to_string().into_boxed_str()),
        )],
        "Throttled",
    )
        .into_response();
    inject_common_headers(&mut r, request_id);
    r
}

fn bad_request_response(request_id: &str, msg: &str) -> Response {
    let mut r = (
        StatusCode::BAD_REQUEST,
        [(header::CONTENT_TYPE.as_str(), "application/xml; charset=utf-8")],
        format!(
            "<?xml version=\"1.0\" encoding=\"utf-8\"?><Status xmlns=\"AirSync:\">4</Status><!-- {} -->",
            msg
        ),
    )
        .into_response();
    inject_common_headers(&mut r, request_id);
    r
}

fn xml_or_wbxml_response(wbxml: &Wbxml, as_wbxml: bool, xml: &str, request_id: &str) -> Response {
    let mut r = if as_wbxml {
        match wbxml.encode(xml) {
            Ok(b) => (
                StatusCode::OK,
                [(
                    header::CONTENT_TYPE.as_str(),
                    "application/vnd.ms-sync.wbxml",
                )],
                b,
            )
                .into_response(),
            Err(e) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                [(header::CONTENT_TYPE.as_str(), "text/plain; charset=utf-8")],
                format!("WBXML Encode Err: {}", e),
            )
                .into_response(),
        }
    } else {
        (
            StatusCode::OK,
            [(
                header::CONTENT_TYPE.as_str(),
                "application/xml; charset=utf-8",
            )],
            xml.to_string(),
        )
            .into_response()
    };
    inject_common_headers(&mut r, request_id);
    r
}

fn unsupported_command_response(
    cmd: &str,
    wbxml: &Wbxml,
    as_wbxml: bool,
    request_id: &str,
) -> Response {
    let body = format!(
        "<?xml version=\"1.0\" encoding=\"utf-8\"?><Status xmlns=\"AirSync:\">5</Status><!-- Unsupported command: {} -->",
        cmd
    );
    xml_or_wbxml_response(wbxml, as_wbxml, &body, request_id)
}

fn success_status_response(
    wbxml: &Wbxml,
    as_wbxml: bool,
    root: &str,
    ns: &str,
    status: &str,
    extra_inner: &str,
    request_id: &str,
) -> Response {
    let xml = format!(
        "<?xml version=\"1.0\" encoding=\"utf-8\"?><{root} xmlns=\"{ns}\"><Status>{status}</Status>{extra}</{root}>",
        root = root,
        ns = ns,
        status = status,
        extra = extra_inner
    );
    xml_or_wbxml_response(wbxml, as_wbxml, &xml, request_id)
}

fn maybe_throttle(owner: &str, device_id: &str) -> bool {
    let key = format!("{}:{}", owner, device_id);
    let now = Instant::now();
    let mut map = match DEVICE_WINDOW.lock() {
        Ok(m) => m,
        Err(_) => return false,
    };
    let entries = map.entry(key).or_insert_with(Vec::new);
    entries.retain(|ts| now.duration_since(*ts) < WINDOW);
    if entries.len() >= MAX_REQUESTS_PER_WINDOW {
        return true;
    }
    entries.push(now);
    false
}

fn forwarded_https_enforced(headers: &HeaderMap) -> bool {
    match headers
        .get("x-forwarded-proto")
        .and_then(|v| v.to_str().ok())
        .map(|v| v.to_ascii_lowercase())
    {
        Some(v) => v == "https",
        None => true,
    }
}

fn parse_request(query: &HashMap<String, String>, xml: &str) -> EasRequest {
    EasRequest {
        command: extract_root_command(xml)
            .or_else(|| command_from_query(query))
            .unwrap_or_default(),
        sync_key: extract_first_tag_text(xml, b"SyncKey"),
        class: extract_first_tag_text(xml, b"Class"),
        collection_id: extract_first_tag_text(xml, b"CollectionId"),
        device_id: value_from_query(query, "DeviceId"),
        policy_key: extract_first_tag_text(xml, b"PolicyKey"),
    }
}

fn scoped_collection_id(visible_collection_id: &str, device_id: &str) -> String {
    format!("{visible_collection_id}::{device_id}")
}

async fn handle_provision(
    state: &Arc<AppState>,
    owner: &str,
    req: &EasRequest,
    wbxml: &Wbxml,
    as_wbxml: bool,
    request_id: &str,
) -> Response {
    let device_id = req
        .device_id
        .clone()
        .unwrap_or_else(|| "unknown-device".to_string());
    let incoming_key = req.policy_key.clone().unwrap_or_else(|| "0".to_string());

    if incoming_key == "0" {
        let server_policy_key = Uuid::new_v4().simple().to_string();
        if let Err(e) = state
            .storage
            .set_provision_policy(owner, &device_id, &server_policy_key, "pending")
            .await
        {
            tracing::warn!(
                "request_id={} failed storing provision policy: {}",
                request_id,
                e
            );
        }
        let response = format!(
            r#"<?xml version="1.0" encoding="utf-8"?>
<Provision xmlns="Provision:"><Status>1</Status><Policies><Policy><PolicyType>MS-EAS-Provisioning-WBXML</PolicyType><Status>1</Status><PolicyKey>{}</PolicyKey></Policy></Policies></Provision>"#,
            server_policy_key
        );
        return xml_or_wbxml_response(wbxml, as_wbxml, &response, request_id);
    }

    let valid = match state.storage.get_provision_policy(owner, &device_id).await {
        Ok(Some((stored, _))) => stored == incoming_key,
        Ok(None) => false,
        Err(_) => false,
    };

    if valid {
        let _ = state
            .storage
            .set_provision_policy(owner, &device_id, &incoming_key, "acknowledged")
            .await;
        let response = format!(
            r#"<?xml version="1.0" encoding="utf-8"?>
<Provision xmlns="Provision:"><Status>1</Status><Policies><Policy><PolicyType>MS-EAS-Provisioning-WBXML</PolicyType><Status>1</Status><PolicyKey>{}</PolicyKey></Policy></Policies></Provision>"#,
            incoming_key
        );
        return xml_or_wbxml_response(wbxml, as_wbxml, &response, request_id);
    }

    success_status_response(
        wbxml,
        as_wbxml,
        "Provision",
        "Provision:",
        "2",
        "",
        request_id,
    )
}

async fn handle_folder_sync(
    state: &Arc<AppState>,
    owner: &str,
    req: &EasRequest,
    wbxml: &Wbxml,
    as_wbxml: bool,
    request_id: &str,
) -> Response {
    let collection_id = scoped_collection_id(
        "folderhierarchy",
        req.device_id.as_deref().unwrap_or("unknown-device"),
    );
    let incoming = req.sync_key.as_deref().unwrap_or("0");
    let stored = state
        .storage
        .get_sync_key(owner, &collection_id)
        .await
        .ok()
        .flatten();

    if incoming != "0" {
        match stored.as_ref() {
            Some((expected, _)) if expected == incoming => {}
            _ => {
                let xml = r#"<?xml version="1.0" encoding="utf-8"?>
<FolderSync xmlns="FolderHierarchy:"><Status>9</Status><SyncKey>0</SyncKey></FolderSync>"#;
                return xml_or_wbxml_response(wbxml, as_wbxml, xml, request_id);
            }
        }
    }

    let new_sync_key = Uuid::new_v4().simple().to_string();
    let _ = state
        .storage
        .set_sync_key(
            owner,
            &collection_id,
            &new_sync_key,
            Some(&format!("ts:{}", chrono::Utc::now().timestamp())),
        )
        .await;

    let changes = if incoming == "0" {
        r#"<Changes><Count>1</Count><Add><ServerId>1</ServerId><ParentId>0</ParentId><DisplayName>Calendar</DisplayName><Type>8</Type></Add></Changes>"#
    } else {
        r#"<Changes><Count>0</Count></Changes>"#
    };
    let resp_xml = format!(
        r#"<?xml version="1.0" encoding="utf-8"?>
<FolderSync xmlns="FolderHierarchy:"><Status>1</Status><SyncKey>{}</SyncKey>{}</FolderSync>"#,
        new_sync_key, changes
    );
    xml_or_wbxml_response(wbxml, as_wbxml, &resp_xml, request_id)
}

async fn handle_get_item_estimate(
    state: &Arc<AppState>,
    owner: &str,
    req: &EasRequest,
    wbxml: &Wbxml,
    as_wbxml: bool,
    request_id: &str,
) -> Response {
    let visible_collection_id = req.collection_id.as_deref().unwrap_or("1");
    let collection_id = scoped_collection_id(
        visible_collection_id,
        req.device_id.as_deref().unwrap_or("unknown-device"),
    );
    let incoming = req.sync_key.as_deref().unwrap_or("0");
    let stored = state
        .storage
        .get_sync_key(owner, &collection_id)
        .await
        .ok()
        .flatten();

    if incoming != "0" {
        match stored.as_ref() {
            Some((expected, _)) if expected == incoming => {}
            _ => {
                let xml = format!(
                    r#"<?xml version="1.0" encoding="utf-8"?><GetItemEstimate xmlns="GetItemEstimate:"><Response><Status>{}</Status><Collection><CollectionId>{}</CollectionId><Estimate>0</Estimate></Collection></Response></GetItemEstimate>"#,
                    crate::sync::INVALID_SYNC_KEY_STATUS,
                    visible_collection_id
                );
                return xml_or_wbxml_response(wbxml, as_wbxml, &xml, request_id);
            }
        }
    }

    let since = if incoming == "0" {
        0
    } else {
        stored
            .as_ref()
            .and_then(|(_, token)| token.as_deref())
            .and_then(|token| token.strip_prefix("ts:"))
            .and_then(|v| v.parse::<i64>().ok())
            .unwrap_or(0)
    };
    let changed = state.storage.list_changes_since(owner, since).await.unwrap_or_default();
    let deleted = state.storage.list_deleted_since(owner, since).await.unwrap_or_default();
    let estimate = changed.len() + deleted.len();
    let xml = format!(
        r#"<?xml version="1.0" encoding="utf-8"?>
<GetItemEstimate xmlns="GetItemEstimate:"><Response><Status>1</Status><Collection><CollectionId>{}</CollectionId><Estimate>{}</Estimate></Collection></Response></GetItemEstimate>"#,
        visible_collection_id, estimate
    );
    xml_or_wbxml_response(wbxml, as_wbxml, &xml, request_id)
}

pub async fn handle(
    State(state): State<Arc<AppState>>,
    method: Method,
    Query(query): Query<HashMap<String, String>>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let request_id = make_request_id();

    if !forwarded_https_enforced(&headers) {
        return bad_request_response(&request_id, "x-forwarded-proto must be https");
    }

    if method == Method::OPTIONS {
        return options_response(&request_id);
    }

    let Some((username, password)) = parse_basic_auth(&headers) else {
        return unauth_response(&request_id);
    };

    let wbxml = Wbxml::new();
    let payload = body.to_vec();
    let wants_wbxml = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(|ct| ct.to_ascii_lowercase().contains("wbxml"))
        .unwrap_or(payload.first().is_some_and(|b| *b != b'<'));

    let xml = if payload.is_empty() {
        String::new()
    } else {
        match wbxml.decode(&payload) {
            Ok(s) => s,
            Err(e) => return bad_request_response(&request_id, &format!("Invalid body: {}", e)),
        }
    };

    let req = parse_request(&query, &xml);
    if req.command.is_empty() {
        return bad_request_response(&request_id, "Cannot determine EAS command");
    }

    if let Err(e) = validate_payload(&req.command, &xml) {
        return bad_request_response(&request_id, e);
    }

    let device_id = req
        .device_id
        .clone()
        .unwrap_or_else(|| "unknown-device".to_string());
    if maybe_throttle(&username, &device_id) {
        return throttled_response(&request_id);
    }

    match req.command.as_str() {
        "FolderSync" => {
            handle_folder_sync(&state, &username, &req, &wbxml, wants_wbxml, &request_id).await
        }
        "Provision" => {
            handle_provision(&state, &username, &req, &wbxml, wants_wbxml, &request_id).await
        }
        "Sync" => {
            let collection_id = req.collection_id.as_deref().unwrap_or("1");
            let state_collection_id =
                scoped_collection_id(collection_id, req.device_id.as_deref().unwrap_or("unknown-device"));
            let incoming_key = req.sync_key.as_deref().unwrap_or("0");
            let class = req.class.as_deref().unwrap_or("Calendar");

            if xml.contains("<Add")
                || xml.contains("<Change")
                || xml.contains("<Delete")
                || xml.contains(":Add")
                || xml.contains(":Change")
                || xml.contains(":Delete")
            {
                if let Err(e) = sync::apply_client_sync_mutations(
                    state.clone(),
                    &username,
                    &username,
                    &password,
                    &xml,
                )
                .await
                {
                    tracing::error!(
                        "request_id={} failed applying Sync mutations: {}",
                        request_id,
                        e
                    );
                    let err_xml = r#"<?xml version="1.0" encoding="utf-8"?><Sync xmlns="AirSync:"><Status>6</Status></Sync>"#;
                    return xml_or_wbxml_response(&wbxml, wants_wbxml, err_xml, &request_id);
                }
            }

            match sync::perform_sync(
                state,
                    &username,
                    collection_id,
                    &state_collection_id,
                    incoming_key,
                    class,
                100,
                &username,
                &password,
            )
            .await
            {
                Ok(resp_xml) => xml_or_wbxml_response(&wbxml, wants_wbxml, &resp_xml, &request_id),
                Err(e) => {
                    tracing::error!("request_id={} Sync Error: {}", request_id, e);
                    let err_xml = r#"<?xml version="1.0" encoding="utf-8"?><Sync xmlns="AirSync:"><Status>6</Status></Sync>"#;
                    xml_or_wbxml_response(&wbxml, wants_wbxml, err_xml, &request_id)
                }
            }
        }
        "Ping" => {
            success_status_response(&wbxml, wants_wbxml, "Ping", "Ping:", "1", "", &request_id)
        }
        "Settings" => success_status_response(
            &wbxml,
            wants_wbxml,
            "Settings",
            "Settings:",
            "1",
            "",
            &request_id,
        ),
        "SendMail" => success_status_response(
            &wbxml,
            wants_wbxml,
            "SendMail",
            "ComposeMail:",
            "1",
            "",
            &request_id,
        ),
        "SmartReply" | "SmartForward" => success_status_response(
            &wbxml,
            wants_wbxml,
            "Status",
            "ComposeMail:",
            "1",
            "",
            &request_id,
        ),
        "ItemOperations" => success_status_response(
            &wbxml,
            wants_wbxml,
            "ItemOperations",
            "ItemOperations:",
            "1",
            "<Responses></Responses>",
            &request_id,
        ),
        "Search" => success_status_response(
            &wbxml,
            wants_wbxml,
            "Search",
            "Search:",
            "1",
            "<Response><Store><Status>1</Status><Result></Result></Store></Response>",
            &request_id,
        ),
        "MeetingResponse" => {
            if let Some(req_id) = extract_first_tag_text(&xml, b"RequestId") {
                let user_response = extract_first_tag_text(&xml, b"UserResponse")
                    .and_then(|v| v.parse::<u8>().ok())
                    .unwrap_or(0);
                if let Err(e) = sync::apply_meeting_response(
                    state.clone(),
                    &username,
                    &username,
                    &password,
                    &req_id,
                    user_response,
                )
                .await
                {
                    tracing::error!(
                        "request_id={} failed applying MeetingResponse: {}",
                        request_id,
                        e
                    );
                    let err_xml = r#"<?xml version="1.0" encoding="utf-8"?><MeetingResponse xmlns="MeetingResponse:"><Result><Status>6</Status></Result></MeetingResponse>"#;
                    xml_or_wbxml_response(&wbxml, wants_wbxml, err_xml, &request_id)
                } else {
                    let payload = format!(
                        r#"<?xml version="1.0" encoding="utf-8"?><MeetingResponse xmlns="MeetingResponse:"><Result><RequestId>{}</RequestId><Status>1</Status></Result></MeetingResponse>"#,
                        req_id
                    );
                    xml_or_wbxml_response(&wbxml, wants_wbxml, &payload, &request_id)
                }
            } else {
                bad_request_response(&request_id, "MeetingResponse requires RequestId")
            }
        }
        "ResolveRecipients" => success_status_response(
            &wbxml,
            wants_wbxml,
            "ResolveRecipients",
            "ResolveRecipients:",
            "1",
            "",
            &request_id,
        ),
        "ValidateCert" => success_status_response(
            &wbxml,
            wants_wbxml,
            "ValidateCert",
            "ValidateCert:",
            "1",
            "",
            &request_id,
        ),
        "GetItemEstimate" => {
            handle_get_item_estimate(&state, &username, &req, &wbxml, wants_wbxml, &request_id)
                .await
        }
        "MoveItems" => success_status_response(
            &wbxml,
            wants_wbxml,
            "MoveItems",
            "Move:",
            "1",
            "",
            &request_id,
        ),
        _ => unsupported_command_response(&req.command, &wbxml, wants_wbxml, &request_id),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        command_from_query, extract_first_tag_text, extract_root_command, validate_payload,
    };
    use std::collections::HashMap;

    #[test]
    fn parses_root_command() {
        let xml = r#"<?xml version=\"1.0\"?><Sync xmlns=\"AirSync:\"></Sync>"#;
        assert_eq!(extract_root_command(xml).as_deref(), Some("Sync"));
    }

    #[test]
    fn parses_sync_key() {
        let xml = r#"<Sync xmlns=\"AirSync:\"><Collections><Collection><SyncKey>123</SyncKey></Collection></Collections></Sync>"#;
        assert_eq!(
            extract_first_tag_text(xml, b"SyncKey").as_deref(),
            Some("123")
        );
    }

    #[test]
    fn extracts_sync_class() {
        let xml = r#"<Sync xmlns=\"AirSync:\"><Collections><Collection><Class>Contacts</Class></Collection></Collections></Sync>"#;
        assert_eq!(
            extract_first_tag_text(xml, b"Class").as_deref(),
            Some("Contacts")
        );
    }

    #[test]
    fn command_from_query_case_insensitive() {
        let mut q = HashMap::new();
        q.insert("cmd".to_string(), "Ping".to_string());
        assert_eq!(command_from_query(&q).as_deref(), Some("Ping"));
    }

    #[test]
    fn validates_sync_namespace() {
        let xml = r#"<Sync><CollectionId>1</CollectionId><SyncKey>1</SyncKey></Sync>"#;
        assert!(validate_payload("Sync", xml).is_err());
    }

    #[test]
    fn validates_sync_class() {
        let xml = r#"<Sync xmlns=\"AirSync:\"><Collections><Collection><CollectionId>1</CollectionId><SyncKey>1</SyncKey><Class>InvalidClass</Class></Collection></Collections></Sync>"#;
        assert!(validate_payload("Sync", xml).is_err());
    }

    #[test]
    fn validates_ping_required_tags() {
        let xml = r#"<Ping xmlns=\"Ping:\"><Folders /></Ping>"#;
        assert!(validate_payload("Ping", xml).is_err());
    }

    #[test]
    fn validates_getitemestimate_required_tags() {
        let xml = r#"<GetItemEstimate xmlns=\"GetItemEstimate:\"></GetItemEstimate>"#;
        assert!(validate_payload("GetItemEstimate", xml).is_err());
    }

    #[test]
    fn command_grammar_matrix_positive_cases() {
        let cases = [
            (
                "Sync",
                r#"<Sync xmlns="AirSync:"><Collections><Collection><CollectionId>1</CollectionId><SyncKey>1</SyncKey><Class>Calendar</Class></Collection></Collections></Sync>"#,
            ),
            (
                "FolderSync",
                r#"<FolderSync xmlns="FolderHierarchy:"><SyncKey>1</SyncKey></FolderSync>"#,
            ),
            (
                "Provision",
                r#"<Provision xmlns="Provision:"><Policies><Policy><PolicyType>MS-EAS-Provisioning-WBXML</PolicyType></Policy></Policies></Provision>"#,
            ),
            (
                "Settings",
                r#"<Settings xmlns="Settings:"><UserInformation><Get/></UserInformation></Settings>"#,
            ),
            (
                "ItemOperations",
                r#"<ItemOperations xmlns="ItemOperations:"><Fetch/></ItemOperations>"#,
            ),
            (
                "Search",
                r#"<Search xmlns="Search:"><Name>Mailbox</Name><Query>x</Query></Search>"#,
            ),
            (
                "MeetingResponse",
                r#"<MeetingResponse xmlns="MeetingResponse:"><Request><RequestId>abc</RequestId><UserResponse>1</UserResponse></Request></MeetingResponse>"#,
            ),
            (
                "ResolveRecipients",
                r#"<ResolveRecipients xmlns="ResolveRecipients:"><To>a@example.com</To></ResolveRecipients>"#,
            ),
            (
                "ValidateCert",
                r#"<ValidateCert xmlns="ValidateCert:"><Certificates/></ValidateCert>"#,
            ),
            (
                "GetItemEstimate",
                r#"<GetItemEstimate xmlns="GetItemEstimate:"><Collections><Collection><CollectionId>1</CollectionId><SyncKey>1</SyncKey></Collection></Collections></GetItemEstimate>"#,
            ),
            (
                "MoveItems",
                r#"<MoveItems xmlns="Move:"><Move><SrcMsgId>1</SrcMsgId><SrcFldId>2</SrcFldId><DstFldId>3</DstFldId></Move></MoveItems>"#,
            ),
            (
                "Ping",
                r#"<Ping xmlns="Ping:"><HeartbeatInterval>60</HeartbeatInterval><Folders/></Ping>"#,
            ),
        ];

        for (cmd, xml) in cases {
            assert!(
                validate_payload(cmd, xml).is_ok(),
                "{} should validate",
                cmd
            );
        }
    }

    #[test]
    fn command_grammar_matrix_negative_namespace() {
        let cases = [
            (
                "FolderSync",
                r#"<FolderSync xmlns="AirSync:"><SyncKey>1</SyncKey></FolderSync>"#,
            ),
            (
                "Provision",
                r#"<Provision xmlns="AirSync:"><Policies/></Provision>"#,
            ),
            (
                "Settings",
                r#"<Settings xmlns="AirSync:"><UserInformation/></Settings>"#,
            ),
            (
                "MoveItems",
                r#"<MoveItems xmlns="ItemOperations:"><Move/></MoveItems>"#,
            ),
        ];

        for (cmd, xml) in cases {
            assert!(
                validate_payload(cmd, xml).is_err(),
                "{} wrong namespace should fail",
                cmd
            );
        }
    }
}
