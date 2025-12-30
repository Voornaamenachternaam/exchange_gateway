// src/sync.rs

use crate::caldav::CaldavClient;
use crate::models::AppState;
use crate::storage::Storage;
use chrono::Utc;
use hmac::{Hmac, Mac};
use sha2::Sha256;
use uuid::Uuid;
type HmacSha256 = Hmac<Sha256>;

pub fn generate_server_id(secret: &str, resource_href: &str) -> String {
    let mut mac = HmacSha256::new_from_slice(secret.as_bytes()).unwrap();
    mac.update(resource_href.as_bytes());
    let result = mac.finalize().into_bytes();
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(result)
}

pub fn generate_change_key(etag: &str) -> String {
    let now = Utc::now();
    let nan = now.timestamp_nanos();
    let payload = format!("{}:{}", etag, nan);
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(payload.as_bytes())
}

/// Perform Sync: get events from CalDAV and produce EAS Sync XML.
/// (This implementation does a simple empty sync for illustration.)
pub async fn perform_sync(
    state: std::sync::Arc<AppState>,
    owner: &str,
    collection_id: &str,
    _incoming_sync_key: &str,
    _window_size: usize,
    username_for_caldav: &str,
    password_for_caldav: &str,
) -> anyhow::Result<String> {
    // Use CalDAV to find the user's calendar collection
    let caldav = CaldavClient::new(&state.cfg);
    let calendars = caldav
        .find_user_calendars(username_for_caldav, password_for_caldav)
        .await?;
    let collection_href = calendars
        .first()
        .ok_or_else(|| anyhow::anyhow!("no calendars found"))?
        .clone();

    // (In a full implementation, query events and compute changes. Here we do minimal.)
    let new_sync_key = Uuid::new_v4().to_string();
    state
        .storage
        .set_sync_key(owner, collection_id, &new_sync_key, Some(""))
        .await?;

    // Return an empty Sync response (client will believe all items are up-to-date)
    let xml = format!(
        r#"<?xml version="1.0" encoding="utf-8"?><Sync xmlns="AirSync:"><Collections><Collection><Class>Calendar</Class><SyncKey>{}</SyncKey><CollectionId>{}</CollectionId><Status>1</Status><Commands></Commands></Collection></Collections></Sync>"#,
        new_sync_key, collection_id
    );
    Ok(xml)
}
