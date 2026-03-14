// src/eas.rs
use crate::models::AppState;
use crate::sync;
use crate::wbxml::Wbxml;
use axum::extract::State;
use axum::http::HeaderMap;
use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
};
use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use bytes::Bytes;
use std::sync::Arc;

fn parse_basic_auth(headers: &HeaderMap) -> Option<(String, String)> {
    if let Some(v) = headers.get("authorization")
        && let Ok(s) = v.to_str()
    {
        let s = s.trim();
        if s.to_lowercase().starts_with("basic ") {
            let b64 = &s[6..].trim();
            let mut out = Vec::new();
            if BASE64.decode_vec(b64.as_bytes(), &mut out).is_ok()
                && let Ok(creds) = String::from_utf8(out)
                && let Some(idx) = creds.find(':')
            {
                return Some((creds[..idx].to_string(), creds[idx + 1..].to_string()));
            }
        }
    }
    None
}

pub async fn handle(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let wbxml = Wbxml::new();
    let payload = body.to_vec();
    let xml = match wbxml.decode(&payload) {
        Ok(s) => s,
        Err(e) => {
            tracing::error!("WBXML Decode Error: {}", e);
            return (StatusCode::BAD_REQUEST, format!("Invalid WBXML: {}", e)).into_response();
        }
    };

    let (username, password) = parse_basic_auth(&headers).unwrap_or((String::new(), String::new()));

    // Handle FolderSync (initial folder hierarchy).
    if xml.contains("<FolderSync") {
        let resp_xml = r#"<?xml version="1.0" encoding="utf-8"?>
<FolderSync xmlns="FolderHierarchy">
  <Status>1</Status>
  <SyncKey>0</SyncKey>
  <Folders>
    <Folder>
      <ServerId>1</ServerId>
      <ParentId>0</ParentId>
      <DisplayName>Calendar</DisplayName>
      <Type>2</Type>
    </Folder>
  </Folders>
</FolderSync>"#;
        return wbxml_response(&wbxml, resp_xml);
    }

    // Handle device Provision requests.
    if xml.contains("<Provision") {
        let resp_xml = r#"<?xml version="1.0" encoding="utf-8"?>
<Provision>
    <Status>1</Status>
    <Policies>
        <Policy>
            <PolicyType>MS-WAP-Provisioning-XML</PolicyType>
            <Status>1</Status>
            <PolicyKey>12345</PolicyKey>
            <Data>&lt;wap-provisioningdoc/&gt;</Data>
        </Policy>
    </Policies>
</Provision>"#;
        return wbxml_response(&wbxml, resp_xml);
    }

    // Handle Sync (calendar items).
    if xml.contains("<Sync") {
        let owner = if !username.is_empty() {
            username.as_str()
        } else {
            "demo"
        };
        let collection_id = "1";
        // Extract incoming sync key from XML (default "0" if not present).
        let incoming_key = if let Some(start) = xml.find("<SyncKey>") {
            let end = xml.find("</SyncKey>").unwrap_or(start + 9);
            xml[start + 9..end].to_string()
        } else {
            "0".to_string()
        };

        match sync::perform_sync(
            state,
            owner,
            collection_id,
            &incoming_key,
            100,
            &username,
            &password,
        )
        .await
        {
            Ok(resp_xml) => return wbxml_response(&wbxml, &resp_xml),
            Err(e) => {
                tracing::error!("Sync Error: {}", e);
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("Sync error: {}", e),
                )
                    .into_response();
            }
        }
    }

    // Handle Ping (heartbeat).
    if xml.contains("<Ping") {
        let resp_xml = r#"<?xml version="1.0" encoding="utf-8"?><Ping><Status>1</Status></Ping>"#;
        return wbxml_response(&wbxml, resp_xml);
    }

    (StatusCode::BAD_REQUEST, "Unsupported ActiveSync command").into_response()
}

fn wbxml_response(wbxml: &Wbxml, xml: &str) -> Response {
    match wbxml.encode(xml) {
        Ok(b) => (
            StatusCode::OK,
            [("Content-Type", "application/vnd.ms-sync.wbxml")],
            b,
        )
            .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("WBXML Encode Err: {}", e),
        )
            .into_response(),
    }
}
