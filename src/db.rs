// src/db.rs
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

    if let Err(reason) = check_db_success(&json) {
        tracing::error!(
            user = user, device_id = device_id, collection = coll,
            "get_sync_state_full: {reason}"
        );
        return Err(reason);
    }

    match extract_first_field(&json, "jmap_state") {
        Some(jmap_state) => Ok(Some(ActiveSyncState { jmap_state })),
        None => {
            if has_result_rows(&json) {
                tracing::warn!(
                    user = user, device_id = device_id, collection = coll,
                    "get_sync_state_full: unexpected DB response format"
                );
                Err("unexpected DB response format".to_string())
            } else {
                Ok(None)
            }
        }
    }
}

/// Check whether the DB API response indicates success.
/// The Worker wrapper format includes `"success": true/false` at the top
/// level; the legacy direct-array format has no such flag (always assumed OK
/// because it only appears when the request actually succeeded).
///
/// Returns `Ok(())` when the query succeeded, `Err(reason)` when the response
/// explicitly signals failure.
fn check_db_success(json: &serde_json::Value) -> Result<(), String> {
    // New format: { "success": true/false, "result": [...], ... }
    if let Some(flag) = json.get("success")
        && flag.as_bool() != Some(true) {
            // Try to extract an error message from the response.
            let detail = json
                .get("errors")
                .and_then(|e| e.get(0))
                .and_then(|e| e.get("message"))
                .and_then(|m| m.as_str())
                .unwrap_or("unknown error");
            return Err(format!("DB query failed: {detail}"));
        }
    // Legacy array format – no top-level success flag; treat as OK.
    if json
        .get(0)
        .and_then(|r| r.get("success"))
        .and_then(|s| s.as_bool())
        == Some(false)
    {
        return Err("DB query failed".to_string());
    }
    Ok(())
}

/// Extract the `meta.changes` count from a D1 API response.  Returns `None`
/// when the field is missing or the response format is unrecognised, so
/// callers can distinguish "no meta field" from an explicit zero.
fn extract_meta_changes(json: &serde_json::Value) -> Option<u64> {
    // New format: { "result": [ { "meta": { "changes": N } } ] }
    if let Some(n) = json
        .get("result")
        .and_then(|r| r.get(0))
        .and_then(|r| r.get("meta"))
        .and_then(|m| m.get("changes"))
        .and_then(|v| v.as_u64())
    {
        return Some(n);
    }
    // Legacy format: [ { "meta": { "changes": N } } ]
    json.get(0)
        .and_then(|r| r.get("meta"))
        .and_then(|m| m.get("changes"))
        .and_then(|v| v.as_u64())
}

/// Atomically claim (invalidate) a SyncKey by updating the row only when the
/// stored `sync_key` still matches `expected_key`.  Returns `Ok(true)` when
/// the claim succeeded (exactly one row was updated), `Ok(false)` when another
/// request already consumed the key (zero rows updated), and `Err` on
/// transient DB / network failures.
///
/// This prevents two concurrent requests carrying the same SyncKey from both
/// passing validation: only the first one to execute the UPDATE will match.
pub async fn claim_sync_key(
    config: &AppConfig,
    user: &str,
    device_id: &str,
    coll: &str,
    expected_key: &str,
    new_key: &str,
) -> Result<bool, String> {
    let client = reqwest::Client::new();
    let body = json!({
        "query": "UPDATE sync_state SET sync_key = ? WHERE user_email = ? AND device_id = ? AND collection_id = ? AND sync_key = ?",
        "params": [new_key, user, device_id, coll, expected_key]
    });
    let res = match client
        .post(&config.db_api_url)
        .bearer_auth(&config.db_auth_token)
        .json(&body)
        .send()
        .await
        .and_then(|res| res.error_for_status())
    {
        Ok(res) => res,
        Err(e) => {
            tracing::error!(
                user = user, device_id = device_id, collection = coll,
                "claim_sync_key: DB request failed: {e}"
            );
            return Err(format!("DB request failed: {e}"));
        }
    };
    let json: serde_json::Value = match res.json().await {
        Ok(json) => json,
        Err(e) => {
            tracing::error!(
                user = user, device_id = device_id, collection = coll,
                "claim_sync_key: failed to parse DB response: {e}"
            );
            return Err(format!("failed to parse DB response: {e}"));
        }
    };
    if let Err(reason) = check_db_success(&json) {
        tracing::error!(
            user = user, device_id = device_id, collection = coll,
            "claim_sync_key: {reason}"
        );
        return Err(reason);
    }
    let changes = extract_meta_changes(&json)
        .ok_or_else(|| "claim_sync_key: unexpected DB response format".to_string())?;
    Ok(changes > 0)
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
    json[0]["results"][0]["jmap_state"]
        .as_str()
        .map(String::from)
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
