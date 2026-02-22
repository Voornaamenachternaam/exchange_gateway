// src/sync.rs
use crate::models::AppState;
use crate::caldav::CaldavClient;
use anyhow::Result;
use uuid::Uuid;
use std::sync::Arc;

/// Perform ActiveSync Sync by querying CalDAV and building XML.
pub async fn perform_sync(
    state: Arc<AppState>,
    owner: &str,
    collection_id: &str,
    _sync_key: &str,
    _steps: i64,
    _username: &str,
    _password: &str,
) -> Result<String> {
    // Initialize CalDAV client with base URL
    let caldav = CaldavClient::new(&state.cfg);

    // List calendars for the user
    let calendars: Vec<String> = caldav.find_user_calendars(owner, _password).await?;
    let _collection_href = calendars.first().cloned().unwrap_or_else(|| state.cfg.caldav_base.clone());

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
        new_sync_key, _sync_key, collection_id
    );

    Ok(xml)
}
