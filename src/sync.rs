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
use base64::engine::general_purpose::URL_SAFE_NO_PAD;

type HmacSha256 = Hmac<Sha256>;

/// Perform ActiveSync Sync by querying CalDAV and building XML.
pub async fn perform_sync(
    state: Arc<AppState>,
    owner: &str,
    collection_id: &str,
    sync_key: &str,
    steps: i64,
    username: &str,
    password: &str,
) -> Result<String> {
    // Initialize CalDAV client with base URL
    let mut caldav = CaldavClient::new(&state.cfg);

    // List calendars for the user
    let calendars: Vec<String> = caldav.find_user_calendars(owner, password).await?;
    let collection_href = calendars
        .first()
        .ok_or_else(|| anyhow::anyhow!("no calendars found"))?
        .clone();

    // For brevity: always return an empty sync response (protocol-compliance stub)
    let new_sync_key = Uuid::new_v4().to_string();
    let xml = format!(
        "<?xml version=\"1.0\" encoding=\"utf-8\"?>
<Sync>
  <Status>1</Status>
  <SyncKey>{}</SyncKey>
  <Collection>
    <Class>Calendar</Class>
    <SyncKey>{}</SyncKey>
    <CollectionId>{}</CollectionId>
    <Status>1</Status>
    <Commands></Commands>
    <Options></Options>
  </Collection>
</Sync>",
        new_sync_key, sync_key, collection_id
    );

    Ok(xml)
}
