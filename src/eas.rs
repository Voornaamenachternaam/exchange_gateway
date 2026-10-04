// src/eas.rs
use crate::caldav::CaldavClient;
use crate::calendar::{parse_datetime, parse_ics_event};

use crate::error::GatewayError as Error;
use crate::jmap::{JmapClient, QueryCalendarEventsParams};
use crate::models::AppState;
use crate::permission::{PermissionContext, PermissionEnforcement};
use crate::sync::{self, SyncOptions, filter_type_to_start};
use crate::util::{
    canonicalize_username, nfc, normalize_username, resolve_xml_reference, xml_escape,
};
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
use dashmap::DashMap;
use futures_util::future::join_all;
use lru::LruCache;
use quick_xml::Reader;
use quick_xml::events::Event;
use roxmltree::Document;
use secrecy::{ExposeSecret, SecretString};
use std::collections::HashMap;
use std::fmt::Write as FmtWrite;
use std::num::NonZeroUsize;
use std::sync::atomic::{AtomicU64, Ordering as AtomicOrdering};
use std::sync::{Arc, LazyLock};
use std::time::{Duration as StdDuration, Instant};
use subtle::ConstantTimeEq;
use tokio::sync::Mutex as TokioMutex;
use tokio::time::timeout;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

const MAX_FREEBUSY_DAYS: i64 = 30;
const MAX_BODY_SIZE: usize = 1_048_576;
const CALDAV_TIMEOUT: StdDuration = StdDuration::from_secs(30);
const MAX_PING_CACHE_ENTRIES: usize = 10_000;
/// How long a cached Ping parameter set stays usable without the device
/// re-pinging it. Devices re-issue Ping back-to-back, so an entry untouched
/// for this long belongs to a dead/re-provisioned device and must not steer
/// a future bare Ping (Status 2 storm / battery-drain guard).
const PING_CACHE_TTL: StdDuration = StdDuration::from_secs(60 * 60);
/// Upper bound on distinct device ids cached for a single mailbox. Re-provision
/// churn (new DeviceId per provisioning pass) must not let one account crowd
/// every other user out of the global LRU.
const MAX_PING_DEVICES_PER_OWNER: usize = 64;
/// Device ids are client-controlled strings; bound their contribution to the
/// cache key so a pathological client cannot blow up entry memory.
const MAX_PING_DEVICE_ID_LEN: usize = 64;

type PingCache = LruCache<String, PingCacheEntry>;

const _: () = assert!(MAX_PING_CACHE_ENTRIES > 0);
const _: () = assert!(MAX_PING_DEVICES_PER_OWNER > 0);

static PING_CACHE: LazyLock<TokioMutex<PingCache>> = LazyLock::new(|| {
    TokioMutex::new(LruCache::new(
        NonZeroUsize::new(MAX_PING_CACHE_ENTRIES).expect("MAX_PING_CACHE_ENTRIES > 0"),
    ))
});

/// Live Pings keyed `owner:device`, carrying a generation and a cancellation
/// token so a newly arriving Ping supersedes the previous outstanding one for
/// the same device (MS-ASCMD servers terminate the older request; without this
/// both Pings wake on the same change and answer `Status 2` together, then the
/// client's next Ping storms).
static PING_IN_FLIGHT: LazyLock<DashMap<String, (u64, CancellationToken)>> =
    LazyLock::new(DashMap::new);
static PING_GENERATION: AtomicU64 = AtomicU64::new(0);

/// Normalise the client-supplied device id for cache/dedup keys.
fn ping_device_key_part(device_id: &str) -> String {
    device_id.chars().take(MAX_PING_DEVICE_ID_LEN).collect()
}

/// Fetch cached Ping parameters, expiring entries untouched for `PING_CACHE_TTL`.
async fn ping_cache_lookup(cache_key: &str) -> Option<PingCacheEntry> {
    let mut cache = PING_CACHE.lock().await;
    match cache.get(cache_key) {
        Some(entry) if entry.last_seen.elapsed() <= PING_CACHE_TTL => Some(entry.clone()),
        Some(_) => {
            cache.pop(cache_key);
            None
        }
        None => None,
    }
}

/// Store refresh Ping parameters for a device, bounding entries per owner.
async fn ping_cache_store(owner: &str, cache_key: String, entry: PingCacheEntry) {
    let mut cache = PING_CACHE.lock().await;
    let is_new = cache.put(cache_key.clone(), entry).is_none();
    if !is_new {
        return;
    }
    // New device id for this owner: if the owner now exceeds its fair share,
    // evict its stalest *other* entry so re-provision churn cannot crowd out
    // the global LRU.
    let prefix = format!("{}:", owner);
    let mut owner_count = 0usize;
    let mut oldest: Option<(String, Instant)> = None;
    for (key, value) in cache.iter() {
        if key.as_str() == cache_key || !key.starts_with(&prefix) {
            continue;
        }
        owner_count += 1;
        if oldest.as_ref().is_none_or(|(_, ts)| value.last_seen < *ts) {
            oldest = Some((key.clone(), value.last_seen));
        }
    }
    if owner_count >= MAX_PING_DEVICES_PER_OWNER
        && let Some((victim, _)) = oldest
    {
        cache.pop(&victim);
    }
}

/// RAII guard removing this Ping's entry from [`PING_IN_FLIGHT`] when the
/// request ends — but only if the entry still belongs to this Ping (a newer
/// Ping for the same device has a higher generation and survives).
struct PingInFlightGuard {
    key: String,
    generation: u64,
}

impl Drop for PingInFlightGuard {
    fn drop(&mut self) {
        PING_IN_FLIGHT.remove_if(&self.key, |_, (generation, _)| {
            *generation == self.generation
        });
    }
}

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
    /// Last time this device was seen pinging; drives TTL expiry so stale
    /// entries from dead or re-provisioned devices age out of the cache.
    last_seen: Instant,
}

/// RAII guard: releases the per-mailbox JMAP EventSource push monitor when
/// the Ping request ends, so the SSE stream is only held open while a client
/// is actually waiting on it.
struct PingMonitorGuard<'a> {
    registry: &'a crate::jmap_push::PushMonitorRegistry,
    owner: String,
}

impl Drop for PingMonitorGuard<'_> {
    fn drop(&mut self) {
        self.registry.release_email_monitor(&self.owner);
    }
}

#[derive(Clone, Debug, Default)]
struct ItemOperationsFetch {
    store: String,
    collection_id: Option<String>,
    server_id: Option<String>,
    long_id: Option<String>,
    file_reference: Option<String>,
    /// The raw `<Range>` value from this Fetch's `<Options>` block
    /// ([MS-ASCMD] §2.2.3.143.2): "m-n", zero-indexed inclusive bounds.
    /// Kept raw here; `parse_item_operations_byte_range` validates it and
    /// maps malformed values onto status 8.
    byte_range: Option<String>,
    /// The `<Options>` block inside this Fetch
    /// ([MS-ASCMD] §2.2.3.125.4), carrying airsyncbase:BodyPreference /
    /// BodyPartPreference ([MS-ASAIRS] §2.2.2.12, §2.2.2.3) and
    /// airsync:MIMESupport for the fetched item's body.
    options: Option<crate::eas_sync_options::EasSyncCollectionOptions>,
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
    /// The request arrived through Cloudflare's edge (`CF-RAY` header present).
    /// Ping uses this to clamp its hold time below Cloudflare's 125s default
    /// unanswered-request cutoff.
    via_cloudflare: bool,
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
    /// `<airsync:ConversationMode>` ([MS-ASCMD] §2.2.3.36.2, direct child of
    /// `<Collection>`): enables conversation-based filtering and the
    /// sync of conversation-based properties. Default when absent is
    /// disabled ([MS-ASCON] §2.2.2.5 semantics inverted only for
    /// GetItemEstimate; for Sync an absent element means "off").
    conversation_mode: bool,
    /// The parsed `<Options>` block ([MS-ASCMD] §2.2.3.125.6):
    /// FilterType, BodyPreference chain, BodyPartPreference, MIMESupport,
    /// MIMETruncation, MaxItems, Conflict, RightsManagementSupport.
    /// `None` when the request carried no `<Options>` for this collection,
    /// which means the sticky block from the previous request applies.
    options: Option<crate::eas_sync_options::EasSyncCollectionOptions>,
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
            conversation_mode: false,
            options: None,
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
                let name = e.name().local_name().as_ref().to_string();
                let qname = e.name();
                let qname_bytes = qname.as_ref();

                let ns = if let Some(colon_pos) = qname_bytes.find(':') {
                    let prefix = &qname_bytes[..colon_pos];
                    e.attributes()
                        .flatten()
                        .find_map(|attr| {
                            let key = attr.key.as_ref();
                            if key.starts_with("xmlns:") && &key[6..] == prefix {
                                Some(attr.value.to_string())
                            } else {
                                None
                            }
                        })
                        .unwrap_or_default()
                } else {
                    e.attributes()
                        .flatten()
                        .find_map(|attr| {
                            if attr.key.as_ref() == "xmlns" {
                                Some(attr.value.to_string())
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
                    if e.name().local_name().as_ref() == local_name =>
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
                        if name.as_ref() == "Collections" {
                            in_collections = true;
                        } else if in_collections && name.as_ref() == "Collection" {
                            found_collection = true;
                        }
                    }
                    Ok(Event::End(e)) => {
                        if e.name().local_name().as_ref() == "Collections" {
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
                return Some(e.name().local_name().as_ref().to_string());
            }
            Ok(Event::Eof) | Err(_) => return None,
            _ => {}
        }
        buf.clear();
    }
}

/// Extract the inner content of every occurrence of `<tag>...</tag>` in
/// `xml`. Used for repeated sibling blocks such as Settings OofMessage nodes.
fn extract_all_tag_blocks(xml: &str, tag: &[u8]) -> Vec<String> {
    let open = b"<"
        .iter()
        .chain(tag.iter())
        .chain(b">".iter())
        .copied()
        .collect::<Vec<u8>>();
    let close = b"</"
        .iter()
        .chain(tag.iter())
        .chain(b">".iter())
        .copied()
        .collect::<Vec<u8>>();
    let mut blocks = Vec::new();
    let bytes = xml.as_bytes();
    let mut pos = 0;
    while let Some(rel) = bytes[pos..]
        .windows(open.len())
        .position(|w| w == open.as_slice())
    {
        let start = pos + rel + open.len();
        if let Some(rel_end) = bytes[start..]
            .windows(close.len())
            .position(|w| w == close.as_slice())
        {
            blocks.push(String::from_utf8_lossy(&bytes[start..start + rel_end]).into_owned());
            pos = start + rel_end + close.len();
        } else {
            break;
        }
    }
    blocks
}

fn extract_first_tag_text(xml: &str, tag: &[u8]) -> Option<String> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);
    let mut buf = Vec::new();
    let mut inside = false;
    let mut value = String::new();
    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) if e.name().local_name().as_ref().as_bytes() == tag => {
                inside = true;
                value.clear();
            }
            Ok(Event::Text(t)) if inside => value.push_str(t.as_ref()),
            Ok(Event::CData(t)) if inside => value.push_str(t.as_ref()),
            Ok(Event::GeneralRef(r)) if inside => {
                value.push_str(&resolve_xml_reference(r.as_ref()))
            }
            Ok(Event::End(e)) if e.name().local_name().as_ref().as_bytes() == tag => {
                return Some(value);
            }
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
    let mut value = String::new();
    let mut values = Vec::new();
    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) if e.name().local_name().as_ref().as_bytes() == tag => {
                inside = true;
                value.clear();
            }
            Ok(Event::Text(t)) if inside => value.push_str(t.as_ref()),
            Ok(Event::CData(t)) if inside => value.push_str(t.as_ref()),
            Ok(Event::GeneralRef(r)) if inside => {
                value.push_str(&resolve_xml_reference(r.as_ref()))
            }
            Ok(Event::End(e)) if e.name().local_name().as_ref().as_bytes() == tag => {
                inside = false;
                values.push(std::mem::take(&mut value));
            }
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
/// Parse the global EAS Sync `WindowSize` — the `WindowSize` that is a
/// *direct child* of the `Sync` root element (MS-ASCMD §2.2.3.199), distinct
/// from the per-collection `WindowSize` nested inside `<Collection>`.
///
/// Spec interpretation of the value: 0 and values above 512 are treated as
/// 512. Returns `None` when the request carries no global WindowSize.
///
/// Depth-1 tracking is required because a naive "first `<WindowSize>` in the
/// document" scan would wrongly capture a collection-level element in
/// multi-collection requests.
fn parse_global_window_size(xml: &str) -> Option<usize> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);
    let mut buf = Vec::new();
    // 0 = at/above the root, 1 = direct child of the root element.
    let mut depth: usize = 0;
    let mut capture = false;
    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) => {
                let is_target = depth == 1 && e.name().local_name().as_ref() == "WindowSize";
                if is_target {
                    capture = true;
                }
                depth += 1;
            }
            Ok(Event::End(_)) => {
                depth = depth.saturating_sub(1);
                if capture && depth == 1 {
                    // Leaving the target element without having seen text:
                    // an empty <WindowSize/> — treat as absent.
                    capture = false;
                }
            }
            Ok(Event::Text(t)) if capture => {
                let text = t.to_string();
                if let Ok(v) = text.trim().parse::<usize>() {
                    return Some(if v == 0 || v > 512 { 512 } else { v });
                }
                capture = false;
            }
            Ok(Event::CData(c)) if capture => {
                let text = c.to_string();
                if let Ok(v) = text.trim().parse::<usize>() {
                    return Some(if v == 0 || v > 512 { 512 } else { v });
                }
                capture = false;
            }
            Ok(Event::Eof) => break,
            Err(_) => break,
            _ => {}
        }
        buf.clear();
    }
    None
}

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
                if name.as_ref() == "Collection" {
                    // Entering a new Collection element.
                    collection_start = Some(start_pos);
                    current_collection = Some(SyncCollection::default());
                    depth = 1;
                    current_tag = None;
                } else if depth == 1 && current_collection.is_some() {
                    // Direct child of Collection: track its tag name for potential text capture.
                    current_tag = Some(name.as_ref().as_bytes().to_vec());
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
                        && let Some(coll) = current_collection.as_mut()
                    {
                        let text = t.to_string();
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
                    && let Some(coll) = current_collection.as_mut()
                {
                    let text = cdata.to_string();
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
            Ok(Event::GeneralRef(r)) => {
                // Treat entity references like Text/CDATA.
                if depth == 2
                    && let Some(tag) = current_tag.as_ref()
                    && let Some(coll) = current_collection.as_mut()
                {
                    let text = resolve_xml_reference(r.as_ref());
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
                if e.name().local_name().as_ref() == "Collection" {
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
                        // Parse the <Options> block ([MS-ASCMD] §2.2.3.125.6)
                        // and the collection-level control elements
                        // (<ConversationMode>, <DeletesAsMoves>) from the raw
                        // Collection XML; they live at Collection scope.
                        let parsed = crate::eas_sync_options::parse_collection_options(&coll.xml);
                        coll.conversation_mode = parsed.controls.conversation_mode.unwrap_or(false);
                        coll.options = if parsed.options.explicitly_set {
                            Some(parsed.options)
                        } else {
                            None
                        };
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
    let mut in_id = false;
    let mut in_class = false;
    let mut current_id: Option<String> = None;
    let mut current_class: Option<String> = None;
    let mut id_buf = String::new();
    let mut class_buf = String::new();
    let mut folders = Vec::new();
    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) if e.name().local_name().as_ref() == "Folder" => {
                in_folder = true;
                current_id = None;
                current_class = None;
            }
            Ok(Event::Start(e)) if in_folder && e.name().local_name().as_ref() == "Id" => {
                in_id = true;
                id_buf.clear();
            }
            Ok(Event::Start(e)) if in_folder && e.name().local_name().as_ref() == "Class" => {
                in_class = true;
                class_buf.clear();
            }
            Ok(Event::Text(t)) if in_id => id_buf.push_str(t.as_ref()),
            Ok(Event::Text(t)) if in_class => class_buf.push_str(t.as_ref()),
            Ok(Event::GeneralRef(r)) if in_id => {
                id_buf.push_str(&resolve_xml_reference(r.as_ref()))
            }
            Ok(Event::GeneralRef(r)) if in_class => {
                class_buf.push_str(&resolve_xml_reference(r.as_ref()))
            }
            Ok(Event::End(e)) if e.name().local_name().as_ref() == "Id" => {
                in_id = false;
                current_id = Some(std::mem::take(&mut id_buf));
            }
            Ok(Event::End(e)) if e.name().local_name().as_ref() == "Class" => {
                in_class = false;
                current_class = Some(std::mem::take(&mut class_buf));
            }
            Ok(Event::End(e)) if e.name().local_name().as_ref() == "Folder" => {
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
            Ok(Event::Start(e)) if e.name().local_name().as_ref() == "Fetch" => {
                current = Some(ItemOperationsFetch::default());
                current_tag = None;
            }
            Ok(Event::Start(e)) if current.is_some() => {
                current_tag = Some(e.name().local_name().as_ref().as_bytes().to_vec());
            }
            Ok(Event::Text(t)) if current.is_some() => {
                let text = t.to_string();
                if let Some(fetch) = current.as_mut() {
                    match current_tag.as_deref() {
                        Some(b"Store") => fetch.store = text,
                        Some(b"CollectionId") => fetch.collection_id = Some(text),
                        Some(b"ServerId") => fetch.server_id = Some(text),
                        Some(b"LongId") => fetch.long_id = Some(text),
                        Some(b"FileReference") => fetch.file_reference = Some(text),
                        Some(b"Range") => fetch.byte_range = Some(text),
                        _ => {}
                    }
                }
            }
            Ok(Event::GeneralRef(r)) if current.is_some() => {
                let text = resolve_xml_reference(r.as_ref());
                if let Some(fetch) = current.as_mut() {
                    match current_tag.as_deref() {
                        Some(b"Store") => fetch.store = text,
                        Some(b"CollectionId") => fetch.collection_id = Some(text),
                        Some(b"ServerId") => fetch.server_id = Some(text),
                        Some(b"LongId") => fetch.long_id = Some(text),
                        Some(b"FileReference") => fetch.file_reference = Some(text),
                        Some(b"Range") => fetch.byte_range = Some(text),
                        _ => {}
                    }
                }
            }
            Ok(Event::End(e)) if e.name().local_name().as_ref() == "Fetch" => {
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
    // Attach each `<Fetch>` block's `<Options>` ([MS-ASCMD] §2.2.3.125.4)
    // — airsyncbase:BodyPreference / BodyPartPreference and
    // airsync:MIMESupport — to its fetch, in document order.
    // `extract_all_tag_blocks` returns the blocks in the same order the
    // streaming parser emitted them.
    let blocks = extract_all_tag_blocks(xml, b"Fetch");
    for (fetch, block) in fetches.iter_mut().zip(blocks) {
        let parsed = crate::eas_sync_options::parse_collection_options(&block);
        if parsed.options.explicitly_set {
            fetch.options = Some(parsed.options);
        }
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
    // MS-ASCMD §2.2.3.199: the server interprets the value 0 (zero) and
    // values above 512 as 512. This path only feeds the legacy
    // single-collection fallback, where the request's WindowSize is the
    // collection's own window.
    let window_size = extract_first_tag_text(xml, b"WindowSize")
        .and_then(|v| v.parse::<usize>().ok())
        .map(|v| if v == 0 || v > 512 { 512 } else { v });
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
        // `CF-RAY` is emitted exclusively by Cloudflare's edge, so its
        // presence identifies requests subject to the 125s Proxy Read Timeout if left
        // unanswered (cloudflared tunnels traverse the same edge).
        via_cloudflare: headers.contains_key("CF-RAY"),
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
    // [MS-ASPROV] §2.2.2.1.1: the Data element of a Provision response
    // carries an EASProvisionDoc in the Provision namespace. The declaration
    // URI must be the exact-case "Provision:" so the WBXML encoder maps the
    // doc's unqualified children to code page 14 ([MS-ASWBXML]) directly
    // instead of relying on the ambient default-namespace fallback.
    r#"<EASProvisionDoc xmlns="Provision:">
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
<AllowWiFi>1</AllowWiFi>
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
<Provision xmlns="Provision:" xmlns:Settings="Settings:">
  <Settings:DeviceInformation>
    <Settings:Status>1</Settings:Status>
  </Settings:DeviceInformation>
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

/// Everything a Ping invocation needs beyond the response encoder, grouped
/// to keep `handle_ping` readable as the surface grows.
struct PingInvocation<'a> {
    owner: &'a str,
    password: &'a SecretString,
    req: &'a EasRequest,
    xml: &'a str,
    request_id: &'a str,
}

/// Build the Ping `Status 2` response listing `folder_ids` as changed.
fn ping_changed_response(
    wbxml: &Wbxml,
    as_wbxml: bool,
    folder_ids: &[String],
    request_id: &str,
) -> Response {
    let xml = format!(
        r#"<?xml version="1.0" encoding="utf-8"?><Ping xmlns="Ping:"><Status>2</Status><Folders>{}</Folders></Ping>"#,
        folder_ids
            .iter()
            .map(|id| format!("<Folder>{}</Folder>", xml_escape(id)))
            .collect::<Vec<_>>()
            .join("")
    );
    xml_or_wbxml_response(wbxml, as_wbxml, &xml, request_id)
}

/// Response for a Ping superseded by a newer Ping on the same device: a clean
/// `Status 1` so the client releases the old request and continues on the new
/// one, instead of both requests racing on the same change set.
fn ping_superseded_response(wbxml: &Wbxml, as_wbxml: bool, request_id: &str) -> Response {
    let xml =
        r#"<?xml version="1.0" encoding="utf-8"?><Ping xmlns="Ping:"><Status>1</Status></Ping>"#;
    xml_or_wbxml_response(wbxml, as_wbxml, xml, request_id)
}

/// Per-folder `Email/changes` probe: returns the pinged email folders whose
/// stored JMAP state token no longer matches the server's. Run at Ping start
/// (to catch changes that landed while no Ping was in flight), on every
/// owner-matching push event (to pinpoint exactly which folders changed
/// instead of over-reporting), and after a lagged broadcast (to recover from
/// dropped push events, since the monitor does not write to the journal).
async fn probe_email_folder_changes(
    state: &Arc<AppState>,
    owner: &str,
    password: &SecretString,
    device_id: &str,
    email_folder_ids: &[String],
    jmap_account: &Option<(Arc<JmapClient>, String)>,
    request_id: &str,
) -> Vec<String> {
    let Some((jmap, account_id)) = jmap_account.as_ref() else {
        return Vec::new();
    };
    let mut changed = Vec::new();
    for folder_id in email_folder_ids {
        let collection_id = scoped_collection_id(folder_id, device_id);
        let jmap_state = state
            .storage
            .get_sync_key(owner, &collection_id)
            .await
            .ok()
            .flatten()
            .and_then(|(_, token)| token)
            .filter(|t| !t.starts_with("seq:"));
        let Some(since) = jmap_state else {
            continue;
        };
        match jmap
            .sync_email_changes(account_id, &since, owner, password, None)
            .await
        {
            Ok(changes) => {
                if !changes.created.is_empty()
                    || !changes.updated.is_empty()
                    || !changes.destroyed.is_empty()
                    || changes.new_state != since
                {
                    changed.push(folder_id.clone());
                }
            }
            Err(e) => {
                tracing::debug!(
                    request_id = %request_id,
                    error = %e,
                    "EAS Ping Email/changes probe failed; deferring to push/journal checks"
                );
            }
        }
    }
    changed
}

/// Effective Ping hold time after the Cloudflare edge guard: an explicit
/// `GATEWAY_MAX_PING_HEARTBEAT` cap always wins; otherwise Cloudflare-proxied
/// requests (`CF-RAY` header) are clamped to
/// [`crate::config::DEFAULT_CLOUDFLARE_PING_CAP_SECS`]; direct requests keep
/// the negotiated heartbeat untouched.
fn effective_ping_heartbeat(
    requested: u64,
    configured_cap: Option<u64>,
    via_cloudflare: bool,
) -> u64 {
    match configured_cap {
        Some(cap) => requested.min(cap),
        None if via_cloudflare => requested.min(crate::config::DEFAULT_CLOUDFLARE_PING_CAP_SECS),
        None => requested,
    }
}

async fn handle_ping(
    state: &Arc<AppState>,
    inv: &PingInvocation<'_>,
    wbxml: &Wbxml,
    as_wbxml: bool,
) -> Response {
    let PingInvocation {
        owner,
        password,
        req,
        xml,
        request_id,
    } = *inv;
    const MIN_HEARTBEAT_SECS: u64 = 60;
    const MAX_HEARTBEAT_SECS: u64 = 3540;
    const MAX_PING_FOLDERS: usize = 200;
    let device_id = req.device_id.as_deref().unwrap_or("unknown-device");
    let cache_key = format!("{}:{}", owner, ping_device_key_part(device_id));

    let cached = ping_cache_lookup(&cache_key).await;

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
    // ---- Cloudflare edge-termination guard (audit P0 #1) -----------------
    // Requests proxied through Cloudflare (every cloudflared tunnel request
    // traverses the CF edge and carries a `CF-RAY` header) are terminated
    // with HTTP 524 after 125s without a response. Any Ping heartbeat longer
    // than that silently breaks push: the client sees a 524, retry-loops,
    // and new mail arrives late. Clamp the *effective* hold time (never the
    // protocol-level heartbeat negotiation or the cached parameters) to
    // `config.max_ping_heartbeat_secs` when explicitly configured, otherwise
    // to `DEFAULT_CLOUDFLARE_PING_CAP_SECS` for Cloudflare-proxied requests
    // only. Answering Status 1 early is spec-legal — MS-ASCMD §2.2.3.79.7
    // allows the server to end a Ping at any time — so the client simply
    // re-issues Ping before the edge cuts the connection.
    let effective_heartbeat = effective_ping_heartbeat(
        heartbeat,
        state.cfg.max_ping_heartbeat_secs,
        req.via_cloudflare,
    );
    if effective_heartbeat != heartbeat {
        tracing::debug!(
            request_id = %request_id,
            device = %device_id,
            requested_heartbeat = heartbeat,
            effective_heartbeat,
            "clamping EAS Ping hold time below Cloudflare edge timeout"
        );
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

    ping_cache_store(
        owner,
        cache_key.clone(),
        PingCacheEntry {
            heartbeat,
            folders: folders.clone(),
            last_seen: Instant::now(),
        },
    )
    .await;

    // One outstanding Ping per device: a fresh Ping supersedes any older one
    // still sleeping on this device. Without this, two overlapping Pings both
    // wake on the same change and both answer Status 2 — the classic battery
    // drain storm. The superseded Ping answers Status 1 promptly; the client
    // immediately re-issues and lands on the surviving request.
    let ping_generation = PING_GENERATION.fetch_add(1, AtomicOrdering::Relaxed);
    let supersede_token = CancellationToken::new();
    if let Some((_, previous)) = PING_IN_FLIGHT.insert(
        cache_key.clone(),
        (ping_generation, supersede_token.clone()),
    ) {
        tracing::debug!(
            request_id = %request_id,
            device = %device_id,
            "superseding outstanding Ping for device"
        );
        previous.cancel();
    }
    // Generation-checked removal: a supersede race must never let this Ping's
    // guard delete the newer Ping's entry.
    let _in_flight_guard = PingInFlightGuard {
        key: cache_key.clone(),
        generation: ping_generation,
    };

    // ---- Live push wiring (audit gap #13) ---------------------------------
    // Email change detection previously relied solely on the ≤15s
    // change-journal poll below, so new mail could lag a full tick after
    // arrival. With live JMAP EventSource push (RFC 8620 §7.3) the Ping wakes
    // the instant the mailbox state advances: register the mailbox in the
    // shared push monitor (deduplicated per mailbox, released when this Ping
    // ends via `PingMonitorGuard`) and probe the stored JMAP Email state once
    // up front so a change that landed while no Ping was in flight is also
    // caught (covers the gap between consecutive Pings).
    let email_folder_ids: Vec<String> = folders
        .iter()
        .filter(|f| matches!(ping_folder_kind(f), Some(PingFolderKind::Email)))
        .map(|f| f.id.clone())
        .collect();
    // Subscribe *before* registering the monitor so an event published in
    // between is never lost to this Ping.
    let mut notify_rx = if email_folder_ids.is_empty() {
        None
    } else {
        Some(state.subscription_manager.subscribe_raw())
    };
    let push_guard = if notify_rx.is_none() {
        None
    } else {
        state.jmap_client.as_ref().map(|jmap| {
            state.push_registry.ensure_email_monitor(
                jmap.clone(),
                owner.to_string(),
                password.clone(),
                state.subscription_manager.clone(),
            );
            PingMonitorGuard {
                registry: &state.push_registry,
                owner: owner.to_string(),
            }
        })
    };
    // Resolve the JMAP account once per Ping; reused by every probe below.
    let jmap_account: Option<(Arc<JmapClient>, String)> = if push_guard.is_some() {
        match state.jmap_client.as_ref() {
            Some(jmap) => match jmap.get_account_id(owner, password).await {
                Ok(id) if !id.is_empty() => Some((jmap.clone(), id)),
                _ => None,
            },
            None => None,
        }
    } else {
        None
    };
    let probe_changed = probe_email_folder_changes(
        state,
        owner,
        password,
        device_id,
        &email_folder_ids,
        &jmap_account,
        request_id,
    )
    .await;
    if !probe_changed.is_empty() {
        return ping_changed_response(wbxml, as_wbxml, &probe_changed, request_id);
    }

    // Per-folder journal baselines, used only by folders with no persisted
    // watermark at all (never synced: no `seq:` token and no `journal_seq`
    // column value). Captured once here — before the loop starts — so a
    // change landing after this point is still reported, while pre-existing
    // journal history is not replayed.
    let mut journal_baselines: HashMap<String, i64> = HashMap::new();
    for folder in &folders {
        if matches!(ping_folder_kind(folder), None | Some(PingFolderKind::Email)) {
            continue;
        }
        let collection_id = scoped_collection_id(&folder.id, device_id);
        let watermark = state
            .storage
            .journal_watermark(owner, &collection_id)
            .await
            .ok()
            .flatten();
        if watermark.is_none() {
            let head = state.storage.get_latest_change_seq().await.unwrap_or(0);
            journal_baselines.insert(folder.id.clone(), head);
        }
    }

    let deadline = Instant::now() + StdDuration::from_secs(effective_heartbeat);
    while Instant::now() < deadline {
        if supersede_token.is_cancelled() {
            // This Ping was superseded by a newer one for the same device.
            return ping_superseded_response(wbxml, as_wbxml, request_id);
        }
        let mut changed_folders = Vec::new();
        for folder in &folders {
            let kind = match ping_folder_kind(folder) {
                Some(k) => k,
                None => continue,
            };
            // Email never consults the change journal: its per-device sync
            // state is a JMAP `Email` state token, not a `seq:` watermark, so
            // the journal watermark would have to be fabricated (previously
            // `0`, which matched every calendar/contacts row the owner ever
            // journaled — a permanent Status 2 storm). Email changes arrive
            // exclusively through the live push wake + Email/changes probe.
            if kind == PingFolderKind::Email {
                continue;
            }

            // Watermark persisted by the collection's last sync completion
            // (a `seq:` token or the `journal_seq` column for collections
            // whose sync token carries a provider state). Folders with no
            // watermark at all — never synced — fall back to the journal head
            // captured at Ping start, so only changes *after* this Ping began
            // can fire; the pre-existing journal history surviving a restart
            // is never replayed as a storm.
            let collection_id = scoped_collection_id(&folder.id, device_id);
            let since = state
                .storage
                .journal_watermark(owner, &collection_id)
                .await
                .ok()
                .flatten()
                .unwrap_or_else(|| journal_baselines.get(&folder.id).copied().unwrap_or(0));

            let changed = match kind {
                // Tasks and Notes are gateway-local stores journaled with a
                // distinct resource_href tag ("task" / "note"), so we can scope
                // change detection precisely to those folders.
                PingFolderKind::Tasks => state
                    .storage
                    .list_journal_since_seq(owner, since)
                    .await
                    .unwrap_or_default()
                    .iter()
                    .any(|r| matches!(r.resource_href.as_deref(), Some("task"))),
                PingFolderKind::Notes => state
                    .storage
                    .list_journal_since_seq(owner, since)
                    .await
                    .unwrap_or_default()
                    .iter()
                    .any(|r| matches!(r.resource_href.as_deref(), Some("note"))),
                // Calendar and Contacts changes flow through item_map and the
                // shared change journal (op='upsert') plus delete tombstones.
                PingFolderKind::Calendar | PingFolderKind::Contacts => {
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
                PingFolderKind::Email => unreachable!("email folders excluded above"),
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
        let tick = remaining.min(StdDuration::from_secs(15));
        use tokio::sync::broadcast::error::RecvError;
        // Why the wake happened, if it did before the poll tick elapsed.
        enum Wake {
            Tick,
            OwnerEvent(crate::notifications::NotificationEvent),
            Lagged,
            Superseded,
        }
        let wake = match notify_rx.as_mut() {
            Some(rx) => {
                let tick_deadline = Instant::now() + tick;
                loop {
                    let t = tick_deadline.saturating_duration_since(Instant::now());
                    if t.is_zero() {
                        break Wake::Tick;
                    }
                    tokio::select! {
                        _ = supersede_token.cancelled() => break Wake::Superseded,
                        _ = tokio::time::sleep(t) => break Wake::Tick,
                        recv = rx.recv() => match recv {
                            Ok(ev) if ev.owner() == owner => break Wake::OwnerEvent(ev),
                            // Foreign-owner events share this broadcast feed;
                            // keep waiting out the tick instead of re-running
                            // this Ping's journal/storage queries (which would
                            // scale queries with global notification rate).
                            Ok(_) => continue,
                            Err(RecvError::Lagged(_)) => break Wake::Lagged,
                            Err(RecvError::Closed) => {
                                notify_rx = None;
                                break Wake::Tick;
                            }
                        },
                    }
                }
            }
            None => {
                tokio::select! {
                    _ = supersede_token.cancelled() => Wake::Superseded,
                    _ = tokio::time::sleep(tick) => Wake::Tick,
                }
            }
        };
        match wake {
            Wake::Superseded => {
                return ping_superseded_response(wbxml, as_wbxml, request_id);
            }
            Wake::Tick => {}
            Wake::OwnerEvent(ev) => {
                // Probe JMAP directly so only email folders whose stored state
                // actually advanced are reported — calendar/contacts events and
                // foreign-folder email changes fall through to the journal
                // checks on the next loop pass instead of forcing an
                // over-broad Status 2.
                let changed = if jmap_account.is_some() {
                    probe_email_folder_changes(
                        state,
                        owner,
                        password,
                        device_id,
                        &email_folder_ids,
                        &jmap_account,
                        request_id,
                    )
                    .await
                } else if matches!(ev, crate::notifications::NotificationEvent::NewMail { .. }) {
                    // No JMAP session to verify against. NewMail is an
                    // unambiguous email event, and the push monitor is then
                    // the only email signal this Ping can see, so report the
                    // pinged email folders rather than sleeping through it.
                    email_folder_ids.clone()
                } else {
                    Vec::new()
                };
                if !changed.is_empty() {
                    return ping_changed_response(wbxml, as_wbxml, &changed, request_id);
                }
            }
            Wake::Lagged => {
                // Push events were dropped under burst. The monitor does not
                // write to the change journal, so re-probe JMAP state per
                // folder; without a live JMAP session, conservatively report
                // the email folders rather than risk sleeping through mail.
                let changed = if jmap_account.is_some() {
                    probe_email_folder_changes(
                        state,
                        owner,
                        password,
                        device_id,
                        &email_folder_ids,
                        &jmap_account,
                        request_id,
                    )
                    .await
                } else {
                    email_folder_ids.clone()
                };
                if !changed.is_empty() {
                    return ping_changed_response(wbxml, as_wbxml, &changed, request_id);
                }
            }
        }
    }

    // Timeout reached with no changes
    let xml =
        r#"<?xml version="1.0" encoding="utf-8"?><Ping xmlns="Ping:"><Status>1</Status></Ping>"#;
    xml_or_wbxml_response(wbxml, as_wbxml, xml, request_id)
}

/// Map a Settings Oof Set request's `<OofMessage>` nodes to the per-audience
/// replies and the ExternalAudience they imply ([MS-ASCMD] §2.2.3.122:
/// one OofMessage per audience, each naming it with exactly one
/// AppliesTo* element; §2.2.3.118.1: no AppliesTo* elements at all means
/// the single external reply covers all external senders).
///
/// The WBXML decoder renders a valueless element as
/// `<AppliesToInternal></AppliesToInternal>` (Start, then End) — never the
/// self-closing `<AppliesToInternal/>` form a plain XML request carries —
/// so the audience tags are matched by OPEN-tag prefix: that covers `<Tag>`,
/// `<Tag/>`, and attribute-carrying forms alike, while `</Tag>` can never
/// match (the `/` precedes the name in a close tag).
fn oof_audience_from_oof_messages(
    oof_inner: &str,
) -> (Option<String>, Option<String>, crate::oof::ExternalAudience) {
    let mut internal_reply = extract_first_tag_text(oof_inner, b"InternalReply");
    let mut external_reply = extract_first_tag_text(oof_inner, b"ExternalReply");
    let mut has_known = false;
    let mut has_unknown = false;
    for block in extract_all_tag_blocks(oof_inner, b"OofMessage") {
        let enabled_text = extract_first_tag_text(&block, b"Enabled").unwrap_or_default();
        if enabled_text != "1" {
            continue;
        }
        let reply = extract_first_tag_text(&block, b"ReplyMessage");
        if block.contains("<AppliesToInternal") {
            internal_reply = reply.clone();
        }
        if block.contains("<AppliesToExternalKnown") {
            has_known = true;
            external_reply = reply.clone().or(external_reply);
        }
        if block.contains("<AppliesToExternalUnknown") {
            has_unknown = true;
            external_reply = reply.or(external_reply);
        }
    }
    let external_audience = match (has_known, has_unknown) {
        (true, true) => crate::oof::ExternalAudience::All,
        (true, false) => crate::oof::ExternalAudience::KnownExternal,
        (false, true) => crate::oof::ExternalAudience::External,
        (false, false) => crate::oof::ExternalAudience::All,
    };
    (internal_reply, external_reply, external_audience)
}

/// Handle the EAS `MeetingResponse` command ([MS-ASCMD] §2.2.1.11).
///
/// The request carries 1..N `<Request>` blocks ([MS-ASCMD] §6.25); each is
/// processed independently and answered by one `<Result>` in request order
/// ([MS-ASCMD] §6.26). Per-request failures (invalid item, nonexistent
/// instance, organizer self-response, …) get their own
/// [MS-ASCMD] §2.2.3.177.9 status without aborting the remaining requests.
async fn handle_meeting_response(
    state: &Arc<AppState>,
    username: &str,
    password: &SecretString,
    xml: &str,
    wbxml: &Wbxml,
    as_wbxml: bool,
    request_id: &str,
) -> Response {
    use crate::meeting::message::{MeetingMessageGenerator, MeetingResponseResult};
    use crate::meeting::rsvp::{self, RsvpRequest, RsvpSource};
    use crate::meeting::response::ResponseDecision;

    // §6.25: at least one <Request> is required. A request with none is
    // malformed — the command cannot produce a spec-shaped response.
    let request_blocks = extract_all_tag_blocks(xml, b"Request");
    if request_blocks.is_empty() {
        return bad_request_response(request_id, "MeetingResponse requires a Request element");
    }

    let generator = MeetingMessageGenerator::new();
    let mut results: Vec<MeetingResponseResult> = Vec::with_capacity(request_blocks.len());

    for block in &request_blocks {
        // Address element ([MS-ASCMD] §2.2.3.151 / §2.2.3.98.2): exactly one
        // of RequestId / search:LongId. `RequestId MUST NOT be present … if
        // the search:LongId element is present`.
        let request_id_el = extract_first_tag_text(block, b"RequestId");
        let long_id = extract_first_tag_text(block, b"LongId");
        let address = match (&request_id_el, &long_id) {
            (Some(rid), None) => Some(rid.clone()),
            (None, Some(lid)) => Some(lid.clone()),
            (Some(_), Some(_)) | (None, None) => None,
        };

        // UserResponse ([MS-ASCMD] §2.2.3.194) is required and restricted to
        // 1 (accept) / 2 (tentative) / 3 (decline).
        let user_response = extract_first_tag_text(block, b"UserResponse")
            .and_then(|v| v.trim().parse::<u8>().ok());
        let decision = match user_response {
            Some(1) => Some(ResponseDecision::Accept),
            Some(2) => Some(ResponseDecision::Tentative),
            Some(3) => Some(ResponseDecision::Decline),
            _ => None,
        };

        // InstanceId ([MS-ASCMD] §2.2.3.92.1): dateTime value naming the
        // occurrence. Schema-valid but unparseable → status 104; keep the raw
        // wire string for the response echo.
        let instance_wire = extract_first_tag_text(block, b"InstanceId");
        let instance_id = instance_wire.as_deref().and_then(parse_datetime);
        if instance_wire.is_some() && instance_id.is_none() {
            results.push(MeetingResponseResult {
                request_id: request_id_el.clone(),
                status: rsvp::STATUS_INSTANCE_MALFORMED,
                calendar_id: None,
                instance_id: instance_wire.clone(),
            });
            continue;
        }

        // SendResponse ([MS-ASCMD] §2.2.3.163): present ⇒ email the organizer;
        // absent ⇒ no email. Optional children: airsyncbase:Body (reply text)
        // and ProposedStartTime/ProposedEndTime ([MS-ASCMD] §2.2.3.141 /
        // §2.2.3.140 — Compact DateTime, and each requires the other).
        let send_response = extract_all_tag_blocks(block, b"SendResponse").into_iter().next();
        let send_reply = send_response.is_some();
        let reply_body_text = send_response
            .as_deref()
            .and_then(|inner| extract_first_tag_text(inner, b"Data"));
        let proposed_start = send_response
            .as_deref()
            .and_then(|inner| extract_first_tag_text(inner, b"ProposedStartTime"))
            .and_then(|v| parse_datetime(&v));
        let proposed_end = send_response
            .as_deref()
            .and_then(|inner| extract_first_tag_text(inner, b"ProposedEndTime"))
            .and_then(|v| parse_datetime(&v));

        let invalid = |results: &mut Vec<MeetingResponseResult>,
                       request_id_el: &Option<String>,
                       instance_wire: &Option<String>| {
            results.push(MeetingResponseResult {
                request_id: request_id_el.clone(),
                status: rsvp::STATUS_INVALID_ITEM,
                calendar_id: None,
                instance_id: instance_wire.clone(),
            });
        };

        if address.is_none() || decision.is_none() {
            invalid(&mut results, &request_id_el, &instance_wire);
            continue;
        }
        // §2.2.3.140/§2.2.3.141: the proposed-time pair is all-or-nothing.
        if proposed_start.is_some() != proposed_end.is_some() {
            invalid(&mut results, &request_id_el, &instance_wire);
            continue;
        }

        let address = address.expect("checked above");
        let decision = decision.expect("checked above");

        // Resolve the addressed item. `em-` prefixed ids are JMAP emails
        // (meeting-request messages); anything else is looked up in the
        // calendar item map.
        let source = if let Some(jmap_email_id) =
            crate::email::jmap_id_from_email_server_id(&address)
        {
            RsvpSource::Email {
                jmap_email_id: jmap_email_id.to_string(),
            }
        } else {
            let known_calendar_item = state
                .storage
                .get_ews_item_by_server_id(username, &address)
                .await
                .ok()
                .flatten()
                .is_some();
            if known_calendar_item {
                RsvpSource::CalendarItem {
                    server_id: address.clone(),
                }
            } else {
                // [MS-ASCMD] §2.2.3.177.9: "referencing an item other than a
                // meeting request, email, or calendar item".
                invalid(&mut results, &request_id_el, &instance_wire);
                continue;
            }
        };

        let req = RsvpRequest {
            decision,
            instance_id,
            send_reply,
            reply_body_text,
            proposed_start,
            proposed_end,
        };

        let outcome = match rsvp::apply_rsvp(state, username, password, &source, &req).await {
            Ok(outcome) => outcome,
            Err(failure) => rsvp::RsvpOutcome::failure(&failure),
        };

        // [MS-ASCMD] §2.2.3.18: CalendarId is returned "if the meeting
        // request was not declined" — never for a decline.
        let calendar_id = match decision {
            ResponseDecision::Decline => None,
            _ => outcome.calendar_server_id.clone(),
        };

        results.push(MeetingResponseResult {
            request_id: request_id_el.clone(),
            status: outcome.status,
            calendar_id,
            instance_id: instance_wire.clone(),
        });
    }

    let payload = generator.generate_eas_meeting_response(&results);
    xml_or_wbxml_response(wbxml, as_wbxml, &payload, request_id)
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
        // A Settings command Oof Set request carries OofState, optional
        // StartTime/EndTime (when OofState is 2), and one OofMessage node
        // per audience ([MS-ASCMD] §2.2.3.122).
        let oof_state_text =
            extract_first_tag_text(oof_inner, b"OofState").unwrap_or("0".to_string());
        let (enabled, time_based) = match oof_state_text.as_str() {
            "2" => (true, true),
            "1" => (true, false),
            _ => (false, false),
        };
        let start_time = extract_first_tag_text(oof_inner, b"StartTime")
            .and_then(|s| chrono::DateTime::parse_from_rfc3339(&s).ok())
            .map(|dt| dt.with_timezone(&chrono::Utc));
        let end_time = extract_first_tag_text(oof_inner, b"EndTime")
            .and_then(|s| chrono::DateTime::parse_from_rfc3339(&s).ok())
            .map(|dt| dt.with_timezone(&chrono::Utc));

        // Audience-specific messages arrive as OofMessage nodes, each naming
        // its audience with exactly one of the AppliesTo* elements
        // ([MS-ASCMD] §2.2.3.123).
        let (internal_reply, external_reply, external_audience) =
            oof_audience_from_oof_messages(oof_inner);

        let settings = crate::oof::OofSettings {
            enabled,
            external_audience,
            internal_reply,
            external_reply,
            start_time: if time_based { start_time } else { None },
            end_time: if time_based { end_time } else { None },
        };

        if let Some(oof_mgr) = &state.oof_manager {
            let _ = oof_mgr.set_oof_settings(username, settings);
            // Ignore result; we'll still respond success.
        }
    }

    if has_user_info || (!has_oof && !has_device_password) {
        // [MS-ASCMD] §2.2.3.192/§2.2.3.5: for 16.1 clients the addresses are
        // returned under UserInformation/Get/Accounts/Account, with a single
        // EmailAddresses container holding one SMTPAddress per address plus
        // the primary SMTP address ([MS-ASCMD] §2.2.3.57/§2.2.3.171).
        let smtp_addresses = active_user_emails(username, &state.cfg.mail_domain)
            .into_iter()
            .map(|email| format!("<SMTPAddress>{}</SMTPAddress>", xml_escape(&email)))
            .collect::<String>();
        sections.push_str(&format!(
            r#"<UserInformation>
    <Status>1</Status>
    <Get>
      <Accounts>
        <Account>
          <AccountId>1</AccountId>
          <AccountName>{}</AccountName>
          <SendDisabled>0</SendDisabled>
          <EmailAddresses>
            {}
            <PrimarySmtpAddress>{}</PrimarySmtpAddress>
          </EmailAddresses>
        </Account>
      </Accounts>
    </Get>
  </UserInformation>"#,
            xml_escape(&primary_email),
            smtp_addresses,
            xml_escape(&primary_email)
        ));
    }
    if has_oof {
        // Query OOF settings for this user. The Oof Get response carries
        // OofState, optional StartTime/EndTime, and one OofMessage node per
        // audience ([MS-ASCMD] §2.2.3.122/§2.2.3.123).
        let oof_section = if let Some(oof_mgr) = &state.oof_manager {
            match oof_mgr.get_oof_settings(username) {
                Ok(settings) => {
                    let (state_val, time_based) = if settings.enabled
                        && settings.start_time.is_some()
                        && settings.end_time.is_some()
                    {
                        ("2", true)
                    } else if settings.enabled {
                        ("1", false)
                    } else {
                        ("0", false)
                    };
                    let duration = if time_based {
                        format!(
                            "<StartTime>{}</StartTime><EndTime>{}</EndTime>",
                            settings.start_time.unwrap().to_rfc3339(),
                            settings.end_time.unwrap().to_rfc3339()
                        )
                    } else {
                        String::new()
                    };
                    let internal_reply =
                        xml_escape(settings.internal_reply.as_deref().unwrap_or(""));
                    let external_reply =
                        xml_escape(settings.external_reply.as_deref().unwrap_or(""));

                    let enabled_flag = |on: bool| if on { "1" } else { "0" };
                    let (external_known, external_unknown) = match settings.external_audience {
                        crate::oof::ExternalAudience::All => (true, true),
                        crate::oof::ExternalAudience::KnownExternal => (true, false),
                        crate::oof::ExternalAudience::External => (false, true),
                    };
                    let oof_message = |audience: &str, on: bool, reply: &str| {
                        format!(
                            "<OofMessage><{}/><Enabled>{}</Enabled><ReplyMessage>{}</ReplyMessage><BodyType>1</BodyType></OofMessage>",
                            audience,
                            enabled_flag(on),
                            reply
                        )
                    };
                    format!(
                        r#"<Oof>
    <Status>1</Status>
    <Get>
      <OofState>{}</OofState>
      {}
      {}{}{}
    </Get>
  </Oof>"#,
                        state_val,
                        duration,
                        oof_message("AppliesToInternal", settings.enabled, &internal_reply),
                        if external_known {
                            oof_message("AppliesToExternalKnown", settings.enabled, &external_reply)
                        } else {
                            String::new()
                        },
                        if external_unknown {
                            oof_message(
                                "AppliesToExternalUnknown",
                                settings.enabled,
                                &external_reply,
                            )
                        } else {
                            String::new()
                        },
                    )
                }
                Err(e) => {
                    tracing::warn!(target: "eas", user = %username, error = %e, "Failed to get OOF settings");
                    // Return OOF disabled in error case.
                    r#"<Oof>
    <Status>1</Status>
    <Get>
      <OofState>0</OofState>
      <OofMessage><AppliesToInternal/><Enabled>0</Enabled><ReplyMessage></ReplyMessage><BodyType>1</BodyType></OofMessage>
      <OofMessage><AppliesToExternalKnown/><Enabled>0</Enabled><ReplyMessage></ReplyMessage><BodyType>1</BodyType></OofMessage>
      <OofMessage><AppliesToExternalUnknown/><Enabled>0</Enabled><ReplyMessage></ReplyMessage><BodyType>1</BodyType></OofMessage>
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
      <OofMessage><AppliesToInternal/><Enabled>0</Enabled><ReplyMessage></ReplyMessage><BodyType>1</BodyType></OofMessage>
      <OofMessage><AppliesToExternalKnown/><Enabled>0</Enabled><ReplyMessage></ReplyMessage><BodyType>1</BodyType></OofMessage>
      <OofMessage><AppliesToExternalUnknown/><Enabled>0</Enabled><ReplyMessage></ReplyMessage><BodyType>1</BodyType></OofMessage>
    </Get>
  </Oof>"#
                .to_string()
        };

        sections.push_str(&oof_section);
    }
    if has_device_password {
        // The DevicePassword response reports the status of the Set
        // operation only ([MS-ASCMD] §2.2.3.46).
        sections.push_str("<DevicePassword><Status>1</Status></DevicePassword>");
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
                    r#"<?xml version="1.0" encoding="utf-8"?><GetItemEstimate xmlns="GetItemEstimate:"><Response><Status>4</Status><Collection><CollectionId>{}</CollectionId><Estimate>0</Estimate></Collection></Response></GetItemEstimate>"#,
                    visible_collection_id
                );
                return xml_or_wbxml_response(wbxml, as_wbxml, &xml, request_id);
            }
        }
    }
    // Use the persisted journal watermark (a `seq:` token or the `journal_seq`
    // column recorded at sync completion). A collection whose token is a
    // provider state (JMAP query_state) previously fell through to 0 and
    // counted the entire journal — wrong (overcount) before pruning and
    // wrong (undercount) once pruning has removed rows below watermarks.
    let since = if incoming == "0" {
        0
    } else {
        state
            .storage
            .journal_watermark(owner, &collection_id)
            .await
            .ok()
            .flatten()
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

/// Parse an ItemOperations byte-range value ([MS-ASCMD] §2.2.3.143.2):
/// "a string value in the format \"m-n\", where m is less than or equal to
/// n … The byte range is zero-indexed". Returns the half-open `[m, n+1)`
/// window, or `None` when the value is malformed (m > n, unparsable) —
/// the caller answers with status 8 ("The byte-range is invalid or too
/// large").
fn parse_item_operations_byte_range(raw: &str) -> Option<std::ops::Range<u64>> {
    let (m, n) = raw.split_once('-')?;
    let m: u64 = m.trim().parse().ok()?;
    let n: u64 = n.trim().parse().ok()?;
    if m > n {
        return None;
    }
    // n is the inclusive last byte; overflow-safe n+1. A range whose end is
    // u64::MAX cannot name a real byte, so treat it as malformed rather
    // than wrapping to 0.
    Some(m..n.checked_add(1)?)
}

/// Append `bytes` base64-encoded to `out` in fixed-size chunks
/// ([MS-ASDTYPE] §2.3 byte content: "a base64 encoding of the binary
/// document" — [MS-ASCMD] §2.2.3.39.2) without materializing a second
/// full-size copy of the content: the previous
/// `BASE64.encode(&bytes)` step allocated the entire encoded body as a
/// temporary before it was pushed into the response string.
fn push_base64(out: &mut String, bytes: &[u8]) {
    const CHUNK: usize = 3 * 1024;
    let mut scratch = String::with_capacity(4 * 1024);
    for piece in bytes.chunks(CHUNK) {
        scratch.clear();
        BASE64.encode_string(piece, &mut scratch);
        out.push_str(&scratch);
    }
}

/// One ItemOperations Fetch outcome: either a per-Fetch status
/// ([MS-ASCMD] §2.2.3.177.8) or the attachment bytes the response carries.
///
/// `AttachmentFetchOutcome` holds the *raw* (unencoded) bytes for the
/// requested window plus the metadata the response `Properties` needs:
/// `AirSyncBase:ContentType`, `Total` ([MS-ASCMD] §2.2.3.184.2) and the
/// authoritative `Range` ([MS-ASCMD] §2.2.3.143.2). Base64 (inline
/// delivery) or part indexing (multipart delivery, §2.2.1.10.1) is applied
/// by the renderer.
#[derive(Debug)]
struct AttachmentFetchOutcome {
    /// Window bytes (already sliced when the request carried a Range).
    data: Vec<u8>,
    content_type: String,
    /// Whole-item size in bytes; `Total` is omitted from the response when
    /// the backend does not report it ([MS-ASCMD] §2.2.3.184.2, 0...1).
    total_size: Option<u64>,
    /// The authoritative half-open `[start, end)` window `data` covers.
    window: (u64, u64),
    /// Whether the window was served in full; `false` → Status 17
    /// ("Partial success; a Fetch … operation completed partially").
    complete: bool,
}

/// Fetch an email attachment's bytes from the user's JMAP account, honoring
/// the request's byte-range ([MS-ASCMD] §2.2.3.143.2).
///
/// EAS email attachment `FileReference`s carry the JMAP blobId. The
/// credentials are the requesting user's own, so a blobId from another
/// mailbox simply fails at Stalwart with 403/404 — classified by
/// [`crate::jmap::BlobFetchError`] into the ItemOperations statuses below.
///
/// Returns the Fetch-scoped [MS-ASCMD] §2.2.3.177.8 status on error:
/// 3 (server error), 8 (byte-range invalid or too large), 11 (requested
/// data size too large), 15 (attachment or attachment ID is invalid) or
/// 16 (access to the resource is denied).
async fn handle_email_attachment_fetch(
    jmap: &Arc<JmapClient>,
    username: &str,
    password: &SecretString,
    blob_id: &str,
    byte_range: Option<std::ops::Range<u64>>,
    max_bytes: usize,
) -> Result<AttachmentFetchOutcome, u16> {
    let account_id = jmap
        .get_account_id(username, password)
        .await
        .map_err(|_| 3u16)?;
    let fetch = jmap
        .download_blob_range(
            &account_id,
            blob_id,
            username,
            password,
            byte_range,
            max_bytes,
        )
        .await
        .map_err(|e| {
            use crate::jmap::BlobFetchError as E;
            tracing::error!(
                file_reference = %blob_id,
                error = %e,
                "ItemOperations JMAP attachment fetch failed"
            );
            match e {
                E::InvalidBlobId | E::NotFound(_) => 15u16,
                E::AccessDenied(_) => 16,
                E::TooLarge { .. } => 11,
                E::RangeTooLarge { .. } | E::RangeStartsPastEof { .. } => 8,
                E::Server(_) => 3,
            }
        })?;
    if fetch.data.is_empty() && fetch.window == (0, 0) {
        // [MS-ASCMD] §2.2.3.94.1 status 10: "The file is empty." A served
        // window is (0,0) only when the item itself holds no bytes; a
        // short ranged window of a non-empty file is complete=false → 17.
        return Err(10);
    }
    Ok(AttachmentFetchOutcome {
        data: fetch.data,
        content_type: fetch.content_type,
        total_size: fetch.total_size,
        window: fetch.window,
        complete: fetch.complete,
    })
}

/// Slice a gateway-managed (calendar) attachment's stored content into an
/// [`AttachmentFetchOutcome`] for the requested byte window
/// ([MS-ASCMD] §2.2.3.143.2), applying the same window semantics the JMAP
/// path enforces.
///
/// Returns the Fetch-scoped [MS-ASCMD] §2.2.3.177.8 status on error:
/// - 15: the stored content is not valid base64 — the attachment or
///   attachment ID is invalid ([MS-ASDTYPE] §2.3 byte content).
/// - 11: no Range was requested and the whole item exceeds the budget
///   ("The requested data size is too large").
/// - 8: the Range starts at/after the end of the content, or the window
///   exceeds the budget ("The byte-range is invalid or too large").
fn calendar_attachment_outcome(
    attachment: &crate::attachment::FileAttachment,
    byte_range: Option<std::ops::Range<u64>>,
    max_bytes: usize,
) -> Result<AttachmentFetchOutcome, u16> {
    let content = BASE64
        .decode(attachment.content_base64.trim())
        .map_err(|e| {
            tracing::error!(
                attachment_id = %attachment.id,
                error = %e,
                "Stored calendar attachment content is not valid base64"
            );
            // [MS-ASCMD] §2.2.3.177.8 status 15: "Attachment fetch provider -
            // Attachment or attachment ID is invalid."
            15u16
        })?;
    let total = content.len() as u64;
    if total == 0 && byte_range.is_none() {
        // [MS-ASCMD] §2.2.3.94.1 status 10: "The file is empty."
        return Err(10);
    }
    let (window, complete) = if let Some(range) = byte_range {
        if range.start >= total {
            // [MS-ASCMD] §2.2.3.177.8 status 8: "The byte-range is invalid
            // or too large" — no byte of the range exists in the item.
            return Err(8);
        }
        if range.end - range.start > max_bytes as u64 {
            return Err(8);
        }
        // The window clamps at EOF when it reaches past the content;
        // the response reports the authoritative shortened window and
        // status 17 ("Partial success").
        let end = range.end.min(total);
        ((range.start, end), end >= range.end)
    } else {
        if total > max_bytes as u64 {
            // [MS-ASCMD] §2.2.3.177.8 status 11: "The requested data
            // size is too large" — the client can retry with explicit
            // windowed ranges.
            return Err(11);
        }
        ((0, total), true)
    };
    let data = content[window.0 as usize..window.1 as usize].to_vec();
    Ok(AttachmentFetchOutcome {
        data,
        content_type: if attachment.content_type.is_empty() {
            "application/octet-stream".to_string()
        } else {
            attachment.content_type.clone()
        },
        total_size: Some(total),
        window,
        complete,
    })
}

/// Fetch one email item for ItemOperations, rendering `<Properties>` per
/// the resolved request/sticky options ([MS-ASCMD] §2.2.3.125.4,
/// [MS-ASCON] §3.2.5.7, [MS-ASAIRS] §2.2.2.12).
///
/// Returns the Fetch-scoped [MS-ASCMD] §2.2.3.177.8 status on error:
/// 6 (object not found), 3 (server error), 14 (conversion failure) or 164
/// (a BodyPartPreference whose Type is not 2, [MS-ASCON] §3.2.5.7).
async fn handle_email_item_fetch(
    state: &Arc<AppState>,
    username: &str,
    password: &SecretString,
    options: &crate::eas_sync_options::EasSyncCollectionOptions,
    server_id: &str,
) -> Result<String, u16> {
    let jmap = match state.jmap_client.as_ref() {
        Some(j) if state.cfg.email_enabled => j.clone(),
        _ => {
            // The email backend is unreachable from this deployment: a
            // server-side failure, not a missing object.
            return Err(3);
        }
    };
    let Some(jmap_id) = crate::email::jmap_id_from_email_server_id(server_id) else {
        // Malformed server id: the object it names cannot exist.
        return Err(6);
    };
    let account_id = match jmap.get_account_id(username, password).await {
        Ok(id) => id,
        Err(_) => return Err(3),
    };
    let emails = match jmap
        .get_emails(
            &account_id,
            std::slice::from_ref(&jmap_id.to_string()),
            None,
            username,
            password,
            true,
        )
        .await
    {
        Ok(emails) => emails,
        Err(_) => return Err(3),
    };
    let Some(email) = emails.into_iter().next() else {
        return Err(6);
    };

    // [MS-ASCON] §3.2.5.7 presence table: a BodyPart is emitted when the
    // request carried a BodyPartPreference; the Body element is emitted
    // unless the request carried ONLY a BodyPartPreference. A
    // BodyPartPreference Type other than 2 is answered with status 164.
    let part_pref = options.body_part_preference.clone();
    if let Some(pref) = &part_pref
        && pref.body_type != 2
    {
        return Err(164);
    }
    let body_pref_requested = !options.body_preferences.is_empty();
    let mut properties = String::new();
    if body_pref_requested || part_pref.is_none() {
        properties.push_str(&crate::email::render_negotiated_eas_body(
            &email, options, true,
        ));
    }
    if let Some(pref) = &part_pref {
        properties.push_str(&crate::email::render_eas_body_part(&email, pref));
    }
    Ok(properties)
}

/// Resolve the effective options for one ItemOperations Fetch:
/// [MS-ASCMD] §2.2.3.125.6 — "The server preserves the Options block across
/// requests, using a concept referred to as 'sticky options'. If the Options
/// block is not included in a request, the previous Options block is used."
/// Sync persists that block under the device-scoped key
/// `"{collection_id}::{device_id}"` (`scoped_collection_id`), so this lookup
/// MUST use the same scoped key or the stored row is never found and the
/// Fetch silently falls back to default options.
async fn item_operations_sticky_options(
    state: &Arc<AppState>,
    username: &str,
    collection_id: &str,
    device_id: &str,
    request_options: Option<crate::eas_sync_options::EasSyncCollectionOptions>,
) -> crate::eas_sync_options::EasSyncCollectionOptions {
    let sticky_key = scoped_collection_id(collection_id, device_id);
    let sticky = state
        .storage
        .get_sync_collection_options(username, &sticky_key)
        .await
        .ok()
        .flatten();
    crate::eas_sync_options::EasSyncCollectionOptions::resolve_sticky(sticky, request_options)
        .unwrap_or_default()
}

/// Per-Fetch error response ([MS-ASCMD] §2.2.3.177.8). Per the
/// §2.2.3.67.1 response-Fetch child table, `Status` leads and the
/// address elements the request used are echoed after it; the response
/// Fetch carries no `Store` child.
fn item_operations_fetch_error_xml(address_xml: &str, status: u16) -> String {
    format!("<Fetch><Status>{}</Status>{}</Fetch>", status, address_xml)
}

/// Render the `<Properties>` element of a successful attachment Fetch
/// ([MS-ASCMD] §2.2.3.139.2): `AirSyncBase:ContentType`
/// ([MS-ASAIRS] §2.2.2.18.2), the whole-item size in `Total`
/// ([MS-ASCMD] §2.2.3.184.2), the authoritative byte window in `Range`
/// ([MS-ASCMD] §2.2.3.143.2 — emitted only when the request carried a
/// Range; the §4.10.4.4 whole-attachment example has no Range), and the
/// content itself: base64 in `Data` for inline delivery ([MS-ASCMD]
/// §2.2.3.39.2: "The content of the Data element is a base64 encoding of
/// the binary document, attachment, or body data") or a `Part` index for
/// multipart delivery (§2.2.3.130: for a mailbox attachment "the Part
/// element is a child of the Properties element").
fn render_attachment_properties(
    outcome: &AttachmentFetchOutcome,
    range_requested: bool,
    part_index: Option<u32>,
) -> String {
    let mut xml = String::with_capacity(512 + (outcome.data.len() / 3) * 4);
    xml.push_str("<Properties>");
    let _ = write!(
        xml,
        "<AirSyncBase:ContentType>{}</AirSyncBase:ContentType>",
        xml_escape(&outcome.content_type)
    );
    if let Some(total) = outcome.total_size {
        let _ = write!(xml, "<Total>{}</Total>", total);
    }
    if range_requested {
        // Inclusive last byte per the "m-n" wire format.
        let _ = write!(
            xml,
            "<Range>{}-{}</Range>",
            outcome.window.0,
            outcome.window.1 - 1
        );
    }
    match part_index {
        Some(index) => {
            let _ = write!(xml, "<Part>{}</Part>", index);
        }
        None => {
            xml.push_str("<Data>");
            push_base64(&mut xml, &outcome.data);
            xml.push_str("</Data>");
        }
    }
    xml.push_str("</Properties>");
    xml
}

/// Move the fetched email body's `AirSyncBase:Data` content out of the
/// rendered XML so it can travel as a raw binary part of a multipart
/// ItemOperations response ([MS-ASCMD] §2.2.1.10.1, §2.2.3.130: the Part
/// element "replaces the Data element … as a child of the
/// airsyncbase:Body element").
///
/// Returns the XML with the Body's `Data` replaced by `<Part>n</Part>`
/// plus the unescaped body content for the binary part. `None` means the
/// properties carry no inline body Data (withheld or empty), in which case
/// they are emitted unchanged.
fn extract_body_for_multipart(properties: &str, part_index: u32) -> Option<(String, String)> {
    const OPEN: &str = "<AirSyncBase:Body>";
    const DATA_OPEN: &str = "<AirSyncBase:Data>";
    const DATA_CLOSE: &str = "</AirSyncBase:Data>";
    let body_start = properties.find(OPEN)? + OPEN.len();
    let data_open = properties[body_start..].find(DATA_OPEN)? + body_start;
    let data_close = properties[data_open..].find(DATA_CLOSE)? + data_open;
    let escaped = &properties[data_open + DATA_OPEN.len()..data_close];
    // The renderer emitted XML-escaped character data ([MS-ASDTYPE] §2.3);
    // the binary part carries the raw characters ([MS-ASCMD] §4.10.5.2).
    let mut raw = String::with_capacity(escaped.len());
    let mut chars = escaped.char_indices();
    while let Some((i, c)) = chars.next() {
        if c != '&' {
            raw.push(c);
            continue;
        }
        let rest = &escaped[i + 1..];
        let semi = rest.find(';')?;
        let replacement = crate::util::resolve_xml_reference_strict(&rest[..semi])?;
        raw.push_str(&replacement);
        for _ in 0..semi + 1 {
            chars.next();
        }
    }
    let mut out =
        String::with_capacity(properties.len() + 16 - (data_close - data_open - DATA_OPEN.len()));
    out.push_str(&properties[..data_open]);
    let _ = write!(out, "<Part>{}</Part>", part_index);
    out.push_str(&properties[data_close + DATA_CLOSE.len()..]);
    Some((out, raw))
}

/// Assemble a multipart ItemOperations response body per [MS-ASCMD]
/// §2.2.1.10.1.1: `PartsCount` (4 bytes, little-endian), one `PartMetaData`
/// (§2.2.1.10.1.1.1: `Offset` + `Length`, 4 bytes each, little-endian) per
/// part, then the parts themselves — part 0 is the WBXML, subsequent parts
/// the raw content ([MS-ASCMD] §2.2.1.10.1: "a multipart structure with the
/// WBXML being the first part, and the requested data populating the
/// subsequent parts"). Offsets count from the first byte of the
/// `MultiPartResponse` structure.
fn multipart_item_operations_response(
    wbxml: &Wbxml,
    xml: &str,
    content_parts: Vec<Vec<u8>>,
    request_id: &str,
) -> Response {
    let wbxml_part = match wbxml.encode(xml) {
        Ok(b) => b,
        Err(e) => {
            tracing::error!(
                request_id = %request_id,
                error = %e,
                "WBXML encode failed for multipart ItemOperations response"
            );
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                [(header::CONTENT_TYPE.as_str(), "text/plain; charset=utf-8")],
                format!("WBXML Encode Err: {}", e).into_bytes(),
            )
                .into_response();
        }
    };
    let mut parts = Vec::with_capacity(content_parts.len() + 1);
    parts.push(wbxml_part);
    parts.extend(content_parts);
    let metadata_len = 4 + 8 * parts.len();
    let total_len = metadata_len + parts.iter().map(|p| p.len()).sum::<usize>();
    let Ok(total_len) = u32::try_from(total_len) else {
        tracing::error!(
            request_id = %request_id,
            total_len,
            "Multipart ItemOperations response exceeds the 4-byte PartMetaData offset space"
        );
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            [(header::CONTENT_TYPE.as_str(), "text/plain; charset=utf-8")],
            "Multipart response too large".to_string().into_bytes(),
        )
            .into_response();
    };
    let mut body = Vec::with_capacity(total_len as usize);
    body.extend_from_slice(&(parts.len() as u32).to_le_bytes());
    let mut offset = metadata_len as u32;
    for part in &parts {
        body.extend_from_slice(&offset.to_le_bytes());
        body.extend_from_slice(&(part.len() as u32).to_le_bytes());
        offset += part.len() as u32;
    }
    for part in parts {
        body.extend_from_slice(&part);
    }
    let mut r = (
        StatusCode::OK,
        [(
            header::CONTENT_TYPE.as_str(),
            "application/vnd.ms-sync.multipart",
        )],
        body,
    )
        .into_response();
    inject_common_headers(&mut r, request_id);
    r
}

/// Whether the request asked for multipart content delivery via the
/// `MS-ASAcceptMultiPart` header ([MS-ASHTTP] §2.2.1.1.2.5): "'T' (TRUE)
/// … the client is requesting that the server return content in multipart
/// format. If the header is not present, or is present and set to 'F', the
/// client is requesting that the server return content in inline format."
fn multipart_requested(headers: &HeaderMap) -> bool {
    headers
        .get("MS-ASAcceptMultiPart")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.trim().eq_ignore_ascii_case("T"))
}

#[allow(clippy::too_many_arguments)] // irreducible EAS request params
async fn handle_item_operations(
    state: &Arc<AppState>,
    username: &str,
    password: SecretString,
    xml: &str,
    wbxml: &Wbxml,
    as_wbxml: bool,
    request_id: &str,
    device_id: &str,
    multipart: bool,
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
    // Binary parts of the multipart response ([MS-ASCMD] §2.2.1.10.1);
    // part n+1 holds the content the WBXML's <Part>n</Part> references.
    let mut content_parts: Vec<Vec<u8>> = Vec::new();
    // The same configured attachment budget caps both a single Fetch and
    // the cumulative content bytes one ItemOperations response may carry
    // ([MS-ASCMD] §2.2.3.67: "Multiple fetch operations can be included
    // within one ItemOperations request", so the per-request total needs a
    // bound of its own — the request-body limit only bounds the Fetch
    // *addresses*, not the attachment data each one materializes). A Fetch
    // that would overflow the remainder is answered per
    // [MS-ASCMD] §2.2.3.177.8 status 11 ("The requested data size is too
    // large", Item scope) while the remaining Fetches still execute in
    // request order.
    let attachment_budget = state.cfg.max_attachment_bytes();
    let mut served_content_bytes: usize = 0;
    for fetch in fetches {
        let store = if fetch.store.is_empty() {
            "Mailbox".to_string()
        } else {
            fetch.store
        };
        let collection_id = fetch.collection_id.unwrap_or_else(|| "1".to_string());
        // Address echo for the response Fetch per the §2.2.3.67.1 child
        // table: airsyncbase:FileReference, or airsync:CollectionId with
        // airsync:ServerId, or airsync:CollectionId with search:LongId —
        // whichever form the request addressed the Fetch with.
        let address_xml = if let Some(fr) = fetch.file_reference.as_deref() {
            format!(
                "<AirSyncBase:FileReference>{}</AirSyncBase:FileReference>",
                xml_escape(fr)
            )
        } else if let Some(sid) = fetch.server_id.as_deref() {
            format!(
                "<AirSync:CollectionId>{}</AirSync:CollectionId><AirSync:ServerId>{}</AirSync:ServerId>",
                xml_escape(&collection_id),
                xml_escape(sid)
            )
        } else if let Some(lid) = fetch.long_id.as_deref() {
            format!(
                "<AirSync:CollectionId>{}</AirSync:CollectionId><Search:LongId>{}</Search:LongId>",
                xml_escape(&collection_id),
                xml_escape(lid)
            )
        } else {
            String::new()
        };

        // [MS-ASCMD] §2.2.3.178.2: the only valid Store values are
        // "Mailbox" and "Document Library"; this gateway has no document
        // library backend, so anything but Mailbox is answered with
        // §2.2.3.177.8 status 9 ("The store is unknown or unsupported").
        if !store.eq_ignore_ascii_case("Mailbox") {
            responses.push_str(&item_operations_fetch_error_xml(&address_xml, 9));
            continue;
        }

        // [MS-ASCMD] §2.2.3.125.6: a Fetch without its own <Options> reuses
        // the sticky block the previous Sync request established for this
        // device-scoped collection.
        let resolved_fetch_options = item_operations_sticky_options(
            state,
            username,
            &collection_id,
            device_id,
            fetch.options.clone(),
        )
        .await;

        if let Some(file_ref) = fetch.file_reference.as_deref() {
            // [MS-ASCMD] §2.2.3.143.2: "If the airsyncbase:FileReference
            // element is present in the request, then the Range element is
            // the only valid child element of the Options element." Any
            // other option is rejected per §2.2.3.125.3 with status 2
            // ("If the client specifies an option that is invalid for the
            // parent element, the server returns a Status element value
            // of 2").
            if let Some(options) = &fetch.options
                && options.is_meaningful()
            {
                responses.push_str(&item_operations_fetch_error_xml(&address_xml, 2));
                continue;
            }
            // A present-but-malformed Range is a byte-range violation:
            // status 8 ("The byte-range is invalid or too large").
            let byte_range = match fetch.byte_range.as_deref() {
                Some(raw) => match parse_item_operations_byte_range(raw) {
                    Some(r) => Some(r),
                    None => {
                        tracing::debug!(range = %raw, "ItemOperations: malformed Range value");
                        responses.push_str(&item_operations_fetch_error_xml(&address_xml, 8));
                        continue;
                    }
                },
                None => None,
            };
            let range_requested = byte_range.is_some();
            let max_bytes = attachment_budget;

            // The attachment bytes come from one of two stores, selected by
            // the FileReference itself: gateway-managed calendar attachments
            // live in the gateway's SQLite store, everything else is a JMAP
            // blobId fetched from the user's own account. Both funnel into
            // the same window slicing and the same response renderer; only
            // the byte source and the failure classification differ.
            let outcome: Result<(AttachmentFetchOutcome, bool), u16> = async {
                match state
                    .attachment_manager
                    .get_attachment(&owner_lower, file_ref)
                    .await
                {
                    Ok(Some(attachment)) => {
                        // Gateway-managed (calendar) attachment: enforce the
                        // parent item's read permission first. Without an
                        // owner row the access cannot be established, so the
                        // request is denied ([MS-ASCMD] §2.2.3.177.8 status
                        // 16, "Access to the resource is denied").
                        let item_owner = match state
                            .storage
                            .get_item_owner(&attachment.parent_item_server_id)
                            .await
                        {
                            Ok(Some(o)) => o,
                            Ok(None) => return Err(16),
                            Err(e) => {
                                tracing::error!(error = %e, "Failed to get item owner");
                                return Err(3);
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
                            Ok(false) => return Err(16),
                            Err(e) => {
                                tracing::error!(error = %e, "Permission check failed");
                                return Err(3);
                            }
                        }
                        calendar_attachment_outcome(&attachment, byte_range, max_bytes)
                            .map(|outcome| (outcome, true))
                    }
                    Ok(None) => {
                        // Not a gateway-managed attachment: EAS email
                        // attachment FileReferences carry the JMAP blobId, so
                        // fetch the bytes from the user's own JMAP account.
                        let Some(jmap) = state
                            .jmap_client
                            .clone()
                            .filter(|_| state.cfg.email_enabled)
                        else {
                            // The email backend is unreachable from this
                            // deployment: a server-side failure.
                            return Err(3);
                        };
                        handle_email_attachment_fetch(
                            &jmap, username, &password, file_ref, byte_range, max_bytes,
                        )
                        .await
                        .map(|outcome| (outcome, false))
                    }
                    Err(e) => {
                        tracing::error!(
                            "ItemOperations attachment fetch error for {}: {}",
                            file_ref,
                            e
                        );
                        // The attachment store itself failed: a server-side
                        // failure, not a missing attachment.
                        Err(3)
                    }
                }
            }
            .await;

            match outcome {
                Ok((mut outcome, is_calendar)) => {
                    // The cumulative per-request budget: a single Fetch is
                    // already capped at `attachment_budget` by the outcome
                    // itself, so this only trips once earlier Fetches of the
                    // same request have consumed the remainder — exactly the
                    // status 11 case ("The requested data size is too large",
                    // Item scope, [MS-ASCMD] §2.2.3.177.8).
                    if served_content_bytes.saturating_add(outcome.data.len()) > attachment_budget {
                        tracing::debug!(
                            served_bytes = served_content_bytes,
                            fetch_bytes = outcome.data.len(),
                            budget = attachment_budget,
                            "ItemOperations: per-request attachment budget exhausted"
                        );
                        responses.push_str(&item_operations_fetch_error_xml(&address_xml, 11));
                        continue;
                    }
                    served_content_bytes += outcome.data.len();
                    // [MS-ASCMD] §2.2.3.177.8: 17 = "Partial success; a
                    // Fetch … operation completed partially" — the served
                    // window hit EOF before covering the requested range.
                    let status = if outcome.complete { 1 } else { 17 };
                    let part_index = if multipart {
                        // The raw bytes move into the binary part; the WBXML
                        // references them by index via <Part>.
                        content_parts.push(std::mem::take(&mut outcome.data));
                        Some(content_parts.len() as u32)
                    } else {
                        None
                    };
                    let properties =
                        render_attachment_properties(&outcome, range_requested, part_index);
                    // The §4.10.4.4 whole-attachment example carries no
                    // airsync:Class; the gateway keeps announcing Calendar
                    // ([MS-ASCMD] §2.2.3.27.3) for its own managed
                    // attachments so clients can tell the two stores apart.
                    let class_xml = if is_calendar {
                        "<AirSync:Class>Calendar</AirSync:Class>"
                    } else {
                        ""
                    };
                    responses.push_str(&format!(
                        "<Fetch><Status>{}</Status>{}{}{}</Fetch>",
                        status, address_xml, class_xml, properties
                    ));
                }
                Err(status) => {
                    responses.push_str(&item_operations_fetch_error_xml(&address_xml, status));
                }
            }
            continue;
        }

        let Some(server_id) = fetch.server_id.or(fetch.long_id) else {
            // [MS-ASCMD] §2.2.3.177.8: 2 = "Protocol error - protocol
            // violation/XML validation error" — a Fetch with neither
            // ServerId nor LongId nor FileReference violates the schema.
            responses.push_str(&item_operations_fetch_error_xml(&address_xml, 2));
            continue;
        };

        // Email items live in the JMAP backend; calendar items below go
        // through the CalDAV/JMAP-calendar store. Splitting on the
        // server-id prefix keeps each backend on its own fetch path.
        if crate::email::is_email_server_id(&server_id) {
            let properties = match handle_email_item_fetch(
                state,
                username,
                &password,
                &resolved_fetch_options,
                &server_id,
            )
            .await
            {
                Ok(props) => props,
                Err(status) => {
                    responses.push_str(&item_operations_fetch_error_xml(&address_xml, status));
                    continue;
                }
            };
            // In multipart mode the negotiated body travels as a raw
            // binary part referenced by <Part> ([MS-ASCMD] §2.2.1.10.1,
            // §2.2.3.130); without inline body data the properties stay
            // as rendered. The part bytes count against the same
            // per-request budget as attachment windows; a body that would
            // overflow the remainder is answered with status 11 instead
            // ([MS-ASCMD] §2.2.3.177.8, Item scope). The first Fetch of a
            // request is never rejected here: its own negotiated
            // preferences ([MS-ASCMD] §2.2.3.125.4) govern its size.
            let properties = if multipart {
                match extract_body_for_multipart(&properties, content_parts.len() as u32 + 1) {
                    Some((xml_without_data, raw_body)) => {
                        if served_content_bytes > 0
                            && served_content_bytes.saturating_add(raw_body.len())
                                > attachment_budget
                        {
                            tracing::debug!(
                                served_bytes = served_content_bytes,
                                body_bytes = raw_body.len(),
                                budget = attachment_budget,
                                "ItemOperations: per-request content budget exhausted"
                            );
                            responses.push_str(&item_operations_fetch_error_xml(&address_xml, 11));
                            continue;
                        }
                        served_content_bytes += raw_body.len();
                        content_parts.push(raw_body.into_bytes());
                        xml_without_data
                    }
                    None => properties,
                }
            } else {
                properties
            };
            responses.push_str(&format!(
                "<Fetch><Status>1</Status>{}<AirSync:Class>Email</AirSync:Class><Properties>{}</Properties></Fetch>",
                address_xml, properties
            ));
            continue;
        }

        match state.storage.get_item_owner(&server_id).await {
            Ok(None) => {
                // [MS-ASCMD] §2.2.3.177.8: 6 = "The object was not found".
                responses.push_str(&item_operations_fetch_error_xml(&address_xml, 6));
                continue;
            }
            Err(e) => {
                tracing::error!("Failed to lookup item owner for {}: {}", server_id, e);
                // [MS-ASCMD] §2.2.3.177.8: 3 = "Server error".
                responses.push_str(&item_operations_fetch_error_xml(&address_xml, 3));
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
                // [MS-ASCMD] §2.2.3.177.8: 16 = "Access to the resource is
                // denied" (4 is the document-library URI code).
                responses.push_str(&item_operations_fetch_error_xml(&address_xml, 16));
                continue;
            }
            Err(e) => {
                tracing::error!("Permission check failed for item {}: {}", server_id, e);
                responses.push_str(&item_operations_fetch_error_xml(&address_xml, 3));
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
                // [MS-ASCMD] §2.2.3.177.8: 6 = "The object was not found".
                responses.push_str(&item_operations_fetch_error_xml(&address_xml, 6));
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
                // [MS-ASCMD] §2.2.3.177.8: 3 = "Server error".
                responses.push_str(&item_operations_fetch_error_xml(&address_xml, 3));
                continue;
            };
            (ics, etag)
        };
        let Some(item) = parse_ics_event(&ics) else {
            // [MS-ASCMD] §2.2.3.177.8: 14 = "Mailbox fetch
            // provider - the item failed conversion".
            responses.push_str(&item_operations_fetch_error_xml(&address_xml, 14));
            continue;
        };
        let mut app_data =
            sync::render_calendar_app_data_with_options(&item, &resolved_fetch_options);
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
            "<Fetch><Status>1</Status>{}<AirSync:Class>Calendar</AirSync:Class><Properties>{}</Properties></Fetch>",
            address_xml, app_data
        ));
    }
    let response = format!(
        r#"<?xml version="1.0" encoding="utf-8"?><ItemOperations xmlns="ItemOperations:" xmlns:AirSync="AirSync:" xmlns:Search="Search:" xmlns:Calendar="Calendar:" xmlns:AirSyncBase="AirSyncBase:"><Status>1</Status><Response>{}</Response></ItemOperations>"#,
        responses
    );
    // [MS-ASHTTP] §2.2.1.1.2.5: a request carrying
    // `MS-ASAcceptMultiPart: T` gets the WBXML as part 0 and every fetched
    // binary payload as a raw part, instead of base64-inflated inline
    // content; without the header the response stays inline
    // ([MS-ASCMD] §2.2.1.10.1).
    if multipart {
        multipart_item_operations_response(wbxml, &response, content_parts, request_id)
    } else {
        xml_or_wbxml_response(wbxml, as_wbxml, &response, request_id)
    }
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
                    Ok(Event::Start(e)) if e.name().local_name().as_ref() == "calendar-data" => {
                        in_cal_data = true;
                        caldata_buf.clear();
                    }
                    Ok(Event::Text(ref t)) if in_cal_data => {
                        caldata_buf.push_str(t);
                    }
                    Ok(Event::CData(ref t)) if in_cal_data => {
                        caldata_buf.push_str(t.as_ref());
                    }
                    Ok(Event::GeneralRef(ref r)) if in_cal_data => {
                        caldata_buf.push_str(&resolve_xml_reference(r.as_ref()));
                    }
                    Ok(Event::End(e)) if e.name().local_name().as_ref() == "calendar-data" => {
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
    // MS-ASCMD §2.2.1.15 Options: CertificateRetrieval (§2.2.3.22) and
    // MaxCertificates (§2.2.3.101) govern the S/MIME GAL `Certificates`
    // block; MaxAmbiguousRecipients (§2.2.3.100) bounds the suggestions
    // list. All are optional; CertificateRetrieval defaults to 1
    // ("do not retrieve certificates").
    let certificate_retrieval: Option<u32> = match extract_first_tag_text(
        xml,
        b"CertificateRetrieval",
    ) {
        Some(v) => match v.trim().parse::<u32>() {
            Ok(n @ 1..=3) => Some(n),
            _ => {
                // MS-ASCMD §2.2.3.177.12: request-level Status 5 -
                // protocol error, invalid parameter.
                return xml_or_wbxml_response(
                    wbxml,
                    as_wbxml,
                    r#"<?xml version="1.0" encoding="utf-8"?><ResolveRecipients xmlns="ResolveRecipients:"><Status>5</Status></ResolveRecipients>"#,
                    request_id,
                );
            }
        },
        None => Some(1),
    };
    let max_certificates: Option<u32> = match extract_first_tag_text(xml, b"MaxCertificates") {
        Some(v) => match v.trim().parse::<u32>() {
            // MS-ASCMD §2.2.3.101: value is limited to 0-9999.
            Ok(n) if n <= 9999 => Some(n),
            _ => {
                return xml_or_wbxml_response(
                    wbxml,
                    as_wbxml,
                    r#"<?xml version="1.0" encoding="utf-8"?><ResolveRecipients xmlns="ResolveRecipients:"><Status>5</Status></ResolveRecipients>"#,
                    request_id,
                );
            }
        },
        None => None,
    };
    let max_ambiguous: u32 = match extract_first_tag_text(xml, b"MaxAmbiguousRecipients") {
        Some(v) => match v.trim().parse::<u32>() {
            // MS-ASCMD §2.2.3.100: value is limited to 0-9999.
            Ok(n) if n <= 9999 => n,
            _ => {
                return xml_or_wbxml_response(
                    wbxml,
                    as_wbxml,
                    r#"<?xml version="1.0" encoding="utf-8"?><ResolveRecipients xmlns="ResolveRecipients:"><Status>5</Status></ResolveRecipients>"#,
                    request_id,
                );
            }
        },
        None => 100, // Exchange default when the client does not bound the list.
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

    // The legacy fallback above synthesises one recipient per requested To
    // when the directory is empty/unavailable; treat recipients.len() as the
    // authoritative count only when lookup succeeded.
    let now_unix = chrono::Utc::now().timestamp();
    let mut responses_xml = String::new();
    for (i, recipient) in recipients.iter().enumerate() {
        let freebusy = &freebusy_results[i];
        let lookup_res: &Result<Vec<crate::directory::Contact>, Error> = &lookup_results[i];

        // Up to `max_ambiguous` matches per To; larger match sets are
        // partial (Response Status 3), multiple matches are ambiguous
        // suggestions (Status 2, no certificate nodes per
        // MS-ASCMD §2.2.3.177.12).
        let requested = recipient.clone();
        let mut matches: Vec<(String, String)> = match lookup_res {
            Ok(contacts) if !contacts.is_empty() => contacts
                .iter()
                .map(|c| (c.display_name.clone(), c.email.clone()))
                .collect(),
            _ => vec![(recipient.clone(), recipient.clone())],
        };
        let total_matches = matches.len() as u32;
        let truncated = total_matches > max_ambiguous;
        matches.truncate(max_ambiguous as usize);
        let response_status: u8 = if truncated {
            3
        } else if matches.len() > 1 {
            2
        } else {
            1
        };

        let mut recipient_xml = String::new();
        for (display_name, email) in &matches {
            let avail_xml = if availability_window.is_some() {
                format!(
                    "<Availability><Status>1</Status><MergedFreeBusy>{}</MergedFreeBusy></Availability>",
                    freebusy
                )
            } else {
                String::new()
            };

            // Certificates block (S/MIME in GAL, audit item 9): only
            // emitted for exactly-resolved recipients when the client opted
            // in with CertificateRetrieval 2 or 3.
            let mut certificates_xml = String::new();
            if response_status == 1
                && let Some(retrieval) = certificate_retrieval
                && retrieval != 1
            {
                let email_key = email.to_lowercase();
                let certs = match state.storage.get_smime_certs(&email_key, now_unix).await {
                    Ok(certs) => certs,
                    Err(e) => {
                        tracing::error!(
                            error = %e,
                            recipient = %email_key,
                            "S/MIME GAL certificate store lookup failed"
                        );
                        Vec::new()
                    }
                };
                let limit = max_certificates.unwrap_or(u32::MAX);
                certificates_xml = if certs.is_empty() {
                    // Status 7: no valid S/MIME certificate for recipient.
                    "<Certificates><Status>7</Status></Certificates>".to_string()
                } else if certs.len() as u32 > limit {
                    // Status 8: the MaxCertificates limit was reached; no
                    // certificates are returned, only the unreturned count.
                    format!(
                        "<Certificates><Status>8</Status><CertificateCount>{}</CertificateCount><RecipientCount>{}</RecipientCount></Certificates>",
                        certs.len(),
                        matches.len()
                    )
                } else {
                    // Status 1 + full X.509 certificates. MS-ASCMD splits
                    // retrieval modes: 2 -> full X.509 `<Certificate>`,
                    // 3 -> proprietary `<MiniCertificate>`. The
                    // minicertificate BLOB format is not defined in any
                    // Open Specification; full certificates are a strict
                    // superset, so both modes receive `<Certificate>`.
                    let mut block = format!(
                        "<Certificates><Status>1</Status><CertificateCount>{}</CertificateCount><RecipientCount>{}</RecipientCount>",
                        certs.len(),
                        matches.len()
                    );
                    use base64::Engine;
                    for cert in &certs {
                        block.push_str(&format!(
                            "<Certificate>{}</Certificate>",
                            base64::engine::general_purpose::STANDARD.encode(&cert.cert_der)
                        ));
                    }
                    block.push_str("</Certificates>");
                    block
                };
            }

            recipient_xml.push_str(&format!(
                "<Recipient><Type>1</Type><DisplayName>{}</DisplayName><EmailAddress>{}</EmailAddress>{}{}</Recipient>",
                xml_escape(display_name),
                xml_escape(email),
                avail_xml,
                certificates_xml
            ));
        }
        responses_xml.push_str(&format!(
            "<Response><To>{}</To><Status>{}</Status><RecipientCount>{}</RecipientCount>{}</Response>",
            xml_escape(&requested),
            response_status,
            matches.len(),
            recipient_xml
        ));
    }
    if recipients.is_empty() {
        let claimed = username.to_string();
        responses_xml.push_str(&format!(
            "<Response><To>{}</To><Status>1</Status><RecipientCount>0</RecipientCount></Response>",
            xml_escape(&claimed)
        ));
    }
    let response = format!(
        r#"<?xml version="1.0" encoding="utf-8"?><ResolveRecipients xmlns="ResolveRecipients:"><Status>1</Status>{}</ResolveRecipients>"#,
        responses_xml
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
    let mut in_href = false;
    let mut out = Vec::new();
    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(ref e)) => match e.name().local_name().as_ref() {
                "href" => {
                    in_href = true;
                    href.clear();
                }
                "calendar-data" => {
                    in_cal_data = true;
                    caldata_buf.clear();
                }
                _ => {}
            },
            Ok(Event::Text(ref t)) if in_href => {
                href.push_str(t.as_ref());
            }
            Ok(Event::GeneralRef(ref r)) if in_href => {
                href.push_str(&resolve_xml_reference(r.as_ref()));
            }
            Ok(Event::Text(ref t)) if in_cal_data => {
                caldata_buf.push_str(t);
            }
            Ok(Event::CData(ref t)) if in_cal_data => {
                caldata_buf.push_str(t.as_ref());
            }
            Ok(Event::GeneralRef(ref r)) if in_cal_data => {
                caldata_buf.push_str(&resolve_xml_reference(r.as_ref()));
            }
            Ok(Event::End(ref e)) => {
                if e.name().local_name().as_ref() == "href" {
                    in_href = false;
                }
                if e.name().local_name().as_ref() == "calendar-data" {
                    in_cal_data = false;
                }
                if e.name().local_name().as_ref() == "response" {
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

/// Handle EAS SendMail / SmartReply / SmartForward commands
/// (MS-ASCMD §2.2.1.17/2.2.1.19/2.2.1.20).
///
/// Per MS-ASCMD, these commands deliver a client-constructed MIME message
/// (`MIMEData`). The gateway parses that MIME with `mail-parser` to recover
/// the true envelope (From/To/Cc/Bcc), subject, bodies, and threading
/// headers; a structurally simple message is re-created losslessly via JMAP
/// EmailSubmission (SMTP fallback), while anything with attachments, S/MIME,
/// or nested parts is relayed byte-for-byte over SMTP — mail content is
/// never dropped or silently rewritten. SmartReply/SmartForward resolve the
/// referenced message via CollectionId/ItemId so In-Reply-To/References are
/// set even when the client omits them.
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

    // Parse the EAS SendMail/SmartReply/SmartForward request and extract the
    // client-supplied MIME content plus (for SmartReply/SmartForward) the
    // referenced original message.
    let req = crate::email::parse_eas_sendmail(xml);
    let Some(mime_data_b64) = req.as_ref().and_then(|r| r.mime_data.clone()) else {
        // Per MS-ASCMD §2.2.1.17, Status 2 = "Protocol error" — the request
        // XML was malformed or missing required MIME content. Returning
        // Status 1 (Success) here would cause silent email loss.
        tracing::warn!(
            "EAS SendMail/SmartReply/SmartForward: failed to parse MIME content from request"
        );
        return xml_or_wbxml_response(
            wbxml,
            as_wbxml,
            r#"<?xml version="1.0" encoding="utf-8"?><Status xmlns="SendMail:">2</Status>"#,
            request_id,
        );
    };

    // Per MS-ASCMD §2.2.1.7, MIMEData is transported base64-encoded (as
    // WBXML opaque data in binary requests and as base64 text in XML
    // requests), so it must be decoded before parsing or relaying.
    let mime_data: Vec<u8> = {
        use base64::Engine;
        let value: String = mime_data_b64.split_whitespace().collect();
        match base64::engine::general_purpose::STANDARD.decode(&value) {
            Ok(decoded) => decoded,
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    "EAS SendMail/SmartReply/SmartForward: MIMEData is not valid base64"
                );
                return xml_or_wbxml_response(
                    wbxml,
                    as_wbxml,
                    r#"<?xml version="1.0" encoding="utf-8"?><Status xmlns="SendMail:">2</Status>"#,
                    request_id,
                );
            }
        }
    };

    let req_ref = req.as_ref();
    // The command name is recovered from the request's root element so this
    // handler works identically for all three commands. Only SmartReply
    // enriches threading headers from the referenced item
    // (MS-ASCMD §2.2.1.19/20 CollectionId + ItemId).
    let smart_kind = if xml.contains("<SmartReply") || xml.contains(":SmartReply") {
        crate::email::EasSmartKind::SmartReply
    } else if xml.contains("<SmartForward") || xml.contains(":SmartForward") {
        crate::email::EasSmartKind::SmartForward
    } else {
        crate::email::EasSmartKind::SendMail
    };
    let reference_item_id = if smart_kind == crate::email::EasSmartKind::SendMail {
        None
    } else {
        req_ref.and_then(|r| r.item_id.as_deref())
    };

    // Parse the MIME with mail-parser, re-express it via JMAP
    // EmailSubmission when it is structurally simple (lossless), otherwise
    // relay the bytes over SMTP with a minimal header rewrite and, when
    // SaveInSentItems is set, import the copy into Sent via JMAP Email/import.
    match crate::email::send_eas_mime_message(
        state,
        username,
        password,
        &mime_data,
        smart_kind,
        reference_item_id,
        req_ref.map(|r| r.save_in_sent).unwrap_or(true),
    )
    .await
    {
        Ok(_) => {}
        Err(e) => {
            tracing::error!(
                error = %e,
                "Failed to send EAS SendMail/SmartReply/SmartForward message"
            );
            return xml_or_wbxml_response(
                wbxml,
                as_wbxml,
                r#"<?xml version="1.0" encoding="utf-8"?><Status xmlns="SendMail:">4</Status>"#,
                request_id,
            );
        }
    }

    // Per MS-ASCMD, SendMail/SmartReply/SmartForward return Status 1 on success
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
    /// Global Sync `WindowSize` (direct child of the `Sync` element,
    /// MS-ASCMD §2.2.3.199), already clamped per spec (0 and >512 → 512).
    /// `None` = not sent by the client.
    global_window_size: Option<usize>,
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
        global_window_size,
    } = *ctx;
    let mut collection_responses: Vec<String> = Vec::new();

    // Remaining global WindowSize budget (MS-ASCMD §2.2.3.199: the WindowSize
    // child of the Sync element "impose[s] a global limit on the number of
    // changes that are returned by the server"). `None` = the client sent no
    // global WindowSize, so only per-collection windows apply.
    let mut remaining_global: Option<usize> = global_window_size;

    for coll in collections {
        // "The server will stop processing after the global WindowSize has
        // been filled and simply not process the remaining collections. Any
        // server-side changes that are pending in the unprocessed collections
        // are picked up in the next synchronization." (MS-ASCMD §2.2.3.199)
        if remaining_global == Some(0) {
            tracing::debug!(
                request_id = %request_id,
                "Global WindowSize exhausted; skipping remaining Sync collections"
            );
            continue;
        }

        let collection_id = coll.collection_id.as_deref().unwrap_or("1");
        let state_collection_id = scoped_collection_id(collection_id, device_id);
        let incoming_key = coll.sync_key.as_deref().unwrap_or("0");
        // Sticky <Options> ([MS-ASCMD] §2.2.3.125.6): "If the Options block
        // is not included in a request, the previous Options block is used.
        // Whenever the client specifies new options … the server MUST replace
        // the original Options block with the new Options block." Resolved
        // against the per-device collection's last stored block and persisted
        // so a mid-session gateway restart cannot silently drop body
        // preferences Outlook sent on the initial SyncKey-0 request.
        let sticky = state
            .storage
            .get_sync_collection_options(username, &state_collection_id)
            .await
            .ok()
            .flatten();
        let resolved_options = crate::eas_sync_options::EasSyncCollectionOptions::resolve_sticky(
            sticky,
            coll.options.clone(),
        );
        if let Some(opts) = resolved_options.as_ref()
            && let Err(e) = state
                .storage
                .set_sync_collection_options(username, &state_collection_id, opts)
                .await
        {
            tracing::warn!(request_id = %request_id, error = %e, "Failed to persist sticky Sync options");
        }
        // This collection's effective response window: the per-collection
        // WindowSize (spec clamps 0 and >512 to 512; absent = 100) capped by
        // the remaining global budget.
        let effective_collection_window = effective_sync_window(coll.window_size, remaining_global);
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
                    effective_collection_window,
                    resolved_options.as_ref(),
                    coll.conversation_mode,
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
                    window_size: effective_collection_window,
                    get_changes: coll.get_changes,
                    // FilterType is a child of <Options> for every class
                    // ([MS-ASCMD] §2.2.3.68.2); the sticky block carries it
                    // across requests.
                    filter_start: resolved_options
                        .as_ref()
                        .and_then(|o| o.filter_type)
                        .map(filter_type_to_start)
                        .unwrap_or_else(|| chrono::Utc::now() - ChronoDuration::weeks(52)),
                    // The same sticky block drives body negotiation
                    // ([MS-ASAIRS] §2.2.2.12) for every rendered item.
                    body_options: resolved_options.clone().unwrap_or_default(),
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
            // Contacts sync via CardDAV with client mutation support.
            // Yields this collection's XML (rather than returning early) so
            // multi-collection Sync responses keep every collection and the
            // global WindowSize budget can be honored across them.
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

            // Error short-circuit: set once, checked before the server-side
            // change delivery below.
            let mut error_xml: Option<String> = None;

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
                    error_xml = Some(format!(
                        "<Collection><Class>Contacts</Class><SyncKey>{}</SyncKey><CollectionId>{}</CollectionId><Status>4</Status></Collection>",
                        xml_escape(incoming_key),
                        xml_escape(collection_id)
                    ));
                } else {
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
                            error_xml = Some(format!(
                                "<Collection><Class>Contacts</Class><SyncKey>{}</SyncKey><CollectionId>{}</CollectionId><Status>6</Status></Collection>",
                                xml_escape(incoming_key),
                                xml_escape(collection_id)
                            ));
                        }
                    }
                }
            }

            match error_xml {
                Some(xml) => xml,
                None => {
                    // Perform server-side sync to fetch changes (after applying
                    // client mutations), windowed per MS-ASCMD §2.2.3.199.
                    match crate::contacts::sync_contacts(
                        state,
                        username,
                        password.expose_secret(),
                        Some(incoming_key),
                        device_id,
                        Some(effective_collection_window),
                    )
                    .await
                    {
                        Ok(result) => {
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
                            let more_tag = if result.more_available {
                                "<MoreAvailable/>"
                            } else {
                                ""
                            };
                            format!(
                                "<Collection><Class>Contacts</Class><SyncKey>{}</SyncKey><CollectionId>{}</CollectionId><Status>1</Status>{}{}<Commands>{}</Commands></Collection>",
                                xml_escape(&new_sync_key),
                                xml_escape(collection_id),
                                more_tag,
                                contact_mutation_responses,
                                result.commands_xml
                            )
                        }
                        Err(e) => {
                            tracing::error!(error = %e, "Contacts sync failed");
                            format!(
                                "<Collection><Class>Contacts</Class><SyncKey>{}</SyncKey><CollectionId>{}</CollectionId><Status>6</Status></Collection>",
                                xml_escape(incoming_key),
                                xml_escape(collection_id)
                            )
                        }
                    }
                }
            }
        } else if is_tasks {
            handle_local_content_sync(&LocalContentSyncCtx {
                state,
                username,
                collection_id,
                state_collection_id: &state_collection_id,
                incoming_key,
                coll,
                kind: LocalContentKind::Tasks,
                global_budget: remaining_global,
            })
            .await
        } else if is_notes {
            handle_local_content_sync(&LocalContentSyncCtx {
                state,
                username,
                collection_id,
                state_collection_id: &state_collection_id,
                incoming_key,
                coll,
                kind: LocalContentKind::Notes,
                global_budget: remaining_global,
            })
            .await
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

        collection_responses.push(coll_xml.clone());

        // Global WindowSize accounting: subtract this collection's delivered
        // commands from the remaining budget (MS-ASCMD §2.2.3.199).
        if let Some(remaining) = remaining_global {
            remaining_global = Some(remaining.saturating_sub(count_sync_commands(&coll_xml)));
        }
    }

    // Build multi-collection response
    let collections_xml = collection_responses.join("");
    let resp_xml = format!(
        r#"<?xml version="1.0" encoding="utf-8"?><Sync xmlns="AirSync:" xmlns:Email="Email:" xmlns:Email2="Email2:" xmlns:Calendar="Calendar:" xmlns:Contacts="Contacts:" xmlns:Tasks="Tasks:" xmlns:Notes="Notes:" xmlns:AirSyncBase="AirSyncBase:"><Collections>{collections_xml}</Collections></Sync>"#
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
/// Identifies which gateway-local content class a Sync collection targets.
#[derive(Clone, Copy)]
enum LocalContentKind {
    Tasks,
    Notes,
}

impl LocalContentKind {
    fn class_name(self) -> &'static str {
        match self {
            LocalContentKind::Tasks => "Tasks",
            LocalContentKind::Notes => "Notes",
        }
    }

    /// Change-journal `resource_href` discriminator for this content class.
    fn resource_href(self) -> &'static str {
        match self {
            LocalContentKind::Tasks => "task",
            LocalContentKind::Notes => "note",
        }
    }
}

/// Request-scoped context for one gateway-local Tasks/Notes Sync collection,
/// mirroring `SyncCtx` so the handler stays under clippy's argument-count
/// threshold.
#[derive(Clone, Copy)]
struct LocalContentSyncCtx<'a> {
    state: &'a Arc<AppState>,
    username: &'a str,
    collection_id: &'a str,
    state_collection_id: &'a str,
    incoming_key: &'a str,
    coll: &'a SyncCollection,
    kind: LocalContentKind,
    global_budget: Option<usize>,
}

/// Handle a Sync collection for the gateway-local Tasks or Notes store.
///
/// Unlike Calendar/Email/Contacts (which diff an upstream JMAP/CardDAV state),
/// these namespaces live entirely in the gateway's SQLite store, so each Sync
/// renders the full owned set as `<Add>` elements. The collection still follows
/// the MS-ASCMD Sync contract: the incoming SyncKey is validated (Status 3 on
/// mismatch, per §2.2.3.177.17), the returned SyncKey is rotated and tied to the
/// current change-journal sequence (so Ping/direct-push can detect further
/// changes without a full resend), server changes are wrapped in `<Commands>`,
/// and GetChanges/WindowSize are honoured. A malformed command body is reported
/// as a protocol error (Status 4) rather than silently truncating mutations.
async fn handle_local_content_sync(ctx: &LocalContentSyncCtx<'_>) -> String {
    let LocalContentSyncCtx {
        state,
        username,
        collection_id,
        state_collection_id,
        incoming_key,
        coll,
        kind,
        global_budget,
    } = *ctx;
    let class = kind.class_name();
    // Parse and apply client mutations (Add/Change/Delete). A malformed command
    // body is a protocol error (Status 4) rather than silently truncating the set.
    let mut mutation_responses = String::new();
    match kind {
        LocalContentKind::Tasks => {
            let mutations = match crate::tasks::parse_task_mutations(&coll.xml) {
                Ok(m) => m,
                Err(e) => {
                    tracing::warn!(
                        collection_id = %collection_id,
                        class = class,
                        error = %e,
                        "Malformed {} Sync body",
                        class
                    );
                    return format!(
                        "<Collection><Class>{}</Class><SyncKey>{}</SyncKey><CollectionId>{}</CollectionId><Status>4</Status></Collection>",
                        class,
                        xml_escape(incoming_key),
                        xml_escape(collection_id)
                    );
                }
            };
            if !mutations.is_empty() {
                match crate::tasks::apply_task_mutations(state, username, &mutations).await {
                    Ok(results) => {
                        mutation_responses = crate::tasks::render_mutation_responses(&results);
                    }
                    Err(e) => {
                        tracing::error!(
                            collection_id = %collection_id,
                            class = class,
                            error = %e,
                            "Failed to apply {} mutations",
                            class
                        );
                        return format!(
                            "<Collection><Class>{}</Class><SyncKey>{}</SyncKey><CollectionId>{}</CollectionId><Status>6</Status></Collection>",
                            class,
                            xml_escape(incoming_key),
                            xml_escape(collection_id)
                        );
                    }
                }
            }
        }
        LocalContentKind::Notes => {
            let mutations = match crate::tasks::parse_note_mutations(&coll.xml) {
                Ok(m) => m,
                Err(e) => {
                    tracing::warn!(
                        collection_id = %collection_id,
                        class = class,
                        error = %e,
                        "Malformed {} Sync body",
                        class
                    );
                    return format!(
                        "<Collection><Class>{}</Class><SyncKey>{}</SyncKey><CollectionId>{}</CollectionId><Status>4</Status></Collection>",
                        class,
                        xml_escape(incoming_key),
                        xml_escape(collection_id)
                    );
                }
            };
            if !mutations.is_empty() {
                match crate::tasks::apply_note_mutations(state, username, &mutations).await {
                    Ok(results) => {
                        mutation_responses = crate::tasks::render_mutation_responses(&results);
                    }
                    Err(e) => {
                        tracing::error!(
                            collection_id = %collection_id,
                            class = class,
                            error = %e,
                            "Failed to apply {} mutations",
                            class
                        );
                        return format!(
                            "<Collection><Class>{}</Class><SyncKey>{}</SyncKey><CollectionId>{}</CollectionId><Status>6</Status></Collection>",
                            class,
                            xml_escape(incoming_key),
                            xml_escape(collection_id)
                        );
                    }
                }
            }
        }
    }

    // A non-zero incoming key must match the last key issued for this collection,
    // otherwise the client must re-prime with SyncKey 0 (MS-ASCMD Status 3).
    if incoming_key != "0" {
        let valid = match state
            .storage
            .get_sync_key(username, state_collection_id)
            .await
        {
            Ok(Some((stored, _))) => stored == incoming_key,
            _ => false,
        };
        if !valid {
            return format!(
                "<Collection><Class>{}</Class><SyncKey>0</SyncKey><CollectionId>{}</CollectionId><Status>3</Status></Collection>",
                class,
                xml_escape(collection_id)
            );
        }
    }

    let latest_seq = state.storage.get_latest_change_seq().await.unwrap_or(0);
    let new_sync_key = Uuid::new_v4().simple().to_string();

    // GetChanges=0 means the client only wants its mutations acknowledged; the
    // key is still rotated so the next Sync yields any new server changes.
    if !coll.get_changes {
        // Nothing is delivered, so nothing may be consumed from the journal
        // either (MS-ASCMD §2.2.3.72): the client still expects every change
        // on its next GetChanges=1 Sync. Carrying the previous `seq:` token
        // forward keeps the stored watermark — and the journal pruner, which
        // floors on these watermarks — from erasing undelivered rows.
        // With no previous token (a prime acknowledged before any state was
        // delivered), `seq:0` forces the next Sync to replay the whole
        // journal, reconstructing the initial state through it.
        let carried_token = state
            .storage
            .get_sync_key(username, state_collection_id)
            .await
            .ok()
            .flatten()
            .and_then(|(_, token)| token)
            .unwrap_or_else(|| "seq:0".to_string());
        if let Err(e) = state
            .storage
            .set_sync_key(
                username,
                state_collection_id,
                &new_sync_key,
                Some(&carried_token),
            )
            .await
        {
            tracing::warn!(error = %e, "Failed to set {} sync key", class);
        }
        return format!(
            "<Collection><Class>{}</Class><SyncKey>{}</SyncKey><CollectionId>{}</CollectionId><Status>1</Status>{}</Collection>",
            class,
            xml_escape(&new_sync_key),
            xml_escape(collection_id),
            mutation_responses
        );
    }

    // ---- Windowed server-change delivery (MS-ASCMD §2.2.3.199) ----------
    // The effective window is the collection WindowSize clamped by the
    // remaining global WindowSize budget (§2.2.3.199 "repurposed to also
    // impose a global limit on the number of changes that are returned").
    let window = effective_sync_window(coll.window_size, global_budget).max(1);

    // A windowed initial sync may still be in progress: its keyset cursor
    // (highest delivered server_id) makes this Sync resume the live set
    // instead of restarting it.
    let pending_cursor = state
        .storage
        .get_local_sync_cursor(username, state_collection_id)
        .await
        .unwrap_or(None);

    let mut commands_xml = String::new();
    let mut more_available = false;
    // Consumed journal position: the highest journal id whose change this
    // response delivers. When the window clips, this is where the next
    // delta Sync resumes (stored as the `seq:<id>` token) — undelivered
    // journal rows stay above it and Ping keeps reporting the collection.
    // Both branches below assign it: the initial flow to its base position,
    // the delta flow to the highest delivered journal row.
    let mut consumed_seq: i64;

    if incoming_key == "0" || pending_cursor.is_some() {
        // Initial sync, or continuation of a windowed initial sync: deliver
        // the live set in stable server_id ASC order, resuming strictly
        // after the keyset cursor.
        //
        // `base_seq` is the journal head captured when the SyncKey "0"
        // window STARTED. Continuation windows must keep anchoring to it —
        // not to the current head — because the keyset continuation only
        // lists live rows after the cursor: mutations of already-delivered
        // items recorded while paginating would otherwise fall below the
        // stored watermark and never be delivered (and the journal pruner
        // would then erase them for good). The first delta after the cursor
        // drains re-walks from `base_seq` and delivers exactly those.
        let (after, base_seq) = if incoming_key == "0" {
            let _ = state
                .storage
                .clear_local_sync_cursor(username, state_collection_id)
                .await;
            (None, latest_seq)
        } else {
            match pending_cursor.as_ref() {
                Some((last_key, cursor_base)) => (Some(last_key.as_str()), *cursor_base),
                None => (None, latest_seq),
            }
        };
        consumed_seq = base_seq;

        // Fetch at most window + 1 rows: the extra row exists only to decide
        // <MoreAvailable/> (MS-ASCMD §2.2.3.116); rendering stays bounded
        // to the delivered window instead of the whole remaining set.
        let fetch_limit = (window as u64) + 1;
        let more;
        let last_delivered: Option<String>;
        match kind {
            LocalContentKind::Tasks => {
                let rows = match state
                    .storage
                    .list_tasks_after(username, after, fetch_limit)
                    .await
                {
                    Ok(rows) => rows,
                    Err(e) => {
                        tracing::error!(class = class, error = %e, "Tasks list failed");
                        return format!(
                            "<Collection><Class>{}</Class><SyncKey>{}</SyncKey><CollectionId>{}</CollectionId><Status>6</Status></Collection>",
                            class,
                            xml_escape(incoming_key),
                            xml_escape(collection_id)
                        );
                    }
                };
                more = rows.len() > window;
                let deliver = &rows[..rows.len().min(window)];
                last_delivered = deliver.last().map(|r| r.server_id.clone());
                for row in deliver {
                    commands_xml.push_str(&crate::tasks::render_eas_task_add(&row.server_id, row));
                }
            }
            LocalContentKind::Notes => {
                let rows = match state
                    .storage
                    .list_notes_after(username, after, fetch_limit)
                    .await
                {
                    Ok(rows) => rows,
                    Err(e) => {
                        tracing::error!(class = class, error = %e, "Notes list failed");
                        return format!(
                            "<Collection><Class>{}</Class><SyncKey>{}</SyncKey><CollectionId>{}</CollectionId><Status>6</Status></Collection>",
                            class,
                            xml_escape(incoming_key),
                            xml_escape(collection_id)
                        );
                    }
                };
                more = rows.len() > window;
                let deliver = &rows[..rows.len().min(window)];
                last_delivered = deliver.last().map(|r| r.server_id.clone());
                for row in deliver {
                    commands_xml.push_str(&crate::tasks::render_eas_note_add(&row.server_id, row));
                }
            }
        }
        if more {
            // Window clipped with live content remaining: persist the keyset
            // cursor at the last delivered server_id, together with the
            // base journal position, and require the client to sync again
            // (MS-ASCMD §2.2.3.116 MoreAvailable).
            if let Some(last_key) = last_delivered.as_deref()
                && let Err(e) = state
                    .storage
                    .set_local_sync_cursor(username, state_collection_id, last_key, base_seq)
                    .await
            {
                tracing::warn!(error = %e, "Failed to persist {} sync cursor", class);
            }
            more_available = true;
        } else {
            // Live set fully delivered: drop the cursor.
            let _ = state
                .storage
                .clear_local_sync_cursor(username, state_collection_id)
                .await;
        }
    } else {
        // Delta sync: resumable change-journal walk from the stored seq token.
        let stored = state
            .storage
            .get_sync_key(username, state_collection_id)
            .await;
        let since = stored
            .ok()
            .flatten()
            .and_then(|(_, token)| token)
            .and_then(|t| t.strip_prefix("seq:").map(|n| n.to_string()))
            .and_then(|n| n.parse::<i64>().ok())
            .unwrap_or(0);
        let journal_rows = match state
            .storage
            .list_local_content_changes_since_seq(username, kind.resource_href(), since)
            .await
        {
            Ok(rows) => rows,
            Err(e) => {
                tracing::error!(
                    collection_id = %collection_id,
                    class = class,
                    error = %e,
                    "{} journal query failed",
                    class
                );
                return format!(
                    "<Collection><Class>{}</Class><SyncKey>{}</SyncKey><CollectionId>{}</CollectionId><Status>6</Status></Collection>",
                    class,
                    xml_escape(incoming_key),
                    xml_escape(collection_id)
                );
            }
        };
        // Collapse to the last op per server_id, preserving journal order of
        // the surviving (last) op — the exact resumable delivery sequence.
        let mut last_op: std::collections::HashMap<String, (i64, String)> =
            std::collections::HashMap::with_capacity(journal_rows.len());
        for row in &journal_rows {
            last_op.insert(row.server_id.clone(), (row.id, row.op.clone()));
        }
        let mut ordered: Vec<(String, i64, String)> = last_op
            .into_iter()
            .map(|(server_id, (id, op))| (server_id, id, op))
            .collect();
        ordered.sort_by_key(|(_, id, _)| *id);

        consumed_seq = since;
        let mut items_included = 0usize;
        for (server_id, journal_id, op) in &ordered {
            if items_included >= window {
                more_available = true;
                break;
            }
            if op == "delete" {
                commands_xml.push_str(&format!(
                    "<Delete><ServerId>{}</ServerId></Delete>",
                    xml_escape(server_id)
                ));
                items_included += 1;
            } else {
                let add = match kind {
                    LocalContentKind::Tasks => {
                        match state.storage.get_task(username, server_id).await {
                            Ok(Some(row)) => {
                                Some(crate::tasks::render_eas_task_add(&row.server_id, &row))
                            }
                            _ => None,
                        }
                    }
                    LocalContentKind::Notes => {
                        match state.storage.get_note(username, server_id).await {
                            Ok(Some(row)) => {
                                Some(crate::tasks::render_eas_note_add(&row.server_id, &row))
                            }
                            _ => None,
                        }
                    }
                };
                if let Some(add) = add {
                    commands_xml.push_str(&add);
                    // The window counts delivered items (MS-ASCMD §2.2.3.199);
                    // a journal entry whose row vanished before delivery emits
                    // nothing and consumes no window slot.
                    items_included += 1;
                }
            }
            consumed_seq = (*journal_id).max(consumed_seq);
        }
    }

    // Persist the new key with the consumed journal position. On a clip this
    // token is the exact resume point (MS-ASCMD §2.2.3.199: "the client MUST
    // synchronize again to continue getting items from the server").
    if let Err(e) = state
        .storage
        .set_sync_key(
            username,
            state_collection_id,
            &new_sync_key,
            Some(&format!("seq:{}", consumed_seq)),
        )
        .await
    {
        tracing::warn!(error = %e, "Failed to set {} sync key", class);
    }
    if let Err(e) = state
        .storage
        .set_journal_watermark(username, state_collection_id, consumed_seq)
        .await
    {
        tracing::warn!(error = %e, "Failed to set {} journal watermark", class);
    }

    let more_tag = if more_available {
        "<MoreAvailable/>"
    } else {
        ""
    };
    let commands = if commands_xml.is_empty() {
        "<Commands></Commands>".to_string()
    } else {
        format!("<Commands>{}</Commands>", commands_xml)
    };

    format!(
        "<Collection><Class>{}</Class><SyncKey>{}</SyncKey><CollectionId>{}</CollectionId><Status>1</Status>{}{}{}</Collection>",
        class,
        xml_escape(&new_sync_key),
        xml_escape(collection_id),
        more_tag,
        mutation_responses,
        commands
    )
}

/// Effective EAS Sync `WindowSize` for a collection (MS-ASCMD §2.2.3.199).
///
/// The client value is interpreted exactly per spec: values of 0 and above
/// 512 are treated as 512, and an absent value behaves as 100. When a
/// caller also passes a remaining global budget (the `WindowSize` child of
/// the `Sync` element — §2.2.3.199 "repurposed to also impose a global limit
/// on the number of changes that are returned"), the per-collection window
/// is capped by that budget so the whole response never exceeds the client's
/// global window.
fn effective_sync_window(requested: Option<usize>, global_budget: Option<usize>) -> usize {
    let per_collection = match requested {
        None => 100,
        Some(0) => 512,
        Some(v) => v.min(512),
    };
    match global_budget {
        Some(0) => 0,
        Some(b) => per_collection.min(b),
        None => per_collection,
    }
}

/// Count the `<Add>`/`<Change>`/`<Delete>` commands a rendered EAS Sync
/// collection response contains, for global WindowSize budget accounting.
///
/// Per MS-ASCMD §2.2.3.199 the WindowSize bounds "a maximum number of
/// changed items in a collection" that the server *returns* — the
/// server→client commands in `<Commands>`. The `<Responses>` echoes of the
/// client's own mutations (§2.2.3.154) are acknowledgments, not changed
/// items, and must not consume the budget. Only the `<Commands>` section is
/// counted. The gateway generates these responses itself, so marker
/// counting inside that section is exact.
fn count_sync_commands(collection_xml: &str) -> usize {
    let Some(start) = collection_xml.find("<Commands>") else {
        return 0;
    };
    let rest = &collection_xml[start..];
    let section = rest.find("</Commands>").map_or(rest, |i| &rest[..i]);
    section.matches("<Add>").count()
        + section.matches("<Change>").count()
        + section.matches("<Delete>").count()
}

/// One queued delta operation for the email overflow continuation.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
struct PendingEmailOp {
    /// `"add"`, `"change"` or `"delete"`.
    op: String,
    /// JMAP Email id.
    id: String,
}

impl PendingEmailOp {
    fn add(id: String) -> Self {
        PendingEmailOp {
            op: "add".to_string(),
            id,
        }
    }

    fn change(id: String) -> Self {
        PendingEmailOp {
            op: "change".to_string(),
            id,
        }
    }

    fn delete(id: String) -> Self {
        PendingEmailOp {
            op: "delete".to_string(),
            id,
        }
    }

    fn is_delete(&self) -> bool {
        self.op == "delete"
    }
}

/// Collapse the accumulated operation list to one operation per email id.
///
/// JMAP guarantees the created/updated/destroyed lists are disjoint within a
/// single `Email/changes` response (RFC 8621 §4.4.5), but across a chained
/// `hasMoreChanges` sequence the same id can recur: created in batch 1 and
/// destroyed in batch 2 would otherwise queue an add (skipped, the message
/// is gone) followed by a `<Delete>` for a ServerId the client never
/// received. Repeated updates would fetch and re-render the same message
/// several times. Rules:
///
/// * an id first seen as `add` (it did not exist at the diff base) and last
///   seen as `delete` never existed for the client: emit nothing.
/// * otherwise the LAST operation wins — except that an id first seen as
///   `add` stays an `add` even after later changes, because the client never
///   received that ServerId and a `<Change>` for it would be a client-side
///   error. Repeated changes collapse to one, and change-then-delete becomes
///   a single delete.
///
/// Each surviving id keeps the position of its FIRST occurrence so the
/// window consumes the earliest possible slots.
fn collapse_email_ops(ops: Vec<PendingEmailOp>) -> Vec<PendingEmailOp> {
    // id -> (index of first occurrence, first op, last op)
    let mut seen: std::collections::HashMap<String, (usize, String, PendingEmailOp)> =
        std::collections::HashMap::with_capacity(ops.len());
    for (idx, op) in ops.into_iter().enumerate() {
        match seen.get_mut(&op.id) {
            Some((_, _, last)) => *last = op,
            None => {
                let first_op = op.op.clone();
                seen.insert(op.id.clone(), (idx, first_op, op));
            }
        }
    }
    let mut collapsed: Vec<(usize, PendingEmailOp)> = seen
        .into_values()
        .filter_map(|(idx, first_op, last)| {
            if first_op == "add" && last.op == "delete" {
                // Created and destroyed within the diff window: a no-op for
                // the client, and a delete would reference a ServerId it
                // never received.
                return None;
            }
            let op = if first_op == "add" {
                "add".to_string()
            } else {
                last.op
            };
            Some((idx, PendingEmailOp { op, id: last.id }))
        })
        .collect();
    collapsed.sort_by_key(|(idx, _)| *idx);
    collapsed.into_iter().map(|(_, op)| op).collect()
}

/// Upper bound on `Email/changes` accumulation while draining
/// `hasMoreChanges` chains, so a pathological backend cannot loop forever.
/// Anything past this bound stays behind `base_state` and is picked up by the
/// next Sync's diff, so nothing is lost.
const MAX_EMAIL_CHANGE_ACCUMULATION: usize = 5000;

/// Accumulate every email change between `old_state` and the backend's
/// current state by chaining `Email/changes` while it reports
/// `hasMoreChanges` (RFC 8621 §4.4).
///
/// Returns the ordered operation list plus the state token that follows the
/// last delivered change (`base_state`). Callers that do not advance their
/// stored token past `base_state` never lose or duplicate a change.
///
/// Operations are collapsed to one per id before returning: chained batches
/// can repeat an id (see `collapse_email_ops`).
async fn accumulate_email_changes(
    jmap: &Arc<JmapClient>,
    account_id: &str,
    old_state: &str,
    username: &str,
    password: &SecretString,
) -> anyhow::Result<(Vec<PendingEmailOp>, String)> {
    let mut ops: Vec<PendingEmailOp> = Vec::new();
    let mut since = old_state.to_string();
    loop {
        let batch = jmap
            .sync_email_changes(account_id, &since, username, password, None)
            .await?;
        for id in batch.created {
            ops.push(PendingEmailOp::add(id));
        }
        for id in batch.updated {
            ops.push(PendingEmailOp::change(id));
        }
        for id in batch.destroyed {
            ops.push(PendingEmailOp::delete(id));
        }
        since = batch.new_state;
        if !batch.has_more_changes || ops.len() >= MAX_EMAIL_CHANGE_ACCUMULATION {
            break;
        }
    }
    Ok((collapse_email_ops(ops), since))
}

/// Render the EAS `<ApplicationData>` payload for a queued email op.
///
/// Fetches the current JMAP Email and renders the same `<Add>`/`<Change>`
/// body the direct delta path produces. Destroyed emails need no fetch.
async fn render_pending_email_op(ctx: &EmailSyncCtx<'_>, op: &PendingEmailOp) -> String {
    let EmailSyncCtx {
        jmap,
        account_id,
        username,
        password,
        collection_id,
        options,
        conversation_mode,
        ..
    } = *ctx;
    if op.is_delete() {
        let server_id = crate::email::email_server_id_from_jmap_id(&op.id);
        return format!(
            "<Delete><ServerId>{}</ServerId></Delete>",
            xml_escape(&server_id)
        );
    }
    let emails = match jmap
        .get_emails(
            account_id,
            std::slice::from_ref(&op.id),
            None,
            username,
            password,
            true,
        )
        .await
    {
        Ok(emails) => emails,
        Err(e) => {
            tracing::warn!(
                target: "eas",
                error = %e,
                email_id = %op.id,
                "Failed to fetch queued email for EAS sync continuation"
            );
            Vec::new()
        }
    };
    let Some(email) = emails.into_iter().next() else {
        // The message vanished between the change report and this fetch
        // (e.g. moved/destroyed). Emitting nothing loses nothing: a later
        // diff reports the destruction, and re-adding a vanished id is a
        // client-side error. Skip silently — same policy as the direct
        // delta path's created/updated filter.
        return String::new();
    };
    let jmap_id = email.id.as_deref().unwrap_or_default();
    let server_id = crate::email::email_server_id_from_jmap_id(jmap_id);
    let app_data = crate::email::render_jmap_email_as_eas_application_data_with_options(
        &email,
        &server_id,
        collection_id,
        options,
        conversation_mode,
        true,
    );
    if op.op == "add" {
        format!(
            "<Add><ServerId>{}</ServerId><ApplicationData>{}</ApplicationData></Add>",
            xml_escape(&server_id),
            app_data
        )
    } else {
        format!(
            "<Change><ServerId>{}</ServerId><ApplicationData>{}</ApplicationData></Change>",
            xml_escape(&server_id),
            app_data
        )
    }
}

/// How an email-collection Sync request must be served.
///
/// The decision depends on the incoming SyncKey *and* the persisted
/// initial-sync cursor: a windowed initial sync that clipped returned
/// `<MoreAvailable/>` with a new SyncKey, and the client's follow-up Sync
/// carries that non-zero key — so routing on the key alone would wrongly
/// send a still-draining initial sync to the delta path and permanently
/// truncate the mailbox. Cursor presence (not the key value) marks a
/// continuation.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum EmailSyncRoute {
    /// SyncKey "0": full prime of the collection from position 0. Any state
    /// left by a previous, abandoned sync must be discarded first.
    Initial,
    /// Non-zero key with a persisted initial-sync cursor: resume the
    /// windowed initial sync from the cursor.
    InitialContinuation,
    /// Non-zero key with no cursor: diff changes from the stored state token
    /// (draining any persisted overflow queue first).
    Delta,
}

fn route_email_sync(incoming_sync_key: &str, cursor_present: bool) -> EmailSyncRoute {
    if incoming_sync_key == "0" {
        EmailSyncRoute::Initial
    } else if cursor_present {
        EmailSyncRoute::InitialContinuation
    } else {
        EmailSyncRoute::Delta
    }
}

/// Handle EAS Email Sync class by routing to JMAP.
///
/// Per MS-ASEMAIL, the Email sync class synchronizes email messages.
/// The gateway translates JMAP Email/get and Email/changes to EAS Sync responses.
///
/// ## WindowSize / MoreAvailable (MS-ASCMD §2.2.3.199, §2.2.3.116)
///
/// `window` is the effective response window already clamped by the caller
/// (`effective_sync_window`: per-collection WindowSize vs. the global
/// WindowSize budget). Both the initial sync and the delta path honor it and
/// emit `<MoreAvailable/>` when the mailbox/change set does not fit, with a
/// persisted continuation so the client's follow-up Sync (spec: "the client
/// MUST synchronize again to continue getting items from the server") picks
/// up exactly where this window stopped:
///
/// * **Initial sync** pages JMAP `Email/query` by `position`; the resume
///   position and the `state` token captured at the start are persisted in
///   `email_sync_cursor`. A follow-up Sync whose key was issued by a clipped
///   initial window is routed back to the initial path by cursor presence,
///   and a fresh SyncKey "0" prime clears the cursor and the overflow queue
///   so a re-provisioned device restarts from zero.
/// * **Delta sync** accumulates `Email/changes` (chaining `hasMoreChanges`);
///   when the accumulated ops exceed the window, the remainder is persisted
///   in `email_sync_pending` together with the state token that follows the
///   full change set. The stored sync-state token is not advanced while the
///   queue is non-empty, so nothing is re-delivered and nothing is lost.
///
/// Outlook Android adds third-party accounts through Microsoft's cloud sync
/// connector, and Android's own Exchange account wizard talks EAS directly;
/// both request large window sizes during the first sync of a big mailbox,
/// so without this windowing the first page would be the *only* page ever
/// delivered. (New Outlook for Windows, by contrast, is not an EAS client
/// at all for third-party accounts — Microsoft documents no EAS account
/// type and no on-premises Exchange support for it — so it never reaches
/// this path.)
#[allow(clippy::too_many_arguments)] // irreducible request inputs; mirrors SyncCtx below
async fn handle_email_sync(
    state: &Arc<AppState>,
    username: &str,
    password: &SecretString,
    collection_id: &str,
    state_collection_id: &str,
    incoming_sync_key: &str,
    window: usize,
    options: Option<&crate::eas_sync_options::EasSyncCollectionOptions>,
    conversation_mode: bool,
) -> anyhow::Result<String> {
    // Sticky <Options> ([MS-ASCMD] §2.2.3.125.6) resolve in
    // handle_sync_collections before dispatch; an absent block falls back
    // to the negotiation defaults (native body, untruncated).
    let default_options = crate::eas_sync_options::EasSyncCollectionOptions::default();
    let resolved_collection_options = options.unwrap_or(&default_options);
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
        Some(j) => j.clone(),
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

    // For initial sync (sync_key="0"), fetch all emails and store JMAP state token.
    // Routing keys off cursor presence: a clipped initial sync returns a new
    // SyncKey plus <MoreAvailable/>, so the client's follow-up request carries
    // a non-zero key that must RESUME the initial sync, not diff deltas.
    let cursor_present = state
        .storage
        .get_email_sync_cursor(username, state_collection_id)
        .await
        .ok()
        .flatten()
        .is_some();
    match route_email_sync(incoming_sync_key, cursor_present) {
        EmailSyncRoute::Initial => {
            // A fresh prime must never inherit a previous sync's continuation:
            // after a Status 3 re-priming, re-provisioning or an account reset
            // the client restarts from SyncKey "0" and must see the first
            // messages again, not the tail of the abandoned cursor. A stale
            // overflow queue from the previous sync state is equally invalid.
            if let Err(e) = state
                .storage
                .clear_email_sync_cursor(username, state_collection_id)
                .await
            {
                tracing::warn!(error = %e, "Failed to clear email sync cursor on re-prime");
            }
            if let Err(e) = state
                .storage
                .clear_email_sync_pending(username, state_collection_id)
                .await
            {
                tracing::warn!(error = %e, "Failed to clear email sync queue on re-prime");
            }
            return handle_email_initial_sync(
                &EmailSyncCtx {
                    state,
                    jmap: &jmap,
                    account_id: &account_id,
                    username,
                    password,
                    collection_id,
                    state_collection_id,
                    window,
                    options: resolved_collection_options,
                    conversation_mode,
                },
                mailbox_role,
            )
            .await;
        }
        EmailSyncRoute::InitialContinuation => {
            handle_email_initial_sync(
                &EmailSyncCtx {
                    state,
                    jmap: &jmap,
                    account_id: &account_id,
                    username,
                    password,
                    collection_id,
                    state_collection_id,
                    window,
                    options: resolved_collection_options,
                    conversation_mode,
                },
                mailbox_role,
            )
            .await
        }
        EmailSyncRoute::Delta => {
            handle_email_delta_sync(&EmailSyncCtx {
                state,
                jmap: &jmap,
                account_id: &account_id,
                username,
                password,
                collection_id,
                state_collection_id,
                window,
                options: resolved_collection_options,
                conversation_mode,
            })
            .await
        }
    }
}

/// Request-scoped context for one email collection Sync, mirroring `SyncCtx`
/// so the handlers stay under clippy's argument-count threshold.
#[derive(Clone, Copy)]
struct EmailSyncCtx<'a> {
    state: &'a Arc<AppState>,
    jmap: &'a Arc<JmapClient>,
    account_id: &'a str,
    username: &'a str,
    password: &'a SecretString,
    collection_id: &'a str,
    state_collection_id: &'a str,
    window: usize,
    /// The sticky-resolved per-collection `<Options>` block
    /// ([MS-ASCMD] §2.2.3.125.6) that shapes every rendered body.
    options: &'a crate::eas_sync_options::EasSyncCollectionOptions,
    /// `<airsync:ConversationMode>` for this collection ([MS-ASCMD] §2.2.3.36.2).
    conversation_mode: bool,
}

/// Initial email sync (EAS SyncKey "0"): page the mailbox by `position`,
/// honoring the response window with `<MoreAvailable/>` + a persisted
/// resume cursor (MS-ASCMD §2.2.3.199).
async fn handle_email_initial_sync(
    ctx: &EmailSyncCtx<'_>,
    mailbox_role: &str,
) -> anyhow::Result<String> {
    let EmailSyncCtx {
        state,
        jmap: _,
        account_id,
        username,
        password,
        collection_id,
        state_collection_id,
        window,
        options,
        conversation_mode,
    } = *ctx;
    // A previous initial sync may have left a resume cursor (window filled
    // before the mailbox drained). Spec-wise the client follows up with the
    // new SyncKey, and this is the request that must continue, not restart.
    //
    // Paging note: `Email/query` sorts `receivedAt` descending (newest
    // first), so mail that arrives while the initial sync paginates inserts
    // BEFORE the resume position and shifts the tail backward — the next
    // window re-delivers the boundary item (a duplicate Add, which clients
    // resolve by ServerId) and never skips past one. A message destroyed in
    // the undelivered region is simply never needed; one destroyed after
    // delivery is reported by the delta diff. Position paging is therefore
    // stable-with-duplicates under concurrent arrival, not lossy.
    let (start_position, cursor_state) = state
        .storage
        .get_email_sync_cursor(username, state_collection_id)
        .await
        .unwrap_or(None)
        .unwrap_or((0, None));

    // Fetch emails from JMAP for the requested mailbox, one window at a time.
    let fetch_limit = window.max(1) as u64;
    let result = match crate::email::fetch_emails_jmap(
        state,
        &crate::email::FetchEmailsParams {
            account_id,
            mailbox_role,
            position: start_position,
            limit: fetch_limit,
            username,
            password,
            search_filter: None,
        },
    )
    .await
    {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(error = %e, "Failed to fetch emails from JMAP for initial sync");
            let new_sync_key = Uuid::new_v4().simple().to_string();
            if let Err(e) = state
                .storage
                .set_sync_key(username, state_collection_id, &new_sync_key, None)
                .await
            {
                tracing::warn!(error = %e, "Failed to set initial email sync key");
            }
            return Ok(format!(
                "<Collection><Class>Email</Class><SyncKey>{}</SyncKey><CollectionId>{}</CollectionId><Status>1</Status></Collection>",
                new_sync_key, collection_id
            ));
        }
    };

    let new_sync_key = Uuid::new_v4().simple().to_string();

    // JMAP reports the full mailbox size in `total`; the delivered page is
    // `emails.len()`. The window is exhausted while content remains when
    // `start_position + delivered < total`.
    let delivered = result.emails.len() as u64;
    let remaining = result
        .total
        .saturating_sub(start_position.saturating_add(delivered));

    // The `state` token captured here (or carried over from the cursor) is
    // the delta base once the initial sync drains. `fetch_emails_jmap` only
    // fills `state` for roles backed by a JMAP mailbox, so keep the cursor's
    // token when this page could not supply one.
    let state_token = if result.state.is_empty() {
        cursor_state.clone()
    } else {
        Some(result.state.clone())
    };

    let mut commands_xml = String::new();
    for email in &result.emails {
        let jmap_id = email.id.as_deref().unwrap_or("unknown");
        let server_id = crate::email::email_server_id_from_jmap_id(jmap_id);
        let app_data = crate::email::render_jmap_email_as_eas_application_data_with_options(
            email,
            &server_id,
            collection_id,
            options,
            conversation_mode,
            true,
        );
        commands_xml.push_str(&format!(
            "<Add><ServerId>{}</ServerId><ApplicationData>{}</ApplicationData></Add>",
            server_id, app_data,
        ));
    }

    tracing::info!(
        user = %username,
        collection_id,
        position = start_position,
        delivered = delivered,
        total = result.total,
        remaining,
        email_count = result.emails.len(),
        sync_type = "initial",
        "Building EAS email sync response"
    );

    if remaining > 0 {
        // Persist the resume point; the stored JMAP state token stays at the
        // initial-sync value so the delta diff starts from a stable base once
        // the cursor drains.
        if let Err(e) = state
            .storage
            .set_email_sync_cursor(
                username,
                state_collection_id,
                start_position + delivered,
                state_token.as_deref(),
            )
            .await
        {
            tracing::warn!(error = %e, "Failed to persist email initial-sync cursor");
        }
        if let Err(e) = state
            .storage
            .set_sync_key(
                username,
                state_collection_id,
                &new_sync_key,
                state_token.as_deref(),
            )
            .await
        {
            tracing::warn!(error = %e, "Failed to set initial email sync key with JMAP state");
        }
        let response = format!(
            "<Collection><Class>Email</Class><SyncKey>{}</SyncKey><CollectionId>{}</CollectionId><Status>1</Status><MoreAvailable/><Commands>{}</Commands></Collection>",
            new_sync_key, collection_id, commands_xml
        );
        tracing::info!(
            user = %username,
            collection_id,
            sync_type = "initial",
            window_clipped = true,
            "EAS email sync response built"
        );
        return Ok(response);
    }

    // Mailbox drained: drop the cursor, persist the state token for deltas.
    if let Err(e) = state
        .storage
        .clear_email_sync_cursor(username, state_collection_id)
        .await
    {
        tracing::warn!(error = %e, "Failed to clear email initial-sync cursor");
    }
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

    let response = format!(
        "<Collection><Class>Email</Class><SyncKey>{}</SyncKey><CollectionId>{}</CollectionId><Status>1</Status><Commands>{}</Commands></Collection>",
        new_sync_key, collection_id, commands_xml
    );
    tracing::info!(
        user = %username,
        collection_id,
        sync_type = "initial",
        email_count = result.emails.len(),
        "EAS email sync response built"
    );
    Ok(response)
}

/// Delta email sync (EAS SyncKey != "0"): drain a persisted overflow queue
/// first, then diff `Email/changes` from the stored state token, honoring the
/// response window with `<MoreAvailable/>` (MS-ASCMD §2.2.3.199).
async fn handle_email_delta_sync(ctx: &EmailSyncCtx<'_>) -> anyhow::Result<String> {
    let EmailSyncCtx {
        state,
        jmap,
        account_id,
        username,
        password,
        collection_id,
        state_collection_id,
        window,
        ..
    } = *ctx;
    let new_sync_key = Uuid::new_v4().simple().to_string();

    // Stored (sync key, JMAP state token).
    let previous_state = state
        .storage
        .get_sync_key(username, state_collection_id)
        .await?;
    let jmap_state_token = previous_state.as_ref().and_then(|(_, token)| token.clone());

    // ---- 1) Drain a previously persisted overflow queue first. ----------
    // Its ops precede (in delivery order) anything a fresh diff would
    // return, and its `base_state` is the only point the stored token may
    // advance to without re-delivering queued ops.
    if let Some((ops_json, base_state)) = state
        .storage
        .get_email_sync_pending(username, state_collection_id)
        .await?
    {
        let queue: Vec<PendingEmailOp> = serde_json::from_str(&ops_json).unwrap_or_default();
        let take = queue.len().min(window.max(1));
        let (deliver, leftover) = queue.split_at(take);

        let mut commands_xml = String::new();
        for op in deliver {
            commands_xml.push_str(&render_pending_email_op(ctx, op).await);
        }

        if !leftover.is_empty() {
            // Still more behind this window: keep the queue (and the stored
            // token exactly where it is) and tell the client to sync again.
            if let Ok(leftover_json) = serde_json::to_string(&leftover)
                && let Err(e) = state
                    .storage
                    .set_email_sync_pending(
                        username,
                        state_collection_id,
                        &leftover_json,
                        &base_state,
                    )
                    .await
            {
                tracing::warn!(error = %e, "Failed to persist email delta queue remainder");
            }
            if let Err(e) = state
                .storage
                .set_sync_key(
                    username,
                    state_collection_id,
                    &new_sync_key,
                    jmap_state_token.as_deref(),
                )
                .await
            {
                tracing::warn!(error = %e, "Failed to set email sync key while draining queue");
            }
            tracing::info!(
                user = %username,
                collection_id,
                delivered = take,
                queued = leftover.len(),
                sync_type = "delta-queue",
                window_clipped = true,
                "Building EAS email delta sync response from overflow queue"
            );
            return Ok(format!(
                "<Collection><Class>Email</Class><SyncKey>{}</SyncKey><CollectionId>{}</CollectionId><Status>1</Status><MoreAvailable/><Commands>{}</Commands></Collection>",
                new_sync_key, collection_id, commands_xml
            ));
        }

        // Queue drained within this window: advance the stored token to the
        // queue's base state. The next Sync diffs from there and picks up
        // everything that happened since — no re-delivery, no loss.
        if let Err(e) = state
            .storage
            .clear_email_sync_pending(username, state_collection_id)
            .await
        {
            tracing::warn!(error = %e, "Failed to clear drained email delta queue");
        }
        if let Err(e) = state
            .storage
            .set_sync_key(
                username,
                state_collection_id,
                &new_sync_key,
                Some(&base_state),
            )
            .await
        {
            tracing::warn!(error = %e, "Failed to advance email sync key to queue base state");
        }
        tracing::info!(
            user = %username,
            collection_id,
            delivered = take,
            sync_type = "delta-queue",
            queue_drained = true,
            "Building EAS email delta sync response from overflow queue"
        );
        return Ok(format!(
            "<Collection><Class>Email</Class><SyncKey>{}</SyncKey><CollectionId>{}</CollectionId><Status>1</Status><Commands>{}</Commands></Collection>",
            new_sync_key, collection_id, commands_xml
        ));
    }

    // ---- 2) Fresh diff from the stored JMAP state token. ---------------
    let Some(old_state) = jmap_state_token.clone() else {
        // No usable state token yet (e.g. the initial sync never captured
        // one because the role had no JMAP mailbox). Keep the client's
        // progress valid and let the next initial sync establish a base.
        if let Err(e) = state
            .storage
            .set_sync_key(username, state_collection_id, &new_sync_key, None)
            .await
        {
            tracing::warn!(error = %e, "Failed to update email sync key");
        }
        return Ok(format!(
            "<Collection><Class>Email</Class><SyncKey>{}</SyncKey><CollectionId>{}</CollectionId><Status>1</Status></Collection>",
            new_sync_key, collection_id
        ));
    };

    let (ops, final_state) = match accumulate_email_changes(
        jmap, account_id, &old_state, username, password,
    )
    .await
    {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(error = %e, "JMAP Email/changes failed; deferring delta sync");
            if let Err(e) = state
                .storage
                .set_sync_key(
                    username,
                    state_collection_id,
                    &new_sync_key,
                    jmap_state_token.as_deref(),
                )
                .await
            {
                tracing::warn!(error = %e, "Failed to update email sync key");
            }
            // Backend hiccup: answer an empty, successful window rather
            // than a Status 6 so the client retries the delta instead
            // of forcing a full resync.
            return Ok(format!(
                "<Collection><Class>Email</Class><SyncKey>{}</SyncKey><CollectionId>{}</CollectionId><Status>1</Status></Collection>",
                new_sync_key, collection_id
            ));
        }
    };

    // ---- 3) Window the accumulated ops. ---------------------------------
    let take = ops.len().min(window.max(1));
    let (deliver, leftover) = ops.split_at(take);

    let mut commands_xml = String::new();
    for op in deliver {
        commands_xml.push_str(&render_pending_email_op(ctx, op).await);
    }

    if !leftover.is_empty() {
        // Overflow: stash the remainder plus the state that follows the full
        // change set, and DO NOT advance the stored token (the next diff
        // must not re-report the queued ops).
        if let Ok(leftover_json) = serde_json::to_string(leftover)
            && let Err(e) = state
                .storage
                .set_email_sync_pending(username, state_collection_id, &leftover_json, &final_state)
                .await
        {
            tracing::warn!(error = %e, "Failed to persist email delta overflow queue");
        }
        if let Err(e) = state
            .storage
            .set_sync_key(
                username,
                state_collection_id,
                &new_sync_key,
                Some(&old_state),
            )
            .await
        {
            tracing::warn!(error = %e, "Failed to set email sync key while truncating delta");
        }
        tracing::info!(
            user = %username,
            collection_id,
            changed_count = ops.len(),
            delivered = take,
            queued = leftover.len(),
            sync_type = "delta",
            window_clipped = true,
            "Building EAS email delta sync response"
        );
        return Ok(format!(
            "<Collection><Class>Email</Class><SyncKey>{}</SyncKey><CollectionId>{}</CollectionId><Status>1</Status><MoreAvailable/><Commands>{}</Commands></Collection>",
            new_sync_key, collection_id, commands_xml
        ));
    }

    // Everything fits: advance to the state that follows the delivered set
    // and finish the window.
    if let Err(e) = state
        .storage
        .set_sync_key(
            username,
            state_collection_id,
            &new_sync_key,
            Some(&final_state),
        )
        .await
    {
        tracing::warn!(error = %e, "Failed to update email sync key with JMAP state");
    }

    tracing::info!(
        user = %username,
        collection_id,
        changed_count = ops.len(),
        sync_type = "delta",
        "Building EAS email delta sync response"
    );
    Ok(format!(
        "<Collection><Class>Email</Class><SyncKey>{}</SyncKey><CollectionId>{}</CollectionId><Status>1</Status><Commands>{}</Commands></Collection>",
        new_sync_key, collection_id, commands_xml
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
                global_window_size: if sync_collections.is_empty() {
                    // Legacy single-collection requests carry their window as
                    // a direct child of Sync: it is the collection's own
                    // window, not a global budget (a budget of one collection
                    // would be redundant and could over-clip the response).
                    None
                } else {
                    parse_global_window_size(&xml)
                },
            };
            if sync_collections.is_empty() {
                // Fallback: use the single-collection fields from EasRequest
                // (backward compat for clients that don't nest Collection elements)
                let collection_id = req.collection_id.as_deref().unwrap_or("1");
                let incoming_key = req.sync_key.as_deref().unwrap_or("0");
                let class = req.class.as_deref().unwrap_or("Calendar");
                let parsed_legacy = crate::eas_sync_options::parse_collection_options(&xml);
                let sc = SyncCollection {
                    sync_key: Some(incoming_key.to_string()),
                    collection_id: Some(collection_id.to_string()),
                    class: Some(class.to_string()),
                    window_size: req.window_size,
                    // EasRequest.get_changes defaults to true when absent
                    get_changes: req.get_changes,
                    filter_type: req.filter_type,
                    conversation_mode: parsed_legacy.controls.conversation_mode.unwrap_or(false),
                    options: if parsed_legacy.options.explicitly_set {
                        Some(parsed_legacy.options)
                    } else {
                        None
                    },
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
                &PingInvocation {
                    owner: &username,
                    password: &password,
                    req: &req,
                    xml: &xml,
                    request_id: &request_id,
                },
                &wbxml,
                wants_wbxml,
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
                &device_id,
                multipart_requested(&headers),
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
            handle_meeting_response(&state, &username, &password, &xml, &wbxml, wants_wbxml, &request_id)
                .await
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
            assert_eq!(
                ping_folder_kind(&folder),
                expected,
                "id={} class={}",
                id,
                class
            );
        }
    }

    #[tokio::test]
    async fn test_ping_email_folder_wakes_on_push_event() {
        let state = test_ping_state().await;
        let xml = r#"<?xml version="1.0" encoding="utf-8"?>
<Ping xmlns="Ping:"><HeartbeatInterval>120</HeartbeatInterval>
<Folders><Folder><Id>4</Id><Class>Email</Class></Folder></Folders></Ping>"#;
        let wbxml = Wbxml::new();
        let req = EasRequest {
            command: "Ping".to_string(),
            device_id: Some("push-test-device".to_string()),
            ..Default::default()
        };

        let mgr = state.subscription_manager.clone();
        let publisher = tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            mgr.publish(crate::notifications::NotificationEvent::NewMail {
                owner: "user@example.com".to_string(),
                folder_id: "mb-inbox".to_string(),
                item_id: "email-1".to_string(),
                change_key: "s-1".to_string(),
            });
        });

        let password = SecretString::from("pw");
        let resp = handle_ping(
            &state,
            &PingInvocation {
                owner: "user@example.com",
                password: &password,
                req: &req,
                xml,
                request_id: "req-push-wake",
            },
            &wbxml,
            false,
        )
        .await;
        publisher.abort();

        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .expect("response body");
        let text = String::from_utf8_lossy(&body);
        assert!(
            text.contains("<Status>2</Status>"),
            "push event must wake Ping with Status 2, got: {text}"
        );
        assert!(
            text.contains("<Folder>4</Folder>"),
            "changed folder must be listed, got: {text}"
        );
        assert_eq!(
            state.push_registry.monitor_count(),
            0,
            "Ping exit must release its push-monitor sink"
        );
    }

    /// Test AppState identically shaped to the live-push test above: JMAP
    /// pointed at unroutable loopback so monitors idle and probes fail fast.
    /// Fresh per test so push-registry counts and storage are exact.
    async fn test_ping_state() -> Arc<AppState> {
        use crate::models::AppState;
        let storage = crate::storage::Storage::new("sqlite::memory:")
            .await
            .expect("in-memory storage");
        storage.init_schema().await.expect("schema init");
        let cfg = crate::config::Config {
            jmap_base: "http://127.0.0.1:1".to_string(),
            email_enabled: true,
            mail_domain: "example.com".to_string(),
            hmac_secret: SecretString::from("a".repeat(32)),
            ..Default::default()
        };
        Arc::new(AppState::new(cfg, Arc::new(storage)))
    }

    fn ping_xml(heartbeat: u64, folders: &str) -> String {
        format!(
            r#"<?xml version="1.0" encoding="utf-8"?>
<Ping xmlns="Ping:"><HeartbeatInterval>{heartbeat}</HeartbeatInterval>
<Folders>{folders}</Folders></Ping>"#
        )
    }

    async fn response_text(resp: Response) -> String {
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .expect("response body");
        String::from_utf8_lossy(&body).into_owned()
    }

    #[test]
    fn test_effective_ping_heartbeat_clamping() {
        use crate::config::DEFAULT_CLOUDFLARE_PING_CAP_SECS;

        // A configured cap applies to every Ping, Cloudflare or not.
        assert_eq!(effective_ping_heartbeat(3540, Some(80), false), 80);
        assert_eq!(effective_ping_heartbeat(3540, Some(80), true), 80);
        assert_eq!(effective_ping_heartbeat(60, Some(80), false), 60);
        // Without a configured cap, only Cloudflare-proxied requests clamp.
        assert_eq!(
            effective_ping_heartbeat(3540, None, true),
            DEFAULT_CLOUDFLARE_PING_CAP_SECS
        );
        assert_eq!(effective_ping_heartbeat(3540, None, false), 3540);
        assert_eq!(effective_ping_heartbeat(60, None, true), 60);
        assert_eq!(
            DEFAULT_CLOUDFLARE_PING_CAP_SECS, 80,
            "cap must stay below Cloudflare's 125s Proxy Read Timeout"
        );
    }

    #[test]
    fn test_parse_request_detects_cloudflare_via_cf_ray() {
        let xml = ping_xml(300, r#"<Folder><Id>4</Id><Class>Email</Class></Folder>"#);
        let query = HashMap::new();

        let mut headers = HeaderMap::new();
        let req = parse_request(&query, &xml, &headers);
        assert!(
            !req.via_cloudflare,
            "direct request must not be flagged as Cloudflare-proxied"
        );

        headers.insert("CF-RAY", HeaderValue::from_static("8f3b2a1c0d9e4f56-FRA"));
        let req = parse_request(&query, &xml, &headers);
        assert!(
            req.via_cloudflare,
            "CF-RAY header must flag the request as Cloudflare-proxied"
        );
    }

    #[tokio::test]
    async fn test_ping_honors_configured_heartbeat_cap() {
        use crate::models::AppState;
        let storage = crate::storage::Storage::new("sqlite::memory:")
            .await
            .expect("in-memory storage");
        storage.init_schema().await.expect("schema init");
        let cfg = crate::config::Config {
            jmap_base: "http://127.0.0.1:1".to_string(),
            email_enabled: true,
            mail_domain: "example.com".to_string(),
            hmac_secret: SecretString::from("a".repeat(32)),
            max_ping_heartbeat_secs: Some(2),
            ..Default::default()
        };
        let state = Arc::new(AppState::new(cfg, Arc::new(storage)));

        let xml = ping_xml(600, r#"<Folder><Id>4</Id><Class>Email</Class></Folder>"#);
        let wbxml = Wbxml::new();
        let req = EasRequest {
            command: "Ping".to_string(),
            device_id: Some("cap-device".to_string()),
            // The configured cap applies regardless of Cloudflare detection.
            via_cloudflare: false,
            ..Default::default()
        };
        let password = SecretString::from("pw");

        let started = Instant::now();
        let resp = timeout(
            StdDuration::from_secs(10),
            handle_ping(
                &state,
                &PingInvocation {
                    owner: "cap-user@example.com",
                    password: &password,
                    req: &req,
                    xml: &xml,
                    request_id: "req-ping-cap",
                },
                &wbxml,
                false,
            ),
        )
        .await
        .expect("Ping must not wait the full 600s heartbeat");
        let text = response_text(resp).await;
        assert!(
            text.contains("<Status>1</Status>"),
            "capped Ping must end with Status 1, got: {text}"
        );
        let elapsed = started.elapsed();
        assert!(
            elapsed >= StdDuration::from_secs(2),
            "Ping returned before the configured cap elapsed ({elapsed:?})"
        );
        assert!(
            elapsed < StdDuration::from_secs(10),
            "Ping must stop at the 2s cap, not the 600s heartbeat ({elapsed:?})"
        );
    }

    #[tokio::test]
    async fn test_ping_cache_entry_expires_after_ttl() {
        let owner = "ttl-user@example.com";
        let key = format!("{}:ttl-device", owner);
        ping_cache_store(
            owner,
            key.clone(),
            PingCacheEntry {
                heartbeat: 300,
                folders: vec![PingFolder {
                    id: "4".to_string(),
                    class_name: "Email".to_string(),
                }],
                last_seen: Instant::now(),
            },
        )
        .await;
        assert!(
            ping_cache_lookup(&key).await.is_some(),
            "fresh entry must be visible"
        );

        // Age the entry beyond the TTL and confirm it vanishes rather than
        // steering a future bare Ping with stale folders/heartbeat.
        {
            let mut cache = PING_CACHE.lock().await;
            if let Some(entry) = cache.get_mut(&key) {
                entry.last_seen = Instant::now()
                    .checked_sub(PING_CACHE_TTL + StdDuration::from_secs(5))
                    .expect("TTL fits within Instant history");
            }
        }
        assert!(
            ping_cache_lookup(&key).await.is_none(),
            "stale entry must be expired"
        );
        let cache = PING_CACHE.lock().await;
        assert!(
            cache.peek(&key).is_none(),
            "expired entry must be evicted, not just hidden"
        );
    }

    #[tokio::test]
    async fn test_ping_cache_caps_devices_per_owner() {
        let owner = "cap-owner@example.com";
        let total = MAX_PING_DEVICES_PER_OWNER + 3;
        for i in 0..total {
            ping_cache_store(
                owner,
                format!("{}:device-{}", owner, i),
                PingCacheEntry {
                    heartbeat: 300,
                    folders: Vec::new(),
                    last_seen: Instant::now(),
                },
            )
            .await;
            // Distinct last_seen so eviction order is deterministic.
            tokio::task::yield_now().await;
        }
        let cache = PING_CACHE.lock().await;
        let prefix = format!("{}:", owner);
        let kept = cache.iter().filter(|(k, _)| k.starts_with(&prefix)).count();
        assert_eq!(
            kept, MAX_PING_DEVICES_PER_OWNER,
            "re-provision churn must not exceed the per-owner device cap"
        );
        // Newest entries survive; the oldest is evicted.
        assert!(
            cache
                .peek(&format!("{}:device-{}", owner, total - 1))
                .is_some()
        );
        assert!(cache.peek(&format!("{}:device-{}", owner, 0)).is_none());
    }

    #[tokio::test]
    async fn test_ping_cache_key_truncates_long_device_id() {
        let long_id = "x".repeat(500);
        assert_eq!(ping_device_key_part(&long_id).len(), MAX_PING_DEVICE_ID_LEN);
    }

    #[tokio::test]
    async fn test_ping_superseded_by_newer_ping_same_device() {
        let state = test_ping_state().await;
        let owner = "supersede-user@example.com";
        let password = SecretString::from("pw");
        let wbxml = Wbxml::new();
        let xml = ping_xml(300, r#"<Folder><Id>4</Id><Class>Email</Class></Folder>"#);

        let req_old = EasRequest {
            command: "Ping".to_string(),
            device_id: Some("supersede-device".to_string()),
            ..Default::default()
        };
        let state_old = state.clone();
        let wbxml_old = Wbxml::new();
        let xml_old = xml.clone();
        let pw_old = SecretString::from("pw");
        let old_ping = tokio::spawn(async move {
            handle_ping(
                &state_old,
                &PingInvocation {
                    owner,
                    password: &pw_old,
                    req: &req_old,
                    xml: &xml_old,
                    request_id: "req-ping-old",
                },
                &wbxml_old,
                false,
            )
            .await
        });

        // Let the old Ping fully register before the newer one arrives.
        tokio::time::sleep(StdDuration::from_millis(150)).await;
        assert!(
            PING_IN_FLIGHT.contains_key("supersede-user@example.com:supersede-device"),
            "old Ping must be in flight"
        );

        let req_new = EasRequest {
            command: "Ping".to_string(),
            device_id: Some("supersede-device".to_string()),
            ..Default::default()
        };
        let state_new = state.clone();
        let new_ping = tokio::spawn(async move {
            handle_ping(
                &state_new,
                &PingInvocation {
                    owner,
                    password: &password,
                    req: &req_new,
                    xml: &xml,
                    request_id: "req-ping-new",
                },
                &wbxml,
                false,
            )
            .await
        });

        let old_text = response_text(
            timeout(StdDuration::from_secs(10), old_ping)
                .await
                .expect("superseded Ping must answer promptly")
                .expect("spawned Ping must not panic"),
        )
        .await;
        assert!(
            old_text.contains("<Status>1</Status>"),
            "superseded Ping must answer Status 1, got: {old_text}"
        );
        assert!(
            !old_text.contains("<Status>2</Status>"),
            "superseded Ping must not report changes, got: {old_text}"
        );
        assert!(
            PING_IN_FLIGHT.contains_key("supersede-user@example.com:supersede-device"),
            "the newer Ping's entry survives the old Ping's cleanup"
        );

        // The surviving Ping is still live: wake it with a push event and
        // confirm it answers Status 2 and cleans up the in-flight map.
        state
            .subscription_manager
            .publish(crate::notifications::NotificationEvent::NewMail {
                owner: owner.to_string(),
                folder_id: "mb-inbox".to_string(),
                item_id: "email-9".to_string(),
                change_key: "s-1".to_string(),
            });
        let new_text = response_text(
            timeout(StdDuration::from_secs(10), new_ping)
                .await
                .expect("surviving Ping must answer on change")
                .expect("spawned Ping must not panic"),
        )
        .await;
        assert!(
            new_text.contains("<Status>2</Status>"),
            "surviving Ping must wake on change, got: {new_text}"
        );
        assert!(
            !PING_IN_FLIGHT.contains_key("supersede-user@example.com:supersede-device"),
            "supersede test device entry must be removed after both Pings end"
        );
        assert_eq!(state.push_registry.monitor_count(), 0);
    }

    #[tokio::test]
    async fn test_ping_multiple_devices_share_monitor_and_both_wake() {
        let state = test_ping_state().await;
        let owner = "multi-device@example.com";
        let xml = ping_xml(300, r#"<Folder><Id>2</Id><Class>Email</Class></Folder>"#);

        let mut handles = Vec::new();
        for (device, req_id) in [("android-phone", "r1"), ("tablet", "r2")] {
            let st = state.clone();
            let xml = xml.clone();
            let handle = tokio::spawn(async move {
                let req = EasRequest {
                    command: "Ping".to_string(),
                    device_id: Some(device.to_string()),
                    ..Default::default()
                };
                let pw = SecretString::from("pw");
                let wbxml = Wbxml::new();
                handle_ping(
                    &st,
                    &PingInvocation {
                        owner,
                        password: &pw,
                        req: &req,
                        xml: &xml,
                        request_id: req_id,
                    },
                    &wbxml,
                    false,
                )
                .await
            });
            handles.push(handle);
        }

        tokio::time::sleep(StdDuration::from_millis(150)).await;
        assert_eq!(
            state.push_registry.monitor_count(),
            1,
            "two devices of one mailbox share a single push monitor"
        );
        assert!(
            PING_IN_FLIGHT.contains_key("multi-device@example.com:android-phone")
                && PING_IN_FLIGHT.contains_key("multi-device@example.com:tablet"),
            "each device has its own Ping"
        );

        state
            .subscription_manager
            .publish(crate::notifications::NotificationEvent::NewMail {
                owner: owner.to_string(),
                folder_id: "mb-inbox".to_string(),
                item_id: "email-7".to_string(),
                change_key: "s-2".to_string(),
            });

        for h in handles {
            let text = response_text(
                timeout(StdDuration::from_secs(10), h)
                    .await
                    .expect("both device Pings must wake")
                    .expect("spawned Ping must not panic"),
            )
            .await;
            assert!(
                text.contains("<Status>2</Status>"),
                "each device wakes on the shared monitor's event, got: {text}"
            );
        }
        assert_eq!(
            state.push_registry.monitor_count(),
            0,
            "monitor torn down once the last device's Ping ends"
        );
        assert!(
            !PING_IN_FLIGHT.contains_key("multi-device@example.com:android-phone")
                && !PING_IN_FLIGHT.contains_key("multi-device@example.com:tablet")
        );
    }

    #[tokio::test]
    async fn test_ping_journal_history_does_not_storm_email_folder() {
        let state = test_ping_state().await;
        let owner = "storm-user@example.com";

        // Simulate journal history (e.g. surviving a container restart):
        // calendar upserts + a deletion, plus an email folder whose sync
        // state is a JMAP token — never a `seq:` watermark.
        state
            .storage
            .upsert_item_map(owner, "http://cal/1", "calendar/", "cal-1", "uid-1", "e1")
            .await
            .expect("journal upsert");
        state
            .storage
            .add_delete_tombstone(owner, "cal-2")
            .await
            .expect("journal delete");
        state
            .storage
            .set_sync_key(owner, "4::storm-device", "synckey", Some("s-42"))
            .await
            .expect("store JMAP state token");

        let password = SecretString::from("pw");
        let req = EasRequest {
            command: "Ping".to_string(),
            device_id: Some("storm-device".to_string()),
            ..Default::default()
        };
        let xml = ping_xml(60, r#"<Folder><Id>4</Id><Class>Email</Class></Folder>"#);
        let wbxml = Wbxml::new();

        // Before the fix, this returned Status 2 immediately (and forever):
        // since=0 matched every journaled row. It must now stay asleep.
        let result = timeout(
            StdDuration::from_millis(600),
            handle_ping(
                &state,
                &PingInvocation {
                    owner,
                    password: &password,
                    req: &req,
                    xml: &xml,
                    request_id: "req-storm",
                },
                &wbxml,
                false,
            ),
        )
        .await;
        assert!(
            result.is_err(),
            "email Ping must not answer Status 2 from unrelated journal history"
        );
        assert_eq!(state.push_registry.monitor_count(), 0);
        assert!(!PING_IN_FLIGHT.contains_key("storm-user@example.com:storm-device"));
    }

    #[tokio::test]
    async fn test_ping_unsynced_calendar_folder_uses_baseline_not_zero() {
        let state = test_ping_state().await;
        let owner = "baseline-user@example.com";

        // Journal history predating this device's first calendar sync.
        state
            .storage
            .upsert_item_map(owner, "http://cal/a", "calendar/", "cal-a", "uid-a", "ea")
            .await
            .expect("journal upsert");
        state
            .storage
            .upsert_item_map(owner, "http://cal/b", "calendar/", "cal-b", "uid-b", "eb")
            .await
            .expect("journal upsert");

        let password = SecretString::from("pw");
        let req = EasRequest {
            command: "Ping".to_string(),
            device_id: Some("baseline-device".to_string()),
            ..Default::default()
        };
        let xml = ping_xml(60, r#"<Folder><Id>1</Id><Class>Calendar</Class></Folder>"#);
        let wbxml = Wbxml::new();

        let ping = tokio::spawn(async move {
            handle_ping(
                &state,
                &PingInvocation {
                    owner,
                    password: &password,
                    req: &req,
                    xml: &xml,
                    request_id: "req-baseline",
                },
                &wbxml,
                false,
            )
            .await
        });

        // Pre-Ping history must not fire; the Ping sleeps at its baseline.
        tokio::time::sleep(StdDuration::from_millis(300)).await;
        assert!(
            !ping.is_finished(),
            "pre-existing journal history must not wake the new-device Ping"
        );
        ping.abort();
        let _ = ping.await; // join so the guard's Drop runs before we assert
        assert!(!PING_IN_FLIGHT.contains_key("baseline-user@example.com:baseline-device"));
    }

    #[tokio::test]
    async fn test_ping_uses_journal_seq_column_when_token_is_provider_state() {
        let state = test_ping_state().await;
        let owner = "colwm-user@example.com";

        // History the client already consumed; the (JMAP calendar path) token
        // is a provider state, with the journal watermark in the column.
        state
            .storage
            .upsert_item_map(owner, "http://cal/1", "calendar/", "cal-1", "uid-1", "e1")
            .await
            .expect("journal upsert");
        let head = state
            .storage
            .get_latest_change_seq()
            .await
            .expect("journal head");
        state
            .storage
            .set_sync_key(
                owner,
                "1::colwm-device",
                "synckey",
                Some("Some(query-state)"),
            )
            .await
            .expect("store provider token");
        state
            .storage
            .set_journal_watermark(owner, "1::colwm-device", head)
            .await
            .expect("store watermark");

        let password = SecretString::from("pw");
        let req = EasRequest {
            command: "Ping".to_string(),
            device_id: Some("colwm-device".to_string()),
            ..Default::default()
        };
        let xml = ping_xml(60, r#"<Folder><Id>1</Id><Class>Calendar</Class></Folder>"#);

        let st = state.clone();
        let ping = tokio::spawn(async move {
            let wbxml = Wbxml::new();
            handle_ping(
                &st,
                &PingInvocation {
                    owner,
                    password: &password,
                    req: &req,
                    xml: &xml,
                    request_id: "req-colwm",
                },
                &wbxml,
                false,
            )
            .await
        });

        tokio::time::sleep(StdDuration::from_millis(300)).await;
        assert!(
            !ping.is_finished(),
            "consumed history below the column watermark must not wake the Ping"
        );

        // A change past the watermark is still detected on the next tick.
        state
            .storage
            .upsert_item_map(owner, "http://cal/2", "calendar/", "cal-2", "uid-2", "e2")
            .await
            .expect("journal upsert");
        let resp = timeout(StdDuration::from_secs(20), ping)
            .await
            .expect("change past the column watermark must wake the Ping")
            .expect("spawned Ping must not panic");
        let text = response_text(resp).await;
        assert!(
            text.contains("<Status>2</Status>") && text.contains("<Folder>1</Folder>"),
            "change past the column watermark must report the folder, got: {text}"
        );
    }

    #[tokio::test]
    async fn test_get_item_estimate_counts_only_after_journal_watermark() {
        let state = test_ping_state().await;
        let owner = "estimate-user@example.com";

        // Two consumed rows, then one new row after the sync.
        state
            .storage
            .upsert_item_map(owner, "http://cal/1", "calendar/", "cal-1", "uid-1", "e1")
            .await
            .expect("journal upsert");
        state
            .storage
            .upsert_item_map(owner, "http://cal/2", "calendar/", "cal-2", "uid-2", "e2")
            .await
            .expect("journal upsert");
        let head = state
            .storage
            .get_latest_change_seq()
            .await
            .expect("journal head");
        state
            .storage
            .set_sync_key(
                owner,
                "1::est-device",
                "keystored",
                Some("non-seq-query-state"),
            )
            .await
            .expect("store provider token");
        state
            .storage
            .set_journal_watermark(owner, "1::est-device", head)
            .await
            .expect("store watermark");
        state
            .storage
            .upsert_item_map(owner, "http://cal/3", "calendar/", "cal-3", "uid-3", "e3")
            .await
            .expect("journal upsert");

        let req = EasRequest {
            command: "GetItemEstimate".to_string(),
            sync_key: Some("keystored".to_string()),
            collection_id: Some("1".to_string()),
            device_id: Some("est-device".to_string()),
            ..Default::default()
        };
        let resp =
            handle_get_item_estimate(&state, owner, &req, &Wbxml::new(), false, "req-estimate")
                .await;
        let text = response_text(resp).await;
        assert!(
            text.contains("<Estimate>1</Estimate>"),
            "estimate must count only rows after the column watermark, got: {text}"
        );
    }

    #[tokio::test]
    async fn test_ping_seq_watermark_change_wakes_calendar_folder() {
        let state = test_ping_state().await;
        let owner = "delta-user@example.com";

        // Device already delta-synced the calendar up to the current head…
        state
            .storage
            .upsert_item_map(owner, "http://cal/1", "calendar/", "cal-1", "uid-1", "e1")
            .await
            .expect("journal upsert");
        let head = state
            .storage
            .get_latest_change_seq()
            .await
            .expect("journal head");
        state
            .storage
            .set_sync_key(
                owner,
                "1::delta-device",
                "synckey",
                Some(&format!("seq:{head}")),
            )
            .await
            .expect("store seq watermark");

        let password = SecretString::from("pw");
        let req = EasRequest {
            command: "Ping".to_string(),
            device_id: Some("delta-device".to_string()),
            ..Default::default()
        };
        let xml = ping_xml(60, r#"<Folder><Id>1</Id><Class>Calendar</Class></Folder>"#);

        let st = state.clone();
        let ping = tokio::spawn(async move {
            let wbxml = Wbxml::new();
            handle_ping(
                &st,
                &PingInvocation {
                    owner,
                    password: &password,
                    req: &req,
                    xml: &xml,
                    request_id: "req-delta",
                },
                &wbxml,
                false,
            )
            .await
        });

        tokio::time::sleep(StdDuration::from_millis(300)).await;
        assert!(
            !ping.is_finished(),
            "no changes yet — Ping must stay asleep"
        );

        // …then a change lands past the watermark: Tick wake within 15s.
        state
            .storage
            .upsert_item_map(owner, "http://cal/2", "calendar/", "cal-2", "uid-2", "e2")
            .await
            .expect("journal upsert");

        let resp = timeout(StdDuration::from_secs(20), ping)
            .await
            .expect("calendar change past the watermark must wake the Ping")
            .expect("spawned Ping must not panic");
        let text = response_text(resp).await;
        assert!(
            text.contains("<Status>2</Status>") && text.contains("<Folder>1</Folder>"),
            "calendar change past the watermark must report the calendar folder, got: {text}"
        );
        assert!(!PING_IN_FLIGHT.contains_key("delta-user@example.com:delta-device"));
    }

    // ===================== Sync WindowSize (MS-ASCMD §2.2.3.199) ===========

    /// Extract the first `<SyncKey>` value from a rendered collection XML.
    fn first_sync_key(coll_xml: &str) -> String {
        let start = coll_xml
            .find("<SyncKey>")
            .expect("collection response must contain a SyncKey")
            + "<SyncKey>".len();
        let end = coll_xml[start..]
            .find("</SyncKey>")
            .expect("SyncKey must be closed")
            + start;
        coll_xml[start..end].to_string()
    }

    /// In-memory AppState with JMAP pointed at unroutable loopback, for
    /// exercising the gateway-local Tasks/Notes sync paths.
    async fn test_sync_state() -> Arc<AppState> {
        let storage = crate::storage::Storage::new("sqlite::memory:")
            .await
            .expect("in-memory storage");
        storage.init_schema().await.expect("schema init");
        let cfg = crate::config::Config {
            jmap_base: "http://127.0.0.1:1".to_string(),
            email_enabled: false,
            mail_domain: "example.com".to_string(),
            hmac_secret: SecretString::from("a".repeat(32)),
            ..Default::default()
        };
        Arc::new(AppState::new(cfg, Arc::new(storage)))
    }

    /// Seed `count` tasks owned by `owner` with server ids task-000…
    async fn seed_tasks(state: &AppState, owner: &str, count: usize) {
        for i in 0..count {
            state
                .storage
                .upsert_task(
                    owner,
                    &format!("task-{i:03}"),
                    &crate::storage::TaskFields {
                        subject: Some(&format!("Task {i}")),
                        importance: Some(1),
                        sensitivity: Some(0),
                        start_date: None,
                        due_date: None,
                        utc_start_date: None,
                        utc_due_date: None,
                        complete: 0,
                        date_completed: None,
                        reminder_set: 0,
                        reminder_time: None,
                        categories: None,
                        body: Some("seed"),
                    },
                )
                .await
                .expect("seed task");
        }
    }

    fn tasks_collection(sync_key: &str, window: Option<usize>) -> SyncCollection {
        SyncCollection {
            sync_key: Some(sync_key.to_string()),
            collection_id: Some(crate::tasks::TASKS_COLLECTION_ID.to_string()),
            class: Some("Tasks".to_string()),
            window_size: window,
            get_changes: true,
            filter_type: None,
            conversation_mode: false,
            options: None,
            xml: String::new(),
        }
    }

    #[test]
    fn test_parse_global_window_size_only_direct_child_of_sync_root() {
        // Present at depth 1 (direct child of Sync) → parsed and clamped.
        assert_eq!(
            parse_global_window_size(
                r#"<Sync xmlns="AirSync:"><WindowSize>25</WindowSize><Collections/></Sync>"#
            ),
            Some(25)
        );
        // Absent → None.
        assert_eq!(
            parse_global_window_size(r#"<Sync xmlns="AirSync:"><Collections/></Sync>"#),
            None
        );
        // Per-collection WindowSize only → NOT a global limit.
        assert_eq!(
            parse_global_window_size(
                r#"<Sync xmlns="AirSync:"><Collections><Collection><WindowSize>25</WindowSize></Collection></Collections></Sync>"#
            ),
            None,
            "per-collection WindowSize must not be mistaken for the global limit"
        );
        // Spec clamps: 0 and >512 are interpreted as 512 (MS-ASCMD §2.2.3.199).
        assert_eq!(
            parse_global_window_size(r#"<Sync xmlns="AirSync:"><WindowSize>0</WindowSize></Sync>"#),
            Some(512)
        );
        assert_eq!(
            parse_global_window_size(
                r#"<Sync xmlns="AirSync:"><WindowSize>600</WindowSize></Sync>"#
            ),
            Some(512)
        );
        // Empty element → treated as absent.
        assert_eq!(
            parse_global_window_size(r#"<Sync xmlns="AirSync:"><WindowSize/></Sync>"#),
            None
        );
    }

    #[test]
    fn test_effective_sync_window_spec_table() {
        // No global budget: per-collection rules only (absent → 100, 0 → 512,
        // >512 → 512, else verbatim).
        assert_eq!(effective_sync_window(None, None), 100);
        assert_eq!(effective_sync_window(Some(0), None), 512);
        assert_eq!(effective_sync_window(Some(513), None), 512);
        assert_eq!(effective_sync_window(Some(25), None), 25);
        // Global budget clamps the per-collection value.
        assert_eq!(effective_sync_window(Some(25), Some(10)), 10);
        assert_eq!(effective_sync_window(None, Some(10)), 10);
        assert_eq!(effective_sync_window(Some(0), Some(10)), 10);
        assert_eq!(effective_sync_window(Some(200), Some(300)), 200);
        // A global budget of 0 exhausts the window before any collection runs.
        assert_eq!(effective_sync_window(Some(50), Some(0)), 0);
    }

    #[test]
    fn test_count_sync_commands_counts_all_command_kinds() {
        assert_eq!(
            count_sync_commands(
                "<Commands><Add>1</Add><Change>2</Change><Delete>3</Delete></Commands><Status>1</Status>"
            ),
            3
        );
        assert_eq!(count_sync_commands("<Status>1</Status>"), 0);
    }

    #[test]
    fn test_collapse_email_ops_across_chained_batches() {
        // RFC 8621 §4.4.5 keeps created/updated/destroyed disjoint within one
        // Email/changes response, but chained hasMoreChanges batches can
        // repeat an id. One surviving op per id, first-occurrence order.
        use PendingEmailOp as P;
        let id = |s: &str| s.to_string();

        // add then delete: the message did not exist at the diff base and no
        // longer exists — nothing to tell the client.
        assert!(collapse_email_ops(vec![P::add(id("m1")), P::delete(id("m1")),]).is_empty());

        // add then change: still one add, rendered from current state.
        assert_eq!(
            collapse_email_ops(vec![P::add(id("m1")), P::change(id("m1"))]),
            vec![P::add(id("m1"))]
        );

        // repeated changes: one change.
        assert_eq!(
            collapse_email_ops(vec![P::change(id("m1")), P::change(id("m1"))]),
            vec![P::change(id("m1"))]
        );

        // change then delete: existed at the base, gone now — a delete.
        assert_eq!(
            collapse_email_ops(vec![P::change(id("m1")), P::delete(id("m1"))]),
            vec![P::delete(id("m1"))]
        );

        // delete then add cannot occur (JMAP ids are never reused), but the
        // last op must win deterministically if a backend misbehaves.
        assert_eq!(
            collapse_email_ops(vec![P::delete(id("m1")), P::add(id("m1"))]),
            vec![P::add(id("m1"))]
        );

        // Independent ids keep their first-occurrence order.
        assert_eq!(
            collapse_email_ops(vec![
                P::change(id("a")),
                P::add(id("b")),
                P::change(id("b")),
                P::add(id("c")),
                P::delete(id("c")),
                P::delete(id("d")),
            ]),
            vec![
                P::change(id("a")),
                P::add(id("b")),
                // c: first op is add, last is delete — dropped.
                P::delete(id("d")),
            ]
        );
    }

    #[test]
    fn test_route_email_sync_keys_off_cursor_presence() {
        // SyncKey "0" always primes from scratch, even with a leftover cursor.
        assert_eq!(route_email_sync("0", false), EmailSyncRoute::Initial);
        assert_eq!(route_email_sync("0", true), EmailSyncRoute::Initial);
        // A non-zero key issued by a clipped initial window must RESUME the
        // initial sync — routing it to the delta path would permanently
        // truncate the mailbox at one window.
        assert_eq!(
            route_email_sync("key-1", true),
            EmailSyncRoute::InitialContinuation
        );
        // No cursor: a non-zero key is a steady-state delta Sync.
        assert_eq!(route_email_sync("key-1", false), EmailSyncRoute::Delta);
    }

    #[test]
    fn test_count_sync_commands_ignores_response_echoes() {
        // MS-ASCMD §2.2.3.199: the window bounds changed items the server
        // returns; the <Responses> echoes of the client's own mutations
        // (§2.2.3.154) are acknowledgments and must not consume budget.
        assert_eq!(
            count_sync_commands(
                "<Collection><Status>1</Status><Responses><Add><ClientId>c1</ClientId><ServerId>s1</ServerId><Status>1</Status></Add><Change><ServerId>s2</ServerId><Status>1</Status></Change></Responses><Commands><Add>x</Add></Commands></Collection>"
            ),
            1
        );
        // Responses without any Commands section contribute nothing.
        assert_eq!(
            count_sync_commands(
                "<Collection><Status>1</Status><Responses><Delete><ServerId>s3</ServerId><Status>1</Status></Delete></Responses></Collection>"
            ),
            0
        );
    }

    #[test]
    fn test_parse_request_window_size_clamps_legacy_path() {
        // The legacy single-collection fallback path shares the spec rule:
        // 0 and >512 are interpreted as 512 (MS-ASCMD §2.2.3.199).
        let mut query = HashMap::new();
        query.insert("Cmd".to_string(), "Sync".to_string());
        let headers = HeaderMap::new();

        let req = parse_request(
            &query,
            r#"<Sync xmlns="AirSync:"><Collections><Collection><SyncKey>0</SyncKey><CollectionId>7</CollectionId><Class>Tasks</Class><WindowSize>0</WindowSize></Collection></Collections></Sync>"#,
            &headers,
        );
        assert_eq!(req.window_size, Some(512), "0 must be interpreted as 512");

        let req = parse_request(
            &query,
            r#"<Sync xmlns="AirSync:"><Collections><Collection><SyncKey>0</SyncKey><CollectionId>7</CollectionId><Class>Tasks</Class><WindowSize>600</WindowSize></Collection></Collections></Sync>"#,
            &headers,
        );
        assert_eq!(
            req.window_size,
            Some(512),
            ">512 must be interpreted as 512"
        );

        let req = parse_request(
            &query,
            r#"<Sync xmlns="AirSync:"><Collections><Collection><SyncKey>0</SyncKey><CollectionId>7</CollectionId><Class>Tasks</Class><WindowSize>25</WindowSize></Collection></Collections></Sync>"#,
            &headers,
        );
        assert_eq!(req.window_size, Some(25), "in-range values pass through");

        let req = parse_request(
            &query,
            r#"<Sync xmlns="AirSync:"><Collections><Collection><SyncKey>0</SyncKey><CollectionId>7</CollectionId><Class>Tasks</Class></Collection></Collections></Sync>"#,
            &headers,
        );
        assert_eq!(req.window_size, None, "absent WindowSize stays absent");
    }

    #[tokio::test]
    async fn test_local_content_sync_initial_windows_across_requests() {
        let state = test_sync_state().await;
        let user = "window-user@example.com";
        let device = "test-device";
        let state_coll = scoped_collection_id(crate::tasks::TASKS_COLLECTION_ID, device);
        seed_tasks(&state, user, 250).await;

        // Window 100 over 250 tasks: 100 Adds + MoreAvailable.
        let coll = tasks_collection("0", Some(100));
        let xml = handle_local_content_sync(&LocalContentSyncCtx {
            state: &state,
            username: user,
            collection_id: crate::tasks::TASKS_COLLECTION_ID,
            state_collection_id: &state_coll,
            incoming_key: "0",
            coll: &coll,
            kind: LocalContentKind::Tasks,
            global_budget: None,
        })
        .await;
        assert!(xml.contains("<Status>1</Status>"), "first window: {xml}");
        assert_eq!(xml.matches("<Add>").count(), 100, "first window: {xml}");
        assert!(xml.contains("<MoreAvailable/>"), "first window: {xml}");
        assert!(
            xml.contains("<Commands>"),
            "server changes must be wrapped in <Commands>: {xml}"
        );
        let key1 = first_sync_key(&xml);

        // Follow-up: the cursor resumes at task-100, delivering the next 100.
        let coll = tasks_collection(&key1, Some(100));
        let xml = handle_local_content_sync(&LocalContentSyncCtx {
            state: &state,
            username: user,
            collection_id: crate::tasks::TASKS_COLLECTION_ID,
            state_collection_id: &state_coll,
            incoming_key: &key1,
            coll: &coll,
            kind: LocalContentKind::Tasks,
            global_budget: None,
        })
        .await;
        assert!(xml.contains("<Status>1</Status>"), "second window: {xml}");
        assert_eq!(xml.matches("<Add>").count(), 100, "second window: {xml}");
        assert!(xml.contains("<MoreAvailable/>"), "second window: {xml}");
        assert!(
            !xml.contains("task-099<"),
            "second window must not redeliver first-window items: {xml}"
        );
        let key2 = first_sync_key(&xml);

        // Third window: the remaining 50, no MoreAvailable.
        let coll = tasks_collection(&key2, Some(100));
        let xml = handle_local_content_sync(&LocalContentSyncCtx {
            state: &state,
            username: user,
            collection_id: crate::tasks::TASKS_COLLECTION_ID,
            state_collection_id: &state_coll,
            incoming_key: &key2,
            coll: &coll,
            kind: LocalContentKind::Tasks,
            global_budget: None,
        })
        .await;
        assert!(xml.contains("<Status>1</Status>"), "third window: {xml}");
        assert_eq!(xml.matches("<Add>").count(), 50, "third window: {xml}");
        assert!(
            !xml.contains("<MoreAvailable/>"),
            "third window must close the sync: {xml}"
        );
        let key3 = first_sync_key(&xml);

        // Fourth: a steady-state delta with nothing new — empty Commands.
        let coll = tasks_collection(&key3, Some(100));
        let xml = handle_local_content_sync(&LocalContentSyncCtx {
            state: &state,
            username: user,
            collection_id: crate::tasks::TASKS_COLLECTION_ID,
            state_collection_id: &state_coll,
            incoming_key: &key3,
            coll: &coll,
            kind: LocalContentKind::Tasks,
            global_budget: None,
        })
        .await;
        assert!(xml.contains("<Status>1</Status>"), "steady state: {xml}");
        assert_eq!(xml.matches("<Add>").count(), 0, "steady state: {xml}");
        assert!(!xml.contains("<MoreAvailable/>"), "steady state: {xml}");
    }

    #[tokio::test]
    async fn test_local_content_sync_continuation_preserves_journal_changes() {
        // The exact regression under review: while a windowed initial sync
        // paginates, mutations of ALREADY-DELIVERED items land in the journal
        // above the sync's base position. Continuation windows must keep the
        // stored watermark at that base (not jump it to the current head) so
        // the delta after the cursor drains delivers the modify/delete —
        // otherwise the journal pruner erases those rows for good and the
        // client keeps a deleted task forever.
        let state = test_sync_state().await;
        let user = "continuation-journal@example.com";
        let device = "test-device";
        let state_coll = scoped_collection_id(crate::tasks::TASKS_COLLECTION_ID, device);
        seed_tasks(&state, user, 5).await;

        // Window 1 of 2: delivers task-000 and task-001, clips.
        let coll = tasks_collection("0", Some(2));
        let xml = handle_local_content_sync(&LocalContentSyncCtx {
            state: &state,
            username: user,
            collection_id: crate::tasks::TASKS_COLLECTION_ID,
            state_collection_id: &state_coll,
            incoming_key: "0",
            coll: &coll,
            kind: LocalContentKind::Tasks,
            global_budget: None,
        })
        .await;
        assert!(xml.contains("<Status>1</Status>"), "window 1: {xml}");
        assert_eq!(xml.matches("<Add>").count(), 2, "window 1: {xml}");
        assert!(xml.contains("<MoreAvailable/>"), "window 1: {xml}");
        let key1 = first_sync_key(&xml);

        // Mid-pagination mutations, all journaled above the sync's base:
        // * task-001 was DELIVERED in window 1 and is now deleted — the
        //   client must see a <Delete> eventually.
        // * task-002 is modified before its window delivers it — the client
        //   must see the new subject in window 2.
        // * task-000a sorts before the keyset cursor, so no continuation
        //   window can ever deliver it; only the journal delta can.
        state
            .storage
            .delete_task(user, "task-001")
            .await
            .expect("delete task-001");
        state
            .storage
            .upsert_task(
                user,
                "task-002",
                &crate::storage::TaskFields {
                    subject: Some("Task 2 MODIFIED MID-SYNC"),
                    importance: Some(1),
                    ..Default::default()
                },
            )
            .await
            .expect("modify task-002");
        state
            .storage
            .upsert_task(
                user,
                "task-000a",
                &crate::storage::TaskFields {
                    subject: Some("Created mid-sync"),
                    importance: Some(1),
                    ..Default::default()
                },
            )
            .await
            .expect("create task-000a");

        // Window 2: continues the keyset live set strictly after task-001.
        // task-001 is gone from the live set and task-000a sorts before the
        // cursor, so this window delivers exactly task-002 and task-003.
        let coll = tasks_collection(&key1, Some(2));
        let xml = handle_local_content_sync(&LocalContentSyncCtx {
            state: &state,
            username: user,
            collection_id: crate::tasks::TASKS_COLLECTION_ID,
            state_collection_id: &state_coll,
            incoming_key: &key1,
            coll: &coll,
            kind: LocalContentKind::Tasks,
            global_budget: None,
        })
        .await;
        assert!(xml.contains("<Status>1</Status>"), "window 2: {xml}");
        assert!(
            !xml.contains("task-001<"),
            "deleted task-001 must not be re-delivered: {xml}"
        );
        assert!(
            !xml.contains("task-000a"),
            "item below the keyset cursor must wait for the delta: {xml}"
        );
        let key2 = first_sync_key(&xml);
        assert_eq!(
            state
                .storage
                .get_local_sync_cursor(user, &state_coll)
                .await
                .expect("cursor read"),
            Some(("task-003".to_string(), 5)),
            "cursor must advance to the last delivered key and carry the base seq"
        );

        // Window 3: the last live item, closing the initial sync.
        let coll = tasks_collection(&key2, Some(2));
        let xml = handle_local_content_sync(&LocalContentSyncCtx {
            state: &state,
            username: user,
            collection_id: crate::tasks::TASKS_COLLECTION_ID,
            state_collection_id: &state_coll,
            incoming_key: &key2,
            coll: &coll,
            kind: LocalContentKind::Tasks,
            global_budget: None,
        })
        .await;
        assert!(xml.contains("<Status>1</Status>"), "window 3: {xml}");
        assert_eq!(xml.matches("<Add>").count(), 1, "window 3: {xml}");
        assert!(!xml.contains("<MoreAvailable/>"), "window 3: {xml}");
        let key3 = first_sync_key(&xml);
        assert_eq!(
            state
                .storage
                .get_local_sync_cursor(user, &state_coll)
                .await
                .expect("cursor read"),
            None,
            "the initial sync must drop its cursor when the live set drains"
        );

        // The first delta after the drain walks from base_seq. It must
        // deliver the mid-pagination delete (task-001 was already shown to
        // the client) and the creation below the keyset cursor; the
        // modified task-002 was already delivered by window 2's live-set
        // read and is re-delivered as a harmless duplicate Add.
        let coll = tasks_collection(&key3, None);
        let xml = handle_local_content_sync(&LocalContentSyncCtx {
            state: &state,
            username: user,
            collection_id: crate::tasks::TASKS_COLLECTION_ID,
            state_collection_id: &state_coll,
            incoming_key: &key3,
            coll: &coll,
            kind: LocalContentKind::Tasks,
            global_budget: None,
        })
        .await;
        assert!(xml.contains("<Status>1</Status>"), "delta: {xml}");
        assert!(
            xml.contains("<Delete><ServerId>task-001</ServerId></Delete>"),
            "the mid-pagination delete must be delta-delivered: {xml}"
        );
        assert!(
            xml.contains("MODIFIED MID-SYNC"),
            "the mid-pagination modify must be delta-delivered: {xml}"
        );
        assert!(
            xml.contains("task-000a"),
            "an item created below the keyset cursor must be delta-delivered: {xml}"
        );
        assert_eq!(
            xml.matches("<Add>").count(),
            2,
            "exactly the two live additions (modified task-002 and task-000a) may appear: {xml}"
        );
    }

    #[tokio::test]
    async fn test_local_content_sync_reprime_with_key_zero_restarts_from_beginning() {
        // A client that re-primes with SyncKey "0" (Status 3 recovery,
        // re-provisioning, account reset) must never inherit the cursor of
        // the sync it abandoned: it restarts from the first item, not from
        // the old resume position.
        let state = test_sync_state().await;
        let user = "reprime-user@example.com";
        let device = "test-device";
        let state_coll = scoped_collection_id(crate::tasks::TASKS_COLLECTION_ID, device);
        seed_tasks(&state, user, 4).await;

        // Window 1 of 2 clips and leaves a cursor at task-001.
        let coll = tasks_collection("0", Some(2));
        let xml = handle_local_content_sync(&LocalContentSyncCtx {
            state: &state,
            username: user,
            collection_id: crate::tasks::TASKS_COLLECTION_ID,
            state_collection_id: &state_coll,
            incoming_key: "0",
            coll: &coll,
            kind: LocalContentKind::Tasks,
            global_budget: None,
        })
        .await;
        assert_eq!(xml.matches("<Add>").count(), 2, "window 1: {xml}");
        assert!(
            state
                .storage
                .get_local_sync_cursor(user, &state_coll)
                .await
                .expect("cursor read")
                .is_some(),
            "clipped window must leave a cursor"
        );

        // Re-prime: the SAME key "0" must discard the cursor and start over.
        let coll = tasks_collection("0", Some(2));
        let xml = handle_local_content_sync(&LocalContentSyncCtx {
            state: &state,
            username: user,
            collection_id: crate::tasks::TASKS_COLLECTION_ID,
            state_collection_id: &state_coll,
            incoming_key: "0",
            coll: &coll,
            kind: LocalContentKind::Tasks,
            global_budget: None,
        })
        .await;
        assert!(xml.contains("task-000"), "re-prime restarts at zero: {xml}");
        assert!(
            xml.contains("task-001"),
            "re-prime must not skip already-delivered items: {xml}"
        );
        let key = first_sync_key(&xml);
        assert_eq!(
            state
                .storage
                .get_local_sync_cursor(user, &state_coll)
                .await
                .expect("cursor read"),
            Some(("task-001".to_string(), 4)),
            "the fresh initial sync owns the cursor again"
        );

        // The follow-up Sync with the re-issued key continues the new
        // initial sync (not a delta): the next two items.
        let coll = tasks_collection(&key, Some(2));
        let xml = handle_local_content_sync(&LocalContentSyncCtx {
            state: &state,
            username: user,
            collection_id: crate::tasks::TASKS_COLLECTION_ID,
            state_collection_id: &state_coll,
            incoming_key: &key,
            coll: &coll,
            kind: LocalContentKind::Tasks,
            global_budget: None,
        })
        .await;
        assert!(xml.contains("task-002"), "continuation: {xml}");
        assert!(xml.contains("task-003"), "continuation: {xml}");
        assert!(
            !xml.contains("task-000<"),
            "continuation must not re-deliver already-delivered items: {xml}"
        );
    }

    #[tokio::test]
    async fn test_local_content_sync_delta_delivers_journal_changes() {
        let state = test_sync_state().await;
        let user = "delta-window-user@example.com";
        let device = "test-device";
        let state_coll = scoped_collection_id(crate::tasks::TASKS_COLLECTION_ID, device);
        seed_tasks(&state, user, 2).await;

        let coll = tasks_collection("0", None);
        let xml = handle_local_content_sync(&LocalContentSyncCtx {
            state: &state,
            username: user,
            collection_id: crate::tasks::TASKS_COLLECTION_ID,
            state_collection_id: &state_coll,
            incoming_key: "0",
            coll: &coll,
            kind: LocalContentKind::Tasks,
            global_budget: None,
        })
        .await;
        assert_eq!(xml.matches("<Add>").count(), 2, "initial: {xml}");
        assert!(!xml.contains("<MoreAvailable/>"), "initial: {xml}");
        let key = first_sync_key(&xml);

        // A new task lands (journal 'upsert') and one is removed ('delete').
        state
            .storage
            .upsert_task(
                user,
                "task-new",
                &crate::storage::TaskFields {
                    subject: Some("New task"),
                    importance: Some(1),
                    sensitivity: Some(0),
                    start_date: None,
                    due_date: None,
                    utc_start_date: None,
                    utc_due_date: None,
                    complete: 0,
                    date_completed: None,
                    reminder_set: 0,
                    reminder_time: None,
                    categories: None,
                    body: None,
                },
            )
            .await
            .expect("journal upsert");
        state
            .storage
            .delete_task(user, "task-001")
            .await
            .expect("journal delete");

        let coll = tasks_collection(&key, None);
        let xml = handle_local_content_sync(&LocalContentSyncCtx {
            state: &state,
            username: user,
            collection_id: crate::tasks::TASKS_COLLECTION_ID,
            state_collection_id: &state_coll,
            incoming_key: &key,
            coll: &coll,
            kind: LocalContentKind::Tasks,
            global_budget: None,
        })
        .await;
        assert!(xml.contains("<Status>1</Status>"), "delta: {xml}");
        assert_eq!(xml.matches("<Add>").count(), 1, "delta adds: {xml}");
        assert_eq!(xml.matches("<Delete>").count(), 1, "delta deletes: {xml}");
        assert!(
            xml.contains("task-new"),
            "delta must deliver the new task: {xml}"
        );
        assert!(
            !xml.contains("task-000"),
            "delta must not redeliver unchanged items: {xml}"
        );
        assert!(!xml.contains("<MoreAvailable/>"), "delta: {xml}");
    }

    #[tokio::test]
    async fn test_local_content_sync_invalid_key_returns_status_3() {
        let state = test_sync_state().await;
        let user = "stale-key-user@example.com";
        let device = "test-device";
        let state_coll = scoped_collection_id(crate::tasks::TASKS_COLLECTION_ID, device);
        seed_tasks(&state, user, 1).await;

        let coll = tasks_collection("0", None);
        let xml = handle_local_content_sync(&LocalContentSyncCtx {
            state: &state,
            username: user,
            collection_id: crate::tasks::TASKS_COLLECTION_ID,
            state_collection_id: &state_coll,
            incoming_key: "0",
            coll: &coll,
            kind: LocalContentKind::Tasks,
            global_budget: None,
        })
        .await;
        assert_eq!(xml.matches("<Add>").count(), 1, "initial: {xml}");

        // A key that was never issued must force re-priming (Status 3).
        let coll = tasks_collection("bogus-key", None);
        let xml = handle_local_content_sync(&LocalContentSyncCtx {
            state: &state,
            username: user,
            collection_id: crate::tasks::TASKS_COLLECTION_ID,
            state_collection_id: &state_coll,
            incoming_key: "bogus-key",
            coll: &coll,
            kind: LocalContentKind::Tasks,
            global_budget: None,
        })
        .await;
        assert!(
            xml.contains("<Status>3</Status>"),
            "stale key must answer Status 3: {xml}"
        );
        assert!(xml.contains("<SyncKey>0</SyncKey>"), "Status 3: {xml}");
    }

    #[tokio::test]
    async fn test_local_content_sync_get_changes_zero_rotates_key_only() {
        let state = test_sync_state().await;
        let user = "getchanges-user@example.com";
        let device = "test-device";
        let state_coll = scoped_collection_id(crate::tasks::TASKS_COLLECTION_ID, device);
        seed_tasks(&state, user, 3).await;

        let coll = tasks_collection("0", None);
        let xml = handle_local_content_sync(&LocalContentSyncCtx {
            state: &state,
            username: user,
            collection_id: crate::tasks::TASKS_COLLECTION_ID,
            state_collection_id: &state_coll,
            incoming_key: "0",
            coll: &coll,
            kind: LocalContentKind::Tasks,
            global_budget: None,
        })
        .await;
        let key = first_sync_key(&xml);

        // GetChanges=0: the key must rotate but nothing may be resent, even
        // though the journal is non-empty at this point.
        let mut coll = tasks_collection(&key, None);
        coll.get_changes = false;
        let xml = handle_local_content_sync(&LocalContentSyncCtx {
            state: &state,
            username: user,
            collection_id: crate::tasks::TASKS_COLLECTION_ID,
            state_collection_id: &state_coll,
            incoming_key: &key,
            coll: &coll,
            kind: LocalContentKind::Tasks,
            global_budget: None,
        })
        .await;
        assert!(xml.contains("<Status>1</Status>"), "getchanges=0: {xml}");
        assert_eq!(xml.matches("<Add>").count(), 0, "getchanges=0: {xml}");
        let key2 = first_sync_key(&xml);
        assert_ne!(key, key2, "key must rotate even with no changes sent");

        // GetChanges=0 must not consume the journal: a change that already
        // landed before the acknowledge-only request still has to reach the
        // client on the next GetChanges=1 Sync (MS-ASCMD §2.2.3.72), and the
        // journal pruner must never floor above it. An acknowledge-only
        // rotation that stored the current head would drop (and then prune)
        // this row permanently.
        state
            .storage
            .upsert_task(
                user,
                "task-001",
                &crate::storage::TaskFields {
                    subject: Some("Changed before ack"),
                    importance: Some(1),
                    ..Default::default()
                },
            )
            .await
            .expect("change lands before the GetChanges=0 request");
        let mut coll = tasks_collection(&key2, None);
        coll.get_changes = false;
        let xml = handle_local_content_sync(&LocalContentSyncCtx {
            state: &state,
            username: user,
            collection_id: crate::tasks::TASKS_COLLECTION_ID,
            state_collection_id: &state_coll,
            incoming_key: &key2,
            coll: &coll,
            kind: LocalContentKind::Tasks,
            global_budget: None,
        })
        .await;
        assert_eq!(
            xml.matches("<Add>").count(),
            0,
            "second getchanges=0: {xml}"
        );
        let key3 = first_sync_key(&xml);

        // The next real Sync must still deliver the pre-ack change.
        let coll = tasks_collection(&key3, None);
        let xml = handle_local_content_sync(&LocalContentSyncCtx {
            state: &state,
            username: user,
            collection_id: crate::tasks::TASKS_COLLECTION_ID,
            state_collection_id: &state_coll,
            incoming_key: &key3,
            coll: &coll,
            kind: LocalContentKind::Tasks,
            global_budget: None,
        })
        .await;
        assert!(
            xml.contains("Changed before ack"),
            "GetChanges=0 must not consume journal changes: {xml}"
        );
    }

    #[tokio::test]
    async fn test_sync_collections_global_window_budget_stops_processing() {
        let state = test_sync_state().await;
        let user = "global-budget-user@example.com";
        let device = "budget-device";
        seed_tasks(&state, user, 7).await;

        let ctx = SyncCtx {
            state: &state,
            username: user,
            password: &SecretString::from("pw"),
            wbxml: &Wbxml::new(),
            as_wbxml: false,
            request_id: "test-req",
            device_id: device,
            global_window_size: Some(5),
        };

        // Two Tasks collections in one request with a global WindowSize of 5:
        // the first collection consumes the entire budget (5 of 7 tasks) and
        // the second must not be processed at all (MS-ASCMD §2.2.3.199).
        let collections = vec![
            tasks_collection("0", Some(10)),
            tasks_collection("0", Some(10)),
        ];
        let resp = handle_sync_collections(&ctx, &collections).await;
        let text = response_text(resp).await;
        assert!(
            text.contains("<Status>1</Status>"),
            "global-budget response: {text}"
        );
        assert_eq!(
            text.matches("<Add>").count(),
            5,
            "global WindowSize must cap the whole response: {text}"
        );
        assert!(text.contains("<MoreAvailable/>"), "global budget: {text}");
        assert_eq!(
            text.matches("<CollectionId>").count(),
            1,
            "budget-exhausted collections must be skipped entirely: {text}"
        );
    }

    /// The initial Provision response (PolicyKey 0 path) must be WBXML-
    /// encodable. The template mixes `Settings:`-prefixed elements with the
    /// Provision default namespace, so the `Settings` prefix must be declared
    /// in the exact case the elements use ([MS-ASPROV] §2.2.2.53;
    /// `xmlns:settings` would leave `Settings:DeviceInformation` undeclared
    /// and resolving against the Provision code page, where DeviceInformation
    /// has no token). The embedded EASProvisionDoc likewise declares the
    /// exact-case Provision namespace URI so its unqualified children map to
    /// code page 14 directly.
    #[test]
    fn test_initial_provision_response_is_wbxml_encodable() {
        let doc = eas_provision_doc_xml();
        let response = format!(
            r#"<?xml version="1.0" encoding="utf-8"?>
<Provision xmlns="Provision:" xmlns:Settings="Settings:">
  <Settings:DeviceInformation>
    <Settings:Status>1</Settings:Status>
  </Settings:DeviceInformation>
  <Status>1</Status>
  <Policies>
    <Policy>
      <PolicyType>MS-EAS-Provisioning-WBXML</PolicyType>
      <Status>1</Status>
      <PolicyKey>0</PolicyKey>
      <Data>
        {doc}
      </Data>
    </Policy>
  </Policies>
</Provision>"#,
        );
        Wbxml::new()
            .encode(&response)
            .expect("initial Provision response must encode without unknown tags");
    }

    /// [MS-ASCMD] §2.2.3.125.4: an ItemOperations Fetch without its own
    /// `<Options>` reuses the sticky block the previous Sync established
    /// for that collection. Sync persists under the device-scoped key
    /// `"{collection_id}::{device_id}"`, so the ItemOperations lookup must
    /// resolve the same scoped key — the plain CollectionId would never
    /// match the stored row and every Fetch would silently fall back to
    /// default options.
    #[tokio::test]
    async fn test_item_operations_sticky_options_use_sync_scoped_key() {
        use crate::eas_sync_options::{EasBodyPreference, EasSyncCollectionOptions};

        let state = test_sync_state().await;
        let user = "sticky-io@example.com";
        let device = "sticky-device";
        let collection_id = "2";

        let mut opts = EasSyncCollectionOptions {
            body_preferences: vec![EasBodyPreference {
                body_type: 1,
                truncation_size: Some(512),
                all_or_none: Some(true),
                preview: Some(64),
            }],
            explicitly_set: true,
            ..Default::default()
        };

        // What handle_sync_collections does: persist under the scoped key.
        let state_collection_id = scoped_collection_id(collection_id, device);
        state
            .storage
            .set_sync_collection_options(user, &state_collection_id, &opts)
            .await
            .expect("seed sticky options");

        // A Fetch with no Options block of its own must resolve to the
        // sticky block (same truncation, AllOrNone, and preview).
        let resolved =
            item_operations_sticky_options(&state, user, collection_id, device, None).await;
        assert_eq!(
            resolved.body_preferences, opts.body_preferences,
            "sticky block must be found via the device-scoped key"
        );
        assert!(resolved.body_preferences[0].all_or_none.is_some());

        // A Fetch that carries its own explicit Options overrides the
        // sticky block ([MS-ASCMD] §2.2.3.125.4).
        opts.body_preferences[0].truncation_size = Some(99);
        opts.body_preferences[0].all_or_none = None;
        let resolved =
            item_operations_sticky_options(&state, user, collection_id, device, Some(opts)).await;
        assert_eq!(resolved.body_preferences[0].truncation_size, Some(99));

        // A different device never sees another device's sticky block.
        let resolved =
            item_operations_sticky_options(&state, user, collection_id, "other-device", None).await;
        assert!(
            resolved.body_preferences.is_empty(),
            "sticky options are device-scoped: {:?}",
            resolved.body_preferences
        );
    }

    /// [MS-ASCMD] §2.2.3.143.2: the Range value is "m-n", zero-indexed,
    /// m ≤ n. Everything else is malformed and maps to status 8.
    #[test]
    fn parse_item_operations_byte_range_matrix() {
        assert_eq!(parse_item_operations_byte_range("0-9"), Some(0..10));
        assert_eq!(parse_item_operations_byte_range("5-5"), Some(5..6));
        assert_eq!(parse_item_operations_byte_range("3 - 9"), Some(3..10));
        assert_eq!(
            parse_item_operations_byte_range("0-18446744073709551614"),
            Some(0..18446744073709551615)
        );
        for bad in [
            "",
            "9-0",
            "abc",
            "5",
            "-5",
            "5-",
            "a-9",
            "9-a",
            "1-2-3",
            "0-18446744073709551615",
        ] {
            assert!(
                parse_item_operations_byte_range(bad).is_none(),
                "Range {bad:?} must be rejected as malformed"
            );
        }
    }

    /// A Fetch's `<Options><Range>` travels on the ItemOperations code page
    /// (the WBXML root namespace), so the streaming parser must capture it
    /// from both the plain-XML and the WBXML-decoded forms.
    #[test]
    fn parse_item_operations_fetches_extracts_range() {
        let xml = concat!(
            r#"<?xml version="1.0" encoding="utf-8"?>"#,
            r#"<ItemOperations xmlns="ItemOperations:" xmlns:AirSyncBase="AirSyncBase:">"#,
            r#"<Fetch><Store>Mailbox</Store>"#,
            r#"<AirSyncBase:FileReference>blob-123</AirSyncBase:FileReference>"#,
            r#"<Options><Range>0-1023</Range></Options>"#,
            r#"</Fetch>"#,
            r#"</ItemOperations>"#,
        );
        let fetches = parse_item_operations_fetches(xml);
        assert_eq!(fetches.len(), 1);
        assert_eq!(fetches[0].store, "Mailbox");
        assert_eq!(
            fetches[0].file_reference.as_deref(),
            Some("blob-123"),
            "AirSyncBase-prefixed FileReference is still captured by local name"
        );
        assert_eq!(fetches[0].byte_range.as_deref(), Some("0-1023"));

        // WBXML round-trip: what a client sends on the wire decodes to the
        // same parse (root page 20 keeps Range unqualified).
        let wbxml = Wbxml::new();
        let encoded = wbxml.encode(xml).expect("request must encode");
        let decoded = wbxml.decode(&encoded).expect("request must decode");
        let fetches_rt = parse_item_operations_fetches(&decoded);
        assert_eq!(fetches_rt.len(), 1);
        assert_eq!(fetches_rt[0].byte_range.as_deref(), Some("0-1023"));
        assert_eq!(fetches_rt[0].file_reference.as_deref(), Some("blob-123"));
    }

    /// Chunked base64 must byte-match a single-shot encode for every size,
    /// including non-multiples of the chunk and of 3.
    #[test]
    fn push_base64_matches_single_shot_encode() {
        for len in [0usize, 1, 2, 3, 3071, 3072, 3073, 6144, 9215, 9216, 12_347] {
            let bytes: Vec<u8> = (0..len).map(|i| (i % 251) as u8).collect();
            let mut chunked = String::new();
            push_base64(&mut chunked, &bytes);
            assert_eq!(
                chunked,
                BASE64.encode(&bytes),
                "chunked encode must equal single-shot at len {len}"
            );
        }
    }

    /// The attachment `Properties` shape per [MS-ASCMD] §2.2.3.139.2 and the
    /// §4.10.4.4 example: ContentType + Total (+ Range when requested) with
    /// inline base64 `Data`, or `<Part>` for multipart delivery.
    #[test]
    fn render_attachment_properties_shapes() {
        let outcome = AttachmentFetchOutcome {
            data: b"hello".to_vec(),
            content_type: "application/pdf".to_string(),
            total_size: Some(5),
            window: (0, 5),
            complete: true,
        };
        let inline = render_attachment_properties(&outcome, false, None);
        assert_eq!(
            inline,
            "<Properties>\
             <AirSyncBase:ContentType>application/pdf</AirSyncBase:ContentType>\
             <Total>5</Total>\
             <Data>aGVsbG8=</Data>\
             </Properties>"
        );
        assert!(
            !inline.contains("<Range>"),
            "no Range in the request → none echoed"
        );

        // Ranged fetch: authoritative window echoes as inclusive "m-n".
        let ranged = render_attachment_properties(&outcome, true, None);
        assert!(ranged.contains("<Range>0-4</Range>"), "got: {ranged}");

        // Multipart: no inline Data, only the part index
        // ([MS-ASCMD] §2.2.3.130).
        let part = render_attachment_properties(&outcome, false, Some(1));
        assert!(part.contains("<Part>1</Part>"));
        assert!(!part.contains("<Data>"));

        // Unknown total ([MS-ASCMD] §2.2.3.184.2, 0...1): Total omitted.
        let unknown_total = AttachmentFetchOutcome {
            total_size: None,
            ..outcome
        };
        let no_total = render_attachment_properties(&unknown_total, false, None);
        assert!(!no_total.contains("<Total>"), "got: {no_total}");
    }

    /// The MultiPartResponse binary layout per [MS-ASCMD] §2.2.1.10.1.1:
    /// PartsCount + per-part Offset/Length metadata (little-endian u32,
    /// offsets from the first byte of the structure), then the parts —
    /// WBXML first, raw content after.
    #[tokio::test]
    async fn multipart_item_operations_response_binary_layout() {
        let xml = concat!(
            r#"<?xml version="1.0" encoding="utf-8"?>"#,
            r#"<ItemOperations xmlns="ItemOperations:" xmlns:AirSyncBase="AirSyncBase:">"#,
            r#"<Status>1</Status><Response><Fetch><Status>1</Status>"#,
            r#"<Properties><Part>1</Part></Properties></Fetch></Response>"#,
            r#"</ItemOperations>"#,
        );
        let content = b"raw attachment bytes".to_vec();
        let wbxml = Wbxml::new();
        let response = multipart_item_operations_response(
            &wbxml,
            xml,
            vec![content.clone()],
            "req-multipart-test",
        );
        assert_eq!(response.status(), 200);
        assert_eq!(
            response
                .headers()
                .get("content-type")
                .and_then(|v| v.to_str().ok()),
            Some("application/vnd.ms-sync.multipart")
        );
        let body = axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .expect("body must be collectable");
        assert!(body.len() > 4 + 8 * 2, "body must hold header + parts");

        let parts_count = u32::from_le_bytes([body[0], body[1], body[2], body[3]]);
        assert_eq!(parts_count, 2, "WBXML part + one content part");
        let meta0_off = u32::from_le_bytes(body[4..8].try_into().unwrap()) as usize;
        let meta0_len = u32::from_le_bytes(body[8..12].try_into().unwrap()) as usize;
        let meta1_off = u32::from_le_bytes(body[12..16].try_into().unwrap()) as usize;
        let meta1_len = u32::from_le_bytes(body[16..20].try_into().unwrap()) as usize;
        assert_eq!(meta0_off, 20, "first part starts after all metadata");
        assert_eq!(meta1_off, meta0_off + meta0_len, "parts are contiguous");
        assert_eq!(meta1_len, content.len());

        // Part 0 is the WBXML and decodes back to the same document.
        let wbxml_part = &body[meta0_off..meta0_off + meta0_len];
        let decoded = wbxml.decode(wbxml_part).expect("part 0 must be WBXML");
        assert!(decoded.contains("<Part>1</Part>"), "got: {decoded}");
        // Part 1 carries the raw, unencoded content bytes.
        assert_eq!(&body[meta1_off..meta1_off + meta1_len], content.as_slice());
    }

    /// [MS-ASCMD] §2.2.3.130: in a multipart response the Part element
    /// replaces the Body's Data element; the binary part carries the
    /// *unescaped* body characters.
    #[test]
    fn extract_body_for_multipart_moves_data_to_part() {
        let properties = concat!(
            "<AirSyncBase:Body><AirSyncBase:Type>2</AirSyncBase:Type>",
            "<AirSyncBase:EstimatedDataSize>17</AirSyncBase:EstimatedDataSize>",
            "<AirSyncBase:Data><p>Hi &amp; bye</p></AirSyncBase:Data>",
            "</AirSyncBase:Body>",
        );
        let (xml, raw) = extract_body_for_multipart(properties, 1)
            .expect("inline body Data must be extractable");
        assert_eq!(raw, "<p>Hi & bye</p>", "XML character data is unescaped");
        // [MS-ASCMD] §4.10.5.2: the Part element replaces the Body's Data
        // element, sitting directly under airsyncbase:Body.
        assert!(
            xml.contains(
                "<AirSyncBase:EstimatedDataSize>17</AirSyncBase:EstimatedDataSize><Part>1</Part></AirSyncBase:Body>"
            ),
            "got: {xml}"
        );
        assert!(
            !xml.contains("AirSyncBase:Data") && !xml.contains("Hi"),
            "the body characters must not remain inline: {xml}"
        );

        // No inline body Data → properties pass through untouched.
        assert_eq!(
            extract_body_for_multipart(
                "<AirSyncBase:Body><AirSyncBase:Type>2</AirSyncBase:Type></AirSyncBase:Body>",
                1
            ),
            None
        );
    }

    /// `MS-ASAcceptMultiPart` parsing per [MS-ASHTTP] §2.2.1.1.2.5: only the
    /// literal "T" (case-insensitive, whitespace-tolerant) requests
    /// multipart; absent/F values mean inline.
    #[test]
    fn multipart_header_parsing() {
        let mut headers = HeaderMap::new();
        assert!(!multipart_requested(&headers), "absent header → inline");
        for value in ["F", "f", "false", "0", ""] {
            headers.insert(
                "MS-ASAcceptMultiPart",
                HeaderValue::from_str(value).unwrap(),
            );
            assert!(
                !multipart_requested(&headers),
                "{value:?} must not request multipart"
            );
        }
        for value in ["T", "t", " T "] {
            headers.insert(
                "MS-ASAcceptMultiPart",
                HeaderValue::from_str(value).unwrap(),
            );
            assert!(
                multipart_requested(&headers),
                "{value:?} must request multipart"
            );
        }
    }

    fn calendar_attachment(
        content_base64: &str,
        content_type: &str,
    ) -> crate::attachment::FileAttachment {
        crate::attachment::FileAttachment {
            id: "calatt-1".to_string(),
            parent_item_server_id: "EWS-evt-1".to_string(),
            owner: "owner@example.com".to_string(),
            name: "notes.txt".to_string(),
            content_type: content_type.to_string(),
            content_size: 0,
            content_base64: content_base64.to_string(),
            is_inline: false,
            content_id: None,
            content_location: None,
            last_modified_time: None,
        }
    }

    /// Window slicing over gateway-managed (calendar) attachments,
    /// including the clamped-window (status 17), past-EOF (8),
    /// oversized-window (8) and whole-item-over-budget (11) paths.
    #[test]
    fn calendar_attachment_outcome_window_matrix() {
        let payload = (0..1000u32).map(|i| (i % 256) as u8).collect::<Vec<u8>>();
        let b64 = BASE64.encode(&payload);
        let att = calendar_attachment(&b64, "application/octet-stream");

        // Whole item.
        let whole = calendar_attachment_outcome(&att, None, 4096).expect("whole item");
        assert_eq!(whole.data, payload);
        assert_eq!(whole.window, (0, 1000));
        assert_eq!(whole.total_size, Some(1000));
        assert!(whole.complete);

        // Window fully inside the content.
        let win = calendar_attachment_outcome(&att, Some(100..300), 4096).expect("window");
        assert_eq!(win.data, payload[100..300].to_vec());
        assert_eq!(win.window, (100, 300));
        assert!(win.complete);

        // Window clamped at EOF → status 17 payload (complete=false). The
        // declared span stays within the byte budget so the clamp, not the
        // budget check, is what shortens it.
        let clamped =
            calendar_attachment_outcome(&att, Some(900..2000), 4096).expect("clamped window");
        assert_eq!(clamped.data, payload[900..].to_vec());
        assert_eq!(clamped.window, (900, 1000));
        assert!(!clamped.complete, "short window signals status 17");

        // Errors.
        assert_eq!(
            calendar_attachment_outcome(&att, Some(1000..1500), 4096).unwrap_err(),
            8,
            "range starting at EOF → status 8"
        );
        assert_eq!(
            calendar_attachment_outcome(&att, Some(0..5000), 4096).unwrap_err(),
            8,
            "window over the budget → status 8"
        );
        assert_eq!(
            calendar_attachment_outcome(&att, Some(900..5000), 4096).unwrap_err(),
            8,
            "declared span past the budget → status 8"
        );
        assert_eq!(
            calendar_attachment_outcome(&att, None, 999).unwrap_err(),
            11,
            "whole item over the budget → status 11"
        );
        let corrupt = calendar_attachment("not!!valid!!base64!!", "application/octet-stream");
        assert_eq!(
            calendar_attachment_outcome(&corrupt, None, 4096).unwrap_err(),
            15,
            "undecodable stored content → status 15"
        );

        // Empty content, no range → the file is empty; a range over it → 8.
        let empty = calendar_attachment("", "text/plain");
        assert_eq!(
            calendar_attachment_outcome(&empty, None, 4096).unwrap_err(),
            10
        );
        assert_eq!(
            calendar_attachment_outcome(&empty, Some(0..1), 4096).unwrap_err(),
            8
        );
    }

    /// End-to-end ItemOperations attachment Fetch against the gateway's own
    /// calendar-attachment store: [MS-ASCMD] §4.10.4.4-shaped response with
    /// Content-Type/Total/Data, the ranged variant with Range + status
    /// 17 when the window runs past EOF, and the multipart variant where
    /// the bytes travel as a raw binary part referenced by `<Part>`.
    #[tokio::test]
    async fn handle_item_operations_calendar_attachment_fetch() {
        let state = test_sync_state().await;
        let owner = "calowner@example.com";
        let payload = (0..300u32).map(|i| (i % 256) as u8).collect::<Vec<u8>>();
        state
            .storage
            .upsert_item_map(
                owner,
                "cal-href",
                "res-href",
                "EWS-evt-1",
                "uid-1",
                "etag-1",
            )
            .await
            .expect("seed parent item");
        let created = state
            .attachment_manager
            .create_file_attachment(&crate::attachment::CreateAttachmentParams {
                owner,
                parent_item_server_id: "EWS-evt-1",
                name: "data.bin",
                content_type: "application/octet-stream",
                content_base64: &BASE64.encode(&payload),
                is_inline: false,
                content_id: None,
                content_location: None,
            })
            .await
            .expect("seed attachment");
        let attachment_id = created.id.clone();

        let wbxml = Wbxml::new();
        let request = format!(
            r#"<?xml version="1.0" encoding="utf-8"?><ItemOperations xmlns="ItemOperations:" xmlns:AirSyncBase="AirSyncBase:"><Fetch><Store>Mailbox</Store><AirSyncBase:FileReference>{attachment_id}</AirSyncBase:FileReference></Fetch></ItemOperations>"#
        );
        let response = handle_item_operations(
            &state,
            owner,
            SecretString::from("pw"),
            &request,
            &wbxml,
            false,
            "req-cal-att",
            "test-device",
            false,
        )
        .await;
        let body = axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .expect("collect response");
        let xml = String::from_utf8(body.to_vec()).unwrap();
        assert!(xml.contains("<Status>1</Status>"), "got: {xml}");
        assert!(
            xml.contains(
                "<AirSyncBase:ContentType>application/octet-stream</AirSyncBase:ContentType>"
            ),
            "got: {xml}"
        );
        assert!(xml.contains("<Total>300</Total>"), "got: {xml}");
        assert!(
            xml.contains(&format!("<Data>{}</Data>", BASE64.encode(&payload))),
            "got: {xml}"
        );
        assert!(
            !xml.contains("<Range>"),
            "unranged fetch echoes no Range: {xml}"
        );

        // Ranged fetch past EOF: the authoritative short window and
        // status 17 ("Partial success").
        let request = format!(
            r#"<?xml version="1.0" encoding="utf-8"?><ItemOperations xmlns="ItemOperations:" xmlns:AirSyncBase="AirSyncBase:"><Fetch><Store>Mailbox</Store><AirSyncBase:FileReference>{attachment_id}</AirSyncBase:FileReference><Options><Range>200-400</Range></Options></Fetch></ItemOperations>"#
        );
        let response = handle_item_operations(
            &state,
            owner,
            SecretString::from("pw"),
            &request,
            &wbxml,
            false,
            "req-cal-att-range",
            "test-device",
            false,
        )
        .await;
        let body = axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .expect("collect response");
        let xml = String::from_utf8(body.to_vec()).unwrap();
        assert!(xml.contains("<Status>17</Status>"), "got: {xml}");
        assert!(xml.contains("<Range>200-299</Range>"), "got: {xml}");
        assert!(xml.contains("<Total>300</Total>"), "got: {xml}");
        assert!(
            xml.contains(&format!("<Data>{}</Data>", BASE64.encode(&payload[200..]))),
            "got: {xml}"
        );

        // The WBXML-decoded form of a ranged request parses identically
        // (Range lives on the root ItemOperations code page).
        let encoded = wbxml.encode(&request).expect("encode request");
        let decoded = wbxml.decode(&encoded).expect("decode request");
        assert!(decoded.contains("<Range>200-400</Range>"), "got: {decoded}");

        // Multipart ([MS-ASHTTP] §2.2.1.1.2.5): the WBXML part references
        // the raw bytes via <Part>1</Part>, and part 1 carries the bytes.
        let request = format!(
            r#"<?xml version="1.0" encoding="utf-8"?><ItemOperations xmlns="ItemOperations:" xmlns:AirSyncBase="AirSyncBase:"><Fetch><Store>Mailbox</Store><AirSyncBase:FileReference>{attachment_id}</AirSyncBase:FileReference></Fetch></ItemOperations>"#
        );
        let response = handle_item_operations(
            &state,
            owner,
            SecretString::from("pw"),
            &request,
            &wbxml,
            true,
            "req-cal-att-mp",
            "test-device",
            true,
        )
        .await;
        assert_eq!(
            response
                .headers()
                .get("content-type")
                .and_then(|v| v.to_str().ok()),
            Some("application/vnd.ms-sync.multipart")
        );
        let body = axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .expect("collect response");
        let parts_count = u32::from_le_bytes([body[0], body[1], body[2], body[3]]);
        assert_eq!(parts_count, 2, "WBXML part + one content part");
        let meta1_off = u32::from_le_bytes(body[12..16].try_into().unwrap()) as usize;
        let meta1_len = u32::from_le_bytes(body[16..20].try_into().unwrap()) as usize;
        assert_eq!(meta1_len, payload.len());
        assert_eq!(
            &body[meta1_off..meta1_off + meta1_len],
            payload.as_slice(),
            "part 1 carries the raw attachment bytes"
        );
        let meta0_off = u32::from_le_bytes(body[4..8].try_into().unwrap()) as usize;
        let meta0_len = u32::from_le_bytes(body[8..12].try_into().unwrap()) as usize;
        let decoded = wbxml
            .decode(&body[meta0_off..meta0_off + meta0_len])
            .expect("part 0 is the WBXML response");
        assert!(decoded.contains("<Part>1</Part>"), "got: {decoded}");
        assert!(
            !decoded.contains(&BASE64.encode(&payload)),
            "the payload must not travel inline in multipart mode: {decoded}"
        );
        assert!(decoded.contains("<Total>300</Total>"), "got: {decoded}");
    }

    /// Seed a gateway-managed file attachment and return its id.
    async fn seed_file_attachment(
        state: &AppState,
        owner: &str,
        parent_item_server_id: &str,
        name: &str,
        len: usize,
    ) -> String {
        let payload = vec![0x42u8; len];
        let b64 = BASE64.encode(&payload);
        let created = state
            .attachment_manager
            .create_file_attachment(&crate::attachment::CreateAttachmentParams {
                owner,
                parent_item_server_id,
                name,
                content_type: "application/octet-stream",
                content_base64: &b64,
                is_inline: false,
                content_id: None,
                content_location: None,
            })
            .await
            .expect("seed attachment");
        created.id
    }

    /// The configured attachment budget caps not only each single Fetch but
    /// the cumulative content one ItemOperations response carries
    /// ([MS-ASCMD] §2.2.3.67 allows "multiple fetch operations … within one
    /// ItemOperations request", and the request-body limit bounds only the
    /// Fetch addresses, not the data each materializes). A Fetch that would
    /// overflow the remainder is answered with [MS-ASCMD] §2.2.3.177.8
    /// status 11 ("The requested data size is too large", Item scope), the
    /// Fetches still execute in request order, and a single-Fetch request
    /// keeps succeeding — the budget is per request, and one Fetch can never
    /// trip it on its own.
    #[tokio::test]
    async fn handle_item_operations_attachment_cumulative_budget() {
        let storage = crate::storage::Storage::new("sqlite::memory:")
            .await
            .expect("in-memory storage");
        storage.init_schema().await.expect("schema init");
        let cfg = crate::config::Config {
            jmap_base: "http://127.0.0.1:1".to_string(),
            email_enabled: false,
            mail_domain: "example.com".to_string(),
            hmac_secret: SecretString::from("a".repeat(32)),
            max_attachment_bytes: 1024,
            ..Default::default()
        };
        let state = Arc::new(AppState::new(cfg, Arc::new(storage)));
        let owner = "budget@example.com";
        state
            .storage
            .upsert_item_map(
                owner,
                "cal-href",
                "res-href",
                "EWS-evt-1",
                "uid-1",
                "etag-1",
            )
            .await
            .expect("seed parent item");
        let id_a = seed_file_attachment(&state, owner, "EWS-evt-1", "a.bin", 700).await;
        let id_b = seed_file_attachment(&state, owner, "EWS-evt-1", "b.bin", 700).await;

        async fn run(
            state: &Arc<AppState>,
            owner: &str,
            fetch_bodies: &[String],
            multipart: bool,
            as_wbxml: bool,
        ) -> axum::body::Bytes {
            let request = format!(
                r#"<?xml version="1.0" encoding="utf-8"?><ItemOperations xmlns="ItemOperations:" xmlns:AirSyncBase="AirSyncBase:">{}</ItemOperations>"#,
                fetch_bodies.join("")
            );
            let response = handle_item_operations(
                state,
                owner,
                SecretString::from("pw"),
                &request,
                &Wbxml::new(),
                as_wbxml,
                "req-budget",
                "test-device",
                multipart,
            )
            .await;
            axum::body::to_bytes(response.into_body(), 1024 * 1024)
                .await
                .expect("collect response")
        }
        let fetch_xml = |id: &str| {
            format!(
                r#"<Fetch><Store>Mailbox</Store><AirSyncBase:FileReference>{id}</AirSyncBase:FileReference></Fetch>"#
            )
        };

        // Three Fetches whose combined content (2100 bytes) exceeds the
        // 1024-byte budget: the first is served, the rest are answered with
        // status 11 — each identified by its own FileReference, in request
        // order, without aborting the command.
        let body = run(
            &state,
            owner,
            &[fetch_xml(&id_a), fetch_xml(&id_a), fetch_xml(&id_b)],
            false,
            false,
        )
        .await;
        let xml = String::from_utf8(body.to_vec()).unwrap();
        let a_block = format!(
            "<Fetch><Status>1</Status><AirSyncBase:FileReference>{id_a}</AirSyncBase:FileReference><AirSync:Class>Calendar</AirSync:Class><Properties><AirSyncBase:ContentType>application/octet-stream</AirSyncBase:ContentType><Total>700</Total><Data>{}</Data></Properties></Fetch>",
            BASE64.encode([0x42u8; 700])
        );
        assert!(xml.contains(&a_block), "first Fetch is served: {xml}");
        assert!(
            xml.contains(&format!(
                "<Fetch><Status>11</Status><AirSyncBase:FileReference>{id_a}</AirSyncBase:FileReference></Fetch>"
            )),
            "second Fetch overflows the remainder: {xml}"
        );
        assert!(
            xml.contains(&format!(
                "<Fetch><Status>11</Status><AirSyncBase:FileReference>{id_b}</AirSyncBase:FileReference></Fetch>"
            )),
            "a different attachment counts against the same budget: {xml}"
        );

        // A single-Fetch request of the very same attachment still succeeds:
        // the budget is per request and one Fetch alone never trips it.
        let body = run(&state, owner, &[fetch_xml(&id_b)], false, false).await;
        let xml = String::from_utf8(body.to_vec()).unwrap();
        assert!(
            xml.contains(&format!(
                "<Fetch><Status>1</Status><AirSyncBase:FileReference>{id_b}</AirSyncBase:FileReference>"
            )),
            "single Fetch is not budget-limited: {xml}"
        );

        // Multipart: the overflowing Fetch pushes no binary part, so the
        // parts metadata stays consistent with the <Part> references the
        // WBXML actually emits.
        let body = run(
            &state,
            owner,
            &[fetch_xml(&id_a), fetch_xml(&id_a)],
            true,
            true,
        )
        .await;
        let parts_count = u32::from_le_bytes([body[0], body[1], body[2], body[3]]);
        assert_eq!(parts_count, 2, "WBXML part + exactly one content part");
        let meta0_off = u32::from_le_bytes(body[4..8].try_into().unwrap()) as usize;
        let meta0_len = u32::from_le_bytes(body[8..12].try_into().unwrap()) as usize;
        let decoded = Wbxml::new()
            .decode(&body[meta0_off..meta0_off + meta0_len])
            .expect("part 0 is the WBXML response");
        assert!(decoded.contains("<Part>1</Part>"), "got: {decoded}");
        assert!(
            decoded.contains(&format!(
                "<Fetch><Status>11</Status><AirSyncBase:FileReference>{id_a}</AirSyncBase:FileReference></Fetch>"
            )),
            "second Fetch is budget-rejected: {decoded}"
        );
    }

    /// Status-code mapping of the attachment Fetch paths per
    /// [MS-ASCMD] §2.2.3.177.8: unknown store → 9; FileReference with a
    /// non-Range Options child → 2; malformed Range → 8; unroutable email
    /// backend → 3.
    #[tokio::test]
    async fn handle_item_operations_attachment_status_mapping() {
        let state = test_sync_state().await;
        let owner = "statusmap@example.com";

        let make = |body: &str| {
            format!(
                r#"<?xml version="1.0" encoding="utf-8"?><ItemOperations xmlns="ItemOperations:" xmlns:AirSyncBase="AirSyncBase:">{body}</ItemOperations>"#
            )
        };
        async fn run(state: &Arc<crate::models::AppState>, owner: &str, request: &str) -> String {
            let response = handle_item_operations(
                state,
                owner,
                SecretString::from("pw"),
                request,
                &Wbxml::new(),
                false,
                "req-status-map",
                "test-device",
                false,
            )
            .await;
            let body = axum::body::to_bytes(response.into_body(), 1024 * 1024)
                .await
                .expect("collect response");
            String::from_utf8(body.to_vec()).unwrap()
        }

        // [MS-ASCMD] §2.2.3.178.2: only Mailbox (and the unsupported
        // Document Library) are valid stores.
        let xml = run(
            &state,
            owner,
            &make(
                r#"<Fetch><Store>DocumentLibrary</Store><AirSyncBase:FileReference>x</AirSyncBase:FileReference></Fetch>"#,
            ),
        )
        .await;
        assert!(
            xml.contains("<Status>9</Status>"),
            "unknown store → 9: {xml}"
        );

        // §2.2.3.143.2: with a FileReference, Range is the only legal
        // Options child.
        let xml = run(
            &state,
            owner,
            &make(
                r#"<Fetch><Store>Mailbox</Store><AirSyncBase:FileReference>x</AirSyncBase:FileReference><Options><AirSyncBase:BodyPreference><AirSyncBase:Type>1</AirSyncBase:Type></AirSyncBase:BodyPreference></Options></Fetch>"#,
            ),
        )
        .await;
        assert!(
            xml.contains("<Status>2</Status>"),
            "invalid option → 2: {xml}"
        );

        // Malformed Range.
        let xml = run(
            &state,
            owner,
            &make(
                r#"<Fetch><Store>Mailbox</Store><AirSyncBase:FileReference>x</AirSyncBase:FileReference><Options><Range>9-0</Range></Options></Fetch>"#,
            ),
        )
        .await;
        assert!(
            xml.contains("<Status>8</Status>"),
            "malformed range → 8: {xml}"
        );

        // A FileReference that is neither gateway-managed nor backed by a
        // reachable email backend: server error, not "attachment missing".
        let xml = run(
            &state,
            owner,
            &make(
                r#"<Fetch><Store>Mailbox</Store><AirSyncBase:FileReference>nosuchblob</AirSyncBase:FileReference></Fetch>"#,
            ),
        )
        .await;
        assert!(
            xml.contains("<Status>3</Status>"),
            "email backend disabled → 3: {xml}"
        );
        assert!(
            xml.contains("<AirSyncBase:FileReference>nosuchblob</AirSyncBase:FileReference>"),
            "the failing Fetch is identified by its FileReference: {xml}"
        );
    }

    /// The Oof Set audience matching must survive the WBXML wire form: the
    /// decoder renders valueless elements as `<AppliesToExternalKnown>
    /// </AppliesToExternalKnown>` (Start+End), not the `<Tag/>` self-closing
    /// form. A Settings request round-tripped through a real WBXML
    /// encode/decode must therefore still yield the per-audience replies and
    /// the ExternalAudience the client asked for ([MS-ASCMD] §2.2.3.122).
    #[test]
    fn test_oof_audience_matching_survives_wbxml_decode() {
        use crate::oof::ExternalAudience;

        let request_xml = r#"<?xml version="1.0" encoding="utf-8"?>
<Settings xmlns="Settings:">
  <Oof>
    <Get/>
    <Set>
      <OofState>1</OofState>
      <OofMessage>
        <AppliesToInternal/>
        <Enabled>1</Enabled>
        <ReplyMessage>I am away (internal)</ReplyMessage>
      </OofMessage>
      <OofMessage>
        <AppliesToExternalKnown/>
        <Enabled>1</Enabled>
        <ReplyMessage>I am away (known external)</ReplyMessage>
      </OofMessage>
      <OofMessage>
        <AppliesToExternalUnknown/>
        <Enabled>1</Enabled>
        <ReplyMessage>I am away (unknown external)</ReplyMessage>
      </OofMessage>
    </Set>
  </Oof>
</Settings>"#;

        // The exact wire path a target client takes: WBXML encode, then the
        // gateway's decode, then the `<Oof>` inner-content extraction.
        let wbxml = Wbxml::new()
            .encode(request_xml)
            .expect("Settings request must encode");
        let decoded = Wbxml::new()
            .decode(&wbxml)
            .expect("Settings request must decode");
        let oof_inner = decoded
            .find("<Oof>")
            .map(|start| start + "<Oof>".len())
            .and_then(|start| {
                decoded[start..]
                    .find("</Oof>")
                    .map(|end| &decoded[start..start + end])
            })
            .expect("decoded request must carry an Oof block");
        assert!(
            oof_inner.contains("<AppliesToInternal></AppliesToInternal>"),
            "decoder must render valueless tags as Start+End pairs, got: {oof_inner}"
        );

        let (internal_reply, external_reply, audience) = oof_audience_from_oof_messages(oof_inner);
        assert_eq!(internal_reply.as_deref(), Some("I am away (internal)"));
        assert_eq!(
            external_reply.as_deref(),
            Some("I am away (unknown external)"),
            "later OofMessage nodes overwrite the external reply"
        );
        assert_eq!(audience, ExternalAudience::All);

        // Known-only audience after WBXML round-trip.
        let known_only = r#"<?xml version="1.0" encoding="utf-8"?>
<Settings xmlns="Settings:"><Oof><Set><OofState>1</OofState><OofMessage><AppliesToExternalKnown/><Enabled>1</Enabled><ReplyMessage>away known</ReplyMessage></OofMessage></Set></Oof></Settings>"#;
        let decoded = Wbxml::new()
            .decode(&Wbxml::new().encode(known_only).expect("encode"))
            .expect("decode");
        let oof_inner = decoded
            .find("<Oof>")
            .map(|start| start + "<Oof>".len())
            .and_then(|start| {
                decoded[start..]
                    .find("</Oof>")
                    .map(|end| &decoded[start..start + end])
            })
            .expect("Oof block");
        let (_, external_reply, audience) = oof_audience_from_oof_messages(oof_inner);
        assert_eq!(external_reply.as_deref(), Some("away known"));
        assert_eq!(audience, ExternalAudience::KnownExternal);

        // Plain-XML self-closing form still matches (XML-mode clients).
        let plain = "<Set><OofState>1</OofState><OofMessage><AppliesToInternal/><Enabled>1</Enabled><ReplyMessage>plain internal</ReplyMessage></OofMessage></Set>";
        let (internal_reply, _, audience) = oof_audience_from_oof_messages(plain);
        assert_eq!(internal_reply.as_deref(), Some("plain internal"));
        assert_eq!(audience, ExternalAudience::All);

        // Disabled OofMessage nodes are skipped entirely.
        let disabled = "<Set><OofState>1</OofState><OofMessage><AppliesToExternalUnknown/><Enabled>0</Enabled><ReplyMessage>ignored</ReplyMessage></OofMessage></Set>";
        let (internal_reply, external_reply, audience) = oof_audience_from_oof_messages(disabled);
        assert_eq!(internal_reply, None);
        assert_eq!(external_reply, None);
        assert_eq!(audience, ExternalAudience::All);
    }

    /// [MS-ASCMD] §2.2.1.11 / §6.25: a MeetingResponse with no `<Request>`
    /// child cannot produce a spec-shaped Result and is rejected with 400.
    #[tokio::test]
    async fn meeting_response_without_request_element_is_bad_request() {
        let state = test_sync_state().await;
        let response = handle_meeting_response(
            &state,
            "user@example.com",
            &SecretString::from("pw"),
            r#"<?xml version="1.0" encoding="utf-8"?><MeetingResponse xmlns="MeetingResponse:"></MeetingResponse>"#,
            &Wbxml::new(),
            false,
            "rid",
        )
        .await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    fn make_meeting_response(body: &str) -> String {
        format!(
            r#"<?xml version="1.0" encoding="utf-8"?><MeetingResponse xmlns="MeetingResponse:">{body}</MeetingResponse>"#
        )
    }

    async fn run_meeting_response(state: &Arc<crate::models::AppState>, xml: &str) -> String {
        let response = handle_meeting_response(
            state,
            "user@example.com",
            &SecretString::from("pw"),
            xml,
            &Wbxml::new(),
            false,
            "rid",
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        response_text(response).await
    }

    /// [MS-ASCMD] §2.2.3.92.1: an InstanceId that passes schema validation
    /// but is not a valid dateTime yields Status 104 — the raw wire value is
    /// echoed back untouched.
    #[tokio::test]
    async fn meeting_response_malformed_instance_id_yields_104() {
        let state = test_sync_state().await;
        let xml = make_meeting_response(
            r#"<Request><UserResponse>1</UserResponse><RequestId>em-abc</RequestId><InstanceId>not-a-date</InstanceId></Request>"#,
        );
        let body = run_meeting_response(&state, &xml).await;
        assert!(
            body.contains("<Status>104</Status>"),
            "expected status 104, body: {body}"
        );
        assert!(
            body.contains("<RequestId>em-abc</RequestId>"),
            "body: {body}"
        );
        assert!(
            body.contains("<InstanceId>not-a-date</InstanceId>"),
            "body: {body}"
        );
    }

    /// [MS-ASCMD] §2.2.3.151 / §2.2.3.98.2 / §2.2.3.194: an address must be
    /// exactly one of RequestId / search:LongId, and UserResponse must be
    /// 1/2/3 — each violation is Status 2 per §2.2.3.177.9.
    #[tokio::test]
    async fn meeting_response_address_and_user_response_validation() {
        let state = test_sync_state().await;
        let xml = make_meeting_response(
            &[
                r#"<Request><UserResponse>1</UserResponse><RequestId>em-a</RequestId><LongId>em-b</LongId></Request>"#,
                r#"<Request><UserResponse>1</UserResponse></Request>"#,
                r#"<Request><UserResponse>7</UserResponse><RequestId>em-c</RequestId></Request>"#,
            ]
            .concat(),
        );
        let body = run_meeting_response(&state, &xml).await;
        assert_eq!(body.matches("<Result>").count(), 3, "body: {body}");
        assert_eq!(body.matches("<Status>2</Status>").count(), 3, "body: {body}");
    }

    /// [MS-ASCMD] §2.2.3.140/§2.2.3.141: ProposedStartTime and ProposedEndTime
    /// are an all-or-nothing pair — one without the other is Status 2.
    #[tokio::test]
    async fn meeting_response_lone_proposed_time_is_invalid() {
        let state = test_sync_state().await;
        let xml = make_meeting_response(
            r#"<Request><UserResponse>2</UserResponse><RequestId>em-a</RequestId><SendResponse><ProposedStartTime>2026-07-10T09:00:00.000Z</ProposedStartTime></SendResponse></Request>"#,
        );
        let body = run_meeting_response(&state, &xml).await;
        assert!(
            body.contains("<Status>2</Status>"),
            "expected status 2, body: {body}"
        );
    }

    /// [MS-ASCMD] §2.2.3.177.9 Status 2: the request references an item
    /// other than a meeting request, email, or calendar item — an unknown
    /// calendar address never touches the backend.
    #[tokio::test]
    async fn meeting_response_unknown_calendar_address_is_invalid_item() {
        let state = test_sync_state().await;
        let xml = make_meeting_response(
            r#"<Request><UserResponse>1</UserResponse><RequestId>cal-does-not-exist</RequestId></Request>"#,
        );
        let body = run_meeting_response(&state, &xml).await;
        assert!(
            body.contains("<Status>2</Status>"),
            "expected status 2, body: {body}"
        );
    }

    /// [MS-ASCMD] §2.2.3.177.9 Status 3: a server/backend failure resolving
    /// the addressed item. The test JMAP endpoint is unroutable, so an em-
    /// address deterministically fails with a server error, not Status 2.
    #[tokio::test]
    async fn meeting_response_unresolvable_email_address_is_server_error() {
        let state = test_sync_state().await;
        let xml = make_meeting_response(
            r#"<Request><UserResponse>1</UserResponse><RequestId>em-unreachable</RequestId></Request>"#,
        );
        let body = run_meeting_response(&state, &xml).await;
        assert!(
            body.contains("<Status>3</Status>"),
            "expected status 3, body: {body}"
        );
    }

    /// [MS-ASWBXML] code page 8: the rendered response must survive a full
    /// WBXML round-trip through the actual binary response path (as_wbxml)
    /// — the deployed clients receive this form.
    #[tokio::test]
    async fn meeting_response_handler_roundtrips_through_wbxml() {
        let state = test_sync_state().await;
        let xml = make_meeting_response(
            r#"<Request><UserResponse>1</UserResponse><RequestId>cal-does-not-exist</RequestId><InstanceId>2026-07-10T09:00:00.000Z</InstanceId></Request>"#,
        );
        let response = handle_meeting_response(
            &state,
            "user@example.com",
            &SecretString::from("pw"),
            &xml,
            &Wbxml::new(),
            true,
            "rid",
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("wbxml body");
        let decoded = Wbxml::new()
            .decode(&bytes)
            .expect("MeetingResponse WBXML response must decode");
        assert!(decoded.contains("<MeetingResponse"), "{decoded}");
        assert!(decoded.contains("<Status>2</Status>"), "{decoded}");
        assert!(
            decoded.contains("<InstanceId>2026-07-10T09:00:00.000Z</InstanceId>"),
            "{decoded}"
        );
    }
}
