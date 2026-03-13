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
fn extract_first_field(json: &serde_json::Value, field: &str) -> Option<String> {
    // New format: { "result": [ { "results": [ { field: "..." } ] } ] }
    if let Some(val) = json
        .get("result")
        .and_then(|r| r.get(0))
        .and_then(|r| r.get("results"))
        .and_then(|r| r.get(0))
        .and_then(|r| r.get(field))
        .and_then(|v| v.as_str())
    {
        return Some(val.to_owned());
    }

    // Legacy format: [ { "results": [ { field: "..." } ] } ]
    json.get(0)
        .and_then(|r| r.get("results"))
        .and_then(|r| r.get(0))
        .and_then(|r| r.get(field))
        .and_then(|v| v.as_str())
        .map(|s| s.to_owned())
}

/// Returns `true` when the DB response contains at least one result row.
/// An empty `"results": []` array (normal "no rows matched") returns `false`.
fn has_result_rows(json: &serde_json::Value) -> bool {
    // New format
    if let Some(true) = json
        .get("result")
        .and_then(|r| r.get(0))
        .and_then(|r| r.get("results"))
        .and_then(|a| a.as_array())
        .map(|a| !a.is_empty())
    {
        return true;
    }
    // Legacy format
    if let Some(true) = json
        .get(0)
        .and_then(|r| r.get("results"))
        .and_then(|a| a.as_array())
        .map(|a| !a.is_empty())
    {
        return true;
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

/// Stored ActiveSync sync state: the JMAP state used to compute deltas.
pub struct ActiveSyncState {
    pub jmap_state: String,
}

/// Retrieve the stored JMAP state for a given user / device / collection.
/// Returns `Ok(None)` when no row exists, `Err` on DB / network / parse
/// failures so callers can distinguish "missing state" from transient errors.
pub async fn get_sync_state_full(
    config: &AppConfig,
    user: &str,
    device_id: &str,
    coll: &str,
) -> Result<Option<ActiveSyncState>, DbError> {
    let client = reqwest::Client::new();
    let body = json!({
        "query": "SELECT jmap_state FROM sync_state WHERE user_email = ? AND device_id = ? AND collection_id = ?",
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
                "get_sync_state_full: DB request failed: {e}"
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
                "get_sync_state_full: failed to parse DB response: {e}"
            );
            return Err(DbError::Parse(e));
        }
    };

    if let Err(reason) = check_db_success(&json) {
        tracing::error!(
            user = user,
            device_id = device_id,
            collection = coll,
            "get_sync_state_full: {reason}"
        );
        return Err(reason);
    }

    match extract_first_field(&json, "jmap_state") {
        Some(jmap_state) => Ok(Some(ActiveSyncState { jmap_state })),
        None => {
            if has_result_rows(&json) {
                tracing::warn!(
                    user = user,
                    device_id = device_id,
                    collection = coll,
                    "get_sync_state_full: unexpected DB response format"
                );
                Err(DbError::UnexpectedFormat)
            } else {
                Ok(None)
            }
        }
    }
}

/// Check whether the DB API response indicates success.
/// The Worker wrapper format includes "success": true/false` at the top
/// level; the legacy direct-array format has no such flag (always assumed OK
/// because it only appears when the request actually succeeded).
///
/// Returns `Ok(())` when the query succeeded, `Err(reason)` when the response
/// explicitly signals failure.
fn check_db_success(json: &serde_json::Value) -> Result<(), DbError> {
    if let Some(obj) = json.as_object() {
        match obj.get("success").and_then(|v| v.as_bool()) {
            Some(true) => {}
            Some(false) => {
                let msg = json
                    .get("errors")
                    .and_then(|e| e.as_array())
                    .and_then(|a| a.first())
                    .and_then(|e| e.get("message"))
                    .and_then(|m| m.as_str())
                    .unwrap_or("DB query failed")
                    .to_owned();
                return Err(DbError::Query(msg));
            }
            None => return Err(DbError::UnexpectedFormat),
        }
    } else if !json.is_array() {
        return Err(DbError::UnexpectedFormat);
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
) -> Result<bool, DbError> {
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
                user = user,
                device_id = device_id,
                collection = coll,
                "claim_sync_key: DB request failed: {e}"
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
                "claim_sync_key: failed to parse DB response: {e}"
            );
            return Err(DbError::Parse(e));
        }
    };
    if let Err(reason) = check_db_success(&json) {
        tracing::error!(
            user = user,
            device_id = device_id,
            collection = coll,
            "claim_sync_key: {reason}"
        );
        return Err(reason);
    }
    let changes = match extract_meta_changes(&json) {
        Some(n) => n,
        None => {
            // The DB confirmed the query succeeded (`check_db_success` passed)
            // but the response lacks the `meta.changes` field.  Without the
            // row-count we cannot tell whether the UPDATE matched (claim
            // succeeded) or not (invalid/replayed SyncKey).  Treating this as
            // success would silently accept bad keys, so return an error
            // instead.  The caller's `Err` handler restores the original
            // SyncKey and returns a transient server error (Status 5), letting
            // the client retry safely.
            tracing::error!(
                user = user,
                device_id = device_id,
                collection = coll,
                "claim_sync_key: meta.changes missing from DB response; \
                 cannot confirm SyncKey claim — treating as transient error"
            );
            return Err(DbError::UnexpectedFormat);
        }
    };
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
