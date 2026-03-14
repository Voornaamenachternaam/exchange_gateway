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
use std::collections::HashMap;
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
        return if matches!(
            cmd.as_str(),
            "ping" | "sendmail" | "smartreply" | "smartforward"
        ) {
            Ok(())
        } else {
            Err("Empty request body")
        };
    }

    match cmd.as_str() {
        "sync" => {
            if !xml.contains("AirSync:") {
                return Err("Sync request missing AirSync namespace");
            }
            if extract_first_tag_text(xml, b"CollectionId").is_none() {
                return Err("Sync request missing CollectionId");
            }
            if extract_first_tag_text(xml, b"SyncKey").is_none() {
                return Err("Sync request missing SyncKey");
            }
            Ok(())
        }
        "provision" => {
            if !xml.contains("Provision:") {
                return Err("Provision request missing Provision namespace");
            }
            Ok(())
        }
        "settings" => {
            if !xml.contains("Settings:") {
                return Err("Settings request missing Settings namespace");
            }
            Ok(())
        }
        _ => Ok(()),
    }
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
                "Sync,SendMail,SmartForward,SmartReply,GetAttachment,FolderSync,FolderCreate,FolderDelete,FolderUpdate,MoveItems,GetItemEstimate,MeetingResponse,Search,Settings,Ping,ItemOperations,Provision,ResolveRecipients,ValidateCert",
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
    extra_inner: &str,
    request_id: &str,
) -> Response {
    let xml = format!(
        "<?xml version=\"1.0\" encoding=\"utf-8\"?><{root} xmlns=\"{ns}\"><Status>1</Status>{extra}</{root}>",
        root = root,
        ns = ns,
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

    let response = r#"<?xml version="1.0" encoding="utf-8"?><Provision xmlns="Provision:"><Status>2</Status></Provision>"#;
    xml_or_wbxml_response(wbxml, as_wbxml, response, request_id)
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
            let resp_xml = r#"<?xml version="1.0" encoding="utf-8"?>
<FolderSync xmlns="FolderHierarchy:"><Status>1</Status><SyncKey>1</SyncKey><Changes><Count>5</Count><Add><ServerId>1</ServerId><ParentId>0</ParentId><DisplayName>Calendar</DisplayName><Type>8</Type></Add><Add><ServerId>2</ServerId><ParentId>0</ParentId><DisplayName>Contacts</DisplayName><Type>9</Type></Add><Add><ServerId>3</ServerId><ParentId>0</ParentId><DisplayName>Tasks</DisplayName><Type>7</Type></Add><Add><ServerId>4</ServerId><ParentId>0</ParentId><DisplayName>Notes</DisplayName><Type>11</Type></Add><Add><ServerId>5</ServerId><ParentId>0</ParentId><DisplayName>Documents</DisplayName><Type>19</Type></Add></Changes></FolderSync>"#;
            xml_or_wbxml_response(&wbxml, wants_wbxml, resp_xml, &request_id)
        }
        "Provision" => {
            handle_provision(&state, &username, &req, &wbxml, wants_wbxml, &request_id).await
        }
        "Sync" => {
            let collection_id = req.collection_id.as_deref().unwrap_or("1");
            let incoming_key = req.sync_key.as_deref().unwrap_or("0");
            let class = req.class.as_deref().unwrap_or("Calendar");

            match sync::perform_sync(
                state,
                &username,
                collection_id,
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
        "Ping" => success_status_response(&wbxml, wants_wbxml, "Ping", "Ping:", "", &request_id),
        "Settings" => success_status_response(
            &wbxml,
            wants_wbxml,
            "Settings",
            "Settings:",
            "<Status>1</Status>",
            &request_id,
        ),
        "SendMail" => success_status_response(
            &wbxml,
            wants_wbxml,
            "SendMail",
            "ComposeMail:",
            "",
            &request_id,
        ),
        "SmartReply" | "SmartForward" => success_status_response(
            &wbxml,
            wants_wbxml,
            "Status",
            "ComposeMail:",
            "",
            &request_id,
        ),
        "ItemOperations" => success_status_response(
            &wbxml,
            wants_wbxml,
            "ItemOperations",
            "ItemOperations:",
            "<Responses></Responses>",
            &request_id,
        ),
        "Search" => success_status_response(
            &wbxml,
            wants_wbxml,
            "Search",
            "Search:",
            "<Response><Store><Status>1</Status><Result></Result></Store></Response>",
            &request_id,
        ),
        "MeetingResponse" => success_status_response(
            &wbxml,
            wants_wbxml,
            "MeetingResponse",
            "MeetingResponse:",
            "",
            &request_id,
        ),
        "ResolveRecipients" => success_status_response(
            &wbxml,
            wants_wbxml,
            "ResolveRecipients",
            "ResolveRecipients:",
            "",
            &request_id,
        ),
        "ValidateCert" => success_status_response(
            &wbxml,
            wants_wbxml,
            "ValidateCert",
            "ValidateCert:",
            "",
            &request_id,
        ),
        "GetItemEstimate" => success_status_response(
            &wbxml,
            wants_wbxml,
            "GetItemEstimate",
            "GetItemEstimate:",
            "",
            &request_id,
        ),
        "MoveItems" => {
            success_status_response(&wbxml, wants_wbxml, "MoveItems", "Move:", "", &request_id)
        }
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
}
