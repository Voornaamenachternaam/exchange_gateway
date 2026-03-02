use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::json;
use uuid::Uuid;

#[derive(Debug, Clone)]
pub struct JmapSession {
    pub api_url: String,
    pub access_token: String,
    pub account_id: String,
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
    #[serde(rename = "participants", skip_serializing_if = "Option::is_none")]
    pub participants: Option<Vec<Participant>>,
    #[serde(rename = "isAllDay")]
    pub is_all_day: bool,
    #[serde(rename = "recurrenceRule", skip_serializing_if = "Option::is_none")]
    pub recurrence_rule: Option<String>,
    #[serde(rename = "updated", skip_serializing_if = "Option::is_none")]
    pub updated: Option<String>, // Added for ChangeKey
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Participant {
    pub email: String,
    pub name: String,
    pub status: Option<String>,
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
    })
}

pub async fn get_default_calendar_id(
    url: &str,
    token: &str,
    account_id: &str,
) -> Result<String, String> {
    let client = Client::new();
    let body = json!({ "using": ["urn:ietf:params:jmap:core", "urn:ietf:params:jmap:calendars"], "methodCalls": [["Calendar/get", { "accountId": account_id, "ids": null }, "c0"]] });
    let res = client
        .post(url)
        .header("Authorization", format!("Basic {}", token))
        .json(&body)
        .send()
        .await
        .map_err(|e| e.to_string())?;
    let json: serde_json::Value = res.json().await.map_err(|e| e.to_string())?;
    if let Some(list) = json["methodResponses"][0][1]["list"].as_array()
        && let Some(first) = list.first()
    {
        return first["id"]
            .as_str()
            .map(String::from)
            .ok_or("Missing ID".into());
    }
    Err("No Calendars".into())
}

pub async fn get_calendar_state(
    url: &str,
    token: &str,
    account_id: &str,
) -> Result<String, String> {
    let client = Client::new();
    let body = json!({ "using": ["urn:ietf:params:jmap:core", "urn:ietf:params:jmap:calendars"], "methodCalls": [["CalendarEvent/get", { "accountId": account_id, "ids": [] }, "c0"]] });
    let res = client
        .post(url)
        .header("Authorization", format!("Basic {}", token))
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

pub async fn get_calendar_events(
    url: &str,
    token: &str,
    account_id: &str,
) -> Result<Vec<JmapEvent>, String> {
    let client = Client::new();
    let body_ids = json!({ "using": ["urn:ietf:params:jmap:core", "urn:ietf:params:jmap:calendars"], "methodCalls": [["CalendarEvent/get", { "accountId": account_id, "ids": null }, "c0"]] });
    let res = client
        .post(url)
        .header("Authorization", format!("Basic {}", token))
        .json(&body_ids)
        .send()
        .await
        .map_err(|e| e.to_string())?;
    let json: serde_json::Value = res.json().await.map_err(|e| e.to_string())?;
    let ids: Vec<String> = json["methodResponses"][0][1]["list"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v["id"].as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();
    get_events_by_ids(url, token, account_id, &ids).await
}

pub async fn get_events_by_ids(
    url: &str,
    token: &str,
    account_id: &str,
    ids: &[String],
) -> Result<Vec<JmapEvent>, String> {
    if ids.is_empty() {
        return Ok(vec![]);
    }
    let client = Client::new();
    // Added 'updated' to properties
    let body = json!({ "using": ["urn:ietf:params:jmap:core", "urn:ietf:params:jmap:calendars"], "methodCalls": [["CalendarEvent/get", { "accountId": account_id, "ids": ids, "properties": ["id", "title", "start", "end", "location", "description", "uid", "participants", "isAllDay", "recurrenceRule", "updated"] }, "c0"]] });
    let res = client
        .post(url)
        .header("Authorization", format!("Basic {}", token))
        .json(&body)
        .send()
        .await
        .map_err(|e| e.to_string())?;
    let json: serde_json::Value = res.json().await.map_err(|e| e.to_string())?;
    let events: Vec<JmapEvent> =
        serde_json::from_value(json["methodResponses"][0][1]["list"].clone()).unwrap_or_default();
    Ok(events)
}

pub async fn get_event_by_id(
    url: &str,
    token: &str,
    account_id: &str,
    id: &str,
) -> Result<JmapEvent, String> {
    let events = get_events_by_ids(url, token, account_id, &[id.to_string()]).await?;
    events.into_iter().next().ok_or("Event not found".into())
}

pub async fn push_event(
    url: &str,
    token: &str,
    account_id: &str,
    event: JmapEvent,
) -> Result<String, String> {
    let client = Client::new();
    let create_map = json!({ Uuid::new_v4().to_string(): event });
    let body = json!({ "using": ["urn:ietf:params:jmap:core", "urn:ietf:params:jmap:calendars"], "methodCalls": [["CalendarEvent/set", { "accountId": account_id, "create": create_map }, "c0"]] });
    let res = client
        .post(url)
        .header("Authorization", format!("Basic {}", token))
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

pub async fn patch_event(
    url: &str,
    token: &str,
    account_id: &str,
    id: &str,
    patch: serde_json::Map<String, serde_json::Value>,
) -> Result<(), String> {
    let client = Client::new();
    let update_map = json!({ id: patch });
    let body = json!({ "using": ["urn:ietf:params:jmap:core", "urn:ietf:params:jmap:calendars"], "methodCalls": [["CalendarEvent/set", { "accountId": account_id, "update": update_map }, "c0"]] });
    let _ = client
        .post(url)
        .header("Authorization", format!("Basic {}", token))
        .json(&body)
        .send()
        .await
        .map_err(|e| e.to_string())?;
    Ok(())
}

pub async fn destroy_events(
    url: &str,
    token: &str,
    account_id: &str,
    ids: Vec<String>,
) -> Result<(), String> {
    let client = Client::new();
    let body = json!({ "using": ["urn:ietf:params:jmap:core", "urn:ietf:params:jmap:calendars"], "methodCalls": [["CalendarEvent/set", { "accountId": account_id, "destroy": ids }, "c0"]] });
    let _ = client
        .post(url)
        .header("Authorization", format!("Basic {}", token))
        .json(&body)
        .send()
        .await
        .map_err(|e| e.to_string())?;
    Ok(())
}

pub async fn get_calendar_changes(
    url: &str,
    token: &str,
    account_id: &str,
    since: &str,
) -> Result<JmapChanges, String> {
    let client = Client::new();
    let body = json!({ "using": ["urn:ietf:params:jmap:core", "urn:ietf:params:jmap:calendars"], "methodCalls": [["CalendarEvent/changes", { "accountId": account_id, "sinceState": since }, "c0"]] });
    let res = client
        .post(url)
        .header("Authorization", format!("Basic {}", token))
        .json(&body)
        .send()
        .await
        .map_err(|e| e.to_string())?;
    let json: serde_json::Value = res.json().await.map_err(|e| e.to_string())?;
    serde_json::from_value(json["methodResponses"][0][1].clone())
        .map_err(|e| format!("Changes Parse: {}", e))
}

pub async fn search_principals(
    url: &str,
    token: &str,
    account_id: &str,
    query: &str,
) -> Result<Vec<Principal>, String> {
    let client = Client::new();
    let body = json!({ "using": ["urn:ietf:params:jmap:core", "urn:ietf:params:jmap:mail"], "methodCalls": [["Principal/query", { "accountId": account_id, "filter": { "operator": "OR", "conditions": [{ "email": query }, { "name": query }] } }, "c0"], ["Principal/get", { "accountId": account_id, "#ids": { "resultOf": "c0", "name": "Principal/query", "path": "/ids" } }, "c1"]] });
    let res = client
        .post(url)
        .header("Authorization", format!("Basic {}", token))
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

pub async fn find_event_by_uid(
    url: &str,
    token: &str,
    account_id: &str,
    uid: &str,
) -> Result<String, String> {
    let client = Client::new();
    let body = json!({ "using": ["urn:ietf:params:jmap:core", "urn:ietf:params:jmap:calendars"], "methodCalls": [["CalendarEvent/query", { "accountId": account_id, "filter": { "uid": uid } }, "c0"], ["CalendarEvent/get", { "accountId": account_id, "#ids": { "resultOf": "c0", "name": "CalendarEvent/query", "path": "/ids" } }, "c1"]] });
    let res = client
        .post(url)
        .header("Authorization", format!("Basic {}", token))
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

pub async fn update_participant_status(
    url: &str,
    token: &str,
    account_id: &str,
    event_id: &str,
    user_email: &str,
    status: &str,
) -> Result<(), String> {
    let client = Client::new();
    let patch = json!({ format!("participants/{}/status", user_email): status });
    let body = json!({ "using": ["urn:ietf:params:jmap:core", "urn:ietf:params:jmap:calendars"], "methodCalls": [["CalendarEvent/set", { "accountId": account_id, "update": { event_id: patch } }, "c0"]] });
    let _ = client
        .post(url)
        .header("Authorization", format!("Basic {}", token))
        .json(&body)
        .send()
        .await
        .map_err(|e| e.to_string())?;
    Ok(())
}

pub async fn get_blob(
    url: &str,
    token: &str,
    account_id: &str,
    blob_id: &str,
) -> Result<Vec<u8>, String> {
    let client = Client::new();
    let body = json!({ "using": ["urn:ietf:params:jmap:core"], "methodCalls": [["Blob/get", { "accountId": account_id, "ids": [blob_id] }, "c0"]] });
    let res = client
        .post(url)
        .header("Authorization", format!("Basic {}", token))
        .json(&body)
        .send()
        .await
        .map_err(|e| e.to_string())?;
    let json: serde_json::Value = res.json().await.map_err(|e| e.to_string())?;
    if let Some(b64) = json["methodResponses"][0][1]["list"][0]["data:asBase64"].as_str() {
        return base64::Engine::decode(&base64::engine::general_purpose::STANDARD, b64)
            .map_err(|e| e.to_string());
    }
    Err("Blob not found".into())
}
