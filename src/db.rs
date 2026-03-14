use crate::config::AppConfig;
use serde_json::json;

#[derive(Debug, thiserror::Error)]
pub enum DbError {
    #[error("DB request failed: {0}")]
    Request(reqwest::Error),
    #[error("failed to parse DB response: {0}")]
    Parse(reqwest::Error),
    #[error("DB query failed: {0}")]
    Query(String),
    #[error("unexpected DB response format")]
    UnexpectedFormat,
}

/// Extract a field from the first row of a DB API response.
/// Supports both the Worker wrapper format `{ "result": [ { "results": [...] } ] }`
/// and the legacy direct-array format `[ { "results": [...] } ]`.
/// Helper to get the `results` array from either the new or legacy DB response format.
fn get_results_array(json: &serde_json::Value) -> Result<Option<&serde_json::Value>, DbError> {
    let check_results_array = |val: &serde_json::Value| {
        if val.is_array() {
            Ok(Some(val))
        } else {
            Err(DbError::UnexpectedFormat)
        }
    };

    // New format: { "result": [ { "results": [...] } ] }
    if let Some(result_wrapper) = json.get("result") {
        if let Some(first_array_item) = result_wrapper.get(0) {
            if let Some(results_field) = first_array_item.get("results") {
                return check_results_array(results_field);
            }
        }
        // If "result" wrapper exists but inner structure is malformed, it's an UnexpectedFormat
        return Err(DbError::UnexpectedFormat);
    }

    // Legacy format: [ { "results": [...] } ]
    if let Some(first_array_item) = json.get(0) {
        if let Some(results_field) = first_array_item.get("results") {
            return check_results_array(results_field);
        }
        // If it's an array but inner structure is malformed, it's an UnexpectedFormat
        return Err(DbError::UnexpectedFormat);
    }

    // Neither format matched, meaning the "result" wrapper is absent AND the top-level is not an array.
    // This should be treated as "no results array found", which maps to Ok(None).
    Ok(None)
}

/// Extract a field from the first row of a DB API response.
/// Supports both the Worker wrapper format `{ "result": [ { "results": [...] } ] }`
/// and the legacy direct-array format `[ { "results": [...] } ]`.
fn extract_first_field(json: &serde_json::Value, field: &str) -> Option<String> {
    get_results_array(json)?
        .get(0)
        .and_then(|r| r.get(field))
        .and_then(|v| v.as_str())
        .map(|s| s.to_owned())
}

/// Returns `true` when the DB response contains at least one result row.
/// An empty `"results": []` array (normal "no rows matched") returns `false`.
fn has_result_rows(json: &serde_json::Value) -> bool {
    get_results_array(json)
        .and_then(|a| a.as_array())
        .map_or(false, |a| !a.is_empty())
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

/// Atomically claim (invalidate) a SyncKey by updating the row only when the
/// stored `sync_key` still matches `expected_key`.  Returns `Ok(true)` when
/// the claim succeeded (exactly one row was updated), `Ok(false)` when another
/// request already consumed the key (zero rows updated), and `Err` on
/// transient DB / network failures.
///
/// This prevents two concurrent requests carrying the same SyncKey from both
/// passing validation: only the first one to execute the UPDATE will match.
pub async fn update_ews_sync_state_cas(
    config: &AppConfig,
    user: &str,
    folder: &str,
    expected_sync_token: &str,
    new_sync_token: &str,
    new_jmap_state: &str,
) -> Result<bool, DbError> {
    let client = reqwest::Client::new();
    let body = json!({
        "query": "UPDATE ews_sync_state SET sync_state = ?, jmap_state = ? WHERE user_email = ? AND folder_id = ? AND sync_state = ?",
        "params": [new_sync_token, new_jmap_state, user, folder, expected_sync_token]
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
                user = user,
                folder = folder,
                "update_ews_sync_state_cas: DB request failed: {e}"
            );
            return Err(DbError::Request(e));
        }
    };
    let json: serde_json::Value = match res.json().await {
        Ok(json) => json,
        Err(e) => {
            tracing::error!(
                user = user,
                folder = folder,
                "update_ews_sync_state_cas: failed to parse DB response: {e}"
            );
            return Err(DbError::Parse(e));
        }
    };
    if let Err(reason) = check_db_success(&json) {
        tracing::error!(
            user = user,
            folder = folder,
            "update_ews_sync_state_cas: {reason}"
        );
        return Err(reason);
    }
    let changes = match extract_meta_changes(&json) {
        Some(n) => n,
        None => {
            tracing::error!(
                user = user,
                folder = folder,
                "update_ews_sync_state_cas: meta.changes missing from DB response; \
                 cannot confirm SyncState claim — treating as transient error"
            );
            return Err(DbError::UnexpectedFormat);
        }
    };
    match changes {
        0 => Ok(false),
        1 => Ok(true),
        _ => Err(DbError::Query(format!(
            "expected CAS update to affect at most one row, got {changes}"
        ))),
    }
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
) -> Result<(), DbError> {
    let client = reqwest::Client::new();
    let body = json!({
        "query": "DELETE FROM sync_state WHERE user_email = ? AND device_id = ? AND collection_id = ?",
        "params": [user, device_id, coll]
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
                user = user,
                device_id = device_id,
                collection = coll,
                "delete_sync_state failed: {e}"
            );
            return Err(DbError::Request(e));
        }
    };
    let json: serde_json::Value = match res.json().await {
        Ok(json) => json,
        Err(e) => {
            tracing::error!(
                user = user,
                device_id = device_id,
                collection = coll,
                "delete_sync_state: failed to parse DB response: {e}"
            );
            return Err(DbError::Parse(e));
        }
    };
    if let Err(reason) = check_db_success(&json) {
        tracing::error!(
            user = user,
            device_id = device_id,
            collection = coll,
            "delete_sync_state: {reason}"
        );
        return Err(reason);
    }
    Ok(())
}

/// Stored EWS sync state: the token issued to the client and the corresponding
/// JMAP state used to compute deltas.
pub struct EwsSyncState {
    pub sync_state: String,
    pub jmap_state: String,
}

/// Retrieve the stored EWS sync state for a given user / folder.
/// Returns `Ok(None)` when no row exists, `Err` on DB / network / parse
/// failures so callers can distinguish "missing state" from transient errors.
pub async fn get_ews_sync_state(
    config: &AppConfig,
    user: &str,
    folder: &str,
) -> Result<Option<EwsSyncState>, DbError> {
    let client = reqwest::Client::new();
    let body = json!({
        "query": "SELECT sync_state, jmap_state FROM ews_sync_state WHERE user_email = ? AND folder_id = ?",
        "params": [user, folder]
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
                user = user,
                folder = folder,
                "get_ews_sync_state: DB request failed: {e}"
            );
            return Err(DbError::Request(e));
        }
    };
    let json: serde_json::Value = match res.json().await {
        Ok(json) => json,
        Err(e) => {
            tracing::error!(
                user = user,
                folder = folder,
                "get_ews_sync_state: failed to parse DB response: {e}"
            );
            return Err(DbError::Parse(e));
        }
    };

    if let Err(reason) = check_db_success(&json) {
        tracing::error!(user = user, folder = folder, "get_ews_sync_state: {reason}");
        return Err(reason);
    }

    let sync_state = extract_first_field(&json, "sync_state");
    let jmap_state = extract_first_field(&json, "jmap_state");
    match (sync_state, jmap_state) {
        (Some(sync_state), Some(jmap_state)) => Ok(Some(EwsSyncState {
            sync_state,
            jmap_state,
        })),
        _ => {
            if has_result_rows(&json) {
                tracing::warn!(
                    user = user,
                    folder = folder,
                    "get_ews_sync_state: unexpected DB response format"
                );
                Err(DbError::UnexpectedFormat)
            } else {
                Ok(None)
            }
        }
    }
}

pub async fn delete_ews_sync_state(
    config: &AppConfig,
    user: &str,
    folder: &str,
) -> Result<(), DbError> {
    let client = reqwest::Client::new();
    let body = json!({
        "query": "DELETE FROM ews_sync_state WHERE user_email = ? AND folder_id = ?",
        "params": [user, folder]
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
                user = user,
                folder = folder,
                "delete_ews_sync_state failed: {e}"
            );
            return Err(DbError::Request(e));
        }
    };
    let json: serde_json::Value = match res.json().await {
        Ok(json) => json,
        Err(e) => {
            tracing::error!(
                user = user,
                folder = folder,
                "delete_ews_sync_state: failed to parse DB response: {e}"
            );
            return Err(DbError::Parse(e));
        }
    };
    if let Err(reason) = check_db_success(&json) {
        tracing::error!(
            user = user,
            folder = folder,
            "delete_ews_sync_state: {reason}"
        );
        return Err(reason);
    }
    Ok(())
}

pub async fn update_ews_sync_state(
    config: &AppConfig,
    user: &str,
    folder: &str,
    state: &str,
    jmap_state: &str,
) -> Result<(), DbError> {
    let client = reqwest::Client::new();
    let body = json!({
        "query": "INSERT OR REPLACE INTO ews_sync_state (user_email, folder_id, sync_state, jmap_state) VALUES (?, ?, ?, ?)",
        "params": [user, folder, state, jmap_state]
    });
    let resp = client
        .post(&config.db_api_url)
        .bearer_auth(&config.db_auth_token)
        .json(&body)
        .send()
        .await
        .map_err(DbError::Request)?
        .error_for_status()
        .map_err(DbError::Request)?;
    let json: serde_json::Value = resp.json().await.map_err(DbError::Parse)?;
    check_db_success(&json)
}
