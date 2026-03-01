// src/jmap_client.rs
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use thiserror::Error;
use std::collections::HashMap;

#[derive(Debug, Error)]
pub enum JmapError {
    #[error("HTTP request failed: {0}")]
    Request(#[from] reqwest::Error),
    #[error("JSON processing failed: {0}")]
    Json(#[from] serde_json::Error),
    #[error("JMAP method error: {0}")]
    Method(String),
    #[error("Account not found")]
    AccountNotFound,
    #[error("Calendar not found")]
    CalendarNotFound,
    #[error("Event not found")]
    EventNotFound,
    #[error("Missing state in response")]
    MissingState,
}

#[derive(Debug, Clone)]
pub struct JmapSession {
    pub api_url: String,
    pub access_token: String,
    pub account_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Participant {
    pub email: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct JmapEvent {
    #[serde(rename = "id", skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(rename = "title")]
    pub title: String,
    #[serde(rename = "start")]
    pub start: String, // UTC String
    #[serde(rename = "end")]
    pub end: String,   // UTC String
    #[serde(rename = "location", skip_serializing_if = "Option::is_none")]
    pub location: Option<String>,
    #[serde(rename = "description", skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(rename = "uid", skip_serializing_if = "Option::is_none")]
    pub uid: Option<String>,
    #[serde(rename = "participants", skip_serializing_if = "Option::is_none")]
    pub participants: Option<Vec<Participant>>,
    #[serde(rename = "isAllDay")]
    pub is_all_day: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct JmapChanges {
    #[serde(rename = "newState")]
    pub new_state: String,
    #[serde(rename = "changed")]
    pub updated: Vec<String>,
    #[serde(rename = "removed")]
    pub destroyed: Vec<String>,
}

// Helper to create basic auth header
fn basic_auth(user: &str, pass: &str) -> String {
    let encoded = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, format!("{}:{}", user, pass));
    format!("Basic {}", encoded)
}

pub async fn get_session(url: &str, user: &str, pass: &str) -> Result<JmapSession, JmapError> {
    let client = Client::new();
    
    // Stalwart typically exposes JMAP session at /.well-known/jmap or root
    let session_url = if url.ends_with("/.well-known/jmap") || url.ends_with("/jmap") {
        url.to_string()
    } else {
        format!("{}/.well-known/jmap", url.trim_end_matches('/'))
    };

    let res = client
        .get(&session_url)
        .header("Authorization", basic_auth(user, pass))
        .send()
        .await?;

    if !res.status().is_success() {
        let status = res.status();
        let text = res.text().await.unwrap_or_default();
        return Err(JmapError::Method(format!("Auth failed with status {}: {}", status, text)));
    }

    let body: Value = res.json().await?;
    
    let api_url = body["apiUrl"].as_str().unwrap_or(url).to_string();
    
    // Extract account_id. Stalwart usually puts it in 'accounts' object with keys.
    // We look for the primary account or the first one.
    let account_id = body["primaryAccounts"]["urn:ietf:params:jmap:calendars"]
        .as_str()
        .map(String::from)
        .or_else(|| {
            body["accounts"].as_object().and_then(|accs| {
                accs.keys().next().cloned()
            })
        })
        .ok_or(JmapError::AccountNotFound)?;

    // We reuse the Basic Auth token as the access token for API calls
    let access_token = basic_auth(user, pass);

    Ok(JmapSession {
        api_url,
        access_token,
        account_id,
    })
}

pub async fn get_default_calendar_id(url: &str, token: &str, account_id: &str) -> Result<String, JmapError> {
    let client = Client::new();
    let body = json!({
        "using": ["urn:ietf:params:jmap:core", "urn:ietf:params:jmap:calendars"],
        "methodCalls": [
            ["Calendar/get", {
                "accountId": account_id,
                "ids": null,
                "properties": ["id", "name", "isDefault"]
            }, "c0"]
        ]
    });

    let res = client
        .post(url)
        .header("Authorization", token)
        .json(&body)
        .send()
        .await?;

    let json: Value = res.json().await?;
    
    let list = json["methodResponses"][0][1]["list"].as_array().ok_or(JmapError::CalendarNotFound)?;
    
    // Find default
    for cal in list {
        if cal["isDefault"].as_bool().unwrap_or(false) {
            return cal["id"].as_str().map(String::from).ok_or(JmapError::CalendarNotFound);
        }
    }
    
    // Fallback to first
    list.first()
        .and_then(|c| c["id"].as_str())
        .map(String::from)
        .ok_or(JmapError::CalendarNotFound)
}

pub async fn get_calendar_state(url: &str, token: &str, account_id: &str) -> Result<String, JmapError> {
    let client = Client::new();
    // We use CalendarEvent/get with empty IDs to just get the state efficiently
    let body = json!({
        "using": ["urn:ietf:params:jmap:core", "urn:ietf:params:jmap:calendars"],
        "methodCalls": [
            ["CalendarEvent/get", {
                "accountId": account_id,
                "ids": []
            }, "c0"]
        ]
    });

    let res = client
        .post(url)
        .header("Authorization", token)
        .json(&body)
        .send()
        .await?;

    let json: Value = res.json().await?;
    
    json["methodResponses"][0][1]["state"]
        .as_str()
        .map(String::from)
        .ok_or(JmapError::MissingState)
}

pub async fn get_calendar_events(url: &str, token: &str, account_id: &str) -> Result<Vec<JmapEvent>, JmapError> {
    let client = Client::new();
    
    // Step 1: Query all event IDs
    let query_body = json!({
        "using": ["urn:ietf:params:jmap:core", "urn:ietf:params:jmap:calendars"],
        "methodCalls": [
            ["CalendarEvent/query", {
                "accountId": account_id,
                "filter": {
                    "inCalendar": "calendar-default" // We'll ignore this specific filter for now and rely on default logic or just query all
                }
            }, "c0"]
        ]
    });

    // We need to find the default calendar ID first if not passed, but assume we query generally
    // For simplicity in this context, we query all events in the account (Stalwart handles default view well)
    // Actually, let's query by generic filter or just query.
    // A safer bet is querying without specific filter or using the default calendar ID if known.
    // Since we don't pass calendar ID here, we just query the account.

    let body = json!({
        "using": ["urn:ietf:params:jmap:core", "urn:ietf:params:jmap:calendars"],
        "methodCalls": [
            ["CalendarEvent/query", {
                "accountId": account_id
            }, "c0"]
        ]
    });

    let res = client
        .post(url)
        .header("Authorization", token)
        .json(&body)
        .send()
        .await?;

    let json: Value = res.json().await?;
    let ids: Vec<String> = json["methodResponses"][0][1]["ids"]
        .as_array()
        .map(|arr| arr.iter().filter_map(|id| id.as_str().map(String::from)).collect())
        .unwrap_or_default();

    if ids.is_empty() {
        return Ok(vec![]);
    }

    get_events_by_ids(url, token, account_id, &ids).await
}

pub async fn get_calendar_changes(url: &str, token: &str, account_id: &str, since_state: &str) -> Result<JmapChanges, JmapError> {
    let client = Client::new();
    let body = json!({
        "using": ["urn:ietf:params:jmap:core", "urn:ietf:params:jmap:calendars"],
        "methodCalls": [
            ["CalendarEvent/changes", {
                "accountId": account_id,
                "sinceState": since_state
            }, "c0"]
        ]
    });

    let res = client
        .post(url)
        .header("Authorization", token)
        .json(&body)
        .send()
        .await?;

    let json: Value = res.json().await?;
    
    serde_json::from_value(json["methodResponses"][0][1].clone()).map_err(JmapError::Json)
}

pub async fn get_events_by_ids(url: &str, token: &str, account_id: &str, ids: &[String]) -> Result<Vec<JmapEvent>, JmapError> {
    if ids.is_empty() {
        return Ok(vec![]);
    }

    let client = Client::new();
    let body = json!({
        "using": ["urn:ietf:params:jmap:core", "urn:ietf:params:jmap:calendars"],
        "methodCalls": [
            ["CalendarEvent/get", {
                "accountId": account_id,
                "ids": ids,
                "properties": ["id", "title", "start", "end", "location", "description", "uid", "participants", "isAllDay"]
            }, "c0"]
        ]
    });

    let res = client
        .post(url)
        .header("Authorization", token)
        .json(&body)
        .send()
        .await?;

    let json: Value = res.json().await?;
    
    let list = json["methodResponses"][0][1]["list"].as_array().ok_or(JmapError::EventNotFound)?;
    
    let events = list.iter().filter_map(|e| {
        // JSCalendar participants is a Map, we convert to Vec<Participant>
        let participants = e["participants"].as_object().map(|p_map| {
            p_map.iter().filter_map(|(_, p_val)| {
                Some(Participant {
                    email: p_val["email"].as_str()?.to_string(),
                    name: p_val["name"].as_str().unwrap_or("").to_string(),
                    status: p_val["participationStatus"].as_str().map(String::from),
                })
            }).collect()
        });

        Some(JmapEvent {
            id: e["id"].as_str().map(String::from),
            title: e["title"].as_str().unwrap_or("").to_string(),
            start: e["start"].as_str().unwrap_or("").to_string(),
            end: e["end"].as_str().unwrap_or("").to_string(),
            location: e["location"].as_str().map(String::from),
            description: e["description"].as_str().map(String::from),
            uid: e["uid"].as_str().map(String::from),
            participants,
            is_all_day: e["isAllDay"].as_bool().unwrap_or(false),
        })
    }).collect();

    Ok(events)
}

pub async fn push_event(url: &str, token: &str, account_id: &str, event: JmapEvent) -> Result<String, JmapError> {
    let client = Client::new();
    
    // Convert Vec<Participant> to JSCalendar Map
    let mut participants_map = Map::new();
    if let Some(parts) = &event.participants {
        for p in parts {
            let mut p_val = Map::new();
            p_val.insert("email".to_string(), json!(p.email));
            p_val.insert("name".to_string(), json!(p.name));
            if let Some(s) = &p.status {
                p_val.insert("participationStatus".to_string(), json!(s));
            }
            participants_map.insert(p.email.clone(), Value::Object(p_val));
        }
    }

    let create_id = uuid::Uuid::new_v4().to_string();
    let body = json!({
        "using": ["urn:ietf:params:jmap:core", "urn:ietf:params:jmap:calendars"],
        "methodCalls": [
            ["CalendarEvent/set", {
                "accountId": account_id,
                "create": {
                    create_id: {
                        "title": event.title,
                        "start": event.start,
                        "end": event.end,
                        "location": event.location,
                        "description": event.description,
                        "uid": event.uid,
                        "participants": if participants_map.is_empty() { Value::Null } else { Value::Object(participants_map) },
                        "isAllDay": event.is_all_day
                    }
                }
            }, "c0"]
        ]
    });

    let res = client
        .post(url)
        .header("Authorization", token)
        .json(&body)
        .send()
        .await?;

    let json: Value = res.json().await?;
    
    json["methodResponses"][0][1]["created"][&create_id]["id"]
        .as_str()
        .map(String::from)
        .ok_or_else(|| {
            let err = json["methodResponses"][0][1]["notCreated"][&create_id].clone();
            JmapError::Method(format!("Creation failed: {:?}", err))
        })
}

pub async fn update_event(url: &str, token: &str, account_id: &str, id: &str, patch: Map<String, Value>) -> Result<(), JmapError> {
    let client = Client::new();
    
    // Convert "participants" if it exists in the patch (it shouldn't from active_sync.rs currently, but just in case)
    // active_sync.rs passes a flat map of properties.
    
    let body = json!({
        "using": ["urn:ietf:params:jmap:core", "urn:ietf:params:jmap:calendars"],
        "methodCalls": [
            ["CalendarEvent/set", {
                "accountId": account_id,
                "update": {
                    id: patch
                }
            }, "c0"]
        ]
    });

    let res = client
        .post(url)
        .header("Authorization", token)
        .json(&body)
        .send()
        .await?;

    let json: Value = res.json().await?;
    
    if let Some(err) = json["methodResponses"][0][1]["notUpdated"][id].as_object() {
        return Err(JmapError::Method(format!("Update failed: {:?}", err)));
    }

    Ok(())
}

pub async fn delete_event(url: &str, token: &str, account_id: &str, id: &str) -> Result<(), JmapError> {
    let client = Client::new();
    let body = json!({
        "using": ["urn:ietf:params:jmap:core", "urn:ietf:params:jmap:calendars"],
        "methodCalls": [
            ["CalendarEvent/set", {
                "accountId": account_id,
                "destroy": [id]
            }, "c0"]
        ]
    });

    let res = client
        .post(url)
        .header("Authorization", token)
        .json(&body)
        .send()
        .await?;

    let json: Value = res.json().await?;
    
    if let Some(err) = json["methodResponses"][0][1]["notDestroyed"][id].as_object() {
        return Err(JmapError::Method(format!("Delete failed: {:?}", err)));
    }

    Ok(())
}

pub async fn find_event_by_uid(url: &str, token: &str, account_id: &str, uid: &str) -> Result<String, JmapError> {
    let client = Client::new();
    let body = json!({
        "using": ["urn:ietf:params:jmap:core", "urn:ietf:params:jmap:calendars"],
        "methodCalls": [
            ["CalendarEvent/query", {
                "accountId": account_id,
                "filter": {
                    "uid": uid
                }
            }, "c0"]
        ]
    });

    let res = client
        .post(url)
        .header("Authorization", token)
        .json(&body)
        .send()
        .await?;

    let json: Value = res.json().await?;
    
    let ids = json["methodResponses"][0][1]["ids"].as_array();
    
    ids.and_then(|arr| arr.first())
        .and_then(|id| id.as_str())
        .map(String::from)
        .ok_or(JmapError::EventNotFound)
}

pub async fn update_participant_status(url: &str, token: &str, account_id: &str, event_id: &str, user_email: &str, status: &str) -> Result<(), JmapError> {
    // In JSCalendar, participants is a map. We need to update a specific key.
    // JMAP "update" supports patching specific paths using JSPath or map updates.
    // The simplest way is to update the participants map entry directly.
    
    // Construct patch to update participants[email]/participationStatus
    // JMAP set does not support deep JSON merge patch automatically for all servers unless specified.
    // Stalwart supports merge semantics in update.
    
    let mut participant_val = Map::new();
    participant_val.insert("participationStatus".to_string(), json!(status));
    
    let mut participants_map = Map::new();
    participants_map.insert(user_email.to_string(), Value::Object(participant_val));
    
    let mut patch = Map::new();
    patch.insert("participants".to_string(), Value::Object(participants_map));
    
    update_event(url, token, account_id, event_id, patch).await
}
