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
    #[serde(rename = "participationStatus", skip_serializing_if = "Option::is_none")]
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
                    let key = p
                        .participant_id
                        .as_deref()
                        .unwrap_or("")
                        .to_string();
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
                    let email = email_from_send_to(&p.send_to)
                        .unwrap_or_else(|| id.clone());
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
fn find_account_for_capability<'a>(body: &'a serde_json::Value, capability: &str) -> Option<&'a str> {
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
    if !res.status().is_success() {
        let status = res.status();
        return Err(if status == reqwest::StatusCode::UNAUTHORIZED
            || status == reqwest::StatusCode::FORBIDDEN
        {
            JmapError::Auth(format!("HTTP {}", status))
        } else {
            JmapError::Api(format!("HTTP {}", status))
        });
    }
    let body: serde_json::Value = res.json().await?;
    let account_id = find_account_for_capability(&body, "urn:ietf:params:jmap:calendars")
        .ok_or_else(|| JmapError::Parse("no usable account in JMAP session".into()))?
        .to_string();
    let principals_account_id =
        find_account_for_capability(&body, "urn:ietf:params:jmap:principals")
            .unwrap_or(&account_id)
            .to_string();
    Ok(JmapSession {
        api_url: body["apiUrl"].as_str().unwrap_or(jmap_url).to_string(),
        access_token: token,
        account_id,
        principals_account_id,
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
    let events: Vec<JmapEvent> =
        serde_json::from_value(list)
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
    let events: Vec<JmapEvent> =
        serde_json::from_value(list)
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
}

/// Create multiple calendar events in a single JMAP `CalendarEvent/set` call.
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
        });
    }

    let mut create_map = serde_json::Map::new();
    // Map from JMAP creation ID (UUID) → caller creation ID so we can
    // correlate the response back.
    let mut id_map: std::collections::HashMap<String, String> = std::collections::HashMap::new();

    for (caller_id, mut event) in events {
        if event.uid.is_none() {
            event.uid = Some(Uuid::new_v4().to_string());
        }
        let mut event_json = serde_json::to_value(&event)
            .map_err(|e| JmapError::Parse(format!("serialize failed: {}", e)))?;
        if let Some(obj) = event_json.as_object_mut() {
            obj.insert("calendarIds".to_string(), json!({ (calendar_id): true }));
        }
        let jmap_creation_id = Uuid::new_v4().to_string();
        id_map.insert(jmap_creation_id.clone(), caller_id);
        create_map.insert(jmap_creation_id, event_json);
    }

    let body = json!({
        "using": ["urn:ietf:params:jmap:core", "urn:ietf:params:jmap:calendars"],
        "methodCalls": [["CalendarEvent/set", {
            "accountId": session.account_id,
            "create": create_map
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
    let json: serde_json::Value = res.json().await?;
    check_jmap_method_error(&json)?;

    let mut result = BatchCreateResult {
        created: Vec::new(),
        not_created: Vec::new(),
    };

    if let Some(created) = json["methodResponses"][0][1]["created"].as_object() {
        for (jmap_id, val) in created {
            if let Some(caller_id) = id_map.remove(jmap_id) {
                let server_id = val["id"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string();
                let updated = val["updated"].as_str().map(String::from);
                result
                    .created
                    .push((caller_id, CreatedEvent { id: server_id, updated }));
            }
        }
    }

    if let Some(not_created) = json["methodResponses"][0][1]["notCreated"].as_object() {
        for (jmap_id, err) in not_created {
            if let Some(caller_id) = id_map.remove(jmap_id) {
                let desc = err["description"]
                    .as_str()
                    .unwrap_or("unknown error")
                    .to_string();
                result.not_created.push((caller_id, desc));
            }
        }
    }

    // Any remaining IDs in id_map were neither created nor explicitly rejected
    // — treat them as failures.
    for (_, caller_id) in id_map {
        result
            .not_created
            .push((caller_id, "no response from server".to_string()));
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
                let desc = resp
                    .get(1)
                    .and_then(|e| e.get("description"))
                    .and_then(|d| d.as_str())
                    .unwrap_or("unknown JMAP error");
                return Err(JmapError::Api(desc.to_string()));
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

/// Batch-update multiple calendar events in a single JMAP `CalendarEvent/set`
/// call.  `updates` maps event ID → patch object.
pub async fn patch_events(
    session: &JmapSession,
    updates: serde_json::Map<String, serde_json::Value>,
) -> Result<(), JmapError> {
    if updates.is_empty() {
        return Ok(());
    }
    let body = json!({ "using": ["urn:ietf:params:jmap:core", "urn:ietf:params:jmap:calendars"], "methodCalls": [["CalendarEvent/set", { "accountId": session.account_id, "update": updates }, "c0"]] });
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
        let ids: Vec<&str> = not_updated.keys().map(|k| k.as_str()).collect();
        let desc = not_updated
            .values()
            .next()
            .and_then(|v| v["description"].as_str())
            .unwrap_or("unknown error");
        return Err(JmapError::Api(format!(
            "update failed for {}: {}",
            ids.join(", "),
            desc
        )));
    }
    Ok(())
}

/// Destroy calendar events by ID.
///
/// Returns a list of IDs that the server refused to destroy (partial
/// failures).  An empty vector means every ID was destroyed successfully.
/// Transport-level and method-level errors still surface as `Err`.
pub async fn destroy_events(
    session: &JmapSession,
    ids: Vec<String>,
) -> Result<Vec<String>, JmapError> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let body = json!({ "using": ["urn:ietf:params:jmap:core", "urn:ietf:params:jmap:calendars"], "methodCalls": [["CalendarEvent/set", { "accountId": session.account_id, "destroy": ids }, "c0"]] });
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
    if let Some(not_destroyed) = json["methodResponses"][0][1]["notDestroyed"].as_object()
        && !not_destroyed.is_empty()
    {
        let failed_ids: Vec<String> = not_destroyed.keys().cloned().collect();
        return Ok(failed_ids);
    }
    Ok(Vec::new())
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
    serde_json::from_value(changes_val)
        .map_err(|e| JmapError::Parse(format!("changes: {}", e)))
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
    // Fetch the event to discover the opaque participant ID that the server
    // uses as the map key for this participant's email (RFC 8984).
    let event = get_event_by_id(session, event_id).await?;
    let participant_key = event
        .participants
        .as_ref()
        .and_then(|ps| {
            ps.iter().find(|p| p.email.eq_ignore_ascii_case(user_email))
        })
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
    let body = json!({ "using": ["urn:ietf:params:jmap:core", "urn:ietf:params:jmap:blob"], "methodCalls": [["Blob/get", { "accountId": session.account_id, "ids": [blob_id], "properties": ["data:asBase64"] }, "c0"]] });
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
