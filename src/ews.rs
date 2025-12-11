use axum::{extract::Extension, http::StatusCode, response::{IntoResponse, Response}};
use axum::http::HeaderMap;
use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine;
use bytes::Bytes;
use quick_xml::Reader;
use quick_xml::events::Event;
use std::sync::Arc;
use crate::models::AppState;
use crate::ews_marshaller;
use crate::caldav::CaldavClient;
use crate::sync;
use crate::utils;
use anyhow::Result;

fn parse_basic_auth(headers: &HeaderMap) -> Option<(String,String)> {
    if let Some(v) = headers.get("authorization") {
        if let Ok(s) = v.to_str() {
            let s = s.trim();
            if s.to_lowercase().starts_with("basic ") {
                let b64 = s[6..].trim();
                let mut out = Vec::new();
                if BASE64.decode_vec(b64.as_bytes(), &mut out).is_ok() {
                    if let Ok(creds) = String::from_utf8(out) {
                        if let Some(idx) = creds.find(':') {
                            let user = creds[..idx].to_string();
                            let pass = creds[idx+1..].to_string();
                            return Some((user, pass));
                        }
                    }
                }
            }
        }
    }
    None
}

pub async fn handle_ews(Extension(state): Extension<Arc<AppState>>, headers: HeaderMap, body: Bytes) -> Response {
    let (auth_user, auth_pass) = parse_basic_auth(&headers).unwrap_or((String::new(), String::new()));
    let xml = String::from_utf8_lossy(&body).to_string();
    let mut reader = Reader::from_str(&xml);
    // Use read_event_into API
    let mut buf = Vec::new();
    let mut op: Option<String> = None;
    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) => {
                if let Ok(name) = std::str::from_utf8(e.local_name().as_ref()) {
                    if name.ends_with("FindItem") { op = Some("FindItem".to_string()); break; }
                    if name.ends_with("GetItem") { op = Some("GetItem".to_string()); break; }
                    if name.ends_with("CreateItem") { op = Some("CreateItem".to_string()); break; }
                    if name.ends_with("UpdateItem") { op = Some("UpdateItem".to_string()); break; }
                    if name.ends_with("DeleteItem") { op = Some("DeleteItem".to_string()); break; }
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

async fn handle_create_item(state: Arc<AppState>, xml: &str, user:&str, password:&str) -> Response {
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
            let coll = calendars.get(0).unwrap().clone();
            let resource_name = format!("{}.ics", uuid::Uuid::new_v4().to_string());
            match caldav.put_event(&coll, &resource_name, &ics, owner, password).await {
                Ok(etag) => {
                    let resource_href = format!("{}/{}", coll.trim_end_matches('/'), resource_name);
                    let server_id = sync::generate_server_id(&state.cfg.hmac_secret, &resource_href);
                    let _ = state.storage.upsert_item_map(owner, &coll, &resource_href, &server_id, "uid-placeholder", &etag).await;
                    let change_key = sync::generate_change_key(&etag);
                    let resp_body = format!(r#"<m:CreateItemResponse xmlns:m="http://schemas.microsoft.com/exchange/services/2006/messages"><m:ResponseMessages><m:CreateItemResponseMessage ResponseClass="Success"><m:Items><t:CalendarItem xmlns:t="http://schemas.microsoft.com/exchange/services/2006/types"><t:ItemId Id="{id}" ChangeKey="{ck}"/></t:CalendarItem></m:Items></m:CreateItemResponseMessage></m:ResponseMessages></m:CreateItemResponse>"#, id=server_id, ck=change_key);
                    let soap = utils::ews_soap_envelope(&resp_body);
                    return (StatusCode::OK, soap).into_response();
                }
                Err(e) => return (StatusCode::BAD_GATEWAY, format!("CalDAV put error: {}", e)).into_response(),
            }
        }
        Err(e) => return (StatusCode::BAD_REQUEST, format!("Invalid EWS CalendarItem: {}", e)).into_response(),
    }
}

async fn handle_get_item(_state: Arc<AppState>, _xml: &str, _user:&str, _pass:&str) -> Response {
    let body = "<m:GetItemResponse xmlns:m=\"http://schemas.microsoft.com/exchange/services/2006/messages\"></m:GetItemResponse>";
    let soap = crate::utils::ews_soap_envelope(body);
    (StatusCode::OK, soap).into_response()
}

async fn handle_update_item(_state: Arc<AppState>, _xml: &str, _user:&str, _pass:&str) -> Response {
    let body = "<m:UpdateItemResponse xmlns:m=\"http://schemas.microsoft.com/exchange/services/2006/messages\"></m:UpdateItemResponse>";
    let soap = crate::utils::ews_soap_envelope(body);
    (StatusCode::OK, soap).into_response()
}

async fn handle_delete_item(_state: Arc<AppState>, _xml: &str, _user:&str, _pass:&str) -> Response {
    let body = "<m:DeleteItemResponse xmlns:m=\"http://schemas.microsoft.com/exchange/services/2006/messages\"></m:DeleteItemResponse>";
    let soap = crate::utils::ews_soap_envelope(body);
    (StatusCode::OK, soap).into_response()
}
