// src/sync.rs
use crate::models::AppState;
use crate::caldav::CaldavClient;
use anyhow::{anyhow, Result};
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

/// Escape special XML characters in event data
fn xml_escape(input: &str) -> String {
    input.replace("&", "&amp;")
         .replace("<", "&lt;")
         .replace(">", "&gt;")
}

/// Perform a calendar sync: discover calendars, fetch events, and build ActiveSync Sync response.
pub async fn perform_sync(
    state: Arc<AppState>,
    owner: &str,
    collection_id: &str,
    incoming_sync_key: &str,
    _window_size: usize,
    username: &str,
    password: &str,
) -> Result<String> {
    let storage = &state.storage;
    let caldav = CaldavClient::new(&state.cfg);

    // Discover the user's calendar collection (e.g. primary calendar)
    let calendars = caldav.find_user_calendars(username, password).await?;
    let collection_href = calendars.first()
        .ok_or_else(|| anyhow!("no calendars found"))?
        .clone();

    // Query events over a wide date range (1 year back/forth)
    let start = (Utc::now() - chrono::Duration::weeks(52))
        .format("%Y%m%dT%H%M%SZ").to_string();
    let end = (Utc::now() + chrono::Duration::weeks(52))
        .format("%Y%m%dT%H%M%SZ").to_string();
    let events_xml = caldav.query_events(&collection_href, &start, &end, username, password).await?;

    // Parse CalDAV XML response for events
    use quick_xml::Reader;
    use quick_xml::events::Event;
    let mut reader = Reader::from_str(&events_xml);
    reader.trim_text(true);
    let mut buf = Vec::new();
    #[derive(Clone)]
    struct EventItem { href: String, etag: String, ics: String }
    let mut events = Vec::new();
    let mut current = EventItem { href: String::new(), etag: String::new(), ics: String::new() };
    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(ref e)) => {
                match e.name().as_ref() {
                    b"D:href" => {
                        if let Ok(Event::Text(e)) = reader.read_event_into(&mut buf) {
                            current.href = e.unescape().unwrap_or_default().to_string();
                        }
                    }
                    b"D:getetag" => {
                        if let Ok(Event::Text(e)) = reader.read_event_into(&mut buf) {
                            current.etag = e.unescape().unwrap_or_default().to_string();
                        }
                    }
                    b"C:calendar-data" => {
                        if let Ok(Event::Text(e)) = reader.read_event_into(&mut buf) {
                            current.ics = e.unescape().unwrap_or_default().to_string();
                        }
                    }
                    _ => {}
                }
            }
            Ok(Event::End(ref e)) if e.name().as_ref() == b"D:response" => {
                // Save the event record if we have an href
                if !current.href.is_empty() {
                    events.push(current.clone());
                }
                current = EventItem { href: String::new(), etag: String::new(), ics: String::new() };
            }
            Ok(Event::Eof) => break,
            _ => {}
        }
        buf.clear();
    }

    // Retrieve existing items (server_ids) from previous sync
    let old_items = storage.list_changes_since(owner, 0).await?;

    // Build ActiveSync Sync commands
    let mut commands = String::new();
    let mut seen_ids = Vec::new();

    for ev in events {
        let href = ev.href;
        if href.is_empty() { continue; }
        let resource_href = href.clone();
        let etag = ev.etag.trim_matches('"').to_string();
        let ics = ev.ics;

        // Generate consistent server_id for this event
        let server_id = generate_server_id(&state.cfg.hmac_secret, &resource_href);
        seen_ids.push(server_id.clone());

        // Parse ICS content for event details
        let mut subject = String::new();
        let mut dtstart = String::new();
        let mut dtend = String::new();
        let mut location = String::new();
        let mut description = String::new();
        let mut uid = String::new();
        for line in ics.lines() {
            if let Some(val) = line.strip_prefix("SUMMARY:") {
                subject = val.to_string();
            } else if line.starts_with("DTSTART:") {
                dtstart = line.trim_start_matches("DTSTART:").to_string();
            } else if line.starts_with("DTEND:") {
                dtend = line.trim_start_matches("DTEND:").to_string();
            } else if let Some(val) = line.strip_prefix("LOCATION:") {
                location = val.to_string();
            } else if let Some(val) = line.strip_prefix("DESCRIPTION:") {
                description = val.to_string();
            } else if let Some(val) = line.strip_prefix("UID:") {
                uid = val.to_string();
            }
        }

        // Upsert mapping in worker-backed storage
        storage.upsert_item_map(owner, &collection_href, &resource_href, &server_id, &uid, &etag).await?;

        // Determine if new or existing item
        let is_new = !old_items.iter().any(|(id, _)| id == &server_id);
        if is_new {
            commands.push_str(&format!("<Add><ServerId>{}</ServerId><ApplicationData>", server_id));
        } else {
            commands.push_str(&format!("<Change><ServerId>{}</ServerId><ApplicationData>", server_id));
        }

        // Fill ApplicationData with event fields
        commands.push_str(&format!("<Subject>{}</Subject>", xml_escape(&subject)));
        commands.push_str(&format!("<StartTime>{}</StartTime>", dtstart));
        commands.push_str(&format!("<EndTime>{}</EndTime>", dtend));
        if !location.is_empty() {
            commands.push_str(&format!("<Location>{}</Location>", xml_escape(&location)));
        }
        if !description.is_empty() {
            commands.push_str(&format!("<Body>{}</Body>", xml_escape(&description)));
        }
        if !uid.is_empty() {
            commands.push_str(&format!("<UID>{}</UID>", xml_escape(&uid)));
        }
        // All-day event flag if dates have no 'T'
        if !dtstart.contains('T') || !dtend.contains('T') {
            commands.push_str("<AllDayEvent>1</AllDayEvent>");
        }
        if is_new {
            commands.push_str("</ApplicationData></Add>");
        } else {
            commands.push_str("</ApplicationData></Change>");
        }
    }

    // Handle deletions: any old item not seen in new list
    for (old_id, _) in old_items {
        if !seen_ids.contains(&old_id) {
            commands.push_str(&format!("<Delete><ServerId>{}</ServerId></Delete>", old_id));
            let _ = storage.delete_item_by_server_id(&old_id).await;
        }
    }

    // Generate new SyncKey and update storage
    let new_sync_key = Uuid::new_v4().to_string();
    storage.set_sync_key(owner, collection_id, &new_sync_key, Some("token")).await?;

    // Construct Sync response XML
    let xml = format!(
        r#"<?xml version="1.0" encoding="utf-8"?><Sync xmlns="AirSync:"><Collections><Collection><Class>Calendar</Class><SyncKey>{}</SyncKey><CollectionId>{}</CollectionId><Status>1</Status><Commands>{}</Commands></Collection></Collections></Sync>"#,
        new_sync_key, collection_id, commands
    );
    Ok(xml)
}
