use bytes::Bytes;
use warp::reply::Response;
use warp::http::StatusCode;
use crate::caldav;
use crate::caldav::AppState;
use base64::Engine;
use base64::engine::general_purpose::STANDARD as base64_engine;
use quick_xml::Reader;
use quick_xml::events::Event;
use std::convert::Infallible;

pub async fn handle_activesync(state: std::sync::Arc<AppState>, auth: Option<String>, body: Bytes) -> Result<impl warp::Reply, Infallible> {
    // Basic auth parsing
    let (user, pass) = match parse_basic(auth) {
        Ok(v) => v,
        Err(_) => {
            let res = warp::reply::with_status("Unauthorized", StatusCode::UNAUTHORIZED);
            return Ok(res);
        }
    };

    // Create CalDAV client for the user
    let caldav_client = match caldav::make_caldav_client(&state.cfg, &user, &pass).await {
        Ok(c) => c,
        Err(e) => {
            tracing::error!("CalDAV error: {:?}", e);
            return Ok(warp::reply::with_status("Bad Gateway", StatusCode::BAD_GATEWAY));
        }
    };

    // Attempt to parse EAS XML (WBXML is common; client might send XML)
    let mut reader = Reader::from_reader(body.reader());
    reader.trim_text(true);
    let mut buf = Vec::new();
    let mut op = None;
    loop {
        match reader.read_event(&mut buf) {
            Ok(Event::Start(e)) => {
                match e.name() {
                    b"Sync" => { op = Some("Sync"); break; }
                    b"ItemOperations" => { op = Some("ItemOperations"); break; }
                    _ => {}
                }
            }
            Ok(Event::Eof) => break,
            Err(_) => break,
            _ => {}
        }
        buf.clear();
    }

    match op {
        Some("Sync") => {
            let resp = build_eas_sync_response().await;
            Ok(warp::reply::with_status(resp, StatusCode::OK))
        }
        Some("ItemOperations") => {
            let resp = build_eas_itemoperations_response().await;
            Ok(warp::reply::with_status(resp, StatusCode::OK))
        }
        _ => Ok(warp::reply::with_status("Unsupported ActiveSync operation", StatusCode::BAD_REQUEST))
    }
}

async fn build_eas_sync_response() -> String {
    r#"<?xml version="1.0" encoding="utf-8"?>
<Sync xmlns="AirSync:">
  <Collections>
    <Collection>
      <Class>Calendar</Class>
      <SyncKey>0</SyncKey>
      <CollectionId>1</CollectionId>
      <Status>1</Status>
      <Commands/>
    </Collection>
  </Collections>
</Sync>"#.to_string()
}

async fn build_eas_itemoperations_response() -> String {
    r#"<?xml version="1.0" encoding="utf-8"?>
<ItemOperations xmlns="ItemOperations:">
  <Status>1</Status>
</ItemOperations>"#.to_string()
}

fn parse_basic(header: Option<String>) -> Result<(String, String), ()> {
    if let Some(h) = header {
        let h = h.trim();
        if h.to_lowercase().starts_with("basic ") {
            let b64 = &h[6..];
            if let Ok(decoded) = base64_engine.decode(b64) {
                if let Ok(s) = String::from_utf8(decoded) {
                    let mut parts = s.splitn(2, ':');
                    if let (Some(u), Some(p)) = (parts.next(), parts.next()) {
                        return Ok((u.to_string(), p.to_string()));
                    }
                }
            }
        }
    }
    Err(())
}
