async fn handle_sync_folder_hierarchy(session: &jmap_client::JmapSession, xml: &str) -> Result<String, EwsError> {
    let req: SyncFolderHierarchyRequest = parse_body_content(xml).map_err(EwsError::XmlParse)?;
    let cal_id = jmap_client::get_default_calendar_id(session).await?;
    // Generate a stable sync state from the calendar ID so clients can
    // distinguish initial from subsequent syncs.
    let sync_state = {
        let mut h = Sha256::new();
        h.update(b"folder-hierarchy:");
        h.update(cal_id.as_bytes());
        base64::Engine::encode(&base64::engine::general_purpose::STANDARD, h.finalize())
    };

    let changes = match req.sync_state.as_deref() {
        Some(state) if state == sync_state.as_str() => {
            // Subsequent sync: hierarchy hasn't changed (single calendar folder),
            // return empty changes.
            String::new()
        }
        Some(_) => {
            // Stale or unrecognised sync state (e.g. server restart, calendar
            // ID change).  Fall back to an initial sync so the client can
            // recover, consistent with handle_sync_folder_items.
            tracing::warn!(
                "SyncFolderHierarchy: client SyncState does not match; falling back to initial sync"
            );
            format!(
                r#"<t:Create><t:CalendarFolder><t:FolderId Id="{}" ChangeKey="AQAAABYAAA=" /><t:DisplayName>Calendar</t:DisplayName></t:CalendarFolder></t:Create>"#,
                utils::escape_xml(&cal_id)
            )
        }
        None => {
            // Initial sync: report the calendar folder as created.
            format!(
                r#"<t:Create><t:CalendarFolder><t:FolderId Id="{}" ChangeKey="AQAAABYAAA=" /><t:DisplayName>Calendar</t:DisplayName></t:CalendarFolder></t:Create>"#,
                utils::escape_xml(&cal_id)
            )
        }
    };

    Ok(soap_response(&format!(
        r#"<m:SyncFolderHierarchyResponse xmlns:m="{}" xmlns:t="{}"><m:ResponseMessages><m:SyncFolderHierarchyResponseMessage ResponseClass="Success"><m:ResponseCode>NoError</m:ResponseCode><m:SyncState>{}</m:SyncState><m:IncludesLastItemInRange>true</m:IncludesLastItemInRange><m:Changes>{}</m:Changes></m:SyncFolderHierarchyResponseMessage></m:ResponseMessages></m:SyncFolderHierarchyResponse>"#,
        NS_M,
        NS_T,
        utils::escape_xml(&sync_state),
        changes
    )))
}

pub async fn process_request(
    config: &AppConfig,
    xml: &str,
    headers: &HeaderMap,
) -> Result<String, EwsError> {
    let auth_header = match headers.get("Authorization").and_then(|v| v.to_str().ok()) {
        Some(a) => a,
        None => return soap_fault("ErrorAccessDenied", "Missing Authorization header"),
    };

    let (user, pass) = match utils::decode_basic_auth(auth_header) {
        Some((u, p)) => (u, p),
        None => return soap_fault("ErrorAccessDenied", "Invalid Authorization header format"),
    };
    let session = match jmap_client::get_session(&config.jmap_url, &user, &pass).await {
        Ok(s) => s,
        Err(jmap_client::JmapError::Auth(_)) => {
            return soap_fault("ErrorAccessDenied", "Auth Failed");
        }
        Err(e) => {
            tracing::error!("JMAP Auth failed: {}", e);
            return soap_fault("ErrorInternalServerError", "Auth Failed");
        }
    };
    let action = extract_action_name(xml);
    tracing::info!("EWS Request: {}", action);

    match action.as_str() {
        "GetFolder" => handle_get_folder(&session, xml).await,
        "FindFolder" => handle_find_folder(&session).await,
        "SyncFolderHierarchy" => handle_sync_folder_hierarchy(&session, xml).await,
        "SyncFolderItems" => handle_sync_folder_items(&session, config, &user, xml).await,
        "CreateItem" => handle_create_item(&session, config, xml).await,
        "UpdateItem" => handle_update_item(&session, config, xml).await,
        "DeleteItem" => handle_delete_item(&session, config, xml).await,
        "GetItem" => handle_get_item(&session, config, xml).await,
        "FindItem" => handle_find_item().await,
        "ResolveNames" => handle_resolve_names(&session, xml).await,
        "GetAttachment" => handle_get_attachment(&session, xml).await,
        "GetRoomLists" => handle_get_room_lists().await,
        "GetRooms" => handle_get_rooms().await,
        _ => soap_fault("ErrorInvalidRequest", &format!("Unsupported: {}", action)),
    }
}
use crate::{config::AppConfig, db, jmap_client, utils};
use axum::http::HeaderMap;
use chrono::{DateTime, NaiveDateTime};
use chrono_tz::Tz;
use quick_xml::Reader;
use quick_xml::events::Event;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use uuid::Uuid;

const NS_SOAP: &str = "http://schemas.xmlsoap.org/soap/envelope/";
const NS_M: &str = "http://schemas.microsoft.com/exchange/services/2006/messages";
const NS_T: &str = "http://schemas.microsoft.com/exchange/services/2006/types";

use thiserror::Error;

#[derive(Debug, Error)]
pub enum EwsError {
    #[error("XML parsing error: {0}")]
    XmlParse(#[from] quick_xml::DeError),
    #[error("HTTP request error: {0}")]
    Http(#[from] reqwest::Error),
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("Missing required element or field: {0}")]
    MissingElement(String),
    #[error("JMAP client error: {0}")]
    Jmap(#[from] crate::jmap_client::JmapError),
    #[error("Authentication error: {0}")]
    Auth(String),
    #[error("Unsupported operation: {0}")]
    UnsupportedOperation(String),
    #[error("Other error: {0}")]
    Other(String),
}
async fn handle_sync_folder_hierarchy(session: &jmap_client::JmapSession, xml: &str) -> Result<String, EwsError> {
    let req: SyncFolderHierarchyRequest = parse_body_content(xml).map_err(EwsError::XmlParse)?;
    let cal_id = jmap_client::get_default_calendar_id(session).await?;
    // Generate a stable sync state from the calendar ID so clients can
    // distinguish initial from subsequent syncs.
    let sync_state = {
        let mut h = Sha256::new();
        h.update(b"folder-hierarchy:");
        h.update(cal_id.as_bytes());
        base64::Engine::encode(&base64::engine::general_purpose::STANDARD, h.finalize())
    };

    let changes = match req.sync_state.as_deref() {
        Some(state) if state == sync_state.as_str() => {
            // Subsequent sync: hierarchy hasn't changed (single calendar folder),
            // return empty changes.
            String::new()
        }
        Some(_) => {
            // Stale or unrecognised sync state (e.g. server restart, calendar
            // ID change).  Fall back to an initial sync so the client can
            // recover, consistent with handle_sync_folder_items.
            tracing::warn!(
                "SyncFolderHierarchy: client SyncState does not match; falling back to initial sync"
            );
            format!(
                r#"<t:Create><t:CalendarFolder><t:FolderId Id="{}" ChangeKey="AQAAABYAAA=" /><t:DisplayName>Calendar</t:DisplayName></t:CalendarFolder></t:Create>"#,
                utils::escape_xml(&cal_id)
            )
        }
        None => {
            // Initial sync: report the calendar folder as created.
            format!(
                r#"<t:Create><t:CalendarFolder><t:FolderId Id="{}" ChangeKey="AQAAABYAAA=" /><t:DisplayName>Calendar</t:DisplayName></t:CalendarFolder></t:Create>"#,
                utils::escape_xml(&cal_id)
            )
        }
    };

    Ok(soap_response(&format!(
        r#"<m:SyncFolderHierarchyResponse xmlns:m="{}" xmlns:t="{}"><m:ResponseMessages><m:SyncFolderHierarchyResponseMessage ResponseClass="Success"><m:ResponseCode>NoError</m:ResponseCode><m:SyncState>{}</m:SyncState><m:IncludesLastItemInRange>true</m:IncludesLastItemInRange><m:Changes>{}</m:Changes></m:SyncFolderHierarchyResponseMessage></m:ResponseMessages></m:SyncFolderHierarchyResponse>"#,
        NS_M,
        NS_T,
        utils::escape_xml(&sync_state),
        changes
    )))

pub async fn process_request(
    config: &AppConfig,
    xml: &str,
    headers: &HeaderMap,
) -> Result<String, EwsError> {
    let auth_header = match headers.get("Authorization").and_then(|v| v.to_str().ok()) {
        Some(a) => a,
        None => return soap_fault("ErrorAccessDenied", "Missing Authorization header"),
    };

    let (user, pass) = match utils::decode_basic_auth(auth_header) {
        Some((u, p)) => (u, p),
        None => return soap_fault("ErrorAccessDenied", "Invalid Authorization header format"),
    };
    let session = match jmap_client::get_session(&config.jmap_url, &user, &pass).await {
        Ok(s) => s,
        Err(jmap_client::JmapError::Auth(_)) => {
            return soap_fault("ErrorAccessDenied", "Auth Failed");
        }
        Err(e) => {
            tracing::error!("JMAP Auth failed: {}", e);
            return soap_fault("ErrorInternalServerError", "Auth Failed");
        }
    };
    let action = extract_action_name(xml);
    tracing::info!("EWS Request: {}", action);

    match action.as_str() {
        "GetFolder" => handle_get_folder(&session, xml).await,
        "FindFolder" => handle_find_folder(&session).await,
        "SyncFolderHierarchy" => handle_sync_folder_hierarchy(&session, xml).await,
        "SyncFolderItems" => handle_sync_folder_items(&session, config, &user, xml).await,
        "CreateItem" => handle_create_item(&session, config, xml).await,
        "UpdateItem" => handle_update_item(&session, config, xml).await,
        "DeleteItem" => handle_delete_item(&session, config, xml).await,
        "GetItem" => handle_get_item(&session, config, xml).await,
        "FindItem" => handle_find_item().await,
        "ResolveNames" => handle_resolve_names(&session, xml).await,
        "GetAttachment" => handle_get_attachment(&session, xml).await,
        "GetRoomLists" => handle_get_room_lists().await,
        "GetRooms" => handle_get_rooms().await,
        _ => soap_fault("ErrorInvalidRequest", &format!("Unsupported: {}", action)),
    }
}

use thiserror::Error;

async fn handle_sync_folder_hierarchy(session: &jmap_client::JmapSession, xml: &str) -> Result<String, EwsError> {
    let req: SyncFolderHierarchyRequest = parse_body_content(xml).map_err(EwsError::XmlParse)?;
    let cal_id = jmap_client::get_default_calendar_id(session).await?;
    // Generate a stable sync state from the calendar ID so clients can
    // distinguish initial from subsequent syncs.
    let sync_state = {
        let mut h = Sha256::new();
        h.update(b"folder-hierarchy:");
        h.update(cal_id.as_bytes());
        base64::Engine::encode(&base64::engine::general_purpose::STANDARD, h.finalize())
    };

    let changes = match req.sync_state.as_deref() {
        Some(state) if state == sync_state.as_str() => {
            // Subsequent sync: hierarchy hasn't changed (single calendar folder),
            // return empty changes.
            String::new()
        }
        Some(_) => {
            // Stale or unrecognised sync state (e.g. server restart, calendar
            // ID change).  Fall back to an initial sync so the client can
            // recover, consistent with handle_sync_folder_items.
            tracing::warn!(
                "SyncFolderHierarchy: client SyncState does not match; falling back to initial sync"
            );
            format!(
                r#"<t:Create><t:CalendarFolder><t:FolderId Id="{}" ChangeKey="AQAAABYAAA=" /><t:DisplayName>Calendar</t:DisplayName></t:CalendarFolder></t:Create>"#,
                utils::escape_xml(&cal_id)
            )
        }
        None => {
            // Initial sync: report the calendar folder as created.
            format!(
                r#"<t:Create><t:CalendarFolder><t:FolderId Id="{}" ChangeKey="AQAAABYAAA=" /><t:DisplayName>Calendar</t:DisplayName></t:CalendarFolder></t:Create>"#,
                utils::escape_xml(&cal_id)
            )
        }
    };

    Ok(soap_response(&format!(
        r#"<m:SyncFolderHierarchyResponse xmlns:m="{}" xmlns:t="{}"><m:ResponseMessages><m:SyncFolderHierarchyResponseMessage ResponseClass="Success"><m:ResponseCode>NoError</m:ResponseCode><m:SyncState>{}</m:SyncState><m:IncludesLastItemInRange>true</m:IncludesLastItemInRange><m:Changes>{}</m:Changes></m:SyncFolderHierarchyResponseMessage></m:ResponseMessages></m:SyncFolderHierarchyResponse>"#,
        NS_M,
        NS_T,
        utils::escape_xml(&sync_state),
        changes
pub async fn process_request(
    config: &AppConfig,
    xml: &str,
    headers: &HeaderMap,
) -> Result<String, EwsError> {
    let auth_header = match headers.get("Authorization").and_then(|v| v.to_str().ok()) {
        Some(a) => a,
        None => return soap_fault("ErrorAccessDenied", "Missing Authorization header"),
    };

    let (user, pass) = match utils::decode_basic_auth(auth_header) {
        Some((u, p)) => (u, p),
        None => return soap_fault("ErrorAccessDenied", "Invalid Authorization header format"),
    };
    let session = match jmap_client::get_session(&config.jmap_url, &user, &pass).await {
        Ok(s) => s,
        Err(jmap_client::JmapError::Auth(_)) => {
            return soap_fault("ErrorAccessDenied", "Auth Failed");
        }
        Err(e) => {
            tracing::error!("JMAP Auth failed: {}", e);
            return soap_fault("ErrorInternalServerError", "Auth Failed");
        }
    };
    let action = extract_action_name(xml);
    tracing::info!("EWS Request: {}", action);

    match action.as_str() {
        "GetFolder" => handle_get_folder(&session, xml).await,
        "FindFolder" => handle_find_folder(&session).await,
        "SyncFolderHierarchy" => handle_sync_folder_hierarchy(&session, xml).await,
        "SyncFolderItems" => handle_sync_folder_items(&session, config, &user, xml).await,
        "CreateItem" => handle_create_item(&session, config, xml).await,
        "UpdateItem" => handle_update_item(&session, config, xml).await,
        "DeleteItem" => handle_delete_item(&session, config, xml).await,
        "GetItem" => handle_get_item(&session, config, xml).await,
        "FindItem" => handle_find_item().await,
        "ResolveNames" => handle_resolve_names(&session, xml).await,
        "GetAttachment" => handle_get_attachment(&session, xml).await,
        "GetRoomLists" => handle_get_room_lists().await,
        "GetRooms" => handle_get_rooms().await,
        _ => soap_fault("ErrorInvalidRequest", &format!("Unsupported: {}", action)),
    }
}

async fn handle_sync_folder_hierarchy(session: &jmap_client::JmapSession, xml: &str) -> String {

    let req: SyncFolderHierarchyRequest = match parse_body_content(xml) {
        Ok(r) => r,
        Err(_) => return soap_fault("ErrorInvalidRequest", "Bad XML"),
    };
    let cal_id = match jmap_client::get_default_calendar_id(session).await {
        Ok(id) => id,
        Err(e) => {
            tracing::error!(
                "SyncFolderHierarchy: failed to get default calendar ID: {}",
                e
            );
            return soap_fault("ErrorInternalServerError", "Failed to get calendar");
        }
    };

    // Generate a stable sync state from the calendar ID so clients can
    // distinguish initial from subsequent syncs.
    let sync_state = {
        let mut h = Sha256::new();
        h.update(b"folder-hierarchy:");
        h.update(cal_id.as_bytes());
        base64::Engine::encode(&base64::engine::general_purpose::STANDARD, h.finalize())
    };

    let changes = match req.sync_state.as_deref() {
        Some(state) if state == sync_state.as_str() => {
            // Subsequent sync: hierarchy hasn't changed (single calendar folder),
            // return empty changes.
            String::new()
        }
        Some(_) => {
            // Stale or unrecognised sync state (e.g. server restart, calendar
            // ID change).  Fall back to an initial sync so the client can
            // recover, consistent with handle_sync_folder_items.
            tracing::warn!(
                "SyncFolderHierarchy: client SyncState does not match; falling back to initial sync"
            );
            format!(
                r#"<t:Create><t:CalendarFolder><t:FolderId Id="{}" ChangeKey="AQAAABYAAA=" /><t:DisplayName>Calendar</t:DisplayName></t:CalendarFolder></t:Create>"#,
                utils::escape_xml(&cal_id)
            )
        }
        None => {
            // Initial sync: report the calendar folder as created.
            format!(
                r#"<t:Create><t:CalendarFolder><t:FolderId Id="{}" ChangeKey="AQAAABYAAA=" /><t:DisplayName>Calendar</t:DisplayName></t:CalendarFolder></t:Create>"#,
                utils::escape_xml(&cal_id)
            )
        }
    };

    soap_response(&format!(
        r#"<m:SyncFolderHierarchyResponse xmlns:m="{}" xmlns:t="{}"><m:ResponseMessages><m:SyncFolderHierarchyResponseMessage ResponseClass="Success"><m:ResponseCode>NoError</m:ResponseCode><m:SyncState>{}</m:SyncState><m:IncludesLastItemInRange>true</m:IncludesLastItemInRange><m:Changes>{}</m:Changes></m:SyncFolderHierarchyResponseMessage></m:ResponseMessages></m:SyncFolderHierarchyResponse>"#,
        NS_M,
        NS_T,
        utils::escape_xml(&sync_state),
        changes
    ))
}

async fn handle_find_folder(session: &jmap_client::JmapSession) -> String {
    let cal_id = match jmap_client::get_default_calendar_id(session).await {
        Ok(id) => id,
        Err(e) => {
            tracing::error!("FindFolder: failed to get default calendar ID: {}", e);
            return soap_fault("ErrorInternalServerError", "Failed to get calendar");
        }
    };
    soap_response(&format!(
        r#"<m:FindFolderResponse xmlns:m="{}" xmlns:t="{}"><m:ResponseMessages><m:FindFolderResponseMessage ResponseClass="Success"><m:ResponseCode>NoError</m:ResponseCode><m:RootFolder TotalItemsInView="1" IncludesLastItemInRange="true"><t:Folders><t:CalendarFolder><t:FolderId Id="{}" ChangeKey="AQAAABYAAA=" /><t:DisplayName>Calendar</t:DisplayName></t:CalendarFolder></t:Folders></m:RootFolder></m:FindFolderResponseMessage></m:ResponseMessages></m:FindFolderResponse>"#,
        NS_M,
        NS_T,
        utils::escape_xml(&cal_id)
    ))
}

async fn handle_resolve_names(session: &jmap_client::JmapSession, xml: &str) -> String {
    let req: ResolveNamesRequest = match parse_body_content(xml) {
        Ok(r) => r,
        Err(_) => return soap_fault("ErrorInvalidRequest", "Bad XML"),
    };
    const MAX_RESOLVE_NAMES_RESULTS: usize = 10;

    let results = match jmap_client::search_principals(session, &req.unresolved_entry).await {
        Ok(results) => results
            .into_iter()
            .take(MAX_RESOLVE_NAMES_RESULTS)
            .collect::<Vec<_>>(),
        Err(e) => {
            tracing::error!("search_principals failed: {}", e);
            return soap_fault("ErrorInternalServerError", "ResolveNames Failed");
        }
    };
    if results.is_empty() {
        return soap_response(&format!(
            r#"<m:ResolveNamesResponse xmlns:m="{}" xmlns:t="{}"><m:ResponseMessages><m:ResolveNamesResponseMessage ResponseClass="Error"><m:ResponseCode>ErrorNameResolutionNoResults</m:ResponseCode><m:MessageText>No results were found.</m:MessageText></m:ResolveNamesResponseMessage></m:ResponseMessages></m:ResolveNamesResponse>"#,
            NS_M, NS_T,
        ));
    }
    let mut resolutions = String::new();
    for p in &results {
        resolutions.push_str(&format!(r#"<t:Resolution><t:Mailbox><t:Name>{}</t:Name><t:EmailAddress>{}</t:EmailAddress><t:RoutingType>SMTP</t:RoutingType></t:Mailbox></t:Resolution>"#, utils::escape_xml(&p.name), utils::escape_xml(&p.email)));
    }
    soap_response(&format!(
        r#"<m:ResolveNamesResponse xmlns:m="{}" xmlns:t="{}"><m:ResponseMessages><m:ResolveNamesResponseMessage ResponseClass="{}"><m:ResponseCode>{}</m:ResponseCode>{}<m:ResolutionSet TotalItemsInView="{}">{}</m:ResolutionSet></m:ResolveNamesResponseMessage></m:ResponseMessages></m:ResolveNamesResponse>"#,
        NS_M,
        NS_T,
        if results.len() > 1 {
            "Warning"
        } else {
            "Success"
        },
        if results.len() > 1 {
            "ErrorNameResolutionMultipleResults"
        } else {
            "NoError"
        },
        if results.len() > 1 {
            "<m:MessageText>Multiple results were found.</m:MessageText>"
        } else {
            ""
        },
        results.len(),
        resolutions
    ))
}

async fn handle_get_attachment(session: &jmap_client::JmapSession, xml: &str) -> String {
    let req: GetAttachmentRequest = match parse_body_content(xml) {
        Ok(r) => r,
        Err(_) => return soap_fault("ErrorInvalidRequest", "Bad XML"),
    };
    let mut response_messages = String::new();
    for attachment_id in req.attachment_ids.items {
        let id_str = &attachment_id.id;
        match jmap_client::get_blob(session, id_str).await {
            Ok(data) => {
                let b64 = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &data);
                response_messages.push_str(&format!(
                    r#"<m:GetAttachmentResponseMessage ResponseClass="Success"><m:ResponseCode>NoError</m:ResponseCode><m:Attachments><t:FileAttachment><t:AttachmentId Id="{}"/><t:Content>{}</t:Content></t:FileAttachment></m:Attachments></m:GetAttachmentResponseMessage>"#,
                    utils::escape_xml(id_str),
                    b64
                ));
            }
            Err(jmap_client::JmapError::NotFound(_)) => {
                tracing::warn!("get_blob not found for attachment {}", id_str);
                response_messages.push_str(&format!(
                    r#"<m:GetAttachmentResponseMessage ResponseClass="Error"><m:ResponseCode>ErrorItemNotFound</m:ResponseCode><m:MessageText>Attachment not found</m:MessageText><m:Attachments><t:FileAttachment><t:AttachmentId Id="{}"/></t:FileAttachment></m:Attachments></m:GetAttachmentResponseMessage>"#,
                    utils::escape_xml(id_str)
                ));
            }
            Err(e) => {
                tracing::error!("get_blob failed for attachment {}: {}", id_str, e);
                response_messages.push_str(&format!(
                    r#"<m:GetAttachmentResponseMessage ResponseClass="Error"><m:ResponseCode>ErrorInternalServerError</m:ResponseCode><m:MessageText>Failed to get attachment</m:MessageText><m:Attachments><t:FileAttachment><t:AttachmentId Id="{}"/></t:FileAttachment></m:Attachments></m:GetAttachmentResponseMessage>"#,
                    utils::escape_xml(id_str)
                ));
            }
        }
    }
    soap_response(&format!(
        r#"<m:GetAttachmentResponse xmlns:m="{}" xmlns:t="{}"><m:ResponseMessages>{}</m:ResponseMessages></m:GetAttachmentResponse>"#,
        NS_M, NS_T, response_messages
    ))
}

async fn handle_get_item(
    session: &jmap_client::JmapSession,
    config: &AppConfig,
    xml: &str,
) -> String {
    let req: GetItemRequest = match parse_body_content(xml) {
        Ok(r) => r,
        Err(_) => return soap_fault("ErrorInvalidRequest", "Bad XML"),
    };
    let ids: Vec<String> = req.item_ids.items.iter().map(|i| i.id.clone()).collect();
    let events = match jmap_client::get_events_by_ids(session, &ids).await {
        Ok(events) => events,
        Err(e) => {
            tracing::error!("get_events_by_ids failed: {}", e);
            return soap_fault("ErrorInternalServerError", "GetItem Failed");
        }
    };
    let mut response_messages = String::new();
    for item_id in &req.item_ids.items {
        if let Some(event) = events.iter().find(|e| e.id.as_deref() == Some(&item_id.id)) {
            let rendered = render_ews_calendar_item(event, &config.timezone);
            response_messages.push_str(&format!(
                r#"<m:GetItemResponseMessage ResponseClass="Success"><m:ResponseCode>NoError</m:ResponseCode><m:Items>{}</m:Items></m:GetItemResponseMessage>"#,
                rendered
            ));
        } else {
            response_messages.push_str(r#"<m:GetItemResponseMessage ResponseClass="Error"><m:ResponseCode>ErrorItemNotFound</m:ResponseCode><m:Items/></m:GetItemResponseMessage>"#);
        }
    }
    soap_response(&format!(
        r#"<m:GetItemResponse xmlns:m="{}" xmlns:t="{}"><m:ResponseMessages>{}</m:ResponseMessages></m:GetItemResponse>"#,
        NS_M, NS_T, response_messages
    ))
}

/// Format a JMAP timestamp as a wall-clock local datetime string for EWS.
///
/// JMAP/JSCalendar has two timestamp shapes:
/// - **Offset-aware** (RFC 3339, e.g. `2024-01-15T15:00:00Z`): represents an
///   absolute instant.  We convert it to the configured timezone.
/// - **LocalDateTime** (no offset, e.g. `2024-01-15T10:00:00`): a floating /
///   local time with no UTC offset.  Per JSCalendar semantics this must be
///   interpreted in the event's (or calendar's) timezone — *not* as UTC.  We
///   therefore keep it as-is.
fn format_jmap_localtime(s: &str, tz: Tz) -> Option<String> {
    // Try offset-aware RFC 3339 first (e.g. "2024-01-15T15:00:00Z").
    if let Ok(dt) = DateTime::parse_from_rfc3339(s) {
        return Some(
            dt.with_timezone(&tz)
                .format("%Y-%m-%dT%H:%M:%S")
                .to_string(),
        );
    }
    // Naive / LocalDateTime – already a wall-clock time in the event timezone.
    if let Ok(naive) = NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S") {
        return Some(naive.format("%Y-%m-%dT%H:%M:%S").to_string());
    }
    None
}

fn render_ews_calendar_item(event: &jmap_client::JmapEvent, tz_str: &str) -> String {
    let tz: Tz = tz_str.parse().unwrap_or(chrono_tz::UTC);
    let start_local = match format_jmap_localtime(&event.start, tz) {
        Some(s) => s,
        None => {
            tracing::warn!(
                "Could not parse start timestamp for event {:?}: '{}'; using raw value",
                event.id,
                event.start,
            );
            event.start.clone()
        }
    };
    let end_local = match format_jmap_localtime(&event.end, tz) {
        Some(s) => s,
        None => {
            tracing::warn!(
                "Could not parse end timestamp for event {:?}: '{}'; using raw value",
                event.id,
                event.end,
            );
            event.end.clone()
        }
    };

    let mut hasher = Sha256::new();
    hasher.update(event.id.as_deref().unwrap_or(""));
    hasher.update(event.updated.as_deref().unwrap_or(&event.start));
    let change_key = base64::Engine::encode(
        &base64::engine::general_purpose::STANDARD,
        hasher.finalize(),
    );

    let mut attendees_xml = String::new();
    if let Some(parts) = &event.participants {
        attendees_xml.push_str("<t:RequiredAttendees>");
        for p in parts {
            attendees_xml.push_str(&format!(r#"<t:Attendee><t:Mailbox><t:EmailAddress>{}</t:EmailAddress><t:Name>{}</t:Name></t:Mailbox></t:Attendee>"#, utils::escape_xml(&p.email), utils::escape_xml(&p.name)));
        }
        attendees_xml.push_str("</t:RequiredAttendees>");
    }
    format!(
        r#"<t:CalendarItem><t:ItemId Id="{}" ChangeKey="{}" /><t:Subject>{}</t:Subject><t:Body BodyType="Text">{}</t:Body><t:Start>{}</t:Start><t:End>{}</t:End><t:Location>{}</t:Location><t:IsAllDayEvent>{}</t:IsAllDayEvent>{}</t:CalendarItem>"#,
        utils::escape_xml(event.id.as_deref().unwrap_or("")),
        utils::escape_xml(&change_key),
        utils::escape_xml(&event.title),
        utils::escape_xml(event.description.as_deref().unwrap_or("")),
        utils::escape_xml(&start_local),
        utils::escape_xml(&end_local),
        utils::escape_xml(event.location.as_deref().unwrap_or("")),
        event.show_without_time,
        attendees_xml
    )
}

async fn handle_update_item(
    session: &jmap_client::JmapSession,
    config: &AppConfig,
    xml: &str,
) -> String {
    let req: UpdateItemRequest = match parse_body_content(xml) {
        Ok(r) => r,
        Err(_) => return soap_fault("ErrorInvalidRequest", "Bad XML"),
    };
    let tz: Tz = config.timezone.parse().unwrap_or(chrono_tz::UTC);

    // Enum to track the outcome for each ItemChange in request order.
    enum ResponsePlaceholder {
        ImmediateSuccess,
        ImmediateError { id: String, code: &'static str, message: &'static str },
        Pending { id: String },
    }

    let mut placeholders = Vec::with_capacity(req.item_changes.items.len());
    let mut merged: std::collections::HashMap<String, serde_json::Map<String, serde_json::Value>> =
        std::collections::HashMap::new();

    for change in req.item_changes.items {
        let id = change.item_id.id;

        // Check for unsupported operations (DeleteItemField, AppendToItemField)
        if !change.updates.delete_fields.is_empty() || !change.updates.append_fields.is_empty() {
            tracing::warn!(
                "Unsupported update operation for item {}: delete or append fields present",
                id
            );
            placeholders.push(ResponsePlaceholder::ImmediateError {
                id,
                code: "ErrorInternalServerError",
                message: "Unsupported update operation (DeleteItemField or AppendToItemField)",
            });
            continue;
        }

        // No set fields → no-op
        if change.updates.set_fields.is_empty() {
            placeholders.push(ResponsePlaceholder::ImmediateSuccess);
            continue;
        }

        // Build patch from SetItemField entries
        let mut patch = serde_json::Map::new();
        let mut unsupported_set_field = false;  
        for update in change.updates.set_fields {
            match update.field_uri.field_uri.as_str() {
                "item:Subject" | "calendar:Subject" => {
                    if let Some(s) = update.calendar_item.subject {
                        patch.insert("title".into(), serde_json::json!(s));
                    }
                }
                "item:Body" => {
                    if let Some(b) = update.calendar_item.body {
                        patch.insert("description".into(), serde_json::json!(b.content));
                    }
                }
                "calendar:Location" => {
                    if let Some(l) = update.calendar_item.location {
                        patch.insert("location".into(), serde_json::json!(l));
                    }
                }
                "calendar:Start" => {
                    if let Some(s) = update.calendar_item.start {
                        patch.insert(
                            "start".into(),
                            serde_json::json!(utils::parse_local_to_utc(&s, tz)),
                        );
                    }
                }
                "calendar:End" => {
                    if let Some(e) = update.calendar_item.end {
                        patch.insert(
                            "end".into(),
                            serde_json::json!(utils::parse_local_to_utc(&e, tz)),
                        );
                    }
                }
                _ => unsupported_set_field = true,
            }
        }
        if unsupported_set_field {
            placeholders.push(ResponsePlaceholder::ImmediateError {
                id,
                code: "ErrorInternalServerError",
                message: "Unsupported update operation",
            });
        } else if patch.is_empty() {
            placeholders.push(ResponsePlaceholder::ImmediateSuccess);
        } else {
            merged.entry(id.clone()).or_default().extend(patch);
            placeholders.push(ResponsePlaceholder::Pending { id });
        }
    }

    // Flatten merged patches into the Vec expected by batch_patch_events.
    let patches: Vec<(String, serde_json::Map<String, serde_json::Value>)> =
        merged.into_iter().collect();

    // Send all non‑empty patches in a single batch request.
    let batch_result = if patches.is_empty() {
        None
    } else {
        match jmap_client::batch_patch_events(session, patches).await {
            Ok(result) => {
                if let Some(ref e) = result.chunk_error {
                    tracing::error!(
                        "batch_patch_events partially failed: {}; {} updated, {} not updated",
                        e,
                        result.updated.len(),
                        result.not_updated.len()
                    );
                }
                Some(result)
            }
            Err(e) => {
                tracing::error!("batch_patch_events failed: {}", e);
                // Total failure – every pending item failed.
                let mut response_messages = String::new();
                for ph in placeholders {
                    match ph {
                        ResponsePlaceholder::ImmediateSuccess => {
                            response_messages.push_str(r#"<m:UpdateItemResponseMessage ResponseClass="Success"><m:ResponseCode>NoError</m:ResponseCode></m:UpdateItemResponseMessage>"#);
                        }
                        ResponsePlaceholder::ImmediateError { code, message, .. } => {
                            response_messages.push_str(&format!(
                                r#"<m:UpdateItemResponseMessage ResponseClass="Error"><m:ResponseCode>{}</m:ResponseCode><m:MessageText>{}</m:MessageText></m:UpdateItemResponseMessage>"#,
                                code, message
                            ));
                        }
                        ResponsePlaceholder::Pending { id } => {
                            tracing::error!("batch_patch_events total failure for item {}", id);
                            response_messages.push_str(r#"<m:UpdateItemResponseMessage ResponseClass="Error"><m:ResponseCode>ErrorInternalServerError</m:ResponseCode><m:MessageText>Update failed</m:MessageText></m:UpdateItemResponseMessage>"#);
                        }
                    }
                }
                return soap_response(&format!(
                    r#"<m:UpdateItemResponse xmlns:m="{}" xmlns:t="{}"><m:ResponseMessages>{}</m:ResponseMessages></m:UpdateItemResponse>"#,
                    NS_M, NS_T, response_messages
                ));
            }
        }
    };

    // Build per‑item response messages in the original request order.
    let not_updated: std::collections::HashMap<String, String> = batch_result
        .as_ref()
        .map(|r| r.not_updated.iter().cloned().collect())
        .unwrap_or_default();

    let mut response_messages = String::new();
    for ph in placeholders {
        match ph {
            ResponsePlaceholder::ImmediateSuccess => {
                response_messages.push_str(r#"<m:UpdateItemResponseMessage ResponseClass="Success"><m:ResponseCode>NoError</m:ResponseCode></m:UpdateItemResponseMessage>"#);
            }
            ResponsePlaceholder::ImmediateError { code, message, .. } => {
                response_messages.push_str(&format!(
                    r#"<m:UpdateItemResponseMessage ResponseClass="Error"><m:ResponseCode>{}</m:ResponseCode><m:MessageText>{}</m:MessageText></m:UpdateItemResponseMessage>"#,
                    code, message
                ));
            }
            ResponsePlaceholder::Pending { id } => {
                if let Some(desc) = not_updated.get(&id) {
                    tracing::error!("patch_event failed for {}: {}", id, desc);
                    response_messages.push_str(r#"<m:UpdateItemResponseMessage ResponseClass="Error"><m:ResponseCode>ErrorInternalServerError</m:ResponseCode><m:MessageText>Update failed</m:MessageText></m:UpdateItemResponseMessage>"#);
                } else {
                    response_messages.push_str(r#"<m:UpdateItemResponseMessage ResponseClass="Success"><m:ResponseCode>NoError</m:ResponseCode></m:UpdateItemResponseMessage>"#);
                }
            }
        }
    }

    soap_response(&format!(
        r#"<m:UpdateItemResponse xmlns:m="{}" xmlns:t="{}"><m:ResponseMessages>{}</m:ResponseMessages></m:UpdateItemResponse>"#,
        NS_M, NS_T, response_messages
    ))
}

async fn handle_sync_folder_items(
    session: &jmap_client::JmapSession,
    config: &AppConfig,
    user: &str,
    xml: &str,
) -> String {
    let req: SyncFolderItemsRequest = match parse_body_content(xml) {
        Ok(r) => r,
        Err(_) => return soap_fault("ErrorInvalidRequest", "Bad XML"),
    };
    let folder_id = req
        .sync_folder_id
        .folder_id
        .map(|f| f.id)
        .or(req.sync_folder_id.distinguished_id.map(|d| d.id))
        .unwrap_or_else(|| "default".to_string());
    let stored = match db::get_ews_sync_state(config, user, &folder_id).await {
        Ok(state) => state,
        Err(e) => {
            tracing::error!(
                user = user, folder = %folder_id,
                "SyncFolderItems: failed to fetch sync state: {e}"
            );
            return soap_fault("ErrorInternalServerError", "Failed to read sync state");
        }
    };
    let current_state = match jmap_client::get_calendar_state(session).await {
        Ok(s) => s,
        Err(e) => {
            tracing::error!(
                user = user, folder = %folder_id,
                "SyncFolderItems: get_calendar_state failed: {e}"
            );
            return soap_fault("ErrorInternalServerError", "State Error");
        }
    };
    let new_sync_token = Uuid::new_v4().to_string();

    // Determine whether this is an initial or delta sync.
    // If the client supplies a SyncState that doesn't match the last token we
    // issued, reject it so the client performs a clean re-sync rather than
    // silently computing deltas from the wrong baseline.
    let is_initial = match (&req.sync_state, &stored) {
        (None, _) => true,
        (Some(_), None) => {
            // Server-side state was cleared (e.g. after a permanent
            // get_calendar_changes failure).  Fall back to a full initial
            // sync so the client can recover, consistent with
            // handle_sync_folder_hierarchy.
            tracing::warn!(
                user = user, folder = %folder_id,
                "SyncFolderItems: client sent SyncState but server state is missing; falling back to initial sync"
            );
            true
        }
        (Some(client_token), Some(s)) => {
            if *client_token != s.sync_state {
                // Clear the stored state so the client's next request sees
                // `stored = None` and falls back to a full initial sync,
                // consistent with the permanent-error path in
                // get_calendar_changes.
                if let Err(del_err) = db::delete_ews_sync_state(config, user, &folder_id).await {
                    tracing::error!(
                        user = user, folder = %folder_id,
                        "SyncFolderItems: failed to delete stale sync state: {del_err}"
                    );
                }
                return soap_fault(
                    "ErrorInvalidSyncStateData",
                    "SyncState does not match; please re-sync",
                );
            }
            false
        }
    };

    let (changes_xml, includes_last, jmap_state_to_persist) = if is_initial {
        let events = match jmap_client::get_calendar_events(session).await {
            Ok(e) => e,
            Err(e) => {
                tracing::error!("get_calendar_events failed during initial sync: {}", e);
                return soap_fault("ErrorInternalServerError", "Sync Failed");
            }
        };
        let mut xml = String::new();
        for ev in events {
            let rendered = render_ews_calendar_item(&ev, &config.timezone);
            xml.push_str(&format!(r#"<t:Create>{}</t:Create>"#, rendered));
        }
        (xml, true, current_state)
    } else {
        let Some(ref stored_state) = stored else {
            tracing::error!("is_initial is false but stored is None — logic error");
            return soap_fault("ErrorInternalServerError", "Internal sync state error");
        };
        let prev_jmap_state = &stored_state.jmap_state;
        if *prev_jmap_state == current_state {
            // Persist the new sync token so the client's next request
            // maps back to the current JMAP state (avoids an unnecessary
            // full re-sync if the stored token were to drift).
            if let Err(e) =
                db::update_ews_sync_state(config, user, &folder_id, &new_sync_token, &current_state)
                    .await
            {
                tracing::error!(
                    user = user, folder = %folder_id,
                    "SyncFolderItems: failed to persist sync state: {e}"
                );
                return soap_fault("ErrorInternalServerError", "Failed to persist sync state");
            }
            return soap_response(&format!(
                r#"<m:SyncFolderItemsResponse xmlns:m="{}" xmlns:t="{}"><m:ResponseMessages><m:SyncFolderItemsResponseMessage ResponseClass="Success"><m:ResponseCode>NoError</m:ResponseCode><m:SyncState>{}</m:SyncState><m:IncludesLastItemInRange>true</m:IncludesLastItemInRange><m:Changes /></m:SyncFolderItemsResponseMessage></m:ResponseMessages></m:SyncFolderItemsResponse>"#,
                NS_M,
                NS_T,
                utils::escape_xml(&new_sync_token)
            ));
        }
        let changes = match jmap_client::get_calendar_changes(session, prev_jmap_state).await {
            Ok(c) => c,
            Err(e) if e.is_transient() => {
                // Network / connection error – don't invalidate the sync state
                // because the server state is likely still valid.
                tracing::warn!("get_calendar_changes transient failure: {}", e);
                return soap_fault("ErrorInternalServerError", "Temporary error, please retry");
            }
            Err(e) => {
                // Permanent error (expired/invalid sinceState, auth failure,
                // etc.) – clear stored state so the client's next request
                // triggers a proper initial (full) sync.
                tracing::warn!(
                    "get_calendar_changes failed, invalidating sync state: {}",
                    e
                );
                if let Err(del_err) = db::delete_ews_sync_state(config, user, &folder_id).await {
                    tracing::error!("additionally failed to delete EWS sync state: {}", del_err);
                }
                return soap_fault(
                    "ErrorInvalidSyncStateData",
                    "Sync state expired, please re-sync",
                );
            }
        };
        // Capture the resulting JMAP state from /changes *before* consuming
        // the struct fields, so we persist the authoritative state rather
        // than the potentially stale `current_state` snapshot.
        let resulting_state = changes.new_state.clone();
        let mut xml = String::new();
        for id in changes.destroyed {
            xml.push_str(&format!(
                r#"<t:Delete><t:ItemId Id="{}"/></t:Delete>"#,
                utils::escape_xml(&id)
            ));
        }
        if !changes.created.is_empty() {
            match jmap_client::get_events_by_ids(session, &changes.created).await {
                Ok(events) => {
                    for ev in events {
                        let rendered = render_ews_calendar_item(&ev, &config.timezone);
                        xml.push_str(&format!(r#"<t:Create>{}</t:Create>"#, rendered));
                    }
                }
                Err(e) => {
                    tracing::error!("Failed to fetch created events during delta sync: {}", e);
                    return soap_fault("ErrorInternalServerError", "Sync Failed");
                }
            }
        }
        if !changes.updated.is_empty() {
            match jmap_client::get_events_by_ids(session, &changes.updated).await {
                Ok(events) => {
                    for ev in events {
                        let rendered = render_ews_calendar_item(&ev, &config.timezone);
                        xml.push_str(&format!(r#"<t:Update>{}</t:Update>"#, rendered));
                    }
                }
                Err(e) => {
                    tracing::error!("Failed to fetch updated events during delta sync: {}", e);
                    return soap_fault("ErrorInternalServerError", "Sync Failed");
                }
            }
        }
        (xml, true, resulting_state)
    };
    if let Err(e) = db::update_ews_sync_state(
        config,
        user,
        &folder_id,
        &new_sync_token,
        &jmap_state_to_persist,
    )
    .await
    {
        tracing::error!(
            user = user, folder = %folder_id,
            "SyncFolderItems: failed to persist sync state: {e}"
        );
        return soap_fault("ErrorInternalServerError", "Failed to persist sync state");
    }
    soap_response(&format!(
        r#"<m:SyncFolderItemsResponse xmlns:m="{}" xmlns:t="{}"><m:ResponseMessages><m:SyncFolderItemsResponseMessage ResponseClass="Success"><m:ResponseCode>NoError</m:ResponseCode><m:SyncState>{}</m:SyncState><m:IncludesLastItemInRange>{}</m:IncludesLastItemInRange><m:Changes>{}</m:Changes></m:SyncFolderItemsResponseMessage></m:ResponseMessages></m:SyncFolderItemsResponse>"#,
        NS_M,
        NS_T,
        utils::escape_xml(&new_sync_token),
        includes_last,
        changes_xml
    ))
}

async fn handle_create_item(
    session: &jmap_client::JmapSession,
    config: &AppConfig,
    xml: &str,
) -> String {
    let req: CreateItemRequest = match parse_body_content(xml) {
        Ok(r) => r,
        Err(_) => return soap_fault("ErrorInvalidRequest", "Bad XML"),
    };
    if let Some(item) = req.items.calendar_item {
        let tz: Tz = config.timezone.parse().unwrap_or(chrono_tz::UTC);
        let cal_id = match jmap_client::get_default_calendar_id(session).await {
            Ok(id) => id,
            Err(e) => {
                tracing::error!("CreateItem: failed to get default calendar ID: {e}");
                return soap_fault("ErrorInternalServerError", "Failed to get calendar");
            }
        };
        let raw_start = match item.start {
            Some(s) if !s.is_empty() => s,
            _ => return soap_fault("ErrorMissingArgument", "Start is required"),
        };
        let raw_end = match item.end {
            Some(s) if !s.is_empty() => s,
            _ => return soap_fault("ErrorMissingArgument", "End is required"),
        };
        let attendees: Vec<jmap_client::Participant> = item
            .required_attendees
            .map(|a| {
                a.attendees
                    .into_iter()
                    .map(|att| jmap_client::Participant {
                        email: att.mailbox.email,
                        name: att.mailbox.name.unwrap_or_default(),
                        status: None,
                        participant_id: None,
                    })
                    .collect()
            })
            .unwrap_or_default();
        let start_utc = utils::parse_local_to_utc(&raw_start, tz);
        let event = jmap_client::JmapEvent {
            id: None,
            title: item.subject.unwrap_or_default(),
            start: start_utc.clone(),
            end: utils::parse_local_to_utc(&raw_end, tz),
            location: item.location,
            description: item.body.map(|b| b.content),
            uid: Some(Uuid::new_v4().to_string()),
            show_without_time: false,
            participants: if attendees.is_empty() {
                None
            } else {
                Some(attendees)
            },
            recurrence_rules: None,
            updated: None,
        };
        match jmap_client::push_event(session, event, &cal_id).await {
            Ok(created) => {
                let change_key = {
                    let mut h = sha2::Sha256::new();
                    sha2::Digest::update(&mut h, created.id.as_str());
                    sha2::Digest::update(&mut h, created.updated.as_deref().unwrap_or(&start_utc));
                    base64::Engine::encode(&base64::engine::general_purpose::STANDARD, h.finalize())
                };
                return soap_response(&format!(
                    r#"<m:CreateItemResponse xmlns:m="{}" xmlns:t="{}"><m:ResponseMessages><m:CreateItemResponseMessage ResponseClass="Success"><m:ResponseCode>NoError</m:ResponseCode><m:Items><t:CalendarItem><t:ItemId Id="{}" ChangeKey="{}" /></t:CalendarItem></m:Items></m:CreateItemResponseMessage></m:ResponseMessages></m:CreateItemResponse>"#,
                    NS_M,
                    NS_T,
                    utils::escape_xml(&created.id),
                    utils::escape_xml(&change_key),
                ));
            }
            Err(e) => {
                tracing::error!("Failed to push JMAP event: {}", e);
                return soap_fault("ErrorInternalServerError", "JMAP Create Failed");
            }
        }
    }
    soap_fault("ErrorInvalidRequest", "No Item")
}

async fn handle_delete_item(
    session: &jmap_client::JmapSession,
    _config: &AppConfig,
    xml: &str,
) -> String {
    let req: DeleteItemRequest = match parse_body_content(xml) {
        Ok(r) => r,
        Err(_) => return soap_fault("ErrorInvalidRequest", "Bad XML"),
    };
    let ids: Vec<String> = req.item_ids.items.into_iter().map(|i| i.id).collect();
    let destroy_result = match jmap_client::destroy_events(session, ids.clone()).await {
        Ok(r) => r,
        Err(e) => {
            tracing::error!("destroy_events failed: {}", e);
            return soap_fault("ErrorInternalServerError", "Delete Failed");
        }
    };
    if let Some(ref e) = destroy_result.chunk_error {
        tracing::error!("destroy_events chunk error: {}", e);
    }
    let mut msgs = String::new();
    for id in &ids {
        if let Some((_, jmap_err)) = destroy_result
            .not_destroyed
            .iter()
            .find(|(fid, _)| fid == id)
        {
            let ews_code = match jmap_err.as_str() {
                "notFound" => "ErrorItemNotFound",
                "forbidden" | "accountReadOnly" => "ErrorAccessDenied",
                _ => "ErrorInternalServerError",
            };
            msgs.push_str(&format!(
                r#"<m:DeleteItemResponseMessage ResponseClass="Error"><m:ResponseCode>{}</m:ResponseCode><m:MessageText>Failed to delete item {}</m:MessageText></m:DeleteItemResponseMessage>"#,
                ews_code,
                utils::escape_xml(id)
            ));
        } else {
            msgs.push_str(
                r#"<m:DeleteItemResponseMessage ResponseClass="Success"><m:ResponseCode>NoError</m:ResponseCode></m:DeleteItemResponseMessage>"#,
            );
        }
    }
    soap_response(&format!(
        r#"<m:DeleteItemResponse xmlns:m="{}" xmlns:t="{}"><m:ResponseMessages>{}</m:ResponseMessages></m:DeleteItemResponse>"#,
        NS_M, NS_T, msgs
    ))
}

async fn handle_get_folder(session: &jmap_client::JmapSession, _xml: &str) -> String {
    let cal_id = match jmap_client::get_default_calendar_id(session).await {
        Ok(id) => id,
        Err(e) => {
            tracing::error!("GetFolder: failed to get default calendar ID: {}", e);
            return soap_fault("ErrorInternalServerError", "Failed to get calendar");
        }
    };
    soap_response(&format!(
        r#"<m:GetFolderResponse xmlns:m="{}" xmlns:t="{}"><m:ResponseMessages><m:GetFolderResponseMessage ResponseClass="Success"><m:ResponseCode>NoError</m:ResponseCode><m:Folders><t:CalendarFolder><t:FolderId Id="{}" ChangeKey="AQAAABYAAA=" /><t:DisplayName>Calendar</t:DisplayName></t:CalendarFolder></m:Folders></m:GetFolderResponseMessage></m:ResponseMessages></m:GetFolderResponse>"#,
        NS_M,
        NS_T,
        utils::escape_xml(&cal_id)
    ))
}

async fn handle_find_item() -> String {
    soap_response(&format!(
        r#"<m:FindItemResponse xmlns:m="{}" xmlns:t="{}"><m:ResponseMessages><m:FindItemResponseMessage ResponseClass="Success"><m:ResponseCode>NoError</m:ResponseCode><m:RootFolder IndexedPagingOffset="0" TotalItemsInView="0" IncludesLastItemInRange="true"><t:Items /></m:RootFolder></m:FindItemResponseMessage></m:ResponseMessages></m:FindItemResponse>"#,
        NS_M, NS_T
    ))
}

async fn handle_get_room_lists() -> String {
    soap_response(&format!(
        r#"<m:GetRoomListsResponse xmlns:m="{}" xmlns:t="{}"><m:ResponseMessages><m:GetRoomListsResponseMessage ResponseClass="Success"><m:ResponseCode>NoError</m:ResponseCode><m:RoomLists /></m:GetRoomListsResponseMessage></m:ResponseMessages></m:GetRoomListsResponse>"#,
        NS_M, NS_T
    ))
}

async fn handle_get_rooms() -> String {
    soap_response(&format!(
        r#"<m:GetRoomsResponse xmlns:m="{}" xmlns:t="{}"><m:ResponseMessages><m:GetRoomsResponseMessage ResponseClass="Success"><m:ResponseCode>NoError</m:ResponseCode><m:Rooms /></m:GetRoomsResponseMessage></m:ResponseMessages></m:GetRoomsResponse>"#,
        NS_M, NS_T
    ))
}

fn extract_action_name(xml: &str) -> String {
    let mut reader = Reader::from_str(xml);
    let mut buf = Vec::new();
    let mut depth = 0;
    let mut in_body = false;
    loop {
        buf.clear();
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(ref e)) | Ok(Event::Empty(ref e)) => {
                let name = String::from_utf8_lossy(e.local_name().as_ref()).to_string();
                if name == "Body" {
                    in_body = true;
                    depth += 1;
                    continue;
                }
                if in_body && depth == 1 {
                    return name;
                }
                if in_body {
                    depth += 1;
                }
            }
            Ok(Event::End(ref e)) => {
                if String::from_utf8_lossy(e.local_name().as_ref()) == "Body" {
                    in_body = false;
                }
                if in_body {
                    depth -= 1;
                }
            }
            Ok(Event::Eof) => break,
            _ => {}
        }
    }
    String::new()
}

fn parse_body_content<T: for<'de> Deserialize<'de>>(xml: &str) -> Result<T, String> {
    let envelope: SoapEnvelope<T> =
        quick_xml::de::from_str(xml).map_err(|e| format!("Deserialize Error: {}", e))?;
    Ok(envelope.body.content)
}

#[derive(Debug, Deserialize)]
struct SoapEnvelope<T> {
    #[serde(rename = "Body")]
    body: SoapBody<T>,
}

#[derive(Debug, Deserialize)]
struct SoapBody<T> {
    #[serde(rename = "$value")]
    content: T,
}

fn soap_response(content: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="utf-8"?><soap:Envelope xmlns:soap="{}" xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance" xmlns:xsd="http://www.w3.org/2001/XMLSchema"><soap:Body>{}</soap:Body></soap:Envelope>"#,
        NS_SOAP, content
    )
}
fn soap_fault(code: &str, msg: &str) -> String {
    soap_response(&format!(
        r#"<soap:Fault><faultcode>soap:Client</faultcode><faultstring>{}: {}</faultstring></soap:Fault>"#,
        utils::escape_xml(code),
        utils::escape_xml(msg)
    ))
}
#[derive(Debug, Deserialize)]
struct ResolveNamesRequest {
    #[serde(rename = "UnresolvedEntry")]
    unresolved_entry: String,
}
#[derive(Debug, Deserialize)]
struct GetAttachmentRequest {
    #[serde(rename = "AttachmentIds")]
    attachment_ids: EwsAttachmentIds,
}
#[derive(Debug, Deserialize)]
struct EwsAttachmentIds {
    #[serde(rename = "AttachmentId")]
    #[serde(default)]
    items: Vec<EwsAttachmentId>,
}
#[derive(Debug, Deserialize)]
struct EwsAttachmentId {
    #[serde(rename = "@Id")]
    id: String,
}
#[derive(Debug, Deserialize)]
struct GetItemRequest {
    #[serde(rename = "ItemIds")]
    item_ids: EwsItemIds,
}
#[derive(Debug, Deserialize)]
struct EwsItemIds {
    #[serde(rename = "ItemId")]
    #[serde(default)]
    items: Vec<EwsItemId>,
}
#[derive(Debug, Deserialize)]
struct EwsItemId {
    #[serde(rename = "@Id")]
    id: String,
}
#[derive(Debug, Deserialize)]
struct UpdateItemRequest {
    #[serde(rename = "ItemChanges")]
    item_changes: EwsItemChanges,
}
#[derive(Debug, Deserialize)]
struct EwsItemChanges {
    #[serde(rename = "ItemChange")]
    #[serde(default)]
    items: Vec<EwsItemChange>,
}
#[derive(Debug, Deserialize)]
struct EwsItemChange {
    #[serde(rename = "ItemId")]
    item_id: EwsItemId,
    #[serde(rename = "Updates")]
    updates: EwsUpdates,
}
#[derive(Debug, Deserialize)]
struct EwsUpdates {
    #[serde(rename = "SetItemField")]
    #[serde(default)]
    set_fields: Vec<SetItemField>,
    #[serde(rename = "DeleteItemField")]
    #[serde(default)]
    delete_fields: Vec<DeleteItemField>,
    #[serde(rename = "AppendToItemField")]
    #[serde(default)]
    append_fields: Vec<AppendToItemField>,
}
#[derive(Debug, Deserialize)]
struct DeleteItemField {
    #[serde(rename = "FieldURI")]
    field_uri: FieldURI,
}
#[derive(Debug, Deserialize)]
struct AppendToItemField {
    #[serde(rename = "FieldURI")]
    field_uri: FieldURI,
    #[serde(rename = "CalendarItem")]
    calendar_item: EwsCalendarItem,
}
#[derive(Debug, Deserialize)]
struct SetItemField {
    #[serde(rename = "FieldURI")]
    field_uri: FieldURI,
    #[serde(rename = "CalendarItem")]
    calendar_item: EwsCalendarItem,
}
#[derive(Debug, Deserialize)]
struct FieldURI {
    #[serde(rename = "@FieldURI")]
    field_uri: String,
}
#[derive(Debug, Deserialize, Default)]
struct EwsCalendarItem {
    #[serde(rename = "Subject", default)]
    subject: Option<String>,
    #[serde(rename = "Body", default)]
    body: Option<EwsBody>,
    #[serde(rename = "Start", default)]
    start: Option<String>,
    #[serde(rename = "End", default)]
    end: Option<String>,
    #[serde(rename = "Location", default)]
    location: Option<String>,
    #[serde(rename = "RequiredAttendees", default)]
    required_attendees: Option<EwsRequiredAttendees>,
}
#[derive(Debug, Deserialize)]
struct EwsRequiredAttendees {
    #[serde(rename = "Attendee", default)]
    attendees: Vec<EwsAttendee>,
}
#[derive(Debug, Deserialize)]
struct EwsAttendee {
    #[serde(rename = "Mailbox")]
    mailbox: EwsMailbox,
}
#[derive(Debug, Deserialize)]
struct EwsMailbox {
    #[serde(rename = "EmailAddress", default)]
    email: String,
    #[serde(rename = "Name", default)]
    name: Option<String>,
}
#[derive(Debug, Deserialize)]
struct EwsBody {
    #[serde(rename = "$value", default)]
    content: String,
}
#[derive(Debug, Deserialize)]
struct SyncFolderItemsRequest {
    #[serde(rename = "SyncFolderId")]
    sync_folder_id: SyncFolderId,
    #[serde(rename = "SyncState", default)]
    sync_state: Option<String>,
}
#[derive(Debug, Deserialize)]
struct SyncFolderId {
    #[serde(rename = "FolderId", default)]
    folder_id: Option<FolderId>,
    #[serde(rename = "DistinguishedFolderId", default)]
    distinguished_id: Option<DistinguishedFolderId>,
}
#[derive(Debug, Deserialize)]
struct FolderId {
    #[serde(rename = "@Id")]
    id: String,
}
#[derive(Debug, Deserialize)]
struct DistinguishedFolderId {
    #[serde(rename = "@Id")]
    id: String,
}
#[derive(Debug, Deserialize)]
struct CreateItemRequest {
    #[serde(rename = "Items")]
    items: EwsItems,
}
#[derive(Debug, Deserialize)]
struct EwsItems {
    #[serde(rename = "CalendarItem")]
    #[serde(default)]
    calendar_item: Option<EwsCalendarItem>,
}
#[derive(Debug, Deserialize)]
struct DeleteItemRequest {
    #[serde(rename = "ItemIds")]
    item_ids: EwsItemIds,
}
#[derive(Debug, Deserialize)]
struct SyncFolderHierarchyRequest {
    #[serde(rename = "SyncState", default)]
    sync_state: Option<String>,
} 
