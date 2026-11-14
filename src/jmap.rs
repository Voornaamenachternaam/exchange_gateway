// src/jmap.rs
//
// JMAP (JSON Meta Application Protocol) client for email and calendar
// operations with Stalwart Mailserver v0.16.6.
//
// JMAP (RFC 8621) provides efficient email query/get/sync operations
// via a single HTTP endpoint. Stalwart v0.16.6 supports JMAP natively
// at the /jmap/ path with Basic authentication.
//
// This module implements:
// - Email query (search/list emails in a mailbox)
// - Email get (fetch full email content)
// - Email sync (delta updates via state tokens)
// - Email submission (send email via EmailSubmission/set, RFC 8621 §2.7)
// - Mailbox query/get (list email folders)
// - Calendar query/get (list calendars, draft-ietf-jmap-calendars-26)
// - CalendarEvent query/get/set/destroy (calendar CRUD)
// - CalendarEvent/parse (iCalendar → JSCalendar conversion)
// - Free-busy via Principal/getAvailability
//
// The gateway uses JMAP for email reading/syncing AND email submission,
// eliminating the need for SMTP between gateway and Stalwart.
// JMAP Calendar (urn:ietf:params:jmap:calendars) replaces CalDAV for
// calendar operations when available, with CalDAV as fallback.

use crate::auth::CredentialCheck;
use anyhow::{Result, anyhow};
use dashmap::DashMap;
use moka::Expiry;
use moka::sync::Cache;
use reqwest::header::{AUTHORIZATION, CONTENT_TYPE};
use secrecy::{ExposeSecret, SecretString};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;
use tracing::{debug, trace, warn};

/// Default `maxChanges` for `Email/changes` calls (RFC 8621 §4.4) when a
/// caller does not request a specific bound. Matches the historical page
/// size used across the gateway's JMAP callers.
pub const DEFAULT_EMAIL_CHANGES_MAX: u32 = 500;

/// JMAP core capability URN (RFC 8620 §2). The session advertises the
/// server's request limits (`maxObjectsInGet`, `maxObjectsInSet`,
/// `maxSizeRequest`, …) under this key.
pub const JMAP_CORE_CAPABILITY: &str = "urn:ietf:params:jmap:core";

/// Fallback for the session's `maxObjectsInGet` when the server omits it.
/// RFC 8620 §2 requires the value, but a conforming client must not assume it
/// is present; 500 matches the value Stalwart v1.0.x advertises, so an
/// absent capability degrades to the same behaviour as a present one.
pub const DEFAULT_MAX_OBJECTS_IN_GET: usize = 500;

/// Split an id list into consecutive slices of at most `max` ids.
///
/// `max` is clamped to at least 1 so a server advertising `0` (or a caller
/// passing `0`) yields single-id slices instead of spinning on an empty chunk
/// forever.
pub fn chunk_ids(ids: &[String], max: usize) -> Vec<&[String]> {
    ids.chunks(max.max(1)).collect()
}

/// SHA-256 (first 16 bytes, lowercase hex) over an event's JSCalendar object.
///
/// Stalwart v1.0.x does not expose a JMAP `@etag` for `CalendarEvent`, so the
/// gateway derives a stable, content-addressed token instead: two reads of an
/// unchanged event yield the same value and any content change yields a new
/// one. The value is computed over the object's canonical
/// [`serde_json::to_string`] form, which is deterministic for `serde_json`'s
/// default `BTreeMap`-backed `Value::Object` (sorted keys). The JMAP `id` is
/// never included, so a re-created event with identical content keeps the same
/// token. `server_etag`, when the server *does* expose one, wins.
///
/// Used for both the change signal (a `CalendarEvent/changes` delta already
/// names its changed ids, so this only guards the initial snapshot diff) and
/// the EWS `ChangeKey`.
pub fn calendar_event_etag(event: &Value, server_etag: Option<&str>) -> String {
    if let Some(e) = server_etag.map(str::trim).filter(|s| !s.is_empty()) {
        return e.trim_matches('"').to_string();
    }
    use sha2::Digest as _;
    let bytes = serde_json::to_vec(event).unwrap_or_default();
    let digest = sha2::Sha256::digest(&bytes);
    const_hex::encode(&digest[..16])
}

/// `maxChanges` requested per `CalendarEvent/changes` page while draining a
/// change chain. Large enough to drain a realistic backlog in one or two
/// round-trips, small enough that a page is a bounded response.
const CALENDAR_CHANGES_PAGE_SIZE: u32 = 500;

/// Upper bound on ids accumulated across a `CalendarEvent/changes` drain, so a
/// pathological backend cannot loop forever. Anything past this bound stays
/// behind the intermediate `newState` and is drained by the next Sync's
/// follow-up delta (the intermediate state is persisted with
/// `has_more_changes = true` and the EAS response signals `<MoreAvailable/>`),
/// so nothing is lost.
const MAX_CALENDAR_CHANGE_ACCUMULATION: usize = 5000;

/// Upper bound on the number of `CalendarEvent/changes` pages drained in one
/// call, independent of the id count (a page may legitimately report
/// `hasMoreChanges` with few ids). Mirrors the accumulation bound's intent.
const MAX_CALENDAR_CHANGE_PAGES: usize = 100;

/// Remove duplicates from an id list, preserving first-seen order.
fn dedupe_ids(ids: Vec<String>) -> Vec<String> {
    let mut seen = std::collections::HashSet::with_capacity(ids.len());
    let mut out = Vec::with_capacity(ids.len());
    for id in ids {
        if seen.insert(id.clone()) {
            out.push(id);
        }
    }
    out
}

/// The server-advertised `maxObjectsInGet` for this session (RFC 8620 §2).
///
/// Falls back to [`DEFAULT_MAX_OBJECTS_IN_GET`] when the core capability is
/// missing or omits the value. A server that advertises `0` is treated as
/// advertising 1 (the value is a maximum, and 0 would be unsatisfiable).
pub fn session_max_objects_in_get(session: &JmapSession) -> usize {
    session
        .capabilities
        .get(JMAP_CORE_CAPABILITY)
        .and_then(|c| c.get("maxObjectsInGet"))
        .and_then(|v| v.as_u64())
        .map(|n| (n.max(1)) as usize)
        .unwrap_or(DEFAULT_MAX_OBJECTS_IN_GET)
}

/// Helper: deserialize a field that may be either a single object, an array, or null.
/// This accommodates servers that sometimes return a single object instead of an array
/// for address fields (from, to, cc, bcc, replyTo). Returns `None` for null, `Some(vec)` otherwise.
///
/// The type `T` must be `DeserializeOwned` because we materialize the JSON value locally
/// and then deserialize `T` from it; borrowing from the deserializer lifetime is not allowed.
fn one_or_array<'de, D, T>(deserializer: D) -> Result<Option<Vec<T>>, D::Error>
where
    D: Deserializer<'de>,
    T: DeserializeOwned,
{
    let value = Value::deserialize(deserializer)?;
    if value.is_null() {
        return Ok(None);
    }
    match value {
        Value::Array(arr) => arr
            .into_iter()
            .map(|v| T::deserialize(v).map_err(serde::de::Error::custom))
            .collect::<Result<Vec<_>, _>>()
            .map(Some),
        _ => {
            let single = T::deserialize(value).map_err(serde::de::Error::custom)?;
            Ok(Some(vec![single]))
        }
    }
}

/// JMAP session object (RFC 8621 §2.1)
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JmapSession {
    pub api_url: String,
    pub download_url: String,
    pub upload_url: String,
    pub event_source_url: String,
    pub state: String,
    pub username: String,
    pub accounts: HashMap<String, JmapAccount>,
    pub primary_accounts: HashMap<String, String>,
    pub capabilities: HashMap<String, Value>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JmapAccount {
    pub name: String,
    pub is_personal: bool,
    pub is_read_only: bool,
    pub account_capabilities: HashMap<String, Value>,
}

/// Outcome of a successful windowed blob download for an EAS ItemOperations
/// Fetch ([MS-ASCMD] §2.2.1.10): the requested bytes plus the metadata the
/// response `Properties` element carries ([MS-ASCMD] §2.2.3.139.2).
#[derive(Clone, Debug)]
pub struct BlobRangeFetch {
    /// The bytes inside the requested window (already sliced; not the whole
    /// blob when a range was requested).
    pub data: Vec<u8>,
    /// `Content-Type` reported by the backend, for
    /// `AirSyncBase:ContentType` ([MS-ASAIRS] §2.2.2.18.2).
    pub content_type: String,
    /// Whole-blob size in bytes, for `Total` ([MS-ASCMD] §2.2.3.184.2).
    pub total_size: Option<u64>,
    /// Half-open absolute window `[start, end)` that `data` actually covers
    /// — the authoritative value for the response `Range` element
    /// ([MS-ASCMD] §2.2.3.143.2: "The byte-range that is specified by the
    /// server in the response is the authoritative value").
    pub window: (u64, u64),
    /// Whether the window was served in full. `false` means EOF was reached
    /// first, which maps to ItemOperations status 17 ("Partial success").
    pub complete: bool,
}

/// Typed failure of a windowed blob download, each variant mapping onto an
/// ItemOperations Fetch status ([MS-ASCMD] §2.2.3.177.8):
/// `InvalidBlobId`/`NotFound` → 15, `AccessDenied` → 16, `Server` → 3,
/// `TooLarge` → 11, `RangeTooLarge`/`RangeStartsPastEof` → 8.
#[derive(Debug)]
pub enum BlobFetchError {
    /// The blobId is not a legal RFC 8620 `Id`.
    InvalidBlobId,
    /// The backend has no such blob (404/410).
    NotFound(String),
    /// The backend refused the download (401/403).
    AccessDenied(String),
    /// Transport failure, backend 5xx, or a failed session lookup.
    Server(anyhow::Error),
    /// The whole item exceeds the per-request budget (no range requested).
    TooLarge { total: u64, limit: usize },
    /// The requested byte range exceeds the per-request window budget.
    RangeTooLarge { requested: u64, limit: usize },
    /// The requested range starts at or after the end of the item.
    RangeStartsPastEof { total: u64 },
}

impl std::fmt::Display for BlobFetchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidBlobId => write!(f, "Invalid JMAP blobId"),
            Self::NotFound(msg) => write!(f, "{msg}"),
            Self::AccessDenied(msg) => write!(f, "{msg}"),
            Self::Server(e) => write!(f, "{e}"),
            Self::TooLarge { total, limit } => write!(
                f,
                "JMAP blob too large: {total} bytes exceeds the {limit}-byte budget"
            ),
            Self::RangeTooLarge { requested, limit } => write!(
                f,
                "Byte range too large: {requested} bytes exceeds the {limit}-byte window budget"
            ),
            Self::RangeStartsPastEof { total } => {
                write!(f, "Byte range starts at or past end of {total}-byte item")
            }
        }
    }
}

impl std::error::Error for BlobFetchError {}

/// JMAP API request (RFC 8621 §3.1)
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct JmapRequest {
    pub using: Vec<String>,
    pub method_calls: Vec<JmapMethodCall>,
}

/// A JMAP method call, serialized as a 3-element array per RFC 8621 §3.2:
/// `["methodName", {arguments}, "id"]`.
///
/// Using a tuple representation is critical because JMAP servers (including
/// Stalwart) reject object-form method calls with a 400 `notRequest` error:
/// "invalid type: map, expected an array with 3 elements".
#[derive(Clone, Debug)]
pub struct JmapMethodCall {
    pub name: String,
    pub arguments: Value,
    pub id: String,
}

impl Serialize for JmapMethodCall {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        // RFC 8621 §3.2: Each invocation is represented as an array of 3 elements:
        // [String:method name, {arguments}, String:method id]
        (&self.name, &self.arguments, &self.id).serialize(serializer)
    }
}

/// JMAP API response
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JmapResponse {
    pub method_responses: Vec<(String, Value, String)>,
}

/// JMAP Email object (RFC 8621 §4.1)
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JmapEmail {
    pub id: Option<String>,
    #[serde(default)]
    pub blob_id: Option<String>,
    #[serde(default)]
    pub thread_id: Option<String>,
    #[serde(default)]
    pub mailbox_ids: Option<HashMap<String, bool>>,
    #[serde(default)]
    pub keywords: Option<HashMap<String, bool>>,
    #[serde(default)]
    pub size: Option<u64>,
    #[serde(default)]
    pub received_at: Option<String>,
    #[serde(default)]
    pub sent_at: Option<String>,
    #[serde(default)]
    pub has_attachment: Option<bool>,
    #[serde(default, deserialize_with = "one_or_array")]
    pub from: Option<Vec<JmapEmailAddress>>,
    #[serde(default, deserialize_with = "one_or_array")]
    pub to: Option<Vec<JmapEmailAddress>>,
    #[serde(default, deserialize_with = "one_or_array")]
    pub cc: Option<Vec<JmapEmailAddress>>,
    #[serde(default, deserialize_with = "one_or_array")]
    pub bcc: Option<Vec<JmapEmailAddress>>,
    #[serde(default, deserialize_with = "one_or_array")]
    pub reply_to: Option<Vec<JmapEmailAddress>>,
    #[serde(default)]
    pub subject: Option<String>,
    #[serde(default)]
    pub preview: Option<String>,
    #[serde(default)]
    pub body_values: Option<HashMap<String, JmapBodyValue>>,
    #[serde(default)]
    pub text_body: Option<Vec<JmapBodyPart>>,
    #[serde(default)]
    pub html_body: Option<Vec<JmapBodyPart>>,
    #[serde(default)]
    pub attachments: Option<Vec<JmapAttachment>>,
    /// Full MIME structure (RFC 8621 §4.1.3 `bodyStructure`). Only populated
    /// when `bodyStructure` is requested via the Email/get property list. Kept
    /// as a generic JSON value because the structure is recursive and the
    /// gateway only needs it to locate attachment part ids, not full typing.
    #[serde(default)]
    pub body_structure: Option<Value>,
    /// Raw RFC 5322 message headers as a single CRLF-delimited blob (RFC 8621
    /// §4.1.3 `header:raw`). Only populated when explicitly requested.
    #[serde(default, rename = "header:raw")]
    pub header_raw: Option<String>,
    /// Sender envelope (RFC 8621 §4.1.3 `sender`).
    #[serde(default, deserialize_with = "one_or_array")]
    pub sender: Option<Vec<JmapEmailAddress>>,
    /// Message-ID header value, sans angle brackets (RFC 8621 §4.1.3 `messageId`).
    #[serde(default)]
    pub message_id: Option<String>,
    /// In-Reply-To header value(s) (RFC 8621 §4.1.3 `inReplyTo`).
    #[serde(default)]
    pub in_reply_to: Option<Vec<String>>,
    /// References header value(s) (RFC 8621 §4.1.3 `references`).
    #[serde(default)]
    pub references: Option<Vec<String>>,
}

impl JmapEmail {
    /// True if the email has the `$draft` keyword (RFC 8621 §4.1.1).
    pub fn is_draft(&self) -> bool {
        self.keywords
            .as_ref()
            .is_some_and(|k| k.contains_key("$draft"))
    }

    /// Expose the user-defined keyword labels (those without the reserved `$`
    /// prefix used for JMAP system flags) for callers that surface them as EWS
    /// Categories. System keywords are not user categories and are excluded.
    pub fn category_labels(&self) -> Vec<String> {
        self.keywords
            .as_ref()
            .map(|k| k.keys().filter(|k| !k.starts_with('$')).cloned().collect())
            .unwrap_or_default()
    }

    /// The email's active keywords (those set to `true`), sorted — the same
    /// canonical form the EAS flag baseline stores (`list_email_ids_with_keywords_page`).
    pub fn sorted_active_keywords(&self) -> Vec<String> {
        let mut keywords: Vec<String> = self
            .keywords
            .as_ref()
            .map(|m| {
                m.iter()
                    .filter(|(_, v)| **v)
                    .map(|(k, _)| k.clone())
                    .collect()
            })
            .unwrap_or_default();
        keywords.sort();
        keywords
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JmapEmailAddress {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub email: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JmapBodyValue {
    pub value: String,
    #[serde(default)]
    pub is_encoding_problem: Option<bool>,
    /// RFC 8621 §4.1.4 `isTruncated`: true when the server returned fewer
    /// bytes than `maxBodyValueSize` ... i.e. the value was cut short. The
    /// EAS body renderer maps this to `<AirSyncBase:Truncated>`.
    #[serde(default)]
    pub is_truncated: Option<bool>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JmapBodyPart {
    pub part_id: String,
    #[serde(default)]
    pub blob_id: Option<String>,
    #[serde(default)]
    pub size: Option<u64>,
    #[serde(rename = "type")]
    #[serde(default)]
    pub content_type: Option<String>,
    #[serde(default)]
    pub charset: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JmapAttachment {
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub blob_id: Option<String>,
    #[serde(default)]
    pub size: Option<u64>,
    #[serde(rename = "type")]
    #[serde(default)]
    pub content_type: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
}

/// JMAP Mailbox object (RFC 8621 §5.1)
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JmapMailbox {
    pub id: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub parent_id: Option<String>,
    #[serde(default)]
    pub role: Option<String>,
    #[serde(default)]
    pub sort_order: Option<u64>,
    #[serde(default)]
    pub total_emails: Option<u64>,
    #[serde(default)]
    pub unread_emails: Option<u64>,
    #[serde(default)]
    pub total_threads: Option<u64>,
    #[serde(default)]
    pub unread_threads: Option<u64>,
    #[serde(default)]
    pub is_subscribed: Option<bool>,
    /// JMAP sharing data (`shareWith`, draft-ietf-jmap-sharing; Stalwart
    /// exposes per-principal rights objects). Absent when the server does
    /// not advertise sharing or the mailbox is not shared.
    #[serde(default, rename = "shareWith")]
    pub share_with: Option<std::collections::BTreeMap<String, serde_json::Value>>,
}

/// Result of an Email/query call
#[derive(Clone, Debug)]
pub struct EmailListResult {
    pub emails: Vec<JmapEmail>,
    pub total: u64,
    pub can_calculate_changes: bool,
    pub query_state: String,
    /// Current state of the Email data type (from Email/get response).
    /// Used as `sinceState` for subsequent `Email/changes` calls.
    pub state: String,
    /// RFC 8620 §5.5 `anchorNotFound`: the query carried an `anchor` that no
    /// longer exists in the result set. The result is empty; the caller falls
    /// back to numeric `position` paging.
    pub anchor_not_found: bool,
    /// RFC 8620 §5.5 response `position`: the zero-based index of the first
    /// returned id within the COMPLETE result set. This is the authoritative
    /// absolute progress of a paged walk — unlike a locally accumulated
    /// numeric offset, it stays correct when an `anchor` is used or when a
    /// concurrent deletion shifts the tail.
    pub query_position: u64,
}

impl EmailListResult {
    /// An empty result (no emails, no state) for a folder that cannot be
    /// queried — an unmapped role, an Outbox, or a mailbox lookup that found
    /// nothing.
    pub fn empty() -> Self {
        EmailListResult {
            emails: Vec::new(),
            total: 0,
            can_calculate_changes: false,
            query_state: String::new(),
            state: String::new(),
            anchor_not_found: false,
            query_position: 0,
        }
    }
}

/// Result of an Email/changes call
#[derive(Clone, Debug)]
pub struct EmailChangesResult {
    pub created: Vec<String>,
    pub updated: Vec<String>,
    pub destroyed: Vec<String>,
    pub new_state: String,
    pub has_more_changes: bool,
}

/// Result of a CalendarEvent/changes call
#[derive(Clone, Debug)]
pub struct CalendarChangesResult {
    pub created: Vec<String>,
    pub updated: Vec<String>,
    pub destroyed: Vec<String>,
    pub new_state: String,
    pub has_more_changes: bool,
}

/// Result of a Mailbox/query call
#[derive(Clone, Debug)]
pub struct MailboxListResult {
    pub mailboxes: Vec<JmapMailbox>,
    pub total: u64,
}

/// Outcome of a JMAP `Mailbox/set` `destroy` (RFC 8621 §2.5). Distinguishes the
/// three cases a caller must react to differently: the mailbox is gone
/// (`Destroyed`), the mailbox was already absent so the desired end state holds
/// (`AlreadyGone`, RFC 8620 §5.3 `notFound`), or the server refused the destroy
/// for another reason (`Refused`: `mailboxHasEmail`, `mailboxHasChild`,
/// `forbidden`, …).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MailboxDestroyOutcome {
    Destroyed,
    AlreadyGone,
    Refused,
}

/// Parameters for querying emails via JMAP.
pub struct QueryEmailsParams<'a> {
    pub account_id: &'a str,
    pub filter: Option<Value>,
    pub sort: Option<Vec<Value>>,
    pub position: u64,
    pub limit: u64,
    /// RFC 8620 §5.5 `anchor`: an id at which the window starts instead of the
    /// numeric `position`. A caller resuming a paged walk pins the last item it
    /// delivered so a concurrent deletion cannot shift the tail below a stale
    /// numeric offset and skip a message. When set, `position` is ignored by
    /// the server (RFC 8620 §5.5) and a not-found anchor surfaces as
    /// [`EmailListResult::anchor_not_found`] so the caller can fall back.
    pub anchor: Option<&'a str>,
    /// RFC 8620 §5.5 `anchorOffset`: the index of the first result relative to
    /// the anchor. `0` includes the anchor itself; `1` starts immediately
    /// after it. Only meaningful when `anchor` is set.
    pub anchor_offset: i32,
    pub username: &'a str,
    pub password: &'a SecretString,
}

/// Parameters for submitting an email via JMAP EmailSubmission/set (RFC 8621 §2.7).
pub struct SubmitEmailParams<'a> {
    pub account_id: &'a str,
    pub from: &'a str,
    pub to: &'a [String],
    pub cc: &'a [String],
    pub bcc: &'a [String],
    pub subject: &'a str,
    pub text_body: &'a str,
    pub html_body: Option<&'a str>,
    /// In-Reply-To header values (message IDs without angle brackets,
    /// RFC 8621 §4.1.3), used by SmartReply/Reply cross-protocol threading.
    pub in_reply_to: Option<&'a [String]>,
    /// References header values (message IDs without angle brackets).
    pub references: Option<&'a [String]>,
    pub username: &'a str,
    pub password: &'a SecretString,
}

// ---------------------------------------------------------------------------
// JMAP Calendar data types (draft-ietf-jmap-calendars-26)
// ---------------------------------------------------------------------------

/// JMAP capability URN for calendar operations (draft-ietf-jmap-calendars §1.5.1).
pub const JMAP_CAL_CAPABILITY: &str = "urn:ietf:params:jmap:calendars";

/// JMAP capability URN for availability/free-busy (draft-ietf-jmap-calendars §1.5.2).
pub const JMAP_CAL_AVAILABILITY_CAPABILITY: &str = "urn:ietf:params:jmap:principals:availability";

/// JMAP capability URN for Stalwart's admin/directory extension. Stalwart
/// exposes the server-wide directory (accounts, groups, mailing lists, domains)
/// through a set of `x:Account/*`, `x:MailingList/*`, `x:Domain/*` and
/// `x:Directory/*` JMAP methods advertised under this capability. This is the
/// supported replacement for the deprecated Stalwart REST admin API
/// (`/api/{v1|v2|...}`); the gateway's directory service (GAL / ResolveNames /
/// OAB / NSPI) and OOF Sieve management talk to Stalwart exclusively through
/// JMAP and never through the legacy REST endpoints.
pub const JMAP_STALWART_CAPABILITY: &str = "urn:stalwart:jmap";

/// JMAP capability URN for Sieve Scripts (RFC 9661). Used by the OOF manager to
/// store vacation scripts via `SieveScript/get`/`set`.
pub const JMAP_SIEVE_CAPABILITY: &str = "urn:ietf:params:jmap:sieve";

/// JMAP capability URN for contacts (RFC 9610).
pub const JMAP_CONTACTS_CAPABILITY: &str = "urn:ietf:params:jmap:contacts";

/// Request-scoped inputs for a verified calendar-probe destroy
/// ([`JmapClient::probe_destroy_confirmed`]). Grouping them keeps the helper
/// under clippy's argument-count threshold.
struct DestroyProbeCtx<'a> {
    session: &'a JmapSession,
    method: &'a str,
    account_id: &'a str,
    ids: &'a [String],
    username: &'a str,
    password: &'a SecretString,
}

/// Result of the deploy-time JMAP Calendars capability verification
/// (`JmapClient::verify_calendar_capability_deploy`).
#[derive(Clone, Debug, Default)]
pub struct CalendarCapabilityReport {
    /// Whether the session advertises `urn:ietf:params:jmap:calendars`.
    pub capability_present: bool,
    /// Raw capability object advertised by the server.
    pub capability_details: Option<Value>,
    /// Primary account ID for the calendars capability.
    pub account_id: Option<String>,
    /// draft-ietf-jmap-calendars §1.5.1 fields extracted from the capability.
    pub max_calendars_per_event: Option<u64>,
    pub min_date_time: Option<String>,
    pub max_date_time: Option<String>,
    pub max_expanded_query_duration: Option<String>,
    pub max_participants_per_event: Option<u64>,
    pub may_create_calendar: Option<bool>,
    /// `Calendar/get` against the primary account succeeded.
    pub calendar_get_ok: bool,
    /// `CalendarEvent/query` against the primary account succeeded.
    pub calendar_event_query_ok: bool,
    /// The write probe succeeded: an ICS document converted to structured
    /// JSCalendar properties (via `ics_to_jscalendar_event` +
    /// `jscalendar_write_payload`, the exact helpers `set_calendar_event`
    /// uses) came back with its content applied. This is the write path the
    /// gateway uses for created events.
    pub ics_round_trip_ok: bool,
    /// The write probe succeeded for a structured event with
    /// `recurrenceRule` + `recurrenceOverrides` (an excluded occurrence),
    /// verified by read-back and by occurrence-window query expansion.
    pub structured_recurrence_ok: bool,
    /// All problems found, for deploy-time diagnostics. Empty when healthy.
    pub issues: Vec<String>,
    /// Informational gaps in the advertised capability object that do NOT
    /// affect gateway operation. draft-ietf-jmap-calendars §1.5.1 declares the
    /// `maxCalendarsPerEvent`/`maxParticipantsPerEvent`/`minDateTime`/
    /// `maxDateTime`/`maxExpandedQueryDuration`/`mayCreateCalendar` fields as
    /// required, but a backend that omits them still functions — Stalwart
    /// v1.0.x advertises `urn:ietf:params:jmap:calendars` as `{}` and works
    /// correctly. These are surfaced for operators without failing the check.
    pub warnings: Vec<String>,
}

impl CalendarCapabilityReport {
    /// The verification passed with no functional issues. The capability
    /// object's optional/absent metadata fields are reported in `warnings`,
    /// not `issues` (see the `warnings` field docs).
    pub fn is_healthy(&self) -> bool {
        self.capability_present
            && self.calendar_get_ok
            && self.calendar_event_query_ok
            && self.structured_recurrence_ok
            && self.ics_round_trip_ok
            && self.issues.is_empty()
    }

    /// Extract draft-ietf-jmap-calendars §1.5.1 capability fields. The fields
    /// are declared required by the spec, but a backend may omit them and
    /// still work (Stalwart v1.0.x advertises `{}`); an absent field is
    /// therefore recorded as a non-fatal WARNING, while a field advertised
    /// with the wrong TYPE is a fatal issue. `maxCalendarsPerEvent`/
    /// `maxParticipantsPerEvent` may explicitly be `null` ("no limit") — that
    /// is accepted and stored as `None`.
    fn extract_capability_details(&mut self, details: &Value) {
        self.capability_details = Some(details.clone());
        let Some(obj) = details.as_object() else {
            self.issues.push(format!(
                "calendars capability object is not a JSON object: {}",
                details
            ));
            return;
        };
        let take_u64 =
            |key: &str, issues: &mut Vec<String>, warnings: &mut Vec<String>| -> Option<u64> {
                match obj.get(key) {
                    None => {
                        warnings.push(format!(
                        "calendars capability does not advertise the §1.5.1 field {}; the server \
                         treats it as unset (no limit), which is safe",
                        key
                    ));
                        None
                    }
                    Some(v) if v.is_null() => None,
                    Some(v) => match v.as_u64() {
                        Some(n) => Some(n),
                        None => {
                            issues.push(format!(
                                "capability field {} is not an unsigned integer or null: {}",
                                key, v
                            ));
                            None
                        }
                    },
                }
            };
        let take_string =
            |key: &str, issues: &mut Vec<String>, warnings: &mut Vec<String>| -> Option<String> {
                match obj.get(key) {
                    None => {
                        warnings.push(format!(
                            "calendars capability does not advertise the §1.5.1 field {}; the \
                             gateway does not require it",
                            key
                        ));
                        None
                    }
                    Some(v) => match v.as_str() {
                        Some(s) => Some(s.to_string()),
                        None => {
                            issues.push(format!("capability field {} is not a string: {}", key, v));
                            None
                        }
                    },
                }
            };
        let mut issues = std::mem::take(&mut self.issues);
        let mut warnings = std::mem::take(&mut self.warnings);
        self.max_calendars_per_event = take_u64("maxCalendarsPerEvent", &mut issues, &mut warnings);
        self.max_participants_per_event =
            take_u64("maxParticipantsPerEvent", &mut issues, &mut warnings);
        self.min_date_time = take_string("minDateTime", &mut issues, &mut warnings);
        self.max_date_time = take_string("maxDateTime", &mut issues, &mut warnings);
        self.max_expanded_query_duration =
            take_string("maxExpandedQueryDuration", &mut issues, &mut warnings);
        match obj.get("mayCreateCalendar") {
            None => warnings.push(
                "calendars capability does not advertise the §1.5.1 field mayCreateCalendar; \
                 assuming calendar creation is permitted"
                    .to_string(),
            ),
            Some(v) => match v.as_bool() {
                Some(b) => self.may_create_calendar = Some(b),
                None => issues.push(format!(
                    "capability field mayCreateCalendar is not a boolean: {}",
                    v
                )),
            },
        }
        if self.may_create_calendar == Some(false) {
            issues.push(
                "server reports mayCreateCalendar=false — calendar creation via JMAP will fail"
                    .to_string(),
            );
        }
        self.issues = issues;
        self.warnings = warnings;
    }
}

/// A ContactCard record as returned by `ContactCard/get`, carrying the raw
/// JSCard JSON (RFC 9553) plus bookkeeping metadata used for change detection.
#[derive(Clone, Debug)]
pub struct JmapContactCard {
    pub id: String,
    /// Native ETag if the server exposes one (e.g. Stalwart `@etag`).
    pub etag: Option<String>,
    /// Raw JSCard document.
    pub card: Value,
}

/// JMAP Calendar object (draft-ietf-jmap-calendars §4).
/// Represents a named collection of CalendarEvents.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JmapCalendar {
    pub id: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub color: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub sort_order: Option<u64>,
    #[serde(default)]
    pub is_subscribed: Option<bool>,
    #[serde(default)]
    pub is_visible: Option<bool>,
    #[serde(default)]
    pub href: Option<String>,
    #[serde(default)]
    pub default_alerts: Option<Value>,
    #[serde(default)]
    pub default_is_all_day: Option<bool>,
    #[serde(default)]
    pub default_uses_default_alerts: Option<bool>,
    #[serde(default, rename = "type")]
    pub calendar_type: Option<String>,
    #[serde(default)]
    pub scale: Option<String>,
    #[serde(default)]
    pub time_zone: Option<String>,
}

/// JMAP CalendarEvent object (draft-ietf-jmap-calendars §5).
/// Represents a calendar event in JSCalendar format.
/// The `iCalendar` property is requested for gateway compatibility —
/// the gateway needs ICS data to render EWS/EAS calendar items.
#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct JmapCalendarEvent {
    pub id: Option<String>,
    #[serde(default)]
    pub uid: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub start: Option<Value>,
    #[serde(default)]
    pub end: Option<Value>,
    #[serde(default)]
    pub duration: Option<String>,
    #[serde(default)]
    pub location: Option<Value>,
    #[serde(default)]
    pub participants: Option<Value>,
    #[serde(default)]
    pub alerts: Option<Value>,
    #[serde(default)]
    pub calendar_ids: Option<HashMap<String, bool>>,
    #[serde(default)]
    pub is_all_day: Option<bool>,
    #[serde(default)]
    pub recurrence_rules: Option<Vec<Value>>,
    #[serde(default)]
    pub recurrence_overrides: Option<Value>,
    #[serde(default)]
    pub excluded: Option<Value>,
    #[serde(default)]
    pub sequence: Option<u64>,
    #[serde(default)]
    pub priority: Option<u64>,
    #[serde(default)]
    pub privacy: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub time_zone: Option<String>,
    #[serde(default)]
    pub created: Option<String>,
    #[serde(default)]
    pub updated: Option<String>,
    /// The raw JSCalendar `iCalendar` property, requested via
    /// `CalendarEvent/get` with `properties: ["iCalendar"]`. Stalwart does
    /// NOT return an ICS string here — it returns the JSCalendar object for
    /// the source component (`{"convertedProperties":…,"name":"vevent"}`).
    /// It is retained as a [`Value`]; use [`Self::to_ics`] for iCalendar.
    #[serde(rename = "iCalendar")]
    #[serde(default)]
    pub i_calendar: Option<Value>,
    /// The complete raw `CalendarEvent/get` object this event was parsed from.
    /// The JSCalendar data needed to rebuild iCalendar is spread across the
    /// whole object (standard properties plus the `iCalendar` fragment), so
    /// the raw object is retained and converted on demand by
    /// [`Self::to_ics`]. Populated by the JMAP client; `None` for
    /// hand-constructed instances.
    #[serde(skip)]
    pub raw: Option<Value>,
    /// The ETag of the event (from the JMAP response). Used for concurrency
    /// and change tracking, replacing CalDAV's ETag handling.
    #[serde(rename = "@etag")]
    #[serde(default)]
    pub etag: Option<String>,
}

impl JmapCalendarEvent {
    /// iCalendar (ICS) for this event, reconstructed from the JSCalendar
    /// object Stalwart returned.
    ///
    /// Stalwart's `iCalendar` property is a JSCalendar object, not an ICS
    /// string, so this reassembles the whole event object and converts it
    /// with `calcard` (the library Stalwart itself uses). Returns `None` when
    /// no event data was fetched or the conversion yields no component.
    pub fn to_ics(&self) -> Option<String> {
        let ics = self
            .raw
            .as_ref()
            .and_then(crate::jscalendar::jscalendar_event_to_ics);
        if ics.is_none() && self.raw.is_some() {
            warn!(
                target: "jmap",
                event_id = self.id.as_deref().unwrap_or(""),
                "CalendarEvent could not be converted to iCalendar; event skipped"
            );
        }
        ics
    }
}

/// Deserialization mirror of [`JmapCalendarEvent`] that exists solely so the
/// whole server object can be retained in [`JmapCalendarEvent::raw`] on the
/// public serde path. `raw` is `#[serde(skip)]` on the public struct, so a
/// directly deserialized event would otherwise lose the JSCalendar data
/// `to_ics` needs. Kept in lockstep with the public struct's field list.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct JmapCalendarEventShadow {
    id: Option<String>,
    #[serde(default)]
    uid: Option<String>,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    start: Option<Value>,
    #[serde(default)]
    end: Option<Value>,
    #[serde(default)]
    duration: Option<String>,
    #[serde(default)]
    location: Option<Value>,
    #[serde(default)]
    participants: Option<Value>,
    #[serde(default)]
    alerts: Option<Value>,
    #[serde(default)]
    calendar_ids: Option<HashMap<String, bool>>,
    #[serde(default)]
    is_all_day: Option<bool>,
    #[serde(default)]
    recurrence_rules: Option<Vec<Value>>,
    #[serde(default)]
    recurrence_overrides: Option<Value>,
    #[serde(default)]
    excluded: Option<Value>,
    #[serde(default)]
    sequence: Option<u64>,
    #[serde(default)]
    priority: Option<u64>,
    #[serde(default)]
    privacy: Option<String>,
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    time_zone: Option<String>,
    #[serde(default)]
    created: Option<String>,
    #[serde(default)]
    updated: Option<String>,
    #[serde(rename = "iCalendar")]
    #[serde(default)]
    i_calendar: Option<Value>,
    #[serde(rename = "@etag")]
    #[serde(default)]
    etag: Option<String>,
}

impl<'de> Deserialize<'de> for JmapCalendarEvent {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let raw = Value::deserialize(deserializer)?;
        let shadow = serde_json::from_value::<JmapCalendarEventShadow>(raw.clone())
            .map_err(serde::de::Error::custom)?;
        Ok(JmapCalendarEvent {
            id: shadow.id,
            uid: shadow.uid,
            title: shadow.title,
            description: shadow.description,
            start: shadow.start,
            end: shadow.end,
            duration: shadow.duration,
            location: shadow.location,
            participants: shadow.participants,
            alerts: shadow.alerts,
            calendar_ids: shadow.calendar_ids,
            is_all_day: shadow.is_all_day,
            recurrence_rules: shadow.recurrence_rules,
            recurrence_overrides: shadow.recurrence_overrides,
            excluded: shadow.excluded,
            sequence: shadow.sequence,
            priority: shadow.priority,
            privacy: shadow.privacy,
            status: shadow.status,
            time_zone: shadow.time_zone,
            created: shadow.created,
            updated: shadow.updated,
            i_calendar: shadow.i_calendar,
            raw: Some(raw),
            etag: shadow.etag,
        })
    }
}

/// JSCalendar properties the gateway requests from `CalendarEvent/get` so a
/// faithful iCalendar document can be reconstructed from the response.
///
/// Stalwart's `iCalendar` property holds only the converted-property mapping
/// (`{"convertedProperties":…,"name":"vevent"}`), NOT the event data, so every
/// standard property that maps to an iCalendar line must be requested
/// explicitly. The names are the JSCalendar (RFC 8984) spellings Stalwart
/// accepts; a name it does not recognize is silently dropped, which would lose
/// data — e.g. the JSCalendar spelling is `showWithoutTime`, not `isAllDay`,
/// and `duration` (with a `dtend` mapping in `convertedProperties`), not `end`.
const CALENDAR_EVENT_PROPERTIES: &[&str] = &[
    "uid",
    "title",
    "description",
    "start",
    "duration",
    "timeZone",
    "showWithoutTime",
    "locations",
    "keywords",
    "status",
    "privacy",
    "freeBusyStatus",
    "priority",
    "sequence",
    "created",
    "updated",
    "recurrenceRule",
    "recurrenceOverrides",
    "organizerCalendarAddress",
    "participants",
    "alerts",
    "links",
    "iCalendar",
    // `calendarIds` is the event's calendar-membership map. A scoped EAS/EWS
    // calendar folder filters a fetch by this property, so it MUST be
    // requested — an omitted property is never returned (RFC 8620 §5.1), which
    // would leave the membership map absent and the scope filter unable to
    // reject another calendar's events.
    "calendarIds",
    "@etag",
];

/// Parse the `list` array of a `CalendarEvent/get` response into events,
/// retaining each raw object so iCalendar can be reconstructed on demand.
///
/// Returns the parsed events and the number of objects that failed to
/// deserialize. Parsing is per element: a single object that fails to
/// deserialize is skipped (and logged) instead of discarding the whole list —
/// the previous whole-list `from_value` turned one unexpected element into zero
/// events. The skip count lets callers avoid advancing sync state past an event
/// they could not process.
fn parse_calendar_event_list(data: &Value) -> (Vec<JmapCalendarEvent>, usize) {
    let Some(list) = data.get("list").and_then(|v| v.as_array()) else {
        return (Vec::new(), 0);
    };
    let mut events = Vec::with_capacity(list.len());
    let mut unparseable = 0usize;
    for item in list {
        match serde_json::from_value::<JmapCalendarEvent>(item.clone()) {
            Ok(event) => events.push(event),
            Err(e) => {
                unparseable += 1;
                warn!(target: "jmap", error = %e, "skipping unparseable CalendarEvent object");
            }
        }
    }
    (events, unparseable)
}

/// First event object from a `CalendarEvent/get` response carried in a batch
/// of JMAP method responses. Used by the write path to read back the event it
/// just created/updated (in the same request, via a `resultOf`/explicit-id
/// `CalendarEvent/get`) and verify the payload was applied.
fn first_calendar_event(response: &JmapResponse) -> Option<Value> {
    response
        .method_responses
        .iter()
        .find(|(m, _, _)| m == "CalendarEvent/get")
        .and_then(|(_, data, _)| data.get("list"))
        .and_then(|list| list.as_array())
        .and_then(|list| list.first())
        .cloned()
}

/// Result of a Calendar/get call.
#[derive(Clone, Debug)]
pub struct CalendarListResult {
    pub calendars: Vec<JmapCalendar>,
    pub total: u64,
}

/// Result of a CalendarEvent/query + CalendarEvent/get call.
#[derive(Clone, Debug)]
pub struct CalendarEventListResult {
    pub events: Vec<JmapCalendarEvent>,
    pub total: u64,
    pub query_state: String,
    /// The `CalendarEvent/get` data-type `state` (draft-ietf-jmap-calendars
    /// §5.7 / RFC 8620 §5.1), captured from the same request. This is the
    /// token `CalendarEvent/changes` accepts as `sinceState`, so it — not the
    /// `CalendarEvent/query` `query_state` — is what the sync path persists
    /// after an initial prime. Empty when the server omitted it.
    pub state: String,
    /// Number of `CalendarEvent/get` objects that failed to deserialize and
    /// were skipped. Callers must not advance sync state while this is non-zero,
    /// or the skipped event would be lost on the next delta.
    pub unparseable: usize,
}

/// A `CalendarEvent/changes` call that the server could not service
/// (RFC 8620 §5.2): the supplied `sinceState` is no longer usable — either
/// the server answered with the `cannotCalculateChanges` error, or it returned
/// no `newState`. The caller must re-prime from a full
/// `CalendarEvent/query` rather than persist a state that would skip changes.
#[derive(Debug, Clone, thiserror::Error)]
#[error("CalendarEvent/changes cannot calculate changes: {0}")]
pub struct CannotCalculateChanges(pub String);

/// Result of a CalendarEvent/changes call.
#[derive(Clone, Debug)]
pub struct CalendarEventChangesResult {
    pub created: Vec<String>,
    pub updated: Vec<String>,
    pub destroyed: Vec<String>,
    pub new_state: String,
    pub has_more_changes: bool,
}

/// Parameters for querying calendar events via JMAP.
pub struct QueryCalendarEventsParams<'a> {
    pub account_id: &'a str,
    pub calendar_id: Option<&'a str>,
    pub start: &'a str,
    pub end: &'a str,
    /// RFC 8620 §5.1 `position`: zero-based offset into the sorted result
    /// set. Non-zero positions page through a result set larger than one
    /// `limit`-sized page; the prime path loops on it until every id is
    /// fetched so a truncated snapshot never advances the delta state.
    pub position: u64,
    pub limit: u64,
    pub username: &'a str,
    pub password: &'a SecretString,
}

/// Parameters for creating/updating a calendar event via JMAP.
pub struct SetCalendarEventParams<'a> {
    pub account_id: &'a str,
    pub ics: &'a str,
    pub event_id: Option<&'a str>,
    pub calendar_id: Option<&'a str>,
    pub username: &'a str,
    pub password: &'a SecretString,
}

/// Error returned by [`JmapClient::set_calendar_event`].
///
/// The [`CalendarWriteError::PayloadDiscarded`] variant is distinct so callers
/// can recognise the "the backend accepted the write but stored an empty
/// event" failure and permanently disable the JMAP calendar write path for the
/// process (a build that discards the payload will do so for every write). A
/// read-back that keeps the content marker but alters another written field is
/// reported as [`CalendarWriteError::Other`] instead — see
/// [`crate::jscalendar::WriteVerificationFailure`].
#[derive(Debug, thiserror::Error)]
pub enum CalendarWriteError {
    /// The server accepted `CalendarEvent/set` but the read-back carried no
    /// content marker (title, or start for a titleless event) — the write path
    /// is lossy on this backend.
    #[error("backend discarded the calendar event payload: {0}")]
    PayloadDiscarded(String),
    /// Any other failure (transport, session, malformed response, a read-back
    /// content mismatch, …).
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

impl CalendarWriteError {
    /// Whether this failure proves the backend's JMAP calendar write path is
    /// lossy (a reason to fall back to CalDAV for the process lifetime).
    pub fn is_payload_discarded(&self) -> bool {
        matches!(self, CalendarWriteError::PayloadDiscarded(_))
    }
}

/// Duration for which a cached JMAP session is considered valid.
/// Per RFC 8621 §2.1, the session state changes when capabilities or
/// accounts change, which is rare. A 5-minute TTL avoids stale sessions
/// while eliminating redundant HTTP GETs per method call.
const SESSION_CACHE_TTL: std::time::Duration = std::time::Duration::from_secs(300);

/// How long a *positive* JMAP credential probe is trusted before the next
/// probe. Kept short so a password changed directly on Stalwart takes effect
/// within this window even before the gateway's own eviction kicks in.
const SESSION_AUTH_TTL: std::time::Duration = std::time::Duration::from_secs(300);

/// How long an *authoritatively rejected* credential probe is remembered,
/// bounding repeated bad-password probes against Stalwart. A successful login
/// for the same user evicts these immediately (`invalidate_other_session_auth`).
const SESSION_AUTH_NEGATIVE_TTL: std::time::Duration = std::time::Duration::from_secs(30);

/// Default capacity of the credential-probe cache when a `JmapClient` is built
/// via [`JmapClient::new`] (the `AppState` path injects the operator's
/// `GATEWAY_AUTH_CACHE_MAX_ENTRIES`). Bounds memory when many distinct
/// credentials are probed (e.g. a username scan that never logs in again).
const DEFAULT_SESSION_AUTH_MAX_ENTRIES: u64 = 10_000;

/// Build the `onSuccessUpdateEmail` patch (RFC 8621 §7.5) that moves a newly
/// submitted message out of every Drafts mailbox, into every Sent mailbox, and
/// clears its `$draft` keyword.
///
/// Returns an error if either mailbox-role list is empty: an empty result would
/// otherwise produce an `Email/set` patch that removes the message from Drafts
/// without adding it anywhere, leaving the Email with zero `mailboxIds`, which
/// RFC 8621 §4.1 forbids. Callers must therefore reject submission when either
/// role cannot be resolved.
fn build_submission_success_patch(
    drafts_ids: &[String],
    sent_ids: &[String],
) -> Result<serde_json::Value> {
    if drafts_ids.is_empty() {
        return Err(anyhow!("Cannot submit: no Drafts mailbox resolved"));
    }
    if sent_ids.is_empty() {
        return Err(anyhow!("Cannot submit: no Sent mailbox resolved"));
    }

    let mut patch = serde_json::Map::new();
    for mb in drafts_ids {
        patch.insert(format!("mailboxIds/{mb}"), json!(null));
    }
    for mb in sent_ids {
        patch.insert(format!("mailboxIds/{mb}"), json!(true));
    }
    patch.insert("keywords/$draft".to_string(), json!(null));
    Ok(serde_json::Value::Object(patch))
}

/// JMAP client for email and calendar operations via Stalwart Mailserver.
#[derive(Clone)]
pub struct JmapClient {
    base_url: String,
    client: reqwest::Client,
    /// Cached sessions keyed by username, with expiry time.
    session_cache: Arc<DashMap<String, (Instant, JmapSession)>>,
    /// Credential-probe cache keyed by `username:sha256(password)`.
    ///
    /// The EAS/EWS front doors authenticate every request; without this, each
    /// one costs a full JMAP `GET /session` round-trip (see AUDIT.md §18). A
    /// positive entry short-circuits that probe for the configured positive
    /// TTL; a negative entry is held for the shorter negative TTL. Backend
    /// outages are never cached. This mirrors the higher-level
    /// `auth::AuthVerifier` cache and, crucially, collapses the burst of
    /// per-request probes fired on a cold start (container restart) into a
    /// single backend round-trip.
    ///
    /// The TTLs and capacity are injected by the owner (`AppState`) so the
    /// client cache can never outlive the verifier cache that governs the
    /// documented `GATEWAY_AUTH_CACHE_TTL_SECS` contract, and so the map is
    /// bounded (moka enforces both the per-entry TTL and `max_capacity`).
    session_auth: Cache<String, SessionAuthEntry>,
    /// Number of `health_check` probes handled by THIS client instance.
    /// Observability hook letting the readiness endpoint's tests prove the
    /// probe goes through the shared §18 client rather than a throwaway
    /// construction (AUDIT.md §20).
    health_probes: Arc<std::sync::atomic::AtomicU64>,
}

/// Cached outcome of a single JMAP credential probe.
#[derive(Clone, Copy, Debug)]
struct SessionAuthEntry {
    valid: bool,
}

/// Per-entry TTL for the JMAP credential-probe cache: positives live for the
/// configured positive TTL, negatives for the shorter negative TTL. Backend
/// errors are never inserted, so they need no lifetime.
#[derive(Clone, Copy)]
struct SessionAuthExpiry {
    positive: std::time::Duration,
    negative: std::time::Duration,
}

impl Expiry<String, SessionAuthEntry> for SessionAuthExpiry {
    fn expire_after_create(
        &self,
        _key: &String,
        value: &SessionAuthEntry,
        _created_at: Instant,
    ) -> Option<std::time::Duration> {
        Some(if value.valid {
            self.positive
        } else {
            self.negative
        })
    }
}

/// Outcome of a JMAP `Email/set` update, surfaced by
/// [`JmapClient::update_email_checked`]. Carries the ids the server reports
/// as `updated`, the (`id`, `description`) pairs it rejected with
/// `notUpdated`, and any method-level `error` rejection. Lets the MAPI
/// property-write handlers distinguish a real success from a silent
/// server-side rejection instead of masking failure as success
/// (qodo #3/#5, cubic #23).
#[derive(Debug, Clone, Default)]
pub struct EmailSetOutcome {
    /// Ids the server accepted in the `updated` array.
    pub updated: Vec<String>,
    /// (`id`, `description`) pairs the server refused to update
    /// (the `notUpdated` map, RFC 8621 §4.5).
    pub not_updated: Vec<(String, String)>,
    /// Non-empty when the server rejected the whole method (e.g. rate limit,
    /// `accountNotFound`). `None` means no method-level error.
    pub method_error: Option<String>,
}

impl JmapClient {
    /// Standard Email/get property list (RFC 8621 §4.1.3) covering everything
    /// the gateway needs to render EWS/EAS: the metadata used by SyncFolderItems
    /// / FindItem / Sync plus the richer fields (`attachments`, `header:raw`,
    /// `bodyStructure`) required for full-fidelity GetItem rendering.
    fn mail_properties() -> Vec<&'static str> {
        vec![
            "id",
            "blobId",
            "threadId",
            "mailboxIds",
            "keywords",
            "size",
            "receivedAt",
            "sentAt",
            "hasAttachment",
            "from",
            "sender",
            "to",
            "cc",
            "bcc",
            "replyTo",
            "subject",
            "preview",
            "bodyValues",
            "textBody",
            "htmlBody",
            "attachments",
            "bodyStructure",
            "messageId",
            "inReplyTo",
            "references",
            "header:raw",
        ]
    }
    /// Create a new JMAP client pointing at the Stalwart JMAP endpoint.
    pub fn new(base_url: &str) -> Result<Self> {
        Self::with_session_auth(
            base_url,
            SESSION_AUTH_TTL,
            SESSION_AUTH_NEGATIVE_TTL,
            DEFAULT_SESSION_AUTH_MAX_ENTRIES,
        )
    }

    /// Create a JMAP client whose credential-probe cache uses the given
    /// positive/negative TTLs and capacity.
    ///
    /// `AppState` derives `positive_ttl` from the verifier's
    /// `auth_cache_ttl_secs` so the client cache can never extend the
    /// documented auth-cache contract (AUDIT.md §18), and caps the cache size
    /// so a scan of usernames that never log in again cannot grow it without
    /// bound.
    pub fn with_session_auth(
        base_url: &str,
        positive_ttl: std::time::Duration,
        negative_ttl: std::time::Duration,
        max_entries: u64,
    ) -> Result<Self> {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .map_err(|e| anyhow!("Failed to create JMAP HTTP client: {}", e))?;

        Ok(Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            client,
            session_cache: Arc::new(DashMap::new()),
            session_auth: Cache::builder()
                .max_capacity(max_entries.max(1))
                .expire_after(SessionAuthExpiry {
                    positive: positive_ttl,
                    negative: negative_ttl,
                })
                .build(),
            health_probes: Arc::new(std::sync::atomic::AtomicU64::new(0)),
        })
    }

    /// Get the base URL for this JMAP client.
    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    /// How many `health_check` probes this client instance has handled.
    pub fn health_probe_count(&self) -> u64 {
        self.health_probes
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Derive the JMAP base URL from the CalDAV base URL.
    ///
    /// Stalwart serves JMAP at the same host as CalDAV, replacing
    /// the /dav/ path with /jmap/. For example:
    ///   http://stalwart:8080/dav → http://stalwart:8080/jmap
    pub fn derive_from_caldav(caldav_base: &str) -> String {
        let url = caldav_base.trim_end_matches('/');
        // Replace /dav suffix with /jmap
        if let Some(idx) = url.rfind("/dav") {
            format!("{}/jmap", &url[..idx])
        } else {
            // Fallback: append /jmap
            format!("{}/jmap", url)
        }
    }

    /// Build the Basic Authorization header value.
    pub(crate) fn basic_auth_header(username: &str, password: &SecretString) -> String {
        use base64::Engine;
        let encoded = base64::engine::general_purpose::STANDARD.encode(format!(
            "{}:{}",
            username,
            password.expose_secret()
        ));
        format!("Basic {}", encoded)
    }

    /// Swap the scheme+host[+port] of a JMAP URL template (e.g. `downloadUrl`
    /// or `uploadUrl`) with the configured internal `base_url`, preserving the
    /// path, query, and `{accountId}`/`{blobId}` placeholders.
    ///
    /// Returns `None` if `template` is not a valid URL with a host part.
    fn internalize_template(template: &str, base_url: &str) -> Option<String> {
        // Find where the path begins (3rd '/' after the scheme, or first '/' overall).
        let scheme_end = template.find("://")?;
        let after_scheme = &template[scheme_end + 3..];
        let path_start = after_scheme.find('/').map(|p| scheme_end + 3 + p)?;
        let template_path = &template[path_start..];
        let base = base_url.trim_end_matches('/');
        Some(format!("{base}{template_path}"))
    }

    /// Build the download URL for a blob, validating that `blob_id` conforms
    /// to the RFC 8620 `Id` character set (URL-safe base64 alphabet without
    /// padding, `A-Za-z0-9_-`). `blob_id` reaches the handler from the
    /// client-supplied EAS `FileReference`; rejecting anything outside the
    /// allowed alphabet prevents `/`, `?`, `..` etc. from altering the
    /// request path or query of the URL constructed from `downloadUrl`.
    fn download_blob_url(
        &self,
        session: &JmapSession,
        account_id: &str,
        blob_id: &str,
    ) -> Result<String> {
        if blob_id.is_empty()
            || !blob_id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        {
            return Err(anyhow!("Invalid JMAP blobId: {:?}", blob_id));
        }
        let template = if session.download_url.is_empty() {
            format!(
                "{}/download/{{accountId}}/{{blobId}}",
                self.base_url.trim_end_matches('/')
            )
        } else {
            session.download_url.clone()
        };
        Ok(template
            .replace("{accountId}", account_id)
            .replace("{blobId}", blob_id))
    }

    /// Download a raw blob by `blobId` (RFC 8621 §4.1.3 `downloadUrl`).
    ///
    /// The session `downloadUrl` template contains `{accountId}` and
    /// `{blobId}` placeholders; Stalwart expects the path
    /// `/download/{accountId}/{blobId}` with optional `?name=`/`?type=`.
    /// Returns the raw bytes of the attachment (or the full MIME body when
    /// `blobId` is the email's own `blobId`).
    pub async fn download_blob(
        &self,
        account_id: &str,
        blob_id: &str,
        username: &str,
        password: &SecretString,
    ) -> Result<Vec<u8>> {
        let session = self.get_session(username, password).await?;
        let url = self.download_blob_url(&session, account_id, blob_id)?;
        let auth = Self::basic_auth_header(username, password);

        trace!(target: "jmap", url = %url, blob_id = %blob_id, "Downloading JMAP blob");

        let resp = self
            .client
            .get(&url)
            .header(AUTHORIZATION, &auth)
            .send()
            .await
            .map_err(|e| anyhow!("JMAP blob download failed: {}", e))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(anyhow!("JMAP blob download returned {}: {}", status, body));
        }

        Ok(resp
            .bytes()
            .await
            .map_err(|e| anyhow!("JMAP blob download body read failed: {}", e))?
            .to_vec())
    }

    /// Download a blob like [`Self::download_blob`], but with a hard byte cap
    /// enforced *while streaming* the body.
    ///
    /// `download_blob` buffers the whole response via `resp.bytes()`; with a
    /// raised attachment size cap that would let an oversized blob be pulled
    /// fully into memory before any limit check runs. This variant instead
    /// consumes `bytes_stream()` chunk-by-chunk and aborts the transfer as
    /// soon as the accumulated bytes exceed `max_bytes`. The `Content-Length`
    /// header (when present) is checked up front so oversized blobs are
    /// rejected before the first body byte is transferred.
    ///
    /// Returns the blob bytes together with the `Content-Type` reported by the
    /// server (defaulting to `application/octet-stream` when absent), so
    /// callers can propagate it to EAS `AirSyncBase:ContentType`.
    pub async fn download_blob_capped(
        &self,
        account_id: &str,
        blob_id: &str,
        username: &str,
        password: &SecretString,
        max_bytes: usize,
    ) -> Result<(Vec<u8>, String)> {
        let session = self.get_session(username, password).await?;
        let url = self.download_blob_url(&session, account_id, blob_id)?;
        let auth = Self::basic_auth_header(username, password);

        trace!(target: "jmap", url = %url, blob_id = %blob_id, max_bytes, "Downloading JMAP blob (capped, streaming)");

        let resp = self
            .client
            .get(&url)
            .header(AUTHORIZATION, &auth)
            .send()
            .await
            .map_err(|e| anyhow!("JMAP blob download failed: {}", e))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(anyhow!("JMAP blob download returned {}: {}", status, body));
        }

        let content_type = resp
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned)
            .unwrap_or_else(|| "application/octet-stream".to_string());

        if let Some(len) = resp.content_length()
            && len > max_bytes as u64
        {
            return Err(anyhow!(
                "JMAP blob too large: Content-Length {} exceeds max {} bytes",
                len,
                max_bytes
            ));
        }

        let mut buf = Vec::with_capacity(
            usize::try_from(resp.content_length().unwrap_or(0).min(max_bytes as u64))
                .unwrap_or(max_bytes),
        );
        let mut stream = resp.bytes_stream();
        while let Some(chunk) = futures_util::StreamExt::next(&mut stream).await {
            let chunk = chunk.map_err(|e| anyhow!("JMAP blob download stream failed: {}", e))?;
            if buf.len() + chunk.len() > max_bytes {
                return Err(anyhow!(
                    "JMAP blob download aborted: size exceeds max {} bytes",
                    max_bytes
                ));
            }
            buf.extend_from_slice(&chunk);
        }

        Ok((buf, content_type))
    }
    /// Download a blob for an EAS ItemOperations Fetch
    /// ([MS-ASCMD] §2.2.1.10), honoring an optional client byte-range
    /// ([MS-ASCMD] §2.2.3.143.2: "For attachments, the range applies to the
    /// file content").
    ///
    /// Unlike [`Self::download_blob_capped`], which buffers the whole body,
    /// this variant only *retains* the bytes inside the requested window:
    /// bytes before the window are counted and discarded, and the transfer
    /// is aborted as soon as the last wanted byte has been received whenever
    /// the total size is already known from `Content-Length`. The window
    /// buffer is additionally bounded by `max_bytes`, which maps the
    /// gateway's per-request attachment budget onto the spec's checkpoint
    /// mechanism ("Facilitates a checkpoint to improve the reliability of
    /// large data downloads", [MS-ASCMD] §2.2.3.125.3): a client can pull an
    /// attachment larger than the budget in consecutive window requests.
    ///
    /// `range` is a half-open `[start, end)` window in absolute blob
    /// coordinates. `None` fetches the whole item, subject to `max_bytes`.
    ///
    /// The returned `total_size` is the whole blob's size when the server
    /// advertises it via `Content-Length` or the transfer reached EOF while
    /// scanning; it is `None` only when a windowed transfer was cut short
    /// before EOF with no `Content-Length` available — in that case the
    /// response omits the optional `Total` element rather than reporting a
    /// wrong value ([MS-ASCMD] §2.2.3.184.2: Total "indicates the total size
    /// of an item on the server").
    pub async fn download_blob_range(
        &self,
        account_id: &str,
        blob_id: &str,
        username: &str,
        password: &SecretString,
        range: Option<std::ops::Range<u64>>,
        max_bytes: usize,
    ) -> std::result::Result<BlobRangeFetch, BlobFetchError> {
        if blob_id.is_empty()
            || !blob_id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        {
            // [MS-ASCMD] §2.2.3.177.8 status 15: "Attachment fetch provider -
            // Attachment or attachment ID is invalid."
            return Err(BlobFetchError::InvalidBlobId);
        }
        let range = range.filter(|r| r.start < r.end);
        let window_len = match &range {
            Some(r) => {
                let len = r.end - r.start;
                if len > max_bytes as u64 {
                    // Status 8: "The byte-range is invalid or too large."
                    return Err(BlobFetchError::RangeTooLarge {
                        requested: len,
                        limit: max_bytes,
                    });
                }
                len
            }
            None => 0,
        };

        let session = self
            .get_session(username, password)
            .await
            .map_err(BlobFetchError::Server)?;
        let url = self
            .download_blob_url(&session, account_id, blob_id)
            .map_err(|_| BlobFetchError::InvalidBlobId)?;
        let auth = Self::basic_auth_header(username, password);

        trace!(target: "jmap", url = %url, blob_id = %blob_id, ?range, max_bytes, "Downloading JMAP blob (windowed)");

        let resp = self
            .client
            .get(&url)
            .header(AUTHORIZATION, &auth)
            .send()
            .await
            .map_err(|e| BlobFetchError::Server(anyhow!("JMAP blob download failed: {}", e)))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(match status.as_u16() {
                401 | 403 => BlobFetchError::AccessDenied(format!(
                    "JMAP blob download returned {}: {}",
                    status, body
                )),
                404 | 410 => BlobFetchError::NotFound(format!(
                    "JMAP blob download returned {}: {}",
                    status, body
                )),
                _ => BlobFetchError::Server(anyhow!(
                    "JMAP blob download returned {}: {}",
                    status,
                    body
                )),
            });
        }

        let content_type = resp
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned)
            .unwrap_or_else(|| "application/octet-stream".to_string());

        let content_length = resp.content_length();
        if let (Some(len), None) = (content_length, &range)
            && len > max_bytes as u64
        {
            // [MS-ASCMD] §2.2.3.177.8 status 11: "The requested data size is
            // too large." The client can retry with explicit windowed ranges.
            return Err(BlobFetchError::TooLarge {
                total: len,
                limit: max_bytes,
            });
        }
        if let (Some(len), Some(r)) = (content_length, &range) {
            // A window entirely past EOF has no bytes to serve: status 8
            // ("The byte-range is invalid or too large").
            if r.start >= len {
                return Err(BlobFetchError::RangeStartsPastEof { total: len });
            }
        }

        // The transfer needs to continue past the window's last byte only
        // while the total size is still unknown; with `Content-Length`
        // present the stream is cut as soon as the window is complete. The
        // declared length travels with the stop position so the early return
        // names the authoritative total without re-reading headers.
        let stop_after = match (&range, content_length) {
            (Some(r), Some(total)) => Some((r.end, total)),
            _ => None,
        };

        let mut data: Vec<u8> = Vec::with_capacity(usize::try_from(window_len).unwrap_or(0));
        let mut position: u64 = 0;
        let mut stream = resp.bytes_stream();
        while let Some(chunk) = futures_util::StreamExt::next(&mut stream).await {
            let chunk = chunk.map_err(|e| {
                BlobFetchError::Server(anyhow!("JMAP blob download stream failed: {}", e))
            })?;
            let chunk_start = position;
            let chunk_end = position + chunk.len() as u64;
            match &range {
                // One chunk can straddle the window boundaries: copy only
                // the intersection of [chunk_start, chunk_end) with the
                // requested window.
                Some(want) => {
                    let copy_from = chunk_start.max(want.start).saturating_sub(chunk_start);
                    let copy_to = chunk_end.min(want.end).saturating_sub(chunk_start);
                    if copy_to > copy_from {
                        data.extend_from_slice(&chunk[copy_from as usize..copy_to as usize]);
                    }
                }
                None => {
                    data.extend_from_slice(&chunk);
                    if data.len() > max_bytes {
                        return Err(BlobFetchError::TooLarge {
                            total: chunk_end,
                            limit: max_bytes,
                        });
                    }
                }
            }
            position = chunk_end;
            if let Some((stop, total)) = stop_after
                && position >= stop
            {
                // The window is fully covered and the total is known from
                // Content-Length — nothing more to learn from the stream.
                let (start, end) = range
                    .as_ref()
                    .map(|r| (r.start, r.end))
                    .expect("stop implies a range");
                return Ok(BlobRangeFetch {
                    data,
                    content_type,
                    total_size: Some(total),
                    window: (start, end),
                    complete: true,
                });
            }
        }
        // EOF: the whole body has been seen, so the total is exact — the
        // declared Content-Length when the server sent one, otherwise the
        // bytes actually received. A windowed request that hit EOF before
        // covering its full span served fewer bytes than requested
        // ([MS-ASCMD] §2.2.3.177.8 status 17: "Partial success").
        let total_size = content_length.unwrap_or(position);
        if let Some(r) = &range
            && r.start >= position
        {
            // The stream ended before the window's first byte, so no byte of
            // the window exists — the same semantics the Content-Length
            // precheck applies ([MS-ASCMD] §2.2.3.177.8 status 8: "The
            // byte-range is invalid or too large"). Without this, a streamed
            // body would build an inverted (start > end) window that renders
            // as a nonsensical Range and underflows the inclusive-end
            // conversion.
            return Err(BlobFetchError::RangeStartsPastEof { total: position });
        }
        let (window, complete) = match &range {
            Some(r) => ((r.start, position.min(r.end)), position >= r.end),
            None => ((0, position), true),
        };
        Ok(BlobRangeFetch {
            data,
            content_type,
            total_size: Some(total_size),
            window,
            complete,
        })
    }

    /// Upload a raw blob to JMAP (RFC 8621 §4.1.2 `uploadUrl`).
    ///
    /// Uploads raw bytes as a JMAP blob and returns the assigned `blobId`.
    /// The caller typically then uses `Email/set` with `create` or `update`
    /// to associate the blob with an email (e.g. as `textBody`, `htmlBody`,
    /// or an attachment via `blobId` in the `blobId` property).
    /// The `name` parameter is optional; if provided it sets the suggested
    /// filename for the blob.
    ///
    /// Implementation note: Per RFC 8621 and RFC 9404, blob upload is performed
    /// by POSTing raw bytes to the session's `uploadUrl` (with `{accountId}`
    /// substituted), not via a JSON `Blob/upload` method call. The `blobId` is
    /// derived from the response (typically from a `Location` header or body).
    pub async fn upload_blob(
        &self,
        account_id: &str,
        data: &[u8],
        _name: Option<&str>,
        username: &str,
        password: &SecretString,
    ) -> Result<String> {
        let session = self.get_session(username, password).await?;
        // RFC 8620 §3.2: the session uploadUrl is a URI Template containing
        // {accountId}; substitute it. Fall back to the legacy path layout if
        // the template is absent.
        let url = {
            let template = session.upload_url.trim();
            if template.is_empty() {
                format!(
                    "{}/upload/{}/",
                    self.base_url.trim_end_matches('/'),
                    urlencoding::encode(account_id)
                )
            } else if template.contains("{accountId}") {
                template.replace("{accountId}", &urlencoding::encode(account_id))
            } else {
                template
                    .parse::<url::Url>()
                    .map(|_| template.to_string())
                    .map_err(|_| anyhow!("Invalid JMAP upload URL template: {}", template))?
            }
        };
        // Credentials must never be sent to a host other than the configured
        // JMAP base: the session-advertised uploadUrl is pinned to the same
        // origin (scheme/host/port) as the configured base URL.
        fn same_origin(a: &str, b: &str) -> Result<()> {
            let pa = url::Url::parse(a)?;
            let pb = url::Url::parse(b)?;
            if pa.scheme() == pb.scheme()
                && pa.host_str() == pb.host_str()
                && pa.port_or_known_default() == pb.port_or_known_default()
            {
                Ok(())
            } else {
                Err(anyhow!(
                    "Refusing to send credentials to JMAP upload URL on a different origin: {} (base: {})",
                    pa.origin().ascii_serialization(),
                    pb.origin().ascii_serialization()
                ))
            }
        }
        same_origin(&url, self.base_url.trim_end_matches('/'))?;
        let auth = Self::basic_auth_header(username, password);

        // Per RFC 8621, upload the raw bytes directly
        let resp = self
            .client
            .post(&url)
            .body(data.to_vec())
            .header("Content-Type", "application/octet-stream")
            .header(AUTHORIZATION, &auth)
            .send()
            .await
            .map_err(|e| anyhow!("JMAP blob upload request failed: {}", e))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(anyhow!("JMAP blob upload returned {}: {}", status, body));
        }

        // Derive blobId from response - JMAP typically returns it in the body
        // or via a Location header. For Stalwart, we expect the blobId in the
        // response body as a JSON object or plain string.
        let response_body = resp.text().await.unwrap_or_default();
        let blob_id = if let Some(id) = response_body
            .strip_prefix('"')
            .and_then(|s| s.strip_suffix('"'))
        {
            id.to_string()
        } else if let Some(id) = response_body
            .strip_prefix("{")
            .and_then(|s| s.strip_suffix("}"))
        {
            // If JSON, try to extract blobId field
            serde_json::from_str::<serde_json::Value>(id)
                .ok()
                .and_then(|v| {
                    v.get("blobId")
                        .and_then(|blob| blob.as_str())
                        .map(str::to_string)
                })
                .unwrap_or_else(|| id.to_string())
        } else {
            response_body
        };

        if blob_id.is_empty() {
            return Err(anyhow!("JMAP blob upload returned empty blobId"));
        }

        Ok(blob_id)
    }

    /// Fetch the JMAP session object (RFC 8621 §2.1).
    ///
    /// The session provides the API URL, account IDs, and capabilities.
    /// Results are cached per-username for `SESSION_CACHE_TTL` (5 minutes)
    /// to avoid redundant HTTP GETs on every method call.
    pub async fn get_session(
        &self,
        username: &str,
        password: &SecretString,
    ) -> Result<JmapSession> {
        // Check the cache first — return if not expired.
        if let Some(entry) = self.session_cache.get(username)
            && entry.key().as_str() == username
            && entry.value().0 > Instant::now()
        {
            return Ok(entry.value().1.clone());
        }

        let url = format!("{}/session", self.base_url);
        let auth = Self::basic_auth_header(username, password);

        trace!(target: "jmap", url = %url, "Fetching JMAP session");

        let resp = self
            .client
            .get(&url)
            .header(AUTHORIZATION, &auth)
            .send()
            .await
            .map_err(|e| anyhow!("JMAP session request failed: {}", e))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(anyhow!(
                "JMAP session request returned {}: {}",
                status,
                body
            ));
        }

        let mut session: JmapSession = resp
            .json()
            .await
            .map_err(|e| anyhow!("Failed to parse JMAP session: {}", e))?;
        // Override the API URL with the configured base URL.
        // Stalwart's session returns the external apiUrl (e.g.
        // https://stalwart.example.com/jmap/) but the gateway should use its
        // configured internal base_url (e.g. http://stalwart:8080/jmap) for
        // API calls. This ensures traffic stays within the Docker network and
        // avoids errors when the external URL routes through a reverse proxy
        // that may modify or reject JMAP POST requests.
        if session.api_url != self.base_url {
            debug!(
                target: "jmap",
                session_api_url = %session.api_url,
                configured_base_url = %self.base_url,
                "Overriding session apiUrl with configured base_url for internal routing"
            );
            session.api_url = self.base_url.clone();
        }

        // Likewise override the download URL host with the configured internal
        // base_url so attachment/blob downloads (RFC 8621 §4.1.3 `downloadUrl`,
        // `Email/get` `attachments[].blobId`) stay on the Docker network. The
        // session `downloadUrl` template has the shape
        //   https://stalwart.example.com/jmap/download/{accountId}/{blobId}[?type=...&name=...]
        // We only swap the scheme+host+port with the configured base, keeping
        // the path and (encoded) query intact.
        if let Some(internal) = Self::internalize_template(&session.download_url, &self.base_url)
            && internal != session.download_url
        {
            debug!(
                target: "jmap",
                session_download_url = %session.download_url,
                "Overriding session downloadUrl with internal base for blob downloads"
            );
            session.download_url = internal;
        }

        // Likewise pin the EventSource push URL (RFC 8620 §7.3 `eventSourceUrl`)
        // to the configured internal `base_url`. The push monitor attaches the
        // mailbox's Basic `Authorization` header to this URL, so it must never
        // resolve to the server-advertised external host — that would transmit
        // the credential to an out-of-network address (CWE-319 cleartext
        // transmission). Only swap the scheme+host[+port]; the path/query
        // (`/jmap/eventsource?...`) is preserved verbatim.
        if !session.event_source_url.is_empty()
            && let Some(internal) =
                Self::internalize_template(&session.event_source_url, &self.base_url)
            && internal != session.event_source_url
        {
            debug!(
                target: "jmap",
                session_event_source_url = %session.event_source_url,
                "Overriding session eventSourceUrl with internal base for push"
            );
            session.event_source_url = internal;
        }

        // Cache the session with expiry
        let expires = Instant::now() + SESSION_CACHE_TTL;
        self.session_cache
            .insert(username.to_string(), (expires, session.clone()));

        Ok(session)
    }

    /// Make a JMAP API call (RFC 8621 §3.2).
    pub async fn api_call(
        &self,
        api_url: &str,
        using: &[&str],
        method_calls: Vec<(&str, Value, &str)>,
        username: &str,
        password: &SecretString,
    ) -> Result<JmapResponse> {
        let auth = Self::basic_auth_header(username, password);

        let calls: Vec<JmapMethodCall> = method_calls
            .into_iter()
            .map(|(name, arguments, id)| JmapMethodCall {
                name: name.to_string(),
                arguments,
                id: id.to_string(),
            })
            .collect();

        let request = JmapRequest {
            using: using.iter().map(|s| s.to_string()).collect(),
            method_calls: calls,
        };

        let body = serde_json::to_string(&request)
            .map_err(|e| anyhow!("Failed to serialize JMAP request: {}", e))?;

        trace!(target: "jmap", api_url = %api_url, body = %body, "JMAP API call");

        let resp = self
            .client
            .post(api_url)
            .header(AUTHORIZATION, &auth)
            .header(CONTENT_TYPE, "application/json")
            .body(body)
            .send()
            .await
            .map_err(|e| anyhow!("JMAP API call failed: {}", e))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(anyhow!("JMAP API call returned {}: {}", status, body));
        }

        resp.json::<JmapResponse>()
            .await
            .map_err(|e| anyhow!("Failed to parse JMAP response: {}", e))
    }

    /// Query emails in a mailbox.
    ///
    /// Maps to `Email/query` (RFC 8621 §4.3). Batches Email/query and
    /// Email/get in a single JMAP request using back-references (RFC 8621 §3.6)
    /// to avoid two separate network round-trips.
    pub async fn query_emails(&self, params: QueryEmailsParams<'_>) -> Result<EmailListResult> {
        let session = self.get_session(params.username, params.password).await?;
        let api_url = &session.api_url;

        let filter_val = params.filter.unwrap_or_else(|| json!({}));
        let sort_val = params
            .sort
            .unwrap_or_else(|| vec![json!({"property": "receivedAt", "isAscending": false})]);

        let properties = json!(Self::mail_properties());

        // RFC 8620 §5.5: an `anchor` (with `anchorOffset`) pins the window to
        // an item instead of a numeric offset; `position` is then ignored by
        // the server. A `null` anchor is the server default and must not be
        // sent, and `anchorOffset` is only meaningful alongside an anchor.
        let mut query_args = json!({
            "accountId": params.account_id,
            "filter": filter_val,
            "sort": sort_val,
            "position": params.position,
            "limit": params.limit,
            "calculateTotal": true,
        });
        if let Some(anchor) = params.anchor {
            query_args["anchor"] = json!(anchor);
            query_args["anchorOffset"] = json!(params.anchor_offset);
        }

        // Batch Email/query + Email/get in a single request (RFC 8621 §3.6).
        // The Email/get ids are a back-reference to Email/query's /ids result.
        let method_calls = vec![
            ("Email/query", query_args, "q0"),
            (
                "Email/get",
                json!({
                    "accountId": params.account_id,
                    "#ids": {
                        "resultOf": "q0",
                        "name": "Email/query",
                        "path": "/ids",
                    },
                    "properties": properties,
                    "bodyProperties": ["partId", "blobId", "size", "type", "charset", "value"],
                    // RFC 8621 §4.4.1: without these arguments the server only
                    // returns body values for text/plain parts. Requesting both
                    // text and HTML values lets the gateway serve Type 2 (HTML)
                    // bodies natively for clients whose BodyPreference chain
                    // starts with HTML — Outlook for Android and New Outlook
                    // both request HTML first.
                    "fetchTextBodyValues": true,
                    "fetchHTMLBodyValues": true,
                }),
                "g0",
            ),
        ];

        let response = self
            .api_call(
                api_url,
                &["urn:ietf:params:jmap:core", "urn:ietf:params:jmap:mail"],
                method_calls,
                params.username,
                params.password,
            )
            .await?;

        let mut query_state = String::new();
        let mut total: u64 = 0;
        let mut can_calc = false;
        let mut emails: Vec<JmapEmail> = Vec::new();
        let mut state = String::new();
        let mut method_errors: Vec<Value> = Vec::new();
        let mut query_position: u64 = 0;

        for (method, data, _) in response.method_responses {
            if method == "Email/query" {
                total = data.get("total").and_then(|v| v.as_u64()).unwrap_or(0);
                can_calc = data
                    .get("canCalculateChanges")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                query_state = data
                    .get("queryState")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                // RFC 8620 §5.5: the authoritative absolute index of the first
                // returned id. Absent only on a malformed response; 0 is then a
                // safe floor.
                query_position = data.get("position").and_then(|v| v.as_u64()).unwrap_or(0);
            } else if method == "Email/get" {
                emails = data
                    .get("list")
                    .and_then(|v| serde_json::from_value(v.clone()).ok())
                    .unwrap_or_default();
                state = data
                    .get("state")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
            } else if method == "error" {
                method_errors.push(data);
            }
        }

        // RFC 8620 §5.5: an `anchor` no longer in the result set is a distinct,
        // recoverable condition — the caller falls back to numeric `position`.
        // A failed `q0` also fails the `Email/get` that back-references it
        // (RFC 8620 §3.7), so when the anchor is the cause BOTH calls carry an
        // error; that cascade must not mask the recoverable case. Any other
        // method error still propagates.
        let anchor_not_found = method_errors
            .iter()
            .any(|e| e.get("type").and_then(|v| v.as_str()) == Some("anchorNotFound"));
        if !anchor_not_found && let Some(err) = method_errors.into_iter().next() {
            return Err(anyhow!("Email/query returned method error: {}", err));
        }

        Ok(EmailListResult {
            emails,
            total,
            can_calculate_changes: can_calc,
            query_state,
            state,
            anchor_not_found,
            query_position,
        })
    }

    /// Query only the message ids in a mailbox (no `Email/get`).
    ///
    /// Maps to `Email/query` (RFC 8621 §4.3) and returns the bare id list.
    /// This is the cheap sibling of [`JmapClient::query_emails`] for callers
    /// that only need to reverse-map a MAPI mid to a JMAP id: it avoids
    /// downloading `mail_properties()`, `bodyStructure`, and `bodyValues` for
    /// every page, which `query_emails` batches in.
    pub async fn query_email_ids(&self, params: QueryEmailsParams<'_>) -> Result<Vec<String>> {
        let session = self.get_session(params.username, params.password).await?;
        let api_url = &session.api_url;

        let filter_val = params.filter.unwrap_or_else(|| json!({}));
        let sort_val = params
            .sort
            .unwrap_or_else(|| vec![json!({"property": "receivedAt", "isAscending": false})]);

        let method_calls = vec![(
            "Email/query",
            {
                let mut args = json!({
                    "accountId": params.account_id,
                    "filter": filter_val,
                    "sort": sort_val,
                    "position": params.position,
                    "limit": params.limit,
                    "calculateTotal": false,
                });
                if let Some(anchor) = params.anchor {
                    args["anchor"] = json!(anchor);
                    args["anchorOffset"] = json!(params.anchor_offset);
                }
                args
            },
            "q0",
        )];

        let response = self
            .api_call(
                api_url,
                &["urn:ietf:params:jmap:core", "urn:ietf:params:jmap:mail"],
                method_calls,
                params.username,
                params.password,
            )
            .await?;

        for (method, data, _) in response.method_responses {
            if method == "Email/query" {
                let ids = data
                    .get("ids")
                    .and_then(|v| v.as_array())
                    .map(|a| {
                        a.iter()
                            .filter_map(|x| x.as_str().map(String::from))
                            .collect()
                    })
                    .unwrap_or_default();
                return Ok(ids);
            }
        }

        Ok(Vec::new())
    }

    /// Get specific emails by ID.
    ///
    /// Maps to `Email/get` (RFC 8621 §4.1). `fetch_bodies` opts in to the
    /// RFC 8621 §4.4.1 `fetchTextBodyValues`/`fetchHTMLBodyValues` flags;
    /// only body-rendering callers (Sync change rendering, ItemOperations
    /// Fetch, EWS GetItem/SyncFolderItems) should pass `true` — a caller
    /// that reads only metadata (`mailboxIds`, `references`, `blobId`, …)
    /// would otherwise download full HTML/plain bodies it never uses.
    pub async fn get_emails(
        &self,
        account_id: &str,
        ids: &[String],
        properties: Option<Value>,
        username: &str,
        password: &SecretString,
        fetch_bodies: bool,
    ) -> Result<Vec<JmapEmail>> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let session = self.get_session(username, password).await?;
        let api_url = &session.api_url;

        let props = properties.unwrap_or_else(|| json!(Self::mail_properties()));

        // RFC 8620 §2: `Email/get` with an explicit `ids` array is bounded by
        // the session's `maxObjectsInGet`. An id list over the cap is rejected
        // whole with `requestTooLarge`, so it must be split into conforming
        // batches. Callers such as the push monitor accumulate an unbounded
        // union of changed ids, which routinely exceeds the cap on a busy
        // mailbox.
        let max = session_max_objects_in_get(&session);

        let mut emails = Vec::with_capacity(ids.len());
        for chunk in chunk_ids(ids, max) {
            let mut arguments = json!({
                "accountId": account_id,
                "ids": chunk,
                "properties": props,
                "bodyProperties": ["partId", "blobId", "size", "type", "charset", "value"],
            });
            if fetch_bodies {
                // RFC 8621 §4.4.1: without these arguments the server returns
                // no body values at all — delta Sync and ItemOperations, which
                // fetch through this method rather than query_emails, would
                // render empty bodies. Both flags mirror query_emails so
                // Type 1 (plain) and Type 2 (HTML) bodies resolve natively.
                arguments["fetchTextBodyValues"] = json!(true);
                arguments["fetchHTMLBodyValues"] = json!(true);
            }
            let method_calls = vec![("Email/get", arguments, "g0")];

            let response = self
                .api_call(
                    api_url,
                    &["urn:ietf:params:jmap:core", "urn:ietf:params:jmap:mail"],
                    method_calls,
                    username,
                    password,
                )
                .await?;

            let mut found = false;
            for (method, data, _) in response.method_responses {
                if method == "Email/get" {
                    found = true;
                    let list: Vec<JmapEmail> = data
                        .get("list")
                        .and_then(|v| serde_json::from_value(v.clone()).ok())
                        .unwrap_or_default();
                    emails.extend(list);
                }
            }
            if !found {
                return Err(anyhow!("Unexpected JMAP response structure for Email/get"));
            }
        }

        Ok(emails)
    }

    /// Get a single email by ID. `fetch_bodies` selects the RFC 8621 §4.4.1
    /// body-value fetch flags; pass `false` for metadata-only reads
    /// (threading headers, `blobId`, `mailboxIds`).
    pub async fn get_email(
        &self,
        account_id: &str,
        email_id: &str,
        username: &str,
        password: &SecretString,
        fetch_bodies: bool,
    ) -> Result<Option<JmapEmail>> {
        let emails = self
            .get_emails(
                account_id,
                &[email_id.to_string()],
                Some(json!(Self::mail_properties())),
                username,
                password,
                fetch_bodies,
            )
            .await?;
        Ok(emails.into_iter().next())
    }

    /// Sync email changes since a given state token.
    ///
    /// Maps to `Email/changes` (RFC 8621 §4.4).
    ///
    /// `max_changes` bounds the server's `maxChanges` property so callers
    /// that paginate (the EAS Sync path, which must obey the client's
    /// `WindowSize`) get bounded batches; `None` keeps the historical
    /// default of 500. The result's `has_more_changes` reflects the JMAP
    /// `hasMoreChanges` property: when true, more changes exist beyond
    /// `new_state` and the caller can chain another call with
    /// `sinceState = new_state` until it drains.
    pub async fn sync_email_changes(
        &self,
        account_id: &str,
        old_state: &str,
        username: &str,
        password: &SecretString,
        max_changes: Option<u32>,
    ) -> Result<EmailChangesResult> {
        let session = self.get_session(username, password).await?;
        let api_url = &session.api_url;

        let method_calls = vec![(
            "Email/changes",
            json!({
                "accountId": account_id,
                "sinceState": old_state,
                "maxChanges": max_changes.unwrap_or(crate::jmap::DEFAULT_EMAIL_CHANGES_MAX),
            }),
            "c0",
        )];

        let response = self
            .api_call(
                api_url,
                &["urn:ietf:params:jmap:core", "urn:ietf:params:jmap:mail"],
                method_calls,
                username,
                password,
            )
            .await?;

        for (method, data, _) in response.method_responses {
            if method == "Email/changes" {
                let old_state = data
                    .get("oldState")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let new_state = data
                    .get("newState")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let has_more: bool = data
                    .get("hasMoreChanges")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                let created: Vec<String> = data
                    .get("created")
                    .and_then(|v| serde_json::from_value(v.clone()).ok())
                    .unwrap_or_default();
                let updated: Vec<String> = data
                    .get("updated")
                    .and_then(|v| serde_json::from_value(v.clone()).ok())
                    .unwrap_or_default();
                let destroyed: Vec<String> = data
                    .get("destroyed")
                    .and_then(|v| serde_json::from_value(v.clone()).ok())
                    .unwrap_or_default();

                debug!(
                    target: "jmap",
                    old_state = %old_state,
                    new_state = %new_state,
                    created = created.len(),
                    updated = updated.len(),
                    destroyed = destroyed.len(),
                    has_more = has_more,
                    "Email changes synced"
                );

                return Ok(EmailChangesResult {
                    created,
                    updated,
                    destroyed,
                    new_state,
                    has_more_changes: has_more,
                });
            }
        }

        Err(anyhow!(
            "Unexpected JMAP response structure for Email/changes"
        ))
    }

    /// Mailbox-scoped email change feed via `Email/queryChanges`
    /// (RFC 8620 §5.6).
    ///
    /// `Email/changes` reports changes account-wide, so using it to drive one
    /// EAS collection would leak mail from every folder into a folder-scoped
    /// Sync. `Email/queryChanges` re-runs the mailbox query at `since_state`
    /// and returns only the ids added to or removed from *that* mailbox. The
    /// `inMailbox` filter is immutable (RFC 8620 §5.6), so the server can
    /// answer it from its change log.
    ///
    /// Returns the added and removed id lists plus the new query state. The
    /// `removed` ids precede the `added` ids in the response (RFC 8620 §5.6
    /// guarantees this, and Stalwart emits it), so a windowed delivery never
    /// applies an add before its preceding remove of the same id.
    pub async fn query_email_changes(
        &self,
        account_id: &str,
        mailbox_id: &str,
        since_state: &str,
        username: &str,
        password: &SecretString,
        max_changes: Option<u32>,
    ) -> Result<EmailChangesResult> {
        let session = self.get_session(username, password).await?;
        let api_url = &session.api_url;

        let method_calls = vec![(
            "Email/queryChanges",
            json!({
                "accountId": account_id,
                // RFC 8620 §5.6: the `filter` and `sort` MUST be identical to
                // those of the query that produced `since_state`, or the server
                // may reject the state. The initial prime runs `Email/query`
                // with the `receivedAt` descending default, so the delta must
                // repeat it.
                "filter": { "inMailbox": mailbox_id },
                "sort": [{ "property": "receivedAt", "isAscending": false }],
                "sinceQueryState": since_state,
                "maxChanges": max_changes.unwrap_or(crate::jmap::DEFAULT_EMAIL_CHANGES_MAX),
                "calculateTotal": false,
            }),
            "qc0",
        )];

        let response = self
            .api_call(
                api_url,
                &["urn:ietf:params:jmap:core", "urn:ietf:params:jmap:mail"],
                method_calls,
                username,
                password,
            )
            .await?;

        for (method, data, _) in response.method_responses {
            // RFC 8620 §3.6.2: a method-level error is reported as a separate
            // `error` response, NOT as a field on the method's own response.
            if method == "error" {
                // RFC 8620 §5.6 method errors, all mapped to the typed
                // `CannotCalculateChanges` so the caller re-primes from a full
                // snapshot rather than storing a state that would silently skip
                // changes:
                //  * `cannotCalculateChanges` — the server cannot compute the
                //    delta.
                //  * `tooManyChanges` — more than `maxChanges` changes are
                //    pending. RFC 8620 §5.6 defines no `hasMoreChanges` flag for
                //    this method, so there is no page to advance; the client may
                //    only retry with a higher `maxChanges` or invalidate its
                //    cache. Re-priming is the recoverable choice (an
                //    unbounded `maxChanges` is not), and it keeps the Ping probe
                //    waking the folder instead of silently deferring.
                //  * `invalidArguments` — Stalwart reports an unresumable
                //    `sinceQueryState` as this ("invalid JMAP State", observed
                //    against v1.1.0) rather than `cannotCalculateChanges`. A
                //    re-prime is self-healing whereas a frozen delta is not, so
                //    it is a deliberate choice to treat the rejection of our
                //    stored opaque state as a re-prime trigger.
                match data.get("type").and_then(|v| v.as_str()) {
                    Some("cannotCalculateChanges") => {
                        return Err(anyhow::Error::new(CannotCalculateChanges(format!(
                            "Email/queryChanges cannot calculate changes from {since_state}"
                        ))));
                    }
                    Some("tooManyChanges") => {
                        return Err(anyhow::Error::new(CannotCalculateChanges(format!(
                            "Email/queryChanges from {since_state} exceeds maxChanges"
                        ))));
                    }
                    Some("invalidArguments") => {
                        return Err(anyhow::Error::new(CannotCalculateChanges(format!(
                            "Email/queryChanges rejected the query state {since_state}: {data}"
                        ))));
                    }
                    _ => {}
                }
                return Err(anyhow!("Email/queryChanges error: {}", data));
            }
            if method == "Email/queryChanges" {
                let old_state = data
                    .get("oldQueryState")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let new_state = data
                    .get("newQueryState")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let removed: Vec<String> = data
                    .get("removed")
                    .and_then(|v| serde_json::from_value(v.clone()).ok())
                    .unwrap_or_default();
                // `added` is a list of `{id, index}` objects (RFC 8620 §5.6);
                // preserve result order.
                let added: Vec<String> = data
                    .get("added")
                    .and_then(|v| v.as_array())
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|item| {
                                item.get("id").and_then(|v| v.as_str()).map(String::from)
                            })
                            .collect()
                    })
                    .unwrap_or_default();

                // A response without a `newQueryState` cannot advance the
                // delta: treat it like `cannotCalculateChanges` (as
                // `changes_calendar_events` does for a missing `newState`) so
                // the caller re-primes instead of storing a state that would
                // replay or skip changes.
                if new_state.is_empty() {
                    return Err(anyhow::Error::new(CannotCalculateChanges(format!(
                        "Email/queryChanges returned no newQueryState (oldState {old_state})"
                    ))));
                }

                debug!(
                    target: "jmap",
                    since = %since_state,
                    new_state = %new_state,
                    added = added.len(),
                    removed = removed.len(),
                    "Email query changes synced"
                );

                // `Email/queryChanges` has no `hasMoreChanges` flag (RFC 8620
                // §5.6); a page that came back exactly at the `maxChanges` cap
                // was almost certainly clipped, so signal the caller to
                // re-query from `newQueryState` to drain the remainder.
                let page_len = added.len() + removed.len();
                let has_more_changes = page_len
                    >= max_changes.unwrap_or(crate::jmap::DEFAULT_EMAIL_CHANGES_MAX) as usize;
                return Ok(EmailChangesResult {
                    created: added,
                    updated: Vec::new(),
                    destroyed: removed,
                    new_state,
                    has_more_changes,
                });
            }
        }

        Err(anyhow!(
            "Unexpected JMAP response structure for Email/queryChanges"
        ))
    }

    /// Get calendar event changes via CalendarEvent/changes
    /// (draft-ietf-jmap-calendars §5.13).
    ///
    /// Returns created, updated, destroyed event IDs and the new state token.
    ///
    /// `max_changes` bounds the server's `maxChanges` property so callers that
    /// drain a long change chain can page through it; `None` keeps the
    /// historical default of 500. When the server answers with the RFC 8620
    /// §5.2 `cannotCalculateChanges` error (or returns no `newState`), the
    /// error downcasts to [`CannotCalculateChanges`] so the caller re-primes
    /// from a full query instead of persisting an unusable state.
    pub async fn changes_calendar_events(
        &self,
        account_id: &str,
        old_state: &str,
        username: &str,
        password: &SecretString,
        max_changes: Option<u32>,
    ) -> Result<CalendarChangesResult> {
        let session = self.get_session(username, password).await?;
        let api_url = &session.api_url;

        let method_calls = vec![(
            "CalendarEvent/changes",
            json!({
                "accountId": account_id,
                "sinceState": old_state,
                "maxChanges": max_changes.unwrap_or(500),
            }),
            "cc0",
        )];

        let response = self
            .api_call(
                api_url,
                &["urn:ietf:params:jmap:core", JMAP_CAL_CAPABILITY],
                method_calls,
                username,
                password,
            )
            .await?;

        for (method, data, _) in response.method_responses {
            if method == "error" {
                let err_type = data
                    .get("type")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown");
                if err_type == "cannotCalculateChanges" {
                    return Err(anyhow::Error::new(CannotCalculateChanges(format!(
                        "sinceState {old_state:?} rejected"
                    ))));
                }
                return Err(anyhow!(
                    "CalendarEvent/changes returned method error: {}",
                    data
                ));
            }
            if method == "CalendarEvent/changes" {
                let old_state = data
                    .get("oldState")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let new_state = data
                    .get("newState")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let has_more: bool = data
                    .get("hasMoreChanges")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                let created: Vec<String> = data
                    .get("created")
                    .and_then(|v| serde_json::from_value(v.clone()).ok())
                    .unwrap_or_default();
                let updated: Vec<String> = data
                    .get("updated")
                    .and_then(|v| serde_json::from_value(v.clone()).ok())
                    .unwrap_or_default();
                let destroyed: Vec<String> = data
                    .get("destroyed")
                    .and_then(|v| serde_json::from_value(v.clone()).ok())
                    .unwrap_or_default();

                debug!(
                    target: "jmap",
                    old_state = %old_state,
                    new_state = %new_state,
                    created = created.len(),
                    updated = updated.len(),
                    destroyed = destroyed.len(),
                    has_more = has_more,
                    "Calendar changes synced"
                );

                // A `newState` is mandatory (RFC 8620 §5.2). Its absence means
                // the token the caller would store is unusable, so treat it the
                // same as `cannotCalculateChanges` and force a re-prime.
                if new_state.is_empty() {
                    return Err(anyhow::Error::new(CannotCalculateChanges(format!(
                        "sinceState {old_state:?} produced no newState"
                    ))));
                }

                return Ok(CalendarChangesResult {
                    created,
                    updated,
                    destroyed,
                    new_state,
                    has_more_changes: has_more,
                });
            }
        }

        Err(anyhow!(
            "Unexpected JMAP response structure for CalendarEvent/changes"
        ))
    }

    /// Drain a `CalendarEvent/changes` chain while the server reports
    /// `hasMoreChanges` (RFC 8620 §5.2), exactly as the email path's
    /// `accumulate_email_changes` does.
    ///
    /// Returns the union of created/updated/destroyed ids across every page
    /// plus the final `newState` — the ONLY token safe to persist, because a
    /// stored intermediate `newState` would make the next delta start after the
    /// undrained pages and silently skip those events. Ids are de-duplicated
    /// while preserving first-seen order; an id that appears in both `created`
    /// and `updated` across pages keeps its earliest classification, and any id
    /// that is destroyed wins (a later create followed by a destroy is a net
    /// deletion).
    ///
    /// The id cap ([`MAX_CALENDAR_CHANGE_ACCUMULATION`]) and the page cap
    /// ([`MAX_CALENDAR_CHANGE_PAGES`]) bound a pathological backend. If
    /// either bound is hit while the server still reports
    /// `hasMoreChanges`, the result preserves that flag (and the
    /// intermediate `newState`) so the sync caller can persist the
    /// intermediate token and signal `<MoreAvailable/>`: the delivered
    /// accumulated changes are consumed, and the next delta resumes from the
    /// intermediate state to drain the remainder.
    pub async fn accumulate_calendar_changes(
        &self,
        account_id: &str,
        old_state: &str,
        username: &str,
        password: &SecretString,
    ) -> Result<CalendarEventChangesResult> {
        let mut created: Vec<String> = Vec::new();
        let mut updated: Vec<String> = Vec::new();
        let mut destroyed: Vec<String> = Vec::new();
        let mut since = old_state.to_string();
        let mut pages = 0usize;
        let more = loop {
            let batch = self
                .changes_calendar_events(
                    account_id,
                    &since,
                    username,
                    password,
                    Some(CALENDAR_CHANGES_PAGE_SIZE),
                )
                .await?;
            created.extend(batch.created);
            updated.extend(batch.updated);
            destroyed.extend(batch.destroyed);
            since = batch.new_state;
            pages += 1;
            if !batch.has_more_changes
                || created.len() + updated.len() + destroyed.len()
                    >= MAX_CALENDAR_CHANGE_ACCUMULATION
                || pages >= MAX_CALENDAR_CHANGE_PAGES
            {
                break batch.has_more_changes;
            }
        };
        // A destroyed id wins over created/updated: across chained pages an id
        // can be created on one page and destroyed on a later one, and the net
        // effect for the client is a deletion.
        let destroyed = dedupe_ids(destroyed);
        let destroyed_set: std::collections::HashSet<&str> =
            destroyed.iter().map(String::as_str).collect();
        let mut created = dedupe_ids(created);
        created.retain(|id| !destroyed_set.contains(id.as_str()));
        let mut updated = dedupe_ids(updated);
        updated.retain(|id| !destroyed_set.contains(id.as_str()));
        Ok(CalendarEventChangesResult {
            created,
            updated,
            destroyed,
            new_state: since,
            has_more_changes: more,
        })
    }

    /// Update email properties via JMAP Email/set (RFC 8621 §4.5).
    ///
    /// `update` is a JSON object mapping email IDs to patch objects.
    /// Example: `{"email-123": {"keywords": {"$seen": true}}}`
    pub async fn update_email(
        &self,
        account_id: &str,
        update: &serde_json::Value,
        username: &str,
        password: &SecretString,
    ) -> Result<()> {
        let session = self.get_session(username, password).await?;
        let api_url = &session.api_url;

        let method_calls = vec![(
            "Email/set",
            serde_json::json!({
                "accountId": account_id,
                "update": update,
            }),
            "es0",
        )];

        let _resp = self
            .api_call(
                api_url,
                &["urn:ietf:params:jmap:core", "urn:ietf:params:jmap:mail"],
                method_calls,
                username,
                password,
            )
            .await?;

        Ok(())
    }

    /// Inspectable `Email/set` update: returns an [`EmailSetOutcome`] so the
    /// MAPI property-write handlers can detect a per-id `notUpdated`
    /// rejection or a method-level `error` instead of masking every `Ok` as
    /// success. The parsing is delegated to the pure module-level
    /// [`parse_email_set_outcome`] helper so unit tests can exercise the
    /// rejection-handling without a live server (qodo #3/#5, cubic #23).
    pub async fn update_email_checked(
        &self,
        account_id: &str,
        update: &serde_json::Value,
        username: &str,
        password: &SecretString,
    ) -> Result<EmailSetOutcome> {
        let session = self.get_session(username, password).await?;
        let api_url = &session.api_url;

        let method_calls = vec![(
            "Email/set",
            serde_json::json!({
                "accountId": account_id,
                "update": update,
            }),
            "es0",
        )];

        let resp = self
            .api_call(
                api_url,
                &["urn:ietf:params:jmap:core", "urn:ietf:params:jmap:mail"],
                method_calls,
                username,
                password,
            )
            .await?;

        Ok(parse_email_set_outcome(resp.method_responses))
    }

    /// Bulk toggle `$seen` for a set of MAPI message ids within one mailbox
    /// (`RopSetReadFlags`, MS-OXCROPS §2.2.6.12). The MAPI ids are FNV-1a
    /// hashes of the JMAP email id (see `mapi::store::message_id_from_jmap`),
    /// so the method pages the mailbox once to reverse-map them, then issues a
    /// single `Email/set` `update` patch. Returns `Ok(true)` when at least one
    /// id resolved and was patched, `Ok(false)` when none resolved (every id
    /// stale), and an `Err` when the Email/set call itself failed.
    pub async fn set_read_flags(
        &self,
        account_id: &str,
        mailbox_id: &str,
        message_ids: &[u64],
        want_read: bool,
        username: &str,
        password: &SecretString,
    ) -> Result<bool> {
        if message_ids.is_empty() {
            return Ok(false);
        }
        // Reverse-map the MAPI mids to JMAP email ids by paging the mailbox.
        let want: std::collections::HashSet<u64> = message_ids.iter().copied().collect();
        let mut patch = serde_json::Map::new();
        const PAGE: u64 = 256;
        const MAX_PAGES: u64 = 64;
        let mut position: u64 = 0;
        for _ in 0..MAX_PAGES {
            let params = QueryEmailsParams {
                account_id,
                filter: Some(json!({"inMailbox": mailbox_id})),
                sort: None,
                position,
                limit: PAGE,
                anchor: None,
                anchor_offset: 0,
                username,
                password,
            };
            let ids = self.query_email_ids(params).await?;
            if ids.is_empty() {
                break;
            }
            let received = ids.len() as u64;
            for jid in ids {
                if want.contains(&crate::mapi::store::message_id_from_jmap(&jid)) {
                    patch.insert(
                        jid,
                        json!({
                            "keywords/$seen": if want_read { json!(true) } else { json!(null) },
                        }),
                    );
                }
            }
            // All targeted mids resolved; stop paging early instead of walking
            // the remaining mailbox pages for nothing.
            if patch.len() == want.len() {
                break;
            }
            if received < PAGE {
                break;
            }
            position += PAGE;
        }
        if patch.is_empty() {
            return Ok(false);
        }
        let outcome = self
            .update_email_checked(
                account_id,
                &serde_json::Value::Object(patch),
                username,
                password,
            )
            .await?;
        if let Some(err) = &outcome.method_error {
            tracing::warn!(
                target: "jmap",
                error = %err,
                "Email/set set_read_flags rejected at method level"
            );
            return Err(anyhow::anyhow!("Email/set set_read_flags rejected: {err}"));
        }
        for (id, desc) in &outcome.not_updated {
            tracing::warn!(
                target: "jmap",
                email_id = %id,
                reason = %desc,
                "set_read_flags notUpdated"
            );
        }
        // `true` only when the server actually accepted at least one patch;
        // an empty `updated` set means every id was stale or rejected.
        Ok(!outcome.updated.is_empty())
    }

    /// Destroy emails via JMAP `Email/set` `destroy` (RFC 8621 §4.5). Returns
    /// the count of ids the server reports as `destroyed` (i.e. successfully
    /// removed). Per-item failures land in `notDestroyed`; they are NOT a hard
    /// error here so the `RopDeleteMessages` dispatcher can encode a partial
    /// completion (`PartialCompletion=1`) for the subset that didn't delete,
    /// matching MS-OXCROPS §2.2.4.11.2 semantics — previously `Ok(())` hid
    /// partial server-side failures as a full success.
    pub async fn destroy_emails(
        &self,
        account_id: &str,
        destroy: &[String],
        username: &str,
        password: &SecretString,
    ) -> Result<usize> {
        let session = self.get_session(username, password).await?;
        let api_url = &session.api_url;

        let method_calls = vec![(
            "Email/set",
            serde_json::json!({
                "accountId": account_id,
                "destroy": destroy,
            }),
            "es0",
        )];

        let resp = self
            .api_call(
                api_url,
                &["urn:ietf:params:jmap:core", "urn:ietf:params:jmap:mail"],
                method_calls,
                username,
                password,
            )
            .await?;

        let mut destroyed = 0usize;
        for (method, data, _) in resp.method_responses {
            if method == "Email/set" {
                if let Some(arr) = data.get("destroyed").and_then(|v| v.as_array()) {
                    destroyed = arr.len();
                }
                if let Some(not_destroyed) = data.get("notDestroyed")
                    && !not_destroyed.is_null()
                    && not_destroyed.as_object().is_none_or(|o| !o.is_empty())
                {
                    tracing::warn!(
                        "JMAP Email/destroy partial failure: {} of {} not destroyed: {}",
                        destroy.len().saturating_sub(destroyed),
                        destroy.len(),
                        not_destroyed
                    );
                }
            }
        }
        Ok(destroyed)
    }

    /// Create a draft email via JMAP `Email/set` (RFC 8621 §4.5), returning
    /// the server-assigned email id. `email_obj` is the full Email object;
    /// the caller supplies the draft `mailboxIds`, `keywords`, headers and
    /// bodyValues. This is the backend op `RopCreateMessage`/
    /// `RopSaveChangesMessage` use to persist a New Outlook compose.
    pub async fn create_email(
        &self,
        account_id: &str,
        email_obj: Value,
        username: &str,
        password: &SecretString,
    ) -> Result<String> {
        let session = self.get_session(username, password).await?;
        let api_url = &session.api_url;
        let method_calls = vec![(
            "Email/set",
            json!({
                "accountId": account_id,
                "create": { "d0": email_obj },
            }),
            "es0",
        )];
        let resp = self
            .api_call(
                api_url,
                &["urn:ietf:params:jmap:core", "urn:ietf:params:jmap:mail"],
                method_calls,
                username,
                password,
            )
            .await?;
        for (method, data, _) in resp.method_responses {
            if method == "Email/set" {
                if let Some(not_created) = data.get("notCreated")
                    && !not_created.is_null()
                    && not_created.as_object().is_none_or(|o| !o.is_empty())
                {
                    return Err(anyhow!("Email/set create failed: {}", not_created));
                }
                if let Some(created) = data.get("created")
                    && let Some(d0) = created.get("d0")
                {
                    let id = d0
                        .get("id")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    if !id.is_empty() {
                        return Ok(id);
                    }
                }
            }
        }
        Err(anyhow!("Email/set create returned no id"))
    }

    /// Map JMAP email ids to MAPI message ids is a one-way FNV hash, so to
    /// resolve the JMAP ids for a set of MAPI ids the caller enumerates the
    /// source folder via `Email/query`+`Email/get` and we match. This helper
    /// returns the `(jmap_id, mailbox_ids)` pair for every message currently
    /// in `mailbox_id`, keyed by the MAPI id derived via
    /// `store::message_id_from_jmap`. Used by `RopDeleteMessages` /
    /// `RopMoveCopyMessages` to translate the client's MAPI ids back to JMAP.
    pub async fn list_email_ids_in_mailbox(
        &self,
        account_id: &str,
        mailbox_id: &str,
        username: &str,
        password: &SecretString,
    ) -> Result<Vec<(String, Vec<String>)>> {
        // Page through the mailbox until it is fully drained. `Email/query`
        // pages with `position`+`limit`; a single 500-item page silently drops
        // the remainder of a larger folder (and worse, an empty-folder caller
        // would report success while leaving messages behind), so loop while
        // the server reports more than the current page.
        const PAGE: u64 = 500;
        let mut out: Vec<(String, Vec<String>)> = Vec::new();
        let mut position: u64 = 0;
        loop {
            let params = QueryEmailsParams {
                account_id,
                filter: Some(json!({"inMailbox": mailbox_id})),
                sort: None,
                position,
                limit: PAGE,
                anchor: None,
                anchor_offset: 0,
                username,
                password,
            };
            let list = self.query_emails(params).await?;
            let fetched = list.emails.len() as u64;
            for e in list.emails {
                let Some(jid) = e.id.clone() else {
                    continue;
                };
                let mids = e
                    .mailbox_ids
                    .as_ref()
                    .map(|m| m.keys().cloned().collect())
                    .unwrap_or_default();
                out.push((jid, mids));
            }
            position += fetched;
            // Stop when the server returned a partial/empty page or we reached
            // the reported total.
            if fetched < PAGE || position >= list.total {
                break;
            }
        }
        Ok(out)
    }

    /// The `(jmap_id, sorted keyword names)` pair for up to `limit` messages
    /// currently in `mailbox_id`, starting at query `position`. A lightweight
    /// `Email/query` + `Email/get` (`properties: ["keywords"]` only — no body
    /// fetch), used by the email delta's flag-only change detector. Callers
    /// page with the returned `position`/`total`/`more` so a delta never scans
    /// the whole mailbox in one shot.
    ///
    /// Returns `(entries, position, total, more)`: `position` is the next query
    /// position to resume at, `total` the mailbox size, and `more` whether a
    /// further page exists.
    pub async fn list_email_ids_with_keywords_page(
        &self,
        account_id: &str,
        mailbox_id: &str,
        position: u64,
        limit: u64,
        username: &str,
        password: &SecretString,
    ) -> Result<(Vec<(String, Vec<String>)>, u64, u64, bool)> {
        let session = self.get_session(username, password).await?;
        let api_url = &session.api_url;
        let calls = vec![
            (
                "Email/query",
                json!({
                    "accountId": account_id,
                    "filter": { "inMailbox": mailbox_id },
                    "sort": [{ "property": "receivedAt", "isAscending": false }],
                    "position": position,
                    "limit": limit.max(1),
                    "calculateTotal": true,
                }),
                "q0",
            ),
            (
                "Email/get",
                json!({
                    "accountId": account_id,
                    "#ids": {
                        "resultOf": "q0",
                        "name": "Email/query",
                        "path": "/ids",
                    },
                    "properties": ["keywords"],
                }),
                "g0",
            ),
        ];
        let response = self
            .api_call(
                api_url,
                &["urn:ietf:params:jmap:core", "urn:ietf:params:jmap:mail"],
                calls,
                username,
                password,
            )
            .await?;
        let mut out: Vec<(String, Vec<String>)> = Vec::new();
        let mut total: u64 = 0;
        let mut fetched: u64 = 0;
        let mut saw_query = false;
        for (method, data, _) in response.method_responses {
            if method == "error" {
                return Err(anyhow!("Email/query error: {}", data));
            }
            if method == "Email/query" {
                saw_query = true;
                total = data.get("total").and_then(|v| v.as_u64()).unwrap_or(0);
                fetched = data
                    .get("ids")
                    .and_then(|v| v.as_array())
                    .map(|a| a.len() as u64)
                    .unwrap_or(0);
            } else if method == "Email/get" {
                let list = data
                    .get("list")
                    .and_then(|v| v.as_array())
                    .cloned()
                    .unwrap_or_default();
                for item in list {
                    let Some(id) = item.get("id").and_then(|v| v.as_str()) else {
                        continue;
                    };
                    let mut keywords: Vec<String> = item
                        .get("keywords")
                        .and_then(|v| v.as_object())
                        .map(|m| {
                            m.iter()
                                .filter(|(_, v)| v.as_bool().unwrap_or(false))
                                .map(|(n, _)| n.clone())
                                .collect()
                        })
                        .unwrap_or_default();
                    keywords.sort();
                    out.push((id.to_string(), keywords));
                }
            }
        }
        if !saw_query {
            return Err(anyhow!(
                "Email/query returned no method response while listing mailbox keywords"
            ));
        }
        let next_position = position + fetched;
        let more = fetched > 0 && next_position < total;
        Ok((out, next_position, total, more))
    }

    /// Move emails from their current mailbox(es) to a target mailbox by
    /// patching `mailboxIds`: `add {target: true}` and `remove` every current
    /// mailbox id. RFC 8621 §4.5 `Email/set` `update` semantics. Returns the
    /// count of emails whose mailboxIds were actually patched.
    pub async fn move_emails(
        &self,
        account_id: &str,
        email_ids: &[String],
        target_mailbox_id: &str,
        username: &str,
        password: &SecretString,
    ) -> Result<usize> {
        if email_ids.is_empty() {
            return Ok(0);
        }
        // We need the current mailboxIds for each id to construct the patch
        // (RFC 8621 update merges; to *move* we must clear the old ids).
        let session = self.get_session(username, password).await?;
        let api_url = &session.api_url;
        // The explicit `ids` array is bounded by the session's
        // `maxObjectsInGet` (RFC 8620 §2); an over-cap list is rejected whole
        // with `requestTooLarge`, so it must be read in conforming batches.
        let max = session_max_objects_in_get(&session);
        let mut current: std::collections::HashMap<String, Vec<String>> =
            std::collections::HashMap::new();
        for chunk in chunk_ids(email_ids, max) {
            let ids_args: Vec<Value> = chunk.iter().map(|i| json!(i)).collect();
            let get_calls = vec![(
                "Email/get",
                json!({
                    "accountId": account_id,
                    "ids": ids_args,
                    "properties": ["id", "mailboxIds"],
                }),
                "g0",
            )];
            let resp = self
                .api_call(
                    api_url,
                    &["urn:ietf:params:jmap:core", "urn:ietf:params:jmap:mail"],
                    get_calls,
                    username,
                    password,
                )
                .await?;
            for (method, data, _) in &resp.method_responses {
                if method == "Email/get"
                    && let Some(list) = data.get("list").and_then(|v| v.as_array())
                {
                    for e in list {
                        let e_id = e.get("id").and_then(|v| v.as_str()).unwrap_or("");
                        if e_id.is_empty() {
                            continue;
                        }
                        let mids = e
                            .get("mailboxIds")
                            .and_then(|v| v.as_object())
                            .map(|o| o.keys().cloned().collect())
                            .unwrap_or_default();
                        current.insert(e_id.to_string(), mids);
                    }
                }
            }
        }
        // Build the update patch keyed BY email id (not by response order). A
        // naive `zip(email_ids, current)` would silently mis-associate patches
        // (or drop them) when Email/get returns fewer entries than requested
        // or in a different order — see PR #1825 review. `build_move_update_patch`
        // is a pure helper so the partial/reorder-safe semantics are unit-tested
        // in `build_move_update_patch_skips_missing_and_keys_by_id`.
        let update = build_move_update_patch(email_ids, &current, target_mailbox_id);
        if update.is_empty() {
            // None of the requested ids were found on the server; nothing to
            // move. Report zero (the dispatcher encodes Success with
            // PartialCompletion reflecting the gap), NOT a hard error — a hard
            // error here would turn a benign "messages already gone" into a
            // DiskError.
            return Ok(0);
        }
        let set_calls = vec![(
            "Email/set",
            json!({
                "accountId": account_id,
                "update": Value::Object(update),
            }),
            "es0",
        )];
        let set_resp = self
            .api_call(
                api_url,
                &["urn:ietf:params:jmap:core", "urn:ietf:params:jmap:mail"],
                set_calls,
                username,
                password,
            )
            .await?;
        let mut updated = 0usize;
        for (method, data, _) in set_resp.method_responses {
            if method == "Email/set" {
                if let Some(u) = data.get("updated").and_then(|v| v.as_object()) {
                    updated = u.len();
                }
                if let Some(not_updated) = data.get("notUpdated")
                    && !not_updated.is_null()
                    && not_updated.as_object().is_none_or(|o| !o.is_empty())
                {
                    // Per-item failures are NOT a hard error: return the
                    // partial `updated` count so the dispatcher encodes
                    // PartialCompletion=1 (MS-OXCROPS §2.2.4.6.4) instead of a
                    // DiskError that hides the ids that did move.
                    tracing::warn!(
                        "JMAP Email/set move partial failure: {} of {} not updated: {}",
                        email_ids.len().saturating_sub(updated),
                        email_ids.len(),
                        not_updated
                    );
                }
            }
        }
        Ok(updated)
    }

    /// Copy emails to a target mailbox using JMAP `Email/set` with
    /// `copyFrom_emailId`. RFC 8621 §4.5 supports server-side copy via the
    /// `copyFrom` create argument. Returns the count of new emails created,
    /// keyed by the source id order.
    pub async fn copy_emails(
        &self,
        account_id: &str,
        email_ids: &[String],
        target_mailbox_id: &str,
        username: &str,
        password: &SecretString,
    ) -> Result<usize> {
        if email_ids.is_empty() {
            return Ok(0);
        }
        let session = self.get_session(username, password).await?;
        let api_url = &session.api_url;
        let mut create = serde_json::Map::new();
        for (i, src) in email_ids.iter().enumerate() {
            let key = format!("c{i}");
            create.insert(
                key,
                json!({
                    "copyFrom": src,
                    "mailboxIds": { (target_mailbox_id): true },
                }),
            );
        }
        let calls = vec![(
            "Email/set",
            json!({
                "accountId": account_id,
                "create": Value::Object(create),
            }),
            "es0",
        )];
        let resp = self
            .api_call(
                api_url,
                &["urn:ietf:params:jmap:core", "urn:ietf:params:jmap:mail"],
                calls,
                username,
                password,
            )
            .await?;
        let mut created = 0usize;
        for (method, data, _) in resp.method_responses {
            if method == "Email/set" {
                if let Some(c) = data.get("created").and_then(|v| v.as_object()) {
                    created = c.len();
                }
                if let Some(not_created) = data.get("notCreated")
                    && !not_created.is_null()
                    && not_created.as_object().is_none_or(|o| !o.is_empty())
                {
                    // Per-item copy failures are partial completion, not a hard
                    // error, matching RopMoveCopyMessages semantics.
                    tracing::warn!(
                        "JMAP Email/set copy partial failure: {} of {} not created: {}",
                        email_ids.len().saturating_sub(created),
                        email_ids.len(),
                        not_created
                    );
                }
            }
        }
        Ok(created)
    }

    /// Create a mailbox via JMAP `Mailbox/set` (RFC 8621 §5.2). Returns the
    /// server-assigned mailbox id on success. Backs `RopCreateFolder` and the
    /// "copy folder" ROP (`RopCopyFolder` needs to first materialise an empty
    /// mailbox under the destination parent before the message copy fills it).
    ///
    /// `parent_id` may be `None` to create a top-level mailbox; `name` is the
    /// display name (RFC 8621 requires a non-empty `name` and rejects a set
    /// with neither `parentId` nor `name`).
    pub async fn create_mailbox(
        &self,
        account_id: &str,
        name: &str,
        parent_id: Option<&str>,
        username: &str,
        password: &SecretString,
    ) -> Result<String> {
        let mut create_args = serde_json::Map::new();
        create_args.insert("name".to_string(), json!(name));
        if let Some(p) = parent_id {
            create_args.insert("parentId".to_string(), json!(p));
        }

        let session = self.get_session(username, password).await?;
        let api_url = &session.api_url;
        let calls = vec![(
            "Mailbox/set",
            json!({
                "accountId": account_id,
                "create": { "c0": Value::Object(create_args) },
            }),
            "ms0",
        )];
        let resp = self
            .api_call(
                api_url,
                &["urn:ietf:params:jmap:core", "urn:ietf:params:jmap:mail"],
                calls,
                username,
                password,
            )
            .await?;

        for (method, data, _) in resp.method_responses {
            if method != "Mailbox/set" {
                continue;
            }
            if let Some(err) = data.get("error").filter(|v| !v.is_null()) {
                let desc = err
                    .get("description")
                    .and_then(|v| v.as_str())
                    .unwrap_or("Mailbox/set method rejected");
                return Err(anyhow!("Mailbox/set create rejected: {desc}"));
            }
            if let Some(not_created) = data.get("notCreated")
                && !not_created.is_null()
                && let Some(obj) = not_created.as_object()
                && !obj.is_empty()
            {
                let desc = obj
                    .values()
                    .next()
                    .and_then(|v| v.get("description"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown");
                return Err(anyhow!("Mailbox/set create failed: {desc}"));
            }
            if let Some(created) = data.get("created").and_then(|v| v.as_object())
                && let Some(id) = created
                    .get("c0")
                    .and_then(|v| v.get("id"))
                    .and_then(|v| v.as_str())
                && !id.is_empty()
            {
                return Ok(id.to_string());
            }
        }
        Err(anyhow!("Mailbox/set response missing created mailbox id"))
    }

    /// Update a mailbox via JMAP `Mailbox/set` `update` (RFC 8621 §5.2). Used
    /// for folder *rename* (patch `name`) and folder *move* (patch
    /// `parentId`). Returns true when the mailbox was updated.
    pub async fn update_mailbox(
        &self,
        account_id: &str,
        mailbox_id: &str,
        name: Option<&str>,
        parent_id: Option<Option<&str>>,
        username: &str,
        password: &SecretString,
    ) -> Result<bool> {
        let mut patch = serde_json::Map::new();
        if let Some(n) = name {
            patch.insert("name".to_string(), json!(n));
        }
        // `parent_id` is an `Option<Option<&str>>`: `Some(Some(id))` sets the
        // parent, `Some(None)` clears it (top-level mailbox). `None` means
        // "don't touch parentId".
        if let Some(p) = parent_id {
            patch.insert(
                "parentId".to_string(),
                match p {
                    Some(id) => json!(id),
                    None => json!(null),
                },
            );
        }
        if patch.is_empty() {
            return Ok(true);
        }

        let session = self.get_session(username, password).await?;
        let api_url = &session.api_url;
        let mut update = serde_json::Map::new();
        update.insert(mailbox_id.to_string(), Value::Object(patch));
        let calls = vec![(
            "Mailbox/set",
            json!({
                "accountId": account_id,
                "update": Value::Object(update),
            }),
            "ms1",
        )];
        let resp = self
            .api_call(
                api_url,
                &["urn:ietf:params:jmap:core", "urn:ietf:params:jmap:mail"],
                calls,
                username,
                password,
            )
            .await?;

        for (method, data, _) in resp.method_responses {
            if method != "Mailbox/set" {
                continue;
            }
            if let Some(err) = data.get("error").filter(|v| !v.is_null()) {
                let desc = err
                    .get("description")
                    .and_then(|v| v.as_str())
                    .unwrap_or("Mailbox/set method rejected");
                return Err(anyhow!("Mailbox/set update rejected: {desc}"));
            }
            if let Some(not_updated) = data.get("notUpdated")
                && !not_updated.is_null()
                && let Some(obj) = not_updated.as_object()
                && obj.contains_key(mailbox_id)
            {
                let desc = obj
                    .get(mailbox_id)
                    .and_then(|v| v.get("description"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown");
                return Err(anyhow!("Mailbox/set update failed: {desc}"));
            }
            if let Some(updated) = data.get("updated").and_then(|v| v.as_object()) {
                return Ok(updated.contains_key(mailbox_id));
            }
        }
        Err(anyhow!("Mailbox/set response missing update outcome"))
    }

    /// Destroy a mailbox via JMAP `Mailbox/set` `destroy` (RFC 8621 §2.5).
    /// Backs `RopDeleteFolder` and the EAS/EWS folder-delete paths.
    ///
    /// `remove_emails` sets `onDestroyRemoveEmails` (RFC 8621 §2.5): without it
    /// the server rejects destroying a mailbox that still contains emails with
    /// `mailboxHasEmail`. The EAS `FolderDelete` semantics remove the folder and
    /// its contents, so the EAS path passes `true`; a caller that must not
    /// destroy mail passes `false`.
    ///
    /// Returns a [`MailboxDestroyOutcome`] so a caller can tell an already-absent
    /// mailbox (RFC 8620 §5.3 `notFound` SetError — the desired end state) from a
    /// refusal (`mailboxHasEmail`/`mailboxHasChild`/`forbidden`/…) and from a
    /// backend failure. A JMAP method-level `error` response, a missing
    /// `Mailbox/set` response, and a missing/malformed `destroyed` field are all
    /// returned as `Err` (fail-closed) rather than being mistaken for a
    /// completed destroy.
    pub async fn destroy_mailbox(
        &self,
        account_id: &str,
        mailbox_id: &str,
        remove_emails: bool,
        username: &str,
        password: &SecretString,
    ) -> Result<MailboxDestroyOutcome> {
        let session = self.get_session(username, password).await?;
        let api_url = &session.api_url;
        let calls = vec![(
            "Mailbox/set",
            json!({
                "accountId": account_id,
                "destroy": [mailbox_id],
                "onDestroyRemoveEmails": remove_emails,
            }),
            "ms2",
        )];
        let resp = self
            .api_call(
                api_url,
                &["urn:ietf:params:jmap:core", "urn:ietf:params:jmap:mail"],
                calls,
                username,
                password,
            )
            .await?;

        for (method, data, _) in resp.method_responses {
            // RFC 8620 §3.6.2: a method-level error is a separate `error`
            // response, not a `Mailbox/set` result. Reject it rather than
            // falling through to a success outcome.
            if method == "error" {
                return Err(anyhow!(
                    "Mailbox/set destroy returned method error: {}",
                    data
                ));
            }
            if method != "Mailbox/set" {
                continue;
            }
            // A per-id SetError (RFC 8620 §5.3): a `notFound` type means the id
            // is already gone (the desired end state); any other type is a
            // refusal the caller must not report as a completed delete.
            if let Some(not_destroyed) = data.get("notDestroyed")
                && !not_destroyed.is_null()
                && let Some(obj) = not_destroyed.as_object()
                && let Some(entry) = obj.get(mailbox_id)
            {
                if entry.get("type").and_then(|v| v.as_str()) == Some("notFound") {
                    return Ok(MailboxDestroyOutcome::AlreadyGone);
                }
                return Ok(MailboxDestroyOutcome::Refused);
            }
            let destroyed = data
                .get("destroyed")
                .and_then(|v| v.as_array())
                .ok_or_else(|| anyhow!("Mailbox/set destroy response missing `destroyed`"))?;
            return Ok(
                if destroyed.iter().any(|v| v.as_str() == Some(mailbox_id)) {
                    MailboxDestroyOutcome::Destroyed
                } else {
                    MailboxDestroyOutcome::Refused
                },
            );
        }
        Err(anyhow!("Mailbox/set response missing destroy outcome"))
    }

    /// Replace a mailbox's `shareWith` map (JMAP sharing, persisted via
    /// `Mailbox/set` `update`). `share_with` maps a Stalwart principal id to
    /// the rights object produced by `mapi::rules::mapi_rights_to_jmap`.
    /// Backs `RopModifyPermissions` (MS-OXCPERM §2.2.6.1).
    pub async fn update_mailbox_share_with(
        &self,
        account_id: &str,
        mailbox_id: &str,
        share_with: serde_json::Value,
        username: &str,
        password: &SecretString,
    ) -> Result<()> {
        let session = self.get_session(username, password).await?;
        let api_url = &session.api_url;
        let mut update = serde_json::Map::new();
        update.insert(mailbox_id.to_string(), json!({ "shareWith": share_with }));
        let calls = vec![(
            "Mailbox/set",
            json!({
                "accountId": account_id,
                "update": Value::Object(update),
            }),
            "ms3",
        )];
        let resp = self
            .api_call(
                api_url,
                &["urn:ietf:params:jmap:core", "urn:ietf:params:jmap:mail"],
                calls,
                username,
                password,
            )
            .await?;

        for (method, data, _) in resp.method_responses {
            if method != "Mailbox/set" {
                continue;
            }
            if let Some(err) = data.get("error").filter(|v| !v.is_null()) {
                let desc = err
                    .get("description")
                    .and_then(|v| v.as_str())
                    .unwrap_or("Mailbox/set method rejected");
                return Err(anyhow!("Mailbox/set shareWith rejected: {desc}"));
            }
            if let Some(not_updated) = data.get("notUpdated")
                && !not_updated.is_null()
                && let Some(obj) = not_updated.as_object()
                && obj.contains_key(mailbox_id)
            {
                let desc = obj
                    .get(mailbox_id)
                    .and_then(|v| v.get("description"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown");
                return Err(anyhow!("Mailbox/set shareWith failed: {desc}"));
            }
            return Ok(());
        }
        Err(anyhow!("Mailbox/set response missing shareWith outcome"))
    }

    /// Fetch the user's active Sieve script (`SieveScript/get`, RFC 9661)
    /// keyed by the username, the same id the OOF manager uses. Returns
    /// `Ok(None)` *only* when the script genuinely does not exist (the id
    /// lands in `notFound`): a transport failure, a JMAP-level `error`
    /// method response, or a structurally incomplete success response are
    /// all returned as `Err` so a caller writing the script back never
    /// mistakes a failed read for "no rules/OOF configured".
    pub async fn get_sieve_script(
        &self,
        username: &str,
        password: &SecretString,
    ) -> Result<Option<String>> {
        let account_id = self.get_account_id(username, password).await?;
        let resp = self
            .api_call(
                self.base_url(),
                &["urn:ietf:params:jmap:core", JMAP_SIEVE_CAPABILITY],
                vec![(
                    "SieveScript/get",
                    json!({"accountId": account_id, "ids": [username]}),
                    "sg0",
                )],
                username,
                password,
            )
            .await?;
        let (name, value, _id) = resp
            .method_responses
            .first()
            .ok_or_else(|| anyhow!("SieveScript/get returned no method response"))?;
        if name == "error" {
            let desc = value
                .get("description")
                .and_then(|v| v.as_str())
                .or_else(|| value.get("detail").and_then(|v| v.as_str()))
                .unwrap_or("unknown JMAP error");
            return Err(anyhow!("SieveScript/get rejected: {desc}"));
        }
        // A genuine "script does not exist" answer.
        if let Some(not_found) = value.get("notFound")
            && not_found
                .as_array()
                .map(|ids| ids.iter().any(|i| i.as_str() == Some(username)))
                .unwrap_or(false)
        {
            return Ok(None);
        }
        let entry = value
            .get("list")
            .and_then(|v| v.as_array())
            .and_then(|list| list.first())
            .ok_or_else(|| anyhow!("SieveScript/get response missing `list` entries"))?;
        let script = entry
            .get("script")
            .and_then(|s| s.as_str())
            .ok_or_else(|| anyhow!("SieveScript/get response missing script content"))?;
        Ok(Some(script.to_string()))
    }

    /// Write the user's active Sieve script (`SieveScript/set`, RFC 9661),
    /// creating-or-updating the script keyed by the username.
    pub async fn set_sieve_script(
        &self,
        script: &str,
        username: &str,
        password: &SecretString,
    ) -> Result<()> {
        let account_id = self.get_account_id(username, password).await?;
        let resp = self
            .api_call(
                self.base_url(),
                &["urn:ietf:params:jmap:core", JMAP_SIEVE_CAPABILITY],
                vec![(
                    "SieveScript/set",
                    json!({
                        "accountId": account_id,
                        "update": { username: { "script": script } },
                    }),
                    "ss0",
                )],
                username,
                password,
            )
            .await?;
        if let Some((_name, value, _id)) = resp.method_responses.first() {
            if let Some(err) = value.get("error").filter(|v| !v.is_null()) {
                let desc = err
                    .get("description")
                    .and_then(|v| v.as_str())
                    .unwrap_or("SieveScript/set rejected");
                return Err(anyhow!("SieveScript/set rejected: {desc}"));
            }
            if let Some(not_updated) = value.get("notUpdated").and_then(|v| v.as_object())
                && let Some(entry) = not_updated.get(username)
            {
                let desc = entry
                    .get("description")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown");
                return Err(anyhow!("SieveScript/set failed: {desc}"));
            }
            return Ok(());
        }
        Err(anyhow!("SieveScript/set response missing outcome"))
    }

    /// Submit an already-saved email (draft) for delivery via JMAP
    /// `EmailSubmission/set` (RFC 8621 §2.7), referencing the existing
    /// `email_id`. `envelope_to` is the recipient address list for the
    /// envelope (`rcptTo`). This is the `RopSubmitMessage` / `RopTransportSend`
    /// backend op.
    pub async fn submit_existing_email(
        &self,
        account_id: &str,
        email_id: &str,
        envelope_from: &str,
        envelope_to: &[String],
        username: &str,
        password: &SecretString,
    ) -> Result<()> {
        let session = self.get_session(username, password).await?;
        let api_url = &session.api_url;

        // Resolve the Drafts and Sent mailboxes so that, on *successful*
        // submission only, the draft is moved out of Drafts and into Sent (and
        // its `$draft` keyword cleared). This mirrors RFC 8621 §7.5.1: the
        // onSuccessUpdateEmail patch is keyed by the EmailSubmission creation
        // id, and only runs after the submission has been accepted by the
        // server — a failed send therefore never leaves a "ghost" copy in Sent.
        // Lookup failures propagate (rather than degrade to an empty set) and
        // absent mailbox roles are rejected, ensuring the patch can never leave
        // the Email with zero mailboxIds (forbidden by RFC 8621 §4.1).
        let (drafts_ids, sent_ids) = self
            .resolve_submission_mailboxes(account_id, username, password)
            .await?;
        let patch = build_submission_success_patch(&drafts_ids, &sent_ids)?;

        let calls = vec![(
            "EmailSubmission/set",
            json!({
                "accountId": account_id,
                "create": {
                    "s0": {
                        "emailId": email_id,
                        "envelope": {
                            "mailFrom": { "email": envelope_from },
                            "rcptTo": envelope_to.iter().map(|a| json!({ "email": a })).collect::<Vec<_>>(),
                        },
                    },
                },
                "onSuccessUpdateEmail": {
                    "#s0": patch,
                },
            }),
            "ess0",
        )];
        let resp = self
            .api_call(
                api_url,
                &[
                    "urn:ietf:params:jmap:core",
                    "urn:ietf:params:jmap:mail",
                    "urn:ietf:params:jmap:submission",
                ],
                calls,
                username,
                password,
            )
            .await?;
        for (method, data, _) in resp.method_responses {
            if method == "EmailSubmission/set"
                && let Some(not_created) = data.get("notCreated")
                && !not_created.is_null()
                && not_created.as_object().is_none_or(|o| !o.is_empty())
            {
                return Err(anyhow!("EmailSubmission/set submit failed: {not_created}"));
            }
        }
        Ok(())
    }

    /// Get the current JMAP Email data state token.
    ///
    /// Per RFC 8621 §4.1, `Email/get` with `ids: []` returns the current
    /// `state` property without fetching any email data. This state token
    /// is required as `sinceState` for subsequent `Email/changes` calls.
    ///
    /// Returns the state string on success.
    pub async fn get_email_state(
        &self,
        account_id: &str,
        username: &str,
        password: &SecretString,
    ) -> Result<String> {
        let session = self.get_session(username, password).await?;
        let api_url = &session.api_url;

        // Per RFC 8621 §4.1, Email/get with empty ids returns just the state
        let method_calls = vec![(
            "Email/get",
            json!({
                "accountId": account_id,
                "ids": [],
            }),
            "g0",
        )];

        let response = self
            .api_call(
                api_url,
                &["urn:ietf:params:jmap:core", "urn:ietf:params:jmap:mail"],
                method_calls,
                username,
                password,
            )
            .await?;

        for (method, data, _) in response.method_responses {
            if method == "Email/get" {
                let state = data
                    .get("state")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                if state.is_empty() {
                    return Err(anyhow!("JMAP Email/get returned empty state token"));
                }
                return Ok(state);
            }
        }

        Err(anyhow!(
            "Unexpected JMAP response structure for Email/get state"
        ))
    }
    /// Submit an email via JMAP EmailSubmission/set (RFC 8621 §2.7).
    ///
    /// This replaces SMTP submission. The flow per RFC 8621 §7.5.1 is:
    /// 1. Create the draft Email via Email/set in the Drafts mailbox with the
    ///    `$draft` keyword set.
    /// 2. Create an EmailSubmission via EmailSubmission/set referencing the
    ///    created email, with an `onSuccessUpdateEmail` patch that moves the
    ///    message out of Drafts into Sent and clears `$draft` — this patch runs
    ///    only after the server has accepted the submission, so a failed send
    ///    never leaves a "ghost" copy in Sent.
    ///
    /// Returns the created email ID on success.
    pub async fn submit_email(&self, params: SubmitEmailParams<'_>) -> Result<String> {
        let session = self.get_session(params.username, params.password).await?;
        let api_url = &session.api_url;

        // Resolve the Drafts and Sent mailboxes. The message is initially placed
        // in Drafts and moved to Sent by the server only on successful submission.
        // Lookup failures propagate and absent roles are rejected, so the email
        // can never be created in (or patched into) an invalid mailbox state.
        let (drafts_ids, sent_ids) = self
            .resolve_submission_mailboxes(params.account_id, params.username, params.password)
            .await?;
        let success_patch = build_submission_success_patch(&drafts_ids, &sent_ids)?;

        // Build the email object per RFC 8621 §4.1. The message is created in
        // every resolved Drafts mailbox (normally exactly one) as a draft.
        let mailbox_ids = drafts_ids
            .iter()
            .map(|mb_id| (mb_id.clone(), json!(true)))
            .collect::<serde_json::Map<String, serde_json::Value>>();

        let mut email_obj = json!({
            "mailboxIds": serde_json::Value::Object(mailbox_ids),
            "keywords": { "$draft": true },
            "from": [{ "email": params.from }],
            "to": params.to.iter().map(|addr| json!({ "email": addr })).collect::<Vec<_>>(),
            "subject": params.subject,
        });

        if !params.cc.is_empty() {
            email_obj["cc"] = json!(
                params
                    .cc
                    .iter()
                    .map(|addr| json!({ "email": addr }))
                    .collect::<Vec<_>>()
            );
        }
        if !params.bcc.is_empty() {
            email_obj["bcc"] = json!(
                params
                    .bcc
                    .iter()
                    .map(|addr| json!({ "email": addr }))
                    .collect::<Vec<_>>()
            );
        }

        // Threading headers (RFC 8621 §4.1.3). JMAP normalises both to lists of
        // message IDs without angle brackets.
        if let Some(irt) = params.in_reply_to
            && !irt.is_empty()
        {
            email_obj["inReplyTo"] = json!(irt);
        }
        if let Some(refs) = params.references
            && !refs.is_empty()
        {
            email_obj["references"] = json!(refs);
        }

        // Construct bodyValues, textBody, and htmlBody per RFC 8621 §4.1.4.
        // When both text and HTML are present, we provide both alternatives.
        // When only one is present, we provide only that one.

        let mut body_values = serde_json::Map::new();
        let mut text_body = None;
        let mut html_body = None;

        // Always include the provided text_body as text/plain
        if !params.text_body.is_empty() {
            body_values.insert(
                "text".to_string(),
                json!({
                    "value": params.text_body,
                    "type": "text/plain",
                    "charset": "utf-8",
                    "isEncodingProblem": false,
                    "isTruncated": false,
                }),
            );
            text_body = Some(json!([{ "partId": "text", "type": "text/plain" }]));
        }

        // If HTML body provided, include it
        if let Some(html) = params.html_body {
            body_values.insert(
                "html".to_string(),
                json!({
                    "value": html,
                    "type": "text/html",
                    "charset": "utf-8",
                    "isEncodingProblem": false,
                    "isTruncated": false,
                }),
            );
            html_body = Some(json!([{ "partId": "html", "type": "text/html" }]));
        }

        // If at least one body exists, attach bodyValues and corresponding *Body arrays
        if !body_values.is_empty() {
            email_obj["bodyValues"] = serde_json::Value::Object(body_values);
            if let Some(tb) = text_body {
                email_obj["textBody"] = tb;
            }
            if let Some(hb) = html_body {
                email_obj["htmlBody"] = hb;
            }
        }

        // Step 1: Create the Email via Email/set (as a draft).
        // Step 2: Create EmailSubmission referencing the created email, with an
        //         onSuccessUpdateEmail patch that moves drafts -> sent and clears
        //         `$draft` only after the submission is accepted.
        // Both calls are batched in a single JMAP request for atomicity.
        let method_calls = vec![
            (
                "Email/set",
                json!({
                    "accountId": params.account_id,
                    "create": {
                        "e0": email_obj,
                    },
                }),
                "es0",
            ),
            (
                "EmailSubmission/set",
                json!({
                    "accountId": params.account_id,
                    "create": {
                        "s0": {
                            "emailId": "#e0",
                            "envelope": {
                                "mailFrom": { "email": params.from },
                                "rcptTo": params.to.iter()
                                    .chain(params.cc.iter())
                                    .chain(params.bcc.iter())
                                    .map(|addr| json!({ "email": addr }))
                                    .collect::<Vec<_>>(),
                            },
                        },
                    },
                    "onSuccessUpdateEmail": {
                        "#s0": success_patch,
                    },
                }),
                "ess0",
            ),
        ];

        let response = self
            .api_call(
                api_url,
                &[
                    "urn:ietf:params:jmap:core",
                    "urn:ietf:params:jmap:mail",
                    "urn:ietf:params:jmap:submission",
                ],
                method_calls,
                params.username,
                params.password,
            )
            .await?;

        // Check for errors in Email/set and EmailSubmission/set
        let mut email_id = String::new();
        for (method, data, _) in &response.method_responses {
            if method == "Email/set" {
                if let Some(not_created) = data.get("notCreated")
                    && !not_created.is_null()
                    && not_created.as_object().is_none_or(|o| !o.is_empty())
                {
                    return Err(anyhow!("Email/set failed: {}", not_created));
                }
                if let Some(created) = data.get("created")
                    && let Some(e0) = created.get("e0")
                {
                    email_id = e0
                        .get("id")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                }
            }
            if method == "EmailSubmission/set"
                && let Some(not_created) = data.get("notCreated")
                && !not_created.is_null()
                && not_created.as_object().is_none_or(|o| !o.is_empty())
            {
                return Err(anyhow!("EmailSubmission/set failed: {}", not_created));
            }
        }

        if email_id.is_empty() {
            warn!("JMAP Email/set succeeded but no email ID returned");
        }

        Ok(email_id)
    }

    /// Get all mailbox IDs for a given JMAP role (e.g., "inbox", "sent").
    /// This is used to filter emails via `mailboxIds` instead of `inMailboxRole`,
    /// which is not supported by some JMAP servers (e.g., older Stalwart versions).
    pub async fn get_mailbox_ids_for_role(
        &self,
        account_id: &str,
        role: &str,
        username: &str,
        password: &SecretString,
    ) -> Result<Vec<String>> {
        let session = self.get_session(username, password).await?;
        let api_url = &session.api_url;

        let method_calls = vec![(
            "Mailbox/query",
            json!({
                "accountId": account_id,
                "filter": { "role": role },
            }),
            "mq0",
        )];

        let response = self
            .api_call(
                api_url,
                &["urn:ietf:params:jmap:core", "urn:ietf:params:jmap:mail"],
                method_calls,
                username,
                password,
            )
            .await?;

        let mut ids = Vec::new();
        let mut found = false;
        for (method, data, _) in response.method_responses {
            if method == "Mailbox/query" {
                found = true;
                if let Some(arr) = data.get("ids").and_then(|v| v.as_array()) {
                    for id_val in arr {
                        if let Some(s) = id_val.as_str() {
                            ids.push(s.to_string());
                        }
                    }
                } else {
                    tracing::warn!(role = %role, "Mailbox/query response missing 'ids' array");
                    return Err(anyhow!(
                        "Malformed Mailbox/query response: missing ids array"
                    ));
                }
            }
        }

        if !found {
            tracing::warn!(role = %role, "No Mailbox/query response present");
            return Err(anyhow!("Mailbox/query response missing"));
        }

        Ok(ids)
    }

    /// Import a raw RFC 5322 message into a mailbox via Email/import
    /// (RFC 8621 §4.1.9). Used to honour EAS `SaveInSentItems` when a message
    /// was relayed as verbatim raw MIME over SMTP (attachments, S/MIME,
    /// TNEF), so the copy in Sent Items is byte-identical to what was sent.
    ///
    /// Returns the created email ID.
    pub async fn import_email(
        &self,
        account_id: &str,
        blob_id: &str,
        mailbox_ids: &[String],
        username: &str,
        password: &SecretString,
    ) -> Result<String> {
        if mailbox_ids.is_empty() {
            return Err(anyhow!("Email/import requires at least one mailbox"));
        }
        let session = self.get_session(username, password).await?;
        let api_url = &session.api_url;

        let mailbox_json: serde_json::Map<String, serde_json::Value> = mailbox_ids
            .iter()
            .map(|id| (id.clone(), json!(true)))
            .collect();

        let method_calls = vec![(
            "Email/import",
            json!({
                "accountId": account_id,
                "emails": {
                    "i0": {
                        "blobId": blob_id,
                        "mailboxIds": serde_json::Value::Object(mailbox_json),
                        "keywords": { "$seen": true },
                        "receivedAt": chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
                    },
                },
            }),
            "ei0",
        )];

        let response = self
            .api_call(
                api_url,
                &["urn:ietf:params:jmap:core", "urn:ietf:params:jmap:mail"],
                method_calls,
                username,
                password,
            )
            .await?;

        for (method, data, _) in response.method_responses {
            if method == "Email/import" {
                if let Some(not_created) = data.get("notCreated")
                    && !not_created.is_null()
                    && not_created.as_object().is_none_or(|o| !o.is_empty())
                {
                    return Err(anyhow!("Email/import failed: {}", not_created));
                }
                if let Some(created) = data.get("created")
                    && let Some(i0) = created.get("i0")
                    && let Some(id) = i0.get("id").and_then(|v| v.as_str())
                {
                    return Ok(id.to_string());
                }
                return Err(anyhow!("Email/import returned no created id"));
            }
        }

        Err(anyhow!(
            "Unexpected JMAP response structure for Email/import"
        ))
    }

    /// Resolve the Drafts and Sent mailboxes required to submit an email.
    ///
    /// Returns an error (rather than an empty set) if either role cannot be
    /// resolved or maps to zero mailboxes. This guarantees a subsequent
    /// `onSuccessUpdateEmail` patch can always move the message out of Drafts
    /// and into Sent without ever leaving it with zero `mailboxIds`, which
    /// RFC 8621 §4.1 forbids.
    async fn resolve_submission_mailboxes(
        &self,
        account_id: &str,
        username: &str,
        password: &SecretString,
    ) -> Result<(Vec<String>, Vec<String>)> {
        let drafts_ids = self
            .get_mailbox_ids_for_role(account_id, "drafts", username, password)
            .await?;
        let sent_ids = self
            .get_mailbox_ids_for_role(account_id, "sent", username, password)
            .await?;
        if drafts_ids.is_empty() {
            return Err(anyhow!("No Drafts mailbox found for account {account_id}"));
        }
        if sent_ids.is_empty() {
            return Err(anyhow!("No Sent mailbox found for account {account_id}"));
        }
        Ok((drafts_ids, sent_ids))
    }

    /// Query mailboxes for an account.
    ///
    /// Maps to `Mailbox/query` (RFC 8621 §5.3).
    pub async fn query_mailboxes(
        &self,
        username: &str,
        password: &SecretString,
    ) -> Result<MailboxListResult> {
        let session = self.get_session(username, password).await?;
        let api_url = &session.api_url;

        // Find the primary account
        let account_id = session
            .primary_accounts
            .get("urn:ietf:params:jmap:mail")
            .ok_or_else(|| anyhow!("No primary mail account found in JMAP session"))?;

        let method_calls = vec![
            (
                "Mailbox/query",
                json!({
                    "accountId": account_id,
                    "calculateTotal": true,
                }),
                "mq0",
            ),
            (
                "Mailbox/get",
                json!({
                    "accountId": account_id,
                    "#ids": {
                        "resultOf": "mq0",
                        "name": "Mailbox/query",
                        "path": "/ids",
                    },
                    "properties": [
                        "id", "name", "parentId", "role", "sortOrder",
                        "totalEmails", "unreadEmails", "totalThreads", "unreadThreads",
                        "isSubscribed", "shareWith"
                    ],
                }),
                "mg0",
            ),
        ];

        let response = self
            .api_call(
                api_url,
                &["urn:ietf:params:jmap:core", "urn:ietf:params:jmap:mail"],
                method_calls,
                username,
                password,
            )
            .await?;

        for (method, data, _) in response.method_responses {
            if method == "Mailbox/get" {
                let list: Vec<JmapMailbox> = data
                    .get("list")
                    .and_then(|v| serde_json::from_value(v.clone()).ok())
                    .unwrap_or_default();
                let total: u64 = list.len() as u64;
                return Ok(MailboxListResult {
                    mailboxes: list,
                    total,
                });
            }
        }

        Err(anyhow!(
            "Unexpected JMAP response structure for Mailbox/get"
        ))
    }

    /// Page size for [`Self::query_all_mailboxes`]'s `Mailbox/query` walk.
    const MAILBOX_QUERY_PAGE: u64 = 500;

    /// Enumerate **every** mailbox of the primary account, paging
    /// `Mailbox/query` by `position`/`limit` until the full list is drained.
    ///
    /// `query_mailboxes` issues a single unpaged `Mailbox/query`; a server may
    /// clamp `limit` (RFC 8620 §5.5) and return only a prefix, so a caller that
    /// needs the complete set (the subtree walks behind `RopDeleteFolder` and
    /// EWS `DeleteFolder` hard delete) must paginate — otherwise a large
    /// hierarchy leaves descendants unvisited and the parent destroy fails with
    /// `mailboxHasChild`.
    pub async fn query_all_mailboxes(
        &self,
        username: &str,
        password: &SecretString,
    ) -> Result<MailboxListResult> {
        let session = self.get_session(username, password).await?;
        let api_url = &session.api_url;
        let account_id = session
            .primary_accounts
            .get("urn:ietf:params:jmap:mail")
            .ok_or_else(|| anyhow!("No primary mail account found in JMAP session"))?;

        let mut all: Vec<JmapMailbox> = Vec::new();
        let mut position: u64 = 0;
        loop {
            let method_calls = vec![
                (
                    "Mailbox/query",
                    json!({
                        "accountId": account_id,
                        "position": position,
                        "limit": Self::MAILBOX_QUERY_PAGE,
                        "calculateTotal": true,
                    }),
                    "mq0",
                ),
                (
                    "Mailbox/get",
                    json!({
                        "accountId": account_id,
                        "#ids": {
                            "resultOf": "mq0",
                            "name": "Mailbox/query",
                            "path": "/ids",
                        },
                        "properties": [
                            "id", "name", "parentId", "role", "sortOrder",
                            "totalEmails", "unreadEmails", "totalThreads", "unreadThreads",
                            "isSubscribed", "shareWith"
                        ],
                    }),
                    "mg0",
                ),
            ];
            let response = self
                .api_call(
                    api_url,
                    &["urn:ietf:params:jmap:core", "urn:ietf:params:jmap:mail"],
                    method_calls,
                    username,
                    password,
                )
                .await?;

            let mut page_len: u64 = 0;
            let mut total: Option<u64> = None;
            let mut saw_get = false;
            for (method, data, _) in response.method_responses {
                if method == "error" {
                    return Err(anyhow!("Mailbox/query returned method error: {}", data));
                }
                if method == "Mailbox/query" {
                    page_len = data
                        .get("ids")
                        .and_then(|v| v.as_array())
                        .map(|a| a.len() as u64)
                        .unwrap_or(0);
                    total = data.get("total").and_then(|v| v.as_u64());
                } else if method == "Mailbox/get" {
                    saw_get = true;
                    let mut list: Vec<JmapMailbox> = data
                        .get("list")
                        .and_then(|v| serde_json::from_value(v.clone()).ok())
                        .unwrap_or_default();
                    all.append(&mut list);
                }
            }
            if !saw_get {
                return Err(anyhow!(
                    "Unexpected JMAP response structure for Mailbox/get"
                ));
            }
            // A zero-length page means the walk is complete. Otherwise advance
            // by the page's actual length and stop once the requested `total`
            // (calculateTotal) is reached. Using the server's own `total` rather
            // than a "short page" heuristic keeps the walk correct even when the
            // server clamps `limit` below `MAILBOX_QUERY_PAGE` — a short page
            // then does NOT mean the mailbox set is exhausted.
            if page_len == 0 {
                break;
            }
            position += page_len;
            if let Some(t) = total
                && all.len() as u64 >= t
            {
                break;
            }
        }

        let total = all.len() as u64;
        Ok(MailboxListResult {
            mailboxes: all,
            total,
        })
    }

    /// Get the primary mail account ID from the JMAP session.
    pub async fn get_account_id(&self, username: &str, password: &SecretString) -> Result<String> {
        let session = self.get_session(username, password).await?;
        session
            .primary_accounts
            .get("urn:ietf:params:jmap:mail")
            .cloned()
            .ok_or_else(|| anyhow!("No primary mail account found in JMAP session"))
    }

    /// Resolve the JMAP EventSource push endpoint (`urn:ietf:params:jmap:push`,
    /// RFC 8620 §7.2) and the matching Basic `Authorization` header for
    /// `username`, enabling a live push monitor to subscribe to server-side
    /// state changes without polling. Returns an error when the server did not
    /// advertise an `eventSourceUrl` (i.e. push is unavailable/supported).
    pub async fn push_endpoint(
        &self,
        username: &str,
        password: &SecretString,
    ) -> Result<(String, String)> {
        let session = self.get_session(username, password).await?;
        if session.event_source_url.is_empty() {
            return Err(anyhow!(
                "JMAP server did not advertise eventSourceUrl (push unsupported)"
            ));
        }
        Ok((
            session.event_source_url.clone(),
            Self::basic_auth_header(username, password),
        ))
    }

    /// Verify JMAP credentials by fetching the session.
    pub async fn verify_credentials(&self, username: &str, password: &SecretString) -> Result<()> {
        self.get_session(username, password).await?;
        Ok(())
    }

    /// Startup warm path (AUDIT.md §18): fetch and cache the JMAP session for
    /// `username` so the first authenticated API call after a container restart
    /// (directory GAL lookups, OOF, calendar capability probes) does not pay a
    /// cold session round-trip. Also primes the shared HTTP connection pool.
    ///
    /// Returns the session so the caller can log/validate it. Failures are
    /// surfaced (not swallowed) so the startup path can report a down backend.
    pub async fn warm_session(
        &self,
        username: &str,
        password: &SecretString,
    ) -> Result<JmapSession> {
        self.get_session(username, password).await
    }

    /// Cache key for a credential probe: the username plus a SHA-256 of the
    /// password (never the password itself, so the map never retains a secret).
    fn session_auth_key(username: &str, password: &SecretString) -> String {
        use sha2::{Digest, Sha256};
        let mut h = Sha256::new();
        h.update(password.expose_secret().as_bytes());
        format!("{username}:{}", const_hex::encode(h.finalize()))
    }

    /// Probe JMAP credentials, consulting a short-lived per-credential cache
    /// so the per-request Basic-auth probe that fronts EAS/EWS does not hit
    /// Stalwart on every request (AUDIT.md §18).
    ///
    /// Semantics match the higher-level `auth::AuthVerifier` cache:
    /// * a positive result is remembered for the configured positive TTL;
    /// * an authoritative rejection (`Invalid`) for the shorter negative TTL;
    /// * a backend outage (`BackendError`) is **never** cached, so recovery is
    ///   observed by the very next request.
    ///
    /// A successful verification evicts *all other* cached credentials for the
    /// same username, so a changed Stalwart password takes effect on first use.
    pub async fn probe_auth(&self, username: &str, password: &SecretString) -> CredentialCheck {
        let key = Self::session_auth_key(username, password);
        if let Some(entry) = self.session_auth.get(&key) {
            return if entry.valid {
                CredentialCheck::Valid
            } else {
                CredentialCheck::Invalid
            };
        }
        let outcome = self.check_credentials(username, password).await;
        match &outcome {
            CredentialCheck::Valid => {
                self.invalidate_other_session_auth(username, &key);
                self.session_auth
                    .insert(key, SessionAuthEntry { valid: true });
            }
            CredentialCheck::Invalid => {
                self.session_auth
                    .insert(key, SessionAuthEntry { valid: false });
            }
            CredentialCheck::BackendError(_) => {
                // Never cache an outage: the next request must re-probe so a
                // recovered Stalwart is observed immediately.
            }
        }
        outcome
    }

    /// Evict every cached credential probe for `username` other than `keep_key`.
    ///
    /// Called only after a *successful* verification, so a changed password
    /// stops working at the gateway as soon as the new one is first used rather
    /// than lingering for the (short) negative TTL.
    fn invalidate_other_session_auth(&self, username: &str, keep_key: &str) {
        let prefix = format!("{username}:");
        let stale: Vec<String> = self
            .session_auth
            .iter()
            .map(|(k, _)| k.as_str().to_string())
            .filter(|k| k.starts_with(&prefix) && k != keep_key)
            .collect();
        for key in stale {
            self.session_auth.invalidate(&key);
        }
    }

    /// Drop every cached session-auth outcome for `username` (all passwords).
    ///
    /// Used by `AuthVerifier::invalidate_user` so an explicit invalidation —
    /// e.g. after a password change or an operator reset — is not shadowed by a
    /// still-fresh positive in this cache.
    pub fn invalidate_user_session_auth(&self, username: &str) {
        let prefix = format!("{username}:");
        let stale: Vec<String> = self
            .session_auth
            .iter()
            .map(|(k, _)| k.as_str().to_string())
            .filter(|k| k.starts_with(&prefix))
            .collect();
        for key in stale {
            self.session_auth.invalidate(&key);
        }
    }

    /// Probe the JMAP session to classify the credential state, preserving the
    /// 401/403 vs 5xx/transport distinction (AUDIT.md §17).
    ///
    /// Unlike [`JmapClient::verify_credentials`], which collapses every failure
    /// into an `Err`, this returns [`CredentialCheck::Invalid`] only for an
    /// authoritative auth rejection and [`CredentialCheck::BackendError`] for a
    /// backend outage, so the caller never turns a Stalwart hiccup into a
    /// password prompt.
    pub async fn check_credentials(
        &self,
        username: &str,
        password: &SecretString,
    ) -> CredentialCheck {
        let url = format!("{}/session", self.base_url);
        let auth = Self::basic_auth_header(username, password);
        let resp = match self
            .client
            .get(&url)
            .header(AUTHORIZATION, &auth)
            .send()
            .await
        {
            Ok(r) => r,
            Err(e) => {
                return CredentialCheck::BackendError(format!(
                    "JMAP session request failed: {}",
                    e
                ));
            }
        };
        let status = resp.status();
        if status.is_success() {
            // A bare 2xx is not proof of valid credentials: a 204, a proxy's
            // HTML 200, or any other non-JMAP responder can return one without
            // ever having authenticated anyone. Require the body to be a real
            // RFC 8621 §2 JMAP session object; a parse failure is a backend
            // error (retryable), never a cached positive.
            match resp.json::<JmapSession>().await {
                Ok(_) => CredentialCheck::Valid,
                Err(e) => CredentialCheck::BackendError(format!(
                    "JMAP session response was not a valid session object: {}",
                    e
                )),
            }
        } else if status == reqwest::StatusCode::UNAUTHORIZED
            || status == reqwest::StatusCode::FORBIDDEN
        {
            CredentialCheck::Invalid
        } else {
            let body = resp.text().await.unwrap_or_default();
            CredentialCheck::BackendError(format!(
                "JMAP session request returned {}: {}",
                status, body
            ))
        }
    }

    /// Health check: verify JMAP endpoint is reachable.
    pub async fn health_check(&self) -> Result<()> {
        self.health_probes
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let url = format!("{}/session", self.base_url);
        let resp = self
            .client
            .get(&url)
            .timeout(std::time::Duration::from_secs(5))
            .send()
            .await
            .map_err(|e| anyhow!("JMAP health check failed: {}", e))?;

        match resp.status().as_u16() {
            200..=299 | 401 | 403 => {
                debug!(target: "jmap", url = %url, "JMAP health check passed");
                Ok(())
            }
            status => {
                warn!(target: "jmap", url = %url, status = status, "JMAP health check failed");
                Err(anyhow!("JMAP endpoint returned status {}", status))
            }
        }
    }

    // -----------------------------------------------------------------------
    // JMAP Calendar methods (draft-ietf-jmap-calendars-26)
    // -----------------------------------------------------------------------

    /// Get the primary calendar account ID from the JMAP session.
    ///
    /// Per draft-ietf-jmap-calendars §1.5.1, the capability URN is
    /// `urn:ietf:params:jmap:calendars`. The primary_accounts map in
    /// the session object maps this URN to the account ID.
    pub async fn get_calendar_account_id(
        &self,
        username: &str,
        password: &SecretString,
    ) -> Result<String> {
        let session = self.get_session(username, password).await?;
        session
            .primary_accounts
            .get(JMAP_CAL_CAPABILITY)
            .cloned()
            .ok_or_else(|| anyhow!("No primary calendar account found in JMAP session"))
    }

    /// Check whether the JMAP server supports calendar operations.
    ///
    /// Returns true if `urn:ietf:params:jmap:calendars` appears in
    /// the session's top-level capabilities.
    pub async fn supports_calendar(&self, username: &str, password: &SecretString) -> bool {
        match self.get_session(username, password).await {
            Ok(session) => session.capabilities.contains_key(JMAP_CAL_CAPABILITY),
            Err(_) => false,
        }
    }

    /// Check whether the JMAP server advertises the principals-availability
    /// capability (`urn:ietf:params:jmap:principals:availability`). When it
    /// does, free/busy is served by `Principal/getAvailability` (the correct
    /// JMAP free/busy primitive); when it does not, callers fall back to the
    /// calendar-event query path. A session-fetch error is `false` (the
    /// caller then falls back and surfaces its own backend error).
    pub async fn supports_availability(&self, username: &str, password: &SecretString) -> bool {
        match self.get_session(username, password).await {
            Ok(session) => session
                .capabilities
                .contains_key(JMAP_CAL_AVAILABILITY_CAPABILITY),
            Err(_) => false,
        }
    }

    /// Deploy-time verification of the JMAP Calendars capability
    /// (`urn:ietf:params:jmap:calendars`, draft-ietf-jmap-calendars §1.5.1)
    /// against the configured backend.
    ///
    /// Unlike `supports_calendar` (a boolean probe used in hot sync paths),
    /// this performs an exhaustive, diagnostics-oriented check intended to run
    /// once at container startup/deploy time:
    ///   1. the session advertises the calendars capability;
    ///   2. the capability object is parsed and every draft §1.5.1 field is
    ///      extracted (`maxCalendarsPerEvent`, `minDateTime`, `maxDateTime`,
    ///      `maxExpandedQueryDuration`, `maxParticipantsPerEvent`,
    ///      `mayCreateCalendar`);
    ///   3. `Calendar/get` and `CalendarEvent/query` succeed against the
    ///      primary account, proving the server accepts the calendar method
    ///      family the gateway relies on (including `CalendarEvent/set`,
    ///      which shares the capability).
    ///
    /// All problems are collected into `report.issues` instead of failing on
    /// the first error so a deployment log shows the full picture. The
    /// network/session failure to even reach JMAP is returned as `Err`.
    pub async fn verify_calendar_capability_deploy(
        &self,
        username: &str,
        password: &SecretString,
    ) -> Result<CalendarCapabilityReport> {
        let session = self.get_session(username, password).await?;

        let mut report = CalendarCapabilityReport {
            capability_present: session.capabilities.contains_key(JMAP_CAL_CAPABILITY),
            ..Default::default()
        };

        if !report.capability_present {
            report.issues.push(format!(
                "session does not advertise {} — the gateway will fall back to CalDAV, losing JMAP-native features",
                JMAP_CAL_CAPABILITY
            ));
            return Ok(report);
        }

        let details = session
            .capabilities
            .get(JMAP_CAL_CAPABILITY)
            .cloned()
            .unwrap_or(Value::Null);
        report.extract_capability_details(&details);

        report.account_id = session.primary_accounts.get(JMAP_CAL_CAPABILITY).cloned();
        let Some(account_id) = report.account_id.clone() else {
            report
                .issues
                .push("no primary account for the calendars capability".to_string());
            return Ok(report);
        };

        // Probe Calendar/get.
        match self
            .api_call(
                &session.api_url,
                &["urn:ietf:params:jmap:core", JMAP_CAL_CAPABILITY],
                vec![(
                    "Calendar/get",
                    json!({ "accountId": account_id, "ids": null, "properties": ["id", "name"] }),
                    "c0",
                )],
                username,
                password,
            )
            .await
        {
            Ok(resp)
                if resp
                    .method_responses
                    .iter()
                    .any(|(m, _, _)| m == "Calendar/get") =>
            {
                report.calendar_get_ok = true;
            }
            Ok(resp) => {
                report.issues.push(format!(
                    "Calendar/get returned no success response: {:?}",
                    resp.method_responses
                ));
            }
            Err(e) => {
                report.issues.push(format!("Calendar/get failed: {}", e));
            }
        }

        // Probe CalendarEvent/query (exercises the event read family).
        match self
            .api_call(
                &session.api_url,
                &["urn:ietf:params:jmap:core", JMAP_CAL_CAPABILITY],
                vec![(
                    "CalendarEvent/query",
                    json!({ "accountId": account_id, "filter": {}, "limit": 1 }),
                    "c1",
                )],
                username,
                password,
            )
            .await
        {
            Ok(resp)
                if resp
                    .method_responses
                    .iter()
                    .any(|(m, _, _)| m == "CalendarEvent/query") =>
            {
                report.calendar_event_query_ok = true;
            }
            Ok(resp) => {
                report.issues.push(format!(
                    "CalendarEvent/query returned no success response: {:?}",
                    resp.method_responses
                ));
            }
            Err(e) => {
                report
                    .issues
                    .push(format!("CalendarEvent/query failed: {}", e));
            }
        }

        // Write probes. Verified live against stalwartlabs/stalwart:v0.16.22
        // and v1.0.3:
        //  - RFC-form `recurrenceRules` arrays are REJECTED with
        //    `invalidProperties`; the implementation speaks the older-draft
        //    form `recurrenceRule` (single object) + `recurrenceOverrides`.
        //  - An event written with a raw `iCalendar` payload comes back with
        //    NO content (title/start/duration dropped), which is why
        //    `set_calendar_event` converts the ICS to structured JSCalendar
        //    properties (`ics_to_jscalendar_event`) before writing. Both
        //    behaviours are fatal to gateway event creation and must be
        //    detected at deploy time, so probe both.
        self.probe_write_paths(&session, &account_id, username, password, &mut report)
            .await;

        Ok(report)
    }

    /// Probe the two write paths the gateway can use to create events:
    /// structured JSCalendar properties (with a recurrence + an excluded
    /// occurrence) and the ICS→structured conversion the gateway actually
    /// writes (`ics_to_jscalendar_event` + `jscalendar_write_payload`). The
    /// first catches a build that rejects the structured form; the second
    /// catches a build that silently drops event content.
    async fn probe_write_paths(
        &self,
        session: &JmapSession,
        account_id: &str,
        username: &str,
        password: &SecretString,
        report: &mut CalendarCapabilityReport,
    ) {
        let using = ["urn:ietf:params:jmap:core", JMAP_CAL_CAPABILITY];

        // The probes write and destroy events. They must never touch a real
        // user calendar: when cleanup fails (backend outage mid-probe, auth
        // expiry, process kill), leftovers in a personal calendar would
        // accumulate on every container start. Run everything inside a
        // dedicated, uniquely-named probe calendar instead, so any leftover
        // is isolated, identifiable by name, and removable as one object.
        if report.may_create_calendar == Some(false) {
            // Already flagged as an issue by extract_capability_details.
            report.issues.push(
                "write probe: skipped — server reports mayCreateCalendar=false, \
                 cannot create an isolated probe calendar"
                    .to_string(),
            );
            return;
        }
        let probe_calendar_name = format!("exchange-gateway-deploy-probe-{}", uuid::Uuid::new_v4());
        let probe_calendar_id = match self
            .api_call(
                &session.api_url,
                &using,
                vec![(
                    "Calendar/set",
                    json!({
                        "accountId": account_id,
                        "create": { "probe": { "name": probe_calendar_name } }
                    }),
                    "p0",
                )],
                username,
                password,
            )
            .await
        {
            Ok(resp) => {
                let created_id = resp
                    .method_responses
                    .iter()
                    .find(|(m, _, _)| m == "Calendar/set")
                    .and_then(|(_, a, _)| a.get("created"))
                    .and_then(|c| c.get("probe"))
                    .and_then(|p| p.get("id"))
                    .and_then(|v| v.as_str())
                    .map(str::to_string);
                match created_id {
                    Some(id) => id,
                    None => {
                        report.issues.push(format!(
                            "write probe: could not create an isolated probe calendar (responses: {:?}); skipping write probes rather than writing into a real user calendar",
                            resp.method_responses
                        ));
                        return;
                    }
                }
            }
            Err(e) => {
                report.issues.push(format!(
                    "write probe: could not create an isolated probe calendar: {}; skipping write probes rather than writing into a real user calendar",
                    e
                ));
                return;
            }
        };
        let calendar_id = probe_calendar_id.clone();

        // --- Probe 1: structured recurring event with one excluded occurrence.
        let struct_uid = format!("gateway-cal-probe-struct-{}", uuid::Uuid::new_v4());
        let id1 = self
            .probe_create(
                session,
                account_id,
                username,
                password,
                json!({
                    "calendarIds": { calendar_id.clone(): true },
                    "uid": struct_uid,
                    "title": struct_uid,
                    "start": "2026-03-02T09:00:00",
                    "timeZone": "Europe/Berlin",
                    "duration": "PT1H",
                    "recurrenceRule": { "frequency": "weekly" },
                    "recurrenceOverrides": {
                        "2026-03-09T09:00:00": { "excluded": true }
                    }
                }),
                report,
            )
            .await;
        if let Some(id1) = id1.as_deref() {
            match self
                .probe_get(session, account_id, username, password, id1)
                .await
            {
                Some(ev) => {
                    let rule_ok = ev
                        .get("recurrenceRule")
                        .and_then(|r| r.get("frequency"))
                        .and_then(|v| v.as_str())
                        == Some("weekly");
                    let override_ok = ev
                        .get("recurrenceOverrides")
                        .and_then(|o| o.get("2026-03-09T09:00:00"))
                        .and_then(|o| o.get("excluded"))
                        .and_then(|v| v.as_bool())
                        == Some(true);
                    if rule_ok && override_ok {
                        report.structured_recurrence_ok = true;
                    } else {
                        report.issues.push(format!(
                            "write probe: recurring event did not round-trip (recurrenceRule ok: {}, override ok: {}): {}",
                            rule_ok, override_ok, ev
                        ));
                    }
                }
                None => report
                    .issues
                    .push("write probe: structured event missing on read-back".to_string()),
            }
            // It must also appear in its first-occurrence time-range query.
            if !self
                .probe_in_window(session, account_id, username, password, id1)
                .await
            {
                report.issues.push(
                    "write probe: recurring event not found in its first-occurrence time-range query"
                        .to_string(),
                );
                report.structured_recurrence_ok = false;
            }
        }

        // --- Probe 2: the gateway's actual create path — an ICS document
        // converted to structured JSCalendar properties via the same helper
        // `set_calendar_event` uses (`ics_to_jscalendar_event` +
        // `jscalendar_write_payload`). This is what must round-trip: a raw
        // `iCalendar` payload is discarded by Stalwart, which is precisely why
        // the gateway converts before writing.
        let ics_uid = format!("gateway-cal-probe-ics-{}", uuid::Uuid::new_v4());
        let ics = format!(
            "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//exchange-gateway//deploy-check//EN\r\n\
             BEGIN:VEVENT\r\nUID:{ics_uid}\r\nDTSTAMP:20260301T000000Z\r\n\
             DTSTART;TZID=Europe/Berlin:20260302T090000\r\nDTEND;TZID=Europe/Berlin:20260302T100000\r\n\
             SUMMARY:{ics_uid}\r\nRRULE:FREQ=WEEKLY\r\nEXDATE;TZID=Europe/Berlin:20260309T090000\r\n\
             END:VEVENT\r\nEND:VCALENDAR\r\n"
        );
        let id2 = match crate::jscalendar::ics_to_jscalendar_event(&ics) {
            Some(entry) => {
                let mut payload = crate::jscalendar::jscalendar_write_payload(&entry, false);
                if let Some(obj) = payload.as_object_mut() {
                    obj.insert("calendarIds".to_string(), json!({ (calendar_id): true }));
                }
                let id2 = self
                    .probe_create(session, account_id, username, password, payload, report)
                    .await;
                if let Some(id2) = id2.as_deref() {
                    match self
                        .probe_get(session, account_id, username, password, id2)
                        .await
                    {
                        Some(ev) => match crate::jscalendar::verify_write_round_trip(&ev, &entry) {
                            Ok(()) => report.ics_round_trip_ok = true,
                            Err(e) => {
                                let detail = if e.is_discarded() {
                                    "the backend discarded the event payload"
                                } else {
                                    "the backend stored the event but altered its content"
                                };
                                report.issues.push(format!(
                                    "write probe: CalendarEvent/set accepted the structured \
                                     payload but {detail} ({}); a server build with this \
                                     behaviour silently loses gateway-created event data",
                                    e
                                ));
                            }
                        },
                        None => report
                            .issues
                            .push("write probe: iCalendar event missing on read-back".to_string()),
                    }
                }
                id2
            }
            None => {
                report.issues.push(
                    "write probe: could not convert the probe ICS into a JSCalendar payload"
                        .to_string(),
                );
                None
            }
        };

        // Cleanup. Destroy the probe events first (Calendar/set refuses to
        // destroy a non-empty calendar), then the probe calendar itself. Both
        // destroys are verified against the `destroyed` list and retried —
        // a 200 response can still carry `notDestroyed` — and any leftover
        // is reported as a health-affecting issue naming the exact ids, the
        // probe calendar id, and its unique name, so an operator can locate
        // and remove the residue manually instead of it accumulating
        // invisibly on every container restart.
        let to_destroy: Vec<String> = [id1, id2].into_iter().flatten().collect();
        if !to_destroy.is_empty() {
            self.probe_destroy_confirmed(
                DestroyProbeCtx {
                    session,
                    method: "CalendarEvent/set",
                    account_id,
                    ids: &to_destroy,
                    username,
                    password,
                },
                report,
            )
            .await;
        }
        let calendar_leftover = !self
            .probe_destroy_confirmed(
                DestroyProbeCtx {
                    session,
                    method: "Calendar/set",
                    account_id,
                    ids: std::slice::from_ref(&probe_calendar_id),
                    username,
                    password,
                },
                report,
            )
            .await;
        if calendar_leftover {
            report.issues.push(format!(
                "write probe: probe calendar '{}' (id {}) could not be removed; \
                 delete it manually in the {} account",
                probe_calendar_name, probe_calendar_id, account_id
            ));
        }
    }

    /// Destroy `ids` via `method` (`CalendarEvent/set` or `Calendar/set`),
    /// confirming every id against the returned `destroyed` list and
    /// retrying transient failures. Returns true when every id was
    /// confirmed destroyed; otherwise records a report issue naming the
    /// ids that remain on the server.
    async fn probe_destroy_confirmed(
        &self,
        ctx: DestroyProbeCtx<'_>,
        report: &mut CalendarCapabilityReport,
    ) -> bool {
        let DestroyProbeCtx {
            session,
            method,
            account_id,
            ids,
            username,
            password,
        } = ctx;
        let using = ["urn:ietf:params:jmap:core", JMAP_CAL_CAPABILITY];
        let mut remaining: Vec<String> = ids.to_vec();
        let mut last_error = String::new();
        // One attempt plus two retries for transient failures.
        for attempt in 0..3 {
            if remaining.is_empty() {
                break;
            }
            if attempt > 0 {
                tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            }
            match self
                .api_call(
                    &session.api_url,
                    &using,
                    vec![(
                        method,
                        json!({ "accountId": account_id, "destroy": remaining }),
                        "pd",
                    )],
                    username,
                    password,
                )
                .await
            {
                Ok(resp) => match resp
                    .method_responses
                    .iter()
                    .find(|(m, _, _)| *m == method)
                    .and_then(|(_, a, _)| a.get("destroyed"))
                    .and_then(|d| d.as_array())
                {
                    Some(destroyed) => {
                        let destroyed: Vec<&str> =
                            destroyed.iter().filter_map(|v| v.as_str()).collect();
                        remaining.retain(|id| !destroyed.contains(&id.as_str()));
                        if !remaining.is_empty() {
                            let not_destroyed = resp
                                .method_responses
                                .iter()
                                .find(|(m, _, _)| *m == method)
                                .and_then(|(_, a, _)| a.get("notDestroyed"))
                                .cloned()
                                .unwrap_or(Value::Null);
                            last_error = format!("notDestroyed: {}", not_destroyed);
                        }
                    }
                    None => {
                        last_error = format!(
                            "no `{}` response carrying a `destroyed` list: {:?}",
                            method, resp.method_responses
                        );
                    }
                },
                Err(e) => {
                    last_error = format!("transport error: {}", e);
                }
            }
        }
        if remaining.is_empty() {
            return true;
        }
        report.issues.push(format!(
            "write probe: cleanup via {} could not destroy {} object(s) after retries \
             ({}); leftover ids on the server: {:?}",
            method,
            remaining.len(),
            last_error,
            remaining
        ));
        false
    }

    /// Create one probe event; returns the assigned id or None (issue pushed).
    async fn probe_create(
        &self,
        session: &JmapSession,
        account_id: &str,
        username: &str,
        password: &SecretString,
        event: Value,
        report: &mut CalendarCapabilityReport,
    ) -> Option<String> {
        let using = ["urn:ietf:params:jmap:core", JMAP_CAL_CAPABILITY];
        match self
            .api_call(
                &session.api_url,
                &using,
                vec![(
                    "CalendarEvent/set",
                    json!({ "accountId": account_id, "create": { "probe": event } }),
                    "p1",
                )],
                username,
                password,
            )
            .await
        {
            Ok(resp) => {
                let args = resp
                    .method_responses
                    .iter()
                    .find(|(m, _, _)| m == "CalendarEvent/set")
                    .map(|(_, a, _)| a)
                    .cloned();
                let created_id = args
                    .as_ref()
                    .and_then(|a| a.get("created"))
                    .and_then(|c| c.get("probe"))
                    .and_then(|p| p.get("id"))
                    .and_then(|v| v.as_str())
                    .map(str::to_string);
                match created_id {
                    Some(id) => Some(id),
                    None => {
                        // A method-level `error` response never matches the
                        // `CalendarEvent/set` name, so `args`/`notCreated`
                        // alone can miss the server's actual failure —
                        // include the complete method responses.
                        report.issues.push(format!(
                            "write probe: CalendarEvent/set create rejected: {:?} (responses: {:?})",
                            args.as_ref()
                                .and_then(|a| a.get("notCreated")?.get("probe").cloned()),
                            resp.method_responses
                        ));
                        None
                    }
                }
            }
            Err(e) => {
                report
                    .issues
                    .push(format!("write probe: CalendarEvent/set failed: {}", e));
                None
            }
        }
    }

    /// Fetch one probe event with all properties.
    async fn probe_get(
        &self,
        session: &JmapSession,
        account_id: &str,
        username: &str,
        password: &SecretString,
        id: &str,
    ) -> Option<Value> {
        let using = ["urn:ietf:params:jmap:core", JMAP_CAL_CAPABILITY];
        let resp = self
            .api_call(
                &session.api_url,
                &using,
                vec![(
                    "CalendarEvent/get",
                    json!({ "accountId": account_id, "ids": [id], "properties": null }),
                    "p2",
                )],
                username,
                password,
            )
            .await
            .ok()?;
        resp.method_responses
            .iter()
            .find(|(m, _, _)| m == "CalendarEvent/get")
            .and_then(|(_, d, _)| d.get("list")?.as_array()?.first().cloned())
    }

    /// Whether `id` appears in the 2026-03-02 occurrence window.
    async fn probe_in_window(
        &self,
        session: &JmapSession,
        account_id: &str,
        username: &str,
        password: &SecretString,
        id: &str,
    ) -> bool {
        let using = ["urn:ietf:params:jmap:core", JMAP_CAL_CAPABILITY];
        let Ok(resp) = self
            .api_call(
                &session.api_url,
                &using,
                vec![(
                    "CalendarEvent/query",
                    json!({
                        "accountId": account_id,
                        "filter": {
                            "after": "2026-03-02T00:00:00Z",
                            "before": "2026-03-03T00:00:00Z"
                        }
                    }),
                    "p3",
                )],
                username,
                password,
            )
            .await
        else {
            return false;
        };
        resp.method_responses
            .iter()
            .find(|(m, _, _)| m == "CalendarEvent/query")
            .and_then(|(_, d, _)| d.get("ids")?.as_array().cloned())
            .map(|ids| ids.iter().any(|i| i.as_str() == Some(id)))
            .unwrap_or(false)
    }

    /// List calendars for the user via Calendar/get
    /// (draft-ietf-jmap-calendars §4.1).
    ///
    /// Replaces CalDAV `find_user_calendars` (PROPFIND).
    /// Returns the list of Calendar objects with their IDs and names.
    pub async fn query_calendars(
        &self,
        username: &str,
        password: &SecretString,
    ) -> Result<CalendarListResult> {
        let session = self.get_session(username, password).await?;
        let api_url = &session.api_url;
        let account_id = session
            .primary_accounts
            .get(JMAP_CAL_CAPABILITY)
            .ok_or_else(|| anyhow!("No primary calendar account found in JMAP session"))?;

        let method_calls = vec![(
            "Calendar/get",
            json!({
                "accountId": account_id,
                "properties": [
                    "id", "name", "color", "description", "sortOrder",
                    "isSubscribed", "isVisible", "href", "timeZone"
                ],
            }),
            "cg0",
        )];

        let response = self
            .api_call(
                api_url,
                &["urn:ietf:params:jmap:core", JMAP_CAL_CAPABILITY],
                method_calls,
                username,
                password,
            )
            .await?;

        for (method, data, _) in response.method_responses {
            if method == "Calendar/get" {
                let list: Vec<JmapCalendar> = data
                    .get("list")
                    .and_then(|v| serde_json::from_value(v.clone()).ok())
                    .unwrap_or_default();
                let total = list.len() as u64;
                return Ok(CalendarListResult {
                    calendars: list,
                    total,
                });
            }
        }

        Err(anyhow!(
            "Unexpected JMAP response structure for Calendar/get"
        ))
    }

    /// Get the default calendar ID for a user.
    /// Returns the first available calendar (sorted by `sortOrder` then `id`).
    /// This is used as the target for calendar CreateItem operations.
    pub async fn get_default_calendar_id(
        &self,
        username: &str,
        password: &SecretString,
    ) -> Result<String> {
        let result = self.query_calendars(username, password).await?;
        let mut calendars = result.calendars;
        // Sort by sortOrder ascending, then by id for deterministic selection
        calendars.sort_by(|a, b| {
            a.sort_order
                .cmp(&b.sort_order)
                .then_with(|| a.id.cmp(&b.id))
        });
        calendars
            .first()
            .and_then(|c| c.id.clone())
            .ok_or_else(|| anyhow!("No calendars found for user"))
    }

    /// Resolve a JMAP calendar id from a stored CalDAV collection href.
    ///
    /// A user-created calendar-family EAS folder (Types 13/15/16/17) is backed
    /// by a CalDAV collection whose href is stored in `folder_map` and used as
    /// the folder's EAS ServerId; when a client Syncs that folder, its
    /// `CollectionId` is that href. Stalwart exposes the same collection to JMAP
    /// as a `Calendar` whose `href` property ends with the collection path, so
    /// matching on a path-suffix basis scopes `CalendarEvent/query` to exactly
    /// that collection. Returns `None` when no calendar matches (an
    /// unknown/foreign href), so the caller can decline to serve rather than
    /// leak every calendar's events.
    pub async fn resolve_calendar_id_for_href(
        &self,
        username: &str,
        password: &SecretString,
        href: &str,
    ) -> Result<Option<String>> {
        let wanted = href.trim_end_matches('/');
        if wanted.is_empty() {
            return Ok(None);
        }
        let result = self.query_calendars(username, password).await?;
        Ok(result
            .calendars
            .into_iter()
            .find(|c| {
                c.href
                    .as_deref()
                    .map(|h| {
                        let h = h.trim_end_matches('/');
                        h == wanted || h.ends_with(wanted)
                    })
                    .unwrap_or(false)
            })
            .and_then(|c| c.id))
    }

    /// Query calendar events in a time range via CalendarEvent/query
    /// + CalendarEvent/get (draft-ietf-jmap-calendars §5.11, §5.7).
    ///
    /// Replaces CalDAV `query_events` (REPORT calendar-query).
    /// The `start`/`end` parameters are iCalendar UTC datetime strings
    /// (e.g. "20260101T000000Z").
    ///
    /// Events are fetched with the `iCalendar` property so the gateway
    /// can use the ICS data directly for EWS/EAS rendering.
    pub async fn query_calendar_events(
        &self,
        params: QueryCalendarEventsParams<'_>,
    ) -> Result<CalendarEventListResult> {
        let session = self.get_session(params.username, params.password).await?;
        let api_url = &session.api_url;

        // Build filter per draft-ietf-jmap-calendars §5.11.1
        let mut filter = json!({
            "after": params.start,
            "before": params.end,
        });
        if let Some(cal_id) = params.calendar_id {
            filter["inCalendarIds"] = json!([cal_id]);
        }

        let method_calls = vec![
            (
                "CalendarEvent/query",
                json!({
                    "accountId": params.account_id,
                    "filter": filter,
                    "sort": [{ "property": "start", "isAscending": true }],
                    "position": params.position,
                    "limit": params.limit,
                    "calculateTotal": true,
                }),
                "eq0",
            ),
            (
                "CalendarEvent/get",
                json!({
                    "accountId": params.account_id,
                    "#ids": {
                        "resultOf": "eq0",
                        "name": "CalendarEvent/query",
                        "path": "/ids",
                    },
                    "properties": CALENDAR_EVENT_PROPERTIES,
                }),
                "eg0",
            ),
        ];

        let response = self
            .api_call(
                api_url,
                &["urn:ietf:params:jmap:core", JMAP_CAL_CAPABILITY],
                method_calls,
                params.username,
                params.password,
            )
            .await?;

        let mut events = Vec::new();
        let mut total: u64 = 0;
        let mut query_state = String::new();
        let mut state = String::new();
        let mut unparseable = 0usize;

        for (method, data, _) in response.method_responses {
            if method == "CalendarEvent/query" {
                total = data.get("total").and_then(|v| v.as_u64()).unwrap_or(0);
                query_state = data
                    .get("queryState")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
            }
            if method == "CalendarEvent/get" {
                state = data
                    .get("state")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let (parsed, skipped) = parse_calendar_event_list(&data);
                events = parsed;
                unparseable = skipped;
            }
        }

        Ok(CalendarEventListResult {
            events,
            total,
            query_state,
            state,
            unparseable,
        })
    }

    /// Get a single calendar event by ID via CalendarEvent/get
    /// (draft-ietf-jmap-calendars §5.7).
    ///
    /// Replaces CalDAV `get_event` (GET).
    /// Returns the ICS data, the JMAP event ID, and the ETag.
    /// The ICS data comes from the `iCalendar` property returned by
    /// Stalwart when requested.
    pub async fn get_calendar_event(
        &self,
        account_id: &str,
        event_id: &str,
        username: &str,
        password: &SecretString,
    ) -> Result<(String, String, String)> {
        let session = self.get_session(username, password).await?;
        let api_url = &session.api_url;

        let method_calls = vec![(
            "CalendarEvent/get",
            json!({
                "accountId": account_id,
                "ids": [event_id],
                "properties": CALENDAR_EVENT_PROPERTIES,
            }),
            "eg0",
        )];

        let response = self
            .api_call(
                api_url,
                &["urn:ietf:params:jmap:core", JMAP_CAL_CAPABILITY],
                method_calls,
                username,
                password,
            )
            .await?;

        for (method, data, _) in response.method_responses {
            if method == "CalendarEvent/get" {
                let (mut events, unparseable) = parse_calendar_event_list(&data);
                if unparseable > 0 {
                    return Err(anyhow!(
                        "CalendarEvent/get returned an unparseable event object for ID {}",
                        event_id
                    ));
                }
                if let Some(event) = events.pop() {
                    let ics = event.to_ics().ok_or_else(|| {
                        anyhow!(
                            "failed to convert JMAP calendar event {} to iCalendar",
                            event.id.as_deref().unwrap_or(event_id)
                        )
                    })?;
                    let id = event.id.unwrap_or_default();
                    let etag = match event.raw.as_ref() {
                        Some(raw) => calendar_event_etag(raw, event.etag.as_deref()),
                        None => event.etag.clone().unwrap_or_default(),
                    };
                    return Ok((ics, id, etag));
                }
            }
            if method == "CalendarEvent/get"
                && let Some(not_found) = data.get("notFound")
                && !not_found.is_null()
                && not_found.as_array().is_none_or(|a| !a.is_empty())
            {
                return Err(anyhow!("CalendarEvent not found: {}", event_id));
            }
        }

        Err(anyhow!(
            "CalendarEvent/get returned no data for ID {}",
            event_id
        ))
    }

    /// Batch fetch multiple calendar events by IDs via CalendarEvent/get.
    /// Returns a HashMap mapping event ID to (ics, etag).
    ///
    /// When `calendar_id` is `Some`, only events that belong to that calendar
    /// (its `calendarIds` map carries the id) are returned. The JMAP
    /// `CalendarEvent/changes` feed is account-wide, so a scoped EAS calendar
    /// folder must filter the delta's fetched events by its own calendar id —
    /// otherwise events from every other calendar are delivered into it.
    ///
    /// The explicit `ids` array is chunked at the session's `maxObjectsInGet`
    /// (RFC 8620 §2): Stalwart rejects an over-cap explicit id list whole with
    /// `requestTooLarge`, so a large delta (the sync path unions every
    /// created+updated id) must be fetched in conforming batches.
    pub async fn get_calendar_events(
        &self,
        account_id: &str,
        event_ids: &[String],
        calendar_id: Option<&str>,
        username: &str,
        password: &SecretString,
    ) -> Result<HashMap<String, (String, String)>> {
        if event_ids.is_empty() {
            return Ok(HashMap::new());
        }
        let session = self.get_session(username, password).await?;
        let api_url = &session.api_url;
        let max = session_max_objects_in_get(&session);
        let mut map = HashMap::new();
        for chunk in chunk_ids(event_ids, max) {
            let method_calls = vec![(
                "CalendarEvent/get",
                json!({
                    "accountId": account_id,
                    "ids": chunk,
                    "properties": CALENDAR_EVENT_PROPERTIES,
                }),
                "eg0",
            )];
            let response = self
                .api_call(
                    api_url,
                    &["urn:ietf:params:jmap:core", JMAP_CAL_CAPABILITY],
                    method_calls,
                    username,
                    password,
                )
                .await?;
            // A method-level error response (RFC 8620 §3.6.2) is named
            // "error", not "CalendarEvent/get", so it never enters the branch
            // below. Without this guard a failed chunk would be skipped and the
            // method would return an incomplete map as `Ok`, making the EWS and
            // sync callers omit events instead of activating their CalDAV
            // fallback. Mirror the per-chunk check `get_emails` performs.
            let mut found = false;
            for (method, data, _) in response.method_responses {
                if method == "CalendarEvent/get" {
                    found = true;
                    let (events, unparseable) = parse_calendar_event_list(&data);
                    if unparseable > 0 {
                        return Err(anyhow!(
                            "CalendarEvent/get returned {unparseable} unparseable event object(s)"
                        ));
                    }
                    for event in events {
                        if let Some(id) = event.id.clone()
                            && !id.is_empty()
                        {
                            // A scoped folder receives only its own calendar's
                            // events: skip a fetched event whose `calendarIds`
                            // does not carry the requested calendar. A scope
                            // filter was requested, so an event object that
                            // carries no `calendarIds` map cannot be proven to
                            // belong to this calendar — reject it rather than
                            // admit another calendar's event into the scope.
                            if let Some(cal_id) = calendar_id
                                && !event
                                    .calendar_ids
                                    .as_ref()
                                    .is_some_and(|ids| ids.contains_key(cal_id))
                            {
                                continue;
                            }
                            let ics = event.to_ics().ok_or_else(|| {
                                anyhow!("failed to convert CalendarEvent {id} to iCalendar")
                            })?;
                            let etag = match event.raw.as_ref() {
                                Some(raw) => calendar_event_etag(raw, event.etag.as_deref()),
                                None => event.etag.clone().unwrap_or_default(),
                            };
                            map.insert(id, (ics, etag));
                        }
                    }
                }
            }
            if !found {
                return Err(anyhow!(
                    "Unexpected JMAP response structure for CalendarEvent/get"
                ));
            }
        }
        Ok(map)
    }

    /// Create or update a calendar event via CalendarEvent/set
    /// (draft-ietf-jmap-calendars §5.9).
    ///
    /// Replaces CalDAV `put_event` (PUT).
    ///
    /// The write payload is the **structured JSCalendar** event produced from
    /// the rendered iCalendar by `crate::jscalendar::ics_to_jscalendar_event`,
    /// NOT the raw ICS string in the `iCalendar` property. Stalwart v1.0.x
    /// accepts a raw-`iCalendar` create but silently discards its content
    /// (verified live: the stored event carries no title/start/duration), so
    /// the ICS is converted to the property form Stalwart actually persists.
    /// Updates send the full replacement property set, with every optional
    /// property the new event does not carry explicitly nulled, because
    /// `CalendarEvent/set` update is a partial patch on Stalwart.
    ///
    /// The write is **fail-closed**: the created/updated event is read back in
    /// the same request and its content is compared with what was sent. If the
    /// backend did not apply the payload, an error is returned so the caller
    /// falls back to CalDAV instead of reporting success over a lost event.
    ///
    /// For creates (event_id is None), the event is placed in the
    /// specified calendar_id.
    /// For updates (event_id is Some), the existing event is replaced.
    ///
    /// Returns (jmap_event_id, uid, etag) on success.
    pub async fn set_calendar_event(
        &self,
        params: SetCalendarEventParams<'_>,
    ) -> std::result::Result<(String, String, String), CalendarWriteError> {
        let session = self.get_session(params.username, params.password).await?;
        let api_url = &session.api_url;

        let entry = crate::jscalendar::ics_to_jscalendar_event(params.ics).ok_or_else(|| {
            anyhow!("failed to convert rendered iCalendar into a JSCalendar event payload")
        })?;

        if let Some(existing_id) = params.event_id {
            let payload = crate::jscalendar::jscalendar_write_payload(&entry, true);
            let method_calls = vec![
                (
                    "CalendarEvent/set",
                    json!({
                        "accountId": params.account_id,
                        "update": { (existing_id): payload },
                    }),
                    "es0",
                ),
                (
                    "CalendarEvent/get",
                    json!({
                        "accountId": params.account_id,
                        "ids": [existing_id],
                        "properties": CALENDAR_EVENT_PROPERTIES,
                    }),
                    "eg0",
                ),
            ];

            let response = self
                .api_call(
                    api_url,
                    &["urn:ietf:params:jmap:core", JMAP_CAL_CAPABILITY],
                    method_calls,
                    params.username,
                    params.password,
                )
                .await?;

            let set_args = response
                .method_responses
                .iter()
                .find(|(m, _, _)| m == "CalendarEvent/set")
                .map(|(_, a, _)| a)
                .ok_or_else(|| anyhow!("CalendarEvent/set returned no response"))?;
            if let Some(not_updated) = set_args.get("notUpdated")
                && !not_updated.is_null()
                && not_updated.as_object().is_none_or(|o| !o.is_empty())
            {
                return Err(anyhow!("CalendarEvent/set update failed: {}", not_updated).into());
            }

            let read_back = first_calendar_event(&response).ok_or_else(|| {
                CalendarWriteError::Other(anyhow!(
                    "CalendarEvent/set update could not be verified: read-back returned no event"
                ))
            })?;
            crate::jscalendar::verify_write_round_trip(&read_back, &entry).map_err(|e| {
                if e.is_discarded() {
                    CalendarWriteError::PayloadDiscarded(e.to_string())
                } else {
                    CalendarWriteError::Other(anyhow!("{e}"))
                }
            })?;

            let uid = entry
                .get("uid")
                .and_then(|v| v.as_str())
                .or_else(|| read_back.get("uid").and_then(|v| v.as_str()))
                .unwrap_or("")
                .to_string();
            let etag =
                calendar_event_etag(&read_back, read_back.get("@etag").and_then(|v| v.as_str()));
            Ok((existing_id.to_string(), uid, etag))
        } else {
            let Some(calendar_id) = params.calendar_id else {
                return Err(anyhow!("calendar_id required for create").into());
            };
            let mut payload = crate::jscalendar::jscalendar_write_payload(&entry, false);
            if let Some(obj) = payload.as_object_mut() {
                obj.insert("calendarIds".to_string(), json!({ (calendar_id): true }));
            }
            let method_calls = vec![
                (
                    "CalendarEvent/set",
                    json!({
                        "accountId": params.account_id,
                        "create": { "e0": payload },
                    }),
                    "es0",
                ),
                (
                    "CalendarEvent/get",
                    json!({
                        "accountId": params.account_id,
                        "#ids": {
                            "resultOf": "es0",
                            "name": "CalendarEvent/set",
                            "path": "/created/e0/id",
                        },
                        "properties": CALENDAR_EVENT_PROPERTIES,
                    }),
                    "eg0",
                ),
            ];

            let response = self
                .api_call(
                    api_url,
                    &["urn:ietf:params:jmap:core", JMAP_CAL_CAPABILITY],
                    method_calls,
                    params.username,
                    params.password,
                )
                .await?;

            let set_args = response
                .method_responses
                .iter()
                .find(|(m, _, _)| m == "CalendarEvent/set")
                .map(|(_, a, _)| a)
                .ok_or_else(|| anyhow!("CalendarEvent/set returned no response"))?;
            if let Some(not_created) = set_args.get("notCreated")
                && !not_created.is_null()
                && not_created.as_object().is_none_or(|o| !o.is_empty())
            {
                return Err(anyhow!("CalendarEvent/set create failed: {}", not_created).into());
            }
            let id = set_args
                .get("created")
                .and_then(|c| c.get("e0"))
                .and_then(|e| e.get("id"))
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow!("CalendarEvent/set create returned no created event"))?
                .to_string();

            let read_back = match first_calendar_event(&response) {
                Some(ev) => ev,
                None => {
                    // The create was accepted but the read-back is missing;
                    // remove the (contentless) orphan so a retry or the CalDAV
                    // fallback does not leave a duplicate behind.
                    let _ = self
                        .destroy_calendar_events(
                            params.account_id,
                            std::slice::from_ref(&id),
                            params.username,
                            params.password,
                        )
                        .await;
                    return Err(CalendarWriteError::Other(anyhow!(
                        "CalendarEvent/set create could not be verified: read-back returned no event"
                    )));
                }
            };
            if let Err(e) = crate::jscalendar::verify_write_round_trip(&read_back, &entry) {
                // Whatever the class, the created event must be removed before
                // the caller falls back to CalDAV: the fallback PUTs a fresh
                // `{uuid}.ics` resource, so leaving the JMAP event behind would
                // surface a duplicate to the client. Best-effort cleanup — the
                // write is already failing.
                let _ = self
                    .destroy_calendar_events(
                        params.account_id,
                        std::slice::from_ref(&id),
                        params.username,
                        params.password,
                    )
                    .await;
                if e.is_discarded() {
                    return Err(CalendarWriteError::PayloadDiscarded(e.to_string()));
                }
                // A content-field mismatch is not proof of a lossy write path
                // (a backend may normalise a value the gateway cannot
                // enumerate), so it must not disable JMAP writes — but the
                // event is still removed and the failure surfaced.
                return Err(CalendarWriteError::Other(anyhow!("{e}")));
            }

            let uid = entry
                .get("uid")
                .and_then(|v| v.as_str())
                .or_else(|| read_back.get("uid").and_then(|v| v.as_str()))
                .unwrap_or("")
                .to_string();
            let etag =
                calendar_event_etag(&read_back, read_back.get("@etag").and_then(|v| v.as_str()));
            Ok((id, uid, etag))
        }
    }

    /// Destroy calendar events via CalendarEvent/set
    /// (draft-ietf-jmap-calendars §5.9).
    ///
    /// Replaces CalDAV `delete_event` (DELETE).
    /// JMAP uses state-based concurrency, so there is no
    /// If-Match/ETag requirement — optimistic concurrency is
    /// handled by the state token.
    pub async fn destroy_calendar_events(
        &self,
        account_id: &str,
        event_ids: &[String],
        username: &str,
        password: &SecretString,
    ) -> Result<()> {
        let session = self.get_session(username, password).await?;
        let api_url = &session.api_url;

        let method_calls = vec![(
            "CalendarEvent/set",
            json!({
                "accountId": account_id,
                "destroy": event_ids,
            }),
            "es0",
        )];

        let response = self
            .api_call(
                api_url,
                &["urn:ietf:params:jmap:core", JMAP_CAL_CAPABILITY],
                method_calls,
                username,
                password,
            )
            .await?;

        for (method, data, _) in &response.method_responses {
            // RFC 8620 §3.3: a method-level error is an ["error", {...}, id]
            // response, not a "CalendarEvent/set" result — reject it rather
            // than falling through to Ok, or the caller (e.g. EWS DeleteItem)
            // would skip the CalDAV fallback and report a deletion that
            // never happened.
            if method == "error" {
                let err_type = data
                    .get("type")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown");
                return Err(anyhow!(
                    "CalendarEvent/set returned method error: {}",
                    err_type
                ));
            }
            if method != "CalendarEvent/set" {
                continue;
            }
            if let Some(not_destroyed) = data.get("notDestroyed")
                && !not_destroyed.is_null()
                && not_destroyed.as_object().is_none_or(|o| !o.is_empty())
            {
                return Err(anyhow!(
                    "CalendarEvent/set destroy failed: {}",
                    not_destroyed
                ));
            }
            // Success only when every requested id is confirmed in
            // `destroyed` — an empty/partial `destroyed` array or a missing
            // one means the server silently did nothing, which must fail
            // the same way a method error does.
            let destroyed_ok = data
                .get("destroyed")
                .and_then(|d| d.as_array())
                .map(|list| {
                    event_ids
                        .iter()
                        .all(|id| list.iter().any(|v| v.as_str() == Some(id.as_str())))
                })
                .unwrap_or(false);
            if !destroyed_ok {
                return Err(anyhow!(
                    "CalendarEvent/set destroy: requested ids absent from `destroyed`"
                ));
            }
            return Ok(());
        }

        Err(anyhow!(
            "CalendarEvent/set returned no method response for destroy"
        ))
    }

    /// Check whether the JMAP server supports contacts (RFC 9610), reporting a
    /// backend outage distinctly from a genuine capability miss.
    ///
    /// Returns `Ok(true)` when the session advertises the contacts capability,
    /// `Ok(false)` when the server answered but does not advertise it, and
    /// `Err` when the session could not be fetched. Contact read paths must
    /// treat the error case as a transient backend failure (retryable) rather
    /// than silently switching backends, which would churn the client-visible
    /// contact identity.
    pub async fn contacts_capability(
        &self,
        username: &str,
        password: &SecretString,
    ) -> Result<bool> {
        let session = self.get_session(username, password).await?;
        Ok(session.capabilities.contains_key(JMAP_CONTACTS_CAPABILITY))
    }

    /// Check whether the JMAP server supports contacts (RFC 9610).
    pub async fn supports_contacts(&self, username: &str, password: &SecretString) -> bool {
        self.contacts_capability(username, password)
            .await
            .unwrap_or(false)
    }

    /// Get the primary contacts account id from the JMAP session.
    pub async fn get_contacts_account_id(
        &self,
        username: &str,
        password: &SecretString,
    ) -> Result<String> {
        let session = self.get_session(username, password).await?;
        session
            .primary_accounts
            .get(JMAP_CONTACTS_CAPABILITY)
            .cloned()
            .ok_or_else(|| anyhow!("No primary contacts account found in JMAP session"))
    }

    /// Get the default address book (AddressBook/get, first entry).
    pub async fn get_default_address_book_id(
        &self,
        username: &str,
        password: &SecretString,
    ) -> Result<String> {
        let session = self.get_session(username, password).await?;
        let api_url = &session.api_url;
        let account_id = session
            .primary_accounts
            .get(JMAP_CONTACTS_CAPABILITY)
            .ok_or_else(|| anyhow!("No primary contacts account found in JMAP session"))?;

        let method_calls = vec![(
            "AddressBook/get",
            json!({
                "accountId": account_id,
                "properties": ["id", "name", "isDefault", "myRights"],
            }),
            "ab0",
        )];

        let response = self
            .api_call(
                api_url,
                &["urn:ietf:params:jmap:core", JMAP_CONTACTS_CAPABILITY],
                method_calls,
                username,
                password,
            )
            .await?;

        for (method, data, _) in response.method_responses {
            if method == "AddressBook/get" {
                let list: Vec<Value> = data
                    .get("list")
                    .and_then(|v| v.as_array().cloned())
                    .unwrap_or_default();
                let default_book = list
                    .iter()
                    .find(|b| b.get("isDefault").and_then(|v| v.as_bool()) == Some(true))
                    .or_else(|| list.first())
                    .and_then(|b| b.get("id").and_then(|v| v.as_str()));
                if let Some(id) = default_book {
                    return Ok(id.to_string());
                }
                return Err(anyhow!("No address books found for contacts account"));
            }
        }

        Err(anyhow!("AddressBook/get returned no list"))
    }

    /// List all contact cards for the user via ContactCard/get (RFC 9610 §4).
    ///
    /// Returns the cards plus the server state token for change detection.
    pub async fn list_contact_cards(
        &self,
        username: &str,
        password: &SecretString,
    ) -> Result<(Vec<JmapContactCard>, String)> {
        let session = self.get_session(username, password).await?;
        let api_url = &session.api_url;
        let account_id = session
            .primary_accounts
            .get(JMAP_CONTACTS_CAPABILITY)
            .ok_or_else(|| anyhow!("No primary contacts account found in JMAP session"))?;

        let method_calls = vec![("ContactCard/get", json!({ "accountId": account_id }), "cg0")];

        let response = self
            .api_call(
                api_url,
                &["urn:ietf:params:jmap:core", JMAP_CONTACTS_CAPABILITY],
                method_calls,
                username,
                password,
            )
            .await?;

        for (method, data, _) in response.method_responses {
            if method == "ContactCard/get" {
                let state = data
                    .get("state")
                    .and_then(|v| v.as_str().map(|s| s.to_string()))
                    .unwrap_or_default();
                let list: Vec<Value> = data
                    .get("list")
                    .and_then(|v| v.as_array().cloned())
                    .unwrap_or_default();
                let mut cards = Vec::with_capacity(list.len());
                for item in list {
                    let Some(id) = item.get("id").and_then(|v| v.as_str()) else {
                        continue;
                    };
                    let etag = item
                        .get("@etag")
                        .and_then(|v| v.as_str())
                        .map(|s| s.trim_matches('"').to_string());
                    cards.push(JmapContactCard {
                        id: id.to_string(),
                        etag,
                        card: item,
                    });
                }
                return Ok((cards, state));
            }
        }

        Err(anyhow!(
            "Unexpected JMAP response structure for ContactCard/get"
        ))
    }

    /// Get a single contact card by id via `ContactCard/get` (RFC 9610 §4).
    ///
    /// Returns the raw JSCard document when the id exists, `None` when the
    /// server reports it as not found.
    pub async fn get_contact_card(
        &self,
        username: &str,
        password: &SecretString,
        id: &str,
    ) -> Result<Option<Value>> {
        let session = self.get_session(username, password).await?;
        let api_url = &session.api_url;
        let account_id = session
            .primary_accounts
            .get(JMAP_CONTACTS_CAPABILITY)
            .ok_or_else(|| anyhow!("No primary contacts account found in JMAP session"))?;

        let method_calls = vec![(
            "ContactCard/get",
            json!({ "accountId": account_id, "ids": [id] }),
            "cg0",
        )];

        let response = self
            .api_call(
                api_url,
                &["urn:ietf:params:jmap:core", JMAP_CONTACTS_CAPABILITY],
                method_calls,
                username,
                password,
            )
            .await?;

        for (method, data, _) in response.method_responses {
            if method == "ContactCard/get" {
                if let Some(not_found) = data.get("notFound").and_then(|v| v.as_array())
                    && not_found.iter().any(|v| v.as_str() == Some(id))
                {
                    return Ok(None);
                }
                let list = data
                    .get("list")
                    .and_then(|v| v.as_array())
                    .cloned()
                    .unwrap_or_default();
                return Ok(list.into_iter().next());
            }
        }

        Err(anyhow!(
            "Unexpected JMAP response structure for ContactCard/get"
        ))
    }

    /// Create a new contact card via ContactCard/set.
    ///
    /// `card` is the JSCard JSON (with `addressBookIds` set by the caller).
    /// Returns the new contact id and the ETag if returned.
    pub async fn create_contact_card(
        &self,
        username: &str,
        password: &SecretString,
        card: &Value,
    ) -> Result<(String, Option<String>)> {
        let session = self.get_session(username, password).await?;
        let api_url = &session.api_url;
        let account_id = session
            .primary_accounts
            .get(JMAP_CONTACTS_CAPABILITY)
            .ok_or_else(|| anyhow!("No primary contacts account found in JMAP session"))?;

        let method_calls = vec![(
            "ContactCard/set",
            json!({
                "accountId": account_id,
                "create": { "c0": card },
            }),
            "cs0",
        )];

        let response = self
            .api_call(
                api_url,
                &["urn:ietf:params:jmap:core", JMAP_CONTACTS_CAPABILITY],
                method_calls,
                username,
                password,
            )
            .await?;

        for (method, data, _) in response.method_responses {
            if method == "ContactCard/set" {
                if let Some(not_created) = data.get("notCreated")
                    && let Some(err) = not_created.get("c0")
                {
                    return Err(anyhow!("ContactCard/set create failed: {}", err));
                }
                if let Some(created) = data.get("created")
                    && let Some(c0) = created.get("c0")
                {
                    let id = c0
                        .get("id")
                        .and_then(|v| v.as_str())
                        .ok_or_else(|| anyhow!("ContactCard/set returned no id"))?
                        .to_string();
                    let etag = c0
                        .get("@etag")
                        .and_then(|v| v.as_str())
                        .map(|s| s.trim_matches('"').to_string());
                    return Ok((id, etag));
                }
            }
        }

        Err(anyhow!(
            "Unexpected JMAP response structure for ContactCard/set (create)"
        ))
    }

    /// Update an existing contact card via ContactCard/set (full-field patch
    /// with null-outs for removed properties handled by the caller).
    pub async fn update_contact_card(
        &self,
        username: &str,
        password: &SecretString,
        id: &str,
        patch: &Value,
    ) -> Result<Option<String>> {
        let session = self.get_session(username, password).await?;
        let api_url = &session.api_url;
        let account_id = session
            .primary_accounts
            .get(JMAP_CONTACTS_CAPABILITY)
            .ok_or_else(|| anyhow!("No primary contacts account found in JMAP session"))?;

        let method_calls = vec![(
            "ContactCard/set",
            json!({
                "accountId": account_id,
                "update": { id: patch },
            }),
            "cs0",
        )];

        let response = self
            .api_call(
                api_url,
                &["urn:ietf:params:jmap:core", JMAP_CONTACTS_CAPABILITY],
                method_calls,
                username,
                password,
            )
            .await?;

        for (method, data, _) in response.method_responses {
            if method == "ContactCard/set" {
                if let Some(not_updated) = data.get("notUpdated")
                    && let Some(err) = not_updated.get(id)
                {
                    return Err(anyhow!("ContactCard/set update failed for {}: {}", id, err));
                }
                if let Some(updated) = data.get("updated")
                    && updated.get(id).is_some()
                {
                    // The updated map entry may be null when no properties
                    // changed; success is established by the key being present.
                    let etag = updated
                        .get(id)
                        .and_then(|u| u.get("@etag"))
                        .and_then(|v| v.as_str())
                        .map(|s| s.trim_matches('"').to_string());
                    return Ok(etag);
                }
                // An id in neither `updated` nor `notUpdated` means the server
                // never processed our update: report failure so the caller can
                // fall back instead of persisting an unconfirmed state.
                return Err(anyhow!(
                    "ContactCard/set: id {} absent from both updated and notUpdated",
                    id
                ));
            }
        }

        Err(anyhow!(
            "Unexpected JMAP response structure for ContactCard/set (update)"
        ))
    }

    /// Destroy a contact card via ContactCard/set.
    pub async fn destroy_contact_card(
        &self,
        username: &str,
        password: &SecretString,
        id: &str,
    ) -> Result<()> {
        let session = self.get_session(username, password).await?;
        let api_url = &session.api_url;
        let account_id = session
            .primary_accounts
            .get(JMAP_CONTACTS_CAPABILITY)
            .ok_or_else(|| anyhow!("No primary contacts account found in JMAP session"))?;

        let method_calls = vec![(
            "ContactCard/set",
            json!({
                "accountId": account_id,
                "destroy": [id],
            }),
            "cs0",
        )];

        let response = self
            .api_call(
                api_url,
                &["urn:ietf:params:jmap:core", JMAP_CONTACTS_CAPABILITY],
                method_calls,
                username,
                password,
            )
            .await?;

        for (method, data, _) in response.method_responses {
            if method == "ContactCard/set" {
                if let Some(not_destroyed) = data.get("notDestroyed")
                    && let Some(err) = not_destroyed.get(id)
                {
                    return Err(anyhow!(
                        "ContactCard/set destroy failed for {}: {}",
                        id,
                        err
                    ));
                }
                // Success only if the id is confirmed in `destroyed`.
                if data
                    .get("destroyed")
                    .and_then(|d| d.as_array())
                    .map(|list| list.iter().any(|v| v.as_str() == Some(id)))
                    .unwrap_or(false)
                {
                    return Ok(());
                }
                return Err(anyhow!(
                    "ContactCard/set: id {} absent from both destroyed and notDestroyed",
                    id
                ));
            }
        }

        Err(anyhow!(
            "Unexpected JMAP response structure for ContactCard/set (destroy)"
        ))
    }

    /// Resolve the Principal `id` for `account_id` (the caller's own
    /// mailbox account) via `Principal/query` filtered on `accountIds`, so
    /// `Principal/getAvailability` can be called with the draft-ietf-jmap-
    /// calendars §2.2 argument shape (`id`, not the account id). Falls back
    /// to `account_id` itself when the query cannot be used — for a caller
    /// authenticated as the principal owner the account id and principal id
    /// denote the same account on Stalwart, and `get_availability`'s callers
    /// already treat any unusable payload as "fall back", so the fallback can
    /// only lose the fast path, never correctness.
    pub async fn get_principal_id(
        &self,
        account_id: &str,
        username: &str,
        password: &SecretString,
    ) -> String {
        let Ok(session) = self.get_session(username, password).await else {
            return account_id.to_string();
        };
        let api_url = &session.api_url;

        let method_calls = vec![(
            "Principal/query",
            json!({
                "accountId": account_id,
                "filter": {
                    "accountIds": [account_id],
                },
            }),
            "pq0",
        )];

        let Ok(response) = self
            .api_call(
                api_url,
                &[
                    "urn:ietf:params:jmap:core",
                    "urn:ietf:params:jmap:principals",
                    JMAP_CAL_AVAILABILITY_CAPABILITY,
                ],
                method_calls,
                username,
                password,
            )
            .await
        else {
            return account_id.to_string();
        };

        for (method, data, _) in response.method_responses {
            if method == "Principal/query"
                && let Some(id) = data
                    .get("ids")
                    .and_then(|v| v.as_array())
                    .and_then(|ids| ids.first())
                    .and_then(|v| v.as_str())
                    .filter(|s| !s.is_empty())
            {
                return id.to_string();
            }
        }
        account_id.to_string()
    }

    /// Get free-busy availability via Principal/getAvailability
    /// (draft-ietf-jmap-calendars §2.2).
    ///
    /// Replaces CalDAV `get_freebusy` (REPORT free-busy-query).
    /// Returns the raw JSON response (`{"list": [...]}`) for the caller to
    /// parse. `account_id` is the calendars account; the Principal `id` is
    /// resolved from it via [`Self::get_principal_id`], because draft §2.2
    /// takes `id` (the Principal), `utcStart`, `utcEnd` — not `principalIds`
    /// / `duration` — and returns `list` entries with `utcStart`, `utcEnd`,
    /// and `busyStatus`.
    pub async fn get_availability(
        &self,
        account_id: &str,
        start: &str,
        end: &str,
        username: &str,
        password: &SecretString,
    ) -> Result<Value> {
        let session = self.get_session(username, password).await?;
        let api_url = &session.api_url;
        let principal_id = self.get_principal_id(account_id, username, password).await;

        let method_calls = vec![(
            "Principal/getAvailability",
            json!({
                "accountId": account_id,
                "id": principal_id,
                "utcStart": start,
                "utcEnd": end,
                "showDetails": true,
            }),
            "pa0",
        )];

        let response = self
            .api_call(
                api_url,
                &[
                    "urn:ietf:params:jmap:core",
                    JMAP_CAL_AVAILABILITY_CAPABILITY,
                ],
                method_calls,
                username,
                password,
            )
            .await?;

        for (method, data, _) in response.method_responses {
            if method == "Principal/getAvailability" {
                return Ok(data);
            }
        }

        Err(anyhow!("Principal/getAvailability returned no data"))
    }

    /// Render a draft-ietf-jmap-calendars §2.2 `Principal/getAvailability`
    /// result (`{"list": [...]}` of `{utcStart, utcEnd, busyStatus, event?}`
    /// BusyPeriods) into the gateway's merged free/busy string (`0`/`1`/`2`/`3`
    /// per `safe_interval`-minute slot, worst-wins) plus the detailed-event XML
    /// rows (EWS `<t:CalendarEvent>` shape, with a `<t:CalendarEventDetails>`
    /// child when the period carries a JSCalendar `event` object — the
    /// `showDetails: true` request). The legacy `availability[].busy[]`
    /// (non-standard) payload is also accepted so an older backend shape still
    /// renders instead of silently falling back.
    ///
    /// Returns `None` when no usable busy period exists, so the caller falls
    /// back to the calendar-event query / CalDAV paths rather than publishing
    /// a fabricated all-free view; also `None` for a non-positive
    /// `safe_interval`.
    ///
    /// Merged-slot mapping (draft `busyStatus` → EWS digits): `confirmed`
    /// and the default `unavailable` → `2` (Busy), `tentative` → `1`, `free`
    /// (legacy `type` only) → `0`, and legacy `type` `oof` → `3`; any other
    /// legacy `type` value → `2`. Per the draft, `unavailable` is the default
    /// when `busyStatus` is omitted.
    pub fn availability_to_merged_freebusy(
        data: &Value,
        start: chrono::DateTime<chrono::Utc>,
        end: chrono::DateTime<chrono::Utc>,
        safe_interval: i64,
    ) -> Option<(String, String)> {
        if safe_interval <= 0 {
            return None;
        }
        let slot_count = (((end - start).num_seconds().max(0) + (safe_interval * 60 - 1))
            / (safe_interval * 60)) as usize;
        if slot_count == 0 {
            return None;
        }
        // Collect (block, busy status) tuples from both wire shapes.
        let mut blocks: Vec<(&Value, &str, bool)> = Vec::new();
        for period in data
            .get("list")
            .and_then(|v| v.as_array())
            .map(Vec::as_slice)
            .unwrap_or_default()
        {
            blocks.push((
                period,
                period
                    .get("busyStatus")
                    .and_then(|v| v.as_str())
                    .unwrap_or(""),
                true,
            ));
        }
        for principal in data
            .get("availability")
            .and_then(|v| v.as_array())
            .map(Vec::as_slice)
            .unwrap_or_default()
        {
            if let Some(busy) = principal.get("busy").and_then(|v| v.as_array()) {
                for block in busy {
                    blocks.push((
                        block,
                        block.get("type").and_then(|v| v.as_str()).unwrap_or(""),
                        false,
                    ));
                }
            }
        }
        let mut merged = vec!['0'; slot_count];
        let mut events_xml = String::new();
        let mut saw_usable_block = false;
        for (block, status, from_draft) in blocks {
            let Some(block_start) = block
                .get("utcStart")
                .or_else(|| block.get("start"))
                .and_then(|v| v.as_str())
                .and_then(crate::calendar::parse_datetime)
            else {
                continue;
            };
            let Some(block_end) = block
                .get("utcEnd")
                .or_else(|| block.get("end"))
                .and_then(|v| v.as_str())
                .and_then(crate::calendar::parse_datetime)
            else {
                continue;
            };
            if block_end <= block_start {
                continue;
            }
            // `free` exists only in the legacy `type` field; the draft
            // `busyStatus` set is confirmed/tentative/unavailable, with
            // `unavailable` the default when omitted (→ Busy).
            let sd = match status {
                "tentative" => '1',
                "oof" => '3',
                "free" if !from_draft => '0',
                _ => '2',
            };
            saw_usable_block = true;
            for (i, slot) in merged.iter_mut().enumerate() {
                let ss = start + chrono::Duration::minutes((i as i64) * safe_interval);
                let se = ss + chrono::Duration::minutes(safe_interval);
                if block_start < se && block_end > ss && sd > *slot {
                    *slot = sd;
                }
            }
            let busy_type = match sd {
                '0' => "Free",
                '1' => "Tentative",
                '3' => "OOF",
                _ => "Busy",
            };
            let details_xml = block
                .get("event")
                .and_then(crate::jscalendar::jscalendar_event_to_ics)
                .and_then(|ics| crate::calendar::parse_ics_event(&ics))
                .map(|item| {
                    format!(
                        "<t:CalendarEventDetails><t:Subject>{}</t:Subject><t:Location>{}</t:Location><t:IsMeeting>{}</t:IsMeeting><t:IsRecurring>{}</t:IsRecurring><t:IsException>false</t:IsException><t:IsReminderSet>{}</t:IsReminderSet><t:IsPrivate>{}</t:IsPrivate></t:CalendarEventDetails>",
                        crate::util::xml_escape(&item.subject),
                        crate::util::xml_escape(&item.location),
                        if item.attendees.is_empty() { "false" } else { "true" },
                        if item.rrule.is_some() { "true" } else { "false" },
                        if item.reminder.is_some() { "true" } else { "false" },
                        if item.sensitivity.map(|v| v >= 2).unwrap_or(false) { "true" } else { "false" },
                    )
                })
                .unwrap_or_default();
            events_xml.push_str(&format!(
                "<t:CalendarEvent><t:StartTime>{}</t:StartTime><t:EndTime>{}</t:EndTime><t:BusyType>{}</t:BusyType>{}</t:CalendarEvent>",
                crate::util::format_ews_datetime(&block_start),
                crate::util::format_ews_datetime(&block_end),
                busy_type,
                details_xml
            ));
        }
        if !saw_usable_block {
            return None;
        }
        Some((merged.into_iter().collect(), events_xml))
    }
}

/// Pure parser: extract `updated` / `notUpdated` / method-level `error` from
/// the Email/set method responses into an [`EmailSetOutcome`]. Pure (no I/O)
/// so unit tests exercise the rejection-handling without a live server or
/// mock — invoked by [`JmapClient::update_email_checked`]. (qodo #3/#5,
/// cubic #23.)
pub fn parse_email_set_outcome(
    method_responses: Vec<(String, serde_json::Value, String)>,
) -> EmailSetOutcome {
    let mut out = EmailSetOutcome::default();
    for (method, data, _) in method_responses {
        if method != "Email/set" {
            continue;
        }
        if let Some(updated) = data.get("updated").and_then(|v| v.as_object()) {
            // RFC 8621 §5.3: `updated` is a map of id -> (optional) null.
            out.updated = updated.keys().cloned().collect();
        } else if let Some(arr) = data.get("updated").and_then(|v| v.as_array()) {
            // Defensive: a few servers echo the requested ids as an array.
            out.updated = arr
                .iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect();
        }
        if let Some(nu) = data.get("notUpdated")
            && !nu.is_null()
            && let Some(obj) = nu.as_object()
        {
            for (id, err) in obj {
                let desc = err
                    .get("description")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown")
                    .to_string();
                out.not_updated.push((id.clone(), desc));
            }
        }
        if let Some(err) = data.get("error")
            && !err.is_null()
        {
            let desc = err
                .get("description")
                .and_then(|v| v.as_str())
                .or_else(|| err.as_str())
                .unwrap_or("Email/set method rejected")
                .to_string();
            out.method_error = Some(desc);
        }
    }
    out
}

/// Build the JMAP `Email/set` `update` patch for a mailbox *move*, keyed by
/// email id (NOT by response order). For each requested id present in
/// `current` it nulls every existing `mailboxIds/<old>` and sets
/// `mailboxIds/<target>=true`; ids missing from `current` are silently
/// skipped (the caller reports partial completion).
///
/// This is a pure helper extracted from `move_emails` so the
/// partial/reorder-safe semantics — fixing the `zip`-mis-association bug
/// flagged in PR #1825 — are unit-testable without a live JMAP server.
/// RFC 8621 §4.5 mailboxIds patch:
///   `{ id: { "mailboxIds/<oldid>": null, "mailboxIds/<newid>": true } }`
fn build_move_update_patch(
    email_ids: &[String],
    current: &std::collections::HashMap<String, Vec<String>>,
    target_mailbox_id: &str,
) -> serde_json::Map<String, Value> {
    let mut update = serde_json::Map::new();
    for eid in email_ids {
        let Some(mids) = current.get(eid) else {
            continue;
        };
        let mut patch = serde_json::Map::new();
        for old in mids {
            patch.insert(format!("mailboxIds/{old}"), json!(null));
        }
        patch.insert(format!("mailboxIds/{target_mailbox_id}"), json!(true));
        update.insert(eid.clone(), Value::Object(patch));
    }
    update
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine;

    #[test]
    fn test_derive_from_caldav_basic() {
        let result = JmapClient::derive_from_caldav("http://stalwart:8080/dav");
        assert_eq!(result, "http://stalwart:8080/jmap");
    }

    #[test]
    fn test_derive_from_caldav_with_trailing_slash() {
        let result = JmapClient::derive_from_caldav("http://stalwart:8080/dav/");
        assert_eq!(result, "http://stalwart:8080/jmap");
    }

    #[test]
    fn test_derive_from_caldav_no_dav_suffix() {
        let result = JmapClient::derive_from_caldav("http://stalwart:8080/api");
        assert_eq!(result, "http://stalwart:8080/api/jmap");
    }

    #[test]
    fn test_jmap_client_new_strips_trailing_slash() {
        let client = JmapClient::new("http://stalwart:8080/jmap/").unwrap();
        assert_eq!(client.base_url, "http://stalwart:8080/jmap");
    }

    #[test]
    fn test_basic_auth_header_format() {
        let auth =
            JmapClient::basic_auth_header("user@example.com", &SecretString::from("pass123"));
        assert!(auth.starts_with("Basic "));
        // Base64 of "user@example.com:pass123"
        let expected_b64 =
            base64::engine::general_purpose::STANDARD.encode("user@example.com:pass123");
        assert_eq!(auth, format!("Basic {}", expected_b64));
    }

    #[test]
    fn test_jmap_cal_capability_urn() {
        assert_eq!(JMAP_CAL_CAPABILITY, "urn:ietf:params:jmap:calendars");
    }

    #[test]
    fn test_jmap_cal_availability_capability_urn() {
        assert_eq!(
            JMAP_CAL_AVAILABILITY_CAPABILITY,
            "urn:ietf:params:jmap:principals:availability"
        );
    }

    #[test]
    fn availability_to_merged_freebusy_renders_busy_slots() {
        // AUDIT3 #14: a Principal/getAvailability payload must render the
        // same merged string the calendar-event path renders — worst-wins
        // per slot, with the busy block's type mapped onto the slot digit.
        use chrono::TimeZone as _;
        let start = chrono::Utc.with_ymd_and_hms(2026, 1, 5, 9, 0, 0).unwrap();
        let end = chrono::Utc.with_ymd_and_hms(2026, 1, 5, 11, 0, 0).unwrap();
        let data = json!({
            "availability": [{
                "id": "p1",
                "busy": [
                    {"start": "2026-01-05T09:30:00", "end": "2026-01-05T10:30:00", "type": "busy"},
                    {"start": "2026-01-05T10:00:00", "end": "2026-01-05T10:15:00", "type": "tentative"}
                ]
            }]
        });
        let (merged, events_xml) =
            JmapClient::availability_to_merged_freebusy(&data, start, end, 30).unwrap();
        // 4 slots (09:00, 09:30, 10:00, 10:30): 09:00 free, 09:30 busy,
        // 10:00 busy (busy beats tentative), 10:30 free (busy ends 10:30).
        assert_eq!(merged, "0220");
        assert!(events_xml.contains("<t:BusyType>Busy</t:BusyType>"));
        assert!(events_xml.contains("<t:BusyType>Tentative</t:BusyType>"));
        assert!(events_xml.contains("2026-01-05T09:30:00Z"));
    }

    #[test]
    fn availability_to_merged_freebusy_empty_payload_is_none() {
        // No availability array (or an empty one) must yield None so the
        // caller falls back instead of publishing an all-free view.
        use chrono::TimeZone as _;
        let start = chrono::Utc.with_ymd_and_hms(2026, 1, 5, 9, 0, 0).unwrap();
        let end = chrono::Utc.with_ymd_and_hms(2026, 1, 5, 10, 0, 0).unwrap();
        assert!(JmapClient::availability_to_merged_freebusy(&json!({}), start, end, 30).is_none());
        assert!(
            JmapClient::availability_to_merged_freebusy(
                &json!({"availability": []}),
                start,
                end,
                30
            )
            .is_none()
        );
        assert!(
            JmapClient::availability_to_merged_freebusy(
                &json!({"availability": [{"id": "p1"}]}),
                start,
                end,
                30
            )
            .is_none()
        );
    }

    #[test]
    fn availability_to_merged_freebusy_standard_list_payload() {
        // draft-ietf-jmap-calendars §2.2: `{"list": [...]}` of
        // `{utcStart, utcEnd, busyStatus, event?}` BusyPeriods is what a
        // conforming server returns, and showDetails periods must render
        // <t:CalendarEventDetails> for DetailedMerged views.
        use chrono::TimeZone as _;
        let start = chrono::Utc.with_ymd_and_hms(2026, 1, 5, 9, 0, 0).unwrap();
        let end = chrono::Utc.with_ymd_and_hms(2026, 1, 5, 11, 0, 0).unwrap();
        let data = json!({
            "list": [
                {
                    "utcStart": "2026-01-05T09:30:00Z",
                    "utcEnd": "2026-01-05T10:30:00Z",
                    "busyStatus": "confirmed",
                    "event": {
                        "@type": "Event",
                        "uid": "avail-1",
                        "title": "Focus Time",
                        "start": "2026-01-05T09:30:00",
                        "duration": "PT1H",
                        "locations": { "l1": { "@type": "Location", "name": "Room 9" } },
                        "iCalendar": {
                            "convertedProperties": { "duration": { "name": "dtend" } },
                            "name": "vevent"
                        }
                    }
                },
                {
                    "utcStart": "2026-01-05T10:00:00Z",
                    "utcEnd": "2026-01-05T10:15:00Z",
                    "busyStatus": "tentative"
                }
            ]
        });
        let (merged, events_xml) =
            JmapClient::availability_to_merged_freebusy(&data, start, end, 30).unwrap();
        assert_eq!(merged, "0220");
        assert!(events_xml.contains("<t:BusyType>Busy</t:BusyType>"));
        assert!(events_xml.contains("<t:BusyType>Tentative</t:BusyType>"));
        assert!(events_xml.contains("2026-01-05T09:30:00Z"));
        // The confirmed period carries an event: DetailedMerged keeps the
        // subject/location rendered through the JSCalendar→ICS chain.
        assert!(events_xml.contains("<t:Subject>Focus Time</t:Subject>"));
        assert!(events_xml.contains("<t:Location>Room 9</t:Location>"));
        // The tentative period carries none — no details element for it.
        assert_eq!(
            events_xml.matches("<t:CalendarEventDetails>").count(),
            1,
            "only the event-carrying period renders details: {events_xml}"
        );
    }

    #[test]
    fn availability_to_merged_freebusy_unparseable_blocks_is_none() {
        // A payload whose only periods are unparseable (or end <= start)
        // must yield None so the caller falls back, never an all-free string.
        use chrono::TimeZone as _;
        let start = chrono::Utc.with_ymd_and_hms(2026, 1, 5, 9, 0, 0).unwrap();
        let end = chrono::Utc.with_ymd_and_hms(2026, 1, 5, 10, 0, 0).unwrap();
        assert!(
            JmapClient::availability_to_merged_freebusy(
                &json!({"list": [{"utcStart": "bogus", "utcEnd": "2026-01-05T10:00:00Z", "busyStatus": "confirmed"}]}),
                start,
                end,
                30
            )
            .is_none()
        );
        assert!(
            JmapClient::availability_to_merged_freebusy(
                &json!({"list": [{"utcStart": "2026-01-05T10:00:00Z", "utcEnd": "2026-01-05T09:00:00Z", "busyStatus": "confirmed"}]}),
                start,
                end,
                30
            )
            .is_none()
        );
        assert!(
            JmapClient::availability_to_merged_freebusy(
                &json!({"availability": [{"busy": [{"start": "bogus", "end": "bogus"}]}]}),
                start,
                end,
                30
            )
            .is_none()
        );
    }

    #[test]
    fn availability_to_merged_freebusy_rejects_nonpositive_interval() {
        use chrono::TimeZone as _;
        let start = chrono::Utc.with_ymd_and_hms(2026, 1, 5, 9, 0, 0).unwrap();
        let end = chrono::Utc.with_ymd_and_hms(2026, 1, 5, 10, 0, 0).unwrap();
        let data = json!({
            "list": [{"utcStart": "2026-01-05T09:00:00Z", "utcEnd": "2026-01-05T10:00:00Z", "busyStatus": "confirmed"}]
        });
        assert!(JmapClient::availability_to_merged_freebusy(&data, start, end, 0).is_none());
        assert!(JmapClient::availability_to_merged_freebusy(&data, start, end, -5).is_none());
    }

    #[test]
    fn test_jmap_calendar_deserialization() {
        let cal_json = json!({
            "id": "cal-abc123",
            "name": "Personal",
            "color": "#FF0000",
            "description": "My calendar",
            "sortOrder": 1,
            "isSubscribed": true,
            "isVisible": true,
            "timeZone": "America/New_York"
        });
        let cal: JmapCalendar = serde_json::from_value(cal_json).unwrap();
        assert_eq!(cal.id.as_deref(), Some("cal-abc123"));
        assert_eq!(cal.name.as_deref(), Some("Personal"));
        assert_eq!(cal.color.as_deref(), Some("#FF0000"));
        assert!(cal.is_subscribed.unwrap());
        assert_eq!(cal.sort_order.unwrap(), 1);
    }

    #[test]
    fn test_jmap_calendar_event_deserialization() {
        let event_json = json!({
            "id": "evt-xyz789",
            "uid": "19970901T130000Z-123401@example.com",
            "title": "Team Meeting",
            "iCalendar": "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nBEGIN:VEVENT\r\nUID:19970901T130000Z-123401@example.com\r\nSUMMARY:Team Meeting\r\nEND:VEVENT\r\nEND:VCALENDAR",
            "isAllDay": false,
            "calendarIds": {"cal-abc123": true}
        });
        let event: JmapCalendarEvent = serde_json::from_value(event_json).unwrap();
        assert_eq!(event.id.as_deref(), Some("evt-xyz789"));
        assert_eq!(
            event.uid.as_deref(),
            Some("19970901T130000Z-123401@example.com")
        );
        assert_eq!(event.title.as_deref(), Some("Team Meeting"));
        assert!(event.i_calendar.is_some());
        assert!(!event.is_all_day.unwrap());
    }

    #[test]
    fn test_jmap_calendar_event_minimal() {
        // Minimal event with just id — all other fields should default to None
        let event_json = json!({"id": "evt-minimal"});
        let event: JmapCalendarEvent = serde_json::from_value(event_json).unwrap();
        assert_eq!(event.id.as_deref(), Some("evt-minimal"));
        assert!(event.uid.is_none());
        assert!(event.title.is_none());
        assert!(event.i_calendar.is_none());
        assert!(event.calendar_ids.is_none());
    }

    #[test]
    fn test_parse_calendar_event_list_skips_unparseable_element() {
        // Stalwart returns `iCalendar` as a JSCalendar object. The old
        // whole-list `from_value` failed on the first such element and
        // yielded ZERO events; per-element parsing must keep the rest.
        let data = json!({
            "list": [
                {
                    "id": "e1",
                    "uid": "u1",
                    "title": "Good",
                    "start": "2026-01-05T09:00:00",
                    "timeZone": "Etc/UTC",
                    "duration": "PT1H",
                    "iCalendar": {"convertedProperties": {}, "name": "vevent"}
                },
                {
                    // Not an object: cannot deserialize into an event.
                    "id": 12345
                },
                {
                    "id": "e3",
                    "uid": "u3",
                    "title": "Also Good",
                    "start": "2026-01-06T09:00:00",
                    "timeZone": "Etc/UTC",
                    "duration": "PT1H",
                    "iCalendar": {"name": "vevent"}
                }
            ]
        });
        let (events, unparseable) = parse_calendar_event_list(&data);
        assert_eq!(events.len(), 2, "one bad element must not drop the list");
        assert_eq!(unparseable, 1, "the skipped element must be counted");
        assert_eq!(events[0].id.as_deref(), Some("e1"));
        assert_eq!(events[1].id.as_deref(), Some("e3"));
        // Each parsed event retains its raw object for iCalendar rebuild.
        assert!(events[0].raw.is_some());
        assert!(events[1].raw.is_some());
    }

    #[test]
    fn test_calendar_event_to_ics_reconstructs_from_stalwart_object() {
        let event_json = json!({
            "id": "e1",
            "uid": "u-1",
            "title": "Team Sync",
            "start": "2026-01-05T09:00:00",
            "timeZone": "Europe/Berlin",
            "duration": "PT1H",
            "iCalendar": {
                "convertedProperties": { "duration": { "name": "dtend" } },
                "name": "vevent"
            }
        });
        let mut event: JmapCalendarEvent = serde_json::from_value(event_json.clone()).unwrap();
        // `raw` is populated by the client; mirror that here.
        event.raw = Some(event_json);
        let ics = event.to_ics().expect("reconstructed ICS");
        assert!(ics.contains("BEGIN:VEVENT"), "{ics}");
        assert!(ics.contains("UID:u-1"), "{ics}");
        assert!(ics.contains("SUMMARY:Team Sync"), "{ics}");
        assert!(
            ics.contains("DTSTART;TZID=Europe/Berlin:20260105T090000"),
            "{ics}"
        );
        assert!(
            ics.contains("DTEND;TZID=Europe/Berlin:20260105T100000"),
            "{ics}"
        );
        // The JSCalendar object must never leak through as an ICS string.
        assert!(!ics.contains("convertedProperties"), "{ics}");
    }

    #[test]
    fn test_calendar_event_to_ics_none_without_raw() {
        let event = JmapCalendarEvent {
            id: Some("e1".to_string()),
            i_calendar: Some(json!({"name": "vevent"})),
            ..Default::default()
        };
        assert!(event.to_ics().is_none());
    }

    #[test]
    fn test_jmap_session_calendar_capability_detection() {
        let session_json = json!({
            "username": "test@example.com",
            "apiUrl": "https://stalwart.example.com/jmap/api/",
            "downloadUrl": "https://stalwart.example.com/jmap/download/{blobId}",
            "uploadUrl": "https://stalwart.example.com/jmap/upload/{accountId}",
            "eventSourceUrl": "https://stalwart.example.com/jmap/eventsource",
            "state": "state-1",
            "primaryAccounts": {
                "urn:ietf:params:jmap:mail": "acct-mail-123",
                "urn:ietf:params:jmap:calendars": "acct-cal-456"
            },
            "capabilities": {
                "urn:ietf:params:jmap:core": {},
                "urn:ietf:params:jmap:mail": {},
                "urn:ietf:params:jmap:calendars": {},
                "urn:ietf:params:jmap:submission": {}
            },
            "accounts": {}
        });
        let session: JmapSession = serde_json::from_value(session_json).unwrap();
        assert!(session.capabilities.contains_key(JMAP_CAL_CAPABILITY));
        assert_eq!(
            session.primary_accounts.get(JMAP_CAL_CAPABILITY),
            Some(&"acct-cal-456".to_string())
        );
    }

    #[test]
    fn test_jmap_session_no_calendar_capability() {
        let session_json = json!({
            "username": "test@example.com",
            "apiUrl": "https://stalwart.example.com/jmap/api/",
            "downloadUrl": "https://stalwart.example.com/jmap/download/{blobId}",
            "uploadUrl": "https://stalwart.example.com/jmap/upload/{accountId}",
            "eventSourceUrl": "https://stalwart.example.com/jmap/eventsource",
            "state": "state-1",
            "primaryAccounts": {
                "urn:ietf:params:jmap:mail": "acct-mail-123"
            },
            "capabilities": {
                "urn:ietf:params:jmap:core": {},
                "urn:ietf:params:jmap:mail": {}
            },
            "accounts": {}
        });
        let session: JmapSession = serde_json::from_value(session_json).unwrap();
        assert!(!session.capabilities.contains_key(JMAP_CAL_CAPABILITY));
        assert!(!session.primary_accounts.contains_key(JMAP_CAL_CAPABILITY));
    }

    #[test]
    fn test_calendar_capability_report_full_stalwart_details() {
        // Stalwart v0.16.x advertises the full draft-ietf-jmap-calendars
        // §1.5.1 capability object.
        let details = json!({
            "maxCalendarsPerEvent": 1,
            "minDateTime": "0000-01-01T00:00:00Z",
            "maxDateTime": "9999-12-31T23:59:59Z",
            "maxExpandedQueryDuration": "P36500D",
            "maxParticipantsPerEvent": 500,
            "mayCreateCalendar": true
        });
        let mut report = CalendarCapabilityReport {
            capability_present: true,
            calendar_get_ok: true,
            calendar_event_query_ok: true,
            structured_recurrence_ok: true,
            ics_round_trip_ok: true,
            ..Default::default()
        };
        report.extract_capability_details(&details);
        assert_eq!(report.max_calendars_per_event, Some(1));
        assert_eq!(
            report.min_date_time.as_deref(),
            Some("0000-01-01T00:00:00Z")
        );
        assert_eq!(
            report.max_date_time.as_deref(),
            Some("9999-12-31T23:59:59Z")
        );
        assert_eq!(
            report.max_expanded_query_duration.as_deref(),
            Some("P36500D")
        );
        assert_eq!(report.max_participants_per_event, Some(500));
        assert_eq!(report.may_create_calendar, Some(true));
        assert!(report.issues.is_empty());
        assert!(report.is_healthy());
    }

    #[test]
    fn test_calendar_capability_report_flags_may_create_calendar_false() {
        let details = json!({"mayCreateCalendar": false});
        let mut report = CalendarCapabilityReport::default();
        report.extract_capability_details(&details);
        assert_eq!(report.may_create_calendar, Some(false));
        assert!(
            report
                .issues
                .iter()
                .any(|i| i.contains("mayCreateCalendar"))
        );
    }

    #[test]
    fn test_calendar_capability_report_flags_wrong_types() {
        let details = json!({
            "maxCalendarsPerEvent": "one",
            "minDateTime": 42,
            "mayCreateCalendar": "yes"
        });
        let mut report = CalendarCapabilityReport::default();
        report.extract_capability_details(&details);
        // Only the three wrong-TYPE fields are fatal issues; the absent
        // metadata fields are non-fatal warnings (Stalwart advertises `{}`).
        assert_eq!(report.issues.len(), 3);
        assert!(
            report
                .issues
                .iter()
                .any(|i| i.contains("maxCalendarsPerEvent") && !i.contains("missing"))
        );
        assert!(
            report
                .issues
                .iter()
                .any(|i| i.contains("minDateTime") && !i.contains("missing"))
        );
        assert!(
            report
                .issues
                .iter()
                .any(|i| i.contains("mayCreateCalendar") && !i.contains("missing"))
        );
        // The three absent fields are warnings, not issues.
        assert_eq!(report.warnings.len(), 3);
        assert!(report.warnings.iter().any(|i| i.contains("maxDateTime")));
        assert!(
            report
                .warnings
                .iter()
                .any(|i| i.contains("maxExpandedQueryDuration"))
        );
        assert!(
            report
                .warnings
                .iter()
                .any(|i| i.contains("maxParticipantsPerEvent"))
        );
    }

    #[test]
    fn test_calendar_capability_report_empty_object_is_healthy_with_warnings() {
        // Stalwart v1.0.x advertises `urn:ietf:params:jmap:calendars` as `{}`
        // and works correctly; an empty capability object must not fail the
        // functional health check — the gaps are surfaced as warnings.
        let mut report = CalendarCapabilityReport {
            capability_present: true,
            calendar_get_ok: true,
            calendar_event_query_ok: true,
            structured_recurrence_ok: true,
            ics_round_trip_ok: true,
            ..Default::default()
        };
        report.extract_capability_details(&json!({}));
        assert!(report.issues.is_empty(), "issues: {:?}", report.issues);
        assert_eq!(report.warnings.len(), 6);
        assert!(report.is_healthy());
    }

    #[test]
    fn test_calendar_capability_report_accepts_null_max_limits() {
        // The §1.5.1 max*PerEvent limits may be explicit null ("no limit").
        let details = json!({
            "maxCalendarsPerEvent": null,
            "minDateTime": "0000-01-01T00:00:00Z",
            "maxDateTime": "9999-12-31T23:59:59Z",
            "maxExpandedQueryDuration": "P36500D",
            "maxParticipantsPerEvent": null,
            "mayCreateCalendar": true
        });
        let mut report = CalendarCapabilityReport {
            capability_present: true,
            calendar_get_ok: true,
            calendar_event_query_ok: true,
            structured_recurrence_ok: true,
            ics_round_trip_ok: true,
            ..Default::default()
        };
        report.extract_capability_details(&details);
        assert_eq!(report.max_calendars_per_event, None);
        assert_eq!(report.max_participants_per_event, None);
        assert!(report.issues.is_empty(), "issues: {:?}", report.issues);
        assert!(report.is_healthy());
    }

    #[test]
    fn test_calendar_capability_report_non_object_details() {
        let mut report = CalendarCapabilityReport::default();
        report.extract_capability_details(&json!(null));
        assert_eq!(report.issues.len(), 1);
        assert!(report.issues[0].contains("not a JSON object"));
    }

    #[test]
    fn test_calendar_capability_report_is_healthy() {
        let mut report = CalendarCapabilityReport::default();
        assert!(!report.is_healthy());
        report.capability_present = true;
        report.calendar_get_ok = true;
        report.calendar_event_query_ok = true;
        assert!(!report.is_healthy());
        report.structured_recurrence_ok = true;
        assert!(!report.is_healthy());
        report.ics_round_trip_ok = true;
        // The functional probes are what determine health: an empty capability
        // object (Stalwart v1.0.x) is healthy as long as the probes pass.
        assert!(report.is_healthy());
        report.extract_capability_details(&json!({
            "maxCalendarsPerEvent": 1,
            "minDateTime": "0000-01-01T00:00:00Z",
            "maxDateTime": "9999-12-31T23:59:59Z",
            "maxExpandedQueryDuration": "P36500D",
            "maxParticipantsPerEvent": 500,
            "mayCreateCalendar": true
        }));
        assert!(report.is_healthy());
        report.issues.push("any issue".to_string());
        assert!(!report.is_healthy());
    }

    #[test]
    fn test_jmap_email_address_camel_case_serde() {
        // Verify that JmapEmailAddress uses camelCase serialization
        let addr = JmapEmailAddress {
            name: Some("John Doe".to_string()),
            email: Some("john@example.com".to_string()),
        };
        let json = serde_json::to_value(&addr).unwrap();
        assert_eq!(json["name"], "John Doe");
        assert_eq!(json["email"], "john@example.com");

        // Roundtrip: serialize then deserialize
        let roundtrip: JmapEmailAddress = serde_json::from_value(json).unwrap();
        assert_eq!(roundtrip.name, Some("John Doe".to_string()));
        assert_eq!(roundtrip.email, Some("john@example.com".to_string()));
    }

    #[test]
    fn test_category_labels_excludes_all_system_keywords() {
        // Every reserved `$`-prefixed JMAP keyword is a system flag, not a user
        // category, and must never surface as an EWS Category — not just the
        // well-known four (`$draft`/`$seen`/`$important`/`$recent`).
        let mut keywords = HashMap::new();
        for k in [
            "$draft",
            "$seen",
            "$important",
            "$recent",
            "$flagged",
            "$answered",
            "$junk",
            "$notjunk",
            "$forwarded",
            "$phishing",
        ] {
            keywords.insert(k.to_string(), true);
        }
        // User labels pass through untouched.
        keywords.insert("Project X".to_string(), true);
        keywords.insert("urgent".to_string(), true);

        let email = JmapEmail {
            keywords: Some(keywords),
            ..Default::default()
        };
        let mut labels = email.category_labels();
        labels.sort();
        assert_eq!(labels, vec!["Project X".to_string(), "urgent".to_string()]);
    }

    #[test]
    fn test_category_labels_none_returns_empty() {
        let email = JmapEmail {
            keywords: None,
            ..Default::default()
        };
        assert!(email.category_labels().is_empty());
    }

    #[test]
    fn test_jmap_method_call_serializes_as_array_per_rfc8621() {
        // RFC 8621 §3.2: Each method invocation is a 3-element array
        // ["methodName", {arguments}, "id"], NOT an object.
        // Stalwart rejects object-form with 400 notRequest:
        // "invalid type: map, expected an array with 3 elements"
        let call = JmapMethodCall {
            name: "Email/query".to_string(),
            arguments: serde_json::json!({"accountId": "u123"}),
            id: "e0".to_string(),
        };
        let json = serde_json::to_value(&call).unwrap();
        assert!(
            json.is_array(),
            "JmapMethodCall must serialize as array, got: {json}"
        );
        let arr = json.as_array().unwrap();
        assert_eq!(arr.len(), 3, "JmapMethodCall array must have 3 elements");
        assert_eq!(arr[0], "Email/query");
        assert_eq!(arr[1]["accountId"], "u123");
        assert_eq!(arr[2], "e0");
    }

    #[test]
    fn test_jmap_request_method_calls_serializes_as_array_of_arrays() {
        let request = JmapRequest {
            using: vec!["urn:ietf:params:jmap:core".to_string()],
            method_calls: vec![
                JmapMethodCall {
                    name: "Email/query".to_string(),
                    arguments: serde_json::json!({"accountId": "u1"}),
                    id: "e0".to_string(),
                },
                JmapMethodCall {
                    name: "Email/get".to_string(),
                    arguments: serde_json::json!({"accountId": "u1", "#ids": {}}),
                    id: "e1".to_string(),
                },
            ],
        };
        let json = serde_json::to_string(&request).unwrap();
        // Verify methodCalls contains arrays, not objects
        let parsed: Value = serde_json::from_str(&json).unwrap();
        let method_calls = &parsed["methodCalls"];
        assert!(method_calls.is_array());
        let calls = method_calls.as_array().unwrap();
        assert_eq!(calls.len(), 2);
        // Each call must be a 3-element array, not an object
        assert!(
            calls[0].is_array(),
            "First methodCall must be array, got: {}",
            calls[0]
        );
        assert!(
            calls[1].is_array(),
            "Second methodCall must be array, got: {}",
            calls[1]
        );
        assert_eq!(calls[0][0], "Email/query");
        assert_eq!(calls[0][2], "e0");
        assert_eq!(calls[1][0], "Email/get");
        assert_eq!(calls[1][2], "e1");
    }

    #[test]
    fn test_one_or_array() {
        // Helper type for testing
        #[derive(Deserialize, PartialEq, Debug)]
        struct Wrapper {
            #[serde(default, deserialize_with = "super::one_or_array")]
            pub values: Option<Vec<String>>,
        }

        // Single object becomes Some(vec![value])
        let json_single = r#"{"values": "a"}"#;
        let w: Wrapper = serde_json::from_str(json_single).unwrap();
        assert_eq!(w.values, Some(vec!["a".to_string()]));

        // Array becomes Some(vec![...])
        let json_array = r#"{"values": ["a", "b"]}"#;
        let w: Wrapper = serde_json::from_str(json_array).unwrap();
        assert_eq!(w.values, Some(vec!["a".to_string(), "b".to_string()]));

        // null becomes None
        let json_null = r#"{"values": null}"#;
        let w: Wrapper = serde_json::from_str(json_null).unwrap();
        assert_eq!(w.values, None);

        // Missing field becomes None (due to #[serde(default)])
        let json_missing = r#"{}"#;
        let w: Wrapper = serde_json::from_str(json_missing).unwrap();
        assert_eq!(w.values, None);

        // Empty array becomes Some(empty vec)
        let json_empty = r#"{"values": []}"#;
        let w: Wrapper = serde_json::from_str(json_empty).unwrap();
        assert_eq!(w.values, Some(vec![] as Vec<String>));
    }

    /// Regression for PR #1825 review: `move_emails` must key the `Email/set`
    /// update patch by email id — NOT zip `email_ids` with the `Email/get`
    /// response list. When the response is partial (an id is missing) or
    /// reordered, the old `zip` either swapped mailboxIds patches between
    /// unrelated emails or silently dropped ids. The pure helper builds the
    /// patch keyed-by-id and skips missing ids.
    #[test]
    fn build_move_update_patch_skips_missing_and_keys_by_id() {
        use std::collections::HashMap;
        // Client wants to move a, b, c.
        let email_ids = vec!["a".to_string(), "b".to_string(), "c".to_string()];
        // Server's Email/get returned only a and c, AND in reversed order
        // (c before a). "b" is missing entirely.
        let mut current: HashMap<String, Vec<String>> = HashMap::new();
        current.insert("a".to_string(), vec!["inbox".to_string()]);
        current.insert("c".to_string(), vec!["drafts".to_string()]);
        // (no "b")

        let update = super::build_move_update_patch(&email_ids, &current, "archive");

        // Exactly the two present ids are patched; the missing id is skipped
        // (no silent drop of the others, no entry for "b").
        assert_eq!(update.len(), 2);
        assert!(update.contains_key("a"));
        assert!(update.contains_key("c"));
        assert!(!update.contains_key("b"));

        // And the patches are associated with the CORRECT current mailboxIds
        // — the zip bug would have applied c's "drafts" removal to a.
        let patch_a = update.get("a").and_then(|v| v.as_object()).unwrap();
        assert_eq!(patch_a.get("mailboxIds/inbox"), Some(&json!(null)));
        assert_eq!(patch_a.get("mailboxIds/archive"), Some(&json!(true)));
        assert!(
            patch_a.get("mailboxIds/drafts").is_none(),
            "a's patch must not pick up c's drafts id"
        );

        let patch_c = update.get("c").and_then(|v| v.as_object()).unwrap();
        assert_eq!(patch_c.get("mailboxIds/drafts"), Some(&json!(null)));
        assert_eq!(patch_c.get("mailboxIds/archive"), Some(&json!(true)));
        assert!(
            patch_c.get("mailboxIds/inbox").is_none(),
            "c's patch must not pick up a's inbox id"
        );
    }

    /// All requested ids absent → empty update (move_emails returns Ok(0),
    /// dispatch encodes PartialCompletion rather than a hard error).
    #[test]
    fn build_move_update_patch_empty_when_no_ids_resolved() {
        use std::collections::HashMap;
        let email_ids = vec!["x".to_string(), "y".to_string()];
        let current: HashMap<String, Vec<String>> = HashMap::new();
        let update = super::build_move_update_patch(&email_ids, &current, "archive");
        assert!(update.is_empty());
    }

    /// A removed-from-everything move still adds the target, so the email is
    /// only in the target mailbox afterwards.
    #[test]
    fn build_move_update_patch_multi_mailbox_source() {
        use std::collections::HashMap;
        let email_ids = vec!["m".to_string()];
        let mut current: HashMap<String, Vec<String>> = HashMap::new();
        current.insert("m".to_string(), vec!["i1".to_string(), "i2".to_string()]);
        let update = super::build_move_update_patch(&email_ids, &current, "t");
        assert_eq!(update.len(), 1);
        let patch = update.get("m").and_then(|v| v.as_object()).unwrap();
        assert_eq!(patch.get("mailboxIds/i1"), Some(&json!(null)));
        assert_eq!(patch.get("mailboxIds/i2"), Some(&json!(null)));
        assert_eq!(patch.get("mailboxIds/t"), Some(&json!(true)));
    }

    #[test]
    fn parse_email_set_outcome_success_records_updated() {
        // A clean Email/set reply with `updated` => fully-applied success.
        // RFC 8621 §5.3 defines `updated` as a map of id -> null, which is what
        // Stalwart returns; the legacy array form is still tolerated.
        let resp = vec![(
            "Email/set".to_string(),
            json!({ "accountId": "a", "updated": { "M-1": null } }),
            "es0".to_string(),
        )];
        let o = super::parse_email_set_outcome(resp);
        assert_eq!(o.updated, vec!["M-1"]);
        assert!(o.not_updated.is_empty());
        assert!(o.method_error.is_none());

        let resp_arr = vec![(
            "Email/set".to_string(),
            json!({ "accountId": "a", "updated": ["M-1"] }),
            "es0".to_string(),
        )];
        assert_eq!(
            super::parse_email_set_outcome(resp_arr).updated,
            vec!["M-1"]
        );
    }

    #[test]
    fn parse_email_set_outcome_records_per_id_rejection() {
        // `notUpdated` => the server refused an id; the MAPI property-write
        // handlers must NOT mask this as success.
        let resp = vec![(
            "Email/set".to_string(),
            json!({
                "accountId": "a",
                "updated": {},
                "notUpdated": {
                    "M-2": { "type": "notFound", "description": "no such email" }
                }
            }),
            "es0".to_string(),
        )];
        let o = super::parse_email_set_outcome(resp);
        assert!(o.updated.is_empty());
        assert_eq!(o.not_updated.len(), 1);
        assert_eq!(o.not_updated[0].0, "M-2");
        assert_eq!(o.not_updated[0].1, "no such email");
        assert!(o.method_error.is_none());
    }

    #[test]
    fn parse_email_set_outcome_records_method_error() {
        // A method-level `error` (rate limit / serverFail) => the whole
        // apply failed.
        let resp = vec![(
            "Email/set".to_string(),
            json!({ "accountId": "a", "error": { "type": "serverFail", "description": "boom" } }),
            "es0".to_string(),
        )];
        let o = super::parse_email_set_outcome(resp);
        assert!(o.updated.is_empty());
        assert!(o.not_updated.is_empty());
        assert_eq!(o.method_error.as_deref(), Some("boom"));
    }

    #[test]
    fn parse_email_set_outcome_ignores_other_methods() {
        // A non-Email/set response must not pollute the outcome.
        let resp = vec![
            ("error".to_string(), json!(""), "es0".to_string()),
            (
                "Email/set".to_string(),
                json!({ "accountId": "a", "updated": { "M-9": null } }),
                "es0".to_string(),
            ),
        ];
        let o = super::parse_email_set_outcome(resp);
        assert_eq!(o.updated, vec!["M-9"]);
    }

    #[test]
    fn build_submission_success_patch_moves_drafts_to_sent() {
        let patches = ["draft-1".to_string()];
        let sent = ["sent-1".to_string()];
        let value = super::build_submission_success_patch(&patches, &sent).unwrap();

        let obj = value.as_object().expect("patch must be an object");
        assert_eq!(obj["mailboxIds/draft-1"], json!(null));
        assert_eq!(obj["mailboxIds/sent-1"], json!(true));
        assert_eq!(obj["keywords/$draft"], json!(null));
    }

    #[test]
    fn build_submission_success_patch_handles_multiple_mailboxes() {
        let patches = ["draft-1".to_string(), "draft-2".to_string()];
        let sent = ["sent-1".to_string(), "sent-2".to_string()];
        let value = super::build_submission_success_patch(&patches, &sent).unwrap();

        let obj = value.as_object().expect("patch must be an object");
        assert_eq!(obj["mailboxIds/draft-1"], json!(null));
        assert_eq!(obj["mailboxIds/draft-2"], json!(null));
        assert_eq!(obj["mailboxIds/sent-1"], json!(true));
        assert_eq!(obj["mailboxIds/sent-2"], json!(true));
    }

    #[test]
    fn build_submission_success_patch_rejects_empty_drafts() {
        let patches: Vec<String> = Vec::new();
        let sent = vec!["sent-1".to_string()];
        let result = super::build_submission_success_patch(&patches, &sent);
        assert!(result.is_err(), "must reject when Drafts is empty");
    }

    #[test]
    fn build_submission_success_patch_rejects_empty_sent() {
        let patches = vec!["draft-1".to_string()];
        let sent: Vec<String> = Vec::new();
        let result = super::build_submission_success_patch(&patches, &sent);
        assert!(result.is_err(), "must reject when Sent is empty");
    }

    #[test]
    fn build_submission_success_patch_rejects_both_empty() {
        let patches: Vec<String> = Vec::new();
        let sent: Vec<String> = Vec::new();
        let result = super::build_submission_success_patch(&patches, &sent);
        assert!(result.is_err(), "must reject when both roles are empty");
    }

    fn session_json(base: &str) -> Value {
        json!({
            "apiUrl": base,
            "downloadUrl": format!("{base}/download/{{accountId}}/{{blobId}}"),
            "uploadUrl": format!("{base}/upload/{{accountId}}"),
            "eventSourceUrl": format!("{base}/eventsource"),
            "state": "s",
            "username": "u",
            "accounts": {},
            "primaryAccounts": {},
            "capabilities": {}
        })
    }

    /// Spawn a stub JMAP server; blob sizes and Content-Length behaviour are
    /// selected by the blobId path segment:
    /// `exact-{n}` = n bytes, `chunked-{n}` = n bytes with no Content-Length,
    /// `long-{n}` = n bytes with Content-Length, `missing` = 404, `denied` = 403.
    async fn spawn_stub_jmap_server() -> String {
        use axum::extract::Path as AxumPath;
        use axum::response::Response;
        use axum::routing::get;

        async fn session(axum::Extension(base): axum::Extension<String>) -> axum::Json<Value> {
            axum::Json(session_json(&base))
        }
        async fn download(AxumPath((_account, blob)): AxumPath<(String, String)>) -> Response {
            if blob == "missing" {
                return Response::builder()
                    .status(axum::http::StatusCode::NOT_FOUND)
                    .body(axum::body::Body::empty())
                    .unwrap();
            }
            if blob == "denied" {
                return Response::builder()
                    .status(axum::http::StatusCode::FORBIDDEN)
                    .body(axum::body::Body::empty())
                    .unwrap();
            }
            let (n, with_length) = if let Some(rest) = blob.strip_prefix("exact-") {
                (rest.parse::<usize>().unwrap(), true)
            } else if let Some(rest) = blob.strip_prefix("chunked-") {
                (rest.parse::<usize>().unwrap(), false)
            } else {
                (6_000_000, true)
            };
            let payload = vec![b'x'; n];
            let bytes = axum::body::Bytes::from(payload);
            if with_length {
                Response::builder()
                    .header("content-length", n.to_string())
                    .body(axum::body::Body::from(bytes))
                    .unwrap()
            } else {
                // Wrap in a stream so no Content-Length is set and the body is
                // delivered in multiple chunks mid-transfer.
                let chunks: Vec<Result<axum::body::Bytes, std::io::Error>> = bytes
                    .chunks(64 * 1024)
                    .map(|c| Ok(axum::body::Bytes::copy_from_slice(c)))
                    .collect();
                let stream = futures_util::stream::iter(chunks);
                Response::new(axum::body::Body::from_stream(stream))
            }
        }

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let base = format!("http://127.0.0.1:{port}");
        let app = axum::Router::new()
            .route("/session", get(session))
            .route("/download/{account}/{blob}", get(download))
            .layer(axum::Extension(base.clone()));
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        base
    }

    #[tokio::test]
    async fn download_blob_capped_succeeds_at_exact_cap() {
        let base = spawn_stub_jmap_server().await;
        let client = JmapClient::new(&base).unwrap();
        let pw = SecretString::from("pw");
        let (out, _ct) = client
            .download_blob_capped("a", "exact-1024", "u", &pw, 1024)
            .await
            .expect("exact-cap download must succeed");
        assert_eq!(out.len(), 1024);
    }

    #[tokio::test]
    async fn download_blob_capped_rejects_invalid_blob_id() {
        let base = spawn_stub_jmap_server().await;
        let client = JmapClient::new(&base).unwrap();
        let pw = SecretString::from("pw");
        for bad in ["../../session", "../x", "a?b=1", "a/b", "a%2f..", ""] {
            let err = client
                .download_blob_capped("a", bad, "u", &pw, 1024)
                .await
                .expect_err("must reject non-RFC-8620 blobId characters");
            assert!(err.to_string().contains("Invalid JMAP blobId"));
        }
    }

    #[tokio::test]
    async fn download_blob_capped_bails_on_content_length() {
        let base = spawn_stub_jmap_server().await;
        let client = JmapClient::new(&base).unwrap();
        let pw = SecretString::from("pw");
        let err = client
            .download_blob_capped("a", "long-6000000", "u", &pw, 1024)
            .await
            .expect_err("must reject when Content-Length exceeds cap");
        assert!(err.to_string().contains("Content-Length"));
    }

    #[tokio::test]
    async fn download_blob_capped_aborts_oversized_stream() {
        let base = spawn_stub_jmap_server().await;
        let client = JmapClient::new(&base).unwrap();
        let pw = SecretString::from("pw");
        let err = client
            .download_blob_capped("a", "chunked-6000000", "u", &pw, 272_144)
            .await
            .expect_err("must abort mid-stream once cap exceeded");
        assert!(err.to_string().contains("exceeds"));
    }

    // --- download_blob_range ([MS-ASCMD] §2.2.3.143.2 byte ranges) ---

    #[tokio::test]
    async fn download_blob_range_serves_whole_item() {
        let base = spawn_stub_jmap_server().await;
        let client = JmapClient::new(&base).unwrap();
        let pw = SecretString::from("pw");
        let fetch = client
            .download_blob_range("a", "exact-1000", "u", &pw, None, 1024)
            .await
            .expect("whole-item download must succeed");
        assert_eq!(fetch.data.len(), 1000);
        assert_eq!(fetch.total_size, Some(1000));
        assert_eq!(fetch.window, (0, 1000));
        assert!(fetch.complete);
    }

    #[tokio::test]
    async fn download_blob_range_serves_window_with_content_length() {
        let base = spawn_stub_jmap_server().await;
        let client = JmapClient::new(&base).unwrap();
        let pw = SecretString::from("pw");
        let fetch = client
            .download_blob_range("a", "exact-1000", "u", &pw, Some(100..300), 1024)
            .await
            .expect("windowed download must succeed");
        assert_eq!(fetch.data.len(), 200, "window 100-299 = 200 bytes");
        assert_eq!(fetch.window, (100, 300));
        assert_eq!(fetch.total_size, Some(1000));
        assert!(fetch.complete);
    }

    #[tokio::test]
    async fn download_blob_range_clamps_window_at_eof() {
        let base = spawn_stub_jmap_server().await;
        let client = JmapClient::new(&base).unwrap();
        let pw = SecretString::from("pw");
        // No Content-Length: the true total is only learned at EOF, and EOF
        // cuts the requested window short — status 17 territory. The
        // window's *requested* span stays within the budget; what's
        // actually served is fewer bytes.
        let fetch = client
            .download_blob_range("a", "chunked-600", "u", &pw, Some(500..1500), 1024)
            .await
            .expect("clamped window must succeed");
        assert_eq!(fetch.data.len(), 100, "only bytes 500-599 exist");
        assert_eq!(fetch.window, (500, 600));
        assert_eq!(fetch.total_size, Some(600));
        assert!(
            !fetch.complete,
            "short window maps to ItemOperations status 17"
        );
    }

    #[tokio::test]
    async fn download_blob_range_rejects_range_past_eof() {
        let base = spawn_stub_jmap_server().await;
        let client = JmapClient::new(&base).unwrap();
        let pw = SecretString::from("pw");
        let err = client
            .download_blob_range("a", "exact-100", "u", &pw, Some(100..200), 1024)
            .await
            .expect_err("a range starting at EOF has no bytes");
        assert!(matches!(
            err,
            super::BlobFetchError::RangeStartsPastEof { total: 100 }
        ));
    }

    #[tokio::test]
    async fn download_blob_range_reports_total_for_multichunk_stream() {
        let base = spawn_stub_jmap_server().await;
        let client = JmapClient::new(&base).unwrap();
        let pw = SecretString::from("pw");
        // No Content-Length: the total is only known at EOF, and the 200 KiB
        // body arrives as four stream chunks. The reported Total must be the
        // whole item's size, not the first chunk's end — a client planning
        // follow-up windows from <Total> would otherwise under-fetch.
        let fetch = client
            .download_blob_range("a", "chunked-200000", "u", &pw, None, 262_144)
            .await
            .expect("multi-chunk whole-item download must succeed");
        assert_eq!(fetch.data.len(), 200_000);
        assert_eq!(fetch.total_size, Some(200_000));
        assert_eq!(fetch.window, (0, 200_000));
        assert!(fetch.complete);

        let fetch = client
            .download_blob_range(
                "a",
                "chunked-200000",
                "u",
                &pw,
                Some(150_000..150_100),
                262_144,
            )
            .await
            .expect("windowed multi-chunk download must succeed");
        assert_eq!(fetch.data.len(), 100);
        assert_eq!(fetch.window, (150_000, 150_100));
        assert_eq!(fetch.total_size, Some(200_000));
        assert!(fetch.complete);
    }

    #[tokio::test]
    async fn download_blob_range_rejects_range_past_eof_without_content_length() {
        let base = spawn_stub_jmap_server().await;
        let client = JmapClient::new(&base).unwrap();
        let pw = SecretString::from("pw");
        // Streamed body, no Content-Length: EOF is the first moment the
        // range's invalidity is provable, and the error must carry the bytes
        // actually observed — not an inverted window that would render as a
        // nonsensical <Range> or underflow the inclusive-end conversion.
        let err = client
            .download_blob_range("a", "chunked-300", "u", &pw, Some(500..600), 1024)
            .await
            .expect_err("a streamed range starting past EOF has no bytes");
        assert!(matches!(
            err,
            super::BlobFetchError::RangeStartsPastEof { total: 300 }
        ));
    }

    #[tokio::test]
    async fn download_blob_range_rejects_oversized_window() {
        let base = spawn_stub_jmap_server().await;
        let client = JmapClient::new(&base).unwrap();
        let pw = SecretString::from("pw");
        let err = client
            .download_blob_range("a", "exact-100000", "u", &pw, Some(0..2000), 1024)
            .await
            .expect_err("window over the per-request budget must fail");
        assert!(matches!(
            err,
            super::BlobFetchError::RangeTooLarge {
                requested: 2000,
                limit: 1024
            }
        ));
    }

    #[tokio::test]
    async fn download_blob_range_rejects_whole_item_over_cap_via_content_length() {
        let base = spawn_stub_jmap_server().await;
        let client = JmapClient::new(&base).unwrap();
        let pw = SecretString::from("pw");
        let err = client
            .download_blob_range("a", "exact-6000000", "u", &pw, None, 1024)
            .await
            .expect_err("whole-item over the budget must fail");
        assert!(matches!(
            err,
            super::BlobFetchError::TooLarge {
                total: 6_000_000,
                limit: 1024
            }
        ));
    }

    #[tokio::test]
    async fn download_blob_range_aborts_chunked_item_over_cap_mid_stream() {
        let base = spawn_stub_jmap_server().await;
        let client = JmapClient::new(&base).unwrap();
        let pw = SecretString::from("pw");
        let err = client
            .download_blob_range("a", "chunked-6000000", "u", &pw, None, 272_144)
            .await
            .expect_err("chunked oversized item must abort mid-stream");
        assert!(matches!(err, super::BlobFetchError::TooLarge { .. }));
    }

    #[tokio::test]
    async fn download_blob_range_rejects_invalid_blob_id() {
        let base = spawn_stub_jmap_server().await;
        let client = JmapClient::new(&base).unwrap();
        let pw = SecretString::from("pw");
        for bad in ["../../session", "a/b", "a?b", ""] {
            let err = client
                .download_blob_range("a", bad, "u", &pw, None, 1024)
                .await
                .expect_err("must reject non-RFC-8620 blobId characters");
            assert!(matches!(err, super::BlobFetchError::InvalidBlobId));
        }
    }

    #[tokio::test]
    async fn download_blob_range_classifies_not_found_as_invalid_attachment() {
        let base = spawn_stub_jmap_server().await;
        let client = JmapClient::new(&base).unwrap();
        let pw = SecretString::from("pw");
        let err = client
            .download_blob_range("a", "missing", "u", &pw, None, 1024)
            .await
            .expect_err("404 must classify as NotFound (ItemOperations status 15)");
        assert!(matches!(err, super::BlobFetchError::NotFound(_)));
    }

    #[tokio::test]
    async fn download_blob_range_classifies_forbidden_as_access_denied() {
        let base = spawn_stub_jmap_server().await;
        let client = JmapClient::new(&base).unwrap();
        let pw = SecretString::from("pw");
        let err = client
            .download_blob_range("a", "denied", "u", &pw, None, 1024)
            .await
            .expect_err("403 must classify as AccessDenied (ItemOperations status 16)");
        assert!(matches!(err, super::BlobFetchError::AccessDenied(_)));
    }

    #[tokio::test]
    async fn download_blob_range_single_byte_window() {
        let base = spawn_stub_jmap_server().await;
        let client = JmapClient::new(&base).unwrap();
        let pw = SecretString::from("pw");
        // "m-n" with m == n names exactly one byte.
        let fetch = client
            .download_blob_range("a", "exact-1000", "u", &pw, Some(7..8), 1024)
            .await
            .expect("single-byte window must succeed");
        assert_eq!(fetch.data.len(), 1);
        assert_eq!(fetch.window, (7, 8));
        assert!(fetch.complete);
    }

    /// Spawn a stub whose `/session` always returns `status` (200 serves a
    /// valid session body). Used to exercise `check_credentials`' real
    /// classification path.
    async fn spawn_auth_stub_server(status: axum::http::StatusCode) -> String {
        use axum::response::Response;
        use axum::routing::get;

        let status_slot = status;
        let handler = move || {
            let status = status_slot;
            async move {
                Response::builder()
                    .status(status)
                    .header("content-type", "application/json")
                    .body(axum::body::Body::from(
                        serde_json::to_vec(&session_json("http://stub")).unwrap(),
                    ))
                    .unwrap()
            }
        };

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let base = format!("http://127.0.0.1:{port}");
        let app = axum::Router::new().route("/session", get(handler));
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        base
    }

    /// A stub auth server with a mutable status, a request counter, and a
    /// mutable "accepted password" for password-sensitive scenarios, used to
    /// prove the session-auth cache collapses probes (AUDIT.md §18). When
    /// `accept_password` is `Some`, the handler answers 200 only for that
    /// Basic-auth password and 401 otherwise; when `None` it answers with the
    /// configured `status` verbatim.
    struct CountingStub {
        base: String,
        status: std::sync::Arc<std::sync::atomic::AtomicU16>,
        hits: std::sync::Arc<std::sync::atomic::AtomicU32>,
        accept_password: std::sync::Arc<std::sync::Mutex<Option<String>>>,
    }

    impl CountingStub {
        async fn spawn(initial: axum::http::StatusCode) -> Self {
            use axum::response::Response;
            use axum::routing::get;
            use std::sync::Arc;
            use std::sync::atomic::{AtomicU16, AtomicU32, Ordering};

            #[derive(Clone)]
            struct Ctl {
                status: Arc<AtomicU16>,
                hits: Arc<AtomicU32>,
                accept_password: Arc<std::sync::Mutex<Option<String>>>,
            }
            async fn handler(
                axum::Extension(ctl): axum::Extension<Ctl>,
                headers: axum::http::HeaderMap,
            ) -> Response {
                use axum::http::StatusCode;
                ctl.hits.fetch_add(1, Ordering::SeqCst);
                let accepted = ctl.accept_password.lock().unwrap().clone();
                let status = match accepted {
                    Some(expected) => {
                        if basic_auth_password(&headers).as_deref() == Some(expected.as_str()) {
                            StatusCode::OK.as_u16()
                        } else {
                            StatusCode::UNAUTHORIZED.as_u16()
                        }
                    }
                    None => ctl.status.load(Ordering::SeqCst),
                };
                Response::builder()
                    .status(status)
                    .header("content-type", "application/json")
                    .body(axum::body::Body::from(
                        serde_json::to_vec(&session_json("http://stub")).unwrap(),
                    ))
                    .unwrap()
            }

            let status = Arc::new(AtomicU16::new(initial.as_u16()));
            let hits = Arc::new(AtomicU32::new(0));
            let accept_password = Arc::new(std::sync::Mutex::new(None));
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let port = listener.local_addr().unwrap().port();
            let base = format!("http://127.0.0.1:{port}");
            let app = axum::Router::new().route(
                "/session",
                get(handler).layer(axum::Extension(Ctl {
                    status: status.clone(),
                    hits: hits.clone(),
                    accept_password: accept_password.clone(),
                })),
            );
            tokio::spawn(async move {
                axum::serve(listener, app).await.unwrap();
            });
            Self {
                base,
                status,
                hits,
                accept_password,
            }
        }

        fn hits(&self) -> u32 {
            self.hits.load(std::sync::atomic::Ordering::SeqCst)
        }
    }

    /// Decode the password from a Basic `Authorization` header, if present.
    fn basic_auth_password(headers: &axum::http::HeaderMap) -> Option<String> {
        use base64::Engine;
        let value = headers.get(axum::http::header::AUTHORIZATION)?;
        let encoded = value.to_str().ok()?.strip_prefix("Basic ")?;
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .ok()?;
        let text = String::from_utf8(decoded).ok()?;
        text.split_once(':').map(|(_, pw)| pw.to_string())
    }

    #[tokio::test]
    async fn probe_auth_caches_positive_until_ttl() {
        use axum::http::StatusCode;
        let stub = CountingStub::spawn(StatusCode::OK).await;
        // Short positive TTL so expiry is observable quickly.
        let client = JmapClient::with_session_auth(
            &stub.base,
            std::time::Duration::from_millis(150),
            std::time::Duration::from_secs(30),
            10_000,
        )
        .unwrap();
        let pw = SecretString::from("pw");

        // First probe hits the backend; the next two are cache hits.
        assert_eq!(client.probe_auth("u", &pw).await, CredentialCheck::Valid);
        assert_eq!(client.probe_auth("u", &pw).await, CredentialCheck::Valid);
        assert_eq!(client.probe_auth("u", &pw).await, CredentialCheck::Valid);
        assert_eq!(stub.hits(), 1, "positive result must be cached");

        // After the TTL the next probe re-hits the backend.
        tokio::time::sleep(std::time::Duration::from_millis(220)).await;
        assert_eq!(client.probe_auth("u", &pw).await, CredentialCheck::Valid);
        assert_eq!(stub.hits(), 2, "an expired positive entry must re-probe");
    }

    #[tokio::test]
    async fn probe_auth_negative_is_cached_and_evicted_on_login() {
        use axum::http::StatusCode;
        let stub = CountingStub::spawn(StatusCode::UNAUTHORIZED).await;
        let client = JmapClient::with_session_auth(
            &stub.base,
            std::time::Duration::from_secs(300),
            std::time::Duration::from_millis(500),
            10_000,
        )
        .unwrap();
        let pw = SecretString::from("pw");

        // Two bad-password probes collapse onto one backend hit.
        assert_eq!(client.probe_auth("u", &pw).await, CredentialCheck::Invalid);
        assert_eq!(client.probe_auth("u", &pw).await, CredentialCheck::Invalid);
        assert_eq!(stub.hits(), 1, "negative result must be cached");

        // Only "new-pw" is accepted from here on. A successful login with it
        // must evict every other cached credential for the user, so the stale
        // bad-password negative stops short-circuiting the old password.
        *stub.accept_password.lock().unwrap() = Some("new-pw".to_string());
        assert_eq!(
            client.probe_auth("u", &SecretString::from("new-pw")).await,
            CredentialCheck::Valid
        );
        let before = stub.hits();
        assert_eq!(
            client.probe_auth("u", &pw).await,
            CredentialCheck::Invalid,
            "the stale bad-password negative must have been evicted"
        );
        assert!(
            stub.hits() > before,
            "eviction must force a fresh probe for the old password"
        );
    }

    #[tokio::test]
    async fn probe_auth_cache_is_capacity_bounded() {
        use axum::http::StatusCode;
        // Tiny capacity so the bound is observable without a large loop.
        let stub = CountingStub::spawn(StatusCode::OK).await;
        let client = JmapClient::with_session_auth(
            &stub.base,
            std::time::Duration::from_secs(300),
            std::time::Duration::from_secs(30),
            8,
        )
        .unwrap();

        // Probe more distinct credentials than the cache can hold; each is a
        // valid login with its own password (the stub accepts any password).
        for i in 0..40 {
            let pw = SecretString::from(format!("pw-{i}"));
            assert_eq!(client.probe_auth("u", &pw).await, CredentialCheck::Valid);
        }
        // moka evicts lazily; force pending maintenance before counting.
        client.session_auth.run_pending_tasks();
        assert!(
            client.session_auth.entry_count() <= 8,
            "cache must stay within its configured capacity, got {}",
            client.session_auth.entry_count()
        );
    }

    #[tokio::test]
    async fn session_auth_positive_ttl_is_injected_not_hardcoded() {
        use axum::http::StatusCode;
        // A very short injected positive TTL must actually govern the entry, so
        // the client cache cannot silently extend the verifier's configured
        // `GATEWAY_AUTH_CACHE_TTL_SECS` contract.
        let stub = CountingStub::spawn(StatusCode::OK).await;
        let client = JmapClient::with_session_auth(
            &stub.base,
            std::time::Duration::from_millis(80),
            std::time::Duration::from_secs(30),
            10_000,
        )
        .unwrap();
        let pw = SecretString::from("pw");
        assert_eq!(client.probe_auth("u", &pw).await, CredentialCheck::Valid);
        // Within the injected TTL: served from cache.
        assert_eq!(stub.hits(), 1);
        // Past the injected TTL: re-probe (proving 300s was NOT hardcoded).
        tokio::time::sleep(std::time::Duration::from_millis(140)).await;
        assert_eq!(client.probe_auth("u", &pw).await, CredentialCheck::Valid);
        assert_eq!(
            stub.hits(),
            2,
            "the injected positive TTL must govern expiry, not a hardcoded 300s"
        );
    }

    #[tokio::test]
    async fn probe_auth_backend_error_is_never_cached() {
        use axum::http::StatusCode;
        let stub = CountingStub::spawn(StatusCode::SERVICE_UNAVAILABLE).await;
        let client = JmapClient::new(&stub.base).unwrap();
        let pw = SecretString::from("pw");

        assert!(matches!(
            client.probe_auth("u", &pw).await,
            CredentialCheck::BackendError(_)
        ));
        // Recovery must be observed by the very next request: no caching of the
        // outage means the next probe tries again and succeeds.
        stub.status
            .store(StatusCode::OK.as_u16(), std::sync::atomic::Ordering::SeqCst);
        assert_eq!(client.probe_auth("u", &pw).await, CredentialCheck::Valid);
        assert_eq!(
            stub.hits(),
            2,
            "an outage must never be cached; the next probe must hit the backend"
        );
    }

    #[tokio::test]
    async fn check_credentials_classification_matrix() {
        use axum::http::StatusCode;
        let pw = SecretString::from("pw");

        let valid = JmapClient::new(&spawn_auth_stub_server(StatusCode::OK).await).unwrap();
        assert_eq!(
            valid.check_credentials("u", &pw).await,
            CredentialCheck::Valid
        );

        for rejected in [StatusCode::UNAUTHORIZED, StatusCode::FORBIDDEN] {
            let client = JmapClient::new(&spawn_auth_stub_server(rejected).await).unwrap();
            assert_eq!(
                client.check_credentials("u", &pw).await,
                CredentialCheck::Invalid,
                "{rejected} must classify as Invalid"
            );
        }

        for backend_error in [
            StatusCode::INTERNAL_SERVER_ERROR,
            StatusCode::BAD_GATEWAY,
            StatusCode::SERVICE_UNAVAILABLE,
            StatusCode::TOO_MANY_REQUESTS,
        ] {
            let client = JmapClient::new(&spawn_auth_stub_server(backend_error).await).unwrap();
            assert!(
                matches!(
                    client.check_credentials("u", &pw).await,
                    CredentialCheck::BackendError(_)
                ),
                "{backend_error} must classify as BackendError (not cached, no password prompt)"
            );
        }
    }

    #[tokio::test]
    async fn check_credentials_rejects_non_session_2xx_body() {
        use axum::response::Response;
        use axum::routing::get;

        // Any 2xx that is not a JMAP session object (here an HTML 200, as a
        // proxy or login portal might return) must not authenticate anyone.
        let handler = || async {
            Response::builder()
                .status(200)
                .header("content-type", "text/html")
                .body(axum::body::Body::from("<html>sign in</html>"))
                .unwrap()
        };
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let base = format!("http://127.0.0.1:{port}");
        let app = axum::Router::new().route("/session", get(handler));
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        let client = JmapClient::new(&base).unwrap();
        let pw = SecretString::from("pw");
        assert!(
            matches!(
                client.check_credentials("u", &pw).await,
                CredentialCheck::BackendError(_)
            ),
            "a non-session 2xx body must be a BackendError, not Valid"
        );
    }

    #[tokio::test]
    async fn check_credentials_rejects_empty_204() {
        use axum::response::Response;
        use axum::routing::get;

        let handler = || async {
            Response::builder()
                .status(204)
                .body(axum::body::Body::empty())
                .unwrap()
        };
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let base = format!("http://127.0.0.1:{port}");
        let app = axum::Router::new().route("/session", get(handler));
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        let client = JmapClient::new(&base).unwrap();
        let pw = SecretString::from("pw");
        assert!(
            matches!(
                client.check_credentials("u", &pw).await,
                CredentialCheck::BackendError(_)
            ),
            "a 204 with no session body must be a BackendError, not Valid"
        );
    }

    #[tokio::test]
    async fn check_credentials_maps_transport_failure_to_backend_error() {
        // Port 1 is never listening: the connection is refused.
        let client = JmapClient::new("http://127.0.0.1:1").unwrap();
        let pw = SecretString::from("pw");
        let check = client.check_credentials("u", &pw).await;
        assert!(
            matches!(check, CredentialCheck::BackendError(_)),
            "transport failure must be BackendError, got {:?}",
            check
        );
    }

    #[test]
    fn public_deserialize_retains_raw_so_to_ics_works() {
        // The public serde path must retain the whole object in `raw`, or a
        // directly deserialized event loses the JSCalendar data `to_ics` needs.
        let event_json = json!({
            "id": "e1",
            "uid": "u-1",
            "title": "Team Sync",
            "start": "2026-01-05T09:00:00",
            "timeZone": "Europe/Berlin",
            "duration": "PT1H",
            "iCalendar": { "convertedProperties": { "duration": { "name": "dtend" } }, "name": "vevent" }
        });
        let event: JmapCalendarEvent = serde_json::from_value(event_json).unwrap();
        assert!(event.raw.is_some(), "raw must be captured on deserialize");
        let ics = event.to_ics().expect("ICS from deserialized event");
        assert!(ics.contains("SUMMARY:Team Sync"), "{ics}");
        assert!(ics.contains("UID:u-1"), "{ics}");
    }

    /// Spawn a stub JMAP server that serves `/session` and answers every API
    /// call (`POST /`) with `api_response`.
    async fn spawn_stub_jmap_api_server(api_response: Value) -> String {
        use axum::routing::{get, post};

        async fn session(axum::Extension(base): axum::Extension<String>) -> axum::Json<Value> {
            axum::Json(session_json(&base))
        }
        async fn api(axum::Extension(response): axum::Extension<Value>) -> axum::Json<Value> {
            axum::Json(response)
        }

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let base = format!("http://127.0.0.1:{port}");
        let app = axum::Router::new()
            .route("/session", get(session))
            .route("/", post(api))
            .layer(axum::Extension(api_response))
            .layer(axum::Extension(base.clone()));
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        base
    }

    /// Like [`spawn_stub_jmap_api_server`] but the session advertises the
    /// calendar capability with a primary calendar account, so calls that read
    /// `primaryAccounts[urn:ietf:params:jmap:calendars]` (e.g.
    /// `query_calendars`) work.
    async fn spawn_stub_calendar_jmap_server(api_response: Value) -> String {
        use axum::routing::{get, post};

        async fn session(axum::Extension(base): axum::Extension<String>) -> axum::Json<Value> {
            axum::Json(json!({
                "apiUrl": base,
                "downloadUrl": format!("{base}/download/{{accountId}}/{{blobId}}"),
                "uploadUrl": format!("{base}/upload/{{accountId}}"),
                "eventSourceUrl": format!("{base}/eventsource"),
                "state": "s",
                "username": "u",
                "accounts": {},
                "primaryAccounts": { JMAP_CAL_CAPABILITY: "acct-cal" },
                "capabilities": {
                    "urn:ietf:params:jmap:core": {},
                    JMAP_CAL_CAPABILITY: {}
                }
            }))
        }
        async fn api(axum::Extension(response): axum::Extension<Value>) -> axum::Json<Value> {
            axum::Json(response)
        }

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let base = format!("http://127.0.0.1:{port}");
        let app = axum::Router::new()
            .route("/session", get(session))
            .route("/", post(api))
            .layer(axum::Extension(api_response))
            .layer(axum::Extension(base.clone()));
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        base
    }

    #[tokio::test]
    async fn query_calendar_events_counts_unparseable_objects() {
        let response = json!({
            "methodResponses": [
                ["CalendarEvent/query", { "ids": ["e1", "e2"], "total": 2, "queryState": "s1" }, "eq0"],
                ["CalendarEvent/get", { "list": [
                    { "id": "e1", "uid": "u1", "title": "Good", "start": "2026-01-05T09:00:00", "timeZone": "Etc/UTC", "duration": "PT1H", "iCalendar": {"name":"vevent"} },
                    { "id": 12345 }
                ] }, "eg0"]
            ]
        });
        let base = spawn_stub_jmap_api_server(response).await;
        let client = JmapClient::new(&base).unwrap();
        let pw = SecretString::from("pw");
        let result = client
            .query_calendar_events(QueryCalendarEventsParams {
                account_id: "acct",
                calendar_id: None,
                start: "2026-01-01T00:00:00Z",
                end: "2026-12-31T00:00:00Z",
                position: 0,
                limit: 100,
                username: "u",
                password: &pw,
            })
            .await
            .expect("query result");
        assert_eq!(result.events.len(), 1);
        assert_eq!(result.unparseable, 1, "skip count must reach the caller");
    }

    #[tokio::test]
    async fn resolve_calendar_id_for_href_matches_by_path_suffix() {
        // A user-created calendar folder's CollectionId is the stored CalDAV
        // collection href; Stalwart reports the same collection to JMAP with a
        // path-suffixed `href`. Resolution must map the href to the calendar id
        // (scoping the Sync query) and must NOT resolve an unknown href to an
        // arbitrary calendar.
        let response = json!({
            "methodResponses": [
                ["Calendar/get", { "list": [
                    { "id": "cal-default", "name": "Default", "href": "/dav/cal/u%40example.com/default/" },
                    { "id": "cal-projects", "name": "Projects", "href": "/dav/cal/u%40example.com/Projects/" }
                ] }, "cg0"]
            ]
        });
        let base = spawn_stub_calendar_jmap_server(response).await;
        let client = JmapClient::new(&base).unwrap();
        let pw = SecretString::from("pw");
        let hit = client
            .resolve_calendar_id_for_href("u", &pw, "/dav/cal/u%40example.com/Projects/")
            .await
            .expect("query ok");
        assert_eq!(hit.as_deref(), Some("cal-projects"));
        let miss = client
            .resolve_calendar_id_for_href("u", &pw, "/dav/cal/u%40example.com/Ghost/")
            .await
            .expect("query ok");
        assert_eq!(miss, None, "an unknown href must not resolve to a calendar");
    }

    #[tokio::test]
    async fn get_calendar_event_errors_when_conversion_fails() {
        // An event the server returns with no event data at all cannot be
        // converted; the single-event getter must surface that as an error so
        // callers activate their CalDAV fallback, not an empty success.
        let response = json!({
            "methodResponses": [
                ["CalendarEvent/get", { "list": [ { "id": "e1" } ] }, "eg0"]
            ]
        });
        let base = spawn_stub_jmap_api_server(response).await;
        let client = JmapClient::new(&base).unwrap();
        let pw = SecretString::from("pw");
        let result = client.get_calendar_event("acct", "e1", "u", &pw).await;
        assert!(result.is_err(), "unconvertible event must be Err");
    }

    #[tokio::test]
    async fn get_calendar_events_errors_when_conversion_fails() {
        let response = json!({
            "methodResponses": [
                ["CalendarEvent/get", { "list": [ { "id": "e1" } ] }, "eg0"]
            ]
        });
        let base = spawn_stub_jmap_api_server(response).await;
        let client = JmapClient::new(&base).unwrap();
        let pw = SecretString::from("pw");
        let ids = vec!["e1".to_string()];
        let result = client
            .get_calendar_events("acct", &ids, None, "u", &pw)
            .await;
        assert!(result.is_err(), "unconvertible batch event must be Err");
    }

    #[tokio::test]
    async fn get_calendar_event_reconstructs_ics_from_stalwart_object() {
        // End-to-end over the wire: Stalwart's object form must produce ICS.
        let response = json!({
            "methodResponses": [
                ["CalendarEvent/get", { "list": [ {
                    "id": "e1",
                    "uid": "u-1",
                    "title": "Team Sync",
                    "start": "2026-01-05T09:00:00",
                    "timeZone": "Etc/UTC",
                    "duration": "PT1H",
                    "iCalendar": { "convertedProperties": { "duration": { "name": "dtend" } }, "name": "vevent" }
                } ] }, "eg0"]
            ]
        });
        let base = spawn_stub_jmap_api_server(response).await;
        let client = JmapClient::new(&base).unwrap();
        let pw = SecretString::from("pw");
        let (ics, id, _etag) = client
            .get_calendar_event("acct", "e1", "u", &pw)
            .await
            .expect("ICS reconstructed");
        assert_eq!(id, "e1");
        assert!(ics.contains("SUMMARY:Team Sync"), "{ics}");
        assert!(ics.contains("BEGIN:VEVENT"), "{ics}");
    }

    /// Spawn a stub JMAP server that returns `responses[i]` for the i-th
    /// `POST /` call and records every request body, so a test can assert the
    /// exact sequence of `CalendarEvent/set` calls the write path made.
    type RecordingState = (
        std::sync::Arc<std::sync::Mutex<Vec<Value>>>,
        std::sync::Arc<std::sync::Mutex<Vec<Value>>>,
    );

    async fn spawn_recording_stub(
        responses: Vec<Value>,
    ) -> (String, std::sync::Arc<std::sync::Mutex<Vec<Value>>>) {
        use axum::routing::{get, post};
        use std::sync::{Arc, Mutex};

        async fn session(axum::Extension(base): axum::Extension<String>) -> axum::Json<Value> {
            axum::Json(session_json(&base))
        }
        async fn api(
            axum::Extension(state): axum::Extension<RecordingState>,
            axum::Json(body): axum::Json<Value>,
        ) -> axum::Json<Value> {
            let (responses, recorded) = &state;
            let idx = {
                let mut rec = recorded.lock().unwrap();
                let i = rec.len();
                rec.push(body);
                i
            };
            let responses = responses.lock().unwrap();
            axum::Json(
                responses
                    .get(idx)
                    .or_else(|| responses.last())
                    .cloned()
                    .unwrap_or_else(|| json!({ "methodResponses": [] })),
            )
        }

        let responses = Arc::new(Mutex::new(responses));
        let recorded = Arc::new(Mutex::new(Vec::new()));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let base = format!("http://127.0.0.1:{port}");
        let app = axum::Router::new()
            .route("/session", get(session))
            .route("/", post(api))
            .layer(axum::Extension((responses, recorded.clone())))
            .layer(axum::Extension(base.clone()));
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        (base, recorded)
    }

    fn ics_with_description() -> String {
        "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//x//EN\r\nBEGIN:VEVENT\r\nUID:u1\r\n\
         DTSTAMP:20260301T000000Z\r\nDTSTART:20260401T090000Z\r\nDTEND:20260401T100000Z\r\n\
         SUMMARY:Kept\r\nDESCRIPTION:body\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n"
            .to_string()
    }

    /// A read-back whose title is empty proves the backend discarded the
    /// payload: the error must be `PayloadDiscarded` and the orphaned create
    /// must be destroyed (a second `CalendarEvent/set` carrying `destroy`).
    #[tokio::test]
    async fn set_calendar_event_create_discarded_payload_disables_and_destroys() {
        let set_and_get = json!({ "methodResponses": [
            ["CalendarEvent/set", { "created": { "e0": { "id": "e1" } } }, "es0"],
            ["CalendarEvent/get", { "list": [ { "id": "e1" } ] }, "eg0"]
        ]});
        let destroy = json!({ "methodResponses": [
            ["CalendarEvent/set", { "destroyed": ["e1"] }, "es1"]
        ]});
        let (base, recorded) = spawn_recording_stub(vec![set_and_get, destroy]).await;
        let client = JmapClient::new(&base).unwrap();
        let pw = SecretString::from("pw");
        let err = client
            .set_calendar_event(SetCalendarEventParams {
                account_id: "acct",
                calendar_id: Some("cal"),
                event_id: None,
                ics: &ics_with_description(),
                username: "u",
                password: &pw,
            })
            .await
            .unwrap_err();
        assert!(err.is_payload_discarded(), "{err:?}");
        let rec = recorded.lock().unwrap();
        assert_eq!(rec.len(), 2, "discarded create must issue a cleanup call");
        assert!(
            rec[1].to_string().contains("destroy"),
            "cleanup must destroy the orphan: {}",
            rec[1]
        );
    }

    /// A read-back that keeps the title but drops the description is a content
    /// MISMATCH, not a discarded payload: the error must NOT be
    /// `PayloadDiscarded` (so JMAP writes stay enabled), but the orphaned
    /// create must still be destroyed so the CalDAV fallback cannot duplicate
    /// it.
    #[tokio::test]
    async fn set_calendar_event_create_mismatch_is_not_payload_discarded() {
        let set_and_get = json!({ "methodResponses": [
            ["CalendarEvent/set", { "created": { "e0": { "id": "e1" } } }, "es0"],
            ["CalendarEvent/get", { "list": [ {
                "id": "e1",
                "uid": "u1",
                "title": "Kept",
                "start": "2026-04-01T09:00:00",
                "timeZone": "Etc/UTC",
                "duration": "PT1H"
            } ] }, "eg0"]
        ]});
        let destroy = json!({ "methodResponses": [
            ["CalendarEvent/set", { "destroyed": ["e1"] }, "es1"]
        ]});
        let (base, recorded) = spawn_recording_stub(vec![set_and_get, destroy]).await;
        let client = JmapClient::new(&base).unwrap();
        let pw = SecretString::from("pw");
        let err = client
            .set_calendar_event(SetCalendarEventParams {
                account_id: "acct",
                calendar_id: Some("cal"),
                event_id: None,
                ics: &ics_with_description(),
                username: "u",
                password: &pw,
            })
            .await
            .unwrap_err();
        assert!(
            !err.is_payload_discarded(),
            "a content mismatch must not disable the JMAP write path: {err:?}"
        );
        let rec = recorded.lock().unwrap();
        assert_eq!(
            rec.len(),
            2,
            "a mismatch must still destroy the orphan to avoid a duplicate"
        );
        assert!(rec[1].to_string().contains("destroy"), "{}", rec[1]);
    }

    /// The same distinction on the update path: an empty read-back is
    /// `PayloadDiscarded`, a partial read-back is not.
    #[tokio::test]
    async fn set_calendar_event_update_classifies_discard_vs_mismatch() {
        let discarded = json!({ "methodResponses": [
            ["CalendarEvent/set", { "updated": { "e1": {} } }, "es0"],
            ["CalendarEvent/get", { "list": [ { "id": "e1" } ] }, "eg0"]
        ]});
        let (base, _rec) = spawn_recording_stub(vec![discarded]).await;
        let client = JmapClient::new(&base).unwrap();
        let pw = SecretString::from("pw");
        let err = client
            .set_calendar_event(SetCalendarEventParams {
                account_id: "acct",
                calendar_id: None,
                event_id: Some("e1"),
                ics: &ics_with_description(),
                username: "u",
                password: &pw,
            })
            .await
            .unwrap_err();
        assert!(err.is_payload_discarded(), "{err:?}");

        let partial = json!({ "methodResponses": [
            ["CalendarEvent/set", { "updated": { "e1": {} } }, "es0"],
            ["CalendarEvent/get", { "list": [ {
                "id": "e1", "uid": "u1", "title": "Kept",
                "start": "2026-04-01T09:00:00", "timeZone": "Etc/UTC", "duration": "PT1H"
            } ] }, "eg0"]
        ]});
        let (base, _rec) = spawn_recording_stub(vec![partial]).await;
        let client = JmapClient::new(&base).unwrap();
        let err = client
            .set_calendar_event(SetCalendarEventParams {
                account_id: "acct",
                calendar_id: None,
                event_id: Some("e1"),
                ics: &ics_with_description(),
                username: "u",
                password: &pw,
            })
            .await
            .unwrap_err();
        assert!(!err.is_payload_discarded(), "{err:?}");
    }

    /// A JMAP method-level `error` response to CalendarEvent/set (RFC 8620
    /// §3.3) must be rejected, not treated as a successful destroy — the
    /// caller (EWS DeleteItem) relies on the Err to fall back to CalDAV.
    #[tokio::test]
    async fn destroy_calendar_events_rejects_method_error() {
        let resp = json!({ "methodResponses": [
            ["error", { "type": "forbidden", "description": "denied" }, "es0"]
        ]});
        let (base, _rec) = spawn_recording_stub(vec![resp]).await;
        let client = JmapClient::new(&base).unwrap();
        let pw = SecretString::from("pw");
        let err = client
            .destroy_calendar_events("acct", &["e1".to_string()], "u", &pw)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("method error"), "{err:?}");
    }

    /// A `CalendarEvent/set` response whose `destroyed` array omits the
    /// requested id (silently did nothing) must also fail — only a fully
    /// confirmed destroy is success.
    #[tokio::test]
    async fn destroy_calendar_events_rejects_missing_destroyed() {
        let empty = json!({ "methodResponses": [
            ["CalendarEvent/set", { "destroyed": [] }, "es0"]
        ]});
        let (base, _rec) = spawn_recording_stub(vec![empty]).await;
        let client = JmapClient::new(&base).unwrap();
        let pw = SecretString::from("pw");
        let err = client
            .destroy_calendar_events("acct", &["e1".to_string()], "u", &pw)
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("absent from `destroyed`"),
            "{err:?}"
        );
    }

    /// A response with no `CalendarEvent/set` method answer at all is an
    /// error, not success.
    #[tokio::test]
    async fn destroy_calendar_events_rejects_no_method_response() {
        let resp = json!({ "methodResponses": [
            ["CalendarEvent/query", { "ids": [] }, "q0"]
        ]});
        let (base, _rec) = spawn_recording_stub(vec![resp]).await;
        let client = JmapClient::new(&base).unwrap();
        let pw = SecretString::from("pw");
        let err = client
            .destroy_calendar_events("acct", &["e1".to_string()], "u", &pw)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("no method response"), "{err:?}");
    }

    /// A `CalendarEvent/set` response that confirms every requested id in
    /// `destroyed` is the one success shape.
    #[tokio::test]
    async fn destroy_calendar_events_succeeds_only_when_confirmed() {
        let resp = json!({ "methodResponses": [
            ["CalendarEvent/set", { "destroyed": ["e1", "e2"] }, "es0"]
        ]});
        let (base, _rec) = spawn_recording_stub(vec![resp]).await;
        let client = JmapClient::new(&base).unwrap();
        let pw = SecretString::from("pw");
        client
            .destroy_calendar_events("acct", &["e1".to_string(), "e2".to_string()], "u", &pw)
            .await
            .unwrap();
    }

    #[test]
    fn chunk_ids_splits_into_conforming_batches() {
        let ids: Vec<String> = (0..5).map(|i| format!("id{i}")).collect();
        let chunks = chunk_ids(&ids, 2);
        assert_eq!(chunks.len(), 3);
        assert_eq!(chunks[0], &ids[0..2]);
        assert_eq!(chunks[1], &ids[2..4]);
        assert_eq!(chunks[2], &ids[4..5]);
    }

    #[test]
    fn chunk_ids_exact_multiple_has_no_empty_tail() {
        let ids: Vec<String> = (0..4).map(|i| format!("id{i}")).collect();
        let chunks = chunk_ids(&ids, 2);
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0].len(), 2);
        assert_eq!(chunks[1].len(), 2);
    }

    #[test]
    fn chunk_ids_cap_at_or_above_len_is_one_chunk() {
        let ids: Vec<String> = (0..3).map(|i| format!("id{i}")).collect();
        assert_eq!(chunk_ids(&ids, 3).len(), 1);
        assert_eq!(chunk_ids(&ids, 500).len(), 1);
        // An empty id list yields no chunk at all (callers short-circuit, but
        // the helper must not fabricate a request for zero ids).
        assert!(chunk_ids(&[], 500).is_empty());
    }

    #[test]
    fn chunk_ids_zero_cap_is_clamped_to_one() {
        // A server advertising `0` must not make the helper spin: clamp to 1.
        let ids: Vec<String> = (0..3).map(|i| format!("id{i}")).collect();
        let chunks = chunk_ids(&ids, 0);
        assert_eq!(chunks.len(), 3);
        assert!(chunks.iter().all(|c| c.len() == 1));
    }

    #[test]
    fn session_max_objects_in_get_reads_advertised_cap() {
        let mut session: JmapSession =
            serde_json::from_value(session_json("http://x")).expect("session");
        assert_eq!(
            session_max_objects_in_get(&session),
            DEFAULT_MAX_OBJECTS_IN_GET,
            "absent core capability falls back to the default"
        );
        session.capabilities.insert(
            JMAP_CORE_CAPABILITY.to_string(),
            json!({ "maxObjectsInGet": 250 }),
        );
        assert_eq!(session_max_objects_in_get(&session), 250);
        session.capabilities.insert(
            JMAP_CORE_CAPABILITY.to_string(),
            json!({ "maxObjectsInGet": 0 }),
        );
        assert_eq!(
            session_max_objects_in_get(&session),
            1,
            "an advertised zero is treated as one"
        );
        session.capabilities.insert(
            JMAP_CORE_CAPABILITY.to_string(),
            json!({ "maxSizeRequest": 1000 }),
        );
        assert_eq!(
            session_max_objects_in_get(&session),
            DEFAULT_MAX_OBJECTS_IN_GET,
            "a core capability without the value falls back to the default"
        );
    }

    /// State shared with the capped stub server: the session base URL, the
    /// advertised `maxObjectsInGet`, the recorded request bodies, and any ids
    /// whose containing chunk must be answered with a method-level error.
    #[derive(Clone)]
    struct CappedStubState {
        base: String,
        max: usize,
        recorded: std::sync::Arc<std::sync::Mutex<Vec<Value>>>,
        error_ids: Vec<String>,
    }

    /// Stub JMAP server that enforces `maxObjectsInGet` on the explicit-`ids`
    /// `.../get` methods exactly as Stalwart does: an over-cap id array is
    /// answered with the RFC 8620 §3.6.2 `requestTooLarge` error and no data.
    /// A client that does not chunk therefore cannot retrieve the objects.
    /// Records every request body so the test can assert batch sizes.
    async fn spawn_capped_get_stub(
        max_objects_in_get: usize,
    ) -> (String, std::sync::Arc<std::sync::Mutex<Vec<Value>>>) {
        spawn_capped_get_stub_with_errors(max_objects_in_get, Vec::new()).await
    }

    /// As [`spawn_capped_get_stub`], but any request whose `ids` array contains
    /// one of `error_ids` is answered with a method-level `serverFail` error
    /// (RFC 8620 §3.6.2) instead of a `list`. Used to prove that a *later*
    /// chunk failing after earlier chunks succeeded surfaces as `Err`.
    async fn spawn_capped_get_stub_with_errors(
        max_objects_in_get: usize,
        error_ids: Vec<String>,
    ) -> (String, std::sync::Arc<std::sync::Mutex<Vec<Value>>>) {
        use axum::routing::{get, post};
        use std::sync::{Arc, Mutex};

        async fn session(
            axum::Extension(state): axum::Extension<CappedStubState>,
        ) -> axum::Json<Value> {
            let mut s = session_json(&state.base);
            s["capabilities"] = json!({
                JMAP_CORE_CAPABILITY: { "maxObjectsInGet": state.max },
            });
            axum::Json(s)
        }
        async fn api(
            axum::Extension(state): axum::Extension<CappedStubState>,
            axum::Json(body): axum::Json<Value>,
        ) -> axum::Json<Value> {
            let max = state.max;
            state.recorded.lock().unwrap().push(body.clone());
            let calls = body
                .get("methodCalls")
                .and_then(|c| c.as_array())
                .cloned()
                .unwrap_or_default();
            let mut responses = Vec::new();
            for call in calls {
                let name = call.get(0).and_then(|v| v.as_str()).unwrap_or("");
                let args = call.get(1).cloned().unwrap_or(Value::Null);
                let id = call.get(2).and_then(|v| v.as_str()).unwrap_or("x");
                let ids: Vec<&str> = args
                    .get("ids")
                    .and_then(|v| v.as_array())
                    .map(|a| a.iter().filter_map(|v| v.as_str()).collect())
                    .unwrap_or_default();
                if ids.iter().any(|i| state.error_ids.iter().any(|e| e == i)) {
                    responses.push(json!([
                        "error",
                        { "type": "serverFail", "description": "injected failure" },
                        id
                    ]));
                    continue;
                }
                if ids.len() > max {
                    responses.push(json!([
                        "error",
                        { "type": "requestTooLarge", "description": "too many ids" },
                        id
                    ]));
                    continue;
                }
                let list: Vec<Value> = ids
                    .iter()
                    .map(|id| match name {
                        "CalendarEvent/get" => json!({
                            "id": id,
                            "uid": format!("uid-{id}"),
                            "title": "Event",
                            "start": "2026-01-05T09:00:00",
                            "timeZone": "Etc/UTC",
                            "duration": "PT1H",
                            "iCalendar": { "name": "vevent" }
                        }),
                        _ => json!({ "id": id, "mailboxIds": { "inbox": true } }),
                    })
                    .collect();
                if name == "Email/set" {
                    let updated: serde_json::Map<String, Value> = args
                        .get("update")
                        .and_then(|v| v.as_object())
                        .map(|o| o.keys().map(|k| (k.clone(), Value::Null)).collect())
                        .unwrap_or_default();
                    responses.push(json!([name, { "accountId": "acct", "updated": updated }, id]));
                } else {
                    responses.push(json!([
                        name,
                        { "accountId": "acct", "state": "s", "list": list, "notFound": [] },
                        id
                    ]));
                }
            }
            axum::Json(json!({ "methodResponses": responses }))
        }

        let recorded = Arc::new(Mutex::new(Vec::new()));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let base = format!("http://127.0.0.1:{port}");
        let app = axum::Router::new()
            .route("/session", get(session))
            .route("/", post(api))
            .layer(axum::Extension(CappedStubState {
                base: base.clone(),
                max: max_objects_in_get,
                recorded: recorded.clone(),
                error_ids,
            }))
            .layer(axum::Extension(base.clone()));
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        (base, recorded)
    }

    /// Every `ids` array in a recorded request for `method` must respect the
    /// cap, and the number of such requests must equal `expected_requests`.
    fn assert_ids_batches(
        recorded: &[Value],
        method: &str,
        cap: usize,
        expected_requests: usize,
    ) -> usize {
        let mut seen = 0;
        for body in recorded {
            let calls = body
                .get("methodCalls")
                .and_then(|c| c.as_array())
                .cloned()
                .unwrap_or_default();
            for call in calls {
                if call.get(0).and_then(|v| v.as_str()) != Some(method) {
                    continue;
                }
                let n = call
                    .get(1)
                    .and_then(|a| a.get("ids"))
                    .and_then(|v| v.as_array())
                    .map(|a| a.len())
                    .unwrap_or(0);
                assert!(
                    n <= cap,
                    "request for {method} sent {n} ids, over the {cap} cap"
                );
                seen += 1;
            }
        }
        assert_eq!(
            seen, expected_requests,
            "{method} must be issued once per conforming batch"
        );
        seen
    }

    #[tokio::test]
    async fn get_emails_chunks_over_max_objects_in_get() {
        // Cap of 2 with 5 ids: an unchunked request would be rejected whole.
        let (base, recorded) = spawn_capped_get_stub(2).await;
        let client = JmapClient::new(&base).unwrap();
        let pw = SecretString::from("pw");
        let ids: Vec<String> = (0..5).map(|i| format!("m{i}")).collect();
        let emails = client
            .get_emails("acct", &ids, None, "u", &pw, false)
            .await
            .expect("chunked get must succeed");
        assert_eq!(emails.len(), 5, "every id must be delivered");
        let rec = recorded.lock().unwrap().clone();
        assert_ids_batches(&rec, "Email/get", 2, 3);
    }

    #[tokio::test]
    async fn get_emails_single_request_when_under_cap() {
        let (base, recorded) = spawn_capped_get_stub(500).await;
        let client = JmapClient::new(&base).unwrap();
        let pw = SecretString::from("pw");
        let ids: Vec<String> = (0..5).map(|i| format!("m{i}")).collect();
        let emails = client
            .get_emails("acct", &ids, None, "u", &pw, false)
            .await
            .expect("under-cap get must succeed");
        assert_eq!(emails.len(), 5);
        let rec = recorded.lock().unwrap().clone();
        assert_ids_batches(&rec, "Email/get", 500, 1);
    }

    #[tokio::test]
    async fn get_emails_empty_ids_makes_no_request() {
        let (base, recorded) = spawn_capped_get_stub(2).await;
        let client = JmapClient::new(&base).unwrap();
        let pw = SecretString::from("pw");
        let emails = client
            .get_emails("acct", &[], None, "u", &pw, false)
            .await
            .expect("empty id list is a no-op");
        assert!(emails.is_empty());
        assert!(
            recorded.lock().unwrap().is_empty(),
            "an empty id list must not issue a JMAP request"
        );
    }

    #[tokio::test]
    async fn get_calendar_events_chunks_over_max_objects_in_get() {
        let (base, recorded) = spawn_capped_get_stub(2).await;
        let client = JmapClient::new(&base).unwrap();
        let pw = SecretString::from("pw");
        let ids: Vec<String> = (0..5).map(|i| format!("e{i}")).collect();
        let map = client
            .get_calendar_events("acct", &ids, None, "u", &pw)
            .await
            .expect("chunked calendar get must succeed");
        assert_eq!(map.len(), 5, "every event must be delivered");
        for id in &ids {
            assert!(map.contains_key(id), "missing event {id}");
        }
        let rec = recorded.lock().unwrap().clone();
        assert_ids_batches(&rec, "CalendarEvent/get", 2, 3);
    }

    #[tokio::test]
    async fn get_calendar_events_errors_on_method_error_chunk() {
        // A chunk answered with a method-level error must fail the whole call,
        // not silently drop those events into an incomplete `Ok` map (which
        // would make the EWS/sync callers skip events instead of falling back
        // to CalDAV). Cap 2 with 5 ids → 3 chunks; the 3rd chunk carries `e4`.
        let (base, _recorded) = spawn_capped_get_stub_with_errors(2, vec!["e4".to_string()]).await;
        let client = JmapClient::new(&base).unwrap();
        let pw = SecretString::from("pw");
        let ids: Vec<String> = (0..5).map(|i| format!("e{i}")).collect();
        let result = client
            .get_calendar_events("acct", &ids, None, "u", &pw)
            .await;
        assert!(
            result.is_err(),
            "a method-level error on any chunk must surface as Err"
        );
    }

    #[tokio::test]
    async fn get_emails_errors_on_method_error_chunk() {
        // The push monitor relies on this: a later failing chunk must be an
        // error (so it does not advance the change state), never an empty Ok.
        let (base, _recorded) = spawn_capped_get_stub_with_errors(2, vec!["m4".to_string()]).await;
        let client = JmapClient::new(&base).unwrap();
        let pw = SecretString::from("pw");
        let ids: Vec<String> = (0..5).map(|i| format!("m{i}")).collect();
        let result = client.get_emails("acct", &ids, None, "u", &pw, false).await;
        assert!(
            result.is_err(),
            "a method-level error on any chunk must surface as Err"
        );
    }

    #[tokio::test]
    async fn move_emails_chunks_over_max_objects_in_get() {
        // move_emails reads current mailboxIds with an explicit id list, so it
        // is subject to the same cap as any other Email/get.
        let (base, recorded) = spawn_capped_get_stub(2).await;
        let client = JmapClient::new(&base).unwrap();
        let pw = SecretString::from("pw");
        let ids: Vec<String> = (0..5).map(|i| format!("m{i}")).collect();
        let moved = client
            .move_emails("acct", &ids, "archive", "u", &pw)
            .await
            .expect("chunked move must succeed");
        assert_eq!(moved, 5, "every email must be moved");
        let rec = recorded.lock().unwrap().clone();
        assert_ids_batches(&rec, "Email/get", 2, 3);
    }

    /// `calendar_event_etag` is content-addressed: identical content yields the
    /// same token regardless of key order, a content change yields a new one,
    /// and a server-provided `@etag` wins over the hash (finding #6).
    #[test]
    fn calendar_event_etag_is_stable_and_content_addressed() {
        let a = json!({ "title": "X", "start": "2026-01-05T09:00:00", "duration": "PT1H" });
        // Same content, different key insertion order: serde_json::Value::Object
        // is BTreeMap-backed, so the canonical form (and thus the hash) matches.
        let b = json!({ "duration": "PT1H", "start": "2026-01-05T09:00:00", "title": "X" });
        let ea = calendar_event_etag(&a, None);
        assert_eq!(ea, calendar_event_etag(&b, None), "order-insensitive");
        assert_eq!(ea.len(), 32, "first 16 bytes as hex");

        let changed = json!({ "title": "Y", "start": "2026-01-05T09:00:00", "duration": "PT1H" });
        assert_ne!(ea, calendar_event_etag(&changed, None), "content change");

        // A server-provided etag wins, with surrounding quotes trimmed.
        assert_eq!(calendar_event_etag(&a, Some("\"abc\"")), "abc");
        // An empty/whitespace server etag falls back to the hash.
        assert_eq!(calendar_event_etag(&a, Some("  ")), ea);
    }

    /// Draining `CalendarEvent/changes` follows `hasMoreChanges` until the
    /// final page and returns the FINAL `newState` (finding #5). Storing an
    /// intermediate state would make the next delta skip the undrained pages.
    #[tokio::test]
    async fn accumulate_calendar_changes_drains_has_more_changes() {
        let page1 = json!({ "methodResponses": [
            ["CalendarEvent/changes", {
                "accountId": "acct", "oldState": "s0", "newState": "s1",
                "hasMoreChanges": true,
                "created": ["a", "b"], "updated": [], "destroyed": []
            }, "cc0"]
        ]});
        let page2 = json!({ "methodResponses": [
            ["CalendarEvent/changes", {
                "accountId": "acct", "oldState": "s1", "newState": "s2",
                "hasMoreChanges": false,
                "created": ["c"], "updated": ["d"], "destroyed": ["e"]
            }, "cc0"]
        ]});
        let (base, recorded) = spawn_recording_stub(vec![page1, page2]).await;
        let client = JmapClient::new(&base).unwrap();
        let pw = SecretString::from("pw");
        let changes = client
            .accumulate_calendar_changes("acct", "s0", "u", &pw)
            .await
            .expect("drain must succeed");
        assert_eq!(changes.created, vec!["a", "b", "c"]);
        assert_eq!(changes.updated, vec!["d"]);
        assert_eq!(changes.destroyed, vec!["e"]);
        assert_eq!(
            changes.new_state, "s2",
            "final state, never an intermediate"
        );
        assert!(!changes.has_more_changes);
        let rec = recorded.lock().unwrap().clone();
        assert_eq!(rec.len(), 2, "both pages drained");
        assert!(
            rec[0].to_string().contains("\"maxChanges\":500"),
            "pages are bounded by maxChanges: {}",
            rec[0]
        );
    }

    /// An id created on one page and destroyed on a later one nets to a
    /// deletion, and duplicate ids collapse to a single occurrence.
    #[tokio::test]
    async fn accumulate_calendar_changes_destroyed_wins_and_dedupes() {
        let page1 = json!({ "methodResponses": [
            ["CalendarEvent/changes", {
                "accountId": "acct", "oldState": "s0", "newState": "s1",
                "hasMoreChanges": true,
                "created": ["a", "a", "b"], "updated": [], "destroyed": []
            }, "cc0"]
        ]});
        let page2 = json!({ "methodResponses": [
            ["CalendarEvent/changes", {
                "accountId": "acct", "oldState": "s1", "newState": "s2",
                "hasMoreChanges": false,
                "created": [], "updated": ["b"], "destroyed": ["b"]
            }, "cc0"]
        ]});
        let (base, _rec) = spawn_recording_stub(vec![page1, page2]).await;
        let client = JmapClient::new(&base).unwrap();
        let pw = SecretString::from("pw");
        let changes = client
            .accumulate_calendar_changes("acct", "s0", "u", &pw)
            .await
            .expect("drain must succeed");
        // `a` was created twice → one occurrence. `b` was created then updated
        // then destroyed → it is a deletion only.
        assert_eq!(changes.created, vec!["a"]);
        assert!(changes.updated.is_empty(), "b is a deletion, not an update");
        assert_eq!(changes.destroyed, vec!["b"]);
        assert_eq!(changes.new_state, "s2");
    }

    /// A `cannotCalculateChanges` method error surfaces as a typed error so the
    /// sync path can re-prime from a full query instead of falling back to
    /// CalDAV (finding #7).
    #[tokio::test]
    async fn accumulate_calendar_changes_surfaces_cannot_calculate_changes() {
        let err_page = json!({ "methodResponses": [
            ["error", { "type": "cannotCalculateChanges", "description": "too old" }, "cc0"]
        ]});
        let (base, _rec) = spawn_recording_stub(vec![err_page]).await;
        let client = JmapClient::new(&base).unwrap();
        let pw = SecretString::from("pw");
        let err = client
            .accumulate_calendar_changes("acct", "stale", "u", &pw)
            .await
            .expect_err("cannotCalculateChanges must surface as Err");
        assert!(
            err.downcast_ref::<CannotCalculateChanges>().is_some(),
            "must be typed CannotCalculateChanges: {err:?}"
        );
    }

    /// A `CalendarEvent/changes` response with no `newState` is unusable: the
    /// token the caller would store is empty, so it is treated as
    /// `cannotCalculateChanges` to force a re-prime rather than persisting a
    /// token that skips changes (finding #7).
    #[tokio::test]
    async fn accumulate_calendar_changes_missing_new_state_is_cannot_calculate() {
        let no_state = json!({ "methodResponses": [
            ["CalendarEvent/changes", {
                "accountId": "acct", "oldState": "s0",
                "created": ["a"], "updated": [], "destroyed": []
            }, "cc0"]
        ]});
        let (base, _rec) = spawn_recording_stub(vec![no_state]).await;
        let client = JmapClient::new(&base).unwrap();
        let pw = SecretString::from("pw");
        let err = client
            .accumulate_calendar_changes("acct", "s0", "u", &pw)
            .await
            .expect_err("missing newState must surface as Err");
        assert!(
            err.downcast_ref::<CannotCalculateChanges>().is_some(),
            "must be typed CannotCalculateChanges: {err:?}"
        );
    }

    /// A Stalwart `Email/queryChanges` method error is a SEPARATE `error`
    /// response (RFC 8620 §3.6.2), not a field on the method's own response.
    /// An unresumable `sinceQueryState` comes back as `invalidArguments`; it
    /// must downcast to `CannotCalculateChanges` so the EAS/EWS delta re-primes
    /// from a full snapshot instead of freezing the feed.
    #[tokio::test]
    async fn query_email_changes_invalid_arguments_surfaces_cannot_calculate() {
        let err_page = json!({ "methodResponses": [
            ["error", { "type": "invalidArguments", "description": "invalid JMAP State" }, "qc0"]
        ]});
        let (base, _rec) = spawn_recording_stub(vec![err_page]).await;
        let client = JmapClient::new(&base).unwrap();
        let pw = SecretString::from("pw");
        let err = client
            .query_email_changes("acct", "mbx", "stale", "u", &pw, None)
            .await
            .expect_err("invalidArguments must surface as Err");
        assert!(
            err.downcast_ref::<CannotCalculateChanges>().is_some(),
            "must be typed CannotCalculateChanges: {err:?}"
        );
    }

    /// A `cannotCalculateChanges` method error maps the same way.
    #[tokio::test]
    async fn query_email_changes_cannot_calculate_surfaces_typed_error() {
        let err_page = json!({ "methodResponses": [
            ["error", { "type": "cannotCalculateChanges", "description": "too old" }, "qc0"]
        ]});
        let (base, _rec) = spawn_recording_stub(vec![err_page]).await;
        let client = JmapClient::new(&base).unwrap();
        let pw = SecretString::from("pw");
        let err = client
            .query_email_changes("acct", "mbx", "stale", "u", &pw, None)
            .await
            .expect_err("cannotCalculateChanges must surface as Err");
        assert!(
            err.downcast_ref::<CannotCalculateChanges>().is_some(),
            "must be typed CannotCalculateChanges: {err:?}"
        );
    }

    /// A successful mailbox-scoped page: `added` → created, `removed` →
    /// destroyed, and the wire call carries the `inMailbox` filter and the
    /// caller's `maxChanges`.
    #[tokio::test]
    async fn query_email_changes_scopes_to_mailbox_and_classifies() {
        let page = json!({ "methodResponses": [
            ["Email/queryChanges", {
                "accountId": "acct", "oldQueryState": "s0", "newQueryState": "s1",
                "added": [{"id": "m1", "index": 0}], "removed": ["m2"]
            }, "qc0"]
        ]});
        let (base, rec) = spawn_recording_stub(vec![page]).await;
        let client = JmapClient::new(&base).unwrap();
        let pw = SecretString::from("pw");
        let result = client
            .query_email_changes("acct", "mbx-inbox", "s0", "u", &pw, Some(25))
            .await
            .expect("queryChanges must succeed");
        assert_eq!(result.created, vec!["m1"]);
        assert_eq!(result.destroyed, vec!["m2"]);
        assert_eq!(result.new_state, "s1");
        let sent = rec.lock().unwrap();
        let args = &sent[0]["methodCalls"][0][1];
        assert_eq!(args["filter"]["inMailbox"], "mbx-inbox");
        assert_eq!(args["sinceQueryState"], "s0");
        assert_eq!(args["maxChanges"], 25);
    }

    /// The initial prime captures the `CalendarEvent/get` data-type `state` —
    /// the token `CalendarEvent/changes` accepts — rather than the query's
    /// `queryState` (finding #7).
    #[tokio::test]
    async fn query_calendar_events_captures_get_state() {
        let response = json!({
            "methodResponses": [
                ["CalendarEvent/query", { "ids": ["e1"], "total": 1, "queryState": "query-7" }, "eq0"],
                ["CalendarEvent/get", { "state": "get-9", "list": [
                    { "id": "e1", "uid": "u1", "title": "Good", "start": "2026-01-05T09:00:00",
                      "timeZone": "Etc/UTC", "duration": "PT1H", "iCalendar": {"name":"vevent"} }
                ] }, "eg0"]
            ]
        });
        let base = spawn_stub_jmap_api_server(response).await;
        let client = JmapClient::new(&base).unwrap();
        let pw = SecretString::from("pw");
        let result = client
            .query_calendar_events(QueryCalendarEventsParams {
                account_id: "acct",
                calendar_id: None,
                start: "2026-01-01T00:00:00Z",
                end: "2027-01-01T00:00:00Z",
                position: 0,
                limit: 100,
                username: "u",
                password: &pw,
            })
            .await
            .expect("query must succeed");
        assert_eq!(result.state, "get-9", "the data-type state, not queryState");
        assert_eq!(result.query_state, "query-7");
    }

    /// Spawn a stub JMAP server whose `Mailbox/set` `destroy` reply is the
    /// supplied scripted `methodResponses` entry, so the `destroy_mailbox`
    /// outcome mapping can be driven directly.
    async fn spawn_mailbox_set_stub(api_response: Value) -> String {
        spawn_stub_jmap_api_server(api_response).await
    }

    #[tokio::test]
    async fn destroy_mailbox_reports_destroyed() {
        let response = json!({
            "methodResponses": [
                ["Mailbox/set", { "accountId": "a", "destroyed": ["mb-1"] }, "ms2"]
            ]
        });
        let base = spawn_mailbox_set_stub(response).await;
        let client = JmapClient::new(&base).unwrap();
        let pw = SecretString::from("pw");
        let outcome = client
            .destroy_mailbox("a", "mb-1", true, "u", &pw)
            .await
            .unwrap();
        assert_eq!(outcome, MailboxDestroyOutcome::Destroyed);
    }

    #[tokio::test]
    async fn destroy_mailbox_maps_notfound_to_already_gone() {
        // RFC 8620 §5.3: a destroy of an id that cannot be found is rejected
        // with the `notFound` SetError. That is the desired end state, so the
        // outcome is `AlreadyGone`, not an error and not a refusal.
        let response = json!({
            "methodResponses": [
                ["Mailbox/set", {
                    "accountId": "a",
                    "notDestroyed": { "mb-1": { "type": "notFound" } }
                }, "ms2"]
            ]
        });
        let base = spawn_mailbox_set_stub(response).await;
        let client = JmapClient::new(&base).unwrap();
        let pw = SecretString::from("pw");
        let outcome = client
            .destroy_mailbox("a", "mb-1", true, "u", &pw)
            .await
            .unwrap();
        assert_eq!(outcome, MailboxDestroyOutcome::AlreadyGone);
    }

    #[tokio::test]
    async fn destroy_mailbox_maps_mailbox_has_email_to_refused() {
        // RFC 8621 §2.5: a non-empty mailbox without `onDestroyRemoveEmails`
        // is rejected with `mailboxHasEmail`. That is a refusal — the mailbox
        // still exists — and must not be mistaken for a completed delete.
        let response = json!({
            "methodResponses": [
                ["Mailbox/set", {
                    "accountId": "a",
                    "notDestroyed": { "mb-1": { "type": "mailboxHasEmail" } }
                }, "ms2"]
            ]
        });
        let base = spawn_mailbox_set_stub(response).await;
        let client = JmapClient::new(&base).unwrap();
        let pw = SecretString::from("pw");
        let outcome = client
            .destroy_mailbox("a", "mb-1", false, "u", &pw)
            .await
            .unwrap();
        assert_eq!(outcome, MailboxDestroyOutcome::Refused);
    }

    #[tokio::test]
    async fn destroy_mailbox_errors_on_method_error() {
        // RFC 8620 §3.6.2: a method-level error is a separate `error` response.
        // It must be a hard error, never `Ok(false)`.
        let response = json!({
            "methodResponses": [
                ["error", { "type": "serverFail", "description": "boom" }, "ms2"]
            ]
        });
        let base = spawn_mailbox_set_stub(response).await;
        let client = JmapClient::new(&base).unwrap();
        let pw = SecretString::from("pw");
        let err = client
            .destroy_mailbox("a", "mb-1", true, "u", &pw)
            .await
            .expect_err("a method-level error must be a hard error");
        assert!(err.to_string().contains("method error"), "{err}");
    }

    #[tokio::test]
    async fn destroy_mailbox_errors_when_destroyed_missing() {
        // A `Mailbox/set` result without a `destroyed` array is malformed and
        // must be a hard error rather than silently reported as not-destroyed.
        let response = json!({
            "methodResponses": [
                ["Mailbox/set", { "accountId": "a" }, "ms2"]
            ]
        });
        let base = spawn_mailbox_set_stub(response).await;
        let client = JmapClient::new(&base).unwrap();
        let pw = SecretString::from("pw");
        let err = client
            .destroy_mailbox("a", "mb-1", true, "u", &pw)
            .await
            .expect_err("a missing `destroyed` must be a hard error");
        assert!(err.to_string().contains("missing `destroyed`"), "{err}");
    }

    /// Spawn a stub JMAP server whose `Mailbox/query` honours `position`/`limit`
    /// over a fixed id list (clamped to `page_cap`) so `query_all_mailboxes`
    /// pagination can be exercised, and whose `Mailbox/get` echoes the queried
    /// ids with a `parentId` derived from the id suffix.
    async fn spawn_paged_mailbox_stub(
        total: usize,
        page_cap: usize,
    ) -> (String, std::sync::Arc<tokio::sync::Mutex<Vec<u64>>>) {
        use axum::routing::{get, post};

        #[derive(Clone)]
        struct PagedCtl {
            base: String,
            total: usize,
            page_cap: usize,
            queries: std::sync::Arc<tokio::sync::Mutex<Vec<u64>>>,
        }

        async fn session(axum::Extension(ctl): axum::Extension<PagedCtl>) -> axum::Json<Value> {
            let mut s = session_json(&ctl.base);
            s["primaryAccounts"] = json!({ "urn:ietf:params:jmap:mail": "a" });
            axum::Json(s)
        }

        async fn api(
            axum::Extension(ctl): axum::Extension<PagedCtl>,
            body: String,
        ) -> axum::Json<Value> {
            let req: Value = serde_json::from_str(&body).unwrap_or(Value::Null);
            let mut responses = Vec::new();
            let mut last_query_ids: Vec<String> = Vec::new();
            for call in req
                .get("methodCalls")
                .and_then(|v| v.as_array())
                .cloned()
                .unwrap_or_default()
            {
                let name = call.get(0).and_then(|v| v.as_str()).unwrap_or("");
                let args = call.get(1).cloned().unwrap_or(Value::Null);
                let cid = call.get(2).and_then(|v| v.as_str()).unwrap_or("c0");
                let result = match name {
                    "Mailbox/query" => {
                        let position =
                            args.get("position").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
                        let limit =
                            args.get("limit").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
                        ctl.queries.lock().await.push(position as u64);
                        // The server clamps the requested limit to its own cap.
                        let eff = limit.min(ctl.page_cap).max(1);
                        let end = (position + eff).min(ctl.total);
                        last_query_ids = (position..end).map(|i| format!("mb-{i}")).collect();
                        json!({
                            "accountId": "a",
                            "ids": last_query_ids,
                            "total": ctl.total,
                            "position": position,
                        })
                    }
                    "Mailbox/get" => {
                        let list: Vec<Value> = last_query_ids
                            .iter()
                            .map(|id| json!({ "id": id, "name": id, "parentId": Value::Null }))
                            .collect();
                        json!({ "accountId": "a", "list": list })
                    }
                    _ => json!({ "accountId": "a" }),
                };
                responses.push(json!([name, result, cid]));
            }
            axum::Json(json!({ "methodResponses": responses, "sessionState": "s" }))
        }

        let queries = std::sync::Arc::new(tokio::sync::Mutex::new(Vec::new()));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let base = format!("http://127.0.0.1:{port}");
        let ctl = PagedCtl {
            base: base.clone(),
            total,
            page_cap,
            queries: queries.clone(),
        };
        let app = axum::Router::new()
            .route("/session", get(session))
            .route("/", post(api))
            .layer(axum::Extension(ctl));
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        (base, queries)
    }

    #[tokio::test]
    async fn query_all_mailboxes_pages_until_exhausted() {
        // 1200 mailboxes with a server page cap of 500: the walk must issue
        // three pages (0, 500, 1000) and return every mailbox.
        let (base, queries) = spawn_paged_mailbox_stub(1200, 500).await;
        let client = JmapClient::new(&base).unwrap();
        let pw = SecretString::from("pw");
        let result = client
            .query_all_mailboxes("u", &pw)
            .await
            .expect("paged walk must succeed");
        assert_eq!(result.mailboxes.len(), 1200);
        assert_eq!(result.total, 1200);
        assert_eq!(
            *queries.lock().await,
            vec![0, 500, 1000],
            "the walk must page at the server's effective limit"
        );
    }

    #[tokio::test]
    async fn query_all_mailboxes_single_page_when_under_cap() {
        let (base, queries) = spawn_paged_mailbox_stub(10, 500).await;
        let client = JmapClient::new(&base).unwrap();
        let pw = SecretString::from("pw");
        let result = client
            .query_all_mailboxes("u", &pw)
            .await
            .expect("single-page walk must succeed");
        assert_eq!(result.mailboxes.len(), 10);
        assert_eq!(*queries.lock().await, vec![0], "one page suffices");
    }

    #[tokio::test]
    async fn query_all_mailboxes_pages_when_server_clamps_limit() {
        // The server clamps every page to 200 (< the requested 500). A "short
        // page means done" heuristic would stop after one page; the walk must
        // instead use the requested `total` and page until it is reached.
        let (base, queries) = spawn_paged_mailbox_stub(1200, 200).await;
        let client = JmapClient::new(&base).unwrap();
        let pw = SecretString::from("pw");
        let result = client
            .query_all_mailboxes("u", &pw)
            .await
            .expect("clamped walk must succeed");
        assert_eq!(result.mailboxes.len(), 1200);
        assert_eq!(
            *queries.lock().await,
            vec![0, 200, 400, 600, 800, 1000],
            "the walk must not stop at the first short page"
        );
    }
}
