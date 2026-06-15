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

use anyhow::{Result, anyhow};
use dashmap::DashMap;
use reqwest::header::{AUTHORIZATION, CONTENT_TYPE};
use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;
use tracing::{debug, trace, warn};

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
    #[serde(default)]
    #[allow(dead_code)]
    pub session_state: Option<String>,
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
    #[serde(default)]
    pub from: Option<Vec<JmapEmailAddress>>,
    #[serde(default)]
    pub to: Option<Vec<JmapEmailAddress>>,
    #[serde(default)]
    pub cc: Option<Vec<JmapEmailAddress>>,
    #[serde(default)]
    pub bcc: Option<Vec<JmapEmailAddress>>,
    #[serde(default)]
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

/// Result of a Mailbox/query call
#[derive(Clone, Debug)]
pub struct MailboxListResult {
    pub mailboxes: Vec<JmapMailbox>,
    pub total: u64,
}

/// Parameters for querying emails via JMAP.
pub struct QueryEmailsParams<'a> {
    pub account_id: &'a str,
    pub filter: Option<Value>,
    pub sort: Option<Vec<Value>>,
    pub position: u64,
    pub limit: u64,
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
#[derive(Clone, Debug, Serialize, Deserialize)]
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
    /// The iCalendar representation of this event.
    /// Requested via CalendarEvent/get with `properties: ["iCalendar"]`.
    /// Stalwart returns the original iCalendar data in this property.
    #[serde(default)]
    pub i_calendar: Option<String>,
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
}

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
    pub limit: u64,
    pub username: &'a str,
    pub password: &'a SecretString,
}

/// Parameters for creating/updating a calendar event via JMAP.
pub struct SetCalendarEventParams<'a> {
    pub account_id: &'a str,
    pub ics: &'a str,
    pub event_id: Option<&'a str>,
    pub calendar_id: &'a str,
    pub username: &'a str,
    pub password: &'a SecretString,
}

/// Duration for which a cached JMAP session is considered valid.
/// Per RFC 8621 §2.1, the session state changes when capabilities or
/// accounts change, which is rare. A 5-minute TTL avoids stale sessions
/// while eliminating redundant HTTP GETs per method call.
const SESSION_CACHE_TTL: std::time::Duration = std::time::Duration::from_secs(300);

/// JMAP client for email and calendar operations via Stalwart Mailserver.
#[derive(Clone)]
pub struct JmapClient {
    base_url: String,
    client: reqwest::Client,
    /// Cached sessions keyed by username, with expiry time.
    session_cache: Arc<DashMap<String, (Instant, JmapSession)>>,
}

impl JmapClient {
    /// Create a new JMAP client pointing at the Stalwart JMAP endpoint.
    pub fn new(base_url: &str) -> Result<Self> {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .map_err(|e| anyhow!("Failed to create JMAP HTTP client: {}", e))?;

        Ok(Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            client,
            session_cache: Arc::new(DashMap::new()),
        })
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
    fn basic_auth_header(username: &str, password: &SecretString) -> String {
        use base64::Engine;
        let encoded = base64::engine::general_purpose::STANDARD.encode(format!(
            "{}:{}",
            username,
            password.expose_secret()
        ));
        format!("Basic {}", encoded)
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

        // Cache the session with expiry
        let expires = Instant::now() + SESSION_CACHE_TTL;
        self.session_cache
            .insert(username.to_string(), (expires, session.clone()));

        Ok(session)
    }

    /// Make a JMAP API call (RFC 8621 §3.2).
    async fn api_call(
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

        let properties = json!([
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
            "to",
            "cc",
            "bcc",
            "replyTo",
            "subject",
            "preview",
            "bodyValues",
            "textBody",
            "htmlBody",
            "attachments"
        ]);

        // Batch Email/query + Email/get in a single request (RFC 8621 §3.6).
        // The Email/get ids are a back-reference to Email/query's /ids result.
        let method_calls = vec![
            (
                "Email/query",
                json!({
                    "accountId": params.account_id,
                    "filter": filter_val,
                    "sort": sort_val,
                    "position": params.position,
                    "limit": params.limit,
                    "calculateTotal": true,
                }),
                "q0",
            ),
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
                    "bodyProperties": ["partId", "blobId", "size", "type", "charset"],
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
            }
        }

        Ok(EmailListResult {
            emails,
            total,
            can_calculate_changes: can_calc,
            query_state,
            state,
        })
    }

    /// Get specific emails by ID.
    ///
    /// Maps to `Email/get` (RFC 8621 §4.1).
    pub async fn get_emails(
        &self,
        account_id: &str,
        ids: &[String],
        properties: Option<Value>,
        username: &str,
        password: &SecretString,
    ) -> Result<Vec<JmapEmail>> {
        let session = self.get_session(username, password).await?;
        let api_url = &session.api_url;

        let props = properties.unwrap_or_else(|| {
            json!([
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
                "to",
                "cc",
                "bcc",
                "replyTo",
                "subject",
                "preview"
            ])
        });

        let method_calls = vec![(
            "Email/get",
            json!({
                "accountId": account_id,
                "ids": ids,
                "properties": props,
                "bodyProperties": ["partId", "blobId", "size", "type", "charset"],
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
                let list: Vec<JmapEmail> = data
                    .get("list")
                    .and_then(|v| serde_json::from_value(v.clone()).ok())
                    .unwrap_or_default();
                return Ok(list);
            }
        }

        Err(anyhow!("Unexpected JMAP response structure for Email/get"))
    }

    /// Get a single email by ID.
    pub async fn get_email(
        &self,
        account_id: &str,
        email_id: &str,
        username: &str,
        password: &SecretString,
    ) -> Result<Option<JmapEmail>> {
        let emails = self
            .get_emails(
                account_id,
                &[email_id.to_string()],
                Some(json!([
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
                    "to",
                    "cc",
                    "bcc",
                    "replyTo",
                    "subject",
                    "preview",
                    "bodyValues",
                    "textBody",
                    "htmlBody",
                    "attachments"
                ])),
                username,
                password,
            )
            .await?;
        Ok(emails.into_iter().next())
    }

    /// Sync email changes since a given state token.
    ///
    /// Maps to `Email/changes` (RFC 8621 §4.4).
    pub async fn sync_email_changes(
        &self,
        account_id: &str,
        old_state: &str,
        username: &str,
        password: &SecretString,
    ) -> Result<EmailChangesResult> {
        let session = self.get_session(username, password).await?;
        let api_url = &session.api_url;

        let method_calls = vec![(
            "Email/changes",
            json!({
                "accountId": account_id,
                "sinceState": old_state,
                "maxChanges": 500,
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

    /// Destroy emails via JMAP Email/set (RFC 8621 §4.5).
    ///
    /// `destroy` is a list of email IDs to permanently delete.
    pub async fn destroy_emails(
        &self,
        account_id: &str,
        destroy: &[String],
        username: &str,
        password: &SecretString,
    ) -> Result<()> {
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
    /// This replaces SMTP submission. The flow per RFC 8621 is:
    /// 1. Create the Email via Email/set with mailboxIds including the sent folder
    /// 2. Create an EmailSubmission via EmailSubmission/set referencing the email
    /// 3. The server processes the submission and delivers the email
    ///
    /// Returns the created email ID on success.
    pub async fn submit_email(&self, params: SubmitEmailParams<'_>) -> Result<String> {
        let session = self.get_session(params.username, params.password).await?;
        let api_url = &session.api_url;

        // Build the mailboxIds — put in "sent" mailbox for SendAndSaveCopy semantics.
        let mailbox_ids = self
            .get_sent_mailbox_id(params.account_id, params.username, params.password)
            .await
            .unwrap_or_else(|_| "sent".to_string());

        // Build the email object per RFC 8621 §4.1
        let mut email_obj = json!({
            "mailboxIds": { (mailbox_ids): true },
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

        // Set body per RFC 8621 §4.1.4 — bodyValues
        email_obj["bodyValues"] = json!({
            "text": {
                "value": params.text_body,
                "isEncodingProblem": false,
                "isTruncated": false,
            }
        });

        // If HTML body provided, add it
        if let Some(html) = params.html_body {
            email_obj["bodyValues"]["html"] = json!({
                "value": html,
                "isEncodingProblem": false,
                "isTruncated": false,
            });
            email_obj["htmlBody"] = json!({
                "partId": "html",
                "type": "text/html"
            });
        }

        // Step 1: Create the Email via Email/set
        // Step 2: Create EmailSubmission referencing the created email
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

    /// Get the mailbox ID for the "sent" role.
    async fn get_sent_mailbox_id(
        &self,
        account_id: &str,
        username: &str,
        password: &SecretString,
    ) -> Result<String> {
        let ids = self.get_mailbox_ids_for_role(account_id, "sent", username, password).await?;
        ids.first()
            .cloned()
            .ok_or_else(|| anyhow!("No 'sent' mailbox found"))
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
        for (method, data, _) in response.method_responses {
            if method == "Mailbox/query"
                && let Some(arr) = data.get("ids").and_then(|v| v.as_array())
            {
                for id_val in arr {
                    if let Some(s) = id_val.as_str() {
                        ids.push(s.to_string());
                    }
                }
            }
        }

        Ok(ids)
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
                        "isSubscribed"
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

    /// Get the primary mail account ID from the JMAP session.
    pub async fn get_account_id(&self, username: &str, password: &SecretString) -> Result<String> {
        let session = self.get_session(username, password).await?;
        session
            .primary_accounts
            .get("urn:ietf:params:jmap:mail")
            .cloned()
            .ok_or_else(|| anyhow!("No primary mail account found in JMAP session"))
    }

    /// Verify JMAP credentials by fetching the session.
    pub async fn verify_credentials(&self, username: &str, password: &SecretString) -> Result<()> {
        self.get_session(username, password).await?;
        Ok(())
    }

    /// Health check: verify JMAP endpoint is reachable.
    pub async fn health_check(&self) -> Result<()> {
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
                    "properties": [
                        "id", "uid", "title", "start", "end", "duration",
                        "location", "participants", "alerts", "calendarIds",
                        "isAllDay", "recurrenceRules", "recurrenceOverrides",
                        "excluded", "sequence", "priority", "privacy",
                        "status", "timeZone", "created", "updated",
                        "iCalendar"
                    ],
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
                events = data
                    .get("list")
                    .and_then(|v| serde_json::from_value(v.clone()).ok())
                    .unwrap_or_default();
            }
        }

        Ok(CalendarEventListResult {
            events,
            total,
            query_state,
        })
    }

    /// Get a single calendar event by ID via CalendarEvent/get
    /// (draft-ietf-jmap-calendars §5.7).
    ///
    /// Replaces CalDAV `get_event` (GET).
    /// Returns the ICS data and the JMAP event ID.
    /// The ICS data comes from the `iCalendar` property returned by
    /// Stalwart when requested.
    pub async fn get_calendar_event(
        &self,
        account_id: &str,
        event_id: &str,
        username: &str,
        password: &SecretString,
    ) -> Result<(String, String)> {
        let session = self.get_session(username, password).await?;
        let api_url = &session.api_url;

        let method_calls = vec![(
            "CalendarEvent/get",
            json!({
                "accountId": account_id,
                "ids": [event_id],
                "properties": [
                    "id", "uid", "title", "iCalendar"
                ],
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
            if method == "CalendarEvent/get"
                && let Some(list) = data.get("list").and_then(|v| v.as_array())
                && let Some(event) = list.first()
            {
                let ics = event
                    .get("iCalendar")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let id = event
                    .get("id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                return Ok((ics, id));
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

    /// Create or update a calendar event via CalendarEvent/set
    /// (draft-ietf-jmap-calendars §5.9).
    ///
    /// Replaces CalDAV `put_event` (PUT).
    /// Stalwart supports creating events from iCalendar data by posting
    /// the raw ICS in CalendarEvent/set via the "iCalendar" property.
    /// This eliminates the need for CalendarEvent/parse + blob upload.
    ///
    /// For creates (event_id is None), the event is placed in the
    /// specified calendar_id.
    /// For updates (event_id is Some), the existing event is patched.
    ///
    /// Returns (jmap_event_id, uid) on success.
    pub async fn set_calendar_event(
        &self,
        params: SetCalendarEventParams<'_>,
    ) -> Result<(String, String)> {
        let session = self.get_session(params.username, params.password).await?;
        let api_url = &session.api_url;

        let create_obj = json!({
            "iCalendar": params.ics,
            "calendarIds": { (params.calendar_id): true },
        });

        if let Some(existing_id) = params.event_id {
            // Update existing event — use /set update with the new ICS
            let method_calls = vec![(
                "CalendarEvent/set",
                json!({
                    "accountId": params.account_id,
                    "update": {
                        (existing_id): {
                            "iCalendar": params.ics,
                        },
                    },
                }),
                "es0",
            )];

            let response = self
                .api_call(
                    api_url,
                    &["urn:ietf:params:jmap:core", JMAP_CAL_CAPABILITY],
                    method_calls,
                    params.username,
                    params.password,
                )
                .await?;

            for (method, data, _) in &response.method_responses {
                if method == "CalendarEvent/set" {
                    if let Some(not_updated) = data.get("notUpdated")
                        && !not_updated.is_null()
                        && not_updated.as_object().is_none_or(|o| !o.is_empty())
                    {
                        return Err(anyhow!("CalendarEvent/set update failed: {}", not_updated));
                    }
                    if let Some(updated) = data.get("updated")
                        && let Some(obj) = updated.get(existing_id)
                    {
                        let id = existing_id.to_string();
                        let uid = obj
                            .get("uid")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string();
                        return Ok((id, uid));
                    }
                }
            }

            Ok((existing_id.to_string(), String::new()))
        } else {
            // Create new event
            let method_calls = vec![(
                "CalendarEvent/set",
                json!({
                    "accountId": params.account_id,
                    "create": {
                        "e0": create_obj,
                    },
                }),
                "es0",
            )];

            let response = self
                .api_call(
                    api_url,
                    &["urn:ietf:params:jmap:core", JMAP_CAL_CAPABILITY],
                    method_calls,
                    params.username,
                    params.password,
                )
                .await?;

            for (method, data, _) in &response.method_responses {
                if method == "CalendarEvent/set" {
                    if let Some(not_created) = data.get("notCreated")
                        && !not_created.is_null()
                        && not_created.as_object().is_none_or(|o| !o.is_empty())
                    {
                        return Err(anyhow!("CalendarEvent/set create failed: {}", not_created));
                    }
                    if let Some(created) = data.get("created")
                        && let Some(e0) = created.get("e0")
                    {
                        let id = e0
                            .get("id")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string();
                        let uid = e0
                            .get("uid")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string();
                        return Ok((id, uid));
                    }
                }
            }

            Err(anyhow!(
                "CalendarEvent/set create returned no created event"
            ))
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
            if method == "CalendarEvent/set"
                && let Some(not_destroyed) = data.get("notDestroyed")
                && !not_destroyed.is_null()
                && not_destroyed.as_object().is_none_or(|o| !o.is_empty())
            {
                return Err(anyhow!(
                    "CalendarEvent/set destroy failed: {}",
                    not_destroyed
                ));
            }
        }

        Ok(())
    }

    /// Get free-busy availability via Principal/getAvailability
    /// (draft-ietf-jmap-calendars §2.2).
    ///
    /// Replaces CalDAV `get_freebusy` (REPORT free-busy-query).
    /// Returns the raw JSON response for the caller to parse.
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

        let method_calls = vec![(
            "Principal/getAvailability",
            json!({
                "accountId": account_id,
                "principalIds": [account_id],
                "duration": {
                    "start": start,
                    "end": end,
                },
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
}
