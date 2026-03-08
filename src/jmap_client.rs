// src/jmap_client.rs
use base64::{Engine as _, engine::general_purpose};
use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize)]
pub struct JmapSession {
    #[serde(rename = "apiUrl")]
    pub api_url: String,
    #[serde(rename = "primaryAccounts")]
    pub primary_accounts: std::collections::HashMap<String, String>,

    pub access_token: String,
    pub account_id: String,
}

#[derive(Debug, Serialize, Deserialize, Default)]
pub struct JmapEvent {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    pub title: String,
    pub start: String,
    pub end: String,
    #[serde(rename = "isAllDay")]
    pub is_all_day: bool,
    pub location: Option<String>,
    pub description: Option<String>,
    pub uid: Option<String>,
    pub participants: Option<Vec<Participant>>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Participant {
    pub email: String,
    pub name: String,
}

#[derive(Debug, Serialize, Deserialize, Default)]
pub struct JmapChanges {
    #[serde(rename = "oldState")]
    pub old_state: String,
    #[serde(rename = "newState")]
    pub new_state: String,
    pub updated: Vec<String>,
    pub destroyed: Vec<String>,
}

pub async fn get_session(url: &str, user: &str, pass: &str) -> Result<JmapSession, anyhow::Error> {
    let client = reqwest::Client::new();
    let resp = client.get(url).basic_auth(user, Some(pass)).send().await?;

    if !resp.status().is_success() {
        return Err(anyhow::anyhow!("Auth failed: {}", resp.status()));
    }

    let json: serde_json::Value = resp.json().await?;

    let account_id = json["primaryAccounts"]["urn:ietf:params:jmap:calendars"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("AccountId missing"))?
        .to_string();

    Ok(JmapSession {
        api_url: json["apiUrl"].as_str().unwrap_or(url).to_string(),
        access_token: general_purpose::STANDARD.encode(format!("{}:{}", user, pass)),
        account_id,
        primary_accounts: std::collections::HashMap::new(),
    })
}

pub async fn get_default_calendar_id(
    url: &str,
    token: &str,
    account_id: &str,
) -> Result<String, anyhow::Error> {
    let client = reqwest::Client::new();
    let body = serde_json::json!({
        "using": ["urn:ietf:params:jmap:core", "urn:ietf:params:jmap:calendars"],
        "methodCalls": [
            ["Calendar/get", {
                "accountId": account_id,
                "ids": null
            }, "c0"]
        ]
    });

    let resp = client
        .post(url)
        .header("Authorization", format!("Basic {}", token))
        .json(&body)
        .send()
        .await?;

    let json: serde_json::Value = resp.json().await?;
    
    let list = json["methodResponses"][0][1]["list"].as_array().ok_or(JmapError::CalendarNotFound)?;
    
    for cal in list {
        if let Some(is_default) = cal.get("isDefault").and_then(|v| v.as_bool())
            && is_default
                && let Some(id) = cal.get("id").and_then(|v| v.as_str()) {
                    return Ok(id.to_string());
                }
    }
    
    if let Some(first) = list.first()
        && let Some(id) = first.get("id").and_then(|v| v.as_str()) {
            return Ok(id.to_string());
        }
    
    Err(JmapError::CalendarNotFound)
}

pub async fn get_calendar_state(
    url: &str,
    token: &str,
    account_id: &str,
) -> Result<String, anyhow::Error> {
    let client = reqwest::Client::new();
    let body = serde_json::json!({
        "using": ["urn:ietf:params:jmap:core", "urn:ietf:params:jmap:calendars"],
        "methodCalls": [
            ["CalendarEvent/get", {
                "accountId": account_id,
                "ids": [],
                "properties": ["id"]
            }, "c0"]
        ]
    });

    let resp = client
        .post(url)
        .header("Authorization", format!("Basic {}", token))
        .json(&body)
        .send()
        .await?;

    let json: serde_json::Value = resp.json().await?;
    Ok(json["methodResponses"][0][1]["state"]
        .as_str()
        .unwrap_or("unknown")
        .to_string())
}

pub async fn get_calendar_events(
    url: &str,
    token: &str,
    account_id: &str,
) -> Result<Vec<JmapEvent>, anyhow::Error> {
    let client = reqwest::Client::new();
    let body = serde_json::json!({
        "using": ["urn:ietf:params:jmap:core", "urn:ietf:params:jmap:calendars"],
        "methodCalls": [
            ["CalendarEvent/get", {
                "accountId": account_id,
                "ids": null,
                "properties": ["id", "title", "start", "end", "isAllDay", "location", "description", "uid", "participants"]
            }, "c0"]
        ]
    });

    let resp = client
        .post(url)
        .header("Authorization", format!("Basic {}", token))
        .json(&body)
        .send()
        .await?;

    let json: serde_json::Value = resp.json().await?;
    let mut events = Vec::new();

    if let Some(list) = json["methodResponses"][0][1]["list"].as_array() {
        for item in list {
            events.push(JmapEvent {
                id: item["id"].as_str().map(String::from),
                title: item["title"].as_str().unwrap_or("Untitled").to_string(),
                start: item["start"].as_str().unwrap_or("").to_string(),
                end: item["end"].as_str().unwrap_or("").to_string(),
                is_all_day: item["isAllDay"].as_bool().unwrap_or(false),
                location: item["location"].as_str().map(String::from),
                description: item["description"].as_str().map(String::from),
                uid: item["uid"].as_str().map(String::from),
                participants: None,
            });
        }
    }
    Ok(events)
}

pub async fn get_events_by_ids(
    url: &str,
    token: &str,
    account_id: &str,
    ids: &[String],
) -> Result<Vec<JmapEvent>, anyhow::Error> {
    let client = reqwest::Client::new();
    let body = serde_json::json!({
        "using": ["urn:ietf:params:jmap:core", "urn:ietf:params:jmap:calendars"],
        "methodCalls": [
            ["CalendarEvent/get", {
                "accountId": account_id,
                "ids": ids,
                "properties": ["id", "title", "start", "end", "isAllDay", "location", "description", "uid"]
            }, "c0"]
        ]
    });

    let resp = client
        .post(url)
        .header("Authorization", format!("Basic {}", token))
        .json(&body)
        .send()
        .await?;

    let json: serde_json::Value = resp.json().await?;
    let mut events = Vec::new();

pub async fn push_event(url: &str, token: &str, account_id: &str, mut event: JmapEvent) -> Result<String, JmapError> {
    let client = Client::new();
    
    if event.uid.is_none() {
        event.uid = Some(Uuid::new_v4().to_string());
    }
    
    let mut event_data = json!({
        "title": event.title,
        "start": event.start,
        "end": event.end,
        "isAllDay": event.is_all_day,
        "uid": event.uid
    });

    if let Some(loc) = &event.location {
        event_data["location"] = json!({ "description": loc });
    }

    if let Some(desc) = &event.description {
        event_data["description"] = json!({ "description": desc });
    }

    // Handle participants: Convert Vec to Map for Stalwart/JSCalendar
    if let Some(parts) = &event.participants
        && !parts.is_empty() {
            let mut participants_map = serde_json::Map::new();
            for p in parts {
                let mut p_map = json!({
                    "email": p.email,
                    "name": p.name
                });
                if let Some(status) = &p.status {
                    p_map["status"] = json!(status);
                }
                participants_map.insert(p.email.clone(), p_map);
            }
            event_data["participants"] = serde_json::Value::Object(participants_map);
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
        .header("Authorization", format!("Basic {}", token))
        .json(&body)
        .send()
        .await?;

    let json: serde_json::Value = res.json().await?;
    
    if let Some(err) = json["methodResponses"][0][1].as_object()
        && err.get("type").and_then(|v| v.as_str()) == Some("error") {
            let desc = err.get("description").and_then(|v| v.as_str()).unwrap_or("Unknown error");
            return Err(JmapError::Method(desc.to_string()));
        }
    
    let created_id = json["methodResponses"][0][1]["created"]["new-event"]["id"]
        .as_str()
        .ok_or(JmapError::Method("Failed to get created ID".to_string()))?;
    
    Ok(created_id.to_string())
}

pub async fn find_event_by_uid(
    url: &str,
    token: &str,
    account_id: &str,
    uid: &str,
) -> Result<String, anyhow::Error> {
    let client = reqwest::Client::new();
    let body = serde_json::json!({
        "using": ["urn:ietf:params:jmap:core", "urn:ietf:params:jmap:calendars"],
        "methodCalls": [
            ["CalendarEvent/query", {
                "accountId": account_id,
                "filter": { "uid": uid }
            }, "c0"]
        ]
    });

    let resp = client
        .post(url)
        .header("Authorization", format!("Basic {}", token))
        .json(&body)
        .send()
        .await?;

    let json: serde_json::Value = res.json().await?;
    
    if let Some(ids) = json["methodResponses"][0][1]["ids"].as_array()
        && let Some(id) = ids.first().and_then(|v| v.as_str()) {
            return Ok(id.to_string());
        }
    
    Err(JmapError::EventNotFound)
}

// Updated to accept user_email to properly address the participant in JMAP
pub async fn update_participant_status(
    url: &str,
    token: &str,
    account_id: &str,
    event_id: &str,
    user_email: &str,
    status: &str,
) -> Result<(), anyhow::Error> {
    let client = reqwest::Client::new();

    // JMAP requires updating the specific participant key in the map.
    // Using /set with update patch.
    let body = serde_json::json!({
        "using": ["urn:ietf:params:jmap:core", "urn:ietf:params:jmap:calendars"],
        "methodCalls": [
            ["CalendarEvent/set", {
                "accountId": account_id,
                "update": {
                    event_id: {
                        format!("participants/{}", user_email): {
                            "participationStatus": status
                        }
                    }
                }
            }, "c0"]
        ]
    });

    let _res = client
        .post(url)
        .header("Authorization", format!("Basic {}", token))
        .json(&body)
        .send()
        .await?;

    let json: serde_json::Value = res.json().await?;
    
    if let Some(err) = json["methodResponses"][0][1].as_object()
        && err.get("type").and_then(|v| v.as_str()) == Some("error") {
            let desc = err.get("description").and_then(|v| v.as_str()).unwrap_or_default();
            return Err(JmapError::Method(desc.to_string()));
        }
    
    Ok(())
}

pub async fn get_blob(session: &JmapSession, blob_id: &str) -> Result<Vec<u8>, JmapError> {
    let _body = json!({ "using": ["urn:ietf:params:jmap:core", "urn:ietf:params:jmap:blob"], "methodCalls": [["Blob/get", { "accountId": session.account_id, "ids": [blob_id], "properties": ["data:asBase64", "data:asText"] }, "c0"]] });
    Err(JmapError::NotFound(format!("blob {}", blob_id)))
}
