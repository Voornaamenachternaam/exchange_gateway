// src/ews.rs
use crate::models::AppState;
use crate::storage::EwsItemRow;
use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use quick_xml::Reader;
use quick_xml::events::Event;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Clone, Debug)]
struct AuthContext {
    username: String,
    _password: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum EwsAction {
    GetFolder,
    FindItem,
    SyncFolderItems,
}

pub async fn handle(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    body: String,
) -> Response {
    let auth = match parse_basic_auth(&headers) {
        Some(a) => a,
        None => return unauthorized(),
    };

    let Some(action) = detect_action(&body) else {
        return soap_fault(
            "ErrorInvalidRequest",
            "Could not detect EWS action from SOAP request body",
            StatusCode::BAD_REQUEST,
        );
    };

    if let Err(e) = validate_schema(&action, &body) {
        return soap_fault("ErrorSchemaValidation", e, StatusCode::BAD_REQUEST);
    }

    match action {
        EwsAction::GetFolder => handle_get_folder(&auth).await,
        EwsAction::FindItem => handle_find_item(&state, &auth, &body).await,
        EwsAction::SyncFolderItems => handle_sync_folder_items(&state, &auth, &body).await,
    }
}

fn parse_basic_auth(headers: &HeaderMap) -> Option<AuthContext> {
    let auth = headers.get("authorization")?.to_str().ok()?;
    let b64 = auth.trim().strip_prefix("Basic ")?;
    let mut decoded = Vec::new();
    STANDARD.decode_vec(b64.as_bytes(), &mut decoded).ok()?;
    let pair = String::from_utf8(decoded).ok()?;
    let idx = pair.find(':')?;
    Some(AuthContext {
        username: pair[..idx].to_string(),
        _password: pair[idx + 1..].to_string(),
    })
}

fn detect_action(xml: &str) -> Option<EwsAction> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);
    let mut buf = Vec::new();
    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) => {
                let name = e.name().local_name();
                if name.as_ref() == b"GetFolder" {
                    return Some(EwsAction::GetFolder);
                }
                if name.as_ref() == b"FindItem" {
                    return Some(EwsAction::FindItem);
                }
                if name.as_ref() == b"SyncFolderItems" {
                    return Some(EwsAction::SyncFolderItems);
                }
            }
            Ok(Event::Eof) => return None,
            Err(_) => return None,
            _ => {}
        }
        buf.clear();
    }
}

fn validate_schema(action: &EwsAction, xml: &str) -> Result<(), &'static str> {
    if !xml.contains("Envelope") || !xml.contains("Body") {
        return Err("Missing SOAP Envelope or Body");
    }
    if !xml.contains("http://schemas.microsoft.com/exchange/services/2006/messages")
        && !xml.contains("xmlns:m=")
    {
        return Err("Missing EWS messages namespace");
    }

    match action {
        EwsAction::GetFolder => {
            if !xml.contains("FolderShape") || !xml.contains("FolderIds") {
                return Err("GetFolder requires FolderShape and FolderIds");
            }
            Ok(())
        }
        EwsAction::FindItem => {
            if !xml.contains("ParentFolderIds") || !xml.contains("ItemShape") {
                return Err("FindItem requires ParentFolderIds and ItemShape");
            }
            Ok(())
        }
        EwsAction::SyncFolderItems => {
            if !xml.contains("SyncFolderId") {
                return Err("SyncFolderItems requires SyncFolderId");
            }
            if !xml.contains("MaxChangesReturned") {
                return Err("SyncFolderItems requires MaxChangesReturned");
            }
            Ok(())
        }
    }
}

fn extract_first_tag_text(xml: &str, tag: &[u8]) -> Option<String> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);
    let mut buf = Vec::new();
    let mut inside = false;
    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) if e.name().local_name().as_ref() == tag => inside = true,
            Ok(Event::Text(t)) if inside => return t.decode().ok().map(|v| v.into_owned()),
            Ok(Event::End(e)) if e.name().local_name().as_ref() == tag => inside = false,
            Ok(Event::Eof) | Err(_) => return None,
            _ => {}
        }
        buf.clear();
    }
}

fn extract_int(xml: &str, tag: &[u8], default: usize) -> usize {
    extract_first_tag_text(xml, tag)
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(default)
}

fn owner_from_username(username: &str) -> &str {
    username
}

fn folder_id_for_owner(owner: &str) -> String {
    let mut h = Sha256::new();
    h.update(owner.as_bytes());
    h.update(b"/calendar");
    let digest = h.finalize();
    format!(
        "CAL-{}",
        digest[..12]
            .iter()
            .map(|b| format!("{:02x}", b))
            .collect::<String>()
    )
}

fn changekey_for_item(item: &EwsItemRow) -> String {
    let mut h = Sha256::new();
    h.update(item.server_id.as_bytes());
    if let Some(e) = &item.etag {
        h.update(e.as_bytes());
    }
    if let Some(u) = &item.updated_at {
        h.update(u.as_bytes());
    }
    let digest = h.finalize();
    digest[..12].iter().map(|b| format!("{:02x}", b)).collect()
}

fn xml_escape(v: &str) -> String {
    v.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

fn unauthorized() -> Response {
    (
        StatusCode::UNAUTHORIZED,
        [("WWW-Authenticate", "Basic realm=\"EWS\"")],
        "Unauthorized",
    )
        .into_response()
}

fn soap_ok(inner: String) -> Response {
    let xml = format!(
        r#"<?xml version="1.0" encoding="utf-8"?>
<s:Envelope xmlns:s="http://schemas.xmlsoap.org/soap/envelope/">
  <s:Body>
    {inner}
  </s:Body>
</s:Envelope>"#
    );
    (
        StatusCode::OK,
        [("Content-Type", "text/xml; charset=utf-8")],
        xml,
    )
        .into_response()
}

fn soap_fault(code: &str, message: &str, status: StatusCode) -> Response {
    let xml = format!(
        r#"<?xml version="1.0" encoding="utf-8"?>
<s:Envelope xmlns:s="http://schemas.xmlsoap.org/soap/envelope/">
  <s:Body>
    <s:Fault>
      <faultcode>s:Client</faultcode>
      <faultstring>{}</faultstring>
      <detail><m:ResponseCode xmlns:m="http://schemas.microsoft.com/exchange/services/2006/messages">{}</m:ResponseCode></detail>
    </s:Fault>
  </s:Body>
</s:Envelope>"#,
        xml_escape(message),
        xml_escape(code)
    );
    (status, [("Content-Type", "text/xml; charset=utf-8")], xml).into_response()
}

async fn handle_get_folder(auth: &AuthContext) -> Response {
    let fid = folder_id_for_owner(owner_from_username(&auth.username));
    let response = format!(
        r#"<m:GetFolderResponse xmlns:m="http://schemas.microsoft.com/exchange/services/2006/messages" xmlns:t="http://schemas.microsoft.com/exchange/services/2006/types">
  <m:ResponseMessages>
    <m:GetFolderResponseMessage ResponseClass="Success">
      <m:ResponseCode>NoError</m:ResponseCode>
      <m:Folders>
        <t:CalendarFolder>
          <t:FolderId Id="{}" ChangeKey="{}" />
          <t:DisplayName>Calendar</t:DisplayName>
          <t:TotalCount>0</t:TotalCount>
        </t:CalendarFolder>
      </m:Folders>
    </m:GetFolderResponseMessage>
  </m:ResponseMessages>
</m:GetFolderResponse>"#,
        fid,
        &fid[4..]
    );
    soap_ok(response)
}

async fn handle_find_item(state: &Arc<AppState>, auth: &AuthContext, body: &str) -> Response {
    let owner = owner_from_username(&auth.username);
    let max = extract_int(body, b"MaxEntriesReturned", 50).min(512);
    let offset = extract_int(body, b"Offset", 0);

    let items = match state.storage.list_ews_items(owner, max, offset).await {
        Ok(v) => v,
        Err(e) => {
            return soap_fault(
                "ErrorInternalServerError",
                &format!("Failed to query items: {}", e),
                StatusCode::INTERNAL_SERVER_ERROR,
            );
        }
    };

    let folder_id = folder_id_for_owner(owner);
    let mut item_xml = String::new();
    for item in &items {
        let change_key = changekey_for_item(item);
        let subject = item
            .uid
            .clone()
            .unwrap_or_else(|| item.resource_href.clone())
            .replace(".ics", "");
        item_xml.push_str(&format!(
            r#"<t:CalendarItem>
  <t:ItemId Id="{}" ChangeKey="{}" />
  <t:Subject>{}</t:Subject>
  <t:UID>{}</t:UID>
</t:CalendarItem>"#,
            xml_escape(&item.server_id),
            xml_escape(&change_key),
            xml_escape(&subject),
            xml_escape(item.uid.as_deref().unwrap_or(&item.server_id))
        ));
    }

    let includes_last = if items.len() < max { "true" } else { "false" };
    let next_offset = offset + items.len();
    let response = format!(
        r#"<m:FindItemResponse xmlns:m="http://schemas.microsoft.com/exchange/services/2006/messages" xmlns:t="http://schemas.microsoft.com/exchange/services/2006/types">
  <m:ResponseMessages>
    <m:FindItemResponseMessage ResponseClass="Success">
      <m:ResponseCode>NoError</m:ResponseCode>
      <m:RootFolder TotalItemsInView="{}" IncludesLastItemInRange="{}" IndexedPagingOffset="{}">
        <t:Items>{}</t:Items>
      </m:RootFolder>
    </m:FindItemResponseMessage>
  </m:ResponseMessages>
</m:FindItemResponse>"#,
        items.len(),
        includes_last,
        next_offset,
        item_xml
    );

    let _ = state
        .storage
        .set_ews_sync_state(owner, &folder_id, &format!("offset:{}", next_offset))
        .await;

    soap_ok(response)
}

fn parse_sync_state_marker(marker: Option<String>) -> i64 {
    marker
        .and_then(|m| {
            if let Some(v) = m.strip_prefix("ts:") {
                return v.parse::<i64>().ok();
            }
            None
        })
        .unwrap_or(0)
}

fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

async fn handle_sync_folder_items(
    state: &Arc<AppState>,
    auth: &AuthContext,
    body: &str,
) -> Response {
    let owner = owner_from_username(&auth.username);
    let max_changes = extract_int(body, b"MaxChangesReturned", 100).min(512);
    let folder_id = folder_id_for_owner(owner);

    let requested_state = extract_first_tag_text(body, b"SyncState");
    let effective_state = if requested_state.as_deref().unwrap_or("0").is_empty() {
        match state.storage.get_ews_sync_state(owner, &folder_id).await {
            Ok(v) => v,
            Err(_) => None,
        }
    } else {
        requested_state
    };

    let since = parse_sync_state_marker(effective_state);
    let changed_ids = match state.storage.list_changes_since(owner, since).await {
        Ok(v) => v,
        Err(e) => {
            return soap_fault(
                "ErrorInternalServerError",
                &format!("Failed to query changes: {}", e),
                StatusCode::INTERNAL_SERVER_ERROR,
            );
        }
    };

    let changed_map: HashMap<_, _> = changed_ids.into_iter().take(max_changes).collect();
    let items = match state.storage.list_ews_items(owner, max_changes, 0).await {
        Ok(v) => v,
        Err(e) => {
            return soap_fault(
                "ErrorInternalServerError",
                &format!("Failed to query current items: {}", e),
                StatusCode::INTERNAL_SERVER_ERROR,
            );
        }
    };

    let mut changes_xml = String::new();
    for item in items {
        if !changed_map.contains_key(&item.server_id) {
            continue;
        }
        let change_key = changekey_for_item(&item);
        let subject = item
            .uid
            .clone()
            .unwrap_or_else(|| item.resource_href.clone())
            .replace(".ics", "");
        changes_xml.push_str(&format!(
            r#"<t:Create>
  <t:CalendarItem>
    <t:ItemId Id="{}" ChangeKey="{}" />
    <t:Subject>{}</t:Subject>
    <t:UID>{}</t:UID>
  </t:CalendarItem>
</t:Create>"#,
            xml_escape(&item.server_id),
            xml_escape(&change_key),
            xml_escape(&subject),
            xml_escape(item.uid.as_deref().unwrap_or(&item.server_id))
        ));
    }

    let new_sync_state = format!("ts:{}", now_unix());
    let _ = state
        .storage
        .set_ews_sync_state(owner, &folder_id, &new_sync_state)
        .await;

    let response = format!(
        r#"<m:SyncFolderItemsResponse xmlns:m="http://schemas.microsoft.com/exchange/services/2006/messages" xmlns:t="http://schemas.microsoft.com/exchange/services/2006/types">
  <m:ResponseMessages>
    <m:SyncFolderItemsResponseMessage ResponseClass="Success">
      <m:ResponseCode>NoError</m:ResponseCode>
      <m:SyncState>{}</m:SyncState>
      <m:IncludesLastItemInRange>true</m:IncludesLastItemInRange>
      <m:Changes>{}</m:Changes>
    </m:SyncFolderItemsResponseMessage>
  </m:ResponseMessages>
</m:SyncFolderItemsResponse>"#,
        xml_escape(&new_sync_state),
        changes_xml
    );

    soap_ok(response)
}
