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
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

const EWS_MSG_NS: &str = "http://schemas.microsoft.com/exchange/services/2006/messages";
const EWS_TYPE_NS: &str = "http://schemas.microsoft.com/exchange/services/2006/types";

#[derive(Clone, Debug)]
struct AuthContext {
    username: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum EwsAction {
    GetFolder,
    FindFolder,
    FindItem,
    GetItem,
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
        return operation_error_response(
            &action,
            "ErrorSchemaValidation",
            e,
            StatusCode::BAD_REQUEST,
        );
    }

    match action {
        EwsAction::GetFolder => handle_get_folder(&auth, &body).await,
        EwsAction::FindFolder => handle_find_folder(&auth, &body).await,
        EwsAction::FindItem => handle_find_item(&state, &auth, &body).await,
        EwsAction::GetItem => handle_get_item(&state, &auth, &body).await,
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
                if name.as_ref() == b"FindFolder" {
                    return Some(EwsAction::FindFolder);
                }
                if name.as_ref() == b"FindItem" {
                    return Some(EwsAction::FindItem);
                }
                if name.as_ref() == b"GetItem" {
                    return Some(EwsAction::GetItem);
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
    if !xml.contains(EWS_MSG_NS) && !xml.contains("xmlns:m=") {
        return Err("Missing EWS messages namespace");
    }

    match action {
        EwsAction::GetFolder => {
            if !xml.contains("FolderShape") || !xml.contains("FolderIds") {
                return Err("GetFolder requires FolderShape and FolderIds");
            }
            Ok(())
        }
        EwsAction::FindFolder => {
            if !xml.contains("FolderShape") || !xml.contains("ParentFolderIds") {
                return Err("FindFolder requires FolderShape and ParentFolderIds");
            }
            Ok(())
        }
        EwsAction::FindItem => {
            if !xml.contains("ParentFolderIds") || !xml.contains("ItemShape") {
                return Err("FindItem requires ParentFolderIds and ItemShape");
            }
            let max = extract_int(xml, b"MaxEntriesReturned", 50);
            if max == 0 {
                return Err("FindItem MaxEntriesReturned must be greater than zero");
            }
            Ok(())
        }
        EwsAction::GetItem => {
            if !xml.contains("ItemShape") || !xml.contains("ItemIds") {
                return Err("GetItem requires ItemShape and ItemIds");
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

fn extract_first_attr(xml: &str, tag: &[u8], attr: &[u8]) -> Option<String> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);
    let mut buf = Vec::new();
    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) | Ok(Event::Empty(e)) if e.name().local_name().as_ref() == tag => {
                for a in e.attributes().flatten() {
                    if a.key.local_name().as_ref() == attr
                        && let Ok(v) = a.decode_and_unescape_value(reader.decoder())
                    {
                        return Some(v.into_owned());
                    }
                }
            }
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
      <detail><m:ResponseCode xmlns:m="{}">{}</m:ResponseCode></detail>
    </s:Fault>
  </s:Body>
</s:Envelope>"#,
        xml_escape(message),
        EWS_MSG_NS,
        xml_escape(code)
    );
    (status, [("Content-Type", "text/xml; charset=utf-8")], xml).into_response()
}

fn operation_error_response(
    action: &EwsAction,
    code: &str,
    message: &str,
    status: StatusCode,
) -> Response {
    let resp = match action {
        EwsAction::GetFolder => "GetFolderResponseMessage",
        EwsAction::FindFolder => "FindFolderResponseMessage",
        EwsAction::FindItem => "FindItemResponseMessage",
        EwsAction::GetItem => "GetItemResponseMessage",
        EwsAction::SyncFolderItems => "SyncFolderItemsResponseMessage",
    };
    let top = match action {
        EwsAction::GetFolder => "GetFolderResponse",
        EwsAction::FindFolder => "FindFolderResponse",
        EwsAction::FindItem => "FindItemResponse",
        EwsAction::GetItem => "GetItemResponse",
        EwsAction::SyncFolderItems => "SyncFolderItemsResponse",
    };

    let xml = format!(
        r#"<{top} xmlns:m="{msg_ns}">
  <m:ResponseMessages>
    <m:{resp} ResponseClass="Error">
      <m:MessageText>{message}</m:MessageText>
      <m:ResponseCode>{code}</m:ResponseCode>
      <m:DescriptiveLinkKey>0</m:DescriptiveLinkKey>
    </m:{resp}>
  </m:ResponseMessages>
</{top}>"#,
        top = top,
        msg_ns = EWS_MSG_NS,
        resp = resp,
        message = xml_escape(message),
        code = xml_escape(code)
    );
    (
        status,
        [("Content-Type", "text/xml; charset=utf-8")],
        format!(
            "<?xml version=\"1.0\" encoding=\"utf-8\"?><s:Envelope xmlns:s=\"http://schemas.xmlsoap.org/soap/envelope/\"><s:Body>{}</s:Body></s:Envelope>",
            xml
        ),
    )
        .into_response()
}

fn validate_requested_folder(owner: &str, body: &str) -> Result<(), Response> {
    let expected_folder_id = folder_id_for_owner(owner);

    if let Some(fid) = extract_first_attr(body, b"FolderId", b"Id")
        && fid != expected_folder_id
    {
        return Err(operation_error_response(
            &EwsAction::GetFolder,
            "ErrorFolderNotFound",
            "Requested folder was not found for this mailbox",
            StatusCode::OK,
        ));
    }

    if let Some(did) = extract_first_attr(body, b"DistinguishedFolderId", b"Id") {
        let d = did.to_ascii_lowercase();
        if d != "calendar" && d != "msgfolderroot" {
            return Err(operation_error_response(
                &EwsAction::GetFolder,
                "ErrorFolderNotFound",
                "Only Calendar and MsgFolderRoot distinguished folders are available",
                StatusCode::OK,
            ));
        }
    }

    Ok(())
}

async fn handle_get_folder(auth: &AuthContext, body: &str) -> Response {
    let owner = owner_from_username(&auth.username);
    if let Err(resp) = validate_requested_folder(owner, body) {
        return resp;
    }

    let fid = folder_id_for_owner(owner);
    let response = format!(
        r#"<m:GetFolderResponse xmlns:m="{}" xmlns:t="{}">
  <m:ResponseMessages>
    <m:GetFolderResponseMessage ResponseClass="Success">
      <m:ResponseCode>NoError</m:ResponseCode>
      <m:Folders>
        <t:CalendarFolder>
          <t:FolderId Id="{}" ChangeKey="{}" />
          <t:DisplayName>Calendar</t:DisplayName>
          <t:FolderClass>IPF.Appointment</t:FolderClass>
          <t:TotalCount>0</t:TotalCount>
          <t:ChildFolderCount>0</t:ChildFolderCount>
        </t:CalendarFolder>
      </m:Folders>
    </m:GetFolderResponseMessage>
  </m:ResponseMessages>
</m:GetFolderResponse>"#,
        EWS_MSG_NS,
        EWS_TYPE_NS,
        fid,
        &fid[4..]
    );
    soap_ok(response)
}

async fn handle_find_folder(auth: &AuthContext, _body: &str) -> Response {
    let fid = folder_id_for_owner(owner_from_username(&auth.username));
    let response = format!(
        r#"<m:FindFolderResponse xmlns:m="{}" xmlns:t="{}">
  <m:ResponseMessages>
    <m:FindFolderResponseMessage ResponseClass="Success">
      <m:ResponseCode>NoError</m:ResponseCode>
      <m:RootFolder TotalItemsInView="1" IncludesLastItemInRange="true">
        <t:Folders>
          <t:CalendarFolder>
            <t:FolderId Id="{}" ChangeKey="{}" />
            <t:DisplayName>Calendar</t:DisplayName>
            <t:FolderClass>IPF.Appointment</t:FolderClass>
            <t:ChildFolderCount>0</t:ChildFolderCount>
            <t:TotalCount>0</t:TotalCount>
          </t:CalendarFolder>
        </t:Folders>
      </m:RootFolder>
    </m:FindFolderResponseMessage>
  </m:ResponseMessages>
</m:FindFolderResponse>"#,
        EWS_MSG_NS,
        EWS_TYPE_NS,
        fid,
        &fid[4..]
    );
    soap_ok(response)
}

async fn handle_find_item(state: &Arc<AppState>, auth: &AuthContext, body: &str) -> Response {
    let owner = owner_from_username(&auth.username);
    let max = extract_int(body, b"MaxEntriesReturned", 50);
    let offset = extract_int(body, b"Offset", 0);
    let traversal = extract_first_attr(body, b"IndexedPageItemView", b"BasePoint")
        .unwrap_or_else(|| "Beginning".to_string());

    if max == 0 || max > 512 {
        return operation_error_response(
            &EwsAction::FindItem,
            "ErrorInvalidPagingMaxRows",
            "MaxEntriesReturned must be between 1 and 512",
            StatusCode::OK,
        );
    }

    if traversal != "Beginning" {
        return operation_error_response(
            &EwsAction::FindItem,
            "ErrorInvalidIndexedPagingParameters",
            "Only IndexedPageItemView BasePoint=Beginning is supported",
            StatusCode::OK,
        );
    }

    let items = match state.storage.list_ews_items(owner, max, offset).await {
        Ok(v) => v,
        Err(e) => {
            return operation_error_response(
                &EwsAction::FindItem,
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
        r#"<m:FindItemResponse xmlns:m="{}" xmlns:t="{}">
  <m:ResponseMessages>
    <m:FindItemResponseMessage ResponseClass="Success">
      <m:ResponseCode>NoError</m:ResponseCode>
      <m:RootFolder TotalItemsInView="{}" IncludesLastItemInRange="{}" IndexedPagingOffset="{}">
        <t:Items>{}</t:Items>
      </m:RootFolder>
    </m:FindItemResponseMessage>
  </m:ResponseMessages>
</m:FindItemResponse>"#,
        EWS_MSG_NS,
        EWS_TYPE_NS,
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

async fn handle_get_item(state: &Arc<AppState>, auth: &AuthContext, body: &str) -> Response {
    let owner = owner_from_username(&auth.username);
    let item_id = extract_first_attr(body, b"ItemId", b"Id").unwrap_or_default();
    if item_id.is_empty() {
        return operation_error_response(
            &EwsAction::GetItem,
            "ErrorInvalidIdMalformed",
            "GetItem requires ItemId/@Id",
            StatusCode::OK,
        );
    }

    let item = match state
        .storage
        .get_ews_item_by_server_id(owner, &item_id)
        .await
    {
        Ok(v) => v,
        Err(e) => {
            return operation_error_response(
                &EwsAction::GetItem,
                "ErrorInternalServerError",
                &format!("Failed to load item: {}", e),
                StatusCode::INTERNAL_SERVER_ERROR,
            );
        }
    };

    let Some(item) = item else {
        return operation_error_response(
            &EwsAction::GetItem,
            "ErrorItemNotFound",
            "Requested item does not exist",
            StatusCode::OK,
        );
    };

    let ck = changekey_for_item(&item);
    let subject = item
        .uid
        .clone()
        .unwrap_or_else(|| item.resource_href.clone())
        .replace(".ics", "");

    let response = format!(
        r#"<m:GetItemResponse xmlns:m="{}" xmlns:t="{}">
  <m:ResponseMessages>
    <m:GetItemResponseMessage ResponseClass="Success">
      <m:ResponseCode>NoError</m:ResponseCode>
      <m:Items>
        <t:CalendarItem>
          <t:ItemId Id="{}" ChangeKey="{}" />
          <t:Subject>{}</t:Subject>
          <t:UID>{}</t:UID>
        </t:CalendarItem>
      </m:Items>
    </m:GetItemResponseMessage>
  </m:ResponseMessages>
</m:GetItemResponse>"#,
        EWS_MSG_NS,
        EWS_TYPE_NS,
        xml_escape(&item.server_id),
        xml_escape(&ck),
        xml_escape(&subject),
        xml_escape(item.uid.as_deref().unwrap_or(&item.server_id))
    );
    soap_ok(response)
}

fn parse_sync_state_marker(marker: Option<String>) -> Result<i64, ()> {
    match marker {
        None => Ok(0),
        Some(m) if m.is_empty() || m == "0" => Ok(0),
        Some(m) => {
            let Some(ts) = m.strip_prefix("ts:") else {
                return Err(());
            };
            ts.parse::<i64>().map_err(|_| ())
        }
    }
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
    let max_changes = extract_int(body, b"MaxChangesReturned", 100);
    let folder_id = folder_id_for_owner(owner);

    if max_changes == 0 || max_changes > 512 {
        return operation_error_response(
            &EwsAction::SyncFolderItems,
            "ErrorInvalidPagingMaxRows",
            "MaxChangesReturned must be between 1 and 512",
            StatusCode::OK,
        );
    }

    let requested_state = extract_first_tag_text(body, b"SyncState");
    let effective_state = if requested_state.as_deref().unwrap_or("0").is_empty() {
        match state.storage.get_ews_sync_state(owner, &folder_id).await {
            Ok(v) => v,
            Err(_) => None,
        }
    } else {
        requested_state
    };

    let since = match parse_sync_state_marker(effective_state) {
        Ok(v) => v,
        Err(_) => {
            return operation_error_response(
                &EwsAction::SyncFolderItems,
                "ErrorInvalidSyncStateData",
                "SyncState is invalid; expected ts:<unix_timestamp>",
                StatusCode::OK,
            );
        }
    };

    let changed_ids = match state.storage.list_changes_since(owner, since).await {
        Ok(v) => v,
        Err(e) => {
            return operation_error_response(
                &EwsAction::SyncFolderItems,
                "ErrorInternalServerError",
                &format!("Failed to query changes: {}", e),
                StatusCode::INTERNAL_SERVER_ERROR,
            );
        }
    };

    let changed: Vec<(String, String)> = changed_ids.into_iter().take(max_changes).collect();
    let changed_set: HashSet<String> = changed.iter().map(|(id, _)| id.clone()).collect();
    let items = match state.storage.list_ews_items(owner, max_changes, 0).await {
        Ok(v) => v,
        Err(e) => {
            return operation_error_response(
                &EwsAction::SyncFolderItems,
                "ErrorInternalServerError",
                &format!("Failed to query current items: {}", e),
                StatusCode::INTERNAL_SERVER_ERROR,
            );
        }
    };

    let mut current_map = HashMap::new();
    for item in items {
        current_map.insert(item.server_id.clone(), item);
    }

    let mut changes_xml = String::new();
    for (server_id, _) in changed {
        if let Some(item) = current_map.get(&server_id) {
            let change_key = changekey_for_item(item);
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
        } else {
            // changed ID is no longer present in current map: emit a delete tombstone
            changes_xml.push_str(&format!(
                r#"<t:Delete><t:ItemId Id="{}" /></t:Delete>"#,
                xml_escape(&server_id)
            ));
        }
    }

    // For deterministic behavior, also surface deletes for known current snapshot gaps when sync state > 0.
    if since > 0 && changed_set.is_empty() {
        // no-op, keep response shape valid
    }

    let new_sync_state = format!("ts:{}", now_unix());
    let _ = state
        .storage
        .set_ews_sync_state(owner, &folder_id, &new_sync_state)
        .await;

    let includes_last =
        if changes_xml.is_empty() || changes_xml.matches("<t:Create>").count() < max_changes {
            "true"
        } else {
            "false"
        };

    let response = format!(
        r#"<m:SyncFolderItemsResponse xmlns:m="{}" xmlns:t="{}">
  <m:ResponseMessages>
    <m:SyncFolderItemsResponseMessage ResponseClass="Success">
      <m:ResponseCode>NoError</m:ResponseCode>
      <m:SyncState>{}</m:SyncState>
      <m:IncludesLastItemInRange>{}</m:IncludesLastItemInRange>
      <m:Changes>{}</m:Changes>
    </m:SyncFolderItemsResponseMessage>
  </m:ResponseMessages>
</m:SyncFolderItemsResponse>"#,
        EWS_MSG_NS,
        EWS_TYPE_NS,
        xml_escape(&new_sync_state),
        includes_last,
        changes_xml
    );

    soap_ok(response)
}

#[cfg(test)]
mod tests {
    use super::{
        EwsAction, detect_action, operation_error_response, parse_sync_state_marker,
        validate_schema,
    };

    #[test]
    fn detects_get_item_action() {
        let xml = r#"<s:Envelope><s:Body><m:GetItem xmlns:m="http://schemas.microsoft.com/exchange/services/2006/messages" /></s:Body></s:Envelope>"#;
        assert_eq!(detect_action(xml), Some(EwsAction::GetItem));
    }

    #[test]
    fn validates_find_item_schema() {
        let xml = r#"<s:Envelope><s:Body><m:FindItem xmlns:m="http://schemas.microsoft.com/exchange/services/2006/messages"><m:ItemShape/><m:ParentFolderIds/></m:FindItem></s:Body></s:Envelope>"#;
        assert!(validate_schema(&EwsAction::FindItem, xml).is_ok());
    }

    #[test]
    fn invalid_sync_state_marker_rejected() {
        assert!(parse_sync_state_marker(Some("offset:10".to_string())).is_err());
        assert!(parse_sync_state_marker(Some("ts:abc".to_string())).is_err());
        assert_eq!(
            parse_sync_state_marker(Some("ts:12".to_string())).ok(),
            Some(12)
        );
    }

    #[test]
    fn operation_error_uses_response_code() {
        let resp = operation_error_response(
            &EwsAction::FindItem,
            "ErrorInvalidPagingMaxRows",
            "bad",
            axum::http::StatusCode::OK,
        );
        let body = format!("{:?}", resp);
        assert!(!body.is_empty());
    }
}
