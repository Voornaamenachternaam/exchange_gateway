use crate::config::AppConfig;
use serde_json::json;

/// Extract a field from the first row of a DB API response.
/// Supports both the Worker wrapper format `{ "result": [ { "results": [...] } ] }`
/// and the legacy direct-array format `[ { "results": [...] } ]`.
fn extract_first_field(json: &serde_json::Value, field: &str) -> Option<String> {
    // New format: { "result": [ { "results": [ { field: "..." } ] } ] }
    if let Some(val) = json["result"][0]["results"][0][field].as_str() {
        return Some(val.to_owned());
    }
    // Legacy format: [ { "results": [ { field: "..." } ] } ]
    if let Some(val) = json[0]["results"][0][field].as_str() {
        return Some(val.to_owned());
    }
    None
}

/// Returns `true` when the DB response contains at least one result row.
/// An empty `"results": []` array (normal "no rows matched") returns `false`.
fn has_result_rows(json: &serde_json::Value) -> bool {
    // New format
    if let Some(arr) = json["result"][0]["results"].as_array() {
        return !arr.is_empty();
    }
    // Legacy format
    if let Some(arr) = json[0]["results"].as_array() {
        return !arr.is_empty();
    }
    false
}

pub async fn register_device(config: &AppConfig, user: &str, device_id: &str) {
    let client = reqwest::Client::new();
    let body = json!({
        "query": "INSERT OR IGNORE INTO device_info (user_email, device_id) VALUES (?, ?)",
        "params": [user, device_id]
    });
    let _ = client
        .post(&config.db_api_url)
        .bearer_auth(&config.db_auth_token)
        .json(&body)
        .send()
        .await;
}

pub async fn get_sync_state(
    config: &AppConfig,
    user: &str,
    device_id: &str,
    coll: &str,
) -> Option<String> {
    let client = reqwest::Client::new();
    let body = json!({
        "query": "SELECT jmap_state FROM sync_state WHERE user_email = ? AND device_id = ? AND collection_id = ?",
        "params": [user, device_id, coll]
    });
    let res = client
        .post(&config.db_api_url)
        .bearer_auth(&config.db_auth_token)
        .json(&body)
        .send()
        .await
        .ok()?;
    let json: serde_json::Value = res.json().await.ok()?;

    let state = extract_first_field(&json, "jmap_state");
    if state.is_none() && has_result_rows(&json) {
        tracing::warn!(
            user = user, device_id = device_id, collection = coll,
            "get_sync_state: unexpected DB response format"
        );
    }
    state
}

pub async fn update_sync_state(
    config: &AppConfig,
    user: &str,
    device_id: &str,
    coll: &str,
    key: &str,
    state: &str,
) {
    let client = reqwest::Client::new();
    let body = json!({
        "query": "INSERT OR REPLACE INTO sync_state (user_email, device_id, collection_id, sync_key, jmap_state) VALUES (?, ?, ?, ?, ?)",
        "params": [user, device_id, coll, key, state]
    });
    let _ = client
        .post(&config.db_api_url)
        .bearer_auth(&config.db_auth_token)
        .json(&body)
        .send()
        .await;
}

pub async fn delete_sync_state(
    config: &AppConfig,
    user: &str,
    device_id: &str,
    coll: &str,
) {
    let client = reqwest::Client::new();
    let body = json!({
        "query": "DELETE FROM sync_state WHERE user_email = ? AND device_id = ? AND collection_id = ?",
        "params": [user, device_id, coll]
    });
    match client
        .post(&config.db_api_url)
        .bearer_auth(&config.db_auth_token)
        .json(&body)
        .send()
        .await
        .and_then(|res| res.error_for_status())
        .and_then(|res| res.error_for_status())
    {
        Ok(_) => {}
        Err(e) => {
            tracing::error!(
                user = user, device_id = device_id, collection = coll,
                "delete_sync_state failed: {e}"
            );
        }
    }
}

pub async fn get_ews_sync_state(config: &AppConfig, user: &str, folder: &str) -> Option<String> {
    let client = reqwest::Client::new();
    let body = json!({
        "query": "SELECT jmap_state FROM ews_sync_state WHERE user_email = ? AND folder_id = ?",
        "params": [user, folder]
    });
    let res = client
        .post(&config.db_api_url)
        .bearer_auth(&config.db_auth_token)
        .json(&body)
        .send()
        .await
        .ok()?;
    let json: serde_json::Value = res.json().await.ok()?;

    let state = extract_first_field(&json, "jmap_state");
    if state.is_none() && has_result_rows(&json) {
        tracing::warn!(
            "get_ews_sync_state: unexpected DB response format for user and folder."
        );
    }
    state
}

pub async fn delete_ews_sync_state(config: &AppConfig, user: &str, folder: &str) {
    let client = reqwest::Client::new();
    let body = json!({
        "query": "DELETE FROM ews_sync_state WHERE user_email = ? AND folder_id = ?",
        "params": [user, folder]
    });
    if let Err(e) = client
        .post(&config.db_api_url)
        .bearer_auth(&config.db_auth_token)
        .json(&body)
        .send()
        .await
    {
        tracing::error!(
            user = user, folder = folder,
            "delete_ews_sync_state failed: {e}"
        );
    }
}

pub async fn update_ews_sync_state(
    config: &AppConfig,
    user: &str,
    folder: &str,
    state: &str,
    jmap_state: &str,
) {
    let client = reqwest::Client::new();
    let body = json!({
        "query": "INSERT OR REPLACE INTO ews_sync_state (user_email, folder_id, sync_state, jmap_state) VALUES (?, ?, ?, ?)",
        "params": [user, folder, state, jmap_state]
    });
    let _ = client
        .post(&config.db_api_url)
        .bearer_auth(&config.db_auth_token)
        .json(&body)
        .send()
        .await;
}
