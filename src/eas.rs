// src/eas.rs
use crate::caldav::CaldavClient;
use crate::calendar::{parse_datetime, parse_ics_event};

use crate::error::GatewayError as Error;
use crate::jmap::{JmapClient, QueryCalendarEventsParams};
use crate::models::AppState;
use crate::permission::{PermissionContext, PermissionEnforcement};
use crate::sync::{self, SyncOptions, filter_type_to_start};
use crate::util::{canonicalize_username, nfc, normalize_username, xml_escape};
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
use chrono::Duration as ChronoDuration;
use futures_util::future::join_all;
use lru::LruCache;
use quick_xml::Reader;
use quick_xml::events::Event;
use roxmltree::Document;
use secrecy::{ExposeSecret, SecretString};
use std::collections::HashMap;
use std::num::NonZeroUsize;
use std::sync::{Arc, LazyLock};
use std::time::{Duration as StdDuration, Instant};
use subtle::ConstantTimeEq;
use tokio::sync::Mutex as TokioMutex;
use tokio::time::timeout;
use uuid::Uuid;

const MAX_FREEBUSY_DAYS: i64 = 30;
const MAX_BODY_SIZE: usize = 1_048_576;
const CALDAV_TIMEOUT: StdDuration = StdDuration::from_secs(30);
const MAX_PING_CACHE_ENTRIES: usize = 10_000;
/// Maximum number of emails to fetch in a single JMAP Email/query for EAS initial sync.
/// Matches sync::DEFAULT_WINDOW_SIZE; keeps requests fast and memory-bounded.
const EMAIL_SYNC_PAGE_SIZE: u64 = 100;

type PingCache = LruCache<String, PingCacheEntry>;

const _: () = assert!(MAX_PING_CACHE_ENTRIES > 0);

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

/// The content class of a folder a client subscribes to via EAS Ping.
///
/// Used to scope change-journal polling correctly: Tasks/Notes are gateway-local
/// (`resource_href` tags "task"/"note"), while Calendar/Email/Contacts flow through
/// `item_map`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PingFolderKind {
    Calendar,
    Email,
    Contacts,
    Tasks,
    Notes,
}

/// Classify a Ping folder by its collection id (falling back to class name).
///
/// Returns `None` for unknown folders, which `handle_ping` rejects with Status 7.
fn ping_folder_kind(folder: &PingFolder) -> Option<PingFolderKind> {
    let id = folder.id.as_str();
    match id {
        "1" => Some(PingFolderKind::Calendar),
        "2" | "3" | "4" | "5" | "6" | "12" => Some(PingFolderKind::Email),
        "8" => Some(PingFolderKind::Contacts),
        crate::tasks::TASKS_COLLECTION_ID => Some(PingFolderKind::Tasks),
        crate::tasks::NOTES_COLLECTION_ID => Some(PingFolderKind::Notes),
        _ => {
            // Fall back to the class name when the id is non-standard.
            match folder.class_name.to_ascii_lowercase().as_str() {
                "calendar" => Some(PingFolderKind::Calendar),
                "email" => Some(PingFolderKind::Email),
                "contacts" => Some(PingFolderKind::Contacts),
                "tasks" => Some(PingFolderKind::Tasks),
                "notes" => Some(PingFolderKind::Notes),
                _ => None,
            }
        }
    }
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

/// A single Collection element within a Sync request.
/// Per MS-ASCMD §2.2.3.31.2, a Sync request can contain multiple
/// Collection elements inside the Collections container, allowing
/// the client to synchronize multiple folders in one request.
#[derive(Clone, Debug)]
struct SyncCollection {
    sync_key: Option<String>,
    collection_id: Option<String>,
    class: Option<String>,
    window_size: Option<usize>,
    /// Per MS-ASCMD §2.2.3.72, GetChanges defaults to true when absent.
    get_changes: bool,
    filter_type: Option<u8>,
    /// The raw XML substring of this <Collection> element, used for
    /// mutation checks and apply_client_sync_mutations instead of
    /// the full request XML. This prevents cross-collection mutation
    /// leakage (e.g., Email <Add> being applied to Calendar collection).
    xml: String,
}

impl Default for SyncCollection {
    fn default() -> Self {
        Self {
            sync_key: None,
            collection_id: None,
            class: None,
            window_size: None,
            // Per MS-ASCMD §2.2.3.72, GetChanges is optional and
            // defaults to true (client wants server changes).
            get_changes: true,
            filter_type: None,
            xml: String::new(),
        }
    }
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
            required_tags: &[], // Accept both single-collection and multi-collection structures; validated in validate_payload.
            _optional_tags: &[
                "SyncKey",
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
        "sendmail" => Some(CommandGrammar {
            namespace: "SendMail:",
            required_tags: &[],
            _optional_tags: &["ClientId", "SaveInSentItems", "MIMEData"],
        }),
        "smartreply" => Some(CommandGrammar {
            namespace: "SmartReply:",
            required_tags: &[],
            _optional_tags: &[
                "ClientId",
                "SaveInSentItems",
                "SourceMessageId",
                "SourceFolderId",
                "MIMEData",
            ],
        }),
        "smartforward" => Some(CommandGrammar {
            namespace: "SmartForward:",
            required_tags: &[],
            _optional_tags: &[
                "ClientId",
                "SaveInSentItems",
                "SourceMessageId",
                "SourceFolderId",
                "MIMEData",
            ],
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

    // Helper to check if an element with the given local name appears in the XML.
    // This is more robust than naive string search (avoids false positives like <FooBar> or comments).
    fn element_exists(xml: &str, local_name: &str) -> bool {
        let mut reader = Reader::from_str(xml);
        reader.config_mut().trim_text(true);
        let mut buf = Vec::new();
        loop {
            match reader.read_event_into(&mut buf) {
                Ok(Event::Start(e)) | Ok(Event::Empty(e))
                    if e.name().local_name().as_ref() == local_name.as_bytes() =>
                {
                    return true;
                }
                Ok(Event::Eof) => return false,
                Ok(_) => {} // ignore other event types (Text, End, CData, etc.)
                Err(_) => return false,
            }
        }
    }

    // Validate required tags. For container elements (e.g., <Collections>), we only need to check for
    // the presence of the opening tag, as they contain nested elements and have no text content.
    // For non-container elements, we require non-empty text content (checked later by handler).
    for &required in grammar.required_tags {
        if !element_exists(xml, required) {
            return Err("Missing required tag");
        }
    }

    match lower_cmd.as_str() {
        "sync" => {
            // Custom validation for sync: accept either multi-collection (<Collections>) or legacy single-collection.
            let mut reader = Reader::from_str(xml);
            reader.config_mut().trim_text(true);
            let mut buf = Vec::new();
            let mut in_collections = false;
            let mut found_collection = false;
            loop {
                match reader.read_event_into(&mut buf) {
                    Ok(Event::Start(e)) => {
                        let name = e.name().local_name();
                        if name.as_ref() == b"Collections" {
                            in_collections = true;
                        } else if in_collections && name.as_ref() == b"Collection" {
                            found_collection = true;
                        }
                    }
                    Ok(Event::End(e)) => {
                        if e.name().local_name().as_ref() == b"Collections" {
                            in_collections = false;
                        }
                    }
                    Ok(Event::Eof) => break,
                    Ok(_) => {} // ignore all other event types (Empty, Text, CData, etc.)
                    Err(_) => return Err("Invalid XML in sync request"),
                }
                buf.clear();
            }
            if !found_collection {
                return Err("Missing required Collection element inside Collections");
            }

            // Add requires ClientId (per MS-ASCAL §2.2.3.22). Use element presence detection.
            if element_exists(xml, "Add") && !element_exists(xml, "ClientId") {
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

/// Parse all Collection elements from a Sync request.
///
/// Per MS-ASCMD §2.2.3.31.2, a Sync request can contain 1..N Collection
/// elements inside the Collections container. Android clients (including
/// Gmail's Exchange account) send multi-collection Sync requests to
/// synchronize calendar and email folders in a single round-trip.
///
/// Each Collection contains its own SyncKey, CollectionId, Class,
/// WindowSize, FilterType, and GetChanges. The response MUST contain
/// a corresponding Collection element for each request Collection.
///
/// Parse all Collection elements from a Sync request.
///
/// Per MS-ASCMD §2.2.3.31.2, a Sync request can contain 1..N Collection
/// elements inside the Collections container. Android clients (including
/// Gmail's Exchange account) send multi-collection Sync requests to
/// synchronize calendar and email folders in a single round-trip.
///
/// Each Collection contains its own SyncKey, CollectionId, Class,
/// WindowSize, FilterType, and GetChanges. The response MUST contain
/// a corresponding Collection element for each request Collection.
///
/// The raw XML of each `<Collection>` element is captured using
/// `reader.buffer_position()` to obtain accurate byte offsets in the original
/// request string, preventing cross-collection mutation leakage when
/// checking permissions and applying mutations.
///
/// This parser tracks the nesting depth inside the current Collection to
/// ensure that only *direct child* elements are interpreted as collection-level
/// fields. This prevents <Add>/<Change> commands inside <Commands> from
/// contaminating the field values (e.g., an <Add><Class>...</Class></Add>
/// will not overwrite the collection's Class). It also handles CDATA
/// sections in the same way as text events, so field values wrapped in CDATA
/// are not silently lost.
fn parse_sync_collections(xml: &str) -> Vec<SyncCollection> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);
    let mut buf = Vec::new();
    let mut collections = Vec::new();
    let mut current_collection: Option<SyncCollection> = None;
    let mut collection_start: Option<u64> = None;
    // Depth counter: 0 = not inside a Collection, 1 = inside the Collection element but not in a child, >1 = nested deeper.
    let mut depth: usize = 0;
    // Current tag name when we are at depth 1 (direct child of Collection).
    let mut current_tag: Option<Vec<u8>> = None;

    loop {
        let start_pos = reader.buffer_position();
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) => {
                let name = e.name().local_name();
                if name.as_ref() == b"Collection" {
                    // Entering a new Collection element.
                    collection_start = Some(start_pos);
                    current_collection = Some(SyncCollection::default());
                    depth = 1;
                    current_tag = None;
                } else if depth == 1 && current_collection.is_some() {
                    // Direct child of Collection: track its tag name for potential text capture.
                    current_tag = Some(name.as_ref().to_vec());
                    depth = 2;
                } else if depth >= 2 {
                    // Nested further inside; increment depth.
                    depth += 1;
                }
            }
            Ok(Event::Text(t)) => {
                if depth == 1 {
                    // Should not happen: text directly inside Collection is not expected.
                    // But we ignore it.
                } else if depth == 2 {
                    // Text content of a direct child element.
                    if let Some(tag) = current_tag.as_ref()
                        && let Ok(text) = t.decode()
                        && let Some(coll) = current_collection.as_mut()
                    {
                        let text = text.into_owned();
                        match tag.as_slice() {
                            b"SyncKey" => coll.sync_key = Some(text),
                            b"CollectionId" => coll.collection_id = Some(text),
                            b"Class" => coll.class = Some(text),
                            b"WindowSize" => coll.window_size = text.parse().ok(),
                            b"FilterType" => coll.filter_type = text.parse().ok(),
                            b"GetChanges" => coll.get_changes = text.trim() != "0",
                            _ => {}
                        }
                    }
                }
                // For depth > 2 we ignore text inside deeper nested structures.
            }
            Ok(Event::CData(cdata)) => {
                // Treat CDATA exactly like Text; decode to String.
                if depth == 2
                    && let Some(tag) = current_tag.as_ref()
                    && let Ok(text) = cdata.decode()
                    && let Some(coll) = current_collection.as_mut()
                {
                    let text = text.into_owned();
                    match tag.as_slice() {
                        b"SyncKey" => coll.sync_key = Some(text),
                        b"CollectionId" => coll.collection_id = Some(text),
                        b"Class" => coll.class = Some(text),
                        b"WindowSize" => coll.window_size = text.parse().ok(),
                        b"FilterType" => coll.filter_type = text.parse().ok(),
                        b"GetChanges" => coll.get_changes = text.trim() != "0",
                        _ => {}
                    }
                }
            }
            Ok(Event::End(e)) => {
                if e.name().local_name().as_ref() == b"Collection" {
                    if let Some(mut coll) = current_collection.take() {
                        // After reading the End event, buffer_position() is just after the closing '>'
                        let end_pos = reader.buffer_position();
                        if let Some(start) = collection_start.take() {
                            let start_idx = start as usize;
                            let end_idx = end_pos as usize;
                            if start_idx < end_idx && end_idx <= xml.len() {
                                coll.xml = xml[start_idx..end_idx].to_string();
                            } else {
                                tracing::warn!(
                                    "parse_sync_collections: invalid slice start={}, end={}, xml_len={}",
                                    start_idx,
                                    end_idx,
                                    xml.len()
                                );
                            }
                        }
                        collections.push(coll);
                    }
                    depth = 0;
                    current_tag = None;
                } else if depth == 2 {
                    // Closing a direct child element of Collection.
                    current_tag = None;
                    depth = 1;
                } else if depth > 2 {
                    depth -= 1;
                }
            }
            Ok(Event::Eof) => break,
            Err(e) => {
                tracing::debug!("parse_sync_collections: XML error: {}", e);
                break;
            }
            _ => {}
        }
        buf.clear();
    }

    collections
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

/// Build the list of active email addresses for the given user.
///
/// Always uses `mail_domain` as the email domain, extracting only the local
/// part from `username`. This ensures the primary SMTP address matches
/// GATEWAY_MAIL_DOMAIN regardless of the domain the client supplied during
/// authentication (e.g. `contact@exchange.com` → `contact@example.com`).
fn active_user_emails(username: &str, mail_domain: &str) -> Vec<String> {
    crate::util::user_primary_email(username, mail_domain)
        .map(|e| vec![e])
        .unwrap_or_default()
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

/// Pre-validated Bearer challenge header value. Uses `from_static` to avoid
/// per-request heap allocation and string parsing — the value is a compile-time
/// constant that must never change.
///
/// Per MS-XOAUTH §4.1 and the Outlook for iOS/Android hybrid modern auth
/// documentation, on-premises Exchange Server returns a Bearer challenge with
/// three parameters:
///
/// - `client_id`: The well-known Exchange ActiveSync application ID in
///   Microsoft Entra ID (`00000002-0000-0ff1-ce00-000000000000`).
/// - `trusted_issuers`: The well-known Microsoft STS issuer GUID with wildcard
///   tenant (`00000001-0001-0000-c000-000000000000@*`), meaning "trust all
///   Microsoft Entra ID tenants".
/// - `authorization_uri`: The common Microsoft Entra ID OAuth 2.0
///   authorization endpoint (`https://login.microsoftonline.com/common/oauth2/authorize`).
///
/// Microsoft's AutoDetect cloud service requires the `authorization_uri`
/// parameter to recognise the endpoint as a valid ActiveSync server. Without
/// it, AutoDetect reports "missing authorization URL" and falls back to IMAP,
/// making the calendar unusable in Outlook for iOS/Android.
///
/// The gateway only supports Basic authentication. The Bearer header is
/// included solely for AutoDetect discovery compatibility. When a client
/// attempts Bearer auth, `parse_basic_auth()` rejects it and the client
/// falls back to Basic.
const BEARER_WWW_AUTHENTICATE: HeaderValue = HeaderValue::from_static(concat!(
    "Bearer ",
    "client_id=\"",
    "00000002-0000-0ff1-ce00-000000000000",
    "\", ",
    "trusted_issuers=\"",
    "00000001-0001-0000-c000-000000000000@*",
    "\", ",
    "authorization_uri=\"",
    "https://login.microsoftonline.com/common/oauth2/authorize",
    "\""
));

/// The well-known Exchange ActiveSync application ID embedded in
/// [`BEARER_WWW_AUTHENTICATE`]. Exposed for test assertions only.
#[cfg(test)]
const EXCHANGE_ACTIVESYNC_CLIENT_ID: &str = "00000002-0000-0ff1-ce00-000000000000";

/// The well-known Microsoft STS issuer embedded in
/// [`BEARER_WWW_AUTHENTICATE`]. Exposed for test assertions only.
#[cfg(test)]
const TRUSTED_ISSUERS: &str = "00000001-0001-0000-c000-000000000000@*";

/// The Microsoft Entra ID OAuth 2.0 authorization endpoint embedded in
/// [`BEARER_WWW_AUTHENTICATE`]. Exposed for test assertions only.
#[cfg(test)]
const AUTHORIZATION_URI: &str = "https://login.microsoftonline.com/common/oauth2/authorize";

fn unauth_response(request_id: &str) -> Response {
    // Return both Bearer and Basic WWW-Authenticate headers.
    //
    // Microsoft's AutoDetect cloud service (prod-autodetect.outlookmobile.com)
    // probes the ActiveSync endpoint with an empty Bearer challenge to determine
    // whether the server is compatible with Outlook mobile. The Bearer header
    // MUST include `authorization_uri` — without it, AutoDetect reports
    // "missing authorization URL" and falls back to IMAP, making the calendar
    // unusable in Outlook for iOS/Android.
    //
    // Per MS-XOAUTH §4.1, on-premises Exchange Server returns:
    //   WWW-Authenticate: Bearer client_id="00000002-0000-0ff1-ce00-000000000000",
    //     trusted_issuers="00000001-0001-0000-c000-000000000000@*",
    //     authorization_uri="https://login.microsoftonline.com/common/oauth2/authorize"
    //   WWW-Authenticate: Basic realm="..."
    //
    // The gateway only supports Basic authentication. The Bearer header is
    // included solely for AutoDetect discovery compatibility. When a client
    // actually attempts Bearer auth, parse_basic_auth() rejects it and this
    // 401 is returned again; the client then falls back to Basic.
    let mut r = (
        StatusCode::UNAUTHORIZED,
        [(
            header::WWW_AUTHENTICATE.as_str(),
            "Basic realm=\"Microsoft-Server-ActiveSync\"",
        )],
        "Unauthorized",
    )
        .into_response();
    r.headers_mut()
        .append(header::WWW_AUTHENTICATE, BEARER_WWW_AUTHENTICATE.clone());
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
                "Sync,FolderSync,Provision,MeetingResponse,Settings,Ping,ItemOperations,Search,ResolveRecipients,GetItemEstimate,ValidateCert,SendMail,SmartReply,SmartForward",
            ),
        ],
        "",
    )
        .into_response();
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
            Err(e) => {
                // Log detailed error to diagnose 500s
                let preview = if xml.len() > 500 { &xml[..500] } else { xml };
                tracing::error!(
                    request_id = %request_id,
                    error = %e,
                    xml_len = xml.len(),
                    preview = %preview,
                    "WBXML encode failed"
                );
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    [(header::CONTENT_TYPE.as_str(), "text/plain; charset=utf-8")],
                    format!("WBXML Encode Err: {}", e).into_bytes(),
                )
                    .into_response()
            }
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
    r#"<Settings xmlns="Settings:">
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
<Calendar>
  <CalendarSyncEnabled>1</CalendarSyncEnabled>
</Calendar>
</Settings>"#
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
        // Per MS-ASCMD §2.2.3.186.3 (FolderSync Type), the default-folder Type
        // values are: 2=Inbox, 3=Drafts, 4=Deleted Items, 5=Sent Items,
        // 6=Outbox, 7=Tasks, 8=Calendar, 9=Contacts, 10=Notes, 12=User mail.
        //
        // Only include email folders when email is actually available, otherwise
        // clients will attempt to sync them and hit errors. Contacts are gated on
        // CardDAV availability for the same reason. Calendar, Tasks and Notes are
        // always present (Calendar via JMAP/CalDAV, Tasks/Notes gateway-local).
        let can_read_email = state.can_read_email();
        let can_read_contacts = state.can_read_contacts();

        let email_folders = if can_read_email {
            crate::email::eas_email_folders_xml()
        } else {
            String::new()
        };
        let contacts_folder = if can_read_contacts {
            r#"<Add><ServerId>8</ServerId><ParentId>0</ParentId><DisplayName>Contacts</DisplayName><Type>9</Type></Add>"#
        } else {
            ""
        };

        // Calendar(1) + Tasks(1) + Notes(1) [+ Contacts(1)] [+ 6 email folders].
        let count = 3 + usize::from(can_read_contacts) + if can_read_email { 6 } else { 0 };
        format!(
            r#"<Changes><Count>{count}</Count>
<Add><ServerId>1</ServerId><ParentId>0</ParentId><DisplayName>Calendar</DisplayName><Type>8</Type></Add>
<Add><ServerId>7</ServerId><ParentId>0</ParentId><DisplayName>Tasks</DisplayName><Type>7</Type></Add>
<Add><ServerId>10</ServerId><ParentId>0</ParentId><DisplayName>Notes</DisplayName><Type>10</Type></Add>
{contacts_folder}
{email_folders}
</Changes>"#
        )
    } else {
        r#"<Changes><Count>0</Count></Changes>"#.to_string()
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
    if folders.iter().any(|f| ping_folder_kind(f).is_none()) {
        // Status 7: one or more folders are not valid for Ping (unknown type).
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

    let deadline = Instant::now() + StdDuration::from_secs(heartbeat);
    while Instant::now() < deadline {
        let mut changed_folders = Vec::new();
        for folder in &folders {
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

            let changed = match ping_folder_kind(folder) {
                // Tasks and Notes are gateway-local stores journaled with a
                // distinct resource_href tag ("task" / "note"), so we can scope
                // change detection precisely to those folders.
                Some(PingFolderKind::Tasks) => state
                    .storage
                    .list_journal_since_seq(owner, since)
                    .await
                    .unwrap_or_default()
                    .iter()
                    .any(|r| matches!(r.resource_href.as_deref(), Some("task"))),
                Some(PingFolderKind::Notes) => state
                    .storage
                    .list_journal_since_seq(owner, since)
                    .await
                    .unwrap_or_default()
                    .iter()
                    .any(|r| matches!(r.resource_href.as_deref(), Some("note"))),
                // Email, Calendar and Contacts changes flow through item_map and
                // the shared change journal (op='upsert') plus delete tombstones.
                Some(PingFolderKind::Calendar)
                | Some(PingFolderKind::Email)
                | Some(PingFolderKind::Contacts) => {
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
                    !changed.is_empty() || !deleted.is_empty()
                }
                None => false,
            };

            if changed {
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
        tokio::time::sleep(remaining.min(StdDuration::from_secs(15))).await;
    }

    // Timeout reached with no changes
    let xml =
        r#"<?xml version="1.0" encoding="utf-8"?><Ping xmlns="Ping:"><Status>1</Status></Ping>"#;
    xml_or_wbxml_response(wbxml, as_wbxml, xml, request_id)
}

async fn handle_settings(
    state: &Arc<AppState>,
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
    let has_calendar = xml_body.contains("<Calendar>") || xml_body.contains("<Calendar/>");

    // Handle Set - OOF operations if present.
    let oof_inner = {
        if let Some(start) = xml_body.find("<Oof>") {
            let start_idx = start + 5; // after "<Oof>"
            if let Some(end) = xml_body[start_idx..].find("</Oof>") {
                &xml_body[start_idx..start_idx + end]
            } else {
                &xml_body[start_idx..]
            }
        } else {
            ""
        }
    };
    if !oof_inner.is_empty() {
        // We expect a full Set - Oof block. For simplicity, parse required fields.
        let oof_state_text =
            extract_first_tag_text(oof_inner, b"OofState").unwrap_or("0".to_string());
        let enabled = oof_state_text == "1";
        let external_audience_text =
            extract_first_tag_text(oof_inner, b"ExternalAudience").unwrap_or("2".to_string());
        let external_audience = match external_audience_text.as_str() {
            "0" => crate::oof::ExternalAudience::External,
            "1" => crate::oof::ExternalAudience::KnownExternal,
            _ => crate::oof::ExternalAudience::All,
        };
        let start_time = extract_first_tag_text(oof_inner, b"StartTime")
            .and_then(|s| chrono::DateTime::parse_from_rfc3339(&s).ok())
            .map(|dt| dt.with_timezone(&chrono::Utc));
        let end_time = extract_first_tag_text(oof_inner, b"EndTime")
            .and_then(|s| chrono::DateTime::parse_from_rfc3339(&s).ok())
            .map(|dt| dt.with_timezone(&chrono::Utc));
        let internal_reply = extract_first_tag_text(oof_inner, b"InternalReply");
        let external_reply = extract_first_tag_text(oof_inner, b"ExternalReply");

        let settings = crate::oof::OofSettings {
            enabled,
            external_audience,
            internal_reply,
            external_reply,
            start_time,
            end_time,
        };

        if let Some(oof_mgr) = &state.oof_manager {
            let _ = oof_mgr.set_oof_settings(username, settings);
            // Ignore result; we'll still respond success.
        }
    }

    if has_user_info || (!has_oof && !has_device_password) {
        let email_entries = active_user_emails(username, &state.cfg.mail_domain)
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
        // Query OOF settings for this user.
        let oof_section = if let Some(oof_mgr) = &state.oof_manager {
            match oof_mgr.get_oof_settings(username) {
                Ok(settings) => {
                    let state_val = if settings.enabled { "1" } else { "0" };
                    // External audience: 0=External, 1=KnownExternal, 2=All. In absence of specific mapping, use 2.
                    let external = match settings.external_audience {
                        crate::oof::ExternalAudience::External => "0",
                        crate::oof::ExternalAudience::KnownExternal => "1",
                        crate::oof::ExternalAudience::All => "2",
                    };
                    // Duration: if set, include; else empty.
                    let duration = if let (Some(start), Some(end)) =
                        (settings.start_time, settings.end_time)
                    {
                        format!(
                            "<StartTime>{}</StartTime><EndTime>{}</EndTime>",
                            start.to_rfc3339(),
                            end.to_rfc3339()
                        )
                    } else {
                        String::new()
                    };
                    // Replies: escape them.
                    let internal_reply =
                        xml_escape(settings.internal_reply.as_deref().unwrap_or(""));
                    let external_reply =
                        xml_escape(settings.external_reply.as_deref().unwrap_or(""));

                    format!(
                        r#"<Oof>
    <Status>1</Status>
    <Get>
      <OofState>{}</OofState>
      <ExternalAudience>{}</ExternalAudience>
      {}
      <InternalReply>{}</InternalReply>
      <ExternalReply>{}</ExternalReply>
      <AllowExternalOof>true</AllowExternalOof>
    </Get>
  </Oof>"#,
                        state_val, external, duration, internal_reply, external_reply
                    )
                }
                Err(e) => {
                    tracing::warn!(target: "eas", user = %username, error = %e, "Failed to get OOF settings");
                    // Return OOF disabled in error case.
                    r#"<Oof>
    <Status>1</Status>
    <Get>
      <OofState>0</OofState>
      <ExternalAudience>2</ExternalAudience>
      <AllowExternalOof>true</AllowExternalOof>
    </Get>
  </Oof>"#
                        .to_string()
                }
            }
        } else {
            // No manager configured; return OOF disabled.
            r#"<Oof>
    <Status>1</Status>
    <Get>
      <OofState>0</OofState>
      <ExternalAudience>2</ExternalAudience>
      <AllowExternalOof>true</AllowExternalOof>
    </Get>
  </Oof>"#
                .to_string()
        };

        sections.push_str(&oof_section);
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
    if has_calendar {
        sections.push_str(
            r#"<Calendar>
    <Status>1</Status>
    <Get>
      <CalendarSyncEnabled>1</CalendarSyncEnabled>
    </Get>
  </Calendar>"#,
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

        // Try JMAP Calendar first if enabled and item is JMAP-backed
        let mut jmap_ics: Option<String> = None;
        if state.cfg.prefer_jmap_calendar
            && let Some(jmap) = &state.jmap_client
            && lookup.resource_href.starts_with("jmap://")
        {
            // Parse: jmap://calendar/{account_id}/{event_id}
            let after = lookup.resource_href.trim_start_matches("jmap://calendar/");
            let parts: Vec<&str> = after.split('/').collect();
            if parts.len() == 2 {
                let account_id = parts[0];
                let event_id = parts[1];
                match jmap
                    .get_calendar_event(account_id, event_id, &owner_lower, &password)
                    .await
                {
                    Ok((ics, _event_id, _etag)) => {
                        jmap_ics = Some(ics);
                    }
                    Err(e) => {
                        tracing::debug!(error = %e, "ItemOperations: JMAP fetch failed, falling back to CalDAV");
                    }
                }
            }
        }

        // Fetch from CalDAV if JMAP didn't yield data
        let (ics, _etag) = if let Some(ics) = jmap_ics {
            (ics, None)
        } else {
            let get_future = caldav.get_event(
                &lookup.resource_href,
                &owner_lower,
                password.expose_secret(),
            );
            let Ok(Ok((ics, etag))) = timeout(CALDAV_TIMEOUT, get_future).await else {
                responses.push_str(&format!(
                    "<Fetch><Store>{}</Store><CollectionId>{}</CollectionId><ServerId>{}</ServerId><Status>8</Status></Fetch>",
                    xml_escape(&store),
                    xml_escape(&collection_id),
                    xml_escape(&server_id)
                ));
                continue;
            };
            (ics, etag)
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

/// Handle EAS MoveItems command.
///
/// Moves items between folders. Supports email (via JMAP) and calendar (within default calendar).
/// If the destination is not supported, returns an appropriate error status.
async fn handle_move_items(
    state: &Arc<AppState>,
    username: &str,
    password: &SecretString,
    xml: &str,
    wbxml: &Wbxml,
    as_wbxml: bool,
    request_id: &str,
) -> Response {
    // Parse the MoveItems XML to extract each <Move> element
    let doc = match Document::parse(xml) {
        Ok(d) => d,
        Err(e) => {
            tracing::debug!(request_id = %request_id, "Failed to parse MoveItems XML: {}", e);
            return bad_request_response(request_id, "Invalid MoveItems XML");
        }
    };

    let mut responses = String::new();
    let mut overall_status = 1u8; // Success by default; any failure sets to failure and continues

    // Iterate over Move elements under MoveItems (any namespace prefix)
    for move_elem in doc
        .descendants()
        .filter(|n| n.is_element() && n.tag_name().name() == "Move")
    {
        let src_msg_id = move_elem
            .children()
            .find(|n| n.is_element() && n.tag_name().name() == "SrcMsgId")
            .and_then(|n| n.text())
            .map(String::from)
            .unwrap_or_default();
        let dst_fld_id = move_elem
            .children()
            .find(|n| n.is_element() && n.tag_name().name() == "DstFldId")
            .and_then(|n| n.text())
            .map(String::from)
            .unwrap_or_default();
        let _src_fld_id = move_elem
            .children()
            .find(|n| n.is_element() && n.tag_name().name() == "SrcFldId")
            .and_then(|n| n.text())
            .map(String::from);

        if src_msg_id.is_empty() || dst_fld_id.is_empty() {
            overall_status = 4; // ErrorInvalidIdMalformed or similar
            responses.push_str(&format!(r#"<Status>{}</Status>"#, overall_status));
            continue;
        }

        // Determine item type by server ID prefix
        let is_email = crate::email::is_email_server_id(&src_msg_id);

        // For email, move via JMAP
        if is_email {
            if !state.email_available() {
                overall_status = 5; // ErrorInvalidRequest?
                responses.push_str(&format!(r#"<Status>{}</Status>"#, overall_status));
                continue;
            }

            // Map destination folder CollectionId to mailbox role
            let target_role = match dst_fld_id.as_str() {
                "2" => "inbox",
                "3" => "drafts",
                "4" => "trash",
                "5" => "sent",
                "6" => {
                    // Outbox not a real mailbox; special handling
                    responses.push_str(r#"<Status>4</Status>"#);
                    continue;
                }
                "12" => "junk",
                _ => {
                    // Check if it's a distinguished folder ID string (like "inbox")
                    match dst_fld_id.to_ascii_lowercase().as_str() {
                        "inbox" => "inbox",
                        "drafts" => "drafts",
                        "deleteditems" => "trash",
                        "sentitems" => "sent",
                        "junkemail" => "junk",
                        _ => {
                            responses.push_str(r#"<Status>6</Status>"#);
                            continue;
                        }
                    }
                }
            };

            // Get JMAP client and account ID
            let Some(jmap) = state.jmap_client.as_ref() else {
                responses.push_str(r#"<Status>8</Status>"#);
                continue;
            };

            let account_id = match jmap.get_account_id(username, password).await {
                Ok(id) => id,
                Err(e) => {
                    tracing::error!(error = %e, "MoveItems: failed to get JMAP account ID");
                    responses.push_str(r#"<Status>8</Status>"#);
                    continue;
                }
            };

            // Perform the move
            match crate::email::move_email_via_jmap(
                state,
                jmap,
                &account_id,
                &src_msg_id,
                target_role,
                username,
                password,
            )
            .await
            {
                Ok(_new_change_key) => {
                    // Success: include SrcMsgId and DstMsgId (same server id)
                    responses.push_str(&format!(
                        r#"<Move><SrcMsgId>{}</SrcMsgId><DstMsgId>{}</DstMsgId></Move>"#,
                        xml_escape(&src_msg_id),
                        xml_escape(&src_msg_id) // DstMsgId equals moved item id
                    ));
                }
                Err(e) => {
                    tracing::error!(error = %e, "MoveItems email move failed");
                    responses.push_str(r#"<Status>8</Status>"#);
                }
            }
        } else {
            // Calendar item move
            // Only support moving to default calendar (collection ID "1")
            // because only one calendar folder is exposed.
            // Destination folder must be the Calendar folder (id "1" or distinguished "calendar").
            // Also source must belong to the default calendar; we can check via DB if needed.
            if dst_fld_id != "1" && !dst_fld_id.eq_ignore_ascii_case("calendar") {
                // Unsupported destination
                responses.push_str(r#"<Status>6</Status>"#);
                continue;
            }

            // For calendar, moving within the same (only) collection is effectively a no-op
            // since there is no other calendar. The Exchange protocol expects a new DstMsgId,
            // but the item identifier does not change when moving within same calendar.
            // We'll return the same server ID.
            // However, if we had multiple calendars, we would perform a CalDAV MOVE here.
            // For now, simply return success.
            responses.push_str(&format!(
                r#"<Move><SrcMsgId>{}</SrcMsgId><DstMsgId>{}</DstMsgId></Move>"#,
                xml_escape(&src_msg_id),
                xml_escape(&src_msg_id)
            ));
        }
    }

    // If no <Move> elements were processed, return error
    if responses.is_empty() {
        overall_status = 5; // ErrorInvalidRequest
        responses.push_str(&format!(r#"<Status>{}</Status>"#, overall_status));
    }

    let payload = if overall_status == 1 {
        format!(
            r#"<?xml version="1.0" encoding="utf-8"?><MoveItems xmlns="Move:"><Status>{}</Status>{}</MoveItems>"#,
            overall_status, responses
        )
    } else {
        // In case of errors, we can still return the fragment per element but overall might be failure
        format!(
            r#"<?xml version="1.0" encoding="utf-8"?><MoveItems xmlns="Move:"><Status>{}</Status>{}</MoveItems>"#,
            overall_status, responses
        )
    };

    xml_or_wbxml_response(wbxml, as_wbxml, &payload, request_id)
}

/// Fetch free-busy information via JMAP Calendar (urn:ietf:params:jmap:calendars).
///
/// Uses `CalendarEvent/query` + `CalendarEvent/get` with the `iCalendar` property
/// to obtain ICS data, then renders the merged free-busy string using the same
/// logic as the CalDAV path.
///
/// Returns `Some(merged_freebusy_string)` on success, `None` to fall back to CalDAV.
async fn fetch_freebusy_jmap_eas(
    jmap: &Arc<JmapClient>,
    mailbox: &str,
    password: &SecretString,
    start: chrono::DateTime<chrono::Utc>,
    end: chrono::DateTime<chrono::Utc>,
    safe_interval: i64,
) -> Option<String> {
    // Check if JMAP Calendar is supported
    if !jmap.supports_calendar(mailbox, password).await {
        return None;
    }

    // Guard against invalid safe_interval (division by zero)
    if safe_interval <= 0 {
        tracing::warn!(target: "eas", ?safe_interval, "Invalid safe_interval; falling back to CalDAV");
        return None;
    }

    let account_id = match jmap.get_calendar_account_id(mailbox, password).await {
        Ok(id) => id,
        Err(e) => {
            tracing::debug!(target: "eas", error = %e, "JMAP Calendar account ID lookup failed");
            return None;
        }
    };

    let result = match jmap
        .query_calendar_events(QueryCalendarEventsParams {
            account_id: &account_id,
            calendar_id: None,
            // RFC 3339 extended format required by Stalwart's JMAP
            // CalendarEvent/query filter deserializer (not basic ISO 8601)
            start: &start.format("%Y-%m-%dT%H:%M:%SZ").to_string(),
            end: &end.format("%Y-%m-%dT%H:%M:%SZ").to_string(),
            limit: 1000,
            username: mailbox,
            password,
        })
        .await
    {
        Ok(r) => r,
        Err(e) => {
            tracing::debug!(target: "eas", error = %e, "JMAP Calendar event query failed");
            return None;
        }
    };

    let slot_count = (((end - start).num_seconds().max(0) + (safe_interval * 60 - 1))
        / (safe_interval * 60)) as usize;
    let mut merged = vec!['0'; slot_count];

    for event in &result.events {
        if let Some(ref ics) = event.i_calendar
            && let Some(item) = parse_ics_event(ics)
        {
            let sd = match item.busy_status.unwrap_or(2) {
                0 => '0',
                1 => '1',
                3 => '3',
                _ => '2',
            };
            for (i, slot) in merged.iter_mut().enumerate() {
                let ss = start + ChronoDuration::minutes((i as i64) * safe_interval);
                let se = ss + ChronoDuration::minutes(safe_interval);
                if item.start < se && item.end > ss && sd > *slot {
                    *slot = sd;
                }
            }
        }
    }

    Some(merged.into_iter().collect())
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

    // Try JMAP Calendar first (urn:ietf:params:jmap:calendars) unless configured to prefer CalDAV.
    // Falls back to CalDAV if JMAP Calendar is unavailable or fails.
    if !state.cfg.prefer_caldav_freebusy
        && let Some(jmap) = &state.jmap_client
    {
        if let Some(result) =
            fetch_freebusy_jmap_eas(jmap, mailbox, password, start, end, safe_interval).await
        {
            return result;
        }
        tracing::debug!(target: "eas", "JMAP Calendar free-busy failed, falling back to CalDAV");
    }

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
            reader.config_mut().trim_text(false);
            let mut buf = Vec::new();
            let mut in_cal_data = false;
            let mut caldata_buf = String::new();
            loop {
                match reader.read_event_into(&mut buf) {
                    Ok(Event::Start(e)) if e.name().local_name().as_ref() == b"calendar-data" => {
                        in_cal_data = true;
                        caldata_buf.clear();
                    }
                    Ok(Event::Text(ref t)) if in_cal_data => {
                        if let Ok(ics) = t.decode() {
                            caldata_buf.push_str(&ics);
                        }
                    }
                    Ok(Event::CData(ref t)) if in_cal_data => {
                        caldata_buf.push_str(&String::from_utf8_lossy(t.as_ref()));
                    }
                    Ok(Event::End(e)) if e.name().local_name().as_ref() == b"calendar-data" => {
                        in_cal_data = false;
                        let ics = caldata_buf.trim();
                        // Skip empty calendar-data (likely calendar collection root)
                        if !ics.is_empty()
                            && let Some(item) = parse_ics_event(ics)
                        {
                            let sd = match item.busy_status.unwrap_or(2) {
                                0 => '0',
                                1 => '1',
                                3 => '3',
                                _ => '2',
                            };
                            for (i, slot) in merged.iter_mut().enumerate() {
                                let ss =
                                    start + ChronoDuration::minutes((i as i64) * safe_interval);
                                let se = ss + ChronoDuration::minutes(safe_interval);
                                if item.start < se && item.end > ss && sd > *slot {
                                    *slot = sd;
                                }
                            }
                        }
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
                .unwrap_or_else(|| start + ChronoDuration::days(7));
            let max_end = start + ChronoDuration::days(MAX_FREEBUSY_DAYS);
            let clamped = if pe > max_end { max_end } else { pe };
            if clamped <= start {
                start + ChronoDuration::days(7)
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

    // Perform directory lookup for each recipient to get display name and email.
    // We use spawn_blocking to run the synchronous directory search.
    let lookup_futures = recipients.iter().map(|recipient| {
        let state = state.clone();
        let query = recipient.clone();
        async move {
            let Some(directory) = state.directory.clone() else {
                return Ok(Vec::<crate::directory::Contact>::new());
            };
            let search_result =
                tokio::task::spawn_blocking(move || directory.search_blocking(&query, None))
                    .await
                    .map_err(|e| Error::Internal(e.to_string()))?
                    .map_err(|e| Error::Internal(e.to_string()))?;
            Ok(search_result.contacts)
        }
    });
    let lookup_results = join_all(lookup_futures).await;

    // Build recipient XML combining directory results and freebusy.
    let mut recipient_xml = String::new();
    for i in 0..recipients.len() {
        let recipient = &recipients[i];
        let freebusy = &freebusy_results[i];
        let lookup_res: &Result<Vec<crate::directory::Contact>, Error> = &lookup_results[i];

        let (display_name, email) = match lookup_res {
            Ok(lookup) if !lookup.is_empty() => {
                let contact = &lookup[0];
                (contact.display_name.clone(), contact.email.clone())
            }
            _ => (recipient.clone(), recipient.clone()),
        };

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
            xml_escape(&display_name),
            xml_escape(&email),
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
    reader.config_mut().trim_text(false);
    let mut buf = Vec::new();
    let mut href = String::new();
    let mut caldata_buf = String::new();
    let mut in_cal_data = false;
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
                    in_cal_data = true;
                    caldata_buf.clear();
                }
                _ => {}
            },
            Ok(Event::Text(ref t)) if in_cal_data => {
                if let Ok(text) = t.decode() {
                    caldata_buf.push_str(&text);
                }
            }
            Ok(Event::CData(ref t)) if in_cal_data => {
                caldata_buf.push_str(&String::from_utf8_lossy(t.as_ref()));
            }
            Ok(Event::End(ref e)) => {
                if e.name().local_name().as_ref() == b"calendar-data" {
                    in_cal_data = false;
                }
                if e.name().local_name().as_ref() == b"response" {
                    let ics = caldata_buf.trim();
                    // Skip empty calendar-data (likely calendar collection root)
                    if !href.is_empty()
                        && !ics.is_empty()
                        && let Some(item) = parse_ics_event(ics)
                    {
                        let server_id = sync::generate_server_id(state.cfg.hmac_secret(), &href);
                        out.push((server_id, href.clone(), item));
                    } else if !href.is_empty() && ics.is_empty() {
                        tracing::debug!(
                            "load_calendar_events: skipping href {} (no calendar-data element - likely calendar collection root)",
                            href
                        );
                    }
                    href.clear();
                    caldata_buf.clear();
                }
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
        .unwrap_or_else(|| chrono::Utc::now() - ChronoDuration::weeks(52));
    let end = req
        .ends
        .unwrap_or_else(|| chrono::Utc::now() + ChronoDuration::weeks(52));
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

/// Handle EAS SendMail command (MS-ASCMD §2.2.2.16).
///
/// Per MS-ASCMD, the SendMail command sends a MIME message to the server
/// for delivery. The client sends the full MIME message in the request.
/// For the gateway, we parse the MIME data and send via SMTP, or
/// parse the simplified XML fields and construct the email.
async fn handle_send_mail(
    state: &Arc<AppState>,
    username: &str,
    password: &SecretString,
    xml: &str,
    wbxml: &Wbxml,
    as_wbxml: bool,
    request_id: &str,
) -> Response {
    if !state.cfg.email_enabled {
        return xml_or_wbxml_response(
            wbxml,
            as_wbxml,
            // Per MS-ASCMD §2.2.1.17, Status 4 = "Mailbox server error".
            // Returning Status 1 (Success) when email is disabled would cause
            // the client to believe the email was sent — silent email loss.
            r#"<?xml version="1.0" encoding="utf-8"?><Status xmlns="SendMail:">4</Status>"#,
            request_id,
        );
    }

    // Try to parse the EAS SendMail request and extract the MIME content
    if let Some(req) = crate::email::parse_eas_sendmail(xml) {
        // Build an EwsMessage from the parsed request for SMTP submission
        let msg = crate::email::EwsMessage {
            subject: String::new(),
            body: req.mime_data.clone().unwrap_or_default(),
            body_type: "Text".to_string(),
            from: username.to_string(),
            to_recipients: Vec::new(),
            cc_recipients: Vec::new(),
            bcc_recipients: Vec::new(),
            ..Default::default()
        };

        match crate::email::send_email(state, &msg, username, password).await {
            Ok(_) => {}
            Err(e) => {
                tracing::error!(error = %e, "SMTP send failed for EAS SendMail");
                return xml_or_wbxml_response(
                    wbxml,
                    as_wbxml,
                    r#"<?xml version="1.0" encoding="utf-8"?><Status xmlns="SendMail:">4</Status>"#,
                    request_id,
                );
            }
        }
    } else {
        // Per MS-ASCMD §2.2.1.17, Status 2 = "Protocol error" — the request
        // XML was malformed or missing required MIME content. Returning
        // Status 1 (Success) here would cause silent email loss.
        tracing::warn!("EAS SendMail: failed to parse MIME content from request");
        return xml_or_wbxml_response(
            wbxml,
            as_wbxml,
            r#"<?xml version="1.0" encoding="utf-8"?><Status xmlns="SendMail:">2</Status>"#,
            request_id,
        );
    }

    // Per MS-ASCMD, SendMail returns Status 1 on success
    xml_or_wbxml_response(
        wbxml,
        as_wbxml,
        r#"<?xml version="1.0" encoding="utf-8"?><Status xmlns="SendMail:">1</Status>"#,
        request_id,
    )
}

/// Request-scoped context for one EAS Sync command. Bundled into a struct so
/// `handle_sync_collections` stays under clippy's argument-count threshold.
#[derive(Clone, Copy)]
struct SyncCtx<'a> {
    state: &'a Arc<AppState>,
    username: &'a str,
    password: &'a SecretString,
    wbxml: &'a Wbxml,
    as_wbxml: bool,
    request_id: &'a str,
    device_id: &'a str,
}

/// Handle a multi-collection EAS Sync command.
///
/// Per MS-ASCMD §2.2.3.31.2, a Sync request can contain 1..N Collection elements.
/// Android clients (including Gmail's Exchange account) typically send all folders
/// in a single Sync request. This function processes each collection independently
/// and combines the responses into a single multi-collection Sync response.
async fn handle_sync_collections(ctx: &SyncCtx<'_>, collections: &[SyncCollection]) -> Response {
    let SyncCtx {
        state,
        username,
        password,
        wbxml,
        as_wbxml,
        request_id,
        device_id,
    } = *ctx;
    let mut collection_responses: Vec<String> = Vec::new();

    for coll in collections {
        let collection_id = coll.collection_id.as_deref().unwrap_or("1");
        let state_collection_id = scoped_collection_id(collection_id, device_id);
        let incoming_key = coll.sync_key.as_deref().unwrap_or("0");
        // Per MS-ASCMD §2.2.3.30, <Class> is optional in Sync requests.
        // When absent, infer from CollectionId using the central EAS email
        // folder mapping; ID "1" is Calendar. Previously, defaulting to "Calendar" caused
        // email Sync requests to be routed to the calendar path, returning
        // zero email items (the root cause of "no emails in Gmail on Android").
        let is_email = match coll.class.as_deref() {
            Some(c) if c.eq_ignore_ascii_case("Email") => true,
            Some(_) => crate::email::is_eas_email_collection_id(collection_id),
            None => crate::email::is_eas_email_collection_id(collection_id),
        };

        // Determine if this is Calendar sync. Collection ID "1" is Calendar.
        let is_calendar = coll
            .class
            .as_deref()
            .map(|c| c.eq_ignore_ascii_case("Calendar"))
            .unwrap_or_else(|| collection_id == "1");

        // Determine if this is Contacts sync. Per MS-ASCMD, Contacts class is "Contacts".
        // Collection ID for contacts is often "8" (the Contacts folder).
        let is_contacts = coll
            .class
            .as_deref()
            .map(|c| c.eq_ignore_ascii_case("Contacts"))
            .unwrap_or_else(|| {
                collection_id == "8" || collection_id.eq_ignore_ascii_case("contacts")
            });

        // Determine if this is Tasks sync. Per MS-ASCMD, Tasks class is "Tasks".
        // Collection ID "7" is the default Tasks folder.
        let is_tasks = coll
            .class
            .as_deref()
            .map(|c| c.eq_ignore_ascii_case("Tasks"))
            .unwrap_or_else(|| {
                collection_id == crate::tasks::TASKS_COLLECTION_ID
                    || collection_id.eq_ignore_ascii_case("tasks")
            });

        // Determine if this is Notes sync. Per MS-ASCMD, Notes class is "Notes".
        // Collection ID "10" is the default Notes folder.
        let is_notes = coll
            .class
            .as_deref()
            .map(|c| c.eq_ignore_ascii_case("Notes"))
            .unwrap_or_else(|| {
                collection_id == crate::tasks::NOTES_COLLECTION_ID
                    || collection_id.eq_ignore_ascii_case("notes")
            });

        let coll_xml = if is_email {
            if state.can_read_email() {
                match handle_email_sync(
                    state,
                    username,
                    password,
                    collection_id,
                    &state_collection_id,
                    incoming_key,
                )
                .await
                {
                    Ok(xml_str) => xml_str,
                    Err(e) => {
                        tracing::warn!(
                            request_id = %request_id,
                            error = %e,
                            "Email sync failed, returning Status 6"
                        );
                        format!(
                            "<Collection><Class>Email</Class><SyncKey>{}</SyncKey><CollectionId>{}</CollectionId><Status>6</Status></Collection>",
                            xml_escape(incoming_key),
                            xml_escape(collection_id)
                        )
                    }
                }
            } else {
                let new_sync_key = Uuid::new_v4().simple().to_string();
                if let Err(e) = state
                    .storage
                    .set_sync_key(username, &state_collection_id, &new_sync_key, None)
                    .await
                {
                    tracing::warn!(error = %e, "Failed to set email sync key");
                }
                format!(
                    "<Collection><Class>Email</Class><SyncKey>{}</SyncKey><CollectionId>{}</CollectionId><Status>1</Status></Collection>",
                    xml_escape(&new_sync_key),
                    xml_escape(collection_id)
                )
            }
        } else if is_calendar {
            let coll_xml_ref = &coll.xml;
            let owner = crate::ews::owner_from_username(username);
            let calendar_folder_id = crate::ews_folders::folder_id_for(
                owner,
                crate::ews_folders::DistinguishedFolder::Calendar,
            );
            let enforcement = PermissionEnforcement::new(&state.storage);
            let perm_ctx = PermissionContext::new(
                username.to_string(),
                owner.to_string(),
                calendar_folder_id.clone(),
            );

            let has_mutations = coll_xml_ref.contains("<Add")
                || coll_xml_ref.contains("<Change")
                || coll_xml_ref.contains("<Delete")
                || coll_xml_ref.contains(":Add")
                || coll_xml_ref.contains(":Change")
                || coll_xml_ref.contains(":Delete");

            let mut mutation_responses = String::new();
            let proceed = if has_mutations {
                let mut ok = true;
                if coll_xml_ref.contains("<Add") || coll_xml_ref.contains(":Add") {
                    match enforcement.can_create_item(&perm_ctx).await {
                        Ok(allowed) => ok &= allowed,
                        Err(e) => {
                            tracing::warn!(
                                request_id = %request_id,
                                collection_id = %collection_id,
                                error = %e,
                                "Permission check failed for can_create_item"
                            );
                            ok = false;
                        }
                    }
                }
                if ok && (coll_xml_ref.contains("<Change") || coll_xml_ref.contains(":Change")) {
                    match enforcement.can_edit_item(&perm_ctx).await {
                        Ok(allowed) => ok &= allowed,
                        Err(e) => {
                            tracing::warn!(
                                request_id = %request_id,
                                collection_id = %collection_id,
                                error = %e,
                                "Permission check failed for can_edit_item"
                            );
                            ok = false;
                        }
                    }
                }
                if ok && (coll_xml_ref.contains("<Delete") || coll_xml_ref.contains(":Delete")) {
                    match enforcement.can_delete_item(&perm_ctx).await {
                        Ok(allowed) => ok &= allowed,
                        Err(e) => {
                            tracing::warn!(
                                request_id = %request_id,
                                collection_id = %collection_id,
                                error = %e,
                                "Permission check failed for can_delete_item"
                            );
                            ok = false;
                        }
                    }
                }
                ok
            } else {
                true
            };

            // Compute the result XML, handling errors inline
            let result_xml = if has_mutations && !proceed {
                tracing::warn!(
                    request_id = %request_id,
                    collection_id = %collection_id,
                    "Calendar mutation permission denied"
                );
                format!(
                    "<Collection><Class>Calendar</Class><SyncKey>{}</SyncKey><CollectionId>{}</CollectionId><Status>4</Status></Collection>",
                    xml_escape(incoming_key),
                    xml_escape(collection_id)
                )
            } else if has_mutations {
                match sync::apply_client_sync_mutations(
                    state.clone(),
                    username,
                    &state_collection_id,
                    username,
                    password.expose_secret(),
                    coll_xml_ref,
                )
                .await
                {
                    Ok(results) => {
                        mutation_responses = sync::render_client_mutation_responses(&results);
                        // Continue to sync below
                        String::new() // marker to indicate we should still sync
                    }
                    Err(e) => {
                        tracing::error!(
                            "request_id={} failed applying Sync mutations: {}",
                            request_id,
                            e
                        );
                        format!(
                            "<Collection><Class>Calendar</Class><SyncKey>{}</SyncKey><CollectionId>{}</CollectionId><Status>6</Status></Collection>",
                            xml_escape(incoming_key),
                            xml_escape(collection_id)
                        )
                    }
                }
            } else {
                // No mutations; will sync directly below
                String::new()
            };

            // If result_xml is non-empty, that's our final collection response
            // otherwise, we need to call perform_sync
            if result_xml.is_empty() {
                let opts = SyncOptions {
                    window_size: coll.window_size.unwrap_or(100),
                    get_changes: coll.get_changes,
                    filter_start: coll
                        .filter_type
                        .map(filter_type_to_start)
                        .unwrap_or_else(|| chrono::Utc::now() - ChronoDuration::weeks(52)),
                };
                match sync::perform_sync(&sync::PerformSyncParams {
                    state: state.clone(),
                    owner: username,
                    collection_id,
                    state_collection_id: &state_collection_id,
                    incoming_sync_key: incoming_key,
                    content_class: "Calendar",
                    opts,
                    username,
                    password: password.expose_secret(),
                    client_mutation_responses: &mutation_responses,
                })
                .await
                {
                    Ok(resp_xml) => extract_inner_collection(&resp_xml),
                    Err(e) => {
                        tracing::error!("request_id={} Sync Error: {}", request_id, e);
                        format!(
                            "<Collection><Class>Calendar</Class><SyncKey>{}</SyncKey><CollectionId>{}</CollectionId><Status>6</Status></Collection>",
                            xml_escape(incoming_key),
                            xml_escape(collection_id)
                        )
                    }
                }
            } else {
                result_xml
            }
        } else if is_contacts {
            // Contacts sync via CardDAV with client mutation support
            let mut contact_mutation_responses = String::new();

            // Setup permission enforcement for contacts
            let coll_xml_ref = &coll.xml;
            let owner = crate::ews::owner_from_username(username);
            // Determine folder ID for permission checks (use collection_id)
            let folder_id = if collection_id == "8" {
                "8".to_string()
            } else {
                collection_id.to_string()
            };
            let enforcement = PermissionEnforcement::new(&state.storage);
            let perm_ctx =
                PermissionContext::new(username.to_string(), owner.to_string(), folder_id.clone());

            // Detect if there are client mutations (Add/Change/Delete)
            let has_mutations = coll_xml_ref.contains("<Add")
                || coll_xml_ref.contains("<Change")
                || coll_xml_ref.contains("<Delete")
                || coll_xml_ref.contains(":Add")
                || coll_xml_ref.contains(":Change")
                || coll_xml_ref.contains(":Delete");

            // Apply client mutations if present
            if has_mutations {
                // Permission checks similar to calendar
                let mut ok = true;
                if coll_xml_ref.contains("<Add") || coll_xml_ref.contains(":Add") {
                    match enforcement.can_create_item(&perm_ctx).await {
                        Ok(allowed) => ok &= allowed,
                        Err(e) => {
                            tracing::warn!(request_id = %request_id, collection_id = %collection_id, error = %e, "Permission check failed for can_create_item (contacts)");
                            ok = false;
                        }
                    }
                }
                if ok && (coll_xml_ref.contains("<Change") || coll_xml_ref.contains(":Change")) {
                    match enforcement.can_edit_item(&perm_ctx).await {
                        Ok(allowed) => ok &= allowed,
                        Err(e) => {
                            tracing::warn!(request_id = %request_id, collection_id = %collection_id, error = %e, "Permission check failed for can_edit_item (contacts)");
                            ok = false;
                        }
                    }
                }
                if ok && (coll_xml_ref.contains("<Delete") || coll_xml_ref.contains(":Delete")) {
                    match enforcement.can_delete_item(&perm_ctx).await {
                        Ok(allowed) => ok &= allowed,
                        Err(e) => {
                            tracing::warn!(request_id = %request_id, collection_id = %collection_id, error = %e, "Permission check failed for can_delete_item (contacts)");
                            ok = false;
                        }
                    }
                }

                if !ok {
                    // Permission denied for mutations
                    let xml = format!(
                        "<Collection><Class>Contacts</Class><SyncKey>{}</SyncKey><CollectionId>{}</CollectionId><Status>4</Status></Collection>",
                        xml_escape(incoming_key),
                        xml_escape(collection_id)
                    );
                    return xml_or_wbxml_response(wbxml, as_wbxml, &xml, request_id);
                }

                // Apply the client mutations
                match crate::contacts::apply_contacts_mutations(
                    state,
                    username,
                    password.expose_secret(),
                    coll_xml_ref,
                )
                .await
                {
                    Ok(results) => {
                        contact_mutation_responses =
                            crate::contacts::render_contacts_mutation_responses(&results);
                    }
                    Err(e) => {
                        tracing::error!(request_id = %request_id, error = %e, "Failed to apply contacts mutations");
                        let xml = format!(
                            "<Collection><Class>Contacts</Class><SyncKey>{}</SyncKey><CollectionId>{}</CollectionId><Status>6</Status></Collection>",
                            xml_escape(incoming_key),
                            xml_escape(collection_id)
                        );
                        return xml_or_wbxml_response(wbxml, as_wbxml, &xml, request_id);
                    }
                }
            }

            // Perform server-side sync to fetch changes (after applying client mutations)
            match crate::contacts::sync_contacts(
                state,
                username,
                password.expose_secret(),
                Some(incoming_key),
                device_id,
            )
            .await
            {
                Ok(server_changes_xml) => {
                    // Fetch the new sync key stored by sync_contacts
                    let new_sync_key = match state
                        .storage
                        .get_sync_key(username, &state_collection_id)
                        .await
                    {
                        Ok(Some((key, _))) => key,
                        Ok(None) => Uuid::new_v4().simple().to_string(),
                        Err(_) => {
                            tracing::warn!(request_id = %request_id, collection_id = %collection_id, "Failed to get sync key for contacts, generating fresh");
                            Uuid::new_v4().simple().to_string()
                        }
                    };
                    let xml = format!(
                        "<Collection><Class>Contacts</Class><SyncKey>{}</SyncKey><CollectionId>{}</CollectionId><Status>1</Status>{}{}</Collection>",
                        xml_escape(&new_sync_key),
                        xml_escape(collection_id),
                        contact_mutation_responses,
                        server_changes_xml
                    );
                    return xml_or_wbxml_response(wbxml, as_wbxml, &xml, request_id);
                }
                Err(e) => {
                    tracing::error!(error = %e, "Contacts sync failed");
                    let xml = format!(
                        "<Collection><Class>Contacts</Class><SyncKey>{}</SyncKey><CollectionId>{}</CollectionId><Status>6</Status></Collection>",
                        xml_escape(incoming_key),
                        xml_escape(collection_id)
                    );
                    return xml_or_wbxml_response(wbxml, as_wbxml, &xml, request_id);
                }
            }
        } else if is_tasks {
            // Gateway-local Tasks sync (MS-ASTASK) via the SQLite-backed store.
            let mutation_xml = coll.xml.clone();
            let mutations = crate::tasks::parse_task_mutations(&mutation_xml);

            let mut mutation_responses = String::new();
            if !mutations.is_empty() {
                match crate::tasks::apply_task_mutations(state, username, &mutations).await {
                    Ok(results) => {
                        mutation_responses =
                            crate::tasks::render_mutation_responses(&results);
                    }
                    Err(e) => {
                        tracing::error!(
                            request_id = %request_id,
                            collection_id = %collection_id,
                            error = %e,
                            "Failed to apply tasks mutations"
                        );
                        return xml_or_wbxml_response(
                            wbxml,
                            as_wbxml,
                            &format!(
                                "<Collection><Class>Tasks</Class><SyncKey>{}</SyncKey><CollectionId>{}</CollectionId><Status>6</Status></Collection>",
                                xml_escape(incoming_key),
                                xml_escape(collection_id)
                            ),
                            request_id,
                        );
                    }
                }
            }

            let new_sync_key = Uuid::new_v4().simple().to_string();
            if let Err(e) = state
                .storage
                .set_sync_key(username, &state_collection_id, &new_sync_key, None)
                .await
            {
                tracing::warn!(error = %e, "Failed to set tasks sync key");
            }

            match crate::tasks::sync_tasks(state, username).await {
                Ok(changes_xml) => format!(
                    "<Collection><Class>Tasks</Class><SyncKey>{}</SyncKey><CollectionId>{}</CollectionId><Status>1</Status>{}{}</Collection>",
                    xml_escape(&new_sync_key),
                    xml_escape(collection_id),
                    mutation_responses,
                    changes_xml
                ),
                Err(e) => {
                    tracing::error!(error = %e, "Tasks sync failed");
                    format!(
                        "<Collection><Class>Tasks</Class><SyncKey>{}</SyncKey><CollectionId>{}</CollectionId><Status>6</Status></Collection>",
                        xml_escape(incoming_key),
                        xml_escape(collection_id)
                    )
                }
            }
        } else if is_notes {
            // Gateway-local Notes sync (MS-ASNOTE) via the SQLite-backed store.
            let mutation_xml = coll.xml.clone();
            let mutations = crate::tasks::parse_note_mutations(&mutation_xml);

            let mut mutation_responses = String::new();
            if !mutations.is_empty() {
                match crate::tasks::apply_note_mutations(state, username, &mutations).await {
                    Ok(results) => {
                        mutation_responses =
                            crate::tasks::render_mutation_responses(&results);
                    }
                    Err(e) => {
                        tracing::error!(
                            request_id = %request_id,
                            collection_id = %collection_id,
                            error = %e,
                            "Failed to apply notes mutations"
                        );
                        return xml_or_wbxml_response(
                            wbxml,
                            as_wbxml,
                            &format!(
                                "<Collection><Class>Notes</Class><SyncKey>{}</SyncKey><CollectionId>{}</CollectionId><Status>6</Status></Collection>",
                                xml_escape(incoming_key),
                                xml_escape(collection_id)
                            ),
                            request_id,
                        );
                    }
                }
            }

            let new_sync_key = Uuid::new_v4().simple().to_string();
            if let Err(e) = state
                .storage
                .set_sync_key(username, &state_collection_id, &new_sync_key, None)
                .await
            {
                tracing::warn!(error = %e, "Failed to set notes sync key");
            }

            match crate::tasks::sync_notes(state, username).await {
                Ok(changes_xml) => format!(
                    "<Collection><Class>Notes</Class><SyncKey>{}</SyncKey><CollectionId>{}</CollectionId><Status>1</Status>{}{}</Collection>",
                    xml_escape(&new_sync_key),
                    xml_escape(collection_id),
                    mutation_responses,
                    changes_xml
                ),
                Err(e) => {
                    tracing::error!(error = %e, "Notes sync failed");
                    format!(
                        "<Collection><Class>Notes</Class><SyncKey>{}</SyncKey><CollectionId>{}</CollectionId><Status>6</Status></Collection>",
                        xml_escape(incoming_key),
                        xml_escape(collection_id)
                    )
                }
            }
        } else {
            // Truly unsupported collection type.
            tracing::warn!(
                request_id = %request_id,
                collection_id = %collection_id,
                class = coll.class.as_deref().unwrap_or("(none)"),
                "Unsupported collection type for Sync; rejecting"
            );
            format!(
                "<Collection><Class>{}</Class><SyncKey>{}</SyncKey><CollectionId>{}</CollectionId><Status>4</Status></Collection>",
                xml_escape(coll.class.as_deref().unwrap_or("")),
                xml_escape(incoming_key),
                xml_escape(collection_id)
            )
        };

        collection_responses.push(coll_xml);
    }

    // Build multi-collection response
    let collections_xml = collection_responses.join("");
    let resp_xml = format!(
        r#"<?xml version="1.0" encoding="utf-8"?><Sync xmlns="AirSync:" xmlns:Email="Email:" xmlns:Calendar="Calendar:" xmlns:Contacts="Contacts:" xmlns:Tasks="Tasks:" xmlns:Notes="Notes:" xmlns:AirSyncBase="AirSyncBase:"><Collections>{collections_xml}</Collections></Sync>"#
    );
    xml_or_wbxml_response(wbxml, as_wbxml, &resp_xml, request_id)
}

/// Extract the inner `<Collection>...</Collection>` element from a single-collection
/// Sync response produced by `perform_sync`. This strips the outer envelope
/// (`<Sync><Collections>...</Collections></Sync>`) to allow nesting inside
/// a multi-collection response.
fn extract_inner_collection(resp_xml: &str) -> String {
    // Find the start of a <Collection> element (avoid matching <Collections>)
    let start = resp_xml
        .find("<Collection>")
        .or_else(|| resp_xml.find("<Collection "));
    let end = resp_xml.rfind("</Collection>");
    if let (Some(start), Some(end)) = (start, end) {
        let end_full = end + "</Collection>".len();
        return resp_xml[start..end_full].to_string();
    }
    // Fallback: return the whole XML as-is
    resp_xml.to_string()
}

/// Handle EAS Email Sync class by routing to JMAP.
///
/// Per MS-ASEMAIL, the Email sync class synchronizes email messages.
/// The gateway translates JMAP Email/get and Email/changes to EAS Sync responses.
async fn handle_email_sync(
    state: &Arc<AppState>,
    username: &str,
    password: &SecretString,
    collection_id: &str,
    state_collection_id: &str,
    incoming_sync_key: &str,
) -> anyhow::Result<String> {
    // Map CollectionId to JMAP mailbox role.
    // Previously hardcoded "inbox" and "2", meaning syncing any other folder
    // (Sent Items, Drafts, etc.) would incorrectly fetch Inbox emails and
    // return them under CollectionId "2", violating the ActiveSync protocol.
    // Use the raw collection_id (e.g. "2"), NOT the scoped state_collection_id
    // (e.g. "2::deviceid") — the scoped form would never match any role.
    let mailbox_role = match crate::email::eas_collection_id_to_mailbox_role(collection_id) {
        Some(role) => role,
        None => {
            // CollectionId has no JMAP mailbox (e.g. Outbox "6").
            // Return empty sync response — no emails to sync.
            let new_sync_key = Uuid::new_v4().simple().to_string();
            if let Err(e) = state
                .storage
                .set_sync_key(username, state_collection_id, &new_sync_key, None)
                .await
            {
                tracing::warn!(error = %e, "Failed to set email sync key");
            }
            return Ok(format!(
                "<Collection><Class>Email</Class><SyncKey>{}</SyncKey><CollectionId>{}</CollectionId><Status>1</Status></Collection>",
                new_sync_key, collection_id
            ));
        }
    };

    let jmap = match &state.jmap_client {
        Some(j) => j,
        None => {
            // JMAP not configured — return empty sync
            let new_sync_key = Uuid::new_v4().simple().to_string();
            if let Err(e) = state
                .storage
                .set_sync_key(username, state_collection_id, &new_sync_key, None)
                .await
            {
                tracing::warn!(error = %e, "Failed to set email sync key");
            }
            return Ok(format!(
                "<Collection><Class>Email</Class><SyncKey>{}</SyncKey><CollectionId>{}</CollectionId><Status>1</Status></Collection>",
                new_sync_key, collection_id
            ));
        }
    };

    let account_id = match jmap.get_account_id(username, password).await {
        Ok(id) => id,
        Err(e) => {
            tracing::warn!(error = %e, "Failed to get JMAP account ID for email sync");
            let new_sync_key = Uuid::new_v4().simple().to_string();
            return Ok(format!(
                "<Collection><Class>Email</Class><SyncKey>{}</SyncKey><CollectionId>{}</CollectionId><Status>1</Status></Collection>",
                new_sync_key, collection_id
            ));
        }
    };

    // For initial sync (sync_key="0"), fetch all emails and store JMAP state token
    if incoming_sync_key == "0" {
        let new_sync_key = Uuid::new_v4().simple().to_string();

        // Fetch emails from JMAP for the requested mailbox
        let result = match crate::email::fetch_emails_jmap(
            state,
            &account_id,
            mailbox_role,
            0,
            EMAIL_SYNC_PAGE_SIZE,
            username,
            password,
        )
        .await
        {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(error = %e, "Failed to fetch emails from JMAP for initial sync");
                return Ok(format!(
                    "<Collection><Class>Email</Class><SyncKey>{}</SyncKey><CollectionId>{}</CollectionId><Status>1</Status></Collection>",
                    new_sync_key, collection_id
                ));
            }
        };

        // Store the JMAP state token for delta sync
        if !result.state.is_empty() {
            if let Err(e) = state
                .storage
                .set_sync_key(
                    username,
                    state_collection_id,
                    &new_sync_key,
                    Some(&result.state),
                )
                .await
            {
                tracing::warn!(error = %e, "Failed to set initial email sync key with JMAP state");
            }
        } else {
            if let Err(e) = state
                .storage
                .set_sync_key(username, state_collection_id, &new_sync_key, None)
                .await
            {
                tracing::warn!(error = %e, "Failed to set initial email sync key");
            }
        }

        let emails = &result.emails;
        tracing::info!(
            user = %username,
            collection_id,
            email_count = emails.len(),
            sync_type = "initial",
            "Building EAS email sync response"
        );
        let mut commands_xml = String::new();
        for email in emails {
            let jmap_id = email.id.as_deref().unwrap_or("unknown");
            let server_id = crate::email::email_server_id_from_jmap_id(jmap_id);
            let app_data = crate::email::render_jmap_email_as_eas_application_data(
                email,
                &server_id,
                collection_id,
            );
            commands_xml.push_str(&format!(
                "<Add><ServerId>{}</ServerId><ApplicationData>{}</ApplicationData></Add>",
                server_id, app_data,
            ));
        }
        let response = format!(
            "<Collection><Class>Email</Class><SyncKey>{}</SyncKey><CollectionId>{}</CollectionId><Status>1</Status><Commands>{}</Commands></Collection>",
            new_sync_key, collection_id, commands_xml
        );
        tracing::info!(
            user = %username,
            collection_id,
            sync_type = "initial",
            "EAS email sync response built"
        );
        return Ok(response);
    }

    // Subsequent syncs — use JMAP Email/changes for delta sync
    let new_sync_key = Uuid::new_v4().simple().to_string();

    // Get the stored JMAP state token
    let previous_state = state
        .storage
        .get_sync_key(username, state_collection_id)
        .await?;
    let jmap_state_token = previous_state.and_then(|(_, token)| token);

    if let Some(old_state) = jmap_state_token {
        // Use JMAP Email/changes for delta sync
        match jmap
            .sync_email_changes(&account_id, &old_state, username, password)
            .await
        {
            Ok(changes) => {
                // Store the new JMAP state token
                if let Err(e) = state
                    .storage
                    .set_sync_key(
                        username,
                        state_collection_id,
                        &new_sync_key,
                        Some(&changes.new_state),
                    )
                    .await
                {
                    tracing::warn!(error = %e, "Failed to update email sync key with JMAP state");
                }

                // Fetch full email data for created/updated emails
                let mut commands_xml = String::new();
                let all_ids: Vec<String> = changes
                    .created
                    .iter()
                    .chain(changes.updated.iter())
                    .cloned()
                    .collect();

                if !all_ids.is_empty() {
                    // Use Email/get to fetch full email data for changed emails by ID
                    let emails = match jmap
                        .get_emails(&account_id, &all_ids, None, username, password)
                        .await
                    {
                        Ok(emails) => emails,
                        Err(e) => {
                            tracing::warn!(error = %e, "Failed to fetch changed emails from JMAP");
                            Vec::new()
                        }
                    };

                    // Filter emails to only those in our created/updated list (defensive)
                    let changed_ids: std::collections::HashSet<&str> =
                        all_ids.iter().map(|s| s.as_str()).collect();
                    for email in &emails {
                        if let Some(id) = email.id.as_deref()
                            && changed_ids.contains(id)
                        {
                            let server_id = crate::email::email_server_id_from_jmap_id(id);
                            let app_data = crate::email::render_jmap_email_as_eas_application_data(
                                email,
                                &server_id,
                                collection_id,
                            );
                            if changes.created.iter().any(|c| c == id) {
                                commands_xml.push_str(&format!(
                                    "<Add><ServerId>{}</ServerId><ApplicationData>{}</ApplicationData></Add>",
                                    server_id, app_data,
                                ));
                            } else {
                                commands_xml.push_str(&format!(
                                    "<Change><ServerId>{}</ServerId><ApplicationData>{}</ApplicationData></Change>",
                                    server_id, app_data,
                                ));
                            }
                        }
                    }
                }

                // Add Delete commands for destroyed emails
                for destroyed_id in &changes.destroyed {
                    let server_id = crate::email::email_server_id_from_jmap_id(destroyed_id);
                    commands_xml.push_str(&format!(
                        "<Delete><ServerId>{}</ServerId></Delete>",
                        server_id,
                    ));
                }

                tracing::info!(
                    user = %username,
                    collection_id,
                    changed_count = all_ids.len(),
                    sync_type = "delta",
                    "Building EAS email delta sync response"
                );
                let response = format!(
                    "<Collection><Class>Email</Class><SyncKey>{}</SyncKey><CollectionId>{}</CollectionId><Status>1</Status><Commands>{}</Commands></Collection>",
                    new_sync_key, collection_id, commands_xml
                );
                tracing::info!(
                    user = %username,
                    collection_id,
                    sync_type = "delta",
                    "EAS email delta sync response built"
                );
                return Ok(response);
            }
            Err(e) => {
                tracing::warn!(error = %e, "JMAP Email/changes failed, falling back to full sync");
                // Fall through to full sync fallback
            }
        }
    }

    // Fallback: return empty changes (client will do full sync on next attempt)
    if let Err(e) = state
        .storage
        .set_sync_key(username, state_collection_id, &new_sync_key, None)
        .await
    {
        tracing::warn!(error = %e, "Failed to update email sync key");
    }

    Ok(format!(
        "<Collection><Class>Email</Class><SyncKey>{}</SyncKey><CollectionId>{}</CollectionId><Status>1</Status></Collection>",
        new_sync_key, collection_id
    ))
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
    let Some((raw_username, password)) = parse_basic_auth(&headers) else {
        return unauth_response(&request_id);
    };
    let username = canonicalize_username(&raw_username, &state.cfg.mail_domain);
    if username != raw_username {
        tracing::info!(
            raw_username = %raw_username,
            canonical_username = %username,
            "Username domain canonicalized to GATEWAY_MAIL_DOMAIN"
        );
    }
    // Verify credentials early to avoid unnecessary processing
    if !state
        .auth_verifier
        .verify(&username, password.expose_secret())
        .await
    {
        tracing::debug!(request_id = %request_id, user = %username, "Authentication failed");
        return unauth_response(&request_id);
    }
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
            // Parse all Collection elements from the Sync request.
            // Per MS-ASCMD §2.2.3.31.2, a Sync request can contain
            // multiple Collection elements. Android clients (including
            // Gmail's Exchange account) send multi-collection Sync
            // requests to synchronize calendar and email in one round-trip.
            let sync_collections = parse_sync_collections(&xml);
            let sync_ctx = SyncCtx {
                state: &state,
                username: &username,
                password: &password,
                wbxml: &wbxml,
                as_wbxml: wants_wbxml,
                request_id: &request_id,
                device_id: &device_id,
            };
            if sync_collections.is_empty() {
                // Fallback: use the single-collection fields from EasRequest
                // (backward compat for clients that don't nest Collection elements)
                let collection_id = req.collection_id.as_deref().unwrap_or("1");
                let incoming_key = req.sync_key.as_deref().unwrap_or("0");
                let class = req.class.as_deref().unwrap_or("Calendar");
                let sc = SyncCollection {
                    sync_key: Some(incoming_key.to_string()),
                    collection_id: Some(collection_id.to_string()),
                    class: Some(class.to_string()),
                    window_size: req.window_size,
                    // EasRequest.get_changes defaults to true when absent
                    get_changes: req.get_changes,
                    filter_type: req.filter_type,
                    // Use the full xml for single-collection requests so
                    // mutation checks and apply_client_sync_mutations work
                    // correctly (no cross-collection leakage possible).
                    xml: xml.clone(),
                };
                handle_sync_collections(&sync_ctx, &[sc]).await
            } else {
                handle_sync_collections(&sync_ctx, &sync_collections).await
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
        "Settings" => {
            handle_settings(&state, &username, &wbxml, wants_wbxml, &request_id, &xml).await
        }
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
        "MoveItems" => {
            handle_move_items(
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
        "SendMail" | "SmartReply" | "SmartForward" => {
            handle_send_mail(
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
        _ => unsupported_command_response(&req.command, &wbxml, wants_wbxml, &request_id),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::header::WWW_AUTHENTICATE;

    #[test]
    fn test_unauth_response_includes_bearer_and_basic() {
        let resp = unauth_response("test-req-1");
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);

        let www_auth_values: Vec<&str> = resp
            .headers()
            .get_all(WWW_AUTHENTICATE)
            .iter()
            .map(|v| v.to_str().unwrap_or(""))
            .collect();

        let has_basic = www_auth_values.iter().any(|v| v.starts_with("Basic "));
        let has_bearer = www_auth_values.iter().any(|v| v.starts_with("Bearer "));

        assert!(has_basic, "WWW-Authenticate must include Basic scheme");
        assert!(
            has_bearer,
            "WWW-Authenticate must include Bearer scheme for AutoDetect compatibility"
        );
    }

    #[test]
    fn test_unauth_response_bearer_contains_exchange_client_id() {
        let resp = unauth_response("test-req-2");

        let bearer_value = resp
            .headers()
            .get_all(WWW_AUTHENTICATE)
            .iter()
            .find_map(|v| {
                let s = v.to_str().ok()?;
                if s.starts_with("Bearer ") {
                    Some(s.to_string())
                } else {
                    None
                }
            });

        let bearer = bearer_value.expect("Bearer WWW-Authenticate header must be present");
        assert!(
            bearer.contains(EXCHANGE_ACTIVESYNC_CLIENT_ID),
            "Bearer header must contain Exchange ActiveSync client_id, got: {}",
            bearer
        );
    }

    #[test]
    fn test_unauth_response_basic_realm() {
        let resp = unauth_response("test-req-3");

        let basic_value = resp
            .headers()
            .get_all(WWW_AUTHENTICATE)
            .iter()
            .find_map(|v| {
                let s = v.to_str().ok()?;
                if s.starts_with("Basic ") {
                    Some(s.to_string())
                } else {
                    None
                }
            });

        let basic = basic_value.expect("Basic WWW-Authenticate header must be present");
        assert!(
            basic.contains("realm=\"Microsoft-Server-ActiveSync\""),
            "Basic header must contain correct realm, got: {}",
            basic
        );
    }

    #[test]
    fn test_unauth_response_includes_ms_server_activesync_header() {
        let resp = unauth_response("test-req-4");
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);

        let ms_header = resp
            .headers()
            .get("MS-Server-ActiveSync")
            .expect("MS-Server-ActiveSync header must be present");
        assert_eq!(ms_header, "16.1");
    }

    #[test]
    fn test_bearer_challenge_constants_are_well_known() {
        // Per MS-XOAUTH §4.1, these values must never change — they
        // are the identifiers that Exchange Server and AutoDetect expect.
        assert_eq!(
            EXCHANGE_ACTIVESYNC_CLIENT_ID,
            "00000002-0000-0ff1-ce00-000000000000"
        );
        assert_eq!(TRUSTED_ISSUERS, "00000001-0001-0000-c000-000000000000@*");
        assert_eq!(
            AUTHORIZATION_URI,
            "https://login.microsoftonline.com/common/oauth2/authorize"
        );
    }

    #[test]
    fn test_parse_basic_auth_rejects_bearer() {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::AUTHORIZATION,
            HeaderValue::from_static(
                "Bearer eyJ0eXAiOiJKV1QiLCJhbGciOiJSUzI1NiIsIng1dCI6Ik1rcE...",
            ),
        );
        assert!(
            parse_basic_auth(&headers).is_none(),
            "parse_basic_auth must reject Bearer auth — the gateway only supports Basic"
        );
    }

    #[test]
    fn test_parse_basic_auth_accepts_basic() {
        let mut headers = HeaderMap::new();
        // Base64("user:pass") = "dXNlcjpwYXNz"
        headers.insert(
            header::AUTHORIZATION,
            HeaderValue::from_static("Basic dXNlcjpwYXNz"),
        );
        let result = parse_basic_auth(&headers);
        assert!(result.is_some(), "parse_basic_auth must accept Basic auth");
        let (user, _) = result.unwrap();
        assert_eq!(user, "user");
    }

    #[tokio::test]
    async fn test_options_response_includes_protocol_headers() {
        let resp = options_response("test-req-5");
        assert_eq!(resp.status(), StatusCode::OK);

        let allow = resp
            .headers()
            .get("Allow")
            .expect("Allow header must be present");
        assert!(allow.to_str().unwrap().contains("OPTIONS"));
        assert!(allow.to_str().unwrap().contains("POST"));

        let versions = resp
            .headers()
            .get("MS-ASProtocolVersions")
            .expect("MS-ASProtocolVersions must be present");
        let versions_str = versions.to_str().unwrap();
        assert!(versions_str.contains("16.1"));

        let commands = resp
            .headers()
            .get("MS-ASProtocolCommands")
            .expect("MS-ASProtocolCommands must be present");
        let commands_str = commands.to_str().unwrap();
        assert!(commands_str.contains("Sync"));
        assert!(commands_str.contains("FolderSync"));
        assert!(commands_str.contains("Provision"));
    }

    #[test]
    fn test_forwarded_https_enforced_absent_header_passes() {
        let headers = HeaderMap::new();
        assert!(
            forwarded_https_enforced(&headers),
            "Missing x-forwarded-proto must pass (direct HTTP access)"
        );
    }

    #[test]
    fn test_forwarded_https_enforced_https_passes() {
        let mut headers = HeaderMap::new();
        headers.insert("x-forwarded-proto", HeaderValue::from_static("https"));
        assert!(
            forwarded_https_enforced(&headers),
            "x-forwarded-proto: https must pass"
        );
    }

    #[test]
    fn test_forwarded_https_enforced_http_fails() {
        let mut headers = HeaderMap::new();
        headers.insert("x-forwarded-proto", HeaderValue::from_static("http"));
        assert!(
            !forwarded_https_enforced(&headers),
            "x-forwarded-proto: http must fail"
        );
    }

    #[test]
    fn test_scoped_collection_id_is_deterministic() {
        let a = scoped_collection_id("1", "device-abc");
        let b = scoped_collection_id("1", "device-abc");
        assert_eq!(a, b, "Same inputs must produce same scoped collection ID");

        let c = scoped_collection_id("1", "device-xyz");
        assert_ne!(
            a, c,
            "Different device IDs must produce different scoped IDs"
        );
    }

    #[test]
    fn test_active_user_emails_plain_username() {
        let emails = active_user_emails("alice", "mail.example.com");
        assert_eq!(emails, vec!["alice@mail.example.com"]);
    }

    #[test]
    fn test_active_user_emails_email_username() {
        // Always uses mail_domain, not the username\'s domain
        let emails = active_user_emails("bob@example.org", "mail.example.com");
        assert_eq!(emails, vec!["bob@mail.example.com"]);
    }

    #[test]
    fn test_active_user_emails_trailing_at() {
        let emails = active_user_emails("carol@", "mail.example.com");
        assert_eq!(emails, vec!["carol@mail.example.com"]);
    }

    #[test]
    fn test_active_user_emails_non_canonical_domain() {
        // Key use-case: user authenticated with wrong domain
        let emails = active_user_emails("contact@exchange.com", "example.com");
        assert_eq!(emails, vec!["contact@example.com"]);
    }

    #[test]
    fn test_parse_sync_collections_multi() {
        // Simulate Android multi-collection Sync: one Calendar, one Email
        let xml = r#"<?xml version="1.0" encoding="utf-8"?>
<Sync xmlns="AirSync:">
  <Collections>
    <Collection>
      <SyncKey>0</SyncKey>
      <CollectionId>1</CollectionId>
      <Class>Calendar</Class>
      <GetChanges>1</GetChanges>
      <WindowSize>25</WindowSize>
      <FilterType>5</FilterType>
    </Collection>
    <Collection>
      <SyncKey>0</SyncKey>
      <CollectionId>2</CollectionId>
      <Class>Email</Class>
      <GetChanges>1</GetChanges>
      <WindowSize>50</WindowSize>
    </Collection>
  </Collections>
</Sync>"#;

        let collections = parse_sync_collections(xml);
        assert_eq!(collections.len(), 2, "Should parse 2 collections");

        assert_eq!(collections[0].class.as_deref(), Some("Calendar"));
        assert_eq!(collections[0].collection_id.as_deref(), Some("1"));
        assert_eq!(collections[0].sync_key.as_deref(), Some("0"));
        assert_eq!(collections[0].window_size, Some(25));
        assert_eq!(collections[0].filter_type, Some(5));
        assert!(collections[0].get_changes);
        // Verify raw XML captured — must only contain Calendar collection content
        assert!(
            collections[0].xml.contains("<Class>Calendar</Class>"),
            "Calendar collection xml should contain Calendar class"
        );
        assert!(
            !collections[0].xml.contains("<Class>Email</Class>"),
            "Calendar collection xml should NOT contain Email class (cross-collection leakage)"
        );

        assert_eq!(collections[1].class.as_deref(), Some("Email"));
        assert_eq!(collections[1].collection_id.as_deref(), Some("2"));
        assert_eq!(collections[1].sync_key.as_deref(), Some("0"));
        assert_eq!(collections[1].window_size, Some(50));
        assert!(collections[1].get_changes);
        // Verify raw XML captured — must only contain Email collection content
        assert!(
            collections[1].xml.contains("<Class>Email</Class>"),
            "Email collection xml should contain Email class"
        );
        assert!(
            !collections[1].xml.contains("<Class>Calendar</Class>"),
            "Email collection xml should NOT contain Calendar class (cross-collection leakage)"
        );
    }

    #[test]
    fn test_parse_sync_collections_single() {
        // Single-collection Sync (older clients)
        let xml = r#"<?xml version="1.0" encoding="utf-8"?>
<Sync xmlns="AirSync:">
  <Collections>
    <Collection>
      <SyncKey>abc123</SyncKey>
      <CollectionId>1</CollectionId>
      <Class>Calendar</Class>
    </Collection>
  </Collections>
</Sync>"#;

        let collections = parse_sync_collections(xml);
        assert_eq!(collections.len(), 1);
        assert_eq!(collections[0].class.as_deref(), Some("Calendar"));
        assert_eq!(collections[0].sync_key.as_deref(), Some("abc123"));
        // GetChanges defaults to true when absent per MS-ASCMD §2.2.3.72
        assert!(
            collections[0].get_changes,
            "GetChanges should default to true when absent"
        );
        // Verify raw XML captured
        assert!(
            collections[0].xml.contains("<SyncKey>abc123</SyncKey>"),
            "Single collection xml should contain its SyncKey"
        );
    }

    #[test]
    fn test_parse_sync_collections_get_changes_default() {
        // When <GetChanges> is absent, it defaults to true per MS-ASCMD §2.2.3.72
        let xml = r#"<Collection><SyncKey>0</SyncKey><CollectionId>1</CollectionId></Collection>"#;
        let collections = parse_sync_collections(xml);
        assert_eq!(collections.len(), 1);
        assert!(
            collections[0].get_changes,
            "GetChanges must default to true"
        );

        // Explicit <GetChanges>0</GetChanges> should set it to false
        let xml_zero = r#"<Collection><SyncKey>0</SyncKey><CollectionId>1</CollectionId><GetChanges>0</GetChanges></Collection>"#;
        let colls = parse_sync_collections(xml_zero);
        assert!(!colls[0].get_changes, "GetChanges=0 must be false");
    }

    #[test]
    fn test_extract_inner_collection() {
        let resp_xml = r#"<?xml version="1.0" encoding="utf-8"?><Sync xmlns="AirSync:"><Collections><Collection><Class>Calendar</Class><SyncKey>new</SyncKey><CollectionId>1</CollectionId><Status>1</Status></Collection></Collections></Sync>"#;
        let inner = extract_inner_collection(resp_xml);
        assert!(inner.contains("<Class>Calendar</Class>"));
        assert!(inner.contains("<SyncKey>new</SyncKey>"));
        assert!(inner.starts_with("<Collection"));
        assert!(inner.ends_with("</Collection>"));
        assert!(
            !inner.contains("<Sync xmlns"),
            "Should not contain outer Sync wrapper"
        );
    }

    #[test]
    fn test_eas_email_collection_id_mapping() {
        use crate::email::eas_collection_id_to_mailbox_role;

        assert_eq!(eas_collection_id_to_mailbox_role("2"), Some("inbox"));
        assert_eq!(eas_collection_id_to_mailbox_role("3"), Some("drafts"));
        assert_eq!(eas_collection_id_to_mailbox_role("4"), Some("trash"));
        assert_eq!(eas_collection_id_to_mailbox_role("5"), Some("sent"));
        assert_eq!(eas_collection_id_to_mailbox_role("6"), None);
        assert_eq!(eas_collection_id_to_mailbox_role("12"), Some("junk"));
        assert_eq!(eas_collection_id_to_mailbox_role("1"), None);
        assert_eq!(eas_collection_id_to_mailbox_role("99"), None);
    }

    #[test]
    fn test_classification_logic_with_class_and_id() {
        use crate::email::is_eas_email_collection_id;

        let test_cases = [
            (Some("Email"), "2", true, false),
            (Some("Email"), "5", true, false),
            (Some("Mail"), "5", true, false),
            (Some("Mail"), "2", true, false),
            (None, "3", true, false),
            (None, "4", true, false),
            (None, "12", true, false),
            (Some("Unknown"), "3", true, false),
            (Some("Calendar"), "1", false, true),
            (None, "1", false, true),
            (Some("Contacts"), "9", false, false),
            (Some("Tasks"), "7", false, false),
        ];

        for (class, collection_id, expect_email, expect_calendar) in test_cases {
            let is_email = match class {
                Some(c) if c.eq_ignore_ascii_case("Email") => true,
                _ => is_eas_email_collection_id(collection_id),
            };
            let is_calendar = match class {
                Some(c) if c.eq_ignore_ascii_case("Calendar") => true,
                _ => collection_id == "1",
            };
            assert_eq!(
                is_email, expect_email,
                "Class={:?}, CollectionId={}",
                class, collection_id
            );
            assert_eq!(
                is_calendar, expect_calendar,
                "Class={:?}, CollectionId={}",
                class, collection_id
            );
        }
    }

    #[test]
    fn test_ping_folder_kind_classification() {
        use super::ping_folder_kind;

        let cases = [
            ("1", "Calendar", Some(PingFolderKind::Calendar)),
            ("2", "Email", Some(PingFolderKind::Email)),
            ("3", "Email", Some(PingFolderKind::Email)),
            ("5", "Email", Some(PingFolderKind::Email)),
            ("6", "Email", Some(PingFolderKind::Email)),
            ("12", "Email", Some(PingFolderKind::Email)),
            ("8", "Contacts", Some(PingFolderKind::Contacts)),
            ("7", "Tasks", Some(PingFolderKind::Tasks)),
            ("10", "Notes", Some(PingFolderKind::Notes)),
            // Unknown id falls back to the class name.
            ("99", "Tasks", Some(PingFolderKind::Tasks)),
            ("99", "notes", Some(PingFolderKind::Notes)),
            // Fully unknown id + class yields None (Status 7).
            ("99", "CalendarX", None),
            ("99", "Bogus", None),
        ];

        for (id, class, expected) in cases {
            let folder = PingFolder {
                id: id.to_string(),
                class_name: class.to_string(),
            };
            assert_eq!(ping_folder_kind(&folder), expected, "id={} class={}", id, class);
        }
    }
}
