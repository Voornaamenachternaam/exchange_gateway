// src/eas.rs

use crate::models::AppState;
use crate::sync;
use crate::wbxml::Wbxml;
use axum::http::HeaderMap;
use axum::{extract::Extension, http::StatusCode, response::IntoResponse};
use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use bytes::Bytes;
use std::sync::Arc;

/// Parse Basic authentication (email:password).
fn parse_basic_auth(headers: &HeaderMap) -> Option<(String, String)> {
    if let Some(v) = headers.get("authorization") {
        if let Ok(s) = v.to_str() {
            let s = s.trim();
            if s.to_lowercase().starts_with("basic ") {
                let b64 = &s[6..];
                let mut out = Vec::new();
                if BASE64.decode_vec(b64.as_bytes(), &mut out).is_ok() {
                    if let Ok(creds) = String::from_utf8(out) {
                        if let Some(idx) = creds.find(':') {
                            let user = creds[..idx].to_string();
                            let pass = creds[idx + 1..].to_string();
                            return Some((user, pass));
                        }
                    }
                }
            }
        }
    }
    None
}

/// Handle the ActiveSync HTTP POST.
pub async fn handle_activesync(
    Extension(state): Extension<Arc<AppState>>,
    headers: HeaderMap,
    body: Bytes,
) -> impl IntoResponse {
    // Decode WBXML or pass-through XML
    let wbxml = Wbxml::new();
    let payload = body.to_vec();
    let xml = match wbxml.decode(&payload) {
        Ok(s) => s,
        Err(e) => {
            return (StatusCode::BAD_REQUEST, format!("Invalid WBXML: {}", e)).into_response();
        }
    };

    let (username, password) = parse_basic_auth(&headers).unwrap_or((String::new(), String::new()));

    // FolderSync: respond with one calendar folder (Type 8, server-id 1)
    if xml.contains("<FolderSync") {
        let resp = r#"<?xml version="1.0" encoding="utf-8"?>
<FolderSync xmlns="FolderHierarchy:">
  <Status>1</Status>
  <Folders>
    <Folder>
      <DisplayName>Calendar</DisplayName>
      <Type>8</Type>
      <ServerId>1</ServerId>
    </Folder>
  </Folders>
</FolderSync>"#;
        return (StatusCode::OK, resp.to_string()).into_response();
    }
    // Sync: return (new) SyncKey and no changes
    else if xml.contains("<Sync") {
        let owner = if !username.is_empty() { username.as_str() } else { "demo" };
        let collection_id = "1";
        match sync::perform_sync(state.clone(), owner, collection_id, "0", 100, &username, &password).await {
            Ok(resp_xml) => return (StatusCode::OK, resp_xml).into_response(),
            Err(e) => {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("Sync error: {}", e),
                ).into_response();
            }
        }
    }
    // Other EAS commands not implemented here
    (StatusCode::BAD_REQUEST, "Unsupported ActiveSync command").into_response()
}
