use crate::caldav::CaldavClient;
use crate::ews_marshaller;
use crate::models::AppState;
use crate::sync;
use crate::utils;
use axum::http::HeaderMap;
use axum::{extract::Extension, http::StatusCode, response::IntoResponse};
use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use bytes::Bytes;
use quick_xml::events::Event;
use quick_xml::Reader;
use std::sync::Arc;
use chrono::Utc;

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

pub async fn handle_ews(
    Extension(state): Extension<Arc<AppState>>,
    headers: HeaderMap,
    body: Bytes,
) -> impl IntoResponse {
    let (auth_user, auth_pass) = parse_basic_auth(&headers)
        .unwrap_or((String::new(), String::new()));
    let xml = String::from_utf8_lossy(&body).to_string();

    // Determine operation from XML root element
    let mut reader = Reader::from_str(&xml);
    let mut buf = Vec::new();
    let mut op: Option<String> = None;
    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) => {
                if let Ok(name) = std::str::from_utf8(e.local_name().as_ref()) {
                    if name.ends_with("FindItem") {
                        op = Some("FindItem".to_string());
                        break;
                    }
                    if name.ends_with("GetItem") {
                        op = Some("GetItem".to_string());
                        break;
                    }
                    if name.ends_with("CreateItem") {
                        op = Some("CreateItem".to_string());
                        break;
                    }
                    if name.ends_with("UpdateItem") {
                        op = Some("UpdateItem".to_string());
                        break;
                    }
                    if name.ends_with("DeleteItem") {
                        op = Some("DeleteItem".to_string());
                        break;
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
        Some("FindItem") => handle_find_item(state, &xml, &auth_user, &auth_pass).await,
        Some("CreateItem") => handle_create_item(state, &xml, &auth_user, &auth_pass).await,
        Some("GetItem")    => handle_get_item(state, &xml, &auth_user, &auth_pass).await,
        Some("UpdateItem") => handle_update_item(state, &xml, &auth_user, &auth_pass).await,
        Some("DeleteItem") => handle_delete_item(state, &xml, &auth_user, &auth_pass).await,
        _ => (StatusCode::BAD_REQUEST, "Unsupported EWS operation").into_response(),
    }
}

async fn handle_create_item(
    state: Arc<AppState>,
    xml: &str,
    user: &str,
    password: &str,
) -> impl IntoResponse {
    match ews_marshaller::ews_calendaritem_to_ics(xml) {
        Ok(ics) => {
            let owner = if !user.is_empty() { user } else { "demo" };
            let caldav = CaldavClient::new(&state.cfg);
            // Discover user calendar
            let calendars = match caldav.find_user_calendars(owner, password).await {
                Ok(c) => c,
                Err(e) => {
                    return (StatusCode::BAD_GATEWAY, format!("CalDAV error: {}", e))
                        .into_response();
                }
            };
            let coll = calendars.first().unwrap().clone();
            let resource_name = format!("{}.ics", uuid::Uuid::new_v4());
            match caldav.put_event(&coll, &resource_name, &ics, owner, password).await {
                Ok(etag) => {
                    let resource_href = format!("{}/{}", coll.trim_end_matches('/'), resource_name);
                    // Compute a unique server ID for the event
                    let server_id = sync::generate_server_id(&state.cfg.hmac_secret, &resource_href);
                    // Store mapping in D1 via Worker
                    let _ = state.storage.upsert_item_map(owner, &coll, &resource_href, &server_id, "uid", &etag).await;
                    let change_key = sync::generate_change_key(&etag);
                    let resp_body = format!(
                        r#"<m:CreateItemResponse xmlns:m="http://schemas.microsoft.com/exchange/services/2006/messages">
                           <m:ResponseMessages>
                             <m:CreateItemResponseMessage ResponseClass="Success">
                               <m:Items>
                                 <t:CalendarItem xmlns:t="http://schemas.microsoft.com/exchange/services/2006/types">
                                   <t:ItemId Id="{id}" ChangeKey="{ck}"/>
                                 </t:CalendarItem>
                               </m:Items>
                             </m:CreateItemResponseMessage>
                           </m:ResponseMessages>
                         </m:CreateItemResponse>"#,
                        id = server_id,
                        ck = change_key
                    );
                    let soap = utils::ews_soap_envelope(&resp_body);
                    (StatusCode::OK, soap).into_response()
                }
                Err(e) => (StatusCode::BAD_GATEWAY, format!("CalDAV put error: {}", e)).into_response(),
            }
        }
        Err(e) => (
            StatusCode::BAD_REQUEST,
            format!("Invalid EWS CalendarItem: {}", e),
        )
            .into_response(),
    }
}

async fn handle_get_item(
    _state: Arc<AppState>,
    _xml: &str,
    _user: &str,
    _pass: &str,
) -> impl IntoResponse {
    // For simplicity, return an empty stub response
    let body = "<m:GetItemResponse xmlns:m=\"http://schemas.microsoft.com/exchange/services/2006/messages\"></m:GetItemResponse>";
    let soap = utils::ews_soap_envelope(body);
    (StatusCode::OK, soap).into_response()
}

async fn handle_update_item(
    state: Arc<AppState>,
    xml: &str,
    user: &str,
    password: &str,
) -> impl IntoResponse {
    // Similar to CreateItem, but update existing event
    if let Some(start) = xml.find("t:ItemId") {
        // Extract server_id from ItemId
        if let Some(id_start) = xml[start..].find("Id=\"") {
            let id_field = &xml[start + id_start + 4..];
            if let Some(id_end) = id_field.find('"') {
                let server_id = &id_field[..id_end];
                // Convert new content to ICS
                match ews_marshaller::ews_calendaritem_to_ics(xml) {
                    Ok(ics) => {
                        let owner = if !user.is_empty() { user } else { "demo" };
                        // Lookup resource by server_id
                        if let Ok(Some((_, resource_href))) = state.storage.get_item_by_server_id(server_id).await {
                            // Determine collection URL and resource name
                            if let Some(pos) = resource_href.rfind('/') {
                                let coll = &resource_href[..pos];
                                let res_name = &resource_href[pos+1..];
                                // Perform CalDAV PUT to update
                                match CaldavClient::new(&state.cfg).put_event(coll, res_name, &ics, owner, password).await {
                                    Ok(etag) => {
                                        let change_key = sync::generate_change_key(&etag);
                                        // Update mapping (last_sync, etag)
                                        let _ = state.storage.upsert_item_map(owner, coll, &resource_href, server_id, "uid", &etag).await;
                                        let resp_body = format!(
                                            r#"<m:UpdateItemResponse xmlns:m="http://schemas.microsoft.com/exchange/services/2006/messages">
                                               <m:ResponseMessages>
                                                 <m:UpdateItemResponseMessage ResponseClass="Success">
                                                   <m:Items>
                                                     <t:CalendarItem xmlns:t="http://schemas.microsoft.com/exchange/services/2006/types">
                                                       <t:ItemId Id="{id}" ChangeKey="{ck}"/>
                                                     </t:CalendarItem>
                                                   </m:Items>
                                                 </m:UpdateItemResponseMessage>
                                               </m:ResponseMessages>
                                             </m:UpdateItemResponse>"#,
                                            id = server_id,
                                            ck = change_key
                                        );
                                        let soap = utils::ews_soap_envelope(&resp_body);
                                        return (StatusCode::OK, soap).into_response();
                                    }
                                    Err(e) => return (StatusCode::BAD_GATEWAY, format!("CalDAV put error: {}", e)).into_response(),
                                }
                            }
                        }
                    }
                    Err(e) => {
                        return (StatusCode::BAD_REQUEST, format!("Invalid EWS CalendarItem: {}", e)).into_response();
                    }
                }
            }
        }
    }
    // If anything fails, return error
    (StatusCode::BAD_REQUEST, "Failed to update item").into_response()
}

async fn handle_delete_item(
    state: Arc<AppState>,
    xml: &str,
    user: &str,
    password: &str,
) -> impl IntoResponse {
    // Extract server_id from ItemId
    if let Some(start) = xml.find("t:ItemId") {
        if let Some(id_start) = xml[start..].find("Id=\"") {
            let id_field = &xml[start + id_start + 4..];
            if let Some(id_end) = id_field.find('"') {
                let server_id = &id_field[..id_end];
                let owner = if !user.is_empty() { user } else { "demo" };
                // Lookup resource by server_id
                if let Ok(Some((_, resource_href))) = state.storage.get_item_by_server_id(server_id).await {
                    // Perform CalDAV DELETE
                    let res = CaldavClient::new(&state.cfg).delete_event(&resource_href, owner, password).await;
                    // Remove mapping
                    let _ = state.storage.delete_item_by_server_id(server_id).await;
                    if res.is_ok() {
                        let body = r#"<m:DeleteItemResponse xmlns:m="http://schemas.microsoft.com/exchange/services/2006/messages"></m:DeleteItemResponse>"#;
                        let soap = utils::ews_soap_envelope(body);
                        return (StatusCode::OK, soap).into_response();
                    }
                }
            }
        }
    }
    (StatusCode::BAD_REQUEST, "Failed to delete item").into_response()
}

async fn handle_find_item(
    state: Arc<AppState>,
    _xml: &str,
    user: &str,
    password: &str,
) -> impl IntoResponse {
    // List all items (changes since epoch)
    let owner = if !user.is_empty() { user } else { "demo" };
    let caldav = CaldavClient::new(&state.cfg);
    let calendars = match caldav.find_user_calendars(owner, password).await {
        Ok(c) => c,
        Err(_) => vec![format!("{}cal/{}", state.cfg.caldav_base.trim_end_matches('/'), owner)],
    };
    let coll = calendars.first().unwrap().clone();

    // Define a broad date range for query
    let start = (Utc::now() - chrono::Duration::weeks(52)).format("%Y%m%dT%H%M%SZ").to_string();
    let end   = (Utc::now() + chrono::Duration::weeks(52)).format("%Y%m%dT%H%M%SZ").to_string();
    let _ = caldav.query_events(&coll, &start, &end, owner, password).await;

    // Retrieve all mapped items from storage
    let items = match state.storage.list_changes_since(owner, 0).await {
        Ok(v) => v,
        Err(_) => Vec::new(),
    };

    // Build <FindItemResponse> with each CalendarItem
    let mut items_xml = String::new();
    for (server_id, resource_href) in items.iter() {
        if let Ok(ics) = caldav.get_event(resource_href, owner, password).await {
            // Parse ICS fields
            let mut summary = "Event".to_string();
            let mut start_str = Utc::now().format("%Y%m%dT%H%M%SZ").to_string();
            let mut end_str = start_str.clone();
            let mut location = "".to_string();
            for line in ics.lines() {
                if let Some(val) = line.strip_prefix("SUMMARY:") { summary = escape_xml(val); }
                if let Some(val) = line.strip_prefix("DTSTART:") { start_str = val.to_string(); }
                if let Some(val) = line.strip_prefix("DTEND:")   { end_str = val.to_string(); }
                if let Some(val) = line.strip_prefix("LOCATION:"){ location = escape_xml(val); }
            }
            let change_key = sync::generate_change_key(&state.storage.get_item_by_server_id(server_id).await.unwrap().unwrap().1);
            items_xml.push_str(&format!(
                r#"<t:CalendarItem xmlns:t="http://schemas.microsoft.com/exchange/services/2006/types">
                     <t:ItemId Id="{id}" ChangeKey="{ck}"/>
                     <t:Subject>{subject}</t:Subject>
                     <t:Start>{start}</t:Start>
                     <t:End>{end}</t:End>
                     <t:Location>{location}</t:Location>
                   </t:CalendarItem>"#,
                id = server_id,
                ck = change_key,
                subject = summary,
                start = start_str,
                end = end_str,
                location = location
            ));
        }
    }

    let resp_body = format!(
        r#"<m:FindItemResponse xmlns:m="http://schemas.microsoft.com/exchange/services/2006/messages">
             <m:ResponseMessages>
               <m:FindItemResponseMessage ResponseClass="Success">
                 <m:RootFolder>
                   <t:IncludesLastItemInRange>true</t:IncludesLastItemInRange>
                   <t:TotalItemsInView>{}</t:TotalItemsInView>
                   <m:Items>{}</m:Items>
                 </m:RootFolder>
               </m:FindItemResponseMessage>
             </m:ResponseMessages>
           </m:FindItemResponse>"#,
        items.len(),
        items_xml
    );
    let soap = crate::utils::ews_soap_envelope(&resp_body);
    (StatusCode::OK, soap).into_response()
}

/// Simple XML escaper for EWS text elements.
fn escape_xml(s: &str) -> String {
    s.replace("&", "&amp;").replace("<", "&lt;").replace(">", "&gt;")
}
