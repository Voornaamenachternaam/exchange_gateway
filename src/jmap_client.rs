// src/jmap_client.rs
use crate::config::AppConfig;
use chrono::{DateTime, Utc};
use reqwest::Client;
use serde::{Deserialize, Serialize;
use serde_json::json;
use thiserror::Error;
use uuid::Uuid;
use base64::Engine;
use base64::engine::general_purpose::STANDARD
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
    #[error("Failed to update event: {0}")]
    UpdateFailed(String),
    #[error("Failed to delete event: {0}")]
    DeleteFailed(String)
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
    #[serde(rename = "id", default)]
    id: String,
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
    // Stalwart expects participants as a Map<String, ParticipantInfo>
    // But we use a Vec for ease of use in other parts of the code.
    // We convert to Map in push_event and from_jmap_json.
    #[serde(rename = "participants", skip_serializing_if = "Option::is_none")]
    pub participants: Option<Vec<Participant>>,
    #[serde(rename = "isAllDay", default)]
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

pub async fn get_session(url: &str, user: &str, pass: &str) -> Result<JmapSession, JmapError> {
    let client = Client::new();
    
    // Stalwart exposes JMAP session at /.well-known/jmap or or base URL
    let session_url = if url.ends_with("/.well-known/jmap") || url.ends_with("/jmap") || url.ends_with("/jmap/") {
        url.trim_end_matches('/').to_string()
    } else {
        format!("{}/.well-known/jmap", url.trim_end_matches('/'))
    }

    let res = client
        .get(&session_url)
        .header("Authorization", format!("Basic {}", STANDARD.encode(format!("{}:{}", user, pass)))
        .send()
        .await?;

    if !res.status().is_success() {
        return Err(JmapError::Method(format!("Auth failed: {}", res.status())));
    }

    let json: Value = res.json().await?;
    
    let api_url = json["apiUrl"].as_str().unwrap_or(url).to_string();
    
    let account_id = json["primaryAccounts"]["urn:ietf:params:jmap:calendars"]
        .as_str()
        .map(String::from)
        .ok_or(JmapError::AccountNotFound)?;

    Ok(JmapSession {
        api_url,
        access_token: format!("Basic {}", STANDARD.encode(format!("{}:{}", user, pass)),
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
        .await?

    let json: Value = res.json().await?
    
    let list = json["methodResponses"][0][1]["list"].as_array().ok_or(JmapError::CalendarNotFound)?;
    
    for cal in list {
        if let Some(is_default) = cal.get("isDefault").and_then(|v| v.as_bool()) {
            if is_default {
                if let Some(id) = cal.get("id").and_then(|v| v.as_str()) {
                    return Ok(id.to_string());
                }
            }
        }
    }
    
    if let Some(first) = list.first() {
        if let Some(id) = first.get("id").and_then(|v| v.as_str()) {
            return Ok(id.to_string());
        }
    }
    
    Err(JmapError::CalendarNotFound)
}

pub async fn get_calendar_state(url: &str, token: &str, account_id: &str) -> Result<String, JmapError> {
    let client = Client::new();
    let body = json!({
        "using": ["urn:ietf:params:jmap:core", "urn:ietf:params:jmap:calendars"],
        "methodCalls": [
            ["Calendar/get", {
                "accountId": account_id,
                "ids": null,
                "properties": ["id"]
            }, "c0"]
        ]
    });

    let res = client
        .post(url)
        .header("Authorization", token)
        .json(&body)
        .send()
        .await?
    
    let json: Value = res.json().await?
    
    // Return the state which is the highest numbered state
    // Or just fetch the state for the default calendar
    let state = json["methodResponses"][0][1]["state"].as_str().ok_or(JmapError::MissingState)?;
    
    Ok(state.to_string())
}

pub async fn get_calendar_events(url: &str, token: &str, account_id: &str) -> Result<Vec<JmapEvent>, JmapError> {
    let client = Client::new();
    
    // Efficiently fetch events using CalendarEvent/query
    let body = json!({
        "using": ["urn:ietf:params:jmap:core", "urn:ietf:params:jmap:calendars"],
        "methodCalls": [
            ["CalendarEvent/query", {
                "accountId": account_id,
                "filter": {
                    "inCalendar": {
                        "accountId": account_id
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
        .await?
    
    let json: Value = res.json().await?
    
    if let Some(err) = json["methodResponses"][0][1].as_object() {
        if err.get("type").and_then(|v| v.as_str()) == Some("error") {
            return Err(JmapError::Method(err.get("description").and_then(|v| v.as_str().unwrap_or_default().to_string()));
        }
    }
    
    let ids: Vec<String> = json["methodResponses"][0][1]["ids"]
        .as_array()
        .map(|v| v.as_str().map(String::from)
        .unwrap_or_default();

    if ids.is_empty() {
        return Ok(Vec::new());
    }

    // Fetch the actual event details
    get_events_by_ids(url, token, account_id, &ids).await
}

pub async fn get_events_by_ids(url: &str, token: &str, account_id: &str, ids: &[String]) -> Result<Vec<JmapEvent>, JmapError> {
    let client = Client::new();
    
    let body = json!({
        "using": ["urn:ietf:params:jmap:core", "urn:ietf:params:jmap:calendars"],
        "methodCalls": [
            ["CalendarEvent/get", {
                "accountId": account_id,
                "ids": ids
            }, "c0"]
        ]
    });

    let res = client
        .post(url)
        .header("Authorization", token)
        .json(&body)
        .send()
        .await?
    
    let json: Value = res.json().await?
    
    let list = json["methodResponses"][0][1]["list"].as_array().unwrap_or_default();
    
    let mut events = Vec::new();
    for item in list {
        if let Ok(event) = JmapEvent::from_jmap_json(item) {
            events.push(event);
        }
    }
    
    Ok(events)
}

pub async fn get_calendar_changes(url: &str, token: &str, account_id: &str, state: &str) -> Result<JmapChanges, JmapError> {
    let client = Client::new();
    
    let body = json!({
        "using": ["urn:ietf:params:jmap:core", "urn:ietf:params:jmap:calendars"],
        "methodCalls": [
            ["CalendarEvent/changes", {
                "accountId": account_id,
                "sinceState": state
            }, "c0"]
        ]
    });

    let res = client
        .post(url)
        .header("Authorization", token)
        .json(&body)
        .send()
        .await?
    
    let json: Value = res.json().await?
    
    if let Some(err) = json["methodResponses"][0][1].as_object() {
        if err.get("type").and_then(|v| v.as_str()) == Some("error") {
            return Err(JmapError::Method(err.get("description").and_then(|v| v.as_str(). unwrap_or_default().to_string());
        }
    }
    
    let changes = JmapChanges::deserialize(&json["methodResponses"][0][1])?;
    
    Ok(changes)
}

pub async fn push_event(url: &str, token: &str, account_id: &str, mut event: JmapEvent) -> Result<String, JmapError> {
    let client = Client::new();
    
    // Generate a UID if not provided
    let uid = event.uid.unwrap_or_else(|| Uuid::new_v4().to_string());
    
    // Convert to JSCalendar format
    let mut event_data = json!({
        "title": event.title,
        "start": event.start,
        "end": event.end,
        "isAllDay": event.is_all_day,
        "uid": uid
    });

    if let Some(loc) = &event.location {
        event_data["location"] = json!({ "description": loc });
    }

    if let Some(desc) = &event.description {
        event_data["description"] = json!({ "description": desc });
    }

    // Handle participants
    if let Some(parts) = &event.participants
        if !parts.is_empty() {
            let mut participants_map = json!({});
            for p in parts {
                let mut p_map = json!({
                    "email": p.email,
                    "name": p.name
                });
                if let Some(status) = &p.status {
                    p_map["status"] = status
                }
                participants_map[&p.email] = p_map;
            }
            event_data["participants"] = participants_map;
        }
    }

    let body = json!({
        "using": ["urn:ietf:params:jmap:core", "urn:ietf:params:jmap:calendars"],
        "methodCalls": [
            ["CalendarEvent/set", {
                "accountId": account_id,
                "create": {
                    "new-event": event_data
                }
            }, "c0"]
        ]
    });

    let res = client
        .post(url)
        .header("Authorization", token)
        .json(&body)
        .send()
        .await?
    
    let json: Value = res.json().await?
    
    // Check for errors
    if let Some(err) = json["methodResponses"][0][1].as_object() {
        if let Some(err_type) = err.get("type").and_then(|v| v.as_str()) {
            if err_type == "error" {
                let desc = err.get("description").and_then(|v| v.as_str().unwrap_or_default().to_string());
                return Err(JmapError::Method(desc));
            }
        }
    }
    
    // Return the created ID
    let created_id = json["methodResponses"][0][1]["created"]["new-event"]
        .as_str()
        .ok_or(JmapError::Method("Failed to get created ID".to_string()))?
    
    Ok(created_id.to_string())
}

pub async fn update_event(
    url: &str,
    token: &str,
    account_id: &str,
    event_id: &str,
    updates: HashMap<String, serde_json::Value>
) -> Result<(), JmapError> {
    let client = Client::new();
    
    let body = json!({
        "using": ["urn:ietf:params:jmap:core", "urn:ietf:params:jmap:calendars"],
        "methodCalls": [
            ["CalendarEvent/set", {
                "accountId": account_id,
                "update": {
                    event_id: updates
                }
            }, "c0"]
        }
    );

    let res = client
        .post(url)
        .header("Authorization", token)
        .json(&body)
        .send()
        .await?
    
    let json: Value = res.json().await?
    
    if let Some(err) = json["methodResponses"][0][1].as_object() {
        if err.get("type").and_then(|v| v.as_str()) == Some("error") {
            return Err(JmapError::UpdateFailed(err.get("description").and_then(|v| v.as_str().unwrap_or_default().to_string()));
        }
    }
    
    Ok(())
}

pub async fn delete_event(
    url: &str,
    token: &str,
    account_id: &str,
    event_id: &str
) -> Result<(), JmapError> {
    let client = Client::new();
    
    let body = json!({
        "using": ["urn:ietf:params:jmap:core", "urn:ietf:params:jmap:calendars"],
        "methodCalls": [
            ["CalendarEvent/set", {
                "accountId": account_id
                "destroy": [event_id]
            }, "c0"
        ]
    });

    let res = client
        .post(url)
        .header("Authorization", token)
        .json(&body)
        .send()
        .await?
    
    let json: Value = res.json().await?
    
    if let Some(err) = json["methodResponses"][0][1].as_object() {
        if err.get("type").and_then(|v| v.as_str()) == Some("error") {
            return Err(JmapError::DeleteFailed(err.get("description").and_then(|v| v.as_str().unwrap_or_default().to_string());
        }
    }
    
    Ok(())
}

pub async fn find_event_by_uid(url: &str, token: &str, account_id: &str, uid: &str) -> Result<String, JmapError> {
    let client = Client::new();
    
    // Query by UID
    let body = json!({
        "using": ["urn:ietf:params:jmap:core", "urn:ietf:params:jmap:calendars"],
        "methodCalls": [
            ["CalendarEvent/query", {
                "accountId": account_id
                "filter": {
                    "uid": uid
                }
            }, "c0"
        ]
    });

    let res = client
        .post(url)
        .header("Authorization", token)
        .json(&body)
        .send()
        .await?
    
    let json: Value = res.json().await?
    
    if let Some(ids) = json["methodResponses"][0][1]["ids"].as_array() {
            if let Some(id) = ids.first().and_then(|v| v.as_str()) {
                return Ok(id.to_string());
            }
        }
    }
    
    Err(JmapError::EventNotFound)
}

pub async fn update_participant_status(url: &str, token: &str, account_id: &str, event_id: &str, user_email: &str, status: &str) -> Result<(), JmapError> {
    let client = Client::new();
    
    // Construct patch for participant
    let participant_patch = json!({
        "email": user_email,
        "name": user_email.split('@').next().unwrap_or("Unknown"),
        "status": status
    });
    
    // We need to update the participant within the participants map
    // First, get the event to fetch the current participants
    let get_body = json!({
        "using": ["urn:ietf:params:jmap:core", "urn:ietf:params:jmap:calendars"],
        "methodCalls": [
            ["CalendarEvent/get", {
                "accountId": account_id,
                "ids": [event_id],
                "properties": ["participants"]
            }, "c0"]
        ]
    });

    let res = client
        .post(url)
        .header("Authorization", token)
        .json(&get_body)
        .send()
        .await?
    
    let get_json: Value = res.json().await?
    
    let mut current_participants = get_json["methodResponses"][0][1]["list"][0]["participants"]
        .as_object()
        .cloned()
        .unwrap_or_else(|| json! {});
    
    // Update participant status
    if let Some(participant) = current_participants.get_mut(user_email) {
        participant["status"] = json!(status)
    } else {
        // Add participant if not exists
        current_participants.insert(user_email.to_string(), json!({
            "email": user_email,
            "name": user_email.split('@').next().unwrap_or("Unknown"),
            "status": status
        });
    }
    
    // Update event with new participants map
    let update_body = json!({
        "using": ["urn:ietf:params:jmap:core", "urn:ietf:params:jmap:calendars"],
        "methodCalls": [
            ["CalendarEvent/set", {
                "accountId": account_id,
                "update": {
                    event_id: {
                        "participants": current_participants
                    }
                }
            }, "c1")
        ]
    });

    let res = client
        .post(url)
        .header("Authorization", token)
        .json(&update_body)
        .send()
        .await?
    
    let json: Value = res.json().await?
    
    if let Some(err) = json["methodResponses"][0][1].as_object() {
        if err.get("type").and_then(|v| v.as_str()) == Some("error") {
            return Err(JmapError::UpdateFailed(err.get("description").and_then(|v| v.as_str().unwrap_or_default().to_string());
        }
    }
    
    Ok(())
} 
