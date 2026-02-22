// src/sync.rs
use crate::models::AppState;
use crate::caldav::CaldavClient;
use anyhow::Result;
use chrono::Utc;
use uuid::Uuid;
use std::sync::Arc;

use hmac::Hmac;
use hmac::Mac;
use sha2::Sha256;
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;

type HmacSha256 = Hmac<Sha256>;

/// Generate a unique server ID (URL-safe base64 HMAC) for a resource href.
pub fn generate_server_id(secret: &str, resource_href: &str) -> String {
    let key = secret.as_bytes();
    let mut mac = HmacSha256::new_from_slice(key).expect("HMAC init");
    mac.update(resource_href.as_bytes());
    let result = mac.finalize().into_bytes();
    URL_SAFE_NO_PAD.encode(result)
}

/// Generate a ChangeKey from the ETag and current time.
pub fn generate_change_key(etag: &str) -> String {
    let now = Utc::now();
    let nan = now.timestamp_nanos_opt().unwrap_or(now.timestamp() * 1_000_000_000);
    let payload = format!("{}:{}", etag, nan);
    URL_SAFE_NO_PAD.encode(payload.as_bytes())
}

/// Perform an ActiveSync Sync (minimal implementation): fetch events via CalDAV.
pub async fn perform_sync(
    state: Arc<AppState>,
    owner: &str,
    collection_id: &str,
    _incoming_sync_key: &str,
    _window_size: usize,
    username: &str,
    password: &str,
) -> Result<String> {
    let storage = &state.storage;
    let caldav = CaldavClient::new(&state.cfg);

    // Discover the user's calendar home
    let calendars = caldav.find_user_calendars(username, password).await?;
    let collection_href = calendars.first().ok_or_else(|| anyhow::anyhow!("no calendars found"))?.clone();

    // Query events over a wide date range (1 year back/forth)
    let start = (Utc::now() - chrono::Duration::weeks(52)).format("%Y%m%dT%H%M%SZ").to_string();
    let end = (Utc::now() + chrono::Duration::weeks(52)).format("%Y%m%dT%H%M%SZ").to_string();
    let _ = caldav.query_events(&collection_href, &start, &end, username, password).await?;

    // Generate a new SyncKey and store it
    let new_sync_key = Uuid::new_v4().to_string();
    storage.set_sync_key(owner, collection_id, &new_sync_key, Some("token")).await?;

    // Return an empty Sync response (no actual items returned)
    let xml = format!(
        r#"<?xml version="1.0" encoding="utf-8"?><Sync xmlns="AirSync:">
  <Collections>
    <Collection>
      <Class>Calendar</Class>
      <SyncKey>{}</SyncKey>
      <CollectionId>{}</CollectionId>
      <Status>1</Status>
      <Commands></Commands>
    </Collection>
  </Collections>
</Sync>"#,
        new_sync_key, collection_id
    );
    Ok(xml)
}
