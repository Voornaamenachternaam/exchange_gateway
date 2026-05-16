// src/eas.rs
use crate::caldav::CaldavClient;
use crate::calendar::{parse_datetime, parse_ics_event};
use crate::models::AppState;
use crate::permission::{PermissionContext, PermissionEnforcement};
use crate::sync::{self, SyncOptions, filter_type_to_start};
use crate::util::{nfc, normalize_username, xml_escape};
use crate::wbxml::Wbxml;
use axum::extract::{Query, State};
use axum::http::{HeaderMap, HeaderValue};
use axum::{
    body::Bytes,
    http::{Method, StatusCode, header},
    response::{IntoResponse, Response},
};
use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use futures_util::future::join_all;
use lru::LruCache;
use quick_xml::Reader;
use quick_xml::events::Event;
use secrecy::{ExposeSecret, SecretString};
use std::collections::HashMap;
use std::num::NonZeroUsize;
use std::sync::{Arc, LazyLock};
use std::time::{Duration, Instant};
use subtle::ConstantTimeEq;
use tokio::sync::Mutex as TokioMutex;
use tokio::time::timeout;
use uuid::Uuid;

const MAX_REQUESTS_PER_WINDOW: usize = 60;
const WINDOW: Duration = Duration::from_secs(60);
const RETRY_AFTER_SECONDS: u64 = 30;
const MAX_FREEBUSY_DAYS: i64 = 30;
const MAX_BODY_SIZE: usize = 1_048_576;
const CALDAV_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_PING_CACHE_ENTRIES: usize = 10_000;
const MAX_DEVICE_WINDOW_ENTRIES: usize = 100_000;

type DeviceWindowCache = LruCache<String, Vec<Instant>>;
type PingCache = LruCache<String, PingCacheEntry>;

const _: () = assert!(MAX_DEVICE_WINDOW_ENTRIES > 0);
const _: () = assert!(MAX_PING_CACHE_ENTRIES > 0);

static DEVICE_WINDOW: LazyLock<TokioMutex<DeviceWindowCache>> = LazyLock::new(|| {
    TokioMutex::new(LruCache::new(
        NonZeroUsize::new(MAX_DEVICE_WINDOW_ENTRIES).expect("MAX_DEVICE_WINDOW_ENTRIES > 0"),
    ))
});
static PING_CACHE: LazyLock<TokioMutex<PingCache>> = LazyLock::new(|| {
    TokioMutex::new(LruCache::new(
        NonZeroUsize::new(MAX_PING_CACHE_ENTRIES).expect("MAX_PING_CACHE_ENTRIES > 0"),
    ))
});

#[derive(Clone, Debug)]
struct PingFolder {
    id: String,
    class_name: String,
}

#[derive(Clone, Debug)]
struct PingCacheEntry {
    heartbeat: u64,
    folders: Vec<PingFolder>,
}

#[derive(Clone, Debug, Default)]
struct ItemOperationsFetch {
    store: String,
    collection_id: Option<String>,
    server_id: Option<String>,
    long_id: Option<String>,
    file_reference: Option<String>,
}

#[derive(Clone, Debug, Default)]
struct SearchRequest {
    store_name: String,
    query_text: Option<String>,
    range_start: usize,
    range_end: usize,
    starts: Option<chrono::DateTime<chrono::Utc>>,
    ends: Option<chrono::DateTime<chrono::Utc>>,
}

#[derive(Clone, Debug, Default)]
struct EasRequest {
    command: String,
    sync_key: Option<String>,
    class: Option<String>,
    collection_id: Option<String>,
    device_id: Option<String>,
    policy_key: Option<String>,
    _protocol_version: Option<String>,
    window_size: Option<usize>,
    get_changes: bool,
    filter_type: Option<u8>,
}

#[derive(Clone, Copy)]
struct CommandGrammar {
    namespace: &'static str,
    required_tags: &'static [&'static str],
    _optional_tags: &'static [&'static str],
}

fn command_grammar(command: &str) -> Option<CommandGrammar> {
    match command.to_ascii_lowercase().as_str() {
        "sync" => Some(CommandGrammar {
            namespace: "AirSync:",
            required_tags: &["Collections", "Collection", "SyncKey"],
            _optional_tags: &[
                "CollectionId",
                "Class",
                "Options",
                "Supported",
                "Commands",
                "WindowSize",
                "FilterType",
                "DeletesAsMoves",
                "GetChanges",
                "MoreAvailable",
                "Partial",
                "ConversationMode",
                "MIMESupport",
                "MIMETruncation",
                "MaxItems",
                "BodyPreference",
            ],
        }),
        "foldersync" => Some(CommandGrammar {
            namespace: "FolderHierarchy:",
            required_tags: &["SyncKey"],
            _optional_tags: &[],
        }),
        "provision" => Some(CommandGrammar {
            namespace: "Provision:",
            required_tags: &[],
            _optional_tags: &[
                "Policies",
                "Policy",
                "PolicyType",
                "PolicyKey",
                "Status",
                "Data",
            ],
        }),
        "settings" => Some(CommandGrammar {
            namespace: "Settings:",
            required_tags: &[],
            _optional_tags: &[
                "UserInformation",
                "Oof",
                "DevicePassword",
                "DeviceInformation",
            ],
        }),
        "ping" => Some(CommandGrammar {
            namespace: "Ping:",
            required_tags: &[],
            _optional_tags: &[
                "HeartbeatInterval",
                "Folders",
                "Folder",
                "Id",
                "Class",
                "MaxFolders",
            ],
        }),
        "itemoperations" => Some(CommandGrammar {
            namespace: "ItemOperations:",
            required_tags: &[],
            _optional_tags: &[
                "Fetch",
                "Store",
                "CollectionId",
                "ServerId",
                "LongId",
                "Options",
            ],
        }),
        "search" => Some(CommandGrammar {
            namespace: "Search:",
            required_tags: &[],
            _optional_tags: &["Store", "Name", "Query", "Options", "Range"],
        }),
        "meetingresponse" => Some(CommandGrammar {
            namespace: "MeetingResponse:",
            required_tags: &[],
            _optional_tags: &["RequestId", "UserResponse", "InstanceId", "SendResponse"],
        }),
        "resolverecipients" => Some(CommandGrammar {
            namespace: "ResolveRecipients:",
            required_tags: &[],
            _optional_tags: &["To", "Options", "MaxCertificates", "MaxAmbiguousRecipients"],
        }),
        "validatecert" => Some(CommandGrammar {
            namespace: "ValidateCert:",
            required_tags: &[],
            _optional_tags: &["Certificates", "Certificate", "CertChain"],
        }),
        "getitemestimate" => Some(CommandGrammar {
            namespace: "GetItemEstimate:",
            required_tags: &[],
            _optional_tags: &[
                "Collections",
                "Collection",
                "SyncKey",
                "CollectionId",
                "Class",
                "Options",
            ],
        }),
        "moveitems" => Some(CommandGrammar {
            namespace: "Move:",
            required_tags: &[],
            _optional_tags: &["Move", "SrcMsgId", "SrcFldId", "DstFldId"],
        }),
        _ => None,
    }
}

fn validate_payload(command: &str, xml: &str) -> Result<(), &'static str> {
    let lower_cmd = command.to_ascii_lowercase();
    let grammar = command_grammar(&lower_cmd).ok_or("Unsupported command")?;
    if xml.trim().is_empty() {
        return Err("Empty request body");
    }

    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);
    let mut buf = Vec::new();
    let (root_name, root_ns) = loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) | Ok(Event::Empty(e)) => {
                let name = String::from_utf8_lossy(e.name().local_name().as_ref()).to_string();
                let qname = e.name();
                let qname_bytes = qname.as_ref();

                let ns = if let Some(colon_pos) = qname_bytes.iter().position(|&b| b == b':') {
                    let prefix = &qname_bytes[..colon_pos];
                    e.attributes()
                        .flatten()
                        .find_map(|attr| {
                            let key = attr.key.as_ref();
                            if key.starts_with(b"xmlns:") && &key[6..] == prefix {
                                Some(String::from_utf8_lossy(attr.value.as_ref()).to_string())
                            } else {
                                None
                            }
                        })
                        .unwrap_or_default()
                } else {
                    e.attributes()
                        .flatten()
                        .find_map(|attr| {
                            if attr.key.as_ref() == b"xmlns" {
                                Some(String::from_utf8_lossy(attr.value.as_ref()).to_string())
                            } else {
                                None
                            }
                        })
                        .unwrap_or_default()
                };
                break (name, ns);
            }
            Ok(Event::Eof) | Err(_) => return Err("Missing root element"),
            _ => {}
        }
        buf.clear();
    };

    if root_name.to_ascii_lowercase() != lower_cmd {
        return Err("Root element does not match command");
    }
    if !root_ns.is_empty() && !root_ns.contains(grammar.namespace) {
        return Err("Invalid namespace");
    }

    for &required in grammar.required_tags {
        if extract_first_tag_text(xml, required.as_bytes()).is_none() {
            return Err("Missing required tag");
        }
    }

    match lower_cmd.as_str() {
        "sync" => {
            if extract_first_tag_text(xml, b"Class")
                .is_some_and(|class| !class.eq_ignore_ascii_case("Calendar"))
            {
                return Err("Only Calendar Sync class is supported");
            }
            if xml.contains("<Add>") && !xml.contains("<ClientId>") {
                return Err("Add requires ClientId");
            }
        }
        "meetingresponse" if extract_first_tag_text(xml, b"UserResponse").is_none() => {
            return Err("MeetingResponse requires UserResponse");
        }
        _ => {}
    }
    Ok(())
}

fn parse_basic_auth(headers: &HeaderMap) -> Option<(String, SecretString)> {
    let auth = headers.get(header::AUTHORIZATION)?.to_str().ok()?;
    let auth = auth.trim();
    if !auth.to_ascii_lowercase().starts_with("basic ") {
        return None;
    }
    let b64 = &auth[6..].trim();
    let mut decoded = zeroize::Zeroizing::new(Vec::new());
    BASE64.decode_vec(b64.as_bytes(), decoded.as_mut()).ok()?;
    let creds = zeroize::Zeroizing::new(String::from_utf8(decoded.to_vec()).ok()?);
    let idx = creds.find(':')?;
    let raw_user = creds[..idx].to_string();
    // Strip domain prefix like "EXAMPLE\user" → "user"
    let user = normalize_username(&raw_user).to_string();
    let pass = SecretString::from(creds[idx + 1..].to_string());
    Some((user, pass))
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
                return Some(String::from_utf8_lossy(e.name().local_name().as_ref()).to_string());
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

fn extract_all_tag_text(xml: &str, tag: &[u8]) -> Vec<String> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);
    let mut buf = Vec::new();
    let mut inside = false;
    let mut values = Vec::new();
    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) if e.name().local_name().as_ref() == tag => inside = true,
            Ok(Event::Text(t)) if inside => {
                if let Ok(v) = t.decode() {
                    values.push(v.into_owned());
                }
            }
            Ok(Event::End(e)) if e.name().local_name().as_ref() == tag => inside = false,
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
        buf.clear();
    }
    values
}

fn value_from_query(query: &HashMap<String, String>, key: &str) -> Option<String> {
    query
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case(key))
        .map(|(_, v)| v.clone())
}

fn command_from_query(query: &HashMap<String, String>) -> Option<String> {
    query
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("Cmd"))
        .map(|(_, v)| v.clone())
}

fn parse_ping_folders(xml: &str) -> Vec<PingFolder> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);
    let mut buf = Vec::new();
    let mut in_folder = false;
    let mut current_id: Option<String> = None;
    let mut current_class: Option<String> = None;
    let mut folders = Vec::new();
    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) if e.name().local_name().as_ref() == b"Folder" => {
                in_folder = true;
                current_id = None;
                current_class = None;
            }
            Ok(Event::Start(e)) if in_folder && e.name().local_name().as_ref() == b"Id" => {
                if let Ok(Event::Text(t)) = reader.read_event_into(&mut buf) {
                    current_id = t.decode().ok().map(|v| v.into_owned());
                }
            }
            Ok(Event::Start(e)) if in_folder && e.name().local_name().as_ref() == b"Class" => {
                if let Ok(Event::Text(t)) = reader.read_event_into(&mut buf) {
                    current_class = t.decode().ok().map(|v| v.into_owned());
                }
            }
            Ok(Event::End(e)) if e.name().local_name().as_ref() == b"Folder" => {
                if let (Some(id), Some(class_name)) = (current_id.take(), current_class.take()) {
                    folders.push(PingFolder { id, class_name });
                }
                in_folder = false;
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
        buf.clear();
    }
    folders
}

fn parse_item_operations_fetches(xml: &str) -> Vec<ItemOperationsFetch> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);
    let mut buf = Vec::new();
    let mut fetches = Vec::new();
    let mut current: Option<ItemOperationsFetch> = None;
    let mut current_tag: Option<Vec<u8>> = None;
    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) if e.name().local_name().as_ref() == b"Fetch" => {
                current = Some(ItemOperationsFetch::default());
                current_tag = None;
            }
            Ok(Event::Start(e)) if current.is_some() => {
                current_tag = Some(e.name().local_name().as_ref().to_vec());
            }
            Ok(Event::Text(t)) if current.is_some() => {
                let text = t.decode().ok().map(|v| v.into_owned()).unwrap_or_default();
                if let Some(fetch) = current.as_mut() {
                    match current_tag.as_deref() {
                        Some(b"Store") => fetch.store = text,
                        Some(b"CollectionId") => fetch.collection_id = Some(text),
                        Some(b"ServerId") => fetch.server_id = Some(text),
                        Some(b"LongId") => fetch.long_id = Some(text),
                        _ => {}
                    }
                }
            }
            Ok(Event::End(e)) if e.name().local_name().as_ref() == b"Fetch" => {
                if let Some(fetch) = current.take() {
                    fetches.push(fetch);
                }
                current_tag = None;
            }
            Ok(Event::End(_)) if current.is_some() => current_tag = None,
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
        buf.clear();
    }
    fetches
}

fn parse_search_request(xml: &str) -> SearchRequest {
    let range = extract_first_tag_text(xml, b"Range").unwrap_or_else(|| "0-9".to_string());
    let (range_start, range_end) = range
        .split_once('-')
        .and_then(|(s, e)| Some((s.trim().parse().ok()?, e.trim().parse().ok()?)))
        .unwrap_or((0, 9));
    SearchRequest {
        store_name: extract_first_tag_text(xml, b"Name").unwrap_or_else(|| "Mailbox".to_string()),
        query_text: extract_first_tag_text(xml, b"Query").map(|v| v.trim().to_string()),
        range_start,
        range_end,
        starts: extract_first_tag_text(xml, b"Starts")
            .as_deref()
            .and_then(parse_datetime),
        ends: extract_first_tag_text(xml, b"Ends")
            .as_deref()
            .and_then(parse_datetime),
    }
}

fn active_user_emails(username: &str) -> Vec<String> {
    let mut emails = vec![username.to_string()];
    if !username.contains('@') {
        emails.push(format!("{username}@example.com"));
    }
    emails
}

fn matches_search(item: &crate::calendar::CalendarItem, query: Option<&str>) -> bool {
    let Some(query) = query.map(str::trim).filter(|v| !v.is_empty()) else {
        return true;
    };
    let q = query.to_ascii_lowercase();
    [
        item.subject.as_str(),
        item.description.as_str(),
        item.location.as_str(),
        item.uid.as_str(),
        item.organizer_name.as_deref().unwrap_or_default(),
        item.organizer_email.as_deref().unwrap_or_default(),
    ]
    .iter()
    .any(|v| v.to_ascii_lowercase().contains(&q))
        || item
            .attendees
            .iter()
            .any(|a| nfc(&a.email).to_ascii_lowercase().contains(&q))
}

type DeviceInfo = (
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
);

fn make_request_id() -> String {
    Uuid::new_v4().to_string()
}

fn parse_device_information(xml: &str) -> DeviceInfo {
    (
        extract_first_tag_text(xml, b"FriendlyName"),
        extract_first_tag_text(xml, b"Model"),
        extract_first_tag_text(xml, b"OS"),
        extract_first_tag_text(xml, b"PhoneNumber"),
        extract_first_tag_text(xml, b"IMEI"),
        extract_first_tag_text(xml, b"UserAgent"),
    )
}

fn parse_request(query: &HashMap<String, String>, xml: &str, headers: &HeaderMap) -> EasRequest {
    let window_size = extract_first_tag_text(xml, b"WindowSize")
        .and_then(|v| v.parse::<usize>().ok())
        .map(|v| {
            if v == 0 {
                100
            } else if v > 512 {
                512
            } else {
                v
            }
        });
    let get_changes = extract_first_tag_text(xml, b"GetChanges")
        .map(|v| v.trim() != "0")
        .unwrap_or(true);
    let filter_type = extract_first_tag_text(xml, b"FilterType").and_then(|v| v.parse::<u8>().ok());

    let protocol_version = headers
        .get("MS-ASProtocolVersion")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string())
        .or_else(|| value_from_query(query, "ProtVer"));

    let policy_key = headers
        .get("X-MS-PolicyKey")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string())
        .or_else(|| extract_first_tag_text(xml, b"PolicyKey"));

    EasRequest {
        command: extract_root_command(xml)
            .or_else(|| command_from_query(query))
            .unwrap_or_default(),
        sync_key: extract_first_tag_text(xml, b"SyncKey"),
        class: extract_first_tag_text(xml, b"Class"),
        collection_id: extract_first_tag_text(xml, b"CollectionId"),
        device_id: value_from_query(query, "DeviceId"),
        policy_key,
        _protocol_version: protocol_version,
        window_size,
        get_changes,
        filter_type,
    }
}

fn scoped_collection_id(visible_collection_id: &str, device_id: &str) -> String {
    format!("{visible_collection_id}::{device_id}")
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

async fn maybe_throttle(owner: &str, device_id: &str) -> bool {
    let key = format!("{}:{}", owner, device_id);
    let now = Instant::now();
    let mut cache = DEVICE_WINDOW.lock().await;
    let entries = cache.get_or_insert_mut(key, Vec::new);
    entries.retain(|ts| now.checked_duration_since(*ts).is_some_and(|d| d < WINDOW));
    if entries.len() >= MAX_REQUESTS_PER_WINDOW {
        return true;
    }
    entries.push(now);
    false
}

fn inject_common_headers(resp: &mut Response, request_id: &str) {
    let h = resp.headers_mut();
    h.insert("MS-Server-ActiveSync", HeaderValue::from_static("16.1"));
    h.insert("X-MS-ProtocolVersion", HeaderValue::from_static("16.1"));
    h.insert(
        "Cache-Control",
        HeaderValue::from_static("private, no-store"),
    );
    h.insert("Pragma", HeaderValue::from_static("no-cache"));
    h.insert(
        "Strict-Transport-Security",
        HeaderValue::from_static("max-age=63072000; includeSubDomains"),
    );
    h.insert(
        "X-Request-Id",
        HeaderValue::from_str(request_id).unwrap_or_else(|e| {
            tracing::warn!("Invalid X-Request-Id value '{}': {}", request_id, e);
            HeaderValue::from_static("unknown")
        }),
    );
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
            (
                "MS-ASProtocolVersions",
                "12.0,12.1,14.0,14.1,16.0,16.1",
            ),
            (
                "MS-ASProtocolCommands",
                "Sync,FolderSync,Provision,MeetingResponse,Settings,Ping,ItemOperations,Search,ResolveRecipients,GetItemEstimate,ValidateCert",
            ),
        ],
        "",
    )
        .into_response();
    inject_common_headers(&mut r, request_id);
    r
}

fn throttled_response(request_id: &str) -> Response {
    let mut r = (StatusCode::SERVICE_UNAVAILABLE, "Throttled").into_response();
    r.headers_mut().insert(
        header::RETRY_AFTER,
        HeaderValue::from_str(&RETRY_AFTER_SECONDS.to_string())
            .expect("RETRY_AFTER_SECONDS must be a valid Retry-After value"),
    );
    inject_common_headers(&mut r, request_id);
    r
}

fn bad_request_response(request_id: &str, msg: &str) -> Response {
    let mut r = (
        StatusCode::BAD_REQUEST,
        [(
            header::CONTENT_TYPE.as_str(),
            "application/xml; charset=utf-8",
        )],
        format!(
            r#"<?xml version="1.0" encoding="utf-8"?><Status xmlns="AirSync:">4</Status><!-- {} -->"#,
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
                format!("WBXML Encode Err: {}", e).into_bytes(),
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
            xml.to_string().into_bytes(),
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
        r#"<?xml version="1.0" encoding="utf-8"?><Status xmlns="AirSync:">5</Status><!-- Unsupported command: {} -->"#,
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
        r#"<?xml version="1.0" encoding="utf-8"?><{root} xmlns="{ns}"><Status>{status}</Status>{extra_inner}</{root}>"#,
    );
    xml_or_wbxml_response(wbxml, as_wbxml, &xml, request_id)
}

fn eas_provision_doc_xml() -> &'static str {
    r#"<EASProvisionDoc>
<DevicePasswordEnabled>0</DevicePasswordEnabled>
<AlphanumericDevicePasswordRequired>0</AlphanumericDevicePasswordRequired>
<PasswordRecoveryEnabled>0</PasswordRecoveryEnabled>
<RequireStorageCardEncryption>0</RequireStorageCardEncryption>
<AttachmentsEnabled>1</AttachmentsEnabled>
<MinDevicePasswordLength/>
<MaxInactivityTimeDeviceLock>9999</MaxInactivityTimeDeviceLock>
<MaxDevicePasswordFailedAttempts>8</MaxDevicePasswordFailedAttempts>
<MaxAttachmentSize/>
<AllowSimpleDevicePassword>1</AllowSimpleDevicePassword>
<DevicePasswordExpiration/>
<DevicePasswordHistory>0</DevicePasswordHistory>
<AllowStorageCard>1</AllowStorageCard>
<AllowCamera>1</AllowCamera>
<RequireDeviceEncryption>0</RequireDeviceEncryption>
<AllowUnsignedApplications>1</AllowUnsignedApplications>
<AllowUnsignedInstallationPackages>1</AllowUnsignedInstallationPackages>
<MinDevicePasswordComplexCharacters>1</MinDevicePasswordComplexCharacters>
<AllowWifi>1</AllowWifi>
<AllowTextMessaging>1</AllowTextMessaging>
<AllowPOPIMAPEmail>1</AllowPOPIMAPEmail>
<AllowBluetooth>2</AllowBluetooth>
<AllowIrDA>1</AllowIrDA>
<RequireManualSyncWhenRoaming>0</RequireManualSyncWhenRoaming>
<AllowDesktopSync>1</AllowDesktopSync>
<MaxCalendarAgeFilter>0</MaxCalendarAgeFilter>
<AllowHTMLEmail>1</AllowHTMLEmail>
<MaxEmailAgeFilter>0</MaxEmailAgeFilter>
<MaxEmailBodyTruncationSize>-1</MaxEmailBodyTruncationSize>
<MaxEmailHTMLBodyTruncationSize>-1</MaxEmailHTMLBodyTruncationSize>
<RequireSignedSMIMEMessages>0</RequireSignedSMIMEMessages>
<RequireEncryptedSMIMEMessages>0</RequireEncryptedSMIMEMessages>
<RequireSignedSMIMEAlgorithm>0</RequireSignedSMIMEAlgorithm>
<RequireEncryptionSMIMEAlgorithm>0</RequireEncryptionSMIMEAlgorithm>
<AllowSMIMEEncryptionAlgorithmNegotiation>2</AllowSMIMEEncryptionAlgorithmNegotiation>
<AllowSMIMESoftCerts>1</AllowSMIMESoftCerts>
<AllowBrowser>1</AllowBrowser>
<AllowConsumerEmail>1</AllowConsumerEmail>
<AllowRemoteDesktop>1</AllowRemoteDesktop>
<AllowInternetSharing>1</AllowInternetSharing>
</EASProvisionDoc>"#
}

async fn handle_provision(
    state: &Arc<AppState>,
    owner: &str,
    req: &EasRequest,
    xml: &str,
    wbxml: &Wbxml,
    as_wbxml: bool,
    request_id: &str,
) -> Response {
    let device_id = req
        .device_id
        .clone()
        .unwrap_or_else(|| "unknown-device".to_string());
    let incoming_key = req.policy_key.clone().unwrap_or_else(|| "0".to_string());
    let (friendly_name, model, os, phone_number, imei, user_agent) = parse_device_information(xml);
    if [
        friendly_name.as_ref(),
        model.as_ref(),
        os.as_ref(),
        phone_number.as_ref(),
        imei.as_ref(),
        user_agent.as_ref(),
    ]
    .iter()
    .any(|v| v.is_some())
    {
        let _ = state
            .storage
            .upsert_device_info(&crate::storage::DeviceInfoParams {
                owner,
                device_id: &device_id,
                friendly_name: friendly_name.as_deref().unwrap_or(""),
                model: model.as_deref().unwrap_or(""),
                os: os.as_deref().unwrap_or(""),
                phone_number: phone_number.as_deref().unwrap_or(""),
                imei: imei.as_deref().unwrap_or(""),
                user_agent: user_agent.as_deref().unwrap_or(""),
            })
            .await;
    }
    if incoming_key.as_bytes().ct_eq(b"0").into() {
        let server_policy_key = Uuid::new_v4().simple().to_string();
        let _ = state
            .storage
            .set_provision_policy(owner, &device_id, &server_policy_key, "pending")
            .await;
        let response = format!(
            r#"<?xml version="1.0" encoding="utf-8"?>
<Provision xmlns="Provision:">
  <Status>1</Status>
  <Policies>
    <Policy>
      <PolicyType>MS-EAS-Provisioning-WBXML</PolicyType>
      <Status>1</Status>
      <PolicyKey>{policy_key}</PolicyKey>
      <Data>
        {doc}
      </Data>
    </Policy>
  </Policies>
</Provision>"#,
            policy_key = server_policy_key,
            doc = eas_provision_doc_xml()
        );
        return xml_or_wbxml_response(wbxml, as_wbxml, &response, request_id);
    }
    let valid = match state.storage.get_provision_policy(owner, &device_id).await {
        Ok(Some((stored, _))) => stored.as_bytes().ct_eq(incoming_key.as_bytes()).into(),
        _ => false,
    };
    if valid {
        let _ = state
            .storage
            .set_provision_policy(owner, &device_id, &incoming_key, "acknowledged")
            .await;
        let response = format!(
            r#"<?xml version="1.0" encoding="utf-8"?>
<Provision xmlns="Provision:">
  <Status>1</Status>
  <Policies>
    <Policy>
      <PolicyType>MS-EAS-Provisioning-WBXML</PolicyType>
      <Status>1</Status>
      <PolicyKey>{}</PolicyKey>
    </Policy>
  </Policies>
</Provision>"#,
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
            Some((expected, _)) if expected.as_bytes().ct_eq(incoming.as_bytes()).into() => {}
            _ => {
                let xml = r#"<?xml version="1.0" encoding="utf-8"?><FolderSync xmlns="FolderHierarchy:"><Status>9</Status><SyncKey>0</SyncKey></FolderSync>"#;
                return xml_or_wbxml_response(wbxml, as_wbxml, xml, request_id);
            }
        }
    }
    let new_sync_key = Uuid::new_v4().simple().to_string();
    let latest_seq = state.storage.get_latest_change_seq().await.unwrap_or(0);
    let _ = state
        .storage
        .set_sync_key(
            owner,
            &collection_id,
            &new_sync_key,
            Some(&format!("seq:{}", latest_seq)),
        )
        .await;
    let changes = if incoming == "0" {
        r#"<Changes><Count>1</Count><Add><ServerId>1</ServerId><ParentId>0</ParentId><DisplayName>Calendar</DisplayName><Type>8</Type></Add></Changes>"#
    } else {
        r#"<Changes><Count>0</Count></Changes>"#
    };
    let resp_xml = format!(
        r#"<?xml version="1.0" encoding="utf-8"?><FolderSync xmlns="FolderHierarchy:"><Status>1</Status><SyncKey>{}</SyncKey>{}</FolderSync>"#,
        new_sync_key, changes
    );
    let mut r = xml_or_wbxml_response(wbxml, as_wbxml, &resp_xml, request_id);
    if incoming == "0" {
        r.headers_mut()
            .insert("X-MS-RP", HeaderValue::from_static("1"));
    }
    r
}

async fn handle_ping(
    state: &Arc<AppState>,
    owner: &str,
    req: &EasRequest,
    xml: &str,
    wbxml: &Wbxml,
    as_wbxml: bool,
    request_id: &str,
) -> Response {
    const MIN_HEARTBEAT_SECS: u64 = 60;
    const MAX_HEARTBEAT_SECS: u64 = 3540;
    const MAX_PING_FOLDERS: usize = 200;
    let device_id = req.device_id.as_deref().unwrap_or("unknown-device");
    let cache_key = format!("{}:{}", owner, device_id);

    let cached = {
        let mut cache = PING_CACHE.lock().await;
        cache.get(&cache_key).cloned()
    };

    let heartbeat = extract_first_tag_text(xml, b"HeartbeatInterval")
        .and_then(|v| v.parse::<u64>().ok())
        .or_else(|| cached.as_ref().map(|e| e.heartbeat));
    let folders = {
        let parsed = parse_ping_folders(xml);
        if parsed.is_empty() {
            cached.map(|e| e.folders).unwrap_or_default()
        } else {
            parsed
        }
    };

    if heartbeat.is_none() || folders.is_empty() {
        let xml = r#"<?xml version="1.0" encoding="utf-8"?><Ping xmlns="Ping:"><Status>3</Status></Ping>"#;
        return xml_or_wbxml_response(wbxml, as_wbxml, xml, request_id);
    }
    let heartbeat = heartbeat.unwrap_or(MIN_HEARTBEAT_SECS);
    if !(MIN_HEARTBEAT_SECS..=MAX_HEARTBEAT_SECS).contains(&heartbeat) {
        let corrected = heartbeat.clamp(MIN_HEARTBEAT_SECS, MAX_HEARTBEAT_SECS);
        let xml = format!(
            r#"<?xml version="1.0" encoding="utf-8"?><Ping xmlns="Ping:"><Status>5</Status><HeartbeatInterval>{}</HeartbeatInterval></Ping>"#,
            corrected
        );
        return xml_or_wbxml_response(wbxml, as_wbxml, &xml, request_id);
    }
    if folders.len() > MAX_PING_FOLDERS {
        let xml = format!(
            r#"<?xml version="1.0" encoding="utf-8"?><Ping xmlns="Ping:"><Status>6</Status><MaxFolders>{}</MaxFolders></Ping>"#,
            MAX_PING_FOLDERS
        );
        return xml_or_wbxml_response(wbxml, as_wbxml, &xml, request_id);
    }
    if folders
        .iter()
        .any(|f| !f.id.eq("1") || !f.class_name.eq_ignore_ascii_case("Calendar"))
    {
        let xml = r#"<?xml version="1.0" encoding="utf-8"?><Ping xmlns="Ping:"><Status>7</Status></Ping>"#;
        return xml_or_wbxml_response(wbxml, as_wbxml, xml, request_id);
    }

    {
        let mut cache = PING_CACHE.lock().await;
        cache.put(
            cache_key.clone(),
            PingCacheEntry {
                heartbeat,
                folders: folders.clone(),
            },
        );
    }

    let deadline = Instant::now() + Duration::from_secs(heartbeat);
    loop {
        let mut changed_folders = Vec::new();
        for folder in &folders {
            if folder.id != "1" {
                continue;
            }
            let collection_id = scoped_collection_id(&folder.id, device_id);
            let since = state
                .storage
                .get_sync_key(owner, &collection_id)
                .await
                .ok()
                .flatten()
                .and_then(|(_, token)| token)
                .and_then(|t| t.strip_prefix("seq:").map(|v| v.to_string()))
                .and_then(|v| v.parse::<i64>().ok())
                .unwrap_or(0);
            let changed = state
                .storage
                .list_changes_since_seq(owner, since, 1000)
                .await
                .unwrap_or_default();
            let deleted = state
                .storage
                .list_deleted_since_seq(owner, since)
                .await
                .unwrap_or_default();
            if !changed.is_empty() || !deleted.is_empty() {
                changed_folders.push(folder.id.as_str());
            }
        }
        if !changed_folders.is_empty() {
            let xml = format!(
                r#"<?xml version="1.0" encoding="utf-8"?><Ping xmlns="Ping:"><Status>2</Status><Folders>{}</Folders></Ping>"#,
                changed_folders
                    .iter()
                    .map(|id| format!("<Folder>{}</Folder>", xml_escape(id)))
                    .collect::<Vec<_>>()
                    .join("")
            );
            return xml_or_wbxml_response(wbxml, as_wbxml, &xml, request_id);
        }
        if Instant::now() >= deadline {
            let xml = r#"<?xml version="1.0" encoding="utf-8"?><Ping xmlns="Ping:"><Status>1</Status></Ping>"#;
            return xml_or_wbxml_response(wbxml, as_wbxml, xml, request_id);
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        tokio::time::sleep(remaining.min(Duration::from_secs(15))).await;
    }
}

async fn handle_settings(
    username: &str,
    wbxml: &Wbxml,
    as_wbxml: bool,
    request_id: &str,
    xml_body: &str,
) -> Response {
    let primary_email = username.to_string();
    let mut sections = String::new();
    let has_user_info =
        xml_body.contains("<UserInformation>") || xml_body.contains("<UserInformation/>");
    let has_oof = xml_body.contains("<Oof>") || xml_body.contains("<Oof/>");
    let has_device_password =
        xml_body.contains("<DevicePassword>") || xml_body.contains("<DevicePassword/>");

    if has_user_info || (!has_oof && !has_device_password) {
        let email_entries = active_user_emails(username)
            .into_iter()
            .map(|email| {
                format!(
                    "<EmailAddresses><SMTPAddress>{}</SMTPAddress><PrimarySmtpAddress>{}</PrimarySmtpAddress></EmailAddresses>",
                    xml_escape(&email),
                    xml_escape(&primary_email)
                )
            })
            .collect::<String>();
        sections.push_str(&format!(
            r#"<UserInformation>
    <Status>1</Status>
    <Get>
      <Accounts>
        <Account>
          <AccountId>{}</AccountId>
          <AccountName>{}</AccountName>
          {}
        </Account>
      </Accounts>
    </Get>
  </UserInformation>"#,
            xml_escape(&primary_email),
            xml_escape(username),
            email_entries
        ));
    }
    if has_oof {
        sections.push_str(
            r#"<Oof>
    <Status>1</Status>
    <Get>
      <OofState>0</OofState>
    </Get>
  </Oof>"#,
        );
    }
    if has_device_password {
        sections.push_str(
            r#"<DevicePassword>
    <Status>1</Status>
    <Get>
      <DevicePasswordEnabled>0</DevicePasswordEnabled>
      <MinPasswordLength>0</MinPasswordLength>
      <MaxPasswordLength>0</MaxPasswordLength>
      <MaxInactivityTimeDeviceLockInMinutes>0</MaxInactivityTimeDeviceLockInMinutes>
      <MaxFailedPasswordAttempts>0</MaxFailedPasswordAttempts>
      <PasswordExpirationInDays>0</PasswordExpirationInDays>
    </Get>
  </DevicePassword>"#,
        );
    }
    let response = format!(
        r#"<?xml version="1.0" encoding="utf-8"?>
<Settings xmlns="Settings:">
  <Status>1</Status>
  {}
</Settings>"#,
        sections
    );
    xml_or_wbxml_response(wbxml, as_wbxml, &response, request_id)
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
            Some((expected, _)) if expected.as_bytes().ct_eq(incoming.as_bytes()).into() => {}
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
            .and_then(|t| t.strip_prefix("seq:"))
            .and_then(|v| v.parse::<i64>().ok())
            .unwrap_or(0)
    };
    let changed = state
        .storage
        .list_changes_since_seq(owner, since, 1000)
        .await
        .unwrap_or_default();
    let deleted = state
        .storage
        .list_deleted_since_seq(owner, since)
        .await
        .unwrap_or_default();
    let estimate = changed.len() + deleted.len();
    let xml = format!(
        r#"<?xml version="1.0" encoding="utf-8"?><GetItemEstimate xmlns="GetItemEstimate:"><Response><Status>1</Status><Collection><CollectionId>{}</CollectionId><Estimate>{}</Estimate></Collection></Response></GetItemEstimate>"#,
        visible_collection_id, estimate
    );
    xml_or_wbxml_response(wbxml, as_wbxml, &xml, request_id)
}

async fn handle_item_operations(
    state: &Arc<AppState>,
    username: &str,
    password: SecretString,
    xml: &str,
    wbxml: &Wbxml,
    as_wbxml: bool,
    request_id: &str,
) -> Response {
    let fetches = parse_item_operations_fetches(xml);
    if fetches.is_empty() {
        return bad_request_response(request_id, "ItemOperations requires at least one Fetch");
    }
    let caldav = match CaldavClient::new(&state.cfg) {
        Ok(c) => c,
        Err(e) => {
            return bad_request_response(request_id, &format!("CalDAV client init failed: {e}"));
        }
    };
    let owner_lower = crate::util::normalize_email(username);
    let mut responses = String::new();
    for fetch in fetches {
        let store = if fetch.store.is_empty() {
            "Mailbox".to_string()
        } else {
            fetch.store
        };
        let collection_id = fetch.collection_id.unwrap_or_else(|| "1".to_string());

        if let Some(file_ref) = fetch.file_reference.as_deref() {
            match state
                .attachment_manager
                .get_attachment(&owner_lower, file_ref)
                .await
            {
                Ok(Some(attachment)) => {
                    let parent_id = &attachment.parent_item_server_id;
                    let item_owner = match state.storage.get_item_owner(parent_id).await {
                        Ok(Some(o)) => o,
                        Ok(None) => {
                            responses.push_str(&format!(
                        "<Fetch><Store>{}</Store><FileReference>{}</FileReference><Status>8</Status></Fetch>",
                        xml_escape(&store),
                        xml_escape(file_ref)
                    ));
                            continue;
                        }
                        Err(e) => {
                            tracing::error!(error = %e, "Failed to get item owner");
                            responses.push_str(&format!(
                        "<Fetch><Store>{}</Store><FileReference>{}</FileReference><Status>8</Status></Fetch>",
                        xml_escape(&store),
                        xml_escape(file_ref)
                    ));
                            continue;
                        }
                    };
                    let calendar_folder_id = crate::ews_folders::folder_id_for(
                        &item_owner,
                        crate::ews_folders::DistinguishedFolder::Calendar,
                    );
                    let enforcement = PermissionEnforcement::new(&state.storage);
                    let perm_ctx = PermissionContext::new(
                        username.to_string(),
                        item_owner.clone(),
                        calendar_folder_id,
                    );
                    match enforcement.can_read_item(&perm_ctx).await {
                        Ok(true) => {}
                        Ok(false) => {
                            responses.push_str(&format!(
                                "<Fetch><Store>{}</Store><FileReference>{}</FileReference><Status>4</Status></Fetch>",
                                xml_escape(&store),
                                xml_escape(file_ref)
                            ));
                            continue;
                        }
                        Err(_) => {
                            responses.push_str(&format!(
                                "<Fetch><Store>{}</Store><FileReference>{}</FileReference><Status>8</Status></Fetch>",
                                xml_escape(&store),
                                xml_escape(file_ref)
                            ));
                            continue;
                        }
                    }
                    responses.push_str(&format!(
                        "<Fetch><Store>{}</Store><FileReference>{}</FileReference><Class>Calendar</Class><Status>1</Status><Properties>{}</Properties></Fetch>",
                        xml_escape(&store),
                        xml_escape(file_ref),
                        crate::attachment::render_eas_attachment_content_xml(&attachment)
                    ));
                }
                Ok(None) => {
                    responses.push_str(&format!(
                        "<Fetch><Store>{}</Store><FileReference>{}</FileReference><Status>8</Status></Fetch>",
                        xml_escape(&store),
                        xml_escape(file_ref)
                    ));
                }
                Err(e) => {
                    tracing::error!(
                        "ItemOperations attachment fetch error for {}: {}",
                        file_ref,
                        e
                    );
                    responses.push_str(&format!(
                        "<Fetch><Store>{}</Store><FileReference>{}</FileReference><Status>8</Status></Fetch>",
                        xml_escape(&store),
                        xml_escape(file_ref)
                    ));
                }
            }
            continue;
        }

        let Some(server_id) = fetch.server_id.or(fetch.long_id) else {
            responses.push_str(&format!(
                "<Fetch><Store>{}</Store><Status>6</Status></Fetch>",
                xml_escape(&store)
            ));
            continue;
        };
        match state.storage.get_item_owner(&server_id).await {
            Ok(None) => {
                responses.push_str(&format!(
            "<Fetch><Store>{}</Store><CollectionId>{}</CollectionId><ServerId>{}</ServerId><Status>8</Status></Fetch>",
            xml_escape(&store),
            xml_escape(&collection_id),
            xml_escape(&server_id)
        ));
                continue;
            }
            Err(e) => {
                tracing::error!("Failed to lookup item owner for {}: {}", server_id, e);
                responses.push_str(&format!(
            "<Fetch><Store>{}</Store><CollectionId>{}</CollectionId><ServerId>{}</ServerId><Status>8</Status></Fetch>",
            xml_escape(&store),
            xml_escape(&collection_id),
            xml_escape(&server_id)
        ));
                continue;
            }
            _ => {}
        };

        let calendar_folder_id = crate::ews_folders::folder_id_for(
            &owner_lower,
            crate::ews_folders::DistinguishedFolder::Calendar,
        );
        let enforcement = PermissionEnforcement::new(&state.storage);
        let perm_ctx = PermissionContext::new(
            username.to_string(),
            owner_lower.clone(),
            calendar_folder_id.clone(),
        );
        match enforcement.can_read_item(&perm_ctx).await {
            Ok(true) => {}
            Ok(false) => {
                responses.push_str(&format!(
                    "<Fetch><Store>{}</Store><CollectionId>{}</CollectionId><ServerId>{}</ServerId><Status>4</Status></Fetch>",
                    xml_escape(&store),
                    xml_escape(&collection_id),
                    xml_escape(&server_id)
                ));
                continue;
            }
            Err(e) => {
                tracing::error!("Permission check failed for item {}: {}", server_id, e);
                responses.push_str(&format!(
                    "<Fetch><Store>{}</Store><CollectionId>{}</CollectionId><ServerId>{}</ServerId><Status>8</Status></Fetch>",
                    xml_escape(&store),
                    xml_escape(&collection_id),
                    xml_escape(&server_id)
                ));
                continue;
            }
        }

        let lookup = match state
            .storage
            .get_ews_item_by_server_id(&owner_lower, &server_id)
            .await
        {
            Ok(Some(row)) => row,
            _ => {
                responses.push_str(&format!(
                    "<Fetch><Store>{}</Store><CollectionId>{}</CollectionId><ServerId>{}</ServerId><Status>8</Status></Fetch>",
                    xml_escape(&store),
                    xml_escape(&collection_id),
                    xml_escape(&server_id)
                ));
                continue;
            }
        };

        let get_future = caldav.get_event(
            &lookup.resource_href,
            &owner_lower,
            password.expose_secret(),
        );
        let Ok(Ok((ics, _etag))) = timeout(CALDAV_TIMEOUT, get_future).await else {
            responses.push_str(&format!(
                "<Fetch><Store>{}</Store><CollectionId>{}</CollectionId><ServerId>{}</ServerId><Status>8</Status></Fetch>",
                xml_escape(&store),
                xml_escape(&collection_id),
                xml_escape(&server_id)
            ));
            continue;
        };
        let Some(item) = parse_ics_event(&ics) else {
            responses.push_str(&format!(
                "<Fetch><Store>{}</Store><CollectionId>{}</CollectionId><ServerId>{}</ServerId><Status>6</Status></Fetch>",
                xml_escape(&store),
                xml_escape(&collection_id),
                xml_escape(&server_id)
            ));
            continue;
        };
        let mut app_data = sync::render_calendar_app_data(&item);
        if let Ok(att_list) = state
            .attachment_manager
            .get_attachments_for_item(&owner_lower, &server_id)
            .await
            && !att_list.is_empty()
        {
            let summaries: Vec<_> = att_list.iter().map(|a| a.to_eas_summary()).collect();
            app_data.push_str(&crate::attachment::render_eas_attachments_xml(&summaries));
        }
        responses.push_str(&format!(
            "<Fetch><Store>{}</Store><CollectionId>{}</CollectionId><ServerId>{}</ServerId><Class>Calendar</Class><Status>1</Status><Properties>{}</Properties></Fetch>",
            xml_escape(&store),
            xml_escape(&collection_id),
            xml_escape(&server_id),
            app_data
        ));
    }
    let response = format!(
        r#"<?xml version="1.0" encoding="utf-8"?><ItemOperations xmlns="ItemOperations:" xmlns:Calendar="Calendar:" xmlns:AirSyncBase="AirSyncBase:"><Status>1</Status><Response>{}</Response></ItemOperations>"#,
        responses
    );
    xml_or_wbxml_response(wbxml, as_wbxml, &response, request_id)
}

async fn merged_freebusy_for_mailbox(
    state: &Arc<AppState>,
    mailbox: &str,
    password: &SecretString,
    start: chrono::DateTime<chrono::Utc>,
    end: chrono::DateTime<chrono::Utc>,
    slot_minutes: i64,
) -> String {
    let safe_interval = slot_minutes.clamp(5, 1440);
    let slot_count = (((end - start).num_seconds().max(0) + (safe_interval * 60 - 1))
        / (safe_interval * 60)) as usize;
    if slot_count == 0 {
        return "4".to_string();
    }
    let mut merged = vec!['0'; slot_count];
    let caldav = match CaldavClient::new(&state.cfg) {
        Ok(c) => c,
        Err(_) => {
            merged.fill('4');
            return merged.into_iter().collect();
        }
    };
    let calendars = timeout(
        CALDAV_TIMEOUT,
        caldav.find_user_calendars(mailbox, password.expose_secret()),
    )
    .await
    .ok()
    .and_then(|r| r.ok());
    if let Some(calendars) = calendars
        && let Some(collection_href) = calendars.first()
    {
        let query_result = timeout(
            CALDAV_TIMEOUT,
            caldav.query_events(
                collection_href,
                &start.format("%Y%m%dT%H%M%SZ").to_string(),
                &end.format("%Y%m%dT%H%M%SZ").to_string(),
                mailbox,
                password.expose_secret(),
            ),
        )
        .await
        .ok()
        .and_then(|r| r.ok());
        if let Some(events_xml) = query_result {
            let mut reader = Reader::from_str(&events_xml);
            reader.config_mut().trim_text(true);
            let mut buf = Vec::new();
            let mut in_cal_data = false;
            loop {
                match reader.read_event_into(&mut buf) {
                    Ok(Event::Start(e)) if e.name().local_name().as_ref() == b"calendar-data" => {
                        in_cal_data = true;
                    }
                    Ok(Event::Text(t)) if in_cal_data => {
                        if let Ok(ics) = t.decode()
                            && let Some(item) = parse_ics_event(&ics)
                        {
                            let sd = match item.busy_status.unwrap_or(2) {
                                0 => '0',
                                1 => '1',
                                3 => '3',
                                _ => '2',
                            };
                            for (i, slot) in merged.iter_mut().enumerate() {
                                let ss =
                                    start + chrono::Duration::minutes((i as i64) * safe_interval);
                                let se = ss + chrono::Duration::minutes(safe_interval);
                                if item.start < se && item.end > ss && sd > *slot {
                                    *slot = sd;
                                }
                            }
                        }
                    }
                    Ok(Event::End(e)) if e.name().local_name().as_ref() == b"calendar-data" => {
                        in_cal_data = false;
                    }
                    Ok(Event::Eof) => break,
                    _ => {}
                }
                buf.clear();
            }
        } else {
            merged.fill('4');
        }
    } else {
        merged.fill('4');
    }
    merged.into_iter().collect()
}

async fn handle_resolve_recipients(
    state: &Arc<AppState>,
    username: &str,
    password: SecretString,
    xml: &str,
    wbxml: &Wbxml,
    as_wbxml: bool,
    request_id: &str,
) -> Response {
    let recipients = {
        let parsed = extract_all_tag_text(xml, b"To")
            .into_iter()
            .filter(|v| !v.is_empty())
            .collect::<Vec<_>>();
        if parsed.is_empty() {
            vec![username.to_string()]
        } else {
            parsed
        }
    };
    let availability_requested = xml.contains("<Availability>");
    let availability_window = if availability_requested {
        let Some(start) =
            extract_first_tag_text(xml, b"StartTime").and_then(|v| parse_datetime(&v))
        else {
            return bad_request_response(
                request_id,
                "ResolveRecipients Availability requires StartTime",
            );
        };
        let end = {
            let pe = extract_first_tag_text(xml, b"EndTime")
                .and_then(|v| parse_datetime(&v))
                .unwrap_or_else(|| start + chrono::Duration::days(7));
            let max_end = start + chrono::Duration::days(MAX_FREEBUSY_DAYS);
            let clamped = if pe > max_end { max_end } else { pe };
            if clamped <= start {
                start + chrono::Duration::days(7)
            } else {
                clamped
            }
        };
        Some((start, end))
    } else {
        None
    };

    let freebusy_futures = recipients.iter().map(|recipient| {
        let state = state.clone();
        let recipient = recipient.clone();
        let password = password.clone();
        let window = availability_window;
        async move {
            if let Some((start, end)) = window {
                merged_freebusy_for_mailbox(&state, &recipient, &password, start, end, 30).await
            } else {
                String::new()
            }
        }
    });
    let freebusy_results = join_all(freebusy_futures).await;

    let mut recipient_xml = String::new();
    for (recipient, freebusy) in recipients.iter().zip(freebusy_results) {
        let avail_xml = if availability_window.is_some() {
            format!(
                "<Availability><Status>1</Status><MergedFreeBusy>{}</MergedFreeBusy></Availability>",
                freebusy
            )
        } else {
            String::new()
        };
        recipient_xml.push_str(&format!(
            "<Recipient><Type>1</Type><DisplayName>{}</DisplayName><EmailAddress>{}</EmailAddress>{}</Recipient>",
            xml_escape(recipient),
            xml_escape(recipient),
            avail_xml
        ));
    }
    let primary = recipients
        .first()
        .cloned()
        .unwrap_or_else(|| username.to_string());
    let response = format!(
        r#"<?xml version="1.0" encoding="utf-8"?><ResolveRecipients xmlns="ResolveRecipients:"><Status>1</Status><Response><To>{}</To><Status>1</Status><RecipientCount>{}</RecipientCount>{}</Response></ResolveRecipients>"#,
        xml_escape(&primary),
        recipients.len(),
        recipient_xml
    );
    xml_or_wbxml_response(wbxml, as_wbxml, &response, request_id)
}

async fn load_calendar_events(
    state: &Arc<AppState>,
    username: &str,
    password: &SecretString,
    start: chrono::DateTime<chrono::Utc>,
    end: chrono::DateTime<chrono::Utc>,
) -> anyhow::Result<Vec<(String, String, crate::calendar::CalendarItem)>> {
    let caldav = CaldavClient::new(&state.cfg)?;
    let calendars = timeout(
        CALDAV_TIMEOUT,
        caldav.find_user_calendars(username, password.expose_secret()),
    )
    .await??;
    let collection_href = calendars
        .first()
        .ok_or_else(|| anyhow::anyhow!("no calendars found"))?
        .clone();
    let events_xml = timeout(
        CALDAV_TIMEOUT,
        caldav.query_events(
            &collection_href,
            &start.format("%Y%m%dT%H%M%SZ").to_string(),
            &end.format("%Y%m%dT%H%M%SZ").to_string(),
            username,
            password.expose_secret(),
        ),
    )
    .await??;
    let mut reader = Reader::from_str(&events_xml);
    reader.config_mut().trim_text(true);
    let mut buf = Vec::new();
    let mut href = String::new();
    let mut ics = String::new();
    let mut out = Vec::new();
    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(ref e)) => match e.name().local_name().as_ref() {
                b"href" => {
                    if let Ok(Event::Text(t)) = reader.read_event_into(&mut buf) {
                        href = t.decode().unwrap_or_default().to_string();
                    }
                }
                b"calendar-data" => {
                    if let Ok(Event::Text(t)) = reader.read_event_into(&mut buf) {
                        ics = t.decode().unwrap_or_default().to_string();
                    }
                }
                _ => {}
            },
            Ok(Event::End(ref e)) if e.name().local_name().as_ref() == b"response" => {
                if !href.is_empty()
                    && let Some(item) = parse_ics_event(&ics)
                {
                    let server_id = sync::generate_server_id(state.cfg.hmac_secret(), &href);
                    out.push((server_id, href.clone(), item));
                }
                href.clear();
                ics.clear();
            }
            Ok(Event::Eof) => break,
            _ => {}
        }
        buf.clear();
    }
    Ok(out)
}

async fn handle_search(
    state: &Arc<AppState>,
    username: &str,
    password: SecretString,
    xml: &str,
    wbxml: &Wbxml,
    as_wbxml: bool,
    request_id: &str,
) -> Response {
    let req = parse_search_request(xml);
    if !req.store_name.eq_ignore_ascii_case("Mailbox") {
        let r = r#"<?xml version="1.0" encoding="utf-8"?><Search xmlns="Search:"><Status>1</Status><Response><Store><Status>11</Status></Store></Response></Search>"#;
        return xml_or_wbxml_response(wbxml, as_wbxml, r, request_id);
    }
    let start = req
        .starts
        .unwrap_or_else(|| chrono::Utc::now() - chrono::Duration::weeks(52));
    let end = req
        .ends
        .unwrap_or_else(|| chrono::Utc::now() + chrono::Duration::weeks(52));
    let Ok(events) = load_calendar_events(state, username, &password, start, end).await else {
        let r = r#"<?xml version="1.0" encoding="utf-8"?><Search xmlns="Search:"><Status>1</Status><Response><Store><Status>6</Status></Store></Response></Search>"#;
        return xml_or_wbxml_response(wbxml, as_wbxml, r, request_id);
    };
    let mut matches: Vec<_> = events
        .into_iter()
        .filter(|(_, _, item)| matches_search(item, req.query_text.as_deref()))
        .collect();
    matches.sort_by_key(|(_, _, item)| item.start);
    let total = matches.len();
    let start_idx = req.range_start.min(total);
    let end_idx = req.range_end.min(total.saturating_sub(1));
    let window = if total == 0 || start_idx > end_idx {
        &[][..]
    } else {
        &matches[start_idx..=end_idx]
    };
    let results_xml = window
        .iter()
        .map(|(sid, _, item)| {
            format!(
                "<Result><Class>Calendar</Class><CollectionId>1</CollectionId><LongId>{}</LongId><Properties>{}</Properties></Result>",
                xml_escape(sid),
                sync::render_calendar_app_data(item)
            )
        })
        .collect::<String>();
    let range_xml = if total == 0 {
        "0-0/0".to_string()
    } else {
        format!(
            "{start_idx}-{}/{}",
            start_idx + window.len().saturating_sub(1),
            total
        )
    };
    let response = format!(
        r#"<?xml version="1.0" encoding="utf-8"?>
<Search xmlns="Search:" xmlns:Calendar="Calendar:" xmlns:AirSyncBase="AirSyncBase:">
  <Status>1</Status>
  <Response>
    <Store>
      <Status>1</Status>
      <Range>{}</Range>
      <Total>{}</Total>
      {}
    </Store>
  </Response>
</Search>"#,
        xml_escape(&range_xml),
        total,
        results_xml
    );
    xml_or_wbxml_response(wbxml, as_wbxml, &response, request_id)
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
    if body.len() > MAX_BODY_SIZE {
        return bad_request_response(&request_id, "Request body too large");
    }
    let Some((username, password)) = parse_basic_auth(&headers) else {
        return unauth_response(&request_id);
    };
    // Verify credentials early to avoid unnecessary processing
    if !state
        .auth_verifier
        .verify(&username, password.expose_secret())
        .await
    {
        tracing::debug!(request_id = %request_id, user = %username, "Authentication failed");
        return unauth_response(&request_id);
    }
    // Mark user as known for fail-open during future backend outages
    state.auth_verifier.mark_user_known(&username);
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
    let req = parse_request(&query, &xml, &headers);
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
    if maybe_throttle(&username, &device_id).await {
        return throttled_response(&request_id);
    }

    match req.command.as_str() {
        "FolderSync" => {
            handle_folder_sync(&state, &username, &req, &wbxml, wants_wbxml, &request_id).await
        }
        "Provision" => {
            handle_provision(
                &state,
                &username,
                &req,
                &xml,
                &wbxml,
                wants_wbxml,
                &request_id,
            )
            .await
        }
        "Sync" => {
            let collection_id = req.collection_id.as_deref().unwrap_or("1");
            let state_collection_id = scoped_collection_id(
                collection_id,
                req.device_id.as_deref().unwrap_or("unknown-device"),
            );
            let incoming_key = req.sync_key.as_deref().unwrap_or("0");
            let class = req.class.as_deref().unwrap_or("Calendar");
            let mut mutation_responses = String::new();
            let owner = crate::ews::owner_from_username(&username);
            let calendar_folder_id = crate::ews_folders::folder_id_for(
                owner,
                crate::ews_folders::DistinguishedFolder::Calendar,
            );
            let enforcement = PermissionEnforcement::new(&state.storage);
            let perm_ctx = PermissionContext::new(
                username.clone(),
                owner.to_string(),
                calendar_folder_id.clone(),
            );
            if xml.contains("<Add") || xml.contains(":Add") {
                match enforcement.can_create_item(&perm_ctx).await {
                    Ok(true) => {}
                    Ok(false) => {
                        let err_xml = r#"
<?xml version="1.0" encoding="utf-8"?><Sync xmlns="AirSync:"><Status>4</Status></Sync>"#;
                        return xml_or_wbxml_response(&wbxml, wants_wbxml, err_xml, &request_id);
                    }
                    Err(e) => {
                        tracing::error!(request_id = %request_id, error = %e, "Permission check failed for Create operation");
                        let err_xml = r#"
<?xml version="1.0" encoding="utf-8"?><Sync xmlns="AirSync:"><Status>4</Status></Sync>"#;
                        return xml_or_wbxml_response(&wbxml, wants_wbxml, err_xml, &request_id);
                    }
                }
            }
            if xml.contains("<Change") || xml.contains(":Change") {
                match enforcement.can_edit_item(&perm_ctx).await {
                    Ok(true) => {}
                    Ok(false) => {
                        let err_xml = r#"
<?xml version="1.0" encoding="utf-8"?><Sync xmlns="AirSync:"><Status>4</Status></Sync>"#;
                        return xml_or_wbxml_response(&wbxml, wants_wbxml, err_xml, &request_id);
                    }
                    Err(e) => {
                        tracing::error!(request_id = %request_id, error = %e, "Permission check failed for Edit operation");
                        let err_xml = r#"
<?xml version="1.0" encoding="utf-8"?><Sync xmlns="AirSync:"><Status>4</Status></Sync>"#;
                        return xml_or_wbxml_response(&wbxml, wants_wbxml, err_xml, &request_id);
                    }
                }
            }
            if xml.contains("<Delete") || xml.contains(":Delete") {
                match enforcement.can_delete_item(&perm_ctx).await {
                    Ok(true) => {}
                    Ok(false) => {
                        let err_xml = r#"
<?xml version="1.0" encoding="utf-8"?><Sync xmlns="AirSync:"><Status>4</Status></Sync>"#;
                        return xml_or_wbxml_response(&wbxml, wants_wbxml, err_xml, &request_id);
                    }
                    Err(e) => {
                        tracing::error!(request_id = %request_id, error = %e, "Permission check failed for Delete operation");
                        let err_xml = r#"
<?xml version="1.0" encoding="utf-8"?><Sync xmlns="AirSync:"><Status>4</Status></Sync>"#;
                        return xml_or_wbxml_response(&wbxml, wants_wbxml, err_xml, &request_id);
                    }
                }
            }
            if xml.contains("<Add")
                || xml.contains("<Change")
                || xml.contains("<Delete")
                || xml.contains(":Add")
                || xml.contains(":Change")
                || xml.contains(":Delete")
            {
                match sync::apply_client_sync_mutations(
                    state.clone(),
                    &username,
                    &state_collection_id,
                    &username,
                    password.expose_secret(),
                    &xml,
                )
                .await
                {
                    Ok(results) => {
                        mutation_responses = sync::render_client_mutation_responses(&results);
                    }
                    Err(e) => {
                        tracing::error!(
                            "request_id={} failed applying Sync mutations: {}",
                            request_id,
                            e
                        );
                        let err_xml = r#"<?xml version="1.0" encoding="utf-8"?><Sync xmlns="AirSync:"><Status>6</Status></Sync>"#;
                        return xml_or_wbxml_response(&wbxml, wants_wbxml, err_xml, &request_id);
                    }
                }
            }
            let opts = SyncOptions {
                window_size: req.window_size.unwrap_or(100),
                get_changes: req.get_changes,
                filter_start: req
                    .filter_type
                    .map(filter_type_to_start)
                    .unwrap_or_else(|| chrono::Utc::now() - chrono::Duration::weeks(52)),
            };
            match sync::perform_sync(&sync::PerformSyncParams {
                state: state.clone(),
                owner: &username,
                collection_id,
                state_collection_id: &state_collection_id,
                incoming_sync_key: incoming_key,
                content_class: class,
                opts,
                username: &username,
                password: password.expose_secret(),
                client_mutation_responses: &mutation_responses,
            })
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
            handle_ping(
                &state,
                &username,
                &req,
                &xml,
                &wbxml,
                wants_wbxml,
                &request_id,
            )
            .await
        }
        "Settings" => handle_settings(&username, &wbxml, wants_wbxml, &request_id, &xml).await,
        "ItemOperations" => {
            handle_item_operations(
                &state,
                &username,
                password,
                &xml,
                &wbxml,
                wants_wbxml,
                &request_id,
            )
            .await
        }
        "Search" => {
            handle_search(
                &state,
                &username,
                password,
                &xml,
                &wbxml,
                wants_wbxml,
                &request_id,
            )
            .await
        }
        "MeetingResponse" => {
            if let Some(req_id) = extract_first_tag_text(&xml, b"RequestId") {
                let user_response = extract_first_tag_text(&xml, b"UserResponse")
                    .and_then(|v| v.parse::<u8>().ok())
                    .unwrap_or(0);
                let instance_id = extract_first_tag_text(&xml, b"InstanceId")
                    .as_deref()
                    .and_then(parse_datetime);
                let send_response = xml.contains("<SendResponse") || xml.contains(":SendResponse");
                if let Err(e) = sync::apply_meeting_response(&sync::MeetingResponseArgs {
                    state: state.clone(),
                    owner: &username,
                    username: &username,
                    password: password.expose_secret(),
                    request_id: &req_id,
                    user_response,
                    instance_id,
                    send_response,
                })
                .await
                {
                    tracing::error!(
                        "request_id={} failed applying MeetingResponse: {}",
                        request_id,
                        e
                    );
                    let err_xml = r#"<?xml version="1.0" encoding="utf-8"?><MeetingResponse xmlns="MeetingResponse:"><Result><Status>6</Status></Result></MeetingResponse>"#;
                    return xml_or_wbxml_response(&wbxml, wants_wbxml, err_xml, &request_id);
                }
                let instance_xml = if let Some(iid) = instance_id {
                    format!(
                        "<InstanceId>{}</InstanceId>",
                        iid.format("%Y-%m-%dT%H:%M:%SZ")
                    )
                } else {
                    String::new()
                };
                let payload = format!(
                    r#"<?xml version="1.0" encoding="utf-8"?><MeetingResponse xmlns="MeetingResponse:"><Result><RequestId>{}</RequestId><CalendarId>{}</CalendarId><Status>1</Status>{}</Result></MeetingResponse>"#,
                    xml_escape(&req_id),
                    xml_escape(&req_id),
                    instance_xml
                );
                xml_or_wbxml_response(&wbxml, wants_wbxml, &payload, &request_id)
            } else {
                bad_request_response(&request_id, "MeetingResponse requires RequestId")
            }
        }
        "ResolveRecipients" => {
            handle_resolve_recipients(
                &state,
                &username,
                password,
                &xml,
                &wbxml,
                wants_wbxml,
                &request_id,
            )
            .await
        }
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
        "MoveItems" => bad_request_response(
            &request_id,
            "MoveItems is not supported for this calendar-only mailbox surface",
        ),
        _ => unsupported_command_response(&req.command, &wbxml, wants_wbxml, &request_id),
    }
}
