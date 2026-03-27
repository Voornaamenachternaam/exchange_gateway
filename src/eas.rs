use crate::caldav::CaldavClient;
use crate::calendar::{parse_datetime, parse_ics_event};
use crate::models::AppState;
use crate::sync;
use crate::sync::xml_escape;
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
    static ref PING_CACHE: Mutex<HashMap<String, PingCacheEntry>> = Mutex::new(HashMap::new());
}

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

pub fn parse_ping_folders(xml: &str) -> Vec<PingFolder> {
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
        .and_then(|(start, end)| Some((start.trim().parse().ok()?, end.trim().parse().ok()?)))
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
    .any(|value| value.to_ascii_lowercase().contains(&q))
        || item
            .attendees
            .iter()
            .any(|attendee| attendee.email.to_ascii_lowercase().contains(&q))
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
        if xml.contains("<Add") && !xml.contains("<ClientId>") {
            return Err("Sync Add requires ClientId");
        }
        if xml.contains("AppointmentReplyTime") {
            return Err("AppointmentReplyTime is response-only in Sync requests");
        }
        if xml.contains("ResponseType") {
            return Err("ResponseType is response-only in Sync requests");
        }
    }

    if cmd == "meetingresponse" && !xml.contains("<UserResponse>") {
        return Err("MeetingResponse requires UserResponse");
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
    let body = "Throttled";
    let mut response = (
        StatusCode::SERVICE_UNAVAILABLE,
        [(header::RETRY_AFTER.as_str(), &RETRY_AFTER_SECONDS.to_string())],
        body,
    ).into_response();
    inject_common_headers(&mut response, request_id);
    response
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
    let cached = PING_CACHE
        .lock()
        .ok()
        .and_then(|cache| cache.get(&cache_key).cloned());

    let heartbeat = extract_first_tag_text(xml, b"HeartbeatInterval")
        .and_then(|v| v.parse::<u64>().ok())
        .or_else(|| cached.as_ref().map(|entry| entry.heartbeat));
    let folders = {
        let parsed = parse_ping_folders(xml);
        if parsed.is_empty() {
            cached.map(|entry| entry.folders).unwrap_or_default()
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
        .any(|folder| !folder.id.eq("1") || !folder.class_name.eq_ignore_ascii_case("Calendar"))
    {
        let xml = r#"<?xml version="1.0" encoding="utf-8"?><Ping xmlns="Ping:"><Status>7</Status></Ping>"#;
        return xml_or_wbxml_response(wbxml, as_wbxml, xml, request_id);
    }

    if let Ok(mut cache) = PING_CACHE.lock() {
        cache.insert(
            cache_key,
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
                .and_then(|token| token.strip_prefix("seq:").map(|v| v.to_string()))
                .and_then(|v| v.parse::<i64>().ok())
                .unwrap_or(0);
            let changed = state
                .storage
                .list_changes_since_seq(owner, since)
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
                    .map(|id| format!("<Folder>{}</Folder>", id))
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
        let sleep_for = remaining.min(Duration::from_secs(1));
        tokio::time::sleep(sleep_for).await;
    }
}

async fn merged_freebusy_for_mailbox(
    state: &Arc<AppState>,
    mailbox: &str,
    password: &str,
    start: chrono::DateTime<chrono::Utc>,
    end: chrono::DateTime<chrono::Utc>,
    slot_minutes: i64,
) -> String {
    let safe_interval = slot_minutes.clamp(5, 1440);
    let slot_count = (((end - start).num_seconds().max(0) + (safe_interval * 60 - 1))
        / (safe_interval * 60)) as usize;
    let mut merged = vec!['0'; slot_count];

    let caldav = CaldavClient::new(&state.cfg);
    if let Ok(calendars) = caldav.find_user_calendars(mailbox, password).await
        && let Some(collection_href) = calendars.first()
        && let Ok(events_xml) = caldav
            .query_events(
                collection_href,
                &start.format("%Y%m%dT%H%M%SZ").to_string(),
                &end.format("%Y%m%dT%H%M%SZ").to_string(),
                mailbox,
                password,
            )
            .await
    {
        let mut reader = Reader::from_str(&events_xml);
        reader.config_mut().trim_text(true);
        let mut buf = Vec::new();
        let mut in_calendar_data = false;
        loop {
            match reader.read_event_into(&mut buf) {
                Ok(Event::Start(e)) if e.name().local_name().as_ref() == b"calendar-data" => {
                    in_calendar_data = true;
                }
                Ok(Event::Text(t)) if in_calendar_data => {
                    if let Ok(ics) = t.decode()
                        && let Some(item) = parse_ics_event(&ics)
                    {
                        let status_digit = match item.busy_status.unwrap_or(2) {
                            0 => '0',
                            1 => '1',
                            3 => '3',
                            _ => '2',
                        };
                        for (index, slot) in merged.iter_mut().enumerate() {
                            let slot_start =
                                start + chrono::Duration::minutes((index as i64) * safe_interval);
                            let slot_end = slot_start + chrono::Duration::minutes(safe_interval);
                            if item.start < slot_end
                                && item.end > slot_start
                                && status_digit > *slot
                            {
                                *slot = status_digit;
                            }
                        }
                    }
                }
                Ok(Event::End(e)) if e.name().local_name().as_ref() == b"calendar-data" => {
                    in_calendar_data = false;
                }
                Ok(Event::Eof) => break,
                _ => {}
            }
            buf.clear();
        }
    } else {
        merged.fill('4');
    }

    merged.into_iter().collect()
}

async fn handle_resolve_recipients(
    state: &Arc<AppState>,
    username: &str,
    password: &str,
    xml: &str,
    wbxml: &Wbxml,
    as_wbxml: bool,
    request_id: &str,
) -> Response {
    let recipients = {
        let parsed = extract_all_tag_text(xml, b"To")
            .into_iter()
            .filter(|value| !value.is_empty())
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
        let end = extract_first_tag_text(xml, b"EndTime")
            .and_then(|v| parse_datetime(&v))
            .unwrap_or_else(|| start + chrono::Duration::days(7));
        Some((start, end))
    } else {
        None
    };

    let mut recipient_xml = String::new();
    for recipient in &recipients {
        let availability_xml = if let Some((start, end)) = availability_window {
            let merged =
                merged_freebusy_for_mailbox(state, recipient, password, start, end, 30).await;
            format!(
                "<Availability><Status>1</Status><MergedFreeBusy>{}</MergedFreeBusy></Availability>",
                merged
            )
        } else {
            String::new()
        };
        recipient_xml.push_str(&format!(
            "<Recipient><Type>1</Type><DisplayName>{}</DisplayName><EmailAddress>{}</EmailAddress>{}</Recipient>",
            xml_escape(recipient),
            xml_escape(recipient),
            availability_xml
        ));
    }

    let primary = recipients
        .first()
        .cloned()
        .unwrap_or_else(|| username.to_string());
    let response = format!(
        r#"<?xml version="1.0" encoding="utf-8"?>
<ResolveRecipients xmlns="ResolveRecipients:"><Status>1</Status><Response><To>{}</To><Status>1</Status><RecipientCount>{}</RecipientCount>{}</Response></ResolveRecipients>"#,
        xml_escape(&primary),
        recipients.len(),
        recipient_xml
    );
    xml_or_wbxml_response(wbxml, as_wbxml, &response, request_id)
}

async fn load_calendar_events(
    state: &Arc<AppState>,
    username: &str,
    password: &str,
    start: chrono::DateTime<chrono::Utc>,
    end: chrono::DateTime<chrono::Utc>,
) -> anyhow::Result<Vec<(String, String, crate::calendar::CalendarItem)>> {
    let caldav = CaldavClient::new(&state.cfg);
    let calendars = caldav.find_user_calendars(username, password).await?;
    let collection_href = calendars
        .first()
        .ok_or_else(|| anyhow::anyhow!("no calendars found"))?
        .clone();
    let events_xml = caldav
        .query_events(
            &collection_href,
            &start.format("%Y%m%dT%H%M%SZ").to_string(),
            &end.format("%Y%m%dT%H%M%SZ").to_string(),
            username,
            password,
        )
        .await?;

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
                    let server_id = sync::generate_server_id(&state.cfg.hmac_secret, &href);
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

async fn handle_settings(
    username: &str,
    wbxml: &Wbxml,
    as_wbxml: bool,
    request_id: &str,
) -> Response {
    let primary_email = username.to_string();
    let email_entries = active_user_emails(username)
        .into_iter()
        .map(|email| {
            format!(
                "<Settings:EmailAddresses><Settings:SmtpAddress>{}</Settings:SmtpAddress><Settings:PrimarySmtpAddress>{}</Settings:PrimarySmtpAddress></Settings:EmailAddresses>",
                sync::xml_escape(&email),
                sync::xml_escape(&primary_email)
            )
        })
        .collect::<String>();
    let response = format!(
        r#"<?xml version="1.0" encoding="utf-8"?>
<Settings xmlns="Settings:">
  <Status>1</Status>
  <UserInformation>
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
  </UserInformation>
</Settings>"#,
        sync::xml_escape(&primary_email),
        sync::xml_escape(username),
        email_entries
    );
    xml_or_wbxml_response(wbxml, as_wbxml, &response, request_id)
}

async fn handle_item_operations(
    state: &Arc<AppState>,
    username: &str,
    password: &str,
    xml: &str,
    wbxml: &Wbxml,
    as_wbxml: bool,
    request_id: &str,
) -> Response {
    let fetches = parse_item_operations_fetches(xml);
    if fetches.is_empty() {
        return bad_request_response(request_id, "ItemOperations requires at least one Fetch");
    }

    let caldav = CaldavClient::new(&state.cfg);
    let mut responses = String::new();
    for fetch in fetches {
        let store = if fetch.store.is_empty() {
            "Mailbox".to_string()
        } else {
            fetch.store
        };
        let collection_id = fetch.collection_id.unwrap_or_else(|| "1".to_string());
        let Some(server_id) = fetch.server_id.or(fetch.long_id) else {
            responses.push_str(&format!(
                "<Fetch><Store>{}</Store><Status>6</Status></Fetch>",
                sync::xml_escape(&store)
            ));
            continue;
        };

        let lookup = match state
            .storage
            .get_ews_item_by_server_id(username, &server_id)
            .await
        {
            Ok(Some(row)) => row,
            _ => {
                responses.push_str(&format!(
                    "<Fetch><Store>{}</Store><CollectionId>{}</CollectionId><ServerId>{}</ServerId><Status>8</Status></Fetch>",
                    sync::xml_escape(&store),
                    sync::xml_escape(&collection_id),
                    sync::xml_escape(&server_id)
                ));
                continue;
            }
        };

        let Ok((ics, _etag)) = caldav
            .get_event(&lookup.resource_href, username, password)
            .await
        else {
            responses.push_str(&format!(
                "<Fetch><Store>{}</Store><CollectionId>{}</CollectionId><ServerId>{}</ServerId><Status>8</Status></Fetch>",
                sync::xml_escape(&store),
                sync::xml_escape(&collection_id),
                sync::xml_escape(&server_id)
            ));
            continue;
        };
        let Some(item) = parse_ics_event(&ics) else {
            responses.push_str(&format!(
                "<Fetch><Store>{}</Store><CollectionId>{}</CollectionId><ServerId>{}</ServerId><Status>6</Status></Fetch>",
                sync::xml_escape(&store),
                sync::xml_escape(&collection_id),
                sync::xml_escape(&server_id)
            ));
            continue;
        };
        responses.push_str(&format!(
            "<Fetch><Store>{}</Store><CollectionId>{}</CollectionId><ServerId>{}</ServerId><Class>Calendar</Class><Status>1</Status><Properties>{}</Properties></Fetch>",
            sync::xml_escape(&store),
            sync::xml_escape(&collection_id),
            sync::xml_escape(&server_id),
            sync::render_calendar_app_data(&item)
        ));
    }

    let response = format!(
        r#"<?xml version="1.0" encoding="utf-8"?><ItemOperations xmlns="ItemOperations:" xmlns:Calendar="Calendar:" xmlns:AirSyncBase="AirSyncBase:"><Status>1</Status><Response>{}</Response></ItemOperations>"#,
        responses
    );
    xml_or_wbxml_response(wbxml, as_wbxml, &response, request_id)
}

async fn handle_search(
    state: &Arc<AppState>,
    username: &str,
    password: &str,
    xml: &str,
    wbxml: &Wbxml,
    as_wbxml: bool,
    request_id: &str,
) -> Response {
    let req = parse_search_request(xml);
    if !req.store_name.eq_ignore_ascii_case("Mailbox") {
        let response = r#"<?xml version="1.0" encoding="utf-8"?><Search xmlns="Search:"><Status>1</Status><Response><Store><Status>11</Status></Store></Response></Search>"#;
        return xml_or_wbxml_response(wbxml, as_wbxml, response, request_id);
    }

    let start = req
        .starts
        .unwrap_or_else(|| chrono::Utc::now() - chrono::Duration::weeks(52));
    let end = req
        .ends
        .unwrap_or_else(|| chrono::Utc::now() + chrono::Duration::weeks(52));
    let Ok(events) = load_calendar_events(state, username, password, start, end).await else {
        let response = r#"<?xml version="1.0" encoding="utf-8"?><Search xmlns="Search:"><Status>1</Status><Response><Store><Status>6</Status></Store></Response></Search>"#;
        return xml_or_wbxml_response(wbxml, as_wbxml, response, request_id);
    };

    let mut matches = events
        .into_iter()
        .filter(|(_, _, item)| matches_search(item, req.query_text.as_deref()))
        .collect::<Vec<_>>();
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
        .map(|(server_id, _, item)| {
            format!(
                "<Result><Class>Calendar</Class><CollectionId>1</CollectionId><LongId>{}</LongId><Properties>{}</Properties></Result>",
                sync::xml_escape(server_id),
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
        sync::xml_escape(&range_xml),
        total,
        results_xml
    );
    xml_or_wbxml_response(wbxml, as_wbxml, &response, request_id)
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
            .and_then(|token| token.strip_prefix("seq:"))
            .and_then(|v| v.parse::<i64>().ok())
            .unwrap_or(0)
    };
    let changed = state
        .storage
        .list_changes_since_seq(owner, since)
        .await
        .unwrap_or_default();
    let deleted = state
        .storage
        .list_deleted_since_seq(owner, since)
        .await
        .unwrap_or_default();
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
            let state_collection_id = scoped_collection_id(
                collection_id,
                req.device_id.as_deref().unwrap_or("unknown-device"),
            );
            let incoming_key = req.sync_key.as_deref().unwrap_or("0");
            let class = req.class.as_deref().unwrap_or("Calendar");
            let mut mutation_responses = String::new();

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
                    &password,
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
                &mutation_responses,
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
        "Settings" => handle_settings(&username, &wbxml, wants_wbxml, &request_id).await,
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
        "ItemOperations" => {
            handle_item_operations(
                &state,
                &username,
                &password,
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
                &password,
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
                        r#"<?xml version="1.0" encoding="utf-8"?><MeetingResponse xmlns="MeetingResponse:"><Result><RequestId>{}</RequestId><CalendarId>{}</CalendarId><Status>1</Status></Result></MeetingResponse>"#,
                        sync::xml_escape(&req_id),
                        sync::xml_escape(&req_id)
                    );
                    xml_or_wbxml_response(&wbxml, wants_wbxml, &payload, &request_id)
                }
            } else {
                bad_request_response(&request_id, "MeetingResponse requires RequestId")
            }
        }
        "ResolveRecipients" => {
            handle_resolve_recipients(
                &state,
                &username,
                &password,
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

#[cfg(test)]
mod tests {
    use super::{
        command_from_query, extract_all_tag_text, extract_first_tag_text, extract_root_command,
        parse_item_operations_fetches, parse_ping_folders, parse_search_request, validate_payload,
    };
    use crate::calendar::{parse_datetime, Attendee, CalendarItem};
    use chrono::{TimeZone, Utc};
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
    fn validates_sync_add_requires_client_id() {
        let xml = r#"<Sync xmlns="AirSync:"><Collections><Collection><CollectionId>1</CollectionId><SyncKey>1</SyncKey><Class>Calendar</Class><Commands><Add><ApplicationData /></Add></Commands></Collection></Collections></Sync>"#;
        assert!(validate_payload("Sync", xml).is_err());
    }

    #[test]
    fn validates_sync_rejects_response_only_calendar_fields() {
        let xml = r#"<Sync xmlns="AirSync:"><Collections><Collection><CollectionId>1</CollectionId><SyncKey>1</SyncKey><Class>Calendar</Class><Commands><Change><ServerId>abc</ServerId><ApplicationData><Calendar:AppointmentReplyTime xmlns:Calendar="Calendar:">20260322T120000Z</Calendar:AppointmentReplyTime></ApplicationData></Change></Commands></Collection></Collections></Sync>"#;
        assert!(validate_payload("Sync", xml).is_err());
    }

    #[test]
    fn validates_meeting_response_requires_user_response() {
        let xml = r#"<MeetingResponse xmlns="MeetingResponse:"><Request><RequestId>abc</RequestId></Request></MeetingResponse>"#;
        assert!(validate_payload("MeetingResponse", xml).is_err());
    }

    #[test]
    fn parses_item_operations_fetches() {
        let xml = r#"<ItemOperations xmlns="ItemOperations:"><Fetch><Store>Mailbox</Store><CollectionId>1</CollectionId><ServerId>srv-1</ServerId></Fetch><Fetch><Store>Mailbox</Store><LongId>srv-2</LongId></Fetch></ItemOperations>"#;
        let fetches = parse_item_operations_fetches(xml);
        assert_eq!(fetches.len(), 2);
        assert_eq!(fetches[0].server_id.as_deref(), Some("srv-1"));
        assert_eq!(fetches[1].long_id.as_deref(), Some("srv-2"));
    }

    #[test]
    fn parses_search_range_and_window() {
        let xml = r#"<Search xmlns="Search:"><Store><Name>Mailbox</Name><Query>project</Query><Options><Range>3-7</Range><DeepTraversal/></Options></Store></Search>"#;
        let req = parse_search_request(xml);
        assert_eq!(req.store_name, "Mailbox");
        assert_eq!(req.query_text.as_deref(), Some("project"));
        assert_eq!((req.range_start, req.range_end), (3, 7));
    }

    #[test]
    fn extracts_multiple_tag_values() {
        let xml = r#"<Settings xmlns="Settings:"><SmtpAddress>a@example.com</SmtpAddress><SmtpAddress>b@example.com</SmtpAddress></Settings>"#;
        assert_eq!(
            extract_all_tag_text(xml, b"SmtpAddress"),
            vec!["a@example.com".to_string(), "b@example.com".to_string()]
        );
    }

    #[test]
    fn matches_search_across_subject_and_attendees() {
        let item = CalendarItem {
            subject: "Project sync".to_string(),
            attendees: vec![Attendee {
                email: "teammate@example.com".to_string(),
                ..Default::default()
            }],
            start: Utc.with_ymd_and_hms(2026, 3, 22, 10, 0, 0).unwrap(),
            end: Utc.with_ymd_and_hms(2026, 3, 22, 11, 0, 0).unwrap(),
            ..Default::default()
        };
        assert!(super::matches_search(&item, Some("project")));
        assert!(super::matches_search(&item, Some("teammate")));
        assert!(!super::matches_search(&item, Some("finance")));
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
    #[test]
    fn extracts_multiple_resolve_recipient_targets() {
        let xml = r#"<ResolveRecipients xmlns="ResolveRecipients:"><To>one@example.com</To><To>two@example.com</To></ResolveRecipients>"#;
        assert_eq!(
            extract_all_tag_text(xml, b"To"),
            vec!["one@example.com", "two@example.com"]
        );
    }

    #[test]
    fn parses_ping_folder_entries() {
        let xml = r#"<Ping xmlns="Ping:"><Folders><Folder><Id>1</Id><Class>Calendar</Class></Folder></Folders></Ping>"#;
        let folders = parse_ping_folders(xml);
        assert_eq!(folders.len(), 1);
        assert_eq!(folders[0].id, "1");
        assert_eq!(folders[0].class_name, "Calendar");
    }
    #[test]
    fn resolve_recipients_command_detected() {
        let xml = r#"<ResolveRecipients xmlns="ResolveRecipients:"><To>a@example.com</To><Options><Availability><StartTime>2026-03-21T00:00:00Z</StartTime><EndTime>2026-03-22T00:00:00Z</EndTime></Availability></Options></ResolveRecipients>"#;
        assert_eq!(
            super::extract_root_command(xml).as_deref(),
            Some("ResolveRecipients")
        );
    }
}
