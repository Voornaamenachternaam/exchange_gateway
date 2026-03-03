use crate::{config::AppConfig, db, jmap_client, utils};
use axum::http::HeaderMap;
use chrono::{DateTime, TimeZone, Utc};
use chrono_tz::Tz;
use quick_xml::Reader;
use quick_xml::events::Event;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use uuid::Uuid;

const NS_SOAP: &str = "http://schemas.xmlsoap.org/soap/envelope/";
const NS_M: &str = "http://schemas.microsoft.com/exchange/services/2006/messages";
const NS_T: &str = "http://schemas.microsoft.com/exchange/services/2006/types";

pub async fn process_request(config: &AppConfig, xml: &str, headers: &HeaderMap) -> String {
    let auth = match headers.get("Authorization").and_then(|v| v.to_str().ok()) {
        Some(a) => a,
        None => return soap_fault("ErrorAccessDenied", "Missing Authorization"),
    };
    let (user, pass) = utils::decode_basic_auth(auth);
    let session = match jmap_client::get_session(&config.jmap_url, &user, &pass).await {
        Ok(s) => s,
        Err(_) => return soap_fault("ErrorInternalServerError", "Auth Failed"),
    };
    let action = extract_action_name(xml);
    tracing::info!("EWS Request: {}", action);

    match action.as_str() {
        "GetFolder" => handle_get_folder(&session, xml).await,
        "FindFolder" => handle_find_folder(&session).await,
        "SyncFolderHierarchy" => handle_sync_folder_hierarchy(&session).await,
        "SyncFolderItems" => handle_sync_folder_items(&session, config, &user, xml).await,
        "CreateItem" => handle_create_item(&session, config, xml).await,
        "UpdateItem" => handle_update_item(&session, config, xml).await,
        "DeleteItem" => handle_delete_item(&session, config, xml).await,
        "GetItem" => handle_get_item(&session, config, xml).await,
        "FindItem" => handle_find_item().await,
        "ResolveNames" => handle_resolve_names(&session, xml).await,
        "GetAttachment" => handle_get_attachment(&session, xml).await,
        _ => soap_fault("ErrorInvalidRequest", &format!("Unsupported: {}", action)),
    }
}

async fn handle_sync_folder_hierarchy(session: &jmap_client::JmapSession) -> String {
    let cal_id = jmap_client::get_default_calendar_id(session)
        .await
        .unwrap_or("default".into());
    soap_response(&format!(
        r#"<m:SyncFolderHierarchyResponse xmlns:m="{}" xmlns:t="{}"><m:ResponseMessages><m:SyncFolderHierarchyResponseMessage ResponseClass="Success"><m:ResponseCode>NoError</m:ResponseCode><m:Changes><t:Create><t:CalendarFolder><t:FolderId Id="{}" ChangeKey="AQAAABYAAA=" /><t:DisplayName>Calendar</t:DisplayName></t:CalendarFolder></t:Create></m:Changes></m:SyncFolderHierarchyResponseMessage></m:ResponseMessages></m:SyncFolderHierarchyResponse>"#,
        NS_M,
        NS_T,
        utils::escape_xml(&cal_id)
    ))
}

async fn handle_find_folder(session: &jmap_client::JmapSession) -> String {
    let cal_id = jmap_client::get_default_calendar_id(session)
        .await
        .unwrap_or("default".into());
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

    let results = jmap_client::search_principals(session, &req.unresolved_entry)
        .await
        .unwrap_or_default()
        .into_iter()
        .take(MAX_RESOLVE_NAMES_RESULTS)
        .collect::<Vec<_>>();
    let mut resolutions = String::new();
    // Fix: Borrow results to iterate, then use results.len()
    for p in &results {
        resolutions.push_str(&format!(r#"<t:Resolution><t:Mailbox><t:Name>{}</t:Name><t:EmailAddress>{}</t:EmailAddress><t:RoutingType>SMTP</t:RoutingType></t:Mailbox></t:Resolution>"#, utils::escape_xml(&p.name), utils::escape_xml(&p.email)));
    }
    soap_response(&format!(
        r#"<m:ResolveNamesResponse xmlns:m="{}" xmlns:t="{}"><m:ResponseMessages><m:ResolveNamesResponseMessage ResponseClass="Success"><m:ResponseCode>NoError</m:ResponseCode><m:ResolutionSet TotalItemsInView="{}">{}</m:ResolutionSet></m:ResolveNamesResponseMessage></m:ResponseMessages></m:ResolveNamesResponse>"#,
        NS_M,
        NS_T,
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
                    utils::escape_xml(id_str), b64
                ));
            }
            Err(e) => {
                tracing::warn!("get_blob failed for attachment {}: {}", id_str, e);
                response_messages.push_str(&format!(
                    r#"<m:GetAttachmentResponseMessage ResponseClass="Error"><m:ResponseCode>ErrorItemNotFound</m:ResponseCode><m:MessageText>Attachment not found</m:MessageText><m:Attachments><t:FileAttachment><t:AttachmentId Id="{}"/></t:FileAttachment></m:Attachments></m:GetAttachmentResponseMessage>"#,
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
            if let Some(rendered) = render_ews_calendar_item(event, &config.timezone) {
                response_messages.push_str(&format!(
                    r#"<m:GetItemResponseMessage ResponseClass="Success"><m:ResponseCode>NoError</m:ResponseCode><m:Items>{}</m:Items></m:GetItemResponseMessage>"#,
                    rendered
                ));
            } else {
                response_messages.push_str(r#"<m:GetItemResponseMessage ResponseClass="Error"><m:ResponseCode>ErrorItemNotFound</m:ResponseCode><m:Items/></m:GetItemResponseMessage>"#);
            }
        } else {
            response_messages.push_str(r#"<m:GetItemResponseMessage ResponseClass="Error"><m:ResponseCode>ErrorItemNotFound</m:ResponseCode><m:Items/></m:GetItemResponseMessage>"#);
        }
    }
    soap_response(&format!(
        r#"<m:GetItemResponse xmlns:m="{}" xmlns:t="{}"><m:ResponseMessages>{}</m:ResponseMessages></m:GetItemResponse>"#,
        NS_M, NS_T, response_messages
    ))
}

fn render_ews_calendar_item(event: &jmap_client::JmapEvent, tz_str: &str) -> Option<String> {
    let tz: Tz = tz_str.parse().unwrap_or(chrono_tz::UTC);
    let start_local: DateTime<Tz> = match event.start.parse::<DateTime<Utc>>() {
        Ok(dt) => dt.with_timezone(&tz),
        Err(e) => {
            tracing::warn!(
                "Skipping event {:?}: failed to parse start timestamp '{}': {}",
                event.id,
                event.start,
                e
            );
            return None;
        }
    };
    let end_local: DateTime<Tz> = match event.end.parse::<DateTime<Utc>>() {
        Ok(dt) => dt.with_timezone(&tz),
        Err(e) => {
            tracing::warn!(
                "Skipping event {:?}: failed to parse end timestamp '{}': {}",
                event.id,
                event.end,
                e
            );
            return None;
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
    Some(format!(
        r#"<t:CalendarItem><t:ItemId Id="{}" ChangeKey="{}" /><t:Subject>{}</t:Subject><t:Body BodyType="Text">{}</t:Body><t:Start>{}</t:Start><t:End>{}</t:End><t:Location>{}</t:Location><t:IsAllDayEvent>{}</t:IsAllDayEvent>{}</t:CalendarItem>"#,
        utils::escape_xml(event.id.as_deref().unwrap_or("")),
        utils::escape_xml(&change_key),
        utils::escape_xml(&event.title),
        utils::escape_xml(event.description.as_deref().unwrap_or("")),
        start_local.format("%Y-%m-%dT%H:%M:%S"),
        end_local.format("%Y-%m-%dT%H:%M:%S"),
        utils::escape_xml(event.location.as_deref().unwrap_or("")),
        event.is_all_day,
        attendees_xml
    ))
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
    for change in req.item_changes.items {
        let id = change.item_id.id;
        let mut patch = serde_json::Map::new();
        let tz: Tz = config.timezone.parse().unwrap_or(chrono_tz::UTC);
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
                            serde_json::json!(parse_local_to_utc(&s, tz)),
                        );
                    }
                }
                "calendar:End" => {
                    if let Some(e) = update.calendar_item.end {
                        patch.insert("end".into(), serde_json::json!(parse_local_to_utc(&e, tz)));
                    }
                }
                _ => {}
            }
        }
        if !patch.is_empty() {
            if let Err(e) = jmap_client::patch_event(session, &id, patch).await {
                tracing::error!("patch_event failed for {}: {}", id, e);
                return soap_fault("ErrorInternalServerError", "Update Failed");
            }
        }
    }
    soap_response(&format!(
        r#"<m:UpdateItemResponse xmlns:m="{}" xmlns:t="{}"><m:ResponseMessages><m:UpdateItemResponseMessage ResponseClass="Success"><m:ResponseCode>NoError</m:ResponseCode></m:UpdateItemResponseMessage></m:ResponseMessages></m:UpdateItemResponse>"#,
        NS_M, NS_T
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
    // Fix: Use to_string() for default
    let folder_id = req
        .sync_folder_id
        .folder_id
        .map(|f| f.id)
        .unwrap_or_else(|| "default".to_string());
    let prev_state = db::get_ews_sync_state(config, user, &folder_id).await;
    let current_state = match jmap_client::get_calendar_state(session).await {
        Ok(s) => s,
        Err(_) => return soap_fault("ErrorInternalServerError", "State Error"),
    };
    let new_sync_token = Uuid::new_v4().to_string();
    let is_initial = req.sync_state.is_none() || prev_state.is_none();

    let (changes_xml, includes_last) = if is_initial {
        let events = match jmap_client::get_calendar_events(session).await {
            Ok(e) => e,
            Err(e) => {
                tracing::error!("get_calendar_events failed during initial sync: {}", e);
                return soap_fault("ErrorInternalServerError", "Sync Failed");
            }
        };
        let mut xml = String::new();
        for ev in events {
            if let Some(rendered) = render_ews_calendar_item(&ev, &config.timezone) {
                xml.push_str(&format!(r#"<t:Create>{}</t:Create>"#, rendered));
            }
        }
        (xml, true)
    } else {
        // When is_initial is false, prev_state is guaranteed to be Some
        // (is_initial includes prev_state.is_none() in its condition).
        let prev_jmap_state = prev_state.expect("prev_state must be Some when is_initial is false");
        if prev_jmap_state == current_state {
            return soap_response(&format!(
                r#"<m:SyncFolderItemsResponse xmlns:m="{}" xmlns:t="{}"><m:ResponseMessages><m:SyncFolderItemsResponseMessage ResponseClass="Success"><m:ResponseCode>NoError</m:ResponseCode><m:SyncState>{}</m:SyncState><m:IncludesLastItemInRange>true</m:IncludesLastItemInRange><m:Changes /></m:SyncFolderItemsResponseMessage></m:ResponseMessages></m:SyncFolderItemsResponse>"#,
                NS_M,
                NS_T,
                utils::escape_xml(&req.sync_state.unwrap_or_default())
            ));
        }
        let changes = match jmap_client::get_calendar_changes(session, &prev_jmap_state).await {
            Ok(c) => c,
            Err(e) => {
                // sinceState may be expired/invalid; clear stored state so the
                // client's next request triggers a proper initial (full) sync
                // that includes both creates and deletions.
                tracing::warn!("get_calendar_changes failed, invalidating sync state: {}", e);
                db::delete_ews_sync_state(config, user, &folder_id).await;
                return soap_fault("ErrorInvalidSyncStateData", "Sync state expired, please re-sync");
            }
        };
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
                        if let Some(rendered) = render_ews_calendar_item(&ev, &config.timezone) {
                            xml.push_str(&format!(r#"<t:Create>{}</t:Create>"#, rendered));
                        }
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
                        if let Some(rendered) = render_ews_calendar_item(&ev, &config.timezone) {
                            xml.push_str(&format!(r#"<t:Update>{}</t:Update>"#, rendered));
                        }
                    }
                }
                Err(e) => {
                    tracing::error!("Failed to fetch updated events during delta sync: {}", e);
                    return soap_fault("ErrorInternalServerError", "Sync Failed");
                }
            }
        }
        (xml, true)
    };
    db::update_ews_sync_state(config, user, &folder_id, &new_sync_token, &current_state).await;
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
        let cal_id = jmap_client::get_default_calendar_id(session)
            .await
            .unwrap_or("default".into());
        let attendees: Vec<jmap_client::Participant> = item
            .required_attendees
            .map(|a| {
                a.attendees
                    .into_iter()
                    .map(|att| jmap_client::Participant {
                        email: att.mailbox.email,
                        name: att.mailbox.name.unwrap_or_default(),
                        status: None,
                    })
                    .collect()
            })
            .unwrap_or_default();
        let event = jmap_client::JmapEvent {
            id: None,
            title: item.subject.unwrap_or_default(),
            start: parse_local_to_utc(&item.start.unwrap_or_default(), tz),
            end: parse_local_to_utc(&item.end.unwrap_or_default(), tz),
            location: item.location,
            description: item.body.map(|b| b.content),
            uid: Some(Uuid::new_v4().to_string()),
            is_all_day: false,
            participants: if attendees.is_empty() {
                None
            } else {
                Some(attendees)
            },
            recurrence_rule: None,
            updated: None,
        };
        match jmap_client::push_event(session, event, &cal_id).await {
            Ok(id) => {
                return soap_response(&format!(
                    r#"<m:CreateItemResponse xmlns:m="{}" xmlns:t="{}"><m:ResponseMessages><m:CreateItemResponseMessage ResponseClass="Success"><m:ResponseCode>NoError</m:ResponseCode><m:Items><t:CalendarItem><t:ItemId Id="{}" ChangeKey="{}" /></t:CalendarItem></m:Items></m:CreateItemResponseMessage></m:ResponseMessages></m:CreateItemResponse>"#,
                    NS_M,
                    NS_T,
                    utils::escape_xml(&id),
                    utils::escape_xml(&id),
                    utils::escape_xml(&{
                        let mut h = sha2::Sha256::new();
                        sha2::Digest::update(&mut h, &id);
                        base64::Engine::encode(&base64::engine::general_purpose::STANDARD, h.finalize())
                    })
                ));
            }
            Err(_) => return soap_fault("ErrorInternalServerError", "JMAP Create Failed"),
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
    if let Err(e) = jmap_client::destroy_events(session, ids).await {
        tracing::error!("destroy_events failed: {}", e);
        return soap_fault("ErrorInternalServerError", "Delete Failed");
    }
    soap_response(&format!(
        r#"<m:DeleteItemResponse xmlns:m="{}" xmlns:t="{}"><m:ResponseMessages><m:DeleteItemResponseMessage ResponseClass="Success"><m:ResponseCode>NoError</m:ResponseCode></m:DeleteItemResponseMessage></m:ResponseMessages></m:DeleteItemResponse>"#,
        NS_M, NS_T
    ))
}

async fn handle_get_folder(session: &jmap_client::JmapSession, _xml: &str) -> String {
    let cal_id = jmap_client::get_default_calendar_id(session)
        .await
        .unwrap_or("default".into());
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

fn extract_action_name(xml: &str) -> String {
    let mut reader = Reader::from_str(xml);
    let mut buf = Vec::new();
    let mut depth = 0;
    let mut in_body = false;
    loop {
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
    // Use quick_xml Reader to find the Body element by local name,
    // which is namespace-prefix-agnostic (handles soap:Body, s:Body, SOAP-ENV:Body, etc.)
    // We track byte positions via buffer_position() to slice the original string.
    // For a Reader created from &str, buffer_position() returns the byte offset
    // into the original input, which is safe to use for slicing.
    let mut reader = Reader::from_str(xml);
    let mut buf = Vec::new();
    let body_start;
    // Find the opening Body tag; after this, buffer_position() points just past the '>'
    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(ref e)) => {
                if e.local_name().as_ref() == b"Body" {
                    body_start = reader.buffer_position();
                    break;
                }
            }
            Ok(Event::Eof) => return Err("Could not find SOAP Body".into()),
            Err(e) => return Err(format!("XML parse error: {}", e)),
            _ => {}
        }
        buf.clear();
    }
    // Find the matching closing Body tag by reading events and tracking depth.
    let mut depth = 1u32;
    buf.clear();
    loop {
        let pos_before = reader.buffer_position();
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(ref e)) => {
                if e.local_name().as_ref() == b"Body" {
                    depth += 1;
                }
            }
            Ok(Event::End(ref e)) => {
                if e.local_name().as_ref() == b"Body" {
                    depth -= 1;
                    if depth == 0 {
                        let inner = &xml[body_start..pos_before];
                        return quick_xml::de::from_str(inner)
                            .map_err(|e| format!("Deserialize Error: {}", e));
                    }
                }
            }
            Ok(Event::Eof) => return Err("Could not find SOAP Body closing tag".into()),
            Err(e) => return Err(format!("XML parse error: {}", e)),
            _ => {}
        }
        buf.clear();
    }
}

fn soap_response(content: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="utf-8"?><soap:Envelope xmlns:soap="{}" xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance" xmlns:xsd="http://www.w3.org/2001/XMLSchema"><soap:Body>{}</soap:Body></soap:Envelope>"#,
        NS_SOAP, content
    )
}
fn soap_fault(code: &str, msg: &str) -> String {
    soap_response(&format!(
        r#"<soap:Fault><faultcode>{}</faultcode><faultstring>{}</faultstring></soap:Fault>"#,
        utils::escape_xml(code), utils::escape_xml(msg)
    ))
}
fn parse_local_to_utc(local_str: &str, tz: Tz) -> String {
    if let Ok(dt) = chrono::NaiveDateTime::parse_from_str(local_str, "%Y-%m-%dT%H:%M:%SZ") {
        return Utc.from_utc_datetime(&dt).to_rfc3339();
    }
    if let Ok(dt) = chrono::NaiveDateTime::parse_from_str(local_str, "%Y-%m-%dT%H:%M:%S") {
        return tz
            .from_local_datetime(&dt)
            .single()
            .map(|dt| dt.with_timezone(&Utc).to_rfc3339())
            .unwrap_or_default();
    }
    local_str.to_string()
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
    set_fields: Vec<SetItemField>,
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
    #[serde(rename = "$value")]
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
    #[serde(rename = "FolderId")]
    folder_id: Option<FolderId>,
}
#[derive(Debug, Deserialize)]
struct FolderId {
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
    calendar_item: Option<EwsCalendarItem>,
}
#[derive(Debug, Deserialize)]
struct DeleteItemRequest {
    #[serde(rename = "ItemIds")]
    item_ids: EwsItemIds,
}
