// src/eas.rs
use crate::models::AppState;
use crate::wbxml::Wbxml;
use crate::sync;
use axum::{extract::State, http::HeaderMap, response::IntoResponse};
use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use bytes::Bytes;
use std::sync::Arc;

/// Parse Basic Authorization header (username:password).
fn parse_basic_auth(headers: &HeaderMap) -> Option<(String, String)> {
    if let Some(v) = headers.get("authorization") {
        if let Ok(s) = v.to_str() {
            let s = s.trim();
            if s.to_lowercase().starts_with("basic ") {
                let b64 = &s[6..].trim();
                let dec = STANDARD.decode(b64.as_bytes()).ok()?;
                let creds = String::from_utf8(dec).ok()?;
                if let Some(idx) = creds.find(':') {
                    let user = creds[..idx].to_string();
                    let pass = creds[idx+1..].to_string();
                    return Some((user, pass));
                }
            }
        }
    }
    None
}

/// ActiveSync minimal handler: supports FolderSync and Sync for calendars.
pub async fn handle(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    body: Bytes,
) -> impl IntoResponse {
    // Convert bytes -> xml (WBXML decode attempt)
    let payload = body.to_vec();
    let wbxml = Wbxml::new();
    let xml = match wbxml.decode(&payload) {
        Ok(s) => s,
        Err(e) => return (axum::http::StatusCode::BAD_REQUEST, format!("WBXML decode error: {}", e)).into_response(),
    };

    // Parse basic auth to figure out username (if present)
    let (username, password) = parse_basic_auth(&headers).unwrap_or((String::new(), String::new()));

    // FolderSync handling: advertise a Calendar folder
    if xml.contains("<FolderSync") {
        let resp = r#"<?xml version="1.0" encoding="utf-8"?>
<FolderSync>
  <Status>1</Status>
  <SyncKey>0</SyncKey>
  <Folders>
    <Folder>
      <ServerId>calendar-1</ServerId>
      <ParentId>0</ParentId>
      <DisplayName>Calendar</DisplayName>
      <Type>8</Type>
    </Folder>
  </Folders>
</FolderSync>"#;
        return (axum::http::StatusCode::OK, resp).into_response();
    }

    // Sync handling: perform a CalDAV query + build minimal sync XML
    if xml.contains("<Sync") {
        // owner defaults to username or "demo"
        let owner = if !username.is_empty() { username.as_str() } else { "demo" };
        match sync::perform_sync(state.0.clone(), owner, "calendar-1", "0", 100, &username, &password).await {
            Ok(xml) => return (axum::http::StatusCode::OK, xml).into_response(),
            Err(e) => return (axum::http::StatusCode::INTERNAL_SERVER_ERROR, format!("sync error: {}", e)).into_response(),
        }
    }

    (axum::http::StatusCode::BAD_REQUEST, "Unsupported command").into_response()
}
