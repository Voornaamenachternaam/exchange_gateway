use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::json;
use uuid::Uuid;

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
                let map: HashMap<&str, ParticipantValue<'_>> = participants
                    .iter()
                    .map(|p| {
                        (
                            p.email.as_str(),
                            ParticipantValue {
                                name: &p.name,
                                status: &p.status,
                            },
                        )
                    })
                    .collect();
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

pub async fn get_session(jmap_url: &str, user: &str, pass: &str) -> Result<JmapSession, String> {
    let client = Client::new();
    let token = base64::Engine::encode(
        &base64::engine::general_purpose::STANDARD,
        format!("{}:{}", user, pass),
    );
    let res = client
        .get(jmap_url)
        .header("Authorization", format!("Basic {}", token))
        .send()
        .await
        .map_err(|e| format!("Connection: {}", e))?;
    if !res.status().is_success() {
        return Err(format!("Auth Failed: {}", res.status()));
    }
    let body: serde_json::Value = res.json().await.map_err(|e| format!("JSON: {}", e))?;
    let account_id = body["primaryAccounts"]["urn:ietf:params:jmap:calendars"]
        .as_str()
        .ok_or("Missing Calendar Account ID")?
        .to_string();
    Ok(JmapSession {
        api_url: body["apiUrl"].as_str().unwrap_or(jmap_url).to_string(),
        access_token: token,
        account_id,
        client,
    })
}

pub async fn get_default_calendar_id(session: &JmapSession) -> Result<String, String> {
    let body = json!({ "using": ["urn:ietf:params:jmap:core", "urn:ietf:params:jmap:calendars"], "methodCalls": [["Calendar/get", { "accountId": session.account_id, "ids": null }, "c0"]] });
    let res = session
        .client
        .post(&session.api_url)
        .header("Authorization", format!("Basic {}", session.access_token))
        .json(&body)
        .send()
        .await
        .map_err(|e| e.to_string())?;
    let json: serde_json::Value = res.json().await.map_err(|e| e.to_string())?;
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
                .ok_or("Missing ID".into());
        }
    }
    Err("No Calendars".into())
}

pub async fn get_calendar_state(session: &JmapSession) -> Result<String, String> {
    let body = json!({ "using": ["urn:ietf:params:jmap:core", "urn:ietf:params:jmap:calendars"], "methodCalls": [["CalendarEvent/get", { "accountId": session.account_id, "ids": [] }, "c0"]] });
    let res = session
        .client
        .post(&session.api_url)
        .header("Authorization", format!("Basic {}", session.access_token))
        .json(&body)
        .send()
        .await
        .map_err(|e| e.to_string())?;
    let json: serde_json::Value = res.json().await.map_err(|e| e.to_string())?;
    json["methodResponses"][0][1]["state"]
        .as_str()
        .map(String::from)
        .ok_or("Missing state".into())
}

pub async fn get_calendar_events(session: &JmapSession) -> Result<Vec<JmapEvent>, String> {
    let body = json!({ "using": ["urn:ietf:params:jmap:core", "urn:ietf:params:jmap:calendars"], "methodCalls": [["CalendarEvent/get", { "accountId": session.account_id, "ids": null, "properties": ["id", "title", "start", "end", "location", "description", "uid", "participants", "isAllDay", "recurrenceRule", "updated"] }, "c0"]] });
    let res = session
        .client
        .post(&session.api_url)
        .header("Authorization", format!("Basic {}", session.access_token))
        .json(&body)
        .send()
        .await
        .map_err(|e| e.to_string())?;
    let json: serde_json::Value = res.json().await.map_err(|e| e.to_string())?;
    let events: Vec<JmapEvent> =
        serde_json::from_value(json["methodResponses"][0][1]["list"].clone())
            .map_err(|e| format!("Event deserialization failed: {}", e))?;
    Ok(events)
}

pub async fn get_events_by_ids(
    session: &JmapSession,
    ids: &[String],
) -> Result<Vec<JmapEvent>, String> {
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
        .await
        .map_err(|e| e.to_string())?;
    let json: serde_json::Value = res.json().await.map_err(|e| e.to_string())?;
    let events: Vec<JmapEvent> =
        serde_json::from_value(json["methodResponses"][0][1]["list"].clone())
            .map_err(|e| format!("Event deserialization failed: {}", e))?;
    Ok(events)
}

pub async fn get_event_by_id(session: &JmapSession, id: &str) -> Result<JmapEvent, String> {
    let events = get_events_by_ids(session, &[id.to_string()]).await?;
    events.into_iter().next().ok_or("Event not found".into())
}

pub async fn push_event(session: &JmapSession, event: JmapEvent) -> Result<String, String> {
    let create_map = json!({ Uuid::new_v4().to_string(): event });
    let body = json!({ "using": ["urn:ietf:params:jmap:core", "urn:ietf:params:jmap:calendars"], "methodCalls": [["CalendarEvent/set", { "accountId": session.account_id, "create": create_map }, "c0"]] });
    let res = session
        .client
        .post(&session.api_url)
        .header("Authorization", format!("Basic {}", session.access_token))
        .json(&body)
        .send()
        .await
        .map_err(|e| e.to_string())?;
    let json: serde_json::Value = res.json().await.map_err(|e| e.to_string())?;
    if let Some(created) = json["methodResponses"][0][1]["created"].as_object()
        && let Some((_, val)) = created.into_iter().next()
    {
        return val["id"]
            .as_str()
            .map(String::from)
            .ok_or("Create Failed: missing server id".into());
    }
    Err("Create Failed".into())
}

fn check_jmap_set_errors(json: &serde_json::Value) -> Result<(), String> {
    let resp = &json["methodResponses"][0];
    if resp[0].as_str() == Some("error") {
        let desc = resp[1]["description"]
            .as_str()
            .unwrap_or("Unknown JMAP error");
        return Err(format!("JMAP error: {}", desc));
    }
    Ok(())
}

pub async fn patch_event(
    session: &JmapSession,
    id: &str,
    patch: serde_json::Map<String, serde_json::Value>,
) -> Result<(), String> {
    let update_map = json!({ id: patch });
    let body = json!({ "using": ["urn:ietf:params:jmap:core", "urn:ietf:params:jmap:calendars"], "methodCalls": [["CalendarEvent/set", { "accountId": session.account_id, "update": update_map }, "c0"]] });
    let res = session
        .client
        .post(&session.api_url)
        .header("Authorization", format!("Basic {}", session.access_token))
        .json(&body)
        .send()
        .await
        .map_err(|e| e.to_string())?;
    let json: serde_json::Value = res.json().await.map_err(|e| e.to_string())?;
    check_jmap_set_errors(&json)?;
    if let Some(not_updated) = json["methodResponses"][0][1]["notUpdated"].as_object()
        && !not_updated.is_empty()
    {
        let desc = not_updated
            .values()
            .next()
            .and_then(|v| v["description"].as_str())
            .unwrap_or("Unknown error");
        return Err(format!("Update failed: {}", desc));
    }
    Ok(())
}

pub async fn destroy_events(session: &JmapSession, ids: Vec<String>) -> Result<(), String> {
    let body = json!({ "using": ["urn:ietf:params:jmap:core", "urn:ietf:params:jmap:calendars"], "methodCalls": [["CalendarEvent/set", { "accountId": session.account_id, "destroy": ids }, "c0"]] });
    let res = session
        .client
        .post(&session.api_url)
        .header("Authorization", format!("Basic {}", session.access_token))
        .json(&body)
        .send()
        .await
        .map_err(|e| e.to_string())?;
    let json: serde_json::Value = res.json().await.map_err(|e| e.to_string())?;
    check_jmap_set_errors(&json)?;
    if let Some(not_destroyed) = json["methodResponses"][0][1]["notDestroyed"].as_object()
        && !not_destroyed.is_empty()
    {
        let desc = not_destroyed
            .values()
            .next()
            .and_then(|v| v["description"].as_str())
            .unwrap_or("Unknown error");
        return Err(format!("Destroy failed: {}", desc));
    }
    Ok(())
}

pub async fn get_calendar_changes(
    session: &JmapSession,
    since: &str,
) -> Result<JmapChanges, String> {
    let body = json!({ "using": ["urn:ietf:params:jmap:core", "urn:ietf:params:jmap:calendars"], "methodCalls": [["CalendarEvent/changes", { "accountId": session.account_id, "sinceState": since }, "c0"]] });
    let res = session
        .client
        .post(&session.api_url)
        .header("Authorization", format!("Basic {}", session.access_token))
        .json(&body)
        .send()
        .await
        .map_err(|e| e.to_string())?;
    let json: serde_json::Value = res.json().await.map_err(|e| e.to_string())?;
    serde_json::from_value(json["methodResponses"][0][1].clone())
        .map_err(|e| format!("Changes Parse: {}", e))
}

pub async fn search_principals(
    session: &JmapSession,
    query: &str,
) -> Result<Vec<Principal>, String> {
    let body = json!({ "using": ["urn:ietf:params:jmap:core", "urn:ietf:params:jmap:principals"], "methodCalls": [["Principal/query", { "accountId": session.account_id, "filter": { "operator": "OR", "conditions": [{ "email": query }, { "name": query }] } }, "c0"], ["Principal/get", { "accountId": session.account_id, "#ids": { "resultOf": "c0", "name": "Principal/query", "path": "/ids" } }, "c1"]] });
    let res = session
        .client
        .post(&session.api_url)
        .header("Authorization", format!("Basic {}", session.access_token))
        .json(&body)
        .send()
        .await
        .map_err(|e| e.to_string())?;
    let json: serde_json::Value = res.json().await.map_err(|e| e.to_string())?;
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

pub async fn find_event_by_uid(session: &JmapSession, uid: &str) -> Result<String, String> {
    let body = json!({ "using": ["urn:ietf:params:jmap:core", "urn:ietf:params:jmap:calendars"], "methodCalls": [["CalendarEvent/query", { "accountId": session.account_id, "filter": { "uid": uid } }, "c0"], ["CalendarEvent/get", { "accountId": session.account_id, "#ids": { "resultOf": "c0", "name": "CalendarEvent/query", "path": "/ids" } }, "c1"]] });
    let res = session
        .client
        .post(&session.api_url)
        .header("Authorization", format!("Basic {}", session.access_token))
        .json(&body)
        .send()
        .await
        .map_err(|e| e.to_string())?;
    let json: serde_json::Value = res.json().await.map_err(|e| e.to_string())?;
    json["methodResponses"][1][1]["list"][0]["id"]
        .as_str()
        .map(String::from)
        .ok_or("Not Found".into())
}

fn is_valid_email_for_path(email: &str) -> bool {
    !email.is_empty()
        && !email.contains('\\')
        && !email.contains('\0')
        && email.contains('@')
        && email.len() <= 254
}

pub async fn update_participant_status(
    session: &JmapSession,
    event_id: &str,
    user_email: &str,
    status: &str,
) -> Result<(), String> {
    if !is_valid_email_for_path(user_email) {
        return Err(format!("Invalid email for participant path: {}", user_email));
    }
    let patch = json!({ format!("participants/{}/participationStatus", user_email.replace('~', "~0").replace('/', "~1")): status });
    let body = json!({ "using": ["urn:ietf:params:jmap:core", "urn:ietf:params:jmap:calendars"], "methodCalls": [["CalendarEvent/set", { "accountId": session.account_id, "update": { event_id: patch } }, "c0"]] });
    let res = session
        .client
        .post(&session.api_url)
        .header("Authorization", format!("Basic {}", session.access_token))
        .json(&body)
        .send()
        .await
        .map_err(|e| e.to_string())?;
    let json: serde_json::Value = res.json().await.map_err(|e| e.to_string())?;
    check_jmap_set_errors(&json)?;
    if let Some(not_updated) = json["methodResponses"][0][1]["notUpdated"].as_object()
        && !not_updated.is_empty()
    {
        let desc = not_updated
            .values()
            .next()
            .and_then(|v| v["description"].as_str())
            .unwrap_or("Unknown error");
        return Err(format!("Update failed: {}", desc));
    }
    Ok(())
}

pub async fn get_blob(session: &JmapSession, blob_id: &str) -> Result<Vec<u8>, String> {
    let body = json!({ "using": ["urn:ietf:params:jmap:core", "urn:ietf:params:jmap:blob"], "methodCalls": [["Blob/get", { "accountId": session.account_id, "ids": [blob_id] }, "c0"]] });
    let res = session
        .client
        .post(&session.api_url)
        .header("Authorization", format!("Basic {}", session.access_token))
        .json(&body)
        .send()
        .await
        .map_err(|e| e.to_string())?;
    let json: serde_json::Value = res.json().await.map_err(|e| e.to_string())?;
    if let Some(b64) = json["methodResponses"][0][1]["list"][0]["data:asBase64"].as_str() {
        return base64::Engine::decode(&base64::engine::general_purpose::STANDARD, b64)
            .map_err(|e| e.to_string());
    }
    if let Some(text) = json["methodResponses"][0][1]["list"][0]["data:asText"].as_str() {
        return Ok(text.as_bytes().to_vec());
    }
    Err("Blob not found".into())
}
