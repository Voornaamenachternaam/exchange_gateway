// src/ews.rs
use crate::{config::AppConfig, db, jmap_client, utils};
use axum::http::HeaderMap;
use chrono::{DateTime, TimeZone, Utc};
use chrono_tz::Tz;
use quick_xml::Reader;
use quick_xml::escape;
use quick_xml::events::Event;
use serde::{Deserialize, Serialize};

// EWS SOAP Namespace Constants
const NS_SOAP: &str = "http://schemas.xmlsoap.org/soap/envelope/";
const NS_M: &str = "http://schemas.microsoft.com/exchange/services/2006/messages";
const NS_T: &str = "http://schemas.microsoft.com/exchange/services/2006/types";

#[derive(Debug, Serialize, Deserialize)]
struct SoapEnvelope<T> {
    #[serde(rename = "Body")]
    body: SoapBody<T>,
}

#[derive(Debug, Serialize, Deserialize)]
struct SoapBody<T> {
    #[serde(rename = "$value")]
    content: T,
}

// --- Request Structs ---

#[derive(Debug, Serialize, Deserialize, Default)]
struct GetFolder {
    #[serde(rename = "FolderIds", default)]
    folder_ids: FolderIds,
    #[serde(rename = "FolderShape", default)]
    folder_shape: FolderShape,
}

#[derive(Debug, Serialize, Deserialize, Default)]
struct FolderIds {
    #[serde(rename = "DistinguishedFolderId", default)]
    distinguished: Option<DistinguishedFolderId>,
}

#[derive(Debug, Serialize, Deserialize)]
struct DistinguishedFolderId {
    #[serde(rename = "@Id", default)]
    id: String,
}

#[derive(Debug, Serialize, Deserialize, Default)]
struct FolderShape {
    #[serde(rename = "BaseShape", default)]
    base_shape: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct SyncFolderItems {
    #[serde(rename = "ItemShape", default)]
    item_shape: ItemShape,
    #[serde(rename = "SyncFolderId", default)]
    sync_folder_id: SyncFolderId,
    #[serde(rename = "SyncState", default)]
    sync_state: Option<String>,
    #[serde(rename = "MaxChangesReturned", default)]
    max_changes: String,
}

#[derive(Debug, Serialize, Deserialize, Default)]
struct ItemShape {
    #[serde(rename = "BaseShape", default)]
    base_shape: String,
    #[serde(rename = "BodyType", default)]
    body_type: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, Default)]
struct SyncFolderId {
    #[serde(rename = "FolderId", default)]
    folder_id: Option<FolderId>,
    #[serde(rename = "DistinguishedFolderId", default)]
    distinguished_id: Option<DistinguishedFolderId>,
}

#[derive(Debug, Serialize, Deserialize, Default)]
struct FolderId {
    #[serde(rename = "@Id", default)]
    id: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct CreateItem {
    #[serde(rename = "Items", default)]
    items: EwsItems,
}

#[derive(Debug, Serialize, Deserialize, Default)]
struct EwsItems {
    #[serde(rename = "CalendarItem", default)]
    calendar_item: Option<EwsCalendarItem>,
}

#[derive(Debug, Serialize, Deserialize, Default)]
struct EwsCalendarItem {
    #[serde(rename = "ItemClass", default)]
    item_class: Option<String>,
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
    required_attendees: Option<EwsAttendees>,
}

#[derive(Debug, Serialize, Deserialize, Default)]
struct EwsBody {
    #[serde(rename = "@BodyType", default)]
    body_type: String,
    #[serde(rename = "$text", default)]
    content: String,
}

#[derive(Debug, Serialize, Deserialize, Default)]
struct EwsAttendees {
    #[serde(rename = "Attendee", default)]
    attendees: Vec<EwsAttendee>,
}

#[derive(Debug, Serialize, Deserialize, Default)]
struct EwsAttendee {
    #[serde(rename = "Mailbox", default)]
    mailbox: EwsMailbox,
}

#[derive(Debug, Serialize, Deserialize, Default)]
struct EwsMailbox {
    #[serde(rename = "EmailAddress", default)]
    email: String,
    #[serde(rename = "Name", default)]
    name: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
struct UpdateItem {
    #[serde(rename = "ItemChanges", default)]
    item_changes: ItemChanges,
}

#[derive(Debug, Serialize, Deserialize, Default)]
struct ItemChanges {
    #[serde(rename = "ItemChange", default)]
    items: Vec<EwsItemChange>,
}

#[derive(Debug, Serialize, Deserialize, Default)]
struct EwsItemChange {
    #[serde(rename = "ItemId", default)]
    item_id: EwsItemId,
    #[serde(rename = "Updates", default)]
    updates: EwsUpdates,
}

#[derive(Debug, Serialize, Deserialize, Default)]
struct EwsItemId {
    #[serde(rename = "@Id", default)]
    id: String,
}

#[derive(Debug, Serialize, Deserialize, Default)]
struct EwsUpdates {
    #[serde(rename = "SetItemField", default)]
    set_fields: Vec<SetItemField>,
}

#[derive(Debug, Serialize, Deserialize, Default)]
struct SetItemField {
    #[serde(rename = "FieldURI", default)]
    field_uri: FieldURI,
    #[serde(rename = "CalendarItem", default)]
    calendar_item: EwsCalendarItem,
}

#[derive(Debug, Serialize, Deserialize, Default)]
struct FieldURI {
    #[serde(rename = "@FieldURI", default)]
    field_uri: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct DeleteItem {
    #[serde(rename = "ItemIds", default)]
    item_ids: DeleteItemIds,
}

#[derive(Debug, Serialize, Deserialize, Default)]
struct DeleteItemIds {
    #[serde(rename = "ItemId", default)]
    items: Vec<EwsItemId>,
}

#[derive(Debug, Serialize, Deserialize)]
struct GetItem {
    #[serde(rename = "ItemIds", default)]
    item_ids: GetItemIds,
}

#[derive(Debug, Serialize, Deserialize, Default)]
struct GetItemIds {
    #[serde(rename = "ItemId", default)]
    items: Vec<EwsItemId>,
}

// --- Main Processor ---

pub async fn process_request(config: &AppConfig, xml: &str, headers: &HeaderMap) -> String {
    let auth = match headers.get("Authorization").and_then(|v| v.to_str().ok()) {
        Some(a) => a,
        None => {
            return soap_fault("ErrorAccessDenied", "Missing Authorization header");
        }
    };
    let (user, pass) = utils::decode_basic_auth(auth);

    let session = match jmap_client::get_session(&config.jmap_url, &user, &pass).await {
        Ok(s) => s,
        Err(e) => {
            tracing::error!("JMAP Auth failed: {}", e);
            return soap_fault("ErrorInternalServerError", "Authentication Failed");
        }
    };

    let action = extract_action_name(xml);
    tracing::info!("EWS Action: {}", action);

    match action.as_str() {
        "GetFolder" => handle_get_folder(&session, xml).await,
        "FindFolder" => handle_find_folder(&session).await,
        "SyncFolderItems" => handle_sync_folder_items(&session, config, &user, xml).await,
        "CreateItem" => handle_create_item(&session, config, xml).await,
        "UpdateItem" => handle_update_item(&session, config, xml).await,
        "DeleteItem" => handle_delete_item(&session, config, xml).await,
        "GetItem" => handle_get_item(&session, config, xml).await,
        "FindItem" => handle_find_item(&session).await,
        "ResolveNames" => handle_resolve_names(&session).await,
        "GetRoomLists" => handle_get_room_lists().await,
        "GetRooms" => handle_get_rooms().await,
        _ => soap_fault(
            "ErrorInvalidRequest",
            &format!("Unsupported EWS Action: {}", action),
        ),
    }
}

fn extract_action_name(xml: &str) -> String {
    let mut reader = Reader::from_str(xml);
    let mut buf = Vec::new();
    let mut depth = 0;
    let mut in_body = false;

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(ref e)) | Ok(Event::Empty(ref e)) => {
                let local_name = e.local_name();
                let name = std::str::from_utf8(local_name.as_ref()).unwrap_or("");

                if name == "Body" {
                    in_body = true;
                    depth += 1;
                    continue;
                }

                if in_body && depth == 1 {
                    return name.to_string();
                }

                if in_body {
                    depth += 1;
                }
            }
            Ok(Event::End(ref e)) => {
                let local_name = e.local_name();
                let name = std::str::from_utf8(local_name.as_ref()).unwrap_or("");
                if name == "Body" {
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

// --- Handlers ---

async fn handle_get_folder(session: &jmap_client::JmapSession, xml: &str) -> String {
    // Parse and validate the request structure, even if we ignore the specific ID for now
    let _req: GetFolder = match parse_body_content(xml) {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!("GetFolder XML parse error: {:?}", e);
            // We proceed with defaults because Outlook often sends complex requests
            GetFolder::default()
        }
    };

    let cal_id = match jmap_client::get_default_calendar_id(
        &session.api_url,
        &session.access_token,
        &session.account_id,
    )
    .await
    {
        Ok(id) => id,
        Err(_) => "calendar-default".to_string(),
    };

    soap_response(&format!(
        r#"<m:GetFolderResponse xmlns:m="{}" xmlns:t="{}">
            <m:ResponseMessages>
                <m:GetFolderResponseMessage ResponseClass="Success">
                    <m:ResponseCode>NoError</m:ResponseCode>
                    <m:Folders>
                        <t:CalendarFolder>
                            <t:FolderId Id="{}" ChangeKey="AQAAABYAAA=" />
                            <t:DisplayName>Calendar</t:DisplayName>
                        </t:CalendarFolder>
                    </m:Folders>
                </m:GetFolderResponseMessage>
            </m:ResponseMessages>
        </m:GetFolderResponse>"#,
        NS_M, NS_T, cal_id
    ))
}

async fn handle_find_folder(_session: &jmap_client::JmapSession) -> String {
    soap_response(&format!(
        r#"<m:FindFolderResponse xmlns:m="{}" xmlns:t="{}">
            <m:ResponseMessages>
                <m:FindFolderResponseMessage ResponseClass="Success">
                    <m:ResponseCode>NoError</m:ResponseCode>
                    <m:RootFolder TotalItemsInView="0" IncludesLastItemInRange="true">
                        <t:Folders />
                    </m:RootFolder>
                </m:FindFolderResponseMessage>
            </m:ResponseMessages>
        </m:FindFolderResponse>"#,
        NS_M, NS_T
    ))
}

async fn handle_sync_folder_items(
    session: &jmap_client::JmapSession,
    config: &AppConfig,
    user: &str,
    xml: &str,
) -> String {
    let req: SyncFolderItems = match parse_body_content(xml) {
        Ok(r) => r,
        Err(e) => {
            tracing::error!("Failed to parse SyncFolderItems: {:?}", e);
            return soap_fault("ErrorInvalidRequest", "Bad XML");
        }
    };

    let folder_id = req
        .sync_folder_id
        .folder_id
        .map(|f| f.id)
        .unwrap_or_else(|| "calendar-default".to_string());

    // Retrieve the JMAP state associated with the last SyncState (UUID) we issued
    let prev_jmap_state = db::get_ews_sync_state(config, user, &folder_id).await;

    // Get current server state
    let current_jmap_state = match jmap_client::get_calendar_state(
        &session.api_url,
        &session.access_token,
        &session.account_id,
    )
    .await
    {
        Ok(s) => s,
        Err(_) => return soap_fault("ErrorInternalServerError", "State Error"),
    };

    // Generate a new SyncState (UUID) for the client to hold
    let new_sync_token = uuid::Uuid::new_v4().to_string();

    // Determine if we need a full sync or delta sync
    // If client didn't send a state (None), or we have no record of their state (None), -> Full Sync
    let is_initial_sync = req.sync_state.is_none() || prev_jmap_state.is_none();

    let (items_xml, includes_last) = if is_initial_sync {
        // FULL SYNC: Fetch all events, render as Create
        let events = jmap_client::get_calendar_events(
            &session.api_url,
            &session.access_token,
            &session.account_id,
        )
        .await
        .unwrap_or_default();

        let xml = render_items(&events, "Create", &config.timezone);
        (xml, true)
    } else {
        // DELTA SYNC
        // Check if anything changed
        if prev_jmap_state.as_ref().unwrap() == &current_jmap_state {
            // No changes
            return soap_response(&format!(
                r#"<m:SyncFolderItemsResponse xmlns:m="{}" xmlns:t="{}">
                    <m:ResponseMessages>
                        <m:SyncFolderItemsResponseMessage ResponseClass="Success">
                            <m:ResponseCode>NoError</m:ResponseCode>
                            <m:SyncState>{}</m:SyncState>
                            <m:IncludesLastItemInRange>true</m:IncludesLastItemInRange>
                            <m:Changes />
                        </m:SyncFolderItemsResponseMessage>
                    </m:ResponseMessages>
                </m:SyncFolderItemsResponse>"#,
                NS_M,
                NS_T,
                escape_xml(&req.sync_state.unwrap_or_default())
            ));
        }

        // Fetch changes
        match jmap_client::get_calendar_changes(
            &session.api_url,
            &session.access_token,
            &session.account_id,
            &prev_jmap_state.unwrap(),
        )
        .await
        {
            Ok(changes) => {
                let mut xml = String::new();

                // Handle Deletes
                for id in &changes.destroyed {
                    xml.push_str(&format!(
                        r#"<t:Delete><t:ItemId Id="{}" /></t:Delete>"#,
                        escape_xml(id)
                    ));
                }

                // Handle Updates/Creates
                // JMAP /changes returns 'updated' which includes both modified and created items.
                // EWS expects <Update> for modified, <Create> for new.
                // Since we lack the local DB state to distinguish, we treat all as <Update>.
                // If an item is new, Outlook might reject the Update or auto-promote.
                // Ideally, we fetch details for these IDs.
                if !changes.updated.is_empty()
                    && let Ok(events) = jmap_client::get_events_by_ids(
                        &session.api_url,
                        &session.access_token,
                        &session.account_id,
                        &changes.updated,
                    )
                    .await
                {
                    // We render as "Update". If it's a new item, Outlook may log a client error
                    // but this is the safest path to avoid duplicates from 'Create'.
                    xml.push_str(&render_items(&events, "Update", &config.timezone));
                }
                (xml, true)
            }
            Err(_) => {
                // Fallback to Full Sync if delta fails (e.g. state expired)
                tracing::warn!("Delta sync failed, falling back to full sync");
                let events = jmap_client::get_calendar_events(
                    &session.api_url,
                    &session.access_token,
                    &session.account_id,
                )
                .await
                .unwrap_or_default();
                (render_items(&events, "Create", &config.timezone), true)
            }
        }
    };

    // Persist the new mapping: SyncToken -> CurrentJmapState
    db::update_ews_sync_state(
        config,
        user,
        &folder_id,
        &new_sync_token,
        &current_jmap_state,
    )
    .await;

    soap_response(&format!(
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
        NS_M, NS_T, new_sync_token, includes_last, items_xml
    ))
}

fn render_items(events: &[jmap_client::JmapEvent], mode: &str, tz_str: &str) -> String {
    let mut xml = String::new();
    let tz: Tz = tz_str.parse().unwrap_or(chrono_tz::UTC);

    for event in events {
        let start_dt: DateTime<Utc> = event.start.parse().unwrap_or_default();
        let end_dt: DateTime<Utc> = event.end.parse().unwrap_or_default();

        let start_local = start_dt.with_timezone(&tz);
        let end_local = end_dt.with_timezone(&tz);

        xml.push_str(&format!(
            r#"<t:{}>
                <t:CalendarItem>
                    <t:ItemId Id="{}" ChangeKey="AAA=" />
                    <t:Subject>{}</t:Subject>
                    <t:Location>{}</t:Location>
                    <t:Start>{}</t:Start>
                    <t:End>{}</t:End>
                    <t:Body BodyType="Text">{}</t:Body>
                </t:CalendarItem>
            </t:{}>"#,
            mode,
            escape_xml(event.id.as_deref().unwrap_or("")),
            escape_xml(&event.title),
            escape_xml(event.location.as_deref().unwrap_or("")),
            start_local.format("%Y-%m-%dT%H:%M:%S"),
            end_local.format("%Y-%m-%dT%H:%M:%S"),
            escape_xml(event.description.as_deref().unwrap_or("")),
            mode
        ));
    }
    xml
}

async fn handle_create_item(
    session: &jmap_client::JmapSession,
    config: &AppConfig,
    xml: &str,
) -> String {
    let req: CreateItem = match parse_body_content(xml) {
        Ok(r) => r,
        Err(e) => {
            tracing::error!("Failed to parse CreateItem: {:?}", e);
            return soap_fault("ErrorInvalidRequest", "Bad XML");
        }
    };

    if let Some(item) = req.items.calendar_item {
        let tz: Tz = config.timezone.parse().unwrap_or(chrono_tz::UTC);

        let start_utc = parse_ews_time(&item.start.unwrap_or_default(), tz);
        let end_utc = parse_ews_time(&item.end.unwrap_or_default(), tz);

        let attendees: Vec<jmap_client::Participant> = item
            .required_attendees
            .map(|a| {
                a.attendees
                    .into_iter()
                    .map(|att| jmap_client::Participant {
                        email: att.mailbox.email,
                        name: att.mailbox.name.unwrap_or_default(),
                    })
                    .collect()
            })
            .unwrap_or_default();

        let jmap_event = jmap_client::JmapEvent {
            id: None,
            title: item.subject.unwrap_or_default(),
            start: start_utc,
            end: end_utc,
            description: item.body.map(|b| b.content),
            location: item.location,
            uid: None,
            participants: if attendees.is_empty() {
                None
            } else {
                Some(attendees)
            },
            is_all_day: false,
        };

        let new_id = match jmap_client::push_event(
            &session.api_url,
            &session.access_token,
            &session.account_id,
            jmap_event,
        )
        .await
        {
            Ok(id) => id,
            Err(e) => {
                tracing::error!("Failed to create item: {}", e);
                return soap_fault("ErrorInternalServerError", "Save Failed");
            }
        };

        return soap_response(&format!(
            r#"<m:CreateItemResponse xmlns:m="{}" xmlns:t="{}">
                <m:ResponseMessages>
                    <m:CreateItemResponseMessage ResponseClass="Success">
                        <m:ResponseCode>NoError</m:ResponseCode>
                        <m:Items>
                            <t:CalendarItem>
                                <t:ItemId Id="{}" ChangeKey="AAA=" />
                            </t:CalendarItem>
                        </m:Items>
                    </m:CreateItemResponseMessage>
                </m:ResponseMessages>
            </m:CreateItemResponse>"#,
            NS_M, NS_T, new_id
        ));
    }

    soap_fault("ErrorInvalidRequest", "No item provided")
}

async fn handle_update_item(
    session: &jmap_client::JmapSession,
    config: &AppConfig,
    xml: &str,
) -> String {
    let req: UpdateItem = match parse_body_content(xml) {
        Ok(r) => r,
        Err(e) => {
            tracing::error!("Failed to parse UpdateItem: {:?}", e);
            return soap_fault("ErrorInvalidRequest", "Bad XML");
        }
    };

    let tz: Tz = config.timezone.parse().unwrap_or(chrono_tz::UTC);
    let client = reqwest::Client::new();

    for change in req.item_changes.items {
        let id = change.item_id.id;
        let mut patch = serde_json::Map::new();

        for set_field in change.updates.set_fields {
            let field = set_field.field_uri.field_uri;
            let val_item = set_field.calendar_item;

            match field.as_str() {
                "item:Subject" => {
                    if let Some(v) = val_item.subject {
                        patch.insert("title".to_string(), serde_json::json!(v));
                    }
                }
                "calendar:Location" => {
                    if let Some(v) = val_item.location {
                        patch.insert("location".to_string(), serde_json::json!(v));
                    }
                }
                "calendar:Start" => {
                    if let Some(v) = val_item.start {
                        patch.insert(
                            "start".to_string(),
                            serde_json::json!(parse_ews_time(&v, tz)),
                        );
                    }
                }
                "calendar:End" => {
                    if let Some(v) = val_item.end {
                        patch.insert("end".to_string(), serde_json::json!(parse_ews_time(&v, tz)));
                    }
                }
                "item:Body" => {
                    if let Some(b) = val_item.body {
                        patch.insert("description".to_string(), serde_json::json!(b.content));
                    }
                }
                _ => {}
            }
        }

        if !patch.is_empty() {
            let body = serde_json::json!({
                "using": ["urn:ietf:params:jmap:core", "urn:ietf:params:jmap:calendars"],
                "methodCalls": [
                    ["CalendarEvent/set", {
                        "accountId": session.account_id,
                        "update": {
                            id: patch
                        }
                    }, "c0"]
                ]
            });

            let res = client
                .post(&session.api_url)
                .header("Authorization", format!("Basic {}", session.access_token))
                .json(&body)
                .send()
                .await;

            if let Err(e) = res {
                tracing::error!("Update failed: {}", e);
            }
        }
    }

    soap_response(&format!(
        r#"<m:UpdateItemResponse xmlns:m="{}" xmlns:t="{}">
            <m:ResponseMessages>
                <m:UpdateItemResponseMessage ResponseClass="Success">
                    <m:ResponseCode>NoError</m:ResponseCode>
                </m:UpdateItemResponseMessage>
            </m:ResponseMessages>
        </m:UpdateItemResponse>"#,
        NS_M, NS_T
    ))
}

async fn handle_delete_item(
    session: &jmap_client::JmapSession,
    _config: &AppConfig,
    xml: &str,
) -> String {
    let req: DeleteItem = match parse_body_content(xml) {
        Ok(r) => r,
        Err(e) => {
            tracing::error!("Failed to parse DeleteItem: {:?}", e);
            return soap_fault("ErrorInvalidRequest", "Bad XML");
        }
    };

    let ids: Vec<String> = req.item_ids.items.into_iter().map(|i| i.id).collect();

    let client = reqwest::Client::new();
    let body = serde_json::json!({
        "using": ["urn:ietf:params:jmap:core", "urn:ietf:params:jmap:calendars"],
        "methodCalls": [
            ["CalendarEvent/set", {
                "accountId": session.account_id,
                "destroy": ids
            }, "c0"]
        ]
    });

    let _ = client
        .post(&session.api_url)
        .header("Authorization", format!("Basic {}", session.access_token))
        .json(&body)
        .send()
        .await;

    soap_response(&format!(
        r#"<m:DeleteItemResponse xmlns:m="{}" xmlns:t="{}">
            <m:ResponseMessages>
                <m:DeleteItemResponseMessage ResponseClass="Success">
                    <m:ResponseCode>NoError</m:ResponseCode>
                </m:DeleteItemResponseMessage>
            </m:ResponseMessages>
        </m:DeleteItemResponse>"#,
        NS_M, NS_T
    ))
}

async fn handle_get_item(
    session: &jmap_client::JmapSession,
    config: &AppConfig,
    xml: &str,
) -> String {
    let req: GetItem = match parse_body_content(xml) {
        Ok(r) => r,
        Err(e) => {
            tracing::error!("Failed to parse GetItem: {:?}", e);
            return soap_fault("ErrorInvalidRequest", "Bad XML");
        }
    };

    let ids: Vec<String> = req.item_ids.items.into_iter().map(|i| i.id).collect();

    let events = match jmap_client::get_events_by_ids(
        &session.api_url,
        &session.access_token,
        &session.account_id,
        &ids,
    )
    .await
    {
        Ok(e) => e,
        Err(_) => return soap_fault("ErrorItemNotFound", "Item not found"),
    };

    let mut items_xml = String::new();
    let tz: Tz = config.timezone.parse().unwrap_or(chrono_tz::UTC);

    for event in events {
        let start_dt: DateTime<Utc> = event.start.parse().unwrap_or_default();
        let end_dt: DateTime<Utc> = event.end.parse().unwrap_or_default();
        let start_local = start_dt.with_timezone(&tz);
        let end_local = end_dt.with_timezone(&tz);

        items_xml.push_str(&format!(
            r#"<t:CalendarItem>
                <t:ItemId Id="{}" ChangeKey="AAA=" />
                <t:Subject>{}</t:Subject>
                <t:Location>{}</t:Location>
                <t:Start>{}</t:Start>
                <t:End>{}</t:End>
                <t:Body BodyType="Text">{}</t:Body>
            </t:CalendarItem>"#,
            event.id.as_deref().unwrap_or(""),
            escape_xml(&event.title),
            escape_xml(event.location.as_deref().unwrap_or("")),
            start_local.format("%Y-%m-%dT%H:%M:%S"),
            end_local.format("%Y-%m-%dT%H:%M:%S"),
            escape_xml(event.description.as_deref().unwrap_or(""))
        ));
    }

    soap_response(&format!(
        r#"<m:GetItemResponse xmlns:m="{}" xmlns:t="{}">
            <m:ResponseMessages>
                <m:GetItemResponseMessage ResponseClass="Success">
                    <m:ResponseCode>NoError</m:ResponseCode>
                    <m:Items>{}</m:Items>
                </m:GetItemResponseMessage>
            </m:ResponseMessages>
        </m:GetItemResponse>"#,
        NS_M, NS_T, items_xml
    ))
}

async fn handle_find_item(_session: &jmap_client::JmapSession) -> String {
    soap_response(&format!(
        r#"<m:FindItemResponse xmlns:m="{}" xmlns:t="{}">
            <m:ResponseMessages>
                <m:FindItemResponseMessage ResponseClass="Success">
                    <m:ResponseCode>NoError</m:ResponseCode>
                    <m:RootFolder IndexedPagingOffset="0" TotalItemsInView="0" IncludesLastItemInRange="true">
                        <t:Items />
                    </m:RootFolder>
                </m:FindItemResponseMessage>
            </m:ResponseMessages>
        </m:FindItemResponse>"#,
        NS_M, NS_T
    ))
}

async fn handle_resolve_names(_session: &jmap_client::JmapSession) -> String {
    soap_response(&format!(
        r#"<m:ResolveNamesResponse xmlns:m="{}" xmlns:t="{}">
            <m:ResponseMessages>
                <m:ResolveNamesResponseMessage ResponseClass="Warning">
                    <m:ResponseCode>ErrorNameResolutionNoResults</m:ResponseCode>
                </m:ResolveNamesResponseMessage>
            </m:ResponseMessages>
        </m:ResolveNamesResponse>"#,
        NS_M, NS_T
    ))
}

async fn handle_get_room_lists() -> String {
    soap_response(&format!(
        r#"<m:GetRoomListsResponse xmlns:m="{}" xmlns:t="{}">
            <m:ResponseMessages>
                <m:GetRoomListsResponseMessage ResponseClass="Success">
                    <m:ResponseCode>NoError</m:ResponseCode>
                    <m:RoomLists />
                </m:GetRoomListsResponseMessage>
            </m:ResponseMessages>
        </m:GetRoomListsResponse>"#,
        NS_M, NS_T
    ))
}

async fn handle_get_rooms() -> String {
    soap_response(&format!(
        r#"<m:GetRoomsResponse xmlns:m="{}" xmlns:t="{}">
            <m:ResponseMessages>
                <m:GetRoomsResponseMessage ResponseClass="Success">
                    <m:ResponseCode>NoError</m:ResponseCode>
                    <m:Rooms />
                </m:GetRoomsResponseMessage>
            </m:ResponseMessages>
        </m:GetRoomsResponse>"#,
        NS_M, NS_T
    ))
}

// --- Helpers ---

fn parse_body_content<T: for<'de> Deserialize<'de>>(xml: &str) -> Result<T, quick_xml::DeError> {
    let envelope: SoapEnvelope<T> = quick_xml::de::from_str(xml)?;
    Ok(envelope.body.content)
}

fn parse_ews_time(time_str: &str, tz: Tz) -> String {
    // EWS typically sends YYYY-MM-DDTHH:MM:SSZ
    if let Ok(dt) = chrono::NaiveDateTime::parse_from_str(time_str, "%Y-%m-%dT%H:%M:%SZ") {
        return Utc.from_utc_datetime(&dt).to_rfc3339();
    }
    // Try without Z (local time)
    if let Ok(dt) = chrono::NaiveDateTime::parse_from_str(time_str, "%Y-%m-%dT%H:%M:%S") {
        return tz
            .from_local_datetime(&dt)
            .single()
            .map(|dt| dt.with_timezone(&Utc).to_rfc3339())
            .unwrap_or_default();
    }
    time_str.to_string()
}

fn soap_response(body: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="utf-8"?>
<s:Envelope xmlns:s="{}">
  <s:Body>
    {}
  </s:Body>
</s:Envelope>"#,
        NS_SOAP, body
    )
}

fn soap_fault(code: &str, msg: &str) -> String {
    soap_response(&format!(
        r#"<s:Fault>
            <faultcode>s:Client</faultcode>
            <faultstring>{}: {}</faultstring>
        </s:Fault>"#,
        code, msg
    ))
}

fn escape_xml(s: &str) -> String {
    escape::escape(s).into_owned()
}
