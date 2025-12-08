use crate::models::AppState;
use crate::caldav::CaldavClient;
use crate::storage::Storage;
use anyhow::Result;
use std::sync::Arc;
use chrono::Utc;
use uuid::Uuid;
use hmac::{Hmac, Mac};
use sha2::Sha256;
type HmacSha256 = Hmac<Sha256>;

pub fn generate_server_id(secret: &str, resource_href: &str) -> String {
    let mut mac = HmacSha256::new_from_slice(secret.as_bytes()).expect("HMAC init");
    mac.update(resource_href.as_bytes());
    let result = mac.finalize().into_bytes();
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(result)
}

pub fn generate_change_key(etag: &str) -> String {
    let payload = format!("{}:{}", etag, Utc::now().timestamp_nanos());
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(payload.as_bytes())
}

/// Perform Sync: list changes via CalDAV REPORT, map them to Add/Change/Delete
pub async fn perform_sync(state: Arc<AppState>, owner: &str, collection_id: &str, incoming_sync_key: &str, window_size: usize, username_for_caldav: &str, password_for_caldav: &str) -> Result<String> {
    let storage: &Storage = &state.storage;
    let caldav = CaldavClient::new(&state.cfg);
    let calendars = caldav.find_user_calendars(username_for_caldav, password_for_caldav).await?;
    let collection_href = calendars.get(0).ok_or_else(|| anyhow::anyhow!("no calendars found"))?.clone();

    let start = (Utc::now() - chrono::Duration::weeks(52)).format("%Y%m%dT%H%M%SZ").to_string();
    let end = (Utc::now() + chrono::Duration::weeks(52)).format("%Y%m%dT%H%M%SZ").to_string();

    let multistatus = caldav.query_events(&collection_href, &start, &end, username_for_caldav, password_for_caldav).await?;
    // Parse multistatus to extract href/etag/calendar-data - left minimal here but can be expanded

    // For each resource: determine add/change/delete compared to items_map
    // ... omitted heavy parsing for brevity but hooks are here

    let new_sync_key = Uuid::new_v4().to_string();
    storage.set_sync_key(owner, collection_id, &new_sync_key, Some("token")).await?;

    // Build Sync XML response
    let mut xml = String::new();
    xml.push_str(r#"<?xml version="1.0" encoding="utf-8"?>"#);
    xml.push_str(r#"<Sync xmlns="AirSync:"><Collections><Collection><Class>Calendar</Class>"#);
    xml.push_str(&format!(r#"<SyncKey>{}</SyncKey>"#, new_sync_key));
    xml.push_str(&format!(r#"<CollectionId>{}</CollectionId>"#, collection_id));
    xml.push_str(r#"<Status>1</Status><Commands>"#);
    // Add/Change/Delete entries would go here
    xml.push_str("</Commands></Collection></Collections></Sync>");
    Ok(xml)
}
