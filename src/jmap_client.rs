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

    #[error("invalid input: {0}")]
    InvalidInput(String),
}

impl JmapError {
    /// Returns `true` for transient errors (network / connection issues) where
    /// retrying later is likely to succeed and cached state should be preserved.
    ///
    /// Inspects the underlying `reqwest::Error` to distinguish genuinely
    /// transient conditions (timeouts, connection resets, DNS failures) from
    /// non-transient ones (e.g. response-body deserialization failures that
    /// would recur on every retry).
    pub fn is_transient(&self) -> bool {
        match self {
            JmapError::Connection(e) => {
                e.is_timeout() || e.is_connect() || e.is_request()
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
    pub client: Client,
}

#[derive(Debug, Serialize, Deserialize, Default)]
pub struct JmapEvent {
    #[serde(rename = "id", skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(rename = "title")]
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
    #[serde(rename = "isAllDay", default)]
    pub is_all_day: bool,
    #[serde(rename = "recurrenceRule", skip_serializing_if = "Option::is_none")]
    pub recurrence_rule: Option<String>,
    #[serde(rename = "updated", skip_serializing_if = "Option::is_none")]
    pub updated: Option<String>, // Added for ChangeKey
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Participant {
    pub email: String,
    pub name: String,
    #[serde(rename = "participationStatus", skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
}

/// Custom serde module to convert between `Vec<Participant>` and the JSCalendar
/// participants map format `{ "email": { "name": "...", ... }, ... }`.
///
/// In JSCalendar, participants are represented as a map keyed by email address,
/// where the value contains the participant properties (name, status) but NOT
/// the email (since it's already the key). This module handles that conversion.
mod participants_serde {
    use super::Participant;
    use serde::{Deserialize, Deserializer, Serialize, Serializer};
    use std::collections::HashMap;

    /// Intermediate struct for serialization that excludes the email field,
    /// since in JSCalendar format the email is used as the map key.
    #[derive(Serialize)]
    struct ParticipantValue<'a> {
        name: &'a str,
        #[serde(rename = "participationStatus", skip_serializing_if = "Option::is_none")]
        status: &'a Option<String>,
    }

    pub fn serialize<S>(
        value: &Option<Vec<Participant>>,
        serializer: S,
    ) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match value {
            Some(participants) => {
                let mut map = HashMap::new();
                for p in participants {
                    if map
                        .insert(
                            p.email.as_str(),
                            ParticipantValue {
                                name: &p.name,
                                status: &p.status,
                            },
                        )
                        .is_some()
                    {
                        tracing::warn!(
                            email = %p.email,
                            "Duplicate participant email during serialization; \
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
        /// Intermediate struct for deserialization that captures the participant
        /// properties from the map value. The email is set afterwards from the map key.
        #[derive(Deserialize)]
        struct ParticipantValue {
            name: String,
            #[serde(rename = "participationStatus", default)]
            status: Option<String>,
        }

        let opt: Option<HashMap<String, ParticipantValue>> = Option::deserialize(deserializer)?;
        Ok(opt.map(|map| {
            map.into_iter()
                .map(|(email, p)| Participant {
                    email,
                    name: p.name,
                    status: p.status,
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
    if !res.status().is_success() {
        return Err(JmapError::Auth(format!("HTTP {}", res.status())));
    }
    let body: serde_json::Value = res.json().await?;
    let account_id = body["primaryAccounts"]["urn:ietf:params:jmap:calendars"]
        .as_str()
        .ok_or_else(|| JmapError::Parse("missing calendar account ID".into()))?
        .to_string();
    Ok(JmapSession {
        api_url: body["apiUrl"].as_str().unwrap_or(jmap_url).to_string(),
        access_token: token,
        account_id,
        client,
    })
}

pub async fn get_default_calendar_id(session: &JmapSession) -> Result<String, JmapError> {
    let body = json!({ "using": ["urn:ietf:params:jmap:core", "urn:ietf:params:jmap:calendars"], "methodCalls": [["Calendar/get", { "accountId": session.account_id, "ids": null }, "c0"]] });
    let res = session
        .client
        .post(&session.api_url)
        .header("Authorization", format!("Basic {}", session.access_token))
        .json(&body)
        .send()
        .await?;
    let json: serde_json::Value = res.json().await?;
    check_jmap_set_errors(&json)?;
    if let Some(list) = json["methodResponses"][0][1]["list"].as_array() {
        // First, look for the calendar marked as default
        for cal in list {
            if cal["isDefault"].as_bool().unwrap_or(false) {
                if let Some(id) = cal["id"].as_str() {
                    return Ok(id.to_string());
                }
            }
        }
        // Fall back to the first calendar if none is marked as default
        if let Some(first) = list.first() {
            return first["id"]
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
        .await?;
    let json: serde_json::Value = res.json().await?;
    json["methodResponses"][0][1]["state"]
        .as_str()
        .map(String::from)
        .ok_or_else(|| JmapError::Parse("missing state".into()))
}

pub async fn get_calendar_events(session: &JmapSession) -> Result<Vec<JmapEvent>, JmapError> {
    let body = json!({ "using": ["urn:ietf:params:jmap:core", "urn:ietf:params:jmap:calendars"], "methodCalls": [["CalendarEvent/get", { "accountId": session.account_id, "ids": null, "properties": ["id", "title", "start", "end", "location", "description", "uid", "participants", "isAllDay", "recurrenceRule", "updated"] }, "c0"]] });
    let res = session
        .client
        .post(&session.api_url)
        .header("Authorization", format!("Basic {}", session.access_token))
        .json(&body)
        .send()
        .await?;
    let json: serde_json::Value = res.json().await?;
    let events: Vec<JmapEvent> =
        serde_json::from_value(json["methodResponses"][0][1]["list"].clone())
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
    let body = json!({ "using": ["urn:ietf:params:jmap:core", "urn:ietf:params:jmap:calendars"], "methodCalls": [["CalendarEvent/get", { "accountId": session.account_id, "ids": ids, "properties": ["id", "title", "start", "end", "location", "description", "uid", "participants", "isAllDay", "recurrenceRule", "updated"] }, "c0"]] });
    let res = session
        .client
        .post(&session.api_url)
        .header("Authorization", format!("Basic {}", session.access_token))
        .json(&body)
        .send()
        .await?;
    let json: serde_json::Value = res.json().await?;
    let events: Vec<JmapEvent> =
        serde_json::from_value(json["methodResponses"][0][1]["list"].clone())
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

pub async fn push_event(
    session: &JmapSession,
    event: JmapEvent,
    calendar_id: &str,
) -> Result<String, JmapError> {
    let mut event = event;
    if event.uid.is_none() {
        event.uid = Some(Uuid::new_v4().to_string());
    }
    let mut event_json =
        serde_json::to_value(&event).map_err(|e| JmapError::Parse(format!("serialize failed: {}", e)))?;
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
        .await?;
    let json: serde_json::Value = res.json().await?;
    if let Some(created) = json["methodResponses"][0][1]["created"].as_object()
        && let Some((_, val)) = created.into_iter().next()
    {
        return val["id"]
            .as_str()
            .map(String::from)
            .ok_or_else(|| JmapError::Api("create succeeded but missing server id".into()));
    }
    Err(JmapError::Api("create failed".into()))
}

fn check_jmap_set_errors(json: &serde_json::Value) -> Result<(), JmapError> {
    if let Some(resp) = json.get("methodResponses").and_then(|mr| mr.get(0)) {
        if resp.get(0).and_then(|v| v.as_str()) == Some("error") {
            let desc = resp
                .get(1)
                .and_then(|e| e.get("description"))
                .and_then(|d| d.as_str())
                .unwrap_or("unknown JMAP error");
            return Err(JmapError::Api(desc.to_string()));
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
        .await?;
    let json: serde_json::Value = res.json().await?;
    check_jmap_set_errors(&json)?;
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

pub async fn destroy_events(session: &JmapSession, ids: Vec<String>) -> Result<(), JmapError> {
    let body = json!({ "using": ["urn:ietf:params:jmap:core", "urn:ietf:params:jmap:calendars"], "methodCalls": [["CalendarEvent/set", { "accountId": session.account_id, "destroy": ids }, "c0"]] });
    let res = session
        .client
        .post(&session.api_url)
        .header("Authorization", format!("Basic {}", session.access_token))
        .json(&body)
        .send()
        .await?;
    let json: serde_json::Value = res.json().await?;
    check_jmap_set_errors(&json)?;
    if let Some(not_destroyed) = json["methodResponses"][0][1]["notDestroyed"].as_object()
        && !not_destroyed.is_empty()
    {
        let desc = not_destroyed
            .values()
            .next()
            .and_then(|v| v["description"].as_str())
            .unwrap_or("unknown error");
        return Err(JmapError::Api(format!("destroy failed: {}", desc)));
    }
    Ok(())
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
        .await?;
    let json: serde_json::Value = res.json().await?;
    // JMAP returns ["error", {"type": "..."}, "c0"] when the server cannot
    // fulfil the request (e.g. cannotCalculateChanges for an expired state).
    if json["methodResponses"][0][0].as_str() == Some("error") {
        let err_type = json["methodResponses"][0][1]["type"]
            .as_str()
            .unwrap_or("unknown");
        return Err(JmapError::Api(format!("CalendarEvent/changes: {}", err_type)));
    }
    serde_json::from_value(json["methodResponses"][0][1].clone())
        .map_err(|e| JmapError::Parse(format!("changes: {}", e)))
}

pub async fn search_principals(
    session: &JmapSession,
    query: &str,
) -> Result<Vec<Principal>, JmapError> {
    let body = json!({ "using": ["urn:ietf:params:jmap:core", "urn:ietf:params:jmap:principals"], "methodCalls": [["Principal/query", { "accountId": session.account_id, "filter": { "operator": "OR", "conditions": [{ "email": query }, { "name": query }] } }, "c0"], ["Principal/get", { "accountId": session.account_id, "#ids": { "resultOf": "c0", "name": "Principal/query", "path": "/ids" } }, "c1"]] });
    let res = session
        .client
        .post(&session.api_url)
        .header("Authorization", format!("Basic {}", session.access_token))
        .json(&body)
        .send()
        .await?;
    let json: serde_json::Value = res.json().await?;
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
        .await?;
    let json: serde_json::Value = res.json().await?;
    json["methodResponses"][1][1]["list"]
        .get(0)
        .and_then(|item| item["id"].as_str())
        .map(String::from)
        .ok_or_else(|| JmapError::NotFound(format!("event with uid {}", uid)))
}

/// Validate that an email address is safe for use as a JMAP patch path segment.
///
/// JMAP `CalendarEvent/set` update patches use RFC 6901 JSON Pointer paths
/// like `participants/<email>/participationStatus`. The email is the literal
/// map key on the server, so `~` and `/` in the email are escaped (`~0`, `~1`)
/// when building the pointer. This function rejects emails that are clearly
/// malformed or contain control characters that could cause unexpected
/// server behaviour.
fn is_valid_email_for_path(email: &str) -> bool {
    if email.is_empty() || email.len() > 254 {
        return false;
    }

    // Must have exactly one '@' separating local and domain parts
    let parts: Vec<&str> = email.splitn(3, '@').collect();
    if parts.len() != 2 {
        return false;
    }
    let (local, domain) = (parts[0], parts[1]);

    if local.is_empty() || domain.is_empty() {
        return false;
    }

    // Reject control characters (including NUL) and backslash
    if email.bytes().any(|b| b < 0x20 || b == 0x7f || b == b'\\') {
        return false;
    }

    // Reject local part starting/ending with '.' or containing '..'
    if local.starts_with('.') || local.ends_with('.') || local.contains("..") {
        return false;
    }

    // Domain must not start/end with '-' or '.' and must not contain '..'
    if domain.starts_with('.') || domain.starts_with('-')
        || domain.ends_with('.') || domain.ends_with('-')
        || domain.contains("..")
    {
        return false;
    }

    // Domain must contain at least one dot (TLD required)
    if !domain.contains('.') {
        return false;
    }

    true
}

pub async fn update_participant_status(
    session: &JmapSession,
    event_id: &str,
    user_email: &str,
    status: &str,
) -> Result<(), JmapError> {
    if !is_valid_email_for_path(user_email) {
        return Err(JmapError::InvalidInput(format!(
            "invalid email for participant path: {}",
            user_email
        )));
    }
    let patch = json!({ format!("participants/{}/participationStatus", user_email.replace('~', "~0").replace('/', "~1")): status });
    let body = json!({ "using": ["urn:ietf:params:jmap:core", "urn:ietf:params:jmap:calendars"], "methodCalls": [["CalendarEvent/set", { "accountId": session.account_id, "update": { (event_id): patch } }, "c0"]] });
    let res = session
        .client
        .post(&session.api_url)
        .header("Authorization", format!("Basic {}", session.access_token))
        .json(&body)
        .send()
        .await?;
    let json: serde_json::Value = res.json().await?;
    check_jmap_set_errors(&json)?;
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

pub async fn get_blob(session: &JmapSession, blob_id: &str) -> Result<Vec<u8>, JmapError> {
    let body = json!({ "using": ["urn:ietf:params:jmap:core", "urn:ietf:params:jmap:blob"], "methodCalls": [["Blob/get", { "accountId": session.account_id, "ids": [blob_id] }, "c0"]] });
    let res = session
        .client
        .post(&session.api_url)
        .header("Authorization", format!("Basic {}", session.access_token))
        .json(&body)
        .send()
        .await?;
    let json: serde_json::Value = res.json().await?;
    if let Some(b64) = json["methodResponses"][0][1]["list"][0]["data:asBase64"].as_str() {
        return base64::Engine::decode(&base64::engine::general_purpose::STANDARD, b64)
            .map_err(|e| JmapError::Parse(format!("base64 decode: {}", e)));
    }
    if let Some(text) = json["methodResponses"][0][1]["list"][0]["data:asText"].as_str() {
        return Ok(text.as_bytes().to_vec());
    }
    Err(JmapError::NotFound(format!("blob {}", blob_id)))
}
