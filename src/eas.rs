use crate::models::AppState;
use crate::sync;
use crate::wbxml::Wbxml;
use axum::http::HeaderMap;
use axum::{extract::Extension, http::StatusCode, response::IntoResponse};
use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use bytes::Bytes;
use std::sync::Arc;

/// Parse Basic Authorization header (username:password).
fn parse_basic_auth(headers: &HeaderMap) -> Option<(String, String)> {
    if let Some(v) = headers.get("authorization") && let Ok(s) = v.to_str() {
        let s = s.trim();
        if s.to_lowercase().starts_with("basic ") {
            let b64 = s[6..].trim();
            let mut out = Vec::new();
            if BASE64.decode_vec(b64.as_bytes(), &mut out).is_ok()
                && let Ok(creds) = String::from_utf8(out)
                && let Some(idx) = creds.find(':')
            {
                let user = creds[..idx].to_string();
                let pass = creds[idx + 1..].to_string();
                return Some((user, pass));
            }
        }
    }
    None
}

pub async fn handle_activesync(
    Extension(state): Extension<Arc<AppState>>,
    headers: HeaderMap,
    body: Bytes,
) -> impl IntoResponse {
    let payload = body.to_vec();
    let wbxml = Wbxml::new();
    let xml = match wbxml.decode(&payload) {
        Ok(s) => s,
        Err(e) => {
            return (StatusCode::BAD_REQUEST, format!("Invalid WBXML: {}", e)).into_response();
        }
    };

    let (username, password) = parse_basic_auth(&headers).unwrap_or((String::new(), String::new()));

    // Minimal parse to route command: look for <FolderSync> or <Sync>
    if xml.contains("<FolderSync") {
        // Respond with one calendar folder
        let resp = r#"<?xml version="1.0" encoding="utf-8"?>
<FolderSync>
  <Status>1</Status>
  <SyncKey>0</SyncKey>
  <Folders>
    <Folder>
      <ServerId>1</ServerId>
      <ParentId>0</ParentId>
      <DisplayName>Calendar</DisplayName>
      <Type>8</Type>
    </Folder>
  </Folders>
</FolderSync>"#;
        return (StatusCode::OK, resp.to_string()).into_response();
    } else if xml.contains("<Sync") {
        // Perform sync (no incremental changes in this minimal implementation)
        let owner = if !username.is_empty() { username.as_str() } else { "demo" };
        let collection_id = "1";
        match sync::perform_sync(state, owner, collection_id, "0", 100, &username, &password).await {
            Ok(resp_xml) => return (StatusCode::OK, resp_xml).into_response(),
            Err(e) => {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("Sync error: {}", e),
                )
                    .into_response();
            }
        }
    }

    (StatusCode::BAD_REQUEST, "Unsupported ActiveSync command").into_response()
}
