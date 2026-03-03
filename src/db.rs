use crate::config::AppConfig;
use serde_json::json;

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

    // Path matches Worker wrapper: { "result": [ { "results": [...] } ] }
    json["result"][0]["results"][0]["jmap_state"]
        .as_str()
        .map(String::from)
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

    json["result"][0]["results"][0]["jmap_state"]
        .as_str()
        .map(String::from)
}

pub async fn delete_ews_sync_state(config: &AppConfig, user: &str, folder: &str) {
    let client = reqwest::Client::new();
    let body = json!({
        "query": "DELETE FROM ews_sync_state WHERE user_email = ? AND folder_id = ?",
        "params": [user, folder]
    });
    let _ = client
        .post(&config.db_api_url)
        .bearer_auth(&config.db_auth_token)
        .json(&body)
        .send()
        .await;
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
