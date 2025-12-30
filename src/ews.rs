// src/ews.rs

use crate::caldav::CaldavClient;
use crate::ews_marshaller;
use crate::models::AppState;
use crate::sync;
use crate::utils;
use axum::http::HeaderMap;
use axum::{extract::Extension, http::StatusCode, response::{IntoResponse, Response}};
use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use bytes::Bytes;
use quick_xml::Reader;
use quick_xml::events::Event;
use std::sync::Arc;

/// Parse Basic auth (same as EAS).
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

/// Handle EWS SOAP POST.
pub async fn handle_ews(
    Extension(state): Extension<Arc<AppState>>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let (auth_user, auth_pass) = parse_basic_auth(&headers).unwrap_or((String::new(), String::new()));
    let xml = String::from_utf8_lossy(&body).to_string();
    // Determine EWS operation by XML tag
    let mut reader = Reader::from_str(&xml);
    let mut buf = Vec::new();
    let mut op: Option<String> = None;
    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) => {
                if let Ok(name) = std::str::from_utf8(e.local_name().as_ref()) {
                    if name.ends_with("CreateItem") {
                        op = Some("CreateItem".to_string()); break;
                    }
                    if name.ends_with("GetItem") {
                        op = Some("GetItem".to_string()); break;
                    }
                    if name.ends_with("UpdateItem") {
                        op = Some("UpdateItem".to_string()); break;
                    }
                    if name.ends_with("DeleteItem") {
                        op = Some("DeleteItem".to_string()); break;
                    }
                }
            }
            Ok(Event::Eof) => break,
            Err(_) => break,
            _ => {}
        }
        buf.clear();
    }
    match op.as_deref() {
        Some("CreateItem") => handle_create_item(state, &xml, &auth_user, &auth_pass).await,
        Some("GetItem") => handle_get_item(state, &xml, &auth_user, &auth_pass).await,
        Some("UpdateItem") => handle_update_item(state, &xml, &auth_user, &auth_pass).await,
        Some("DeleteItem") => handle_delete_item(state, &xml, &auth_user, &auth_pass).await,
        _ => (StatusCode::BAD_REQUEST, "Unsupported EWS operation").into_response(),
    }
}

/// Handle CreateItem for a CalendarItem: convert EWS XML to ICS and PUT to CalDAV.
async fn handle_create_item(
    state: Arc<AppState>,
    xml: &str,
    user: &str,
    password: &str,
) -> Response {
    match ews_marshaller::ews_calendaritem_to_ics(xml) {
        Ok(ics) => {
            let owner = if !user.is_empty() { user } else { "demo" };
            let caldav = CaldavClient::new(&state.cfg);
            let calendars = match caldav.find_user_calendars(owner, password).await {
                Ok(c) => c,
                Err(e) => {
                    return (StatusCode::BAD_GATEWAY, format!("CalDAV error: {}", e)).into_response();
                }
            };
            let coll = calendars.first().unwrap().clone();
            let resource_name = format!("{}.ics", uuid::Uuid::new_v4());
            match caldav.put_event(&coll, &resource_name, &ics, owner, password).await {
                Ok(etag) => {
                    let resource_href = format!("{}/{}", coll.trim_end_matches('/'), resource_name);
                    let server_id = sync::generate_server_id(&state.cfg.hmac_secret, &resource_href);
                    let _ = state
                        .storage
                        .upsert_item_map(owner, &coll, &resource_href, &server_id, "uid-placeholder", &etag)
                        .await;
                    let change_key = sync::generate_change_key(&etag);
                    let resp_body = format!(
                        r#"<m:CreateItemResponse xmlns:m="http://schemas.microsoft.com/exchange/services/2006/messages" xmlns:t="http://schemas.microsoft.com/exchange/services/2006/types"><m:ResponseMessages><m:CreateItemResponseMessage ResponseClass="Success"><m:ResponseCode>NoError</m:ResponseCode><m:Items><t:CalendarItem><t:ItemId Id="{id}" ChangeKey="{ck}"/></t:CalendarItem></m:Items></m:CreateItemResponseMessage></m:ResponseMessages></m:CreateItemResponse>"#,
                        id = server_id,
                        ck = change_key
                    );
                    let soap = utils::ews_soap_envelope(&resp_body);
                    (StatusCode::OK, soap).into_response()
                }
                Err(e) => {
                    (StatusCode::BAD_GATEWAY, format!("CalDAV error: {}", e)).into_response()
                }
            }
        }
        Err(e) => {
            (StatusCode::BAD_REQUEST, format!("Invalid CalendarItem XML: {}", e)).into_response()
        }
    }
}

/// Handle GetItem: fetch ICS from CalDAV and return basic fields.
async fn handle_get_item(
    state: Arc<AppState>,
    xml: &str,
    user: &str,
    password: &str,
) -> Response {
    // Extract server_id from <ItemId Id="...">
    if let Some(id_idx) = xml.find("Id=\"") {
        if let Some(end_quote) = xml[id_idx+4..].find('"') {
            let server_id = &xml[id_idx+4..id_idx+4+end_quote];
            if let Ok(Some((_id, href))) = state.storage.get_item_by_server_id(server_id).await {
                let caldav = CaldavClient::new(&state.cfg);
                if let Ok(ics) = caldav.get_event(&href, user, password).await {
                    // Extract simple fields from ICS
                    let summary = ics.split("SUMMARY:").nth(1).unwrap_or("").split("\\r\\n").next().unwrap_or("");
                    let dtstart = ics.split("DTSTART:").nth(1).unwrap_or("").split("\\r\\n").next().unwrap_or("");
                    let dtend = ics.split("DTEND:").nth(1).unwrap_or("").split("\\r\\n").next().unwrap_or("");
                    let resp_body = format!(
                        r#"<m:GetItemResponse xmlns:m="http://schemas.microsoft.com/exchange/services/2006/messages" xmlns:t="http://schemas.microsoft.com/exchange/services/2006/types"><m:ResponseMessages><m:GetItemResponseMessage ResponseClass="Success"><m:ResponseCode>NoError</m:ResponseCode><m:Items><t:CalendarItem><t:Subject>{}</t:Subject><t:Start>{}</t:Start><t:End>{}</t:End><t:ItemId Id="{id}" ChangeKey="0"/></t:CalendarItem></m:Items></m:GetItemResponseMessage></m:ResponseMessages></m:GetItemResponse>"#,
                        summary, dtstart, dtend,
                        id = server_id
                    );
                    let soap = utils::ews_soap_envelope(&resp_body);
                    return (StatusCode::OK, soap).into_response();
                }
            }
        }
    }
    (StatusCode::NOT_FOUND, "Item not found").into_response()
}

/// Handle UpdateItem: overwrite the existing CalDAV event.
async fn handle_update_item(
    state: Arc<AppState>,
    xml: &str,
    user: &str,
    password: &str,
) -> Response {
    // Extract server_id from <ItemId Id="...">
    if let Some(id_idx) = xml.find("ItemId Id=\"") {
        if let Some(end_quote) = xml[id_idx+11..].find('"') {
            let server_id = &xml[id_idx+11..id_idx+11+end_quote];
            if let Ok(Some((_id, href))) = state.storage.get_item_by_server_id(server_id).await {
                if let Ok(ics) = ews_marshaller::ews_calendaritem_to_ics(xml) {
                    let caldav = CaldavClient::new(&state.cfg);
                    if let Some(pos) = href.rfind('/') {
                        let coll = &href[..pos];
                        let resource_name = &href[pos+1..];
                        if caldav.put_event(coll, resource_name, &ics, user, password).await.is_ok() {
                            let resp_body = r#"<m:UpdateItemResponse xmlns:m="http://schemas.microsoft.com/exchange/services/2006/messages"><m:ResponseMessages><m:UpdateItemResponseMessage ResponseClass="Success"><m:ResponseCode>NoError</m:ResponseCode></m:UpdateItemResponseMessage></m:ResponseMessages></m:UpdateItemResponse>"#;
                            let soap = utils::ews_soap_envelope(resp_body);
                            return (StatusCode::OK, soap).into_response();
                        }
                    }
                }
            }
        }
    }
    (StatusCode::BAD_REQUEST, "Update failed").into_response()
}

/// Handle DeleteItem: remove the event from CalDAV.
async fn handle_delete_item(
    state: Arc<AppState>,
    xml: &str,
    user: &str,
    password: &str,
) -> Response {
    if let Some(id_idx) = xml.find("ItemId Id=\"") {
        if let Some(end_quote) = xml[id_idx+11..].find('"') {
            let server_id = &xml[id_idx+11..id_idx+11+end_quote];
            if let Ok(Some((_id, href))) = state.storage.get_item_by_server_id(server_id).await {
                let caldav = CaldavClient::new(&state.cfg);
                if caldav.delete_event(&href, user, password).await.is_ok() {
                    let _ = state.storage.delete_item_by_server_id(server_id).await;
                    let resp_body = r#"<m:DeleteItemResponse xmlns:m="http://schemas.microsoft.com/exchange/services/2006/messages"><m:ResponseMessages><m:DeleteItemResponseMessage ResponseClass="Success"><m:ResponseCode>NoError</m:ResponseCode></m:DeleteItemResponseMessage></m:ResponseMessages></m:DeleteItemResponse>"#;
                    let soap = utils::ews_soap_envelope(resp_body);
                    return (StatusCode::OK, soap).into_response();
                }
            }
        }
    }
    (StatusCode::BAD_REQUEST, "Delete failed").into_response()
}
