// src/jmap_client.rs
use serde::{Deserialize, Serialize};
use base64::{Engine as _, engine::general_purpose};

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

#[derive(Debug, Serialize, Deserialize)]
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
    let resp = client.get(url)
        .basic_auth(user, Some(pass))
        .send()
        .await?;

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

pub async fn get_default_calendar_id(url: &str, token: &str, account_id: &str) -> Result<String, anyhow::Error> {
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
    
    let resp = client.post(url)
        .header("Authorization", format!("Basic {}", token))
        .json(&body)
        .send()
        .await?;

    let json: serde_json::Value = resp.json().await?;
    
    if let Some(list) = json["methodResponses"][0][1]["list"].as_array() {
        if let Some(first) = list.first() {
            return Ok(first["id"].as_str().unwrap_or("default").to_string());
        }
    }
    
    Ok("default".to_string())
}

pub async fn get_calendar_state(url: &str, token: &str, account_id: &str) -> Result<String, anyhow::Error> {
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
    
    let resp = client.post(url)
        .header("Authorization", format!("Basic {}", token))
        .json(&body)
        .send()
        .await?;

    let json: serde_json::Value = resp.json().await?;
    Ok(json["methodResponses"][0][1]["state"].as_str().unwrap_or("unknown").to_string())
}

pub async fn get_calendar_events(url: &str, token: &str, account_id: &str) -> Result<Vec<JmapEvent>, anyhow::Error> {
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
    
    let resp = client.post(url)
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

pub async fn get_events_by_ids(url: &str, token: &str, account_id: &str, ids: &[String]) -> Result<Vec<JmapEvent>, anyhow::Error> {
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
    
    let resp = client.post(url)
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

pub async fn find_event_by_uid(url: &str, token: &str, account_id: &str, uid: &str) -> Result<String, anyhow::Error> {
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
    
    let resp = client.post(url)
        .header("Authorization", format!("Basic {}", token))
        .json(&body)
        .send()
        .await?;

    let json: serde_json::Value = resp.json().await?;
    
    if let Some(ids) = json["methodResponses"][0][1]["ids"].as_array() {
        if let Some(id) = ids.first() {
            return Ok(id.as_str().unwrap_or_default().to_string());
        }
    }
    
    Err(anyhow::anyhow!("Event not found"))
}

pub async fn update_participant_status(url: &str, token: &str, account_id: &str, event_id: &str, status: &str) -> Result<(), anyhow::Error> {
    // Placeholder for robust update logic
    Ok(())
}

pub async fn get_calendar_changes(url: &str, token: &str, account_id: &str, since: &str) -> Result<JmapChanges, anyhow::Error> {
    let client = reqwest::Client::new();
    let body = serde_json::json!({
        "using": ["urn:ietf:params:jmap:core", "urn:ietf:params:jmap:calendars"],
        "methodCalls": [
            ["CalendarEvent/changes", {
                "accountId": account_id,
                "sinceState": since
            }, "c0"]
        ]
    });
    
    let resp = client.post(url)
        .header("Authorization", format!("Basic {}", token))
        .json(&body)
        .send()
        .await?;

    let json: serde_json::Value = resp.json().await?;
    
    let res = &json["methodResponses"][0][1];
    Ok(JmapChanges {
        old_state: since.to_string(),
        new_state: res["newState"].as_str().unwrap_or(since).to_string(),
        updated: res["changed"].as_array().map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect()).unwrap_or_default(),
        destroyed: res["removed"].as_array().map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect()).unwrap_or_default(),
    })
}

pub async fn push_event(url: &str, token: &str, account_id: &str, event: JmapEvent) -> Result<String, anyhow::Error> {
    let client = reqwest::Client::new();
    
    let body = serde_json::json!({
        "using": ["urn:ietf:params:jmap:core", "urn:ietf:params:jmap:calendars"],
        "methodCalls": [
            ["CalendarEvent/set", {
                "accountId": account_id,
                "create": {
                    "client-id-123": {
                        "title": event.title,
                        "start": event.start,
                        "end": event.end,
                        "location": event.location,
                        "description": event.description,
                        "isAllDay": event.is_all_day,
                        "uid": event.uid
                    }
                }
            }, "c0"]
        ]
    });
    
    let resp = client.post(url)
        .header("Authorization", format!("Basic {}", token))
        .json(&body)
        .send()
        .await?;

    let json: serde_json::Value = resp.json().await?;
    Ok(json["methodResponses"][0][1]["created"]["client-id-123"]["id"].as_str().unwrap_or("new-id").to_string())
}
