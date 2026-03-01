// src/jmap_client.rs
use crate::config::AppConfig;
use chrono::{DateTime, Utc};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::json;
use thiserror::Error;
use uuid::Uuid;
use base64::Engine;
use base64::engine::general_purpose::STANDARD;

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
    DeleteFailed(String),
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

impl JmapEvent {
    pub fn from_jmap_json(value: serde_json::Value) -> Result<Self, JmapError> {
        let map = value.as_object().ok_or(JmapError::Json(serde::from_value(value)))?;
        
        Ok(Self {
            id: map.get("id").and_then(|v| v.as_str().map(String::from)),
            title: map.get("title").and_then(|v| v.as_str().map(String::from)).unwrap_or_default(),
            start: map.get("start").and_then(|v| v.as_str().map(String::from)).unwrap_or_default(),
            end: map.get("end").and_then(|v| v.as_str().map(String::from)).unwrap_or_default(),
            location: map.get("location").and_then(|v| v.as_str().map(String::from)),
            description: map.get("description").and_then(|v| v.as_str().map(String::from)),
            uid: map.get("uid").and_then(|v| v.as_str().map(String::from)),
            participants: parse_participants(map.get("participants")),
            is_all_day: map.get("isAllDay").and_then(|v| v.as_bool()).unwrap_or(false),
        })
    }
}

// Parses the JSCalendar 'participants' map (keyed by email) into a Vec of Participant structs.
fn parse_participants(value: Option<&serde_json::Value>) -> Option<Vec<Participant>> {
    value.and_then(|v| {
        let map = v.as_object()?;
        Some(map.iter().filter_map(|(_, p)| {
            let p_map = p.as_object()?;
            Some(Participant {
                email: p_map.get("email")?.as_str()?.unwrap_or_default().to_string(),
                name: p_map.get("name")?.as_str()?.unwrap_or_default().to_string(),
                status: p_map.get("status").and_then(|s| s.as_str().map(String::from)),
            })
        }).collect::<Vec<Participant>>().into()
    })
}

// Helper to create basic auth header
fn basic_auth(user: &str, pass: &str) -> String {
    let encoded = STANDARD.encode(format!("{}:{}", user, pass));
    format!("Basic {}", encoded)
}

// Helper to build participants map for JMAP from a Vec<Participant>
fn build_participants_map(participants: &[Participant]) -> serde_json::Value {
    let mut map = serde_json::Map::new();
    for p in participants {
        let mut p_val = serde_json::Map::new();
        p_val.insert("email".to_string(), json!(p.email));
        p_val.insert("name".to_string(), json!(p.name));
        if let Some(status) = &p.status {
            p_val.insert("status".to_string(), json!(status));
        }
        map.insert(p.email.clone(), serde_json::Value::Object(p_val));
    }
    serde_json::Value::Object(map)
}

pub async fn get_session(url: &str, user: &str, pass: &str) -> Result<JmapSession, JmapError> {
    let client = Client::new();
    
    // Determine session URL - Stalwart uses /.well-known/jmap
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

    let body: serde_json::Value = res.json().await?;
    
    let api_url = body["apiUrl"].as_str().unwrap_or(url).to_string();
    
    // Extract account_id - typically primaryAccounts or first account
    let account_id = body["primaryAccounts"]["urn:ietf:params:jmap:calendars"]
        .as_str()
        .map(String::from)
        .or_else(|| {
            body["accounts"].as_object().and_then(|accs| {
                accs.keys().next().cloned()
            })
        })
        .ok_or(JmapError::AccountNotFound)?;

    // Reuse Basic Auth for API calls
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

    let json: serde_json::Value = res.json().await?;
    
    let list = json["methodResponses"][0][1]["list"]
        .as_array()
        .ok_or(JmapError::CalendarNotFound)?;
    
    // Find default calendar or return the first one
    let default_cal = list.iter()
        .find(|cal| cal["isDefault"].as_bool().unwrap_or(false))
        .map(|cal| cal["id"].as_str().unwrap_or_default().to_string())
        .or_else(|| list.first().and_then(|cal| cal["id"].as_str().map(String::from)))
        .ok_or(JmapError::CalendarNotFound)
    )?;

    Ok(default_cal)
}

pub async fn get_calendar_state(url: &str, token: &str, account_id: &str) -> Result<String, JmapError> {
    let client = Client::new();
    let body = json!({
        "using": ["urn:ietf:params:jmap:core", "urn:ietf:params:jmap:calendars"],
        "methodCalls": [
            ["CalendarEvent/changes", {
                "accountId": account_id,
                "filter": {
                    "inCalendars": [{
                        "position": 0
                    }]
                },
                "sort": []
            }, "c0"]
        ]
    });

    let res = client
        .post(url)
        .header("Authorization", token)
        .json(&body)
        .send()
        .await?;

    let json: serde_json::Value = res.json().await?;
    
    let state = json["methodResponses"][0][1]["newState"]
        .as_str()
        .ok_or(JmapError::MissingState)?
        .map(String::from);

    Ok(state)
}

pub async fn get_calendar_events(url: &str, token: &str, account_id: &str) -> Result<Vec<JmapEvent>, JmapError> {
    // Get default calendar ID first
    let cal_id = get_default_calendar_id(url, token, account_id).await?;
    
    let client = Client::new();
    let body = json!({
        "using": ["urn:ietf:params:jmap:core", "urn:ietf:params:jmap:calendars"],
        "methodCalls": [
            ["CalendarEvent/query", {
                "accountId": account_id,
                "filter": {
                    "inCalendars": [{
                        "calendarId": cal_id,
                        "position": 0
                    }]
                }
            }, "c0"],
            ["CalendarEvent/get", {
                "accountId": account_id,
                "#refs": {
                    "resultOf": "c0",
                    "path": "/ids"
                },
                "properties": ["id", "title", "start", "end", "location", "description", "uid", "participants", "isAllDay"]
            }, "c1"]
        ]
    });

    let res = client
        .post(url)
        .header("Authorization", token)
        .json(&body)
        .send()
        .await?;

    let json: serde_json::Value = res.json().await?;
    
    let list = json["methodResponses"][1][1]["list"]
        .as_array()
        .ok_or(JmapError::EventNotFound)?;
    
    let events: Vec<JmapEvent> = list
        .iter()
        .map(|v| JmapEvent::from_jmap_json(v.clone()))
        .collect::<Result<JmapEvent, JmapError>>();
    
    // Filter out errors
    let events: Vec<JmapEvent> = events.into_iter().filter_map(|r| r.ok()).collect();
    
    Ok(events)
}

pub async fn get_calendar_changes(
    url: &str,
    token: &str,
    account_id: &str,
    since: &str,
) -> Result<JmapChanges, JmapError> {
    let client = Client::new();
    let body = json!({
        "using": ["urn:ietf:params:jmap:core", "urn:ietf:params:jmap:calendars"],
        "methodCalls": [
            ["CalendarEvent/changes", {
                "accountId": account_id,
                "sinceState": since,
                "filter": {
                    "inCalendars": [{
                        "position": 0
                    }]
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

    let json: serde_json::Value = res.json().await?;
    
    let response = json["methodResponses"][0][1].clone();
    
    Ok(JmapChanges {
        new_state: response["newState"].as_str().unwrap_or_default().to_string(),
        updated: response["changed"].as_array().map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect::<Vec<String>>()).unwrap_or_default(),
        destroyed: response["removed"].as_array().map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect::<Vec<String>>()).unwrap_or_default(),
    })
}

pub async fn get_events_by_ids(url: &str, token: &str, account_id: &str, ids: &[String]) -> Result<Vec<JmapEvent>, JmapError> {
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

    let json: serde_json::Value = res.json().await?;
    
    let list = json["methodResponses"][0][1]["list"]
        .as_array()
        .ok_or(JmapError::EventNotFound)?;
    
    let events: Vec<JmapEvent> = list
        .iter()
        .map(|v| JmapEvent::from_jmap_json(v.clone()))
        .collect::<Result<JmapEvent, JmapError>>();
    
    let events: Vec<JmapEvent> = events.into_iter().filter_map(|r| r.ok()).collect();
    
    Ok(events)
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

    let json: serde_json::Value = res.json().await?;
    
    let ids = json["methodResponses"][0][1]["ids"]
        .as_array()
        .ok_or(JmapError::EventNotFound)?;
    
    let id = ids.first()
        .and_then(|v| v.as_str().map(String::from))
        .ok_or(JmapError::EventNotFound)?;

    Ok(id)
}

pub async fn push_event(url: &str, token: &str, account_id: &str, event: JmapEvent) -> Result<String, JmapError> {
    let client = Client::new();
    
    // Generate UID if not provided
    let uid = event.uid.unwrap_or_else(|| Uuid::new_v4().to_string());
    
    let mut event_obj = json!({
        "title": event.title,
        "start": event.start,
        "end": event.end,
        "isAllDay": event.is_all_day,
        "uid": uid
    });
    
    if let Some(loc) = event.location {
        event_obj["location"] = json!(loc);
    }
    
    if let Some(desc) = event.description {
        event_obj["description"] = json!(desc);
    }
    
    if let Some(ref participants) = event.participants {
        if !participants.is_empty() {
            event_obj["participants"] = build_participants_map(participants);
        }
    }

    let body = json!({
        "using": ["urn:ietf:params:jmap:core", "urn:ietf:params:jmap:calendars"],
        "methodCalls": [
            ["CalendarEvent/set", {
                "accountId": account_id,
                "create": {
                    "new-event": event_obj
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

    let json: serde_json::Value = res.json().await?;
    
    // Check for errors
    if let Some(err) = json["methodResponses"][0][1]["notCreated"].get("new-event") {
        let error = err["type"].as_str().unwrap_or("Unknown error");
        return Err(JmapError::Method(format!("Failed to create event: {}", error)));
    }
    
    // Extract the ID
    let id = json["methodResponses"][0][1]["created"]["new-event"]
        .as_str()
        .map(String::from)
        .ok_or_else(|| JmapError::Method("Failed to get created ID".to_string()))?;

    Ok(id)
}

pub async fn update_event(
    url: &str,
    token: &str,
    account_id: &str,
    event_id: &str,
    patch: serde_json::Map<String, serde_json::Value>
) -> Result<(), JmapError> {
    let client = Client::new();
    
    let body = json!({
        "using": ["urn:ietf:params:jmap:core", "urn:ietf:params:jmap:calendars"],
        "methodCalls": [
            ["CalendarEvent/set", {
                "accountId": account_id,
                "update": {
                    event_id: patch
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

    let json: serde_json::Value = res.json().await?;
    
    // Check for errors
    if let Some(err) = json["methodResponses"][0][1]["notUpdated"].get(event_id) {
        let error = err["type"].as_str().unwrap_or("Unknown error");
        return Err(JmapError::UpdateFailed(format!("Failed to update event: {}", error)));
    }
    
    Ok(())
}

pub async fn delete_event(url: &str, token: &str, account_id: &str, event_id: &str) -> Result<(), JmapError> {
    let client = Client::new();
    
    let body = json!({
        "using": ["urn:ietf:params:jmap:core", "urn:ietf:params:jmap:calendars"],
        "methodCalls": [
            ["CalendarEvent/set", {
                "accountId": account_id,
                "destroy": [event_id]
            }, "c0"]
        ]
    });

    let res = client
        .post(url)
        .header("Authorization", token)
        .json(&body)
        .send()
        .await?;

    let json: serde_json::Value = res.json().await?
    
    // Check for errors
    if let Some(err) = json["methodResponses"][0][1]["notDestroyed"].get(event_id) {
        let error = err["type"].as_str().unwrap_or("Unknown error");
        return Err(JmapError::DeleteFailed(format!("Failed to delete event: {}", error)));
    }
    
    Ok(())
}

pub async fn update_participant_status(
    url: &str,
    token: &str,
    account_id: &str,
    event_id: &str,
    user_email: &str,
    status: &str,
) -> Result<(), JmapError> {
    // Build the participant patch
    let mut patch = serde_json::Map::new();
    
    // In JSCalendar, participants is a Map keyed by email
    let mut participant_map = serde_json::Map::new();
    participant_map.insert("status".to_string(), json!(status));
    
    let mut participants_map = serde_json::Map::new();
    participants_map.insert(user_email.to_string(), serde_json::Value::Object(participant_map));
    
    patch.insert("participants".to_string(), serde_json::Value::Object(participants_map));
    
    update_event(url, token, account_id, event_id, patch).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_jmap_event_parsing() {
        let json = json!({
            "id": "123",
            "title": "Test Event",
            "start": "2025-01-01T10:00:00Z",
            "end": "2025-01-01T11:00:00Z",
            "location": "Test Location",
            "description": "Test Description",
            "uid": "test-uid",
            "participants": {
                "test@example.com": {
                    "email": "test@example.com",
                    "name": "Test User",
                    "status": "accepted"
                }
            },
            "isAllDay": false
        });
        
        let event = JmapEvent::from_jmap_json(json).unwrap();
        assert_eq!(event.id, Some("123".to_string()));
        assert_eq!(event.title, "Test Event");
        assert_eq!(event.location, Some("Test Location".to_string()));
        assert_eq!(event.participants.unwrap().len(), 1);
    }
}
