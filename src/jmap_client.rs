use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::json;
use thiserror::Error;
use uuid::Uuid;

#[derive(Debug, Error)]
pub enum JmapError {
    #[error("connection error: {0}")]
    Connection(#[from] reqwest::Error),

    #[error("authentication failed: {0}")]
    Auth(String),

    #[error("parse error: {0}")]
    Parse(String),

    #[error("not found: {0}")]
    NotFound(String),

    #[error("JMAP API error: {0}")]
    Api(String),
}

impl JmapError {
    /// Returns `true` for transient errors (network / connection / server
    /// issues) where retrying later is likely to succeed and cached state
    /// should be preserved.
    ///
    /// Inspects the underlying `reqwest::Error` to distinguish genuinely
    /// transient conditions (timeouts, connection resets, DNS failures,
    /// HTTP 5xx server errors) from non-transient ones.
    ///
    /// Parse errors are treated as **non-transient** because they typically
    /// indicate a persistent problem (missing fields, schema mismatches,
    /// deserialization failures) that will recur on every retry.  Treating
    /// them as transient would preserve stale sync state and trap the client
    /// in a loop re-encountering the same parse failure.  By classifying
    /// them as non-transient, the caller can trigger a full re-sync which
    /// rebuilds state from scratch and recovers cleanly.
    pub fn is_transient(&self) -> bool {
        match self {
            JmapError::Connection(e) => {
                e.is_timeout()
                    || e.is_connect()
                    || e.status().is_some_and(|s| {
                        s.is_server_error()
                            || s == reqwest::StatusCode::TOO_MANY_REQUESTS
                            || s == reqwest::StatusCode::REQUEST_TIMEOUT
                    })
            }
            // JMAP method-level errors (RFC 8620 §3.6.1) embed the error
            // type as a "[type]" prefix.  The transient types are:
            //   serverFail          — unexpected/transient internal error
            //   serverUnavailable   — temporarily unavailable
            //   serverPartialFail   — some calls may have succeeded
            JmapError::Api(msg) => {
                msg.starts_with("[serverFail]")
                    || msg.starts_with("[serverUnavailable]")
                    || msg.starts_with("[serverPartialFail]")
            }
            _ => false,
        }
    }
}

#[derive(Debug, Clone)]
pub struct JmapSession {
    pub api_url: String,
    pub access_token: String,
    pub account_id: String,
    pub principals_account_id: String,
    pub client: Client,
    /// Server-advertised `maxObjectsInSet` from `urn:ietf:params:jmap:core`
    /// capabilities.  Defaults to 500 when the server omits the value.
    pub max_objects_in_set: usize,
}

/// JSCalendar NDay object (RFC 8984 Section 4.3.2) used inside
/// `RecurrenceRule.by_day` to represent a day-of-week with an optional
/// week-offset (e.g. "second Tuesday").
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NDay {
    #[serde(rename = "@type", default = "nday_type_default")]
    pub r#type: String,
    pub day: String,
    #[serde(rename = "nthOfPeriod", skip_serializing_if = "Option::is_none")]
    pub nth_of_period: Option<i32>,
}

fn nday_type_default() -> String {
    "NDay".to_string()
}

/// JSCalendar RecurrenceRule object (RFC 8984 Section 4.3.2).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecurrenceRule {
    #[serde(rename = "@type", default = "recurrence_rule_type_default")]
    pub r#type: String,
    pub frequency: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub interval: Option<u32>,
    #[serde(rename = "byDay", skip_serializing_if = "Option::is_none")]
    pub by_day: Option<Vec<NDay>>,
    #[serde(rename = "byMonthDay", skip_serializing_if = "Option::is_none")]
    pub by_month_day: Option<Vec<i32>>,
    #[serde(rename = "byMonth", skip_serializing_if = "Option::is_none")]
    pub by_month: Option<Vec<String>>,
    #[serde(rename = "bySetPosition", skip_serializing_if = "Option::is_none")]
    pub by_set_position: Option<Vec<i32>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub count: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub until: Option<String>,
}

fn recurrence_rule_type_default() -> String {
    "RecurrenceRule".to_string()
}

#[derive(Debug, Serialize, Deserialize, Default)]
pub struct JmapEvent {
    #[serde(rename = "id", skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(rename = "title", default)]
    pub title: String,
    #[serde(rename = "start")]
    pub start: String,
    #[serde(rename = "end")]
    pub end: String,
    #[serde(rename = "location", skip_serializing_if = "Option::is_none")]
    pub location: Option<String>,
    #[serde(rename = "description", skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(rename = "uid", skip_serializing_if = "Option::is_none")]
    pub uid: Option<String>,
    #[serde(
        rename = "participants",
        skip_serializing_if = "Option::is_none",
        with = "participants_serde",
        default
    )]
    pub participants: Option<Vec<Participant>>,
    #[serde(rename = "showWithoutTime", default)]
    pub show_without_time: bool,
    #[serde(rename = "recurrenceRules", skip_serializing_if = "Option::is_none")]
    pub recurrence_rules: Option<Vec<RecurrenceRule>>,
    #[serde(rename = "updated", skip_serializing_if = "Option::is_none")]
    pub updated: Option<String>, // Added for ChangeKey
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Participant {
    pub email: String,
    pub name: String,
    #[serde(
        rename = "participationStatus",
        skip_serializing_if = "Option::is_none"
    )]
    pub status: Option<String>,
    /// The opaque participant ID used as the map key in JSCalendar
    /// (RFC 8984).  Preserved across round-trips so that patch paths
    /// (`participants/<id>/…`) address the correct entry on the server.
    #[serde(skip)]
    pub participant_id: Option<String>,
}

/// Custom serde module to convert between `Vec<Participant>` and the JSCalendar
/// participants map format (RFC 8984).
///
/// In JSCalendar the `participants` property is `Id[Participant]` — an object
/// whose keys are opaque identifiers and whose values carry participant
/// properties including `sendTo` (a map of method → URI, e.g.
/// `"imip": "mailto:user@example.com"`).
///
/// On **serialization** we use the stored `participant_id` (or generate a UUID)
/// as the map key and encode the email as `sendTo.imip`.
///
/// On **deserialization** we extract the email from `sendTo.imip` (stripping a
/// `mailto:` prefix when present) and fall back to the map key for backward
/// compatibility with servers that still key by email.
mod participants_serde {
    use super::Participant;
    use serde::{Deserialize, Deserializer, Serialize, Serializer};
    use std::collections::HashMap;
    use uuid::Uuid;

    /// Intermediate struct for serialization (RFC 8984 compliant).
    #[derive(Serialize)]
    struct ParticipantValue<'a> {
        name: &'a str,
        #[serde(rename = "sendTo")]
        send_to: HashMap<&'a str, String>,
        #[serde(
            rename = "participationStatus",
            skip_serializing_if = "Option::is_none"
        )]
        status: &'a Option<String>,
    }

    pub fn serialize<S>(value: &Option<Vec<Participant>>, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match value {
            Some(participants) => {
                let mut map = HashMap::new();
                for p in participants {
                    let key = p.participant_id.as_deref().unwrap_or("").to_string();
                    let key = if key.is_empty() {
                        Uuid::new_v4().to_string()
                    } else {
                        key
                    };
                    let mut send_to = HashMap::new();
                    send_to.insert("imip", format!("mailto:{}", p.email));
                    if map
                        .insert(
                            key,
                            ParticipantValue {
                                name: &p.name,
                                send_to,
                                status: &p.status,
                            },
                        )
                        .is_some()
                    {
                        tracing::warn!(
                            email = %p.email,
                            "Duplicate participant id during serialization; \
                             last entry wins"
                        );
                    }
                }
                map.serialize(serializer)
            }
            None => serializer.serialize_none(),
        }
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Option<Vec<Participant>>, D::Error>
    where
        D: Deserializer<'de>,
    {
        /// Intermediate struct for deserialization.
        #[derive(Deserialize)]
        struct ParticipantValue {
            #[serde(default)]
            name: String,
            #[serde(rename = "sendTo", default)]
            send_to: Option<HashMap<String, String>>,
            #[serde(rename = "participationStatus", default)]
            status: Option<String>,
        }

        /// Extract the email from `sendTo.imip`, stripping the `mailto:` prefix.
        fn email_from_send_to(send_to: &Option<HashMap<String, String>>) -> Option<String> {
            send_to.as_ref().and_then(|m| {
                m.get("imip").map(|uri| {
                    if uri
                        .as_bytes()
                        .get(..7)
                        .is_some_and(|prefix| prefix.eq_ignore_ascii_case(b"mailto:"))
                    {
                        uri[7..].to_string()
                    } else {
                        uri.to_string()
                    }
                })
            })
        }

        let opt: Option<HashMap<String, ParticipantValue>> = Option::deserialize(deserializer)?;
        Ok(opt.map(|map| {
            map.into_iter()
                .map(|(id, p)| {
                    let email = email_from_send_to(&p.send_to).unwrap_or_else(|| id.clone());
                    Participant {
                        email,
                        name: p.name,
                        status: p.status,
                        participant_id: Some(id),
                    }
                })
                .collect()
        }))
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Principal {
    pub name: String,
    pub email: String,
}

// Fix: Added Default derive
#[derive(Debug, Serialize, Deserialize, Default)]
pub struct JmapChanges {
    #[serde(rename = "accountId")]
    pub account_id: String,
    #[serde(rename = "oldState")]
    pub old_state: String,
    #[serde(rename = "newState")]
    pub new_state: String,
    pub created: Vec<String>,
    pub updated: Vec<String>,
    pub destroyed: Vec<String>,
}

/// Look up the account ID for a given JMAP capability URN, first checking
/// `primaryAccounts` and falling back to scanning `accounts` for a matching
/// `accountCapabilities` entry.
fn find_account_for_capability<'a>(
    body: &'a serde_json::Value,
    capability: &str,
) -> Option<&'a str> {
    body["primaryAccounts"][capability].as_str().or_else(|| {
        body["accounts"].as_object().and_then(|accounts| {
            accounts.iter().find_map(|(id, account)| {
                account
                    .get("accountCapabilities")
                    .and_then(|caps| caps.get(capability))
                    .map(|_| id.as_str())
            })
        })
    })
}

pub async fn get_session(jmap_url: &str, user: &str, pass: &str) -> Result<JmapSession, JmapError> {
    let client = Client::new();
    let token = base64::Engine::encode(
        &base64::engine::general_purpose::STANDARD,
        format!("{}:{}", user, pass),
    );
    let res = client
        .get(jmap_url)
        .header("Authorization", format!("Basic {}", token))
        .send()
        .await?;
    let status = res.status();
    if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
        return Err(JmapError::Auth(format!("HTTP {}", status)));
    }
    let body: serde_json::Value = res.error_for_status()?.json().await?;
    let account_id = find_account_for_capability(&body, "urn:ietf:params:jmap:calendars")
        .ok_or_else(|| JmapError::Parse("no usable account in JMAP session".into()))?
        .to_string();
    let principals_account_id =
        find_account_for_capability(&body, "urn:ietf:params:jmap:principals")
            .unwrap_or(&account_id)
            .to_string();
    let max_objects_in_set = body
        .get("capabilities")
        .and_then(|c| c.get("urn:ietf:params:jmap:core"))
        .and_then(|core| core.get("maxObjectsInSet"))
        .and_then(|v| v.as_u64())
        .map(|v| v as usize)
        .unwrap_or(500);
    Ok(JmapSession {
        api_url: body["apiUrl"].as_str().unwrap_or(jmap_url).to_string(),
        access_token: token,
        account_id,
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::json;
use thiserror::Error;
use uuid::Uuid;

#[derive(Debug, Error)]
pub enum JmapError {
    #[error("connection error: {0}")]
    Connection(#[from] reqwest::Error),

    #[error("authentication failed: {0}")]
    Auth(String),

    #[error("parse error: {0}")]
    Parse(String),

    #[error("not found: {0}")]
    NotFound(String),

    #[error("JMAP API error: {0}")]
    Api(String),
}

impl JmapError {
    /// Returns `true` for transient errors (network / connection / server
    /// issues) where retrying later is likely to succeed and cached state
    /// should be preserved.
    ///
    /// Inspects the underlying `reqwest::Error` to distinguish genuinely
    /// transient conditions (timeouts, connection resets, DNS failures,
    /// HTTP 5xx server errors) from non-transient ones.
    ///
    /// Parse errors are treated as **non-transient** because they typically
    /// indicate a persistent problem (missing fields, schema mismatches,
    /// deserialization failures) that will recur on every retry.  Treating
    /// them as transient would preserve stale sync state and trap the client
    /// in a loop re-encountering the same parse failure.  By classifying
    /// them as non-transient, the caller can trigger a full re-sync which
    /// rebuilds state from scratch and recovers cleanly.
    pub fn is_transient(&self) -> bool {
        match self {
            JmapError::Connection(e) => {
                e.is_timeout()
                    || e.is_connect()
                    || e.status().is_some_and(|s| {
                        s.is_server_error()
                            || s == reqwest::StatusCode::TOO_MANY_REQUESTS
                            || s == reqwest::StatusCode::REQUEST_TIMEOUT
                    })
            }
            // JMAP method-level errors (RFC 8620 §3.6.1) embed the error
            // type as a "[type]" prefix.  The transient types are:
            //   serverFail          — unexpected/transient internal error
            //   serverUnavailable   — temporarily unavailable
            //   serverPartialFail   — some calls may have succeeded
            JmapError::Api(msg) => {
                msg.starts_with("[serverFail]")
                    || msg.starts_with("[serverUnavailable]")
                    || msg.starts_with("[serverPartialFail]")
            }
            _ => false,
        }
    }
}

#[derive(Debug, Clone)]
pub struct JmapSession {
    pub api_url: String,
    pub access_token: String,
    pub account_id: String,
    pub principals_account_id: String,
    pub client: Client,
    /// Server-advertised `maxObjectsInSet` from `urn:ietf:params:jmap:core`
    /// capabilities.  Defaults to 500 when the server omits the value.
    pub max_objects_in_set: usize,
    pub blob_account_id: Option<String>,
}

/// JSCalendar NDay object (RFC 8984 Section 4.3.2) used inside
/// `RecurrenceRule.by_day` to represent a day-of-week with an optional
/// week-offset (e.g. "second Tuesday").
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NDay {
    #[serde(rename = "@type", default = "nday_type_default")]
    pub r#type: String,
    pub day: String,
    #[serde(rename = "nthOfPeriod", skip_serializing_if = "Option::is_none")]
    pub nth_of_period: Option<i32>,
}

fn nday_type_default() -> String {
    "NDay".to_string()
}

/// JSCalendar RecurrenceRule object (RFC 8984 Section 4.3.2).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecurrenceRule {
    #[serde(rename = "@type", default = "recurrence_rule_type_default")]
    pub r#type: String,
    pub frequency: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub interval: Option<u32>,
    #[serde(rename = "byDay", skip_serializing_if = "Option::is_none")]
    pub by_day: Option<Vec<NDay>>,
    #[serde(rename = "byMonthDay", skip_serializing_if = "Option::is_none")]
    pub by_month_day: Option<Vec<i32>>,
    #[serde(rename = "byMonth", skip_serializing_if = "Option::is_none")]
    pub by_month: Option<Vec<String>>,
    #[serde(rename = "bySetPosition", skip_serializing_if = "Option::is_none")]
    pub by_set_position: Option<Vec<i32>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub count: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub until: Option<String>,
}

fn recurrence_rule_type_default() -> String {
    "RecurrenceRule".to_string()
}

#[derive(Debug, Serialize, Deserialize, Default)]
pub struct JmapEvent {
    #[serde(rename = "id", skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(rename = "title", default)]
    pub title: String,
    #[serde(rename = "start")]
    pub start: String,
    #[serde(rename = "end")]
    pub end: String,
    #[serde(rename = "location", skip_serializing_if = "Option::is_none")]
    pub location: Option<String>,
    #[serde(rename = "description", skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(rename = "uid", skip_serializing_if = "Option::is_none")]
    pub uid: Option<String>,
    #[serde(
        rename = "participants",
        skip_serializing_if = "Option::is_none",
        with = "participants_serde",
        default
    )]
    pub participants: Option<Vec<Participant>>,
    #[serde(rename = "showWithoutTime", default)]
    pub show_without_time: bool,
    #[serde(rename = "recurrenceRules", skip_serializing_if = "Option::is_none")]
    pub recurrence_rules: Option<Vec<RecurrenceRule>>,
    #[serde(rename = "updated", skip_serializing_if = "Option::is_none")]
    pub updated: Option<String>, // Added for ChangeKey
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Participant {
    pub email: String,
    pub name: String,
    #[serde(
        rename = "participationStatus",
        skip_serializing_if = "Option::is_none"
    )]
    pub status: Option<String>,
    /// The opaque participant ID used as the map key in JSCalendar
    /// (RFC 8984).  Preserved across round-trips so that patch paths
    /// (`participants/<id>/…`) address the correct entry on the server.
    #[serde(skip)]
    pub participant_id: Option<String>,
}

/// Custom serde module to convert between `Vec<Participant>` and the JSCalendar
/// participants map format (RFC 8984).
///
/// In JSCalendar the `participants` property is `Id[Participant]` — an object
/// whose keys are opaque identifiers and whose values carry participant
/// properties including `sendTo` (a map of method → URI, e.g.
/// `"imip": "mailto:user@example.com"`).
///
/// On **serialization** we use the stored `participant_id` (or generate a UUID)
/// as the map key and encode the email as `sendTo.imip`.
///
/// On **deserialization** we extract the email from `sendTo.imip` (stripping a
/// `mailto:` prefix when present) and fall back to the map key for backward
/// compatibility with servers that still key by email.
mod participants_serde {
    use super::Participant;
    use serde::{Deserialize, Deserializer, Serialize, Serializer};
    use std::collections::HashMap;
    use uuid::Uuid;

    /// Intermediate struct for serialization (RFC 8984 compliant).
    #[derive(Serialize)]
    struct ParticipantValue<'a> {
        name: &'a str,
        #[serde(rename = "sendTo")]
        send_to: HashMap<&'a str, String>,
        #[serde(
            rename = "participationStatus",
            skip_serializing_if = "Option::is_none"
        )]
        status: &'a Option<String>,
    }

    pub fn serialize<S>(value: &Option<Vec<Participant>>, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match value {
            Some(participants) => {
                let mut map = HashMap::new();
                for p in participants {
                    let key = p.participant_id.as_deref().unwrap_or("").to_string();
                    let key = if key.is_empty() {
                        Uuid::new_v4().to_string()
                    } else {
                        key
                    };
                    let mut send_to = HashMap::new();
                    send_to.insert("imip", format!("mailto:{}", p.email));
                    if map
                        .insert(
                            key,
                            ParticipantValue {
                                name: &p.name,
                                send_to,
                                status: &p.status,
                            },
                        )
                        .is_some()
                    {
                        tracing::warn!(
                            email = %p.email,
                            "Duplicate participant id during serialization; \
                             last entry wins"
                        );
                    }
                }
                map.serialize(serializer)
            }
            None => serializer.serialize_none(),
        }
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Option<Vec<Participant>>, D::Error>
    where
        D: Deserializer<'de>,
    {
        /// Intermediate struct for deserialization.
        #[derive(Deserialize)]
        struct ParticipantValue {
            #[serde(default)]
            name: String,
            #[serde(rename = "sendTo", default)]
            send_to: Option<HashMap<String, String>>,
            #[serde(rename = "participationStatus", default)]
            status: Option<String>,
        }

        /// Extract the email from `sendTo.imip`, stripping the `mailto:` prefix.
        fn email_from_send_to(send_to: &Option<HashMap<String, String>>) -> Option<String> {
            send_to.as_ref().and_then(|m| {
                m.get("imip").map(|uri| {
                    if uri
                        .as_bytes()
                        .get(..7)
                        .is_some_and(|prefix| prefix.eq_ignore_ascii_case(b"mailto:"))
                    {
                        uri[7..].to_string()
                    } else {
                        uri.to_string()
                    }
                })
            })
        }

        let opt: Option<HashMap<String, ParticipantValue>> = Option::deserialize(deserializer)?;
        Ok(opt.map(|map| {
            map.into_iter()
                .map(|(id, p)| {
                    let email = email_from_send_to(&p.send_to).unwrap_or_else(|| id.clone());
                    Participant {
                        email,
                        name: p.name,
                        status: p.status,
                        participant_id: Some(id),
                    }
                })
                .collect()
        }))
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Principal {
    pub name: String,
    pub email: String,
}

// Fix: Added Default derive
#[derive(Debug, Serialize, Deserialize, Default)]
pub struct JmapChanges {
    #[serde(rename = "accountId")]
    pub account_id: String,
    #[serde(rename = "oldState")]
    pub old_state: String,
    #[serde(rename = "newState")]
    pub new_state: String,
    pub created: Vec<String>,
    pub updated: Vec<String>,
    pub destroyed: Vec<String>,
}

/// Look up the account ID for a given JMAP capability URN, first checking
/// `primaryAccounts` and falling back to scanning `accounts` for a matching
/// `accountCapabilities` entry.
fn find_account_for_capability<'a>(
    body: &'a serde_json::Value,
    capability: &str,
) -> Option<&'a str> {
    body["primaryAccounts"][capability].as_str().or_else(|| {
        body["accounts"].as_object().and_then(|accounts| {
            accounts.iter().find_map(|(id, account)| {
                account
                    .get("accountCapabilities")
                    .and_then(|caps| caps.get(capability))
                    .map(|_| id.as_str())
            })
        })
    })
}

pub async fn get_session(jmap_url: &str, user: &str, pass: &str) -> Result<JmapSession, JmapError> {
    let client = Client::new();
    let token = base64::Engine::encode(
        &base64::engine::general_purpose::STANDARD,
        format!("{}:{}", user, pass),
    );
    let res = client
        .get(jmap_url)
        .header("Authorization", format!("Basic {}", token))
        .send()
        .await?;
    let status = res.status();
    if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
        return Err(JmapError::Auth(format!("HTTP {}", status)));
    }
    let body: serde_json::Value = res.error_for_status()?.json().await?;
    let account_id = find_account_for_capability(&body, "urn:ietf:params:jmap:calendars")
        .ok_or_else(|| JmapError::Parse("no usable account in JMAP session".into()))?
        .to_string();
    let principals_account_id =
        find_account_for_capability(&body, "urn:ietf:params:jmap:principals")
            .unwrap_or(&account_id)
            .to_string();
    let max_objects_in_set = body
        .get("capabilities")
        .and_then(|c| c.get("urn:ietf:params:jmap:core"))
        .and_then(|core| core.get("maxObjectsInSet"))
        .and_then(|v| v.as_u64())
        .map(|v| v as usize)
        .unwrap_or(500);
    let blob_account_id = find_account_for_capability(&body, "urn:ietf:params:jmap:blob")
        .map(String::from);
    Ok(JmapSession {
        api_url: body["apiUrl"].as_str().unwrap_or(jmap_url).to_string(),
        access_token: token,
        account_id,
        principals_account_id,
        client,
        max_objects_in_set,
        blob_account_id,
}

pub async fn get_default_calendar_id(session: &JmapSession) -> Result<String, JmapError> {
    let body = json!({ "using": ["urn:ietf:params:jmap:core", "urn:ietf:params:jmap:calendars"], "methodCalls": [["Calendar/get", { "accountId": session.account_id, "ids": null }, "c0"]] });
    let res = session
        .client
        .post(&session.api_url)
        .header("Authorization", format!("Basic {}", session.access_token))
        .json(&body)
        .send()
        .await?
        .error_for_status()?;
    let json: serde_json::Value = res.json().await?;
    check_jmap_method_error(&json)?;
    if let Some(list) = json["methodResponses"][0][1]["list"].as_array() {
        let cal_to_use = list
            .iter()
            .find(|cal| cal["isDefault"].as_bool().unwrap_or(false))
            .or_else(|| list.first());

        if let Some(cal) = cal_to_use {
            return cal["id"]
                .as_str()
                .map(String::from)
                .ok_or_else(|| JmapError::Parse("missing calendar ID".into()));
        }
    }
    Err(JmapError::NotFound("no calendars found".into()))
}

pub async fn get_calendar_state(session: &JmapSession) -> Result<String, JmapError> {
    let body = json!({ "using": ["urn:ietf:params:jmap:core", "urn:ietf:params:jmap:calendars"], "methodCalls": [["CalendarEvent/get", { "accountId": session.account_id, "ids": [] }, "c0"]] });
    let res = session
        .client
        .post(&session.api_url)
        .header("Authorization", format!("Basic {}", session.access_token))
        .json(&body)
        .send()
        .await?
        .error_for_status()?;
    let json: serde_json::Value = res.json().await?;
    check_jmap_method_error(&json)?;
    json["methodResponses"][0][1]["state"]
        .as_str()
        .map(String::from)
        .ok_or_else(|| JmapError::Parse("missing state".into()))
}

pub async fn get_calendar_events(session: &JmapSession) -> Result<Vec<JmapEvent>, JmapError> {
    let body = json!({ "using": ["urn:ietf:params:jmap:core", "urn:ietf:params:jmap:calendars"], "methodCalls": [["CalendarEvent/get", { "accountId": session.account_id, "ids": null, "properties": ["id", "title", "start", "end", "location", "description", "uid", "participants", "showWithoutTime", "recurrenceRules", "updated"] }, "c0"]] });
    let res = session
        .client
        .post(&session.api_url)
        .header("Authorization", format!("Basic {}", session.access_token))
        .json(&body)
        .send()
        .await?
        .error_for_status()?;
    let mut json: serde_json::Value = res.json().await?;
    check_jmap_method_error(&json)?;
    let list = json
        .get_mut("methodResponses")
        .and_then(|v| v.get_mut(0))
        .and_then(|v| v.get_mut(1))
        .and_then(|v| v.get_mut("list"))
        .map(serde_json::Value::take)
        .unwrap_or_default();
    let events: Vec<JmapEvent> = serde_json::from_value(list)
        .map_err(|e| JmapError::Parse(format!("event deserialization failed: {}", e)))?;
    Ok(events)
}

pub async fn get_events_by_ids(
    session: &JmapSession,
    ids: &[String],
) -> Result<Vec<JmapEvent>, JmapError> {
    if ids.is_empty() {
        return Ok(vec![]);
    }
    let body = json!({ "using": ["urn:ietf:params:jmap:core", "urn:ietf:params:jmap:calendars"], "methodCalls": [["CalendarEvent/get", { "accountId": session.account_id, "ids": ids, "properties": ["id", "title", "start", "end", "location", "description", "uid", "participants", "showWithoutTime", "recurrenceRules", "updated"] }, "c0"]] });
    let res = session
        .client
        .post(&session.api_url)
        .header("Authorization", format!("Basic {}", session.access_token))
        .json(&body)
        .send()
        .await?
        .error_for_status()?;
    let mut json: serde_json::Value = res.json().await?;
    check_jmap_method_error(&json)?;
    let list = json
        .get_mut("methodResponses")
        .and_then(|v| v.get_mut(0))
        .and_then(|v| v.get_mut(1))
        .and_then(|v| v.get_mut("list"))
        .map(serde_json::Value::take)
        .unwrap_or_default();
    let events: Vec<JmapEvent> = serde_json::from_value(list)
        .map_err(|e| JmapError::Parse(format!("event deserialization failed: {}", e)))?;
    Ok(events)
}

pub async fn get_event_by_id(session: &JmapSession, id: &str) -> Result<JmapEvent, JmapError> {
    let events = get_events_by_ids(session, &[id.to_string()]).await?;
    events
        .into_iter()
        .next()
        .ok_or_else(|| JmapError::NotFound(format!("event {}", id)))
}

/// Result of a successfully created JMAP event.
pub struct CreatedEvent {
    pub id: String,
    pub updated: Option<String>,
}

pub async fn push_event(
    session: &JmapSession,
    event: JmapEvent,
    calendar_id: &str,
) -> Result<CreatedEvent, JmapError> {
    let mut event = event;
    if event.uid.is_none() {
        event.uid = Some(Uuid::new_v4().to_string());
    }
    event.id = None;
    let mut event_json = serde_json::to_value(&event)
        .map_err(|e| JmapError::Parse(format!("serialize failed: {}", e)))?;
    if let Some(obj) = event_json.as_object_mut() {
        obj.insert("calendarIds".to_string(), json!({ (calendar_id): true }));
    }
    let create_map = json!({ Uuid::new_v4().to_string(): event_json });
    let body = json!({ "using": ["urn:ietf:params:jmap:core", "urn:ietf:params:jmap:calendars"], "methodCalls": [["CalendarEvent/set", { "accountId": session.account_id, "create": create_map }, "c0"]] });
    let res = session
        .client
        .post(&session.api_url)
        .header("Authorization", format!("Basic {}", session.access_token))
        .json(&body)
        .send()
        .await?
        .error_for_status()?;
    let json: serde_json::Value = res.json().await?;
    check_jmap_method_error(&json)?;
    if let Some(created) = json["methodResponses"][0][1]["created"].as_object()
        && let Some((_, val)) = created.into_iter().next()
    {
        let id = val["id"]
            .as_str()
            .map(String::from)
            .ok_or_else(|| JmapError::Api("create succeeded but missing server id".into()))?;
        let updated = val["updated"].as_str().map(String::from);
        return Ok(CreatedEvent { id, updated });
    }
    if let Some(not_created) = json["methodResponses"][0][1]["notCreated"].as_object()
        && let Some((_, err)) = not_created.into_iter().next()
    {
        let desc = err["description"].as_str().unwrap_or("unknown error");
        return Err(JmapError::Api(format!("create failed: {}", desc)));
    }
    Err(JmapError::Api("create failed".into()))
}

/// Result of a batch create: maps each creation ID to either a successfully
/// created event or an error description.
pub struct BatchCreateResult {
    pub created: Vec<(String, CreatedEvent)>,
    pub not_created: Vec<(String, String)>,
    /// If a transport or JMAP method-level error interrupted the batch mid-way,
    /// it is captured here.  Earlier successful chunks are still available in
    /// `created` / `not_created` so the caller can correctly account for them.
    pub chunk_error: Option<JmapError>,
}

/// Create multiple calendar events via JMAP `CalendarEvent/set`, splitting
/// the batch into chunks that respect `session.max_objects_in_set` so that
/// large sync uploads do not exceed the server limit.
///
/// `events` is a list of `(creation_id, JmapEvent)` pairs.  The `creation_id`
/// is an opaque caller-chosen key used to correlate results back to the input
/// (typically the ActiveSync ClientId).
///
/// Returns a [`BatchCreateResult`] with per-event outcomes.  Transport-level
/// and method-level errors still surface as `Err`.
pub async fn push_events(
    session: &JmapSession,
    events: Vec<(String, JmapEvent)>,
    calendar_id: &str,
) -> Result<BatchCreateResult, JmapError> {
    if events.is_empty() {
        return Ok(BatchCreateResult {
            created: Vec::new(),
            not_created: Vec::new(),
            chunk_error: None,
        });
    }

    // Prepare (jmap_creation_id, caller_id, event_json) triples up-front so
    // we can chunk them without re-serialising.
    let mut prepared: Vec<(String, String, serde_json::Value)> = Vec::with_capacity(events.len());
    for (caller_id, mut event) in events {
        event.id = None;
        if event.uid.is_none() {
            event.uid = Some(Uuid::new_v4().to_string());
        }
        let mut event_json = serde_json::to_value(&event)
            .map_err(|e| JmapError::Parse(format!("serialize failed: {}", e)))?;
        if let Some(obj) = event_json.as_object_mut() {
            obj.insert("calendarIds".to_string(), json!({ (calendar_id): true }));
        }
        let jmap_creation_id = Uuid::new_v4().to_string();
        prepared.push((jmap_creation_id, caller_id, event_json));
    }

    let mut result = BatchCreateResult {
        created: Vec::new(),
        not_created: Vec::new(),
        chunk_error: None,
    };

    // Send chunks that stay within the server's maxObjectsInSet limit.
    let chunk_size = session.max_objects_in_set.max(1);
    for chunk in prepared.chunks(chunk_size) {
        let mut create_map = serde_json::Map::new();
        let mut id_map: std::collections::HashMap<String, String> =
            std::collections::HashMap::new();
        for (jmap_creation_id, caller_id, event_json) in chunk {
            id_map.insert(jmap_creation_id.clone(), caller_id.clone());
            create_map.insert(jmap_creation_id.clone(), event_json.clone());
        }

        let body = json!({
            "using": ["urn:ietf:params:jmap:core", "urn:ietf:params:jmap:calendars"],
            "methodCalls": [["CalendarEvent/set", {
                "accountId": session.account_id,
                "create": create_map
            }, "c0"]]
        });

        // Instead of using `?` (which would discard results from earlier
        // successful chunks), we catch transport / method-level errors,
        // mark the remaining items in this chunk as failed, record the
        // error, and stop processing further chunks.
        let chunk_result: Result<serde_json::Value, JmapError> = async {
            let res = session
                .client
                .post(&session.api_url)
                .header("Authorization", format!("Basic {}", session.access_token))
                .json(&body)
                .send()
                .await?
                .error_for_status()?;
            let json: serde_json::Value = res.json().await?;
            check_jmap_method_error(&json)?;
            Ok(json)
        }
        .await;

        let json = match chunk_result {
            Ok(json) => json,
            Err(e) => {
                // Record all items in this (and any subsequent) chunk as failed.
                for (_, caller_id) in id_map {
                    result
                        .not_created
                        .push((caller_id, format!("chunk request failed: {}", e)));
                }
                result.chunk_error = Some(e);
                break;
            }
        };

        if let Some(created) = json["methodResponses"][0][1]["created"].as_object() {
            for (jmap_id, val) in created {
                if let Some(caller_id) = id_map.remove(jmap_id) {
                    let Some(server_id) = val["id"].as_str() else {
                        result.not_created.push((
                            caller_id,
                            "create succeeded but missing server id".to_string(),
                        ));
                        continue;
                    };
                    let updated = val["updated"].as_str().map(String::from);
                    result.created.push((
                        caller_id,
                        CreatedEvent {
                            id: server_id.to_string(),
                            updated,
                        },
                    ));
                }
            }
        }

        if let Some(not_created) = json["methodResponses"][0][1]["notCreated"].as_object() {
            for (jmap_id, err) in not_created {
                if let Some(caller_id) = id_map.remove(jmap_id) {
                    let error_type = err["type"].as_str().unwrap_or("unknown").to_string();
                    let desc = err["description"]
                        .as_str()
                        .unwrap_or("unknown error")
                        .to_string();
                    result
                        .not_created
                        .push((caller_id, format!("{}: {}", error_type, desc)));
                }
            }
        } // <-- missing brace added here

        // Any remaining IDs in id_map were neither created nor explicitly
        // rejected — treat them as failures.
        for (_, caller_id) in id_map {
            result
                .not_created
                .push((caller_id, "no response from server".to_string()));
        }
    }

    // When a chunk error interrupted the loop, mark items from any
    // remaining (unsent) chunks as failed too.  `prepared.chunks()` is an
    // iterator so we cannot easily know where we stopped; instead we
    // collect the caller IDs that were never placed into `result` at all.
    if result.chunk_error.is_some() {
        let accounted: std::collections::HashSet<String> = result
            .created
            .iter()
            .map(|(id, _)| id.clone())
            .chain(result.not_created.iter().map(|(id, _)| id.clone()))
            .collect();
        for (_, caller_id, _) in &prepared {
            if !accounted.contains(caller_id) {
                result.not_created.push((
                    caller_id.clone(),
                    "chunk request failed (unsent)".to_string(),
                ));
            }
        }
    }

    Ok(result)
}

fn check_jmap_method_error(json: &serde_json::Value) -> Result<(), JmapError> {
    if let Some(responses) = json.get("methodResponses").and_then(|v| v.as_array()) {
        if responses.is_empty() {
            return Err(JmapError::Parse(
                "Malformed or missing methodResponses".to_string(),
            ));
        }
        for resp in responses {
            if resp.get(0).and_then(|v| v.as_str()) == Some("error") {
                let error_obj = resp.get(1);
                let error_type = error_obj
                    .and_then(|e| e.get("type"))
                    .and_then(|t| t.as_str())
                    .unwrap_or("unknownError");
                let desc = error_obj
                    .and_then(|e| e.get("description"))
                    .and_then(|d| d.as_str())
                    .unwrap_or("unknown JMAP error");
                return Err(JmapError::Api(format!("[{}] {}", error_type, desc)));
            }
        }
    } else {
        return Err(JmapError::Parse(
            "Malformed or missing methodResponses".to_string(),
        ));
    }
    Ok(())
}

pub async fn patch_event(
    session: &JmapSession,
    id: &str,
    patch: serde_json::Map<String, serde_json::Value>,
) -> Result<(), JmapError> {
    let update_map = json!({ (id): patch });
    let body = json!({ "using": ["urn:ietf:params:jmap:core", "urn:ietf:params:jmap:calendars"], "methodCalls": [["CalendarEvent/set", { "accountId": session.account_id, "update": update_map }, "c0"]] });
    let res = session
        .client
        .post(&session.api_url)
        .header("Authorization", format!("Basic {}", session.access_token))
        .json(&body)
        .send()
        .await?
        .error_for_status()?;
    let json: serde_json::Value = res.json().await?;
    check_jmap_method_error(&json)?;
    if let Some(not_updated) = json["methodResponses"][0][1]["notUpdated"].as_object()
        && !not_updated.is_empty()
    {
        let desc = not_updated
            .values()
            .next()
            .and_then(|v| v["description"].as_str())
            .unwrap_or("unknown error");
        return Err(JmapError::Api(format!("update failed: {}", desc)));
    }
    Ok(())
}

/// Result of a batch update: tracks which event IDs were successfully updated
/// and which were rejected by the server.
pub struct BatchUpdateResult {
    pub updated: Vec<String>,
    pub not_updated: Vec<(String, String)>,
    /// If a transport or JMAP method-level error interrupted the batch mid-way,
    /// it is captured here.  Earlier successful chunks are still available in
    /// `updated` / `not_updated` so the caller can correctly account for them.
    pub chunk_error: Option<JmapError>,
}

/// Update multiple calendar events via JMAP `CalendarEvent/set`, splitting the
/// batch into chunks that respect `session.max_objects_in_set`.
///
/// `patches` is a list of `(event_id, patch)` pairs where each patch is a
/// JSON object of JMAP property updates.
///
/// Returns a [`BatchUpdateResult`] with per-event outcomes.
pub async fn batch_patch_events(
    session: &JmapSession,
    patches: Vec<(String, serde_json::Map<String, serde_json::Value>)>,
) -> Result<BatchUpdateResult, JmapError> {
    if patches.is_empty() {
        return Ok(BatchUpdateResult {
            updated: Vec::new(),
            not_updated: Vec::new(),
            chunk_error: None,
        });
    }

    let mut result = BatchUpdateResult {
        updated: Vec::new(),
        not_updated: Vec::new(),
        chunk_error: None,
    };

    let chunk_size = session.max_objects_in_set.max(1);
    for chunk in patches.chunks(chunk_size) {
        let mut update_map = serde_json::Map::new();
        let mut ids_in_chunk: Vec<String> = Vec::with_capacity(chunk.len());
        for (id, patch) in chunk {
            ids_in_chunk.push(id.clone());
            update_map.insert(id.clone(), serde_json::Value::Object(patch.clone()));
        }

        let body = json!({
            "using": ["urn:ietf:params:jmap:core", "urn:ietf:params:jmap:calendars"],
            "methodCalls": [["CalendarEvent/set", {
                "accountId": session.account_id,
                "update": update_map
            }, "c0"]]
        });

        let chunk_result: Result<serde_json::Value, JmapError> = async {
            let res = session
                .client
                .post(&session.api_url)
                .header("Authorization", format!("Basic {}", session.access_token))
                .json(&body)
                .send()
                .await?
                .error_for_status()?;
            let json: serde_json::Value = res.json().await?;
            check_jmap_method_error(&json)?;
            Ok(json)
        }
        .await;

        let json = match chunk_result {
            Ok(json) => json,
            Err(e) => {
                for id in ids_in_chunk {
                    result
                        .not_updated
                        .push((id, format!("chunk request failed: {}", e)));
                }
                result.chunk_error = Some(e);
                break;
            }
        };

        // Collect IDs that the server explicitly confirmed as updated.
        let confirmed_ids: std::collections::HashSet<String> =
            json["methodResponses"][0][1]["updated"]
                .as_object()
                .map(|m| m.keys().cloned().collect())
                .unwrap_or_default();

        // Collect IDs that the server explicitly rejected.
        let mut failed_ids: std::collections::HashSet<String> = std::collections::HashSet::new();
        if let Some(not_updated) = json["methodResponses"][0][1]["notUpdated"].as_object() {
            for (id, err) in not_updated {
                failed_ids.insert(id.clone());
                let desc = err["description"]
                    .as_str()
                    .unwrap_or("unknown error")
                    .to_string();
                result.not_updated.push((id.clone(), desc));
            }
        }

        // Only mark IDs as updated if the server explicitly confirmed them.
        // IDs absent from both `updated` and `notUpdated` are treated as
        // failures since the server did not confirm the update.
        for id in ids_in_chunk {
            if confirmed_ids.contains(&id) {
                result.updated.push(id);
            } else if !failed_ids.contains(&id) {
                result
                    .not_updated
                    .push((id, "server did not confirm update".to_string()));
            }
        }
    }

    // Mark items from unsent chunks as failed when a chunk error occurred.
    if result.chunk_error.is_some() {
        let accounted: std::collections::HashSet<String> = result
            .updated
            .iter()
            .cloned()
            .chain(result.not_updated.iter().map(|(id, _)| id.clone()))
            .collect();
        for (id, _) in &patches {
            if !accounted.contains(id) {
                result
                    .not_updated
                    .push((id.clone(), "chunk request failed (unsent)".to_string()));
            }
        }
    }

    Ok(result)
}

/// Result of a batch destroy: tracks which event IDs were successfully
/// destroyed and which were rejected by the server.
pub struct BatchDestroyResult {
    pub destroyed: Vec<String>,
    pub not_destroyed: Vec<(String, String)>,
    /// If a transport or JMAP method-level error interrupted the batch mid-way,
    /// it is captured here.  Earlier successful chunks are still available in
    /// `destroyed` / `not_destroyed` so the caller can correctly account for them.
    pub chunk_error: Option<JmapError>,
}

/// Destroy calendar events by ID, splitting the batch into chunks that respect
/// `session.max_objects_in_set`.
///
/// Returns a [`BatchDestroyResult`] with per-event outcomes.  Transport-level
/// and method-level errors still surface as `Err`.
pub async fn destroy_events(
    session: &JmapSession,
    ids: Vec<String>,
) -> Result<BatchDestroyResult, JmapError> {
    if ids.is_empty() {
        return Ok(BatchDestroyResult {
            destroyed: Vec::new(),
            not_destroyed: Vec::new(),
            chunk_error: None,
        });
    }

    let mut result = BatchDestroyResult {
        destroyed: Vec::new(),
        not_destroyed: Vec::new(),
        chunk_error: None,
    };

    let chunk_size = session.max_objects_in_set.max(1);
    for chunk in ids.chunks(chunk_size) {
        let chunk_ids: Vec<&String> = chunk.iter().collect();

        let body = json!({
            "using": ["urn:ietf:params:jmap:core", "urn:ietf:params:jmap:calendars"],
            "methodCalls": [["CalendarEvent/set", {
                "accountId": session.account_id,
                "destroy": chunk_ids
            }, "c0"]]
        });

        let chunk_result: Result<serde_json::Value, JmapError> = async {
            let res = session
                .client
                .post(&session.api_url)
                .header("Authorization", format!("Basic {}", session.access_token))
                .json(&body)
                .send()
                .await?
                .error_for_status()?;
            let json: serde_json::Value = res.json().await?;
            check_jmap_method_error(&json)?;
            Ok(json)
        }
        .await;

        let json = match chunk_result {
            Ok(json) => json,
            Err(e) => {
                for id in chunk {
                    result
                        .not_destroyed
                        .push((id.clone(), format!("chunk request failed: {}", e)));
                }
                result.chunk_error = Some(e);
                break;
            }
        };

        // Collect IDs the server explicitly confirmed as destroyed.
        let confirmed: std::collections::HashSet<String> =
            json["methodResponses"][0][1]["destroyed"]
                .as_array()
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default();

        // Collect IDs the server explicitly rejected.
        let mut failed: std::collections::HashSet<String> = std::collections::HashSet::new();
        if let Some(not_destroyed) = json["methodResponses"][0][1]["notDestroyed"].as_object() {
            for (id, err) in not_destroyed {
                failed.insert(id.clone());
                let err_type = err["type"].as_str().unwrap_or("serverFail").to_string();
                result.not_destroyed.push((id.clone(), err_type));
            }
        }

        for id in chunk {
            if confirmed.contains(id) {
                result.destroyed.push(id.clone());
            } else if !failed.contains(id) {
                result
                    .not_destroyed
                    .push((id.clone(), "server did not confirm destroy".to_string()));
            }
        }
    }

    // Mark items from unsent chunks as failed when a chunk error occurred.
    if result.chunk_error.is_some() {
        let accounted: std::collections::HashSet<String> = result
            .destroyed
            .iter()
            .cloned()
            .chain(result.not_destroyed.iter().map(|(id, _)| id.clone()))
            .collect();
        for id in &ids {
            if !accounted.contains(id) {
                result
                    .not_destroyed
                    .push((id.clone(), "chunk request failed (unsent)".to_string()));
            }
        }
    }

    Ok(result)
}

pub async fn get_calendar_changes(
    session: &JmapSession,
    since: &str,
) -> Result<JmapChanges, JmapError> {
    let body = json!({ "using": ["urn:ietf:params:jmap:core", "urn:ietf:params:jmap:calendars"], "methodCalls": [["CalendarEvent/changes", { "accountId": session.account_id, "sinceState": since }, "c0"]] });
    let res = session
        .client
        .post(&session.api_url)
        .header("Authorization", format!("Basic {}", session.access_token))
        .json(&body)
        .send()
        .await?
        .error_for_status()?;
    let mut json: serde_json::Value = res.json().await?;
    check_jmap_method_error(&json)?;
    let changes_val = json
        .get_mut("methodResponses")
        .and_then(|v| v.get_mut(0))
        .and_then(|v| v.get_mut(1))
        .map(serde_json::Value::take)
        .unwrap_or_default();
    serde_json::from_value(changes_val).map_err(|e| JmapError::Parse(format!("changes: {}", e)))
}

pub async fn search_principals(
    session: &JmapSession,
    query: &str,
) -> Result<Vec<Principal>, JmapError> {
    let body = json!({ "using": ["urn:ietf:params:jmap:core", "urn:ietf:params:jmap:principals"], "methodCalls": [["Principal/query", { "accountId": session.principals_account_id, "filter": { "operator": "OR", "conditions": [{ "email": query }, { "name": query }] } }, "c0"], ["Principal/get", { "accountId": session.principals_account_id, "#ids": { "resultOf": "c0", "name": "Principal/query", "path": "/ids" } }, "c1"]] });
    let res = session
        .client
        .post(&session.api_url)
        .header("Authorization", format!("Basic {}", session.access_token))
        .json(&body)
        .send()
        .await?
        .error_for_status()?;
    let json: serde_json::Value = res.json().await?;
    check_jmap_method_error(&json)?;
    let mut results = Vec::new();
    if let Some(list) = json["methodResponses"][1][1]["list"].as_array() {
        for item in list {
            results.push(Principal {
                name: item["name"].as_str().unwrap_or_default().to_string(),
                email: item["email"].as_str().unwrap_or_default().to_string(),
            });
        }
    }
    Ok(results)
}

pub async fn find_event_by_uid(session: &JmapSession, uid: &str) -> Result<String, JmapError> {
    let body = json!({ "using": ["urn:ietf:params:jmap:core", "urn:ietf:params:jmap:calendars"], "methodCalls": [["CalendarEvent/query", { "accountId": session.account_id, "filter": { "uid": uid } }, "c0"], ["CalendarEvent/get", { "accountId": session.account_id, "#ids": { "resultOf": "c0", "name": "CalendarEvent/query", "path": "/ids" } }, "c1"]] });
    let res = session
        .client
        .post(&session.api_url)
        .header("Authorization", format!("Basic {}", session.access_token))
        .json(&body)
        .send()
        .await?
        .error_for_status()?;
    let json: serde_json::Value = res.json().await?;
    check_jmap_method_error(&json)?;
    json["methodResponses"][1][1]["list"]
        .get(0)
        .and_then(|item| item["id"].as_str())
        .map(String::from)
        .ok_or_else(|| JmapError::NotFound(format!("event with uid {}", uid)))
}

pub async fn update_participant_status(
    session: &JmapSession,
    event_id: &str,
    user_email: &str,
    status: &str,
) -> Result<(), JmapError> {
    // Fetch only the properties needed to discover the opaque participant
    // ID that the server uses as the map key for this email (RFC 8984).
    // We include "start" and "end" because the response is deserialized
    // into JmapEvent which requires those fields.
    let body = json!({
        "using": ["urn:ietf:params:jmap:core", "urn:ietf:params:jmap:calendars"],
        "methodCalls": [["CalendarEvent/get", {
            "accountId": session.account_id,
            "ids": [event_id],
            "properties": ["id", "start", "end", "participants"]
        }, "c0"]]
    });
    let res = session
        .client
        .post(&session.api_url)
        .header("Authorization", format!("Basic {}", session.access_token))
        .json(&body)
        .send()
        .await?
        .error_for_status()?;
    let mut json_resp: serde_json::Value = res.json().await?;
    check_jmap_method_error(&json_resp)?;
    let list = json_resp
        .get_mut("methodResponses")
        .and_then(|v| v.get_mut(0))
        .and_then(|v| v.get_mut(1))
        .and_then(|v| v.get_mut("list"))
        .map(serde_json::Value::take)
        .unwrap_or_default();
    let events: Vec<JmapEvent> = serde_json::from_value(list)
        .map_err(|e| JmapError::Parse(format!("event deserialization failed: {}", e)))?;
    let event = events
        .into_iter()
        .next()
        .ok_or_else(|| JmapError::NotFound(format!("event {}", event_id)))?;
    let participant_key = event
        .participants
        .as_ref()
        .and_then(|ps| ps.iter().find(|p| p.email.eq_ignore_ascii_case(user_email)))
        .and_then(|p| p.participant_id.as_deref())
        .ok_or_else(|| {
            JmapError::NotFound(format!(
                "participant {} not found in event {}",
                user_email, event_id
            ))
        })?
        .to_string();

    let escaped_key = participant_key.replace('~', "~0").replace('/', "~1");
    let mut patch = serde_json::Map::new();
    patch.insert(
        format!("participants/{}/participationStatus", escaped_key),
        json!(status),
    );
    patch_event(session, event_id, patch).await
}

pub async fn get_blob(session: &JmapSession, blob_id: &str) -> Result<Vec<u8>, JmapError> {
    let blob_account_id = session.blob_account_id.as_ref().ok_or_else(|| {
        JmapError::NotFound("JMAP server does not support Blob Management Extension".into())
    })?;

    let body = json!({ "using": ["urn:ietf:params:jmap:core", "urn:ietf:params:jmap:blob"], "methodCalls": [["Blob/get", { "accountId": blob_account_id, "ids": [blob_id], "properties": ["data:asBase64", "data:asText"] }, "c0"]] });
    let res = session
        .client
        .post(&session.api_url)
        .header("Authorization", format!("Basic {}", session.access_token))
        .json(&body)
        .send()
        .await?
        .error_for_status()?;
    let json: serde_json::Value = res.json().await?;
    check_jmap_method_error(&json)?;
    if let Some(b64) = json["methodResponses"][0][1]["list"][0]["data:asBase64"].as_str() {
        return base64::Engine::decode(&base64::engine::general_purpose::STANDARD, b64)
            .map_err(|e| JmapError::Parse(format!("base64 decode: {}", e)));
    }
    if let Some(text) = json["methodResponses"][0][1]["list"][0]["data:asText"].as_str() {
        return Ok(text.as_bytes().to_vec());
    }
    Err(JmapError::NotFound(format!("blob {}", blob_id)))
}
